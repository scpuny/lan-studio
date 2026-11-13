//! M2.3 built-in HTTP gateway (F-H01..F-H04).
//!
//! Scope is deliberately narrow, per PRD 1.4: **static Host routing for
//! explicitly registered names only**. There is no load balancing, TLS
//! termination, health-based eviction, body caching or payload inspection.
//! The request head is forwarded byte-for-byte; the body is streamed through
//! with `io::copy` and never buffered in memory.
//!
//! The global switch owns the listener: when `enabled = false` this module
//! creates **no socket at all** (F-H02).

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use crate::config::{GatewayConfig, GatewayRoute};

const MAX_CONNECTIONS: usize = 128;
const MAX_HEAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayStats {
    pub requests: u64,
    pub forwarded: u64,
    pub admin_routes: u64,
    pub not_registered: u64,
    pub unreachable: u64,
    pub rejected_concurrency: u64,
}

struct Shared {
    routes: RwLock<Vec<GatewayRoute>>,
    admin_host: RwLock<String>,
    admin_addr: String,
    timeout: Duration,
    stats: Mutex<GatewayStats>,
    active: AtomicUsize,
    running: AtomicBool,
    on_unreachable: Box<dyn Fn(String) + Send + Sync>,
}

/// Handle to a running gateway listener.
pub struct Gateway {
    shared: Arc<Shared>,
    pub port: u16,
}

fn normalize_host(raw: &str) -> String {
    let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    // Strip an explicit port, but keep IPv6 literals intact.
    if host.starts_with('[') {
        return host;
    }
    match host.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|ch| ch.is_ascii_digit()) => {
            head.to_string()
        }
        _ => host,
    }
}

/// Explicit `gateway.hosts[]` entries win over service-derived routes.
fn route_for(routes: &[GatewayRoute], host: &str) -> Option<GatewayRoute> {
    routes
        .iter()
        .find(|route| normalize_host(&route.host) == host)
        .cloned()
}

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn page(title: &str, body: &str, routes: &[GatewayRoute]) -> String {
    let links: Vec<String> = routes
        .iter()
        .filter(|route| route.proxy)
        .map(|route| {
            let host = html_escape(&normalize_host(&route.host));
            format!(
                "<li><a href=\"/go/?host={0}\">{0}</a><span>{1}</span></li>",
                host,
                html_escape(&route.target)
            )
        })
        .collect();
    format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{0} · Lan Studio</title><style>\
:root{{color-scheme:light dark}}body{{margin:0;min-height:100vh;display:grid;place-items:center;\
font:15px/1.7 system-ui,-apple-system,\"Segoe UI\",sans-serif;background:#f6f7f9;color:#1c2028}}\
@media(prefers-color-scheme:dark){{body{{background:#14171c;color:#e8eaed}}}}\
main{{width:min(560px,92vw);padding:32px;border-radius:18px;background:rgba(255,255,255,.72);\
box-shadow:0 12px 40px rgba(20,30,60,.08)}}@media(prefers-color-scheme:dark){{main{{background:rgba(36,40,48,.72)}}}}\
h1{{font-size:20px;margin:0 0 8px}}p{{color:#6b7280;margin:0 0 20px}}\
ul{{list-style:none;padding:0;margin:0}}li{{display:flex;justify-content:space-between;gap:16px;\
padding:9px 0;border-top:1px solid rgba(127,127,127,.18)}}li span{{color:#6b7280;font-size:13px}}\
a{{color:#2f6feb;text-decoration:none}}code{{font-size:13px}}</style></head>\
<body><main><h1>{0}</h1><p>{1}</p><ul>{2}</ul>\
<p style=\"margin-top:22px;font-size:12px\">Lan Studio · 仅转发已登记域名</p></main></body></html>",
        html_escape(title),
        html_escape(body),
        links.join("")
    )
}

fn write_html(stream: &mut TcpStream, status: u16, html: &str) {
    let reason = match status {
        404 => "Not Found",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{}",
        status,
        reason,
        html.len(),
        html
    );
    let _ = stream.write_all(response.as_bytes());
}

fn read_head(mut stream: &TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buffer: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "empty request",
            ));
        }
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            let tail = buffer.split_off(position + 4);
            return Ok((buffer, tail));
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn first_line(head: &[u8]) -> String {
    let text = String::from_utf8_lossy(head);
    text.lines().next().unwrap_or("").to_string()
}

