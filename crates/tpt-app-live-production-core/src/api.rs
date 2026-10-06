//! Optional localhost control API (spec 17).
//!
//! - **disabled by default**: [`ApiConfig::default()`] has `enabled=false`,
//!   and [`LocalApi::spawn`] refuses to start a disabled config — the API
//!   cannot come up by accident,
//! - **loopback only by default**: binding a non-loopback address requires
//!   `allow_external: true` explicitly,
//! - **token auth**: when `token` is set, every request (including
//!   `/health`) must carry `Authorization: Bearer <token>`,
//! - endpoints: `GET /show/state`, `POST /show/cue/next`,
//!   `POST /show/cue/:id/go`, `GET /health`, `WS /events`.
//!
//! Implemented directly on `std::net` + `tungstenite` (feature `api`) so
//! the engine keeps its no-async runtime posture; one thread per
//! connection, bounded event queues, no blocking on the engine.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tungstenite::WebSocket;

use crate::engine::LiveEngine;
use tpt_app_live_production_model::CueId;

/// Default TCP port for the local API.
pub const DEFAULT_PORT: u16 = 8590;

/// API configuration. **Disabled by default** (spec 17).
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// The API only starts when this is true.
    pub enabled: bool,
    /// Bind address. Must be loopback unless `allow_external` is set.
    pub bind: SocketAddr,
    /// Bearer token; when set, all endpoints require it.
    pub token: Option<String>,
    /// Explicit opt-in to non-loopback binding (discouraged; Art-Net-style
    /// LAN tools may want it for a dedicated control VLAN).
    pub allow_external: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), DEFAULT_PORT),
            token: None,
            allow_external: false,
        }
    }
}

/// Errors from starting the API.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The API is disabled in configuration.
    #[error("local API is disabled (set enabled = true to start it)")]
    Disabled,
    /// Refusing to bind a non-loopback address without explicit opt-in.
    #[error("refusing to bind non-loopback address {0} (set allow_external to override)")]
    ExternalBind(SocketAddr),
    /// The socket could not be bound.
    #[error("cannot bind {0}: {1}")]
    Bind(SocketAddr, std::io::Error),
}

/// A running API server. Drop (or call [`RunningApi::shutdown`]) to stop.
pub struct RunningApi {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    pub(crate) local_addr: SocketAddr,
}

impl std::fmt::Debug for RunningApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningApi")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

