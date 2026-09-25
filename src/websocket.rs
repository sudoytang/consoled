use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use crate::config::Config;
use crate::http::WsStream;
use crate::protocol::{parse_control, ControlError, ControlMessage, Resize};
use crate::pty::set_winsize;
use crate::session::ExitReason;

pub async fn wait_initial_resize(
    mut ws: WsStream,
    timeout: Duration,
) -> Result<(WsStream, Resize), (WsStream, ExitReason)> {
    let first = tokio::time::timeout(timeout, ws.next()).await;
    match first {
        Err(_) => {
            let _ = close(&mut ws, CloseCode::from(4000), "initial resize timeout").await;
            Err((ws, ExitReason::ResizeTimeout))
        }
        Ok(None) => Err((ws, ExitReason::WsClosed)),
        Ok(Some(Err(_))) => Err((ws, ExitReason::WsClosed)),
        Ok(Some(Ok(Message::Close(_)))) => Err((ws, ExitReason::WsClosed)),
        Ok(Some(Ok(Message::Ping(p)))) => {
            let _ = ws.send(Message::Pong(p)).await;
            Box::pin(wait_initial_resize(ws, timeout)).await
        }
        Ok(Some(Ok(Message::Pong(_)))) | Ok(Some(Ok(Message::Frame(_)))) => {
            Box::pin(wait_initial_resize(ws, timeout)).await
        }
        Ok(Some(Ok(Message::Binary(_)))) => {
            let _ = close(&mut ws, CloseCode::Protocol, "binary before resize").await;
            Err((ws, ExitReason::ProtocolError))
        }
        Ok(Some(Ok(Message::Text(text)))) => match parse_control(text.as_str()) {
            Ok(ControlMessage::Resize(resize)) => Ok((ws, resize)),
            Err(e) => {
                let _ = close(&mut ws, close_code(e), e.as_str()).await;
                Err((ws, ExitReason::ProtocolError))
            }
        },
    }
}

pub async fn pump(
    mut ws: WsStream,
    mut pty: tokio::fs::File,
    mut pty_write: tokio::fs::File,
    master_fd: i32,
    cfg: &Config,
) -> ExitReason {
    let mut buf = vec![0u8; cfg.max_message_size.min(64 * 1024)];
    let lifetime = sleep_opt(cfg.max_connection_lifetime);
    tokio::pin!(lifetime);
    let mut last_io = tokio::time::Instant::now();

    loop {
        let idle = idle_sleep(cfg.idle_timeout, last_io);
        tokio::pin!(idle);
        tokio::select! {
            _ = &mut lifetime => {
                let _ = close(&mut ws, CloseCode::from(4001), "lifetime exceeded").await;
                return ExitReason::LifetimeExceeded;
            }
            _ = &mut idle => {
                let _ = close(&mut ws, CloseCode::from(4002), "idle timeout").await;
                return ExitReason::IdleTimeout;
            }
            msg = ws.next() => {
                match msg {
                    None => return ExitReason::WsClosed,
                    Some(Err(e)) => {
                        if is_size_error(&e) {
                            return ExitReason::MessageTooBig;
                        }
                        return ExitReason::WsClosed;
                    }
                    Some(Ok(Message::Close(_))) => return ExitReason::WsClosed,
                    Some(Ok(Message::Ping(p))) => {
                        let _ = ws.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => {}
                    Some(Ok(Message::Binary(data))) => {
                        if let Err(e) = pty_write.write_all(&data).await {
                            tracing::debug!("pty write failed: {e}");
                            return ExitReason::PtyEof;
                        }
                        last_io = tokio::time::Instant::now();
                    }
                    Some(Ok(Message::Text(text))) => {
                        match parse_control(text.as_str()) {
                            Ok(ControlMessage::Resize(resize)) => {
                                if let Err(e) = set_winsize(master_fd, resize) {
                                    tracing::debug!("TIOCSWINSZ failed: {e}");
                                }
                                last_io = tokio::time::Instant::now();
                            }
                            Err(err) => {
                                let _ = close(&mut ws, close_code(err), err.as_str()).await;
                                return ExitReason::ProtocolError;
                            }
                        }
                    }
                }
            }
            read = pty.read(&mut buf) => {
                match read {
                    Ok(0) => {
                        let _ = close(&mut ws, CloseCode::Normal, "session ended").await;
                        return ExitReason::PtyEof;
                    }
                    Ok(n) => {
                        if ws.send(Message::Binary(bytes::Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                            return ExitReason::WsClosed;
                        }
                        last_io = tokio::time::Instant::now();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        let _ = close(&mut ws, CloseCode::Normal, "session ended").await;
                        return ExitReason::PtyEof;
                    }
                }
            }
        }
    }
}

fn close_code(err: ControlError) -> CloseCode {
    match err {
        ControlError::Malformed => CloseCode::Protocol,
        ControlError::UnknownType | ControlError::InvalidResize => CloseCode::Policy,
    }
}

async fn close(ws: &mut WsStream, code: CloseCode, reason: &str) {
    let _ = ws
        .close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))
        .await;
}

fn is_size_error(err: &tokio_tungstenite::tungstenite::Error) -> bool {
    matches!(
        err,
        tokio_tungstenite::tungstenite::Error::Capacity(_)
            | tokio_tungstenite::tungstenite::Error::Protocol(_)
    )
}

fn sleep_opt(d: Option<Duration>) -> tokio::time::Sleep {
    match d {
        Some(d) => tokio::time::sleep(d),
        None => tokio::time::sleep(Duration::from_secs(u64::MAX / 4)),
    }
}

fn idle_sleep(d: Option<Duration>, last_io: tokio::time::Instant) -> tokio::time::Sleep {
    match d {
        Some(limit) => {
            let elapsed = last_io.elapsed();
            let remain = limit.saturating_sub(elapsed);
            tokio::time::sleep(remain)
        }
        None => tokio::time::sleep(Duration::from_secs(u64::MAX / 4)),
    }
}
