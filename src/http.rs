use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Role, WebSocketConfig};

use crate::config::Config;
use crate::error::Result;
use crate::static_files::{self, CSP};

pub type WsStream = WebSocketStream<TokioIo<Upgraded>>;

#[derive(Clone)]
pub struct HttpState {
    pub cfg: Arc<Config>,
    pub peer: SocketAddr,
    pub upgrade_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<WsStream>>>>,
}

pub async fn handle(
    state: HttpState,
    req: Request<Incoming>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path().to_string();
    if path == "/console" {
        return Ok(upgrade_console(state, req));
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return Ok(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed\n",
        ));
    }
    if path == "/favicon.ico" {
        return Ok(empty_response(StatusCode::NO_CONTENT));
    }
    match static_files::lookup(&path) {
        Some(file) => {
            let body = if req.method() == Method::HEAD {
                Bytes::new()
            } else {
                Bytes::from_static(file.body)
            };
            Ok(static_response(file.content_type, file.cache, body))
        }
        None => Ok(text_response(StatusCode::NOT_FOUND, "not found\n")),
    }
}

fn upgrade_console(state: HttpState, req: Request<Incoming>) -> Response<Full<Bytes>> {
    if !is_websocket_upgrade(&req) {
        return text_response(StatusCode::BAD_REQUEST, "expected websocket upgrade\n");
    }
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    if !state.cfg.origin_allowed(origin) {
        tracing::info!(
            src = %state.peer.ip(),
            origin = origin.unwrap_or("-"),
            "consoled: event=origin_rejected"
        );
        return text_response(StatusCode::FORBIDDEN, "origin not allowed\n");
    }
    let Some(key) = req
        .headers()
        .get("Sec-WebSocket-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
    else {
        return text_response(StatusCode::BAD_REQUEST, "missing websocket key\n");
    };
    let accept = derive_accept_key(key.as_bytes());
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let io = TokioIo::new(upgraded);
                let mut ws_cfg = WebSocketConfig::default();
                ws_cfg.max_message_size = Some(state.cfg.max_message_size);
                ws_cfg.max_frame_size = Some(state.cfg.max_message_size);
                let ws = WebSocketStream::from_raw_socket(io, Role::Server, Some(ws_cfg)).await;
                if let Some(tx) = state.upgrade_tx.lock().await.take() {
                    let _ = tx.send(ws);
                }
            }
            Err(e) => tracing::error!("websocket upgrade failed: {e}"),
        }
    });
    let mut res = Response::new(Full::new(Bytes::new()));
    *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = res.headers_mut();
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&accept) {
        headers.insert("Sec-WebSocket-Accept", v);
    }
    apply_security_headers(headers, false);
    res
}

fn is_websocket_upgrade(req: &Request<Incoming>) -> bool {
    if req.method() != Method::GET {
        return false;
    }
    let upgrade = header_contains(req, header::UPGRADE, "websocket");
    let connection = header_contains(req, header::CONNECTION, "upgrade");
    let version_ok = req
        .headers()
        .get("Sec-WebSocket-Version")
        .and_then(|v| v.to_str().ok())
        == Some("13");
    upgrade && connection && version_ok
}

fn header_contains(req: &Request<Incoming>, name: header::HeaderName, needle: &str) -> bool {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(needle))
        })
}

fn static_response(content_type: &str, cache: bool, body: Bytes) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(body));
    *res.status_mut() = StatusCode::OK;
    let headers = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    if cache {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=3600"),
        );
    } else {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    apply_security_headers(headers, false);
    res
}

fn text_response(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *res.status_mut() = status;
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(headers, false);
    res
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(Bytes::new()));
    *res.status_mut() = status;
    apply_security_headers(res.headers_mut(), false);
    res
}

fn apply_security_headers(headers: &mut http::HeaderMap, _hsts: bool) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "Permissions-Policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
}

pub fn tls_server_config(cfg: &Config) -> Result<rustls::ServerConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_pem = std::fs::read(&cfg.cert)?;
    let key_pem = std::fs::read(&cfg.key)?;
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<std::io::Result<Vec<_>>>()?;
    if certs.is_empty() {
        return Err(crate::error::Error::msg(
            "certificate file contains no certs",
        ));
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
        .ok_or_else(|| crate::error::Error::msg("key file contains no private key"))?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}
