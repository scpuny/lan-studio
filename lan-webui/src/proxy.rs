//! Same-origin, allowlisted, bounded proxy for the aggregated console.
//!
//! `GET|POST /p/<name>/api/...` forwards to `127.0.0.1:<component port>`. The
//! target set and endpoint allowlist come from `modules.yaml` + the generated
//! `capabilities.allowlist.txt` (D-M7-1 / D-M7-6): the proxy never discovers
//! services, never follows redirects, never caches credentials, and only
//! forwards `X-Admin-Password` / `X-Admin-Token` — the write guard stays in the
//! target component.

use common_api::{err, Request, Response};
use common_auth::check;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::capabilities::{allowlist_has, parse_modules};

pub const PROXY_TIMEOUT_MS: u64 = 5_000;
pub const PROXY_MAX_BYTES: usize = 1 << 20;

pub fn view_proxy(req: &Request, ctx: &common_api::Context) -> Response {
    if check(
        &req.headers,
        &req.query,
        &ctx.admin_token,
        req.peer.ip().is_loopback(),
    )
    .is_err()
    {
        return err(req, "auth_failed", "invalid admin token", 403);
    }
    let rest = req.path.strip_prefix("/p/").unwrap_or("");
    let mut parts = rest.splitn(2, '/');
    let name = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("");
    if name.is_empty() || !tail.starts_with("api/") || tail.contains("..") || tail.len() > 256 {
        return err(req, "not_found", "unknown proxy target", 404);
    }
    let modules = match parse_modules(&ctx.config_raw) {
        Ok(modules) => modules,
        Err(error) => return err(req, "internal_error", &error, 500),
    };
    let port = match modules.components.iter().find(|c| c.name == name) {
        Some(component) => component.port,
        None => return err(req, "not_found", "unknown component", 404),
    };
    let target_path = format!("/{tail}");
    if !allowlist_has(name, &target_path) {
        return err(
            req,
            "not_found",
            "endpoint not declared in capabilities",
            404,
        );
    }

    let timeout = Duration::from_millis(PROXY_TIMEOUT_MS);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(stream) => stream,
        Err(_) => return err(req, "target_unreachable", "component is offline", 502),
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return err(
            req,
            "target_unreachable",
            "socket timeout setup failed",
            502,
        );
    }
    let query = if req.query.is_empty() {
        String::new()
    } else {
        format!("?{}", req.query)
    };
    let body = if req.method == "POST" {
        req.body.as_str()
    } else {
        ""
    };
    let mut raw = format!(
        "{} {}{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n",
        req.method, target_path, query, port
    );
    for (header, forwarded) in [
        ("content-type", "Content-Type"),
        ("x-admin-password", "X-Admin-Password"),
        ("x-admin-token", "X-Admin-Token"),
    ] {
        if let Some(value) = req.headers.get(header) {
            raw.push_str(&format!("{forwarded}: {value}\r\n"));
        }
    }
    raw.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
    if stream.write_all(raw.as_bytes()).is_err() {
        return err(req, "target_unreachable", "forward failed", 502);
    }

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > PROXY_MAX_BYTES {
                    return err(req, "internal_error", "response too large", 502);
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let split = text.find("\r\n\r\n").unwrap_or(text.len());
    let (head, remainder) = text.split_at(split);
    let body_text = remainder.strip_prefix("\r\n\r\n").unwrap_or(remainder);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(502);
    let content_type = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_else(|| "application/json; charset=utf-8".to_string());
    Response::body(status, &content_type, body_text.to_string())
}
