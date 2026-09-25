use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::IntoRawFd;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::poll::{poll, PollFd, PollFlags};
use rand::rngs::OsRng;

use crate::config::Config;
use crate::error::Result;
use crate::limits::{normalize_ip, AdmitDecision, LimitTracker};
use crate::linux::{self, shutdown_requested};
use crate::monitor::{spawn_monitor, MonitorChild};

struct LiveConn {
    monitor: MonitorChild,
    ip: std::net::IpAddr,
    started_at: Instant,
}

pub fn run(cfg: Config) -> Result<()> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return Err(crate::error::Error::msg(
            "the listener must run as root so it can spawn login(1) (PAM needs euid 0)",
        ));
    }
    if !cfg.no_privdrop {
        if !cfg.chroot.is_dir() {
            return Err(crate::error::Error::msg(format!(
                "chroot directory {} does not exist (mkdir -p /var/empty && chmod 755 /var/empty)",
                cfg.chroot.display()
            )));
        }
        if nix::unistd::User::from_name(&cfg.user)
            .ok()
            .flatten()
            .is_none()
        {
            return Err(crate::error::Error::msg(format!(
                "system user '{}' does not exist (useradd --system --no-create-home --shell /usr/sbin/nologin {})",
                cfg.user, cfg.user
            )));
        }
    }
    if !cfg.cert.is_file() || !cfg.key.is_file() {
        return Err(crate::error::Error::msg(
            "TLS --cert and --key must exist; plaintext mode is not supported",
        ));
    }
    if !cfg.login.is_file() {
        return Err(crate::error::Error::msg(format!(
            "login binary not found at {}",
            cfg.login.display()
        )));
    }

    linux::install_shutdown_handler()?;
    let listener = TcpListener::bind(cfg.listen)?;
    listener.set_nonblocking(true)?;
    tracing::info!(listen = %cfg.listen, "consoled: event=listen");

    let limits = Arc::new(Mutex::new(LimitTracker::new(cfg.limits.clone())));
    let live: Arc<Mutex<Vec<LiveConn>>> = Arc::new(Mutex::new(Vec::new()));

    while !shutdown_requested() {
        reap(&live, &limits);
        let mut fds = [PollFd::new(listener.as_fd_borrowed(), PollFlags::POLLIN)];
        let _ = poll(&mut fds, 250u16);
        if shutdown_requested() {
            break;
        }
        match listener.accept() {
            Ok((stream, addr)) => {
                if let Err(e) = accept_one(&cfg, stream, addr, &limits, &live) {
                    tracing::warn!("accept handler failed: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                if !shutdown_requested() {
                    tracing::error!("accept failed: {e}");
                }
            }
        }
    }

    tracing::info!("consoled: event=shutdown");
    {
        let guard = live.lock().unwrap();
        for conn in guard.iter() {
            conn.monitor.sigterm();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        reap(&live, &limits);
        if live.lock().unwrap().is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    {
        let guard = live.lock().unwrap();
        for conn in guard.iter() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(conn.monitor.pid() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
    reap(&live, &limits);
    Ok(())
}

fn accept_one(
    cfg: &Config,
    stream: TcpStream,
    addr: SocketAddr,
    limits: &Arc<Mutex<LimitTracker>>,
    live: &Arc<Mutex<Vec<LiveConn>>>,
) -> Result<()> {
    let ip = normalize_ip(addr.ip());
    let decision = {
        let mut tracker = limits.lock().unwrap();
        tracker.admit(ip, Instant::now(), &mut OsRng)
    };
    if decision != AdmitDecision::Allow {
        tracing::info!(
            src = %ip,
            reason = decision.as_str(),
            "consoled: event=drop"
        );
        drop(stream);
        return Ok(());
    }

    tracing::info!(src = %ip, "consoled: event=accept");
    let fd = stream.into_raw_fd();
    let spawned = spawn_monitor(cfg, fd, &addr.to_string());
    let _ = nix::unistd::close(fd);
    match spawned {
        Ok(monitor) => {
            live.lock().unwrap().push(LiveConn {
                monitor,
                ip,
                started_at: Instant::now(),
            });
        }
        Err(e) => {
            limits
                .lock()
                .unwrap()
                .on_disconnect(ip, Instant::now(), false, Duration::ZERO);
            return Err(e);
        }
    }
    Ok(())
}

fn reap(live: &Arc<Mutex<Vec<LiveConn>>>, limits: &Arc<Mutex<LimitTracker>>) {
    let mut live = live.lock().unwrap();
    let mut i = 0;
    while i < live.len() {
        let just_started = live[i].monitor.poll_status().unwrap_or(false);
        if just_started {
            limits.lock().unwrap().mark_session_started();
            tracing::info!(src = %live[i].ip, "consoled: event=session_start");
        }
        match live[i].monitor.try_wait() {
            Ok(Some(_)) => {
                let conn = live.remove(i);
                let duration = conn.started_at.elapsed();
                limits.lock().unwrap().on_disconnect(
                    conn.ip,
                    Instant::now(),
                    conn.monitor.session_started,
                    duration,
                );
                tracing::info!(
                    src = %conn.ip,
                    duration_s = format!("{:.3}", duration.as_secs_f64()),
                    session = conn.monitor.session_started,
                    "consoled: event=monitor_exit"
                );
            }
            Ok(None) => i += 1,
            Err(_) => {
                live.remove(i);
            }
        }
    }
}

trait BorrowListenFd {
    fn as_fd_borrowed(&self) -> std::os::fd::BorrowedFd<'_>;
}

impl BorrowListenFd for TcpListener {
    fn as_fd_borrowed(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.as_fd()
    }
}
