use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::poll::{poll, PollFd, PollFlags};
use nix::unistd::Pid;

use crate::config::Config;
use crate::error::Result;
use crate::ipc::{self, StartStatus};
use crate::linux::{self, set_cloexec};
use crate::pty::spawn_login;
use crate::session::{self, waitpid_nonblock};

pub fn run(cfg: Config, client_fd: RawFd, peer: String) -> Result<()> {
    linux::install_shutdown_handler()?;

    let (mon_ipc, child_ipc) = UnixStream::pair()?;
    set_cloexec(mon_ipc.as_raw_fd(), true)?;
    set_cloexec(child_ipc.as_raw_fd(), false)?;
    set_cloexec(client_fd, false)?;

    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cfg.push_reexec_args(&mut cmd);
    cmd.arg("--internal-role")
        .arg("child")
        .arg("--client-fd")
        .arg(client_fd.to_string())
        .arg("--ipc-fd")
        .arg(child_ipc.as_raw_fd().to_string())
        .arg("--peer")
        .arg(&peer);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::inherit());
    // Force fork+exec so CLOEXEC-cleared fds (client, ipc) survive exec.
    unsafe {
        cmd.pre_exec(|| Ok(()));
    }
    let child = cmd.spawn()?;
    let child_pid = Pid::from_raw(child.id() as i32);
    drop(child_ipc);
    let _ = nix::unistd::close(client_fd);

    monitor_loop(&cfg, mon_ipc, child_pid)
}

fn monitor_loop(cfg: &Config, mut ipc: UnixStream, child_pid: Pid) -> Result<()> {
    ipc.set_nonblocking(true)?;
    let mut session_pid: Option<Pid> = None;
    let mut started = false;

    loop {
        if linux::shutdown_requested() {
            if let Some(sid) = session_pid.take() {
                session::cleanup_session(sid, cfg);
            }
            let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                session::reap_zombies();
                if waitpid_nonblock(child_pid).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGKILL);
            session::reap_zombies();
            return Ok(());
        }

        if waitpid_nonblock(child_pid).is_some() {
            if let Some(sid) = session_pid.take() {
                session::cleanup_session(sid, cfg);
            }
            session::reap_zombies();
            return Ok(());
        }

        if let Some(sid) = session_pid {
            if waitpid_nonblock(sid).is_some() {
                session_pid = None;
            }
        }

        if !started {
            let mut fds = [PollFd::new(ipc.as_fd_view(), PollFlags::POLLIN)];
            let _ = poll(&mut fds, 200u16);
            if fds[0]
                .revents()
                .is_some_and(|r| r.contains(PollFlags::POLLIN))
            {
                match read_start(&mut ipc) {
                    Ok(req) => match spawn_login(&cfg.login, req.resize, &req.client_ip) {
                        Ok(pty) => {
                            let sid = pty.session_pid;
                            let master_fd = pty.master.as_raw_fd();
                            if let Err(e) =
                                ipc::send_reply_with_fd(&ipc, StartStatus::Ok, Some(master_fd))
                            {
                                tracing::error!("failed to pass pty fd: {e}");
                                session::cleanup_session(sid, cfg);
                                return Ok(());
                            }
                            drop(pty.master);
                            session_pid = Some(sid);
                            started = true;
                            println!("session_started");
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        }
                        Err(e) => {
                            tracing::error!("spawn login failed: {e}");
                            let _ = ipc::send_reply_with_fd(&ipc, StartStatus::Failed, None);
                            return Ok(());
                        }
                    },
                    Err(e) if is_would_block(&e) => {}
                    Err(e) => {
                        tracing::debug!("ipc closed before session: {e}");
                        return Ok(());
                    }
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn read_start(ipc: &mut UnixStream) -> Result<ipc::StartSessionRequest> {
    ipc.set_nonblocking(false)?;
    let req = ipc::recv_request(ipc);
    ipc.set_nonblocking(true)?;
    req
}

fn is_would_block(err: &crate::error::Error) -> bool {
    matches!(err, crate::error::Error::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

trait AsFdView {
    fn as_fd_view(&self) -> std::os::fd::BorrowedFd<'_>;
}

impl AsFdView for UnixStream {
    fn as_fd_view(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.as_fd()
    }
}

pub fn spawn_monitor(cfg: &Config, client_fd: RawFd, peer: &str) -> Result<MonitorChild> {
    let exe = std::env::current_exe()?;
    set_cloexec(client_fd, false)?;
    let mut cmd = Command::new(exe);
    cfg.push_reexec_args(&mut cmd);
    cmd.arg("--internal-role")
        .arg("monitor")
        .arg("--client-fd")
        .arg(client_fd.to_string())
        .arg("--peer")
        .arg(peer);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    unsafe {
        cmd.pre_exec(|| Ok(()));
    }
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| crate::error::Error::msg("missing monitor stdout"))?;
    crate::linux::set_nonblocking(stdout.as_raw_fd(), true)?;
    Ok(MonitorChild {
        child,
        stdout: BufReader::new(stdout),
        session_started: false,
    })
}

pub struct MonitorChild {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
    pub session_started: bool,
}

impl MonitorChild {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn poll_status(&mut self) -> Option<bool> {
        let mut line = String::new();
        loop {
            line.clear();
            match self.stdout.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.trim() == "session_started" {
                        self.session_started = true;
                        return Some(true);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        None
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub fn sigterm(&self) {
        let _ = nix::sys::signal::kill(
            Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
}
