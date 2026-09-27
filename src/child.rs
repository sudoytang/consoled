use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::sync::{oneshot, Mutex};
use tokio_rustls::TlsAcceptor;

use crate::config::Config;
use crate::error::Result;
use crate::http::{self, HttpState, WsStream};
use crate::ipc::{self, StartSessionRequest, StartStatus};
use crate::privilege;
use crate::session::ExitReason;
use crate::websocket;

pub fn run(cfg: Config, client_fd: RawFd, ipc_fd: RawFd, peer: SocketAddr) -> Result<()> {
    let tls = http::tls_server_config(&cfg)?;
    privilege::drop_privileges(&cfg)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(child_main(cfg, tls, client_fd, ipc_fd, peer))
}

async fn child_main(
    cfg: Config,
    tls: rustls::ServerConfig,
    client_fd: RawFd,
    ipc_fd: RawFd,
    peer: SocketAddr,
) -> Result<()> {
    let started = Instant::now();
    let std_tcp = unsafe { std::net::TcpStream::from_raw_fd(client_fd) };
    std_tcp.set_nonblocking(true)?;
    let tcp = tokio::net::TcpStream::from_std(std_tcp)?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let tls_stream = acceptor
        .accept(tcp)
        .await
        .map_err(|e| crate::error::Error::msg(format!("tls handshake failed: {e}")))?;

    let (upgrade_tx, upgrade_rx) = oneshot::channel::<WsStream>();
    let state = HttpState {
        cfg: Arc::new(cfg.clone()),
        peer,
        upgrade_tx: Arc::new(Mutex::new(Some(upgrade_tx))),
    };

    let io = TokioIo::new(tls_stream);
    let conn = http1::Builder::new()
        .serve_connection(
            io,
            service_fn(move |req: hyper::Request<Incoming>| {
                let state = state.clone();
                async move { http::handle(state, req).await }
            }),
        )
        .with_upgrades();

    let http_task = tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!("http connection ended: {e}");
        }
    });

    let reason = match upgrade_rx.await {
        Ok(ws) => match run_console(&cfg, ws, ipc_fd, peer).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("console session error: {e}");
                ExitReason::ChildError
            }
        },
        Err(_) => {
            let _ = http_task.await;
            ExitReason::HttpOnly
        }
    };

    if reason != ExitReason::HttpOnly {
        tracing::info!(
            src = %peer.ip(),
            duration_s = format!("{:.3}", started.elapsed().as_secs_f64()),
            reason = reason.as_str(),
            "consoled: event=session_end"
        );
    }
    Ok(())
}

async fn run_console(
    cfg: &Config,
    ws: WsStream,
    ipc_fd: RawFd,
    peer: SocketAddr,
) -> Result<ExitReason> {
    let (ws, resize) = match websocket::wait_initial_resize(ws, cfg.initial_resize_timeout).await {
        Ok(v) => v,
        Err((_, reason)) => return Ok(reason),
    };

    let ipc_fd_copy = ipc_fd;
    let ip = peer.ip().to_string();
    let started = tokio::task::spawn_blocking(move || {
        let mut ipc = unsafe { UnixStream::from_raw_fd(ipc_fd_copy) };
        ipc::send_request(
            &mut ipc,
            &StartSessionRequest {
                resize,
                client_ip: ip,
            },
        )?;
        let (status, fd) = ipc::recv_reply_with_fd(&ipc)?;
        let _ = ipc;
        if status != StartStatus::Ok {
            return Err(crate::error::Error::msg("monitor refused session"));
        }
        fd.ok_or_else(|| crate::error::Error::msg("monitor sent no pty fd"))
    })
    .await
    .map_err(|e| crate::error::Error::msg(format!("ipc task: {e}")))?;

    let master = match started {
        Ok(fd) => fd,
        Err(e) => {
            tracing::error!("start session failed: {e}");
            return Ok(ExitReason::SessionSetupFailed);
        }
    };

    crate::linux::set_nonblocking(master.as_raw_fd(), true)?;
    let pty = tokio::io::unix::AsyncFd::new(master)?;
    Ok(websocket::pump(ws, pty, cfg).await)
}
