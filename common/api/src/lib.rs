//! Zero-dependency HTTP server and the unified API envelope.
//! Routes are shared by every component; see contracts/openapi.yaml.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub use common_auth::AuthError;
pub use common_health::Health;
pub use common_metrics::Registry;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub peer: SocketAddr,
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

impl Response {
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8".to_string(),
            body,
        }
    }

    pub fn html(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8".to_string(),
            body,
        }
    }

    pub fn text(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8".to_string(),
            body,
        }
    }
}

pub struct Context {
    pub component: String,
    pub version: String,
    pub started: Instant,
    pub metrics: Registry,
    pub health: Arc<Health>,
    pub admin_token: String,
    pub config_raw: String,
    pub webui: bool,
}

pub type Handler = fn(&Request, &Context) -> Response;

#[derive(Clone)]
struct Route {
    method: &'static str,
    path: String,
    handler: Handler,
}

#[derive(Clone, Default)]
pub struct App {
    routes: Vec<Route>,
}

impl App {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn route(mut self, method: &'static str, path: &str, handler: Handler) -> Self {
        self.routes.push(Route {
            method,
            path: path.to_string(),
            handler,
        });
        self
    }

    pub fn dispatch(&self, req: &Request, ctx: &Context) -> Response {
        for route in &self.routes {
            if route.method == req.method && route.path == req.path {
                return (route.handler)(req, ctx);
            }
        }
        err(req, "not_found", "unknown endpoint", 404)
    }

    pub fn serve(self, addr: &str, ctx: Arc<Context>) -> std::io::Result<()> {
        let listener = TcpListener::bind(addr)?;
        let app = Arc::new(self);
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let app = Arc::clone(&app);
            let ctx = Arc::clone(&ctx);
            std::thread::spawn(move || {
                let mut stream = stream;
                handle_conn(&mut stream, app.as_ref(), ctx.as_ref());
            });
        }
        Ok(())
    }
}

fn handle_conn(stream: &mut TcpStream, app: &App, ctx: &Context) {
    let request = match read_request(stream) {
        Ok(Some(request)) => request,
        _ => return,
    };
    let response = if request.path.starts_with("/api/") {
        let loopback = request.peer.ip().is_loopback();
        match common_auth::check(&request.headers, &request.query, &ctx.admin_token, loopback) {
            Ok(()) => app.dispatch(&request, ctx),
            Err(AuthError::Required) => err(&request, "auth_required", "admin token required", 401),
            Err(AuthError::Failed) => err(&request, "auth_failed", "invalid admin token", 403),
        }
    } else {
        app.dispatch(&request, ctx)
    };
    let code = response.status.to_string();
    ctx.metrics.inc(
        "http_requests_total",
        &[
            ("method", request.method.as_str()),
            ("path", request.path.as_str()),
            ("code", code.as_str()),
        ],
        1,
    );
    let _ = write_response(stream, &response);
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let peer = stream.peer_addr()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    let parts: Vec<&str> = first.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(None);
    }
    let method = parts[0].to_string();
    let target = parts[1].to_string();
    let (path, query) = if let Some((path, query)) = target.split_once('?') {
        (path.to_string(), query.to_string())
    } else {
        (target.clone(), String::new())
    };
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(1 << 20);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(Some(Request {
        method,
        path,
        query,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
        peer,
    }))
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(response.body.as_bytes())?;
    stream.flush()
}

pub fn json_escape(value: &str) -> String {
    common_log::escape(value)
}

pub fn json_quote(value: &str) -> String {
    format!("\"{}\"", json_escape(value))
}

pub fn request_id(req: &Request) -> String {
    if let Some(rid) = req.headers.get("x-request-id") {
        return rid.clone();
    }
    let seq = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}-{:x}", nanos, seq)
}

pub fn ok(req: &Request, data_json: &str) -> Response {
    Response::json(
        200,
        format!(
            "{{\"ok\":true,\"data\":{},\"request_id\":{}}}",
            data_json,
            json_quote(&request_id(req))
        ),
    )
}

pub fn err(req: &Request, code: &str, message: &str, status: u16) -> Response {
    Response::json(
        status,
        format!(
            "{{\"ok\":false,\"error\":{{\"code\":{},\"message\":{}}},\"request_id\":{}}}",
            json_quote(code),
            json_quote(message),
            json_quote(&request_id(req))
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str) -> Request {
        Request {
            method: "GET".to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: HashMap::new(),
            body: String::new(),
            peer: "127.0.0.1:12345".parse().unwrap(),
        }
    }

    fn context() -> Context {
        Context {
            component: "lan-mdns".to_string(),
            version: "0.1.0".to_string(),
            started: Instant::now(),
            metrics: Registry::new("lan-mdns"),
            health: Arc::new(Health::new()),
            admin_token: String::new(),
            config_raw: String::new(),
            webui: true,
        }
    }

    fn dummy(_: &Request, _: &Context) -> Response {
        Response::text(200, "ok".to_string())
    }

    #[test]
    fn ok_envelope_shape() {
        let response = ok(&request("/api/status"), "{\"running\":true}");
        assert_eq!(response.status, 200);
        assert!(response.body.contains("\"ok\":true"));
        assert!(response.body.contains("\"data\":{\"running\":true}"));
        assert!(response.body.contains("\"request_id\":"));
    }

    #[test]
    fn error_envelope_shape() {
        let response = err(&request("/x"), "not_found", "missing", 404);
        assert_eq!(response.status, 404);
        assert!(response.body.contains("\"code\":\"not_found\""));
        assert!(response.body.contains("\"ok\":false"));
    }

    #[test]
    fn dispatches_known_route_and_404s_unknown() {
        let app = App::new().route("GET", "/ping", dummy);
        assert_eq!(app.dispatch(&request("/ping"), &context()).status, 200);
        assert_eq!(app.dispatch(&request("/missing"), &context()).status, 404);
    }
}