fn header_value(head: &[u8], wanted: &str) -> String {
    let text = String::from_utf8_lossy(head);
    for line in text.lines().skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case(wanted) {
                return value.trim().to_string();
            }
        }
    }
    String::new()
}

fn connect_target(target: &str, timeout: Duration) -> io::Result<TcpStream> {
    let address: SocketAddr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "target did not resolve"))?;
    let stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

impl Gateway {
    /// Start the listener. Returns `Ok(None)` when the global switch is off.
    pub fn start(
        config: &GatewayConfig,
        admin_addr: &str,
        on_unreachable: impl Fn(String) + Send + Sync + 'static,
    ) -> io::Result<Option<Arc<Self>>> {
        if !config.enabled {
            return Ok(None);
        }
        let listener = TcpListener::bind(format!("{}:{}", config.listen, config.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            routes: RwLock::new(config.routes.clone()),
            admin_host: RwLock::new(config.admin_host.to_ascii_lowercase()),
            admin_addr: admin_addr.to_string(),
            timeout: config.timeout,
            stats: Mutex::new(GatewayStats::default()),
            active: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            on_unreachable: Box::new(on_unreachable),
        });
        let worker = Arc::clone(&shared);
        thread::spawn(move || accept_loop(listener, &worker));
        Ok(Some(Arc::new(Self { shared, port })))
    }

    /// F-G05/F-E03: hot-swap routes and the admin host without rebinding.
    pub fn reconfigure(&self, config: &GatewayConfig) {
        *self.shared.routes.write().unwrap() = config.routes.clone();
        *self.shared.admin_host.write().unwrap() = config.admin_host.to_ascii_lowercase();
    }

    pub fn stats(&self) -> GatewayStats {
        self.shared.stats.lock().unwrap().clone()
    }

    pub fn routes(&self) -> Vec<GatewayRoute> {
        self.shared.routes.read().unwrap().clone()
    }

    pub fn shutdown(&self) {
        self.shared.running.store(false, Ordering::Relaxed);
    }
}

fn accept_loop(listener: TcpListener, shared: &Arc<Shared>) {
    while shared.running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_read_timeout(Some(shared.timeout));
                let _ = stream.set_write_timeout(Some(shared.timeout));
                if shared.active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    shared.stats.lock().unwrap().rejected_concurrency += 1;
                    drop(stream);
                    continue;
                }
                shared.active.fetch_add(1, Ordering::Relaxed);
                let context = Arc::clone(shared);
                thread::spawn(move || {
                    handle_conn(stream, &context);
                    context.active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(30));
            }
            Err(_) => thread::sleep(Duration::from_millis(30)),
        }
    }
}

fn bump(shared: &Shared, field: impl FnOnce(&mut GatewayStats)) {
    let mut stats = shared.stats.lock().unwrap();
    stats.requests += 1;
    field(&mut stats);
}

