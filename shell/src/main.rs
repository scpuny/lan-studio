//! Lan Studio Shell — unified navigation exit (M5 aggregation layer).
//!
//! This binary is deliberately NOT a `lan-*` component: it does not go through
//! `common_component::run`, because that would bind `GET /` to the six-region
//! component console and register the 18 standard endpoints. Shell is a thin
//! navigation layer with exactly three routes:
//!
//! - `GET /`                  single-file Calm Console with eight cards
//! - `GET /healthz`           liveness (plain JSON, not the unified envelope)
//! - `GET /api/shell/status`  bounded component online state
//!
//! Security boundary: Shell only *forwards* an optional AdminToken to the
//! component pages in the browser. It never stores an administrator password.
//!
//! M6 adds a bounded same-origin proxy (`/p/<name>/api/...`) so the aggregated
//! SPA can reach every component without cross-origin requests. The proxy is
//! component- and endpoint-allowlisted, timeout- and size-bounded, and never
//! stores credentials: `X-Admin-Password` is forwarded verbatim to the target,
//! which enforces its own write guard. Shell still does no service discovery
//! and does not act as a data bus.

use common_api::{err, json_quote, ok, App, Context, Health, Registry, Request, Response};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SHELL_HTML: &str = include_str!("assets/shell.html");
/// One renderer source, shared with every component console (zero build chain).
const RENDERER_JS: &str = include_str!("../../common/component/src/assets/console-renderer.js");
/// Capability manifests are the single source of truth for the aggregated UI.
const CAPABILITIES: [&str; 8] = [
    include_str!("../../contracts/examples/capabilities.lan-mdns.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-relay.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-ca.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-scan.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-hosts.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-wol.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-sync.sample.json"),
    include_str!("../../contracts/examples/capabilities.lan-ssdp.sample.json"),
];
/// `name|path` lines generated from the manifests; keeps the Rust side free of
/// a JSON parser and prevents forwarding to undeclared endpoints.
const ALLOWLIST: &str = include_str!("../../contracts/capabilities.allowlist.txt");
/// Bounded, but long enough for a write request whose admin-password KDF runs
/// in a debug build. Health probes keep their own 300ms budget.
const PROXY_TIMEOUT_MS: u64 = 5_000;
const PROXY_MAX_BYTES: usize = 1 << 20;
const DEFAULT_PORT: u16 = 8090;
const DEFAULT_MANIFEST: &str = "shell/components.yaml";
const MAX_COMPONENTS: usize = 32;
const PROBE_CONCURRENCY: usize = 4;
const TONES: [&str; 8] = [
    "blue", "cyan", "purple", "teal", "indigo", "amber", "green", "orange",
];

const HELP: &str = "\
Lan Studio Shell — unified navigation exit (aggregation layer, not a component)

Usage: lan-shell [OPTIONS]

Options:
  --check     Parse the manifest and print a JSON summary, then exit
  --version   Print the version and exit
  --help      Print this help and exit

Routes:
  GET  /                       aggregated SPA (8 components, capability-driven)
  GET  /healthz                liveness
  GET  /api/shell/status       bounded online state
  GET|POST /p/<name>/api/...   same-origin proxy, allowlisted + bounded