impl RunningApi {
    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops the server and joins its thread.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RunningApi {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawns the API server for `engine`.
pub fn spawn(engine: Arc<Mutex<LiveEngine>>, config: ApiConfig) -> Result<RunningApi, ApiError> {
    if !config.enabled {
        return Err(ApiError::Disabled);
    }
    if !config.bind.ip().is_loopback() && !config.allow_external {
        return Err(ApiError::ExternalBind(config.bind));
    }
    let listener = TcpListener::bind(config.bind).map_err(|e| ApiError::Bind(config.bind, e))?;
    let local_addr = listener.local_addr().unwrap_or(config.bind);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let token = config.token.clone();
    let handle = std::thread::Builder::new()
        .name("tpt-lp-api".to_string())
        .spawn(move || {
            accept_loop(listener, engine, token, stop_flag);
        })
        .expect("api thread spawn");
    Ok(RunningApi {
        stop,
        handle: Some(handle),
        local_addr,
    })
}

fn accept_loop(
    listener: TcpListener,
    engine: Arc<Mutex<LiveEngine>>,
    token: Option<String>,
    stop: Arc<AtomicBool>,
) {
    listener
        .set_nonblocking(true)
        .expect("listener nonblocking");
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let engine = engine.clone();
                let token = token.clone();
                let stop = stop.clone();
                std::thread::Builder::new()
                    .name("tpt-lp-api-conn".to_string())
                    .spawn(move || {
                        let _ = handle_connection(stream, engine, token, stop);
                    })
                    .ok();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                log::warn!("api accept error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    /// Request body (read by command handlers as the API grows).
    #[allow(dead_code)]
    body: Vec<u8>,
    wants_websocket: bool,
    ws_key: Option<String>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end;
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    let mut wants_websocket = false;
    let mut ws_key = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_lowercase();
            let value = value.trim().to_string();
            match name.as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                "upgrade" if value.eq_ignore_ascii_case("websocket") => wants_websocket = true,
                "sec-websocket-key" => ws_key = Some(value.clone()),
                _ => {}
            }
            headers.push((name, value));
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length.max(body.len().min(content_length)));
    Ok(Request {
        method,
        path,
        headers,
        body,
        wants_websocket,
        ws_key,
    })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

fn write_json(stream: &mut TcpStream, status: u16, body: &impl Serialize) {
    let payload = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn handle_connection(
    mut stream: TcpStream,
    engine: Arc<Mutex<LiveEngine>>,
    token: Option<String>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    let req = read_request(&mut stream)?;

    // Auth: every endpoint requires the token when one is configured.
    if let Some(expected) = &token {
        let provided = header(&req, "authorization").unwrap_or("");
        if !constant_time_eq(provided.trim_start_matches("Bearer "), expected) {
            write_json(
                &mut stream,
                401,
                &serde_json::json!({"error": "unauthorized"}),
            );
            return Ok(());
        }
    }

    // WebSocket event stream.
    if req.path == "/events" {
        if req.wants_websocket {
            if let Some(key) = &req.ws_key {
                return run_events_socket(stream, engine, key.clone(), stop);
            }
        }
        // Fall through: an SSE-ish poll fallback is unnecessary; tell the
        // client to upgrade.
        write_json(
            &mut stream,
            400,
            &serde_json::json!({"error": "websocket upgrade required"}),
        );
        return Ok(());
    }

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => {
            write_json(&mut stream, 200, &serde_json::json!({"ok": true}));
        }
        ("GET", "/show/state") => {
            let state = engine.lock().expect("engine lock").state();
            write_json(&mut stream, 200, &state);
        }
        ("POST", "/show/cue/next") => {
            let fired = engine.lock().expect("engine lock").go();
            match fired {
                Some(exec) => write_json(
                    &mut stream,
                    200,
                    &serde_json::json!({"fired": {"number": exec.cue.number.0, "label": exec.cue.label}}),
                ),
                None => write_json(
                    &mut stream,
                    409,
                    &serde_json::json!({"error": "cue stack exhausted"}),
                ),
            }
        }
        ("POST", path) if path.starts_with("/show/cue/") && path.ends_with("/go") => {
            let id = path
                .trim_start_matches("/show/cue/")
                .trim_end_matches("/go");
            let result = engine
                .lock()
                .expect("engine lock")
                .go_to_cue(&CueId::new(id));
            match result {
                Ok(exec) => write_json(
                    &mut stream,
                    200,
                    &serde_json::json!({"fired": {"number": exec.cue.number.0, "label": exec.cue.label}}),
                ),
                Err(_) => write_json(
                    &mut stream,
                    404,
                    &serde_json::json!({"error": "unknown cue"}),
                ),
            }
        }
        ("GET", _) | ("POST", _) => {
            write_json(&mut stream, 404, &serde_json::json!({"error": "not found"}));
        }
        _ => {
            write_json(
                &mut stream,
                405,
                &serde_json::json!({"error": "method not allowed"}),
            );
        }
    }
    Ok(())
}

fn run_events_socket(
    stream: TcpStream,
    engine: Arc<Mutex<LiveEngine>>,
    key: String,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    use tungstenite::Message;

    // Complete the RFC 6455 handshake manually (we already consumed the
    // request bytes).
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    let mut socket = WebSocket::from_raw_socket(stream, tungstenite::protocol::Role::Server, None);
    let subscription = {
        let engine = engine.lock().expect("engine lock");
        engine.subscribe("api-events", 256)
    };

    loop {
        if stop.load(Ordering::Acquire) {
            let _ = socket.close(None);
            return Ok(());
        }
        // Service inbound frames (pings, pongs, closes) non-blockingly.
        socket.read().ok();
        match subscription.recv_timeout(Duration::from_millis(50)) {
            Some(event) => {
                let payload = serde_json::to_string(&event).unwrap_or_default();
                if socket.send(Message::Text(payload.into())).is_err() {
                    return Ok(());
                }
            }
            None => continue,
        }
    }
}

/// Comparison that does not short-circuit on the first differing byte.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, LiveEngine};

    fn test_engine() -> Arc<Mutex<LiveEngine>> {
        let show = crate::engine::tests::test_show();
        let engine = LiveEngine::build(show, EngineConfig::default()).expect("engine");
        Arc::new(Mutex::new(engine))
    }

    fn spawn_api(cfg: ApiConfig) -> (RunningApi, Arc<Mutex<LiveEngine>>) {
        let engine = test_engine();
        (spawn(engine.clone(), cfg).expect("spawn"), engine)
    }

    fn http_get(addr: SocketAddr, path: &str, token: Option<&str>) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).expect("connect");
        let auth = token
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = std::io::Read::read_to_string(&mut stream, &mut response);
        let status: u16 = response
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (status, body)
    }

    #[test]
    fn disabled_config_refuses_to_start() {
        let engine = test_engine();
        let err = spawn(engine, ApiConfig::default()).unwrap_err();
        assert!(matches!(err, ApiError::Disabled));
    }

    #[test]
    fn external_bind_is_refused_by_default() {
        let engine = test_engine();
        let cfg = ApiConfig {
            enabled: true,
            bind: SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0),
            token: None,
            allow_external: false,
        };
        let err = spawn(engine, cfg).unwrap_err();
        assert!(matches!(err, ApiError::ExternalBind(_)));
    }

    #[test]
    fn health_and_state_endpoints_work() {
        let cfg = ApiConfig {
            enabled: true,
            bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
            ..ApiConfig::default()
        };
        let (mut api, _engine) = spawn_api(cfg);
        let addr = api.local_addr();
        let (status, body) = http_get(addr, "/health", None);
        assert_eq!(status, 200);
        assert!(body.contains("\"ok\":true"), "{body}");
        let (status, body) = http_get(addr, "/show/state", None);
        assert_eq!(status, 200);
        assert!(body.contains("Test Show"), "{body}");
        api.shutdown();
    }

    #[test]
    fn token_is_required_when_configured() {
        let cfg = ApiConfig {
            enabled: true,
            bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
            token: Some("sekrit".to_string()),
            allow_external: false,
        };
        let (mut api, _) = spawn_api(cfg);
        let addr = api.local_addr();
        let (status, _) = http_get(addr, "/health", None);
        assert_eq!(status, 401, "missing token rejected");
        let (status, _) = http_get(addr, "/health", Some("wrong"));
        assert_eq!(status, 401, "wrong token rejected");
        let (status, body) = http_get(addr, "/health", Some("sekrit"));
        assert_eq!(status, 200);
        assert!(body.contains("\"ok\":true"));
        api.shutdown();
    }

    #[test]
    fn cue_next_fires_and_409s_when_exhausted() {
        let cfg = ApiConfig {
            enabled: true,
            bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
            ..ApiConfig::default()
        };
        let (mut api, _engine) = spawn_api(cfg);
        let addr = api.local_addr();
        let mut fired = 0;
        for _ in 0..5 {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(b"POST /show/cue/next HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            let _ = std::io::Read::read_to_string(&mut stream, &mut response);
            let status: u16 = response
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if status == 200 {
                fired += 1;
            } else {
                assert_eq!(status, 409);
            }
        }
        assert_eq!(fired, 3, "exactly the three cues fire, then 409");
        api.shutdown();
    }

    #[test]
    fn unknown_routes_404_and_malformed_requests_do_not_panic() {
        let cfg = ApiConfig {
            enabled: true,
            bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
            ..ApiConfig::default()
        };
        let (mut api, _) = spawn_api(cfg);
        let addr = api.local_addr();
        let (status, _) = http_get(addr, "/nope", None);
        assert_eq!(status, 404);
        // Garbage request: server survives, connection just fails.
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(b"NOT A REQUEST\r\n\r\n").ok();
        drop(stream);
        // Server still healthy.
        let (status, _) = http_get(addr, "/health", None);
        assert_eq!(status, 200);
        api.shutdown();
    }
}