fn handle_conn(mut client: TcpStream, shared: &Shared) {
    let (head, tail) = match read_head(&client) {
        Ok(value) => value,
        Err(_) => {
            bump(shared, |_| {});
            return;
        }
    };
    let request_line = first_line(&head);
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let host = normalize_host(&header_value(&head, "Host"));
    let routes = shared.routes.read().unwrap().clone();

    // F-H05 helper entry (delivered early; formally signed off in M2.4).
    if path == "/go" || path.starts_with("/go/") || path.starts_with("/go?") {
        let wanted = path
            .split_once("host=")
            .map(|(_, value)| value.split('&').next().unwrap_or(""))
            .unwrap_or("");
        if !wanted.is_empty() {
            let safe = wanted
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_'))
                .collect::<String>();
            if !safe.is_empty() {
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    safe
                );
                let _ = client.write_all(response.as_bytes());
                bump(shared, |_| {});
                return;
            }
        }
        let html = page("局域网服务导航", "选择已登记的服务：", &routes);
        write_html(&mut client, 200, &html);
        bump(shared, |_| {});
        return;
    }

    let admin_host = shared.admin_host.read().unwrap().clone();
    let is_admin = !admin_host.is_empty() && host == admin_host;
    let route = route_for(&routes, &host);
    let target = if is_admin {
        Some(shared.admin_addr.clone())
    } else {
        route
            .as_ref()
            .filter(|item| item.proxy)
            .map(|item| item.target.clone())
    };
    let Some(target) = target else {
        let html = page(
            "域名未登记或代理已关闭",
            &format!(
                "{} 尚未在配置中登记，或该服务的代理开关处于关闭状态。",
                host
            ),
            &routes,
        );
        write_html(&mut client, 404, &html);
        bump(shared, |stats| stats.not_registered += 1);
        return;
    };

    let mut upstream = match connect_target(&target, shared.timeout) {
        Ok(stream) => stream,
        Err(_) => {
            let html = page(
                "目标暂不可达",
                &format!(
                    "{} 当前无法连接（{}），请稍后重试或检查目标服务。",
                    host, target
                ),
                &routes,
            );
            write_html(&mut client, 502, &html);
            bump(shared, |stats| stats.unreachable += 1);
            (shared.on_unreachable)(host);
            return;
        }
    };

    // Forward the untouched request head, then stream the body both ways.
    if upstream.write_all(&head).is_err()
        || (!tail.is_empty() && upstream.write_all(&tail).is_err())
    {
        return;
    }
    let mut upstream_read = match upstream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut client_write = match client.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let reply = thread::spawn(move || {
        let _ = io::copy(&mut upstream_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Write);
    });
    let mut client_read = match client.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let _ = io::copy(&mut client_read, &mut upstream);
    let _ = upstream.shutdown(Shutdown::Write);
    let _ = reply.join();
    bump(shared, |stats| {
        stats.forwarded += 1;
        if is_admin {
            stats.admin_routes += 1;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes() -> Vec<GatewayRoute> {
        vec![
            GatewayRoute {
                host: "NAS.local".into(),
                target: "192.168.5.10:8080".into(),
                proxy: true,
            },
            GatewayRoute {
                host: "box.local".into(),
                target: "192.168.5.11:80".into(),
                proxy: false,
            },
        ]
    }

    #[test]
    fn host_normalization_strips_port_case_and_trailing_dot() {
        assert_eq!(normalize_host("NAS.Local:8080"), "nas.local");
        assert_eq!(normalize_host("nas.local."), "nas.local");
        assert_eq!(normalize_host("  Box.LOCAL  "), "box.local");
    }

    #[test]
    fn disabled_proxy_route_is_not_routable() {
        let list = routes();
        assert_eq!(
            route_for(&list, "nas.local").unwrap().target,
            "192.168.5.10:8080"
        );
        assert!(route_for(&list, "box.local").is_some_and(|route| !route.proxy));
        assert!(route_for(&list, "unknown.local").is_none());
    }

    #[test]
    fn friendly_pages_escape_host_and_list_only_proxied_names() {
        let html = page("域名未登记", "请检查 <script>alert(1)</script>", &routes());
        assert!(html.contains("nas.local"));
        assert!(
            !html.contains(">box.local</a>"),
            "disabled route must not be linked"
        );
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("仅转发已登记域名"));
    }

    #[test]
    fn head_parser_returns_unbuffered_body_prefix() {
        assert_eq!(
            find_subslice(b"GET / HTTP/1.1\r\n\r\nBODY", b"\r\n\r\n"),
            Some(14)
        );
        let head = b"GET /x HTTP/1.1\r\nHost: NAS.local:80\r\nX: y\r\n\r\n";
        assert_eq!(header_value(head, "host"), "NAS.local:80");
        assert_eq!(first_line(head), "GET /x HTTP/1.1");
    }
}