Environment:
  LAN_SHELL_CONFIG   manifest path (default: shell/components.yaml)
  LAN_STUDIO_PORT    listen port   (default: 8090)
  LAN_SHELL_TOKEN    optional admin token required for /api/* and /p/* (default: empty)
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub label: String,
    pub port: u16,
    pub icon: String,
    pub tone: String,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub version: u64,
    pub probe_enabled: bool,
    pub probe_interval_seconds: u64,
    pub probe_timeout_ms: u64,
    pub components: Vec<Component>,
}

impl Manifest {
    fn defaults() -> Self {
        Self {
            version: 1,
            probe_enabled: true,
            probe_interval_seconds: 15,
            probe_timeout_ms: 300,
            components: Vec::new(),
        }
    }
}

/// Shared, bounded probe result. `None` means "not checked yet".
pub struct ProbeState {
    online: Mutex<HashMap<String, Option<bool>>>,
    checked_at: Mutex<Option<String>>,
}

static STATE: OnceLock<Arc<ProbeState>> = OnceLock::new();

// ---------------------------------------------------------------- manifest

fn split_kv(text: &str) -> Option<(&str, String)> {
    let (key, rest) = text.split_once(':')?;
    let mut value = rest.trim().to_string();
    if let Some(pos) = value.find(" #") {
        value.truncate(pos);
    }
    let value = value.trim().trim_matches('"').trim_matches('\'').trim();
    Some((key.trim(), value.to_string()))
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

fn push_component(manifest: &mut Manifest, item: Component) -> Result<(), String> {
    if item.name.is_empty() || item.name.len() > 64 {
        return Err("manifest component name must be 1..64 bytes".to_string());
    }
    if item.port == 0 {
        return Err(format!("manifest component {} has no port", item.name));
    }
    if !TONES.contains(&item.tone.as_str()) {
        return Err(format!("manifest component {} has unknown tone", item.name));
    }
    if manifest.components.len() >= MAX_COMPONENTS {
        return Err("manifest exceeds MAX_COMPONENTS".to_string());
    }
    manifest.components.push(item);
    Ok(())
}

/// Minimal line-based YAML reader for `shell/components.yaml` (object arrays
/// are intentionally out of scope for `common_config::parse_flat`).
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut manifest = Manifest::defaults();
    let mut current: Option<Component> = None;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("- name:") {
            if let Some(item) = current.take() {
                push_component(&mut manifest, item)?;
            }
            let name = value.trim().to_string();
            current = Some(Component {
                name: name.clone(),
                label: name,
                port: 0,
                icon: "cube".to_string(),
                tone: "blue".to_string(),
            });
            continue;
        }
        // Nested `- <module-id>` lines under `modules:` are capability
        // metadata consumed by scripts/gen_capabilities.py, not components.
        if trimmed.starts_with("- ") {
            continue;
        }
        let (key, value) = match split_kv(trimmed) {
            Some(pair) => pair,
            None => continue,
        };
        if let Some(item) = current.as_mut() {
            match key {
                "name" => item.name = value,
                "label" => item.label = value,
                "port" => {
                    item.port = value
                        .parse::<u16>()
                        .map_err(|_| format!("invalid port for {}", item.name))?;
                }
                "icon" => item.icon = value,
                "tone" => item.tone = value,
                _ => {}
            }
        } else {
            match key {
                "version" => {
                    if let Some(v) = parse_u64(&value) {
                        manifest.version = v;
                    }
                }
                "probe_enabled" => manifest.probe_enabled = value == "true",
                "probe_interval_seconds" => {
                    if let Some(v) = parse_u64(&value) {
                        manifest.probe_interval_seconds = v;
                    }
                }
                "probe_timeout_ms" => {
                    if let Some(v) = parse_u64(&value) {
                        manifest.probe_timeout_ms = v;
                    }
                }
                "components" => {}
                _ => {}
            }
        }
    }
    if let Some(item) = current.take() {
        push_component(&mut manifest, item)?;
    }
    Ok(manifest)
}

// ------------------------------------------------------------------ helpers

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day), Howard Hinnant's civil calendar.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + (if mp < 10 { 3 } else { -9 });
    let year = y + (if month <= 2 { 1 } else { 0 });
    (year, month, day)
}

fn component_json(item: &Component) -> String {
    format!(
        "{{\"name\":{},\"label\":{},\"port\":{},\"icon\":{},\"tone\":{}}}",
        json_quote(&item.name),
        json_quote(&item.label),
        item.port,
        json_quote(&item.icon),
        json_quote(&item.tone)
    )
}

fn capabilities_json() -> String {
    let items: Vec<&str> = CAPABILITIES.iter().map(|text| text.trim()).collect();
    format!("[{}]", items.join(","))
}

fn manifest_data_json(manifest: &Manifest) -> String {
    let items: Vec<String> = manifest.components.iter().map(component_json).collect();
    format!(
        "{{\"version\":{},\"probe_enabled\":{},\"probe_interval_seconds\":{},\"components\":[{}],\"capabilities\":{}}}",
        manifest.version,
        manifest.probe_enabled,
        manifest.probe_interval_seconds,
        items.join(","),
        capabilities_json()
    )
}

// ------------------------------------------------------------------- probing

fn probe_healthz(port: u16, timeout_ms: u64) -> bool {
    let timeout = Duration::from_millis(timeout_ms.clamp(50, 5_000));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    if stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    matches!(reader.read_line(&mut first), Ok(n) if n > 0 && first.contains(" 200 "))
}

fn probe_once(manifest: &Manifest, state: &ProbeState, metrics: &Registry) {
    let results: Mutex<HashMap<String, Option<bool>>> = Mutex::new(HashMap::new());
    for chunk in manifest.components.chunks(PROBE_CONCURRENCY) {
        let results = &results;
        std::thread::scope(|scope| {
            for item in chunk {
                let timeout = manifest.probe_timeout_ms;
                scope.spawn(move || {
                    let online = probe_healthz(item.port, timeout);
                    results
                        .lock()
                        .unwrap()
                        .insert(item.name.clone(), Some(online));
                });
            }
        });
    }
    let mut table = results.into_inner().unwrap_or_default();
    let online_count = table.values().filter(|v| **v == Some(true)).count();
    // Keep an explicit entry for every manifest component, even when disabled.
    for item in &manifest.components {
        table.entry(item.name.clone()).or_insert(None);
    }
    *state.online.lock().unwrap() = table;
    *state.checked_at.lock().unwrap() = Some(now_rfc3339());
    metrics.set("components_online", &[], online_count as f64);
}

fn spawn_probe_loop(manifest: Manifest, state: Arc<ProbeState>, metrics: Registry) {
    std::thread::spawn(move || loop {
        if manifest.probe_enabled {
            probe_once(&manifest, &state, &metrics);
        }
        let interval = manifest.probe_interval_seconds.clamp(5, 3_600);
        std::thread::sleep(Duration::from_secs(interval));
    });
}

// -------------------------------------------------------------------- routes

fn view_index(_req: &Request, ctx: &Context) -> Response {
    let manifest = match parse_manifest(&ctx.config_raw) {
        Ok(manifest) => manifest,
        Err(_) => Manifest::defaults(),
    };
    let html = SHELL_HTML
        .replace("__SHELL_DATA__", &manifest_data_json(&manifest))
        .replace("__RENDERER_JS__", RENDERER_JS);
    Response::html(200, html)
}

/// Same-origin, allowlisted, bounded proxy for the aggregated SPA.
/// The target host is always 127.0.0.1:<manifest port>; credentials are only
/// forwarded, never stored, and the write guard stays in the target component.
fn view_proxy(req: &Request, ctx: &Context) -> Response {
    if common_auth::check(
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
    let manifest = match parse_manifest(&ctx.config_raw) {
        Ok(manifest) => manifest,
        Err(error) => return err(req, "internal_error", &error, 500),
    };
    let component = match manifest.components.iter().find(|c| c.name == name) {
        Some(component) => component,
        None => return err(req, "not_found", "unknown component", 404),
    };
    let target_path = format!("/{tail}");
    let key = format!("{name}|{target_path}");
    if !ALLOWLIST.lines().any(|line| line == key) {
        return err(
            req,
            "not_found",
            "endpoint not declared in capabilities",
            404,
        );
    }

    let timeout = Duration::from_millis(PROXY_TIMEOUT_MS);
    let addr = SocketAddr::from(([127, 0, 0, 1], component.port));
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
        req.method, target_path, query, component.port
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

fn view_healthz(_req: &Request, _ctx: &Context) -> Response {
    Response::json(200, "{\"ok\":true}".to_string())
}

fn bool_json(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

fn view_status(req: &Request, ctx: &Context) -> Response {
    let manifest = match parse_manifest(&ctx.config_raw) {
        Ok(manifest) => manifest,
        Err(error) => return err(req, "internal_error", &error, 500),
    };
    let state = STATE.get();
    let checked_at = state
        .and_then(|s| s.checked_at.lock().unwrap().clone())
        .map(|s| json_quote(&s))
        .unwrap_or_else(|| "null".to_string());
    // Take one snapshot: holding a MutexGuard while locking the same mutex
    // again (for the online count) would deadlock the request thread.
    let online = state
        .map(|s| s.online.lock().unwrap().clone())
        .unwrap_or_default();
    let items: Vec<String> = manifest
        .components
        .iter()
        .map(|item| {
            let value = online.get(&item.name).copied().unwrap_or(None);
            format!(
                "{{\"name\":{},\"label\":{},\"port\":{},\"icon\":{},\"tone\":{},\"online\":{},\"checked_at\":{}}}",
                json_quote(&item.name),
                json_quote(&item.label),
                item.port,
                json_quote(&item.icon),
                json_quote(&item.tone),
                bool_json(value),
                checked_at
            )
        })
        .collect();
    let online_count = online.values().filter(|v| **v == Some(true)).count();
    ok(
        req,
        &format!(
            "{{\"components\":[{}],\"total\":{},\"online\":{},\"probe_enabled\":{},\"token_hint\":false}}",
            items.join(","),
            items.len(),
            online_count,
            manifest.probe_enabled
        ),
    )
}

// ---------------------------------------------------------------------- main

fn test_context(text: &str) -> Context {
    Context {
        component: "lan-shell".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        started: Instant::now(),
        metrics: Registry::new("lan-shell"),
        health: Arc::new(Health::new()),
        admin_token: String::new(),
        config_raw: text.to_string(),
        webui: true,
        events: None,
        password: None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let version = env!("CARGO_PKG_VERSION");
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("lan-shell {version}");
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return;
    }
    let path = std::env::var("LAN_SHELL_CONFIG").unwrap_or_else(|_| DEFAULT_MANIFEST.to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("cannot read manifest {path}: {error}");
            std::process::exit(2);
        }
    };
    let manifest = match parse_manifest(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("invalid manifest {path}: {error}");
            std::process::exit(2);
        }
    };
    if args.iter().any(|a| a == "--check") {
        println!(
            "{{\"ok\":true,\"version\":{},\"components\":{},\"probe_enabled\":{}}}",
            manifest.version,
            manifest.components.len(),
            manifest.probe_enabled
        );
        return;
    }
    let port = std::env::var("LAN_STUDIO_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let token = std::env::var("LAN_SHELL_TOKEN").unwrap_or_default();
    let state = Arc::new(ProbeState {
        online: Mutex::new(HashMap::new()),
        checked_at: Mutex::new(None),
    });
    let _ = STATE.set(Arc::clone(&state));
    let metrics = Registry::new("lan-shell");
    metrics.declare_gauge("components_online", "Components reachable via /healthz");
    if manifest.probe_enabled {
        spawn_probe_loop(manifest.clone(), Arc::clone(&state), metrics.clone());
    }
    let ctx = Arc::new(Context {
        // The token lives only in the request guard; it is never echoed by the API.
        admin_token: token,
        metrics,
        ..test_context(&text)
    });
    let app = App::new()
        .route("GET", "/", view_index)
        .route("GET", "/healthz", view_healthz)
        .route("GET", "/api/shell/status", view_status)
        .route_prefix("GET", "/p/", view_proxy)
        .route_prefix("POST", "/p/", view_proxy);
    let addr = format!("127.0.0.1:{port}");
    if let Err(error) = app.serve(&addr, ctx) {
        eprintln!("lan-shell failed to listen on {addr}: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = include_str!("../components.yaml");

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

    #[test]
    fn manifest_parses_eight_components_with_contiguous_ports() {
        let manifest = parse_manifest(MANIFEST).unwrap();
        assert_eq!(manifest.components.len(), 8);
        assert_eq!(manifest.components[0].name, "lan-mdns");
        assert_eq!(manifest.components[7].name, "lan-ssdp");
        for (index, item) in manifest.components.iter().enumerate() {
            assert_eq!(item.port, 8080 + index as u16);
            assert!(!item.label.is_empty());
            assert!(TONES.contains(&item.tone.as_str()));
        }
        assert!(manifest.probe_enabled);
        assert_eq!(manifest.probe_interval_seconds, 15);
    }

    #[test]
    fn manifest_tolerates_missing_optional_fields() {
        let text = "version: 1\ncomponents:\n  - name: demo\n    port: 9999\n";
        let manifest = parse_manifest(text).unwrap();
        assert_eq!(manifest.components.len(), 1);
        assert_eq!(manifest.components[0].label, "demo");
        assert_eq!(manifest.components[0].tone, "blue");
    }

    #[test]
    fn manifest_rejects_invalid_ports_and_tones() {
        assert!(parse_manifest("components:\n  - name: demo\n").is_err());
        assert!(
            parse_manifest("components:\n  - name: demo\n    port: 8080\n    tone: neon\n")
                .is_err()
        );
    }

    #[test]
    fn healthz_is_plain_ok_json() {
        let ctx = test_context(MANIFEST);
        let response = view_healthz(&request("/healthz"), &ctx);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
    }

    #[test]
    fn status_never_echoes_credentials() {
        let ctx = test_context(MANIFEST);
        let response = view_status(&request("/api/shell/status"), &ctx);
        assert_eq!(response.status, 200);
        assert!(response.body.contains("\"total\":8"));
        assert!(response.body.contains("lan-ssdp"));
        for forbidden in ["password", "shared_secret", "\"token\"", "admin_token"] {
            assert!(!response.body.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn index_is_a_self_contained_single_file() {
        let ctx = test_context(MANIFEST);
        let response = view_index(&request("/"), &ctx);
        assert_eq!(response.status, 200);
        let html = response.body;
        assert!(!html.contains("__SHELL_DATA__"));
        for name in [
            "lan-mdns",
            "lan-relay",
            "lan-ca",
            "lan-scan",
            "lan-hosts",
            "lan-wol",
            "lan-sync",
            "lan-ssdp",
        ] {
            assert!(html.contains(name), "missing {name}");
        }
        for forbidden in [
            "<script src=",
            "src=\"http",
            "url(http",
            "<link rel=\"stylesheet\"",
            "type=\"password\"",
        ] {
            assert!(
                !html.contains(forbidden),
                "external resource or password field: {forbidden}"
            );
        }
        // M6: the eight cards became a capability-driven single-page console.
        assert!(html.contains("ls-pill"));
        assert!(html.contains("window.LanConsole"));
        assert!(html.contains("\"capabilities\":["));
        assert!(!html.contains("__RENDERER_JS__"));
        assert!(html.contains("localStorage"));
    }

    #[test]
    fn check_mode_only_parses_the_manifest() {
        // `--check` is handled before any listener is created; parsing twice
        // must be side-effect free and deterministic.
        let first = parse_manifest(MANIFEST).unwrap();
        let second = parse_manifest(MANIFEST).unwrap();
        assert_eq!(first.components.len(), second.components.len());
        let summary = format!(
            "{{\"ok\":true,\"version\":{},\"components\":{}}}",
            first.version,
            first.components.len()
        );
        assert!(summary.contains("\"ok\":true"));
        assert!(summary.contains("\"components\":8"));
    }

    #[test]
    fn rfc3339_is_well_formed_utc() {
        let stamp = now_rfc3339();
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
        assert!(stamp.contains('T'));
    }
}
