//! lan-mdns M2.1 — real mDNS / DNS-SD responder.
//!
//! Wires the shared API surface to the protocol engine: multicast receive loop,
//! query answering (QU unicast + Known-Answer suppression), startup/periodic
//! announcements, Goodbye on shutdown, inbound response ingestion, a bounded
//! query audit log, an observed-service map and active probing.
//!
//! The mDNS protocol itself lives in `common-dns` / `common-net` / `engine.rs`;
//! this file is only composition, HTTP routing and graceful lifecycle.

mod config;
mod engine;

use std::fs;
use std::net::Ipv4Addr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common_api::{err, json_quote, ok, App, Context, Request, Response};
use common_config::{apply_flat, config_path_from_env, port_from_env, validate_text, Config};
use common_dns::{
    type_name, validate_name, TYPE_A, TYPE_AAAA, TYPE_ANY, TYPE_PTR, TYPE_SRV, TYPE_TXT,
};
use common_health::Health;
use common_metrics::Registry;
use common_net::list_interfaces;
use common_webui::INDEX_HTML;

use config::MdnsConfig;
use engine::Engine;

/// Compiled-in fallback so the single binary is usable without a config file.
const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");
const PROBE_TIMEOUT_MS: u64 = 800;

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();

#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FLAG: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_: i32) {
        // Only lock-free atomics are async-signal-safe; the watchdog thread
        // performs the actual shutdown and Goodbye broadcast.
        FLAG.store(true, Ordering::Relaxed);
    }

    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }

    pub fn install() {
        let handler: extern "C" fn(i32) = on_signal;
        unsafe {
            signal(2, handler as usize); // SIGINT
            signal(15, handler as usize); // SIGTERM
        }
    }

    pub fn triggered() -> bool {
        FLAG.load(Ordering::Relaxed)
    }
}

#[cfg(not(unix))]
mod signals {
    pub fn install() {}
    pub fn triggered() -> bool {
        false
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = config_path_from_env();
    let (text, common, mut mdns) = match load_and_validate(&path) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{}", common_log::error("lan-mdns", &message));
            std::process::exit(1);
        }
    };

    let version = env!("CARGO_PKG_VERSION");
    let commit = option_env!("GIT_COMMIT").unwrap_or("dev");
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let record_count = mdns.build_records().len();
    let service_count = mdns.services.len();

    if args.iter().any(|arg| arg == "--cli") {
        println!(
            "component=lan-mdns version={} commit={} target={}",
            version, commit, target
        );
        println!(
            "config={} records={} services={} group={} port={}",
            path, record_count, service_count, mdns.group, mdns.port
        );
        println!(
            "{}",
            common_log::info(
                "lan-mdns",
                "cli mode: config validated, no listener started"
            )
        );
        return;
    }

    // Test-isolation overrides are applied only to the running engine, so
    // `--cli` above always reports the config file's own values.
    apply_overrides(&mut mdns);
    let engine = match Engine::start(mdns) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!(
                "{}",
                common_log::error(
                    "lan-mdns",
                    &format!("mDNS engine failed to start: {}", error)
                )
            );
            std::process::exit(1);
        }
    };
    let group = engine.config().group;
    let mdns_port = engine.config().port;
    let _ = ENGINE.set(Arc::clone(&engine));

    let health = Arc::new(Health::new());
    let metrics = Registry::new("lan-mdns");
    metrics.set_build_info(version, commit, &target);
    declare_mdns_metrics(&metrics);

    let ctx = Arc::new(Context {
        component: "lan-mdns".to_string(),
        version: version.to_string(),
        started: Instant::now(),
        metrics,
        health: Arc::clone(&health),
        admin_token: common.admin_token.clone(),
        config_raw: text,
        webui: common.webui,
    });

    let app = App::new()
        .route("GET", "/", view_index)
        .route("GET", "/healthz", view_healthz)
        .route("GET", "/readyz", view_readyz)
        .route("GET", "/api/status", view_status)
        .route("GET", "/api/metrics", view_metrics)
        .route("GET", "/api/config", view_config)
        .route("GET", "/api/auth", view_auth)
        .route("GET", "/api/records", view_records)
        .route("GET", "/api/interfaces", view_interfaces)
        .route("GET", "/api/queries", view_queries)
        .route("GET", "/api/browse", view_browse)
        .route("POST", "/api/probe", view_probe);

    signals::install();
    spawn_shutdown_watchdog();

    let addr = format!("{}:{}", common.listen, port_from_env());
    health.mark_ready();
    eprintln!(
        "{}",
        common_log::info(
            "lan-mdns",
            &format!(
                "mDNS {}:{} ready, HTTP {}, {} records / {} services",
                group, mdns_port, addr, record_count, service_count
            )
        )
    );
    if let Err(error) = app.serve(&addr, ctx) {
        eprintln!(
            "{}",
            common_log::error("lan-mdns", &format!("server failed: {}", error))
        );
        std::process::exit(1);
    }
}

/// Read, offline-validate and split a config into its common and component
/// namespaces. No socket is touched here (F-E02).
fn load_and_validate(path: &str) -> Result<(String, Config, MdnsConfig), String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            eprintln!(
                "{}",
                common_log::warning(
                    "lan-mdns",
                    &format!("config {} not found, using compiled-in default", path)
                )
            );
            DEFAULT_CONFIG.to_string()
        }
    };
    let flat = validate_text(&text).map_err(|e| format!("common config invalid: {}", e))?;
    let mdns = MdnsConfig::from_flat(&flat).map_err(|e| format!("mdns config invalid: {}", e))?;
    let mut common = Config {
        raw: text.clone(),
        ..Config::default()
    };
    apply_flat(&mut common, &flat);
    Ok((text, common, mdns))
}

/// Test-isolation hooks. Production keeps the frozen YAML schema untouched;
/// these env vars only exist so CI / local smoke tests never emit real mDNS
/// traffic onto the LAN (see docs/ADR/0002).
fn apply_overrides(mdns: &mut MdnsConfig) {
    if let Some(port) = std::env::var("LAN_MDNS_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
    {
        if port > 0 {
            mdns.port = port;
        }
    }
    if let Some(group) = std::env::var("LAN_MDNS_GROUP")
        .ok()
        .and_then(|value| value.parse::<Ipv4Addr>().ok())
    {
        if group.is_multicast() {
            mdns.group = group;
        }
    }
}

fn spawn_shutdown_watchdog() {
    std::thread::spawn(|| loop {
        if signals::triggered() {
            if let Some(engine) = ENGINE.get() {
                engine.shutdown();
            }
            eprintln!(
                "{}",
                common_log::info("lan-mdns", "shutdown signal received, goodbye sent")
            );
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(100));
    });
}

fn declare_mdns_metrics(metrics: &Registry) {
    metrics.declare_counter("queries_total", "mDNS queries received");
    metrics.declare_counter("responses_total", "mDNS responses sent");
    metrics.declare_counter("announces_total", "mDNS proactive announcements");
    metrics.declare_counter("probe_total", "mDNS active probes by result");
    metrics.declare_gauge("records", "Published mDNS records by type");
    metrics.declare_gauge("query_audit_entries", "Query audit ring buffer size");
    metrics.declare_gauge("browse_instances", "Observed LAN service instances");
}

// ---------------------------------------------------------------- handlers

fn view_healthz(_: &Request, _: &Context) -> Response {
    Response::json(200, "{\"ok\":true}".to_string())
}

fn view_readyz(req: &Request, ctx: &Context) -> Response {
    if ctx.health.is_ready() && ENGINE.get().is_some() {
        ok(req, "{\"ready\":true}")
    } else {
        err(req, "internal_error", "not ready", 503)
    }
}

fn view_status(req: &Request, ctx: &Context) -> Response {
    let uptime = ctx.started.elapsed().as_secs_f64();
    let (protocol, records, stats, group, mdns_port, interface_mode) = match ENGINE.get() {
        Some(engine) => {
            let cfg = engine.config();
            (
                "mdns/1".to_string(),
                engine.records_snapshot().len(),
                engine.stats_snapshot(),
                cfg.group.to_string(),
                cfg.port,
                cfg.interface_mode.clone(),
            )
        }
        None => (
            "degraded".to_string(),
            0,
            Default::default(),
            String::new(),
            0,
            String::new(),
        ),
    };
    let data = format!(
        "{{\"component\":{},\"version\":{},\"running\":true,\"uptime\":{:.3},\"protocol\":{},\"records\":{},\"group\":{},\"mdns_port\":{},\"interface_mode\":{},\"queries\":{},\"responses\":{},\"ingested\":{},\"announces\":{},\"malformed\":{}}}",
        json_quote(&ctx.component),
        json_quote(&ctx.version),
        uptime,
        json_quote(&protocol),
        records,
        json_quote(&group),
        mdns_port,
        json_quote(&interface_mode),
        stats.queries,
        stats.responses,
        stats.ingested,
        stats.announces,
        stats.malformed,
    );
    ok(req, &data)
}

fn view_metrics(_: &Request, ctx: &Context) -> Response {
    ctx.metrics
        .set("uptime_seconds", &[], ctx.started.elapsed().as_secs_f64());
    if let Some(engine) = ENGINE.get() {
        ctx.metrics.set("up", &[], 1.0);
        let stats = engine.stats_snapshot();
        ctx.metrics.set_counter("queries_total", &[], stats.queries);
        ctx.metrics
            .set_counter("responses_total", &[], stats.responses);
        ctx.metrics
            .set_counter("announces_total", &[], stats.announces);
        let records = engine.records_snapshot();
        ctx.metrics.set("records", &[], records.len() as f64);
        for rtype in [TYPE_A, TYPE_AAAA, TYPE_PTR, TYPE_SRV, TYPE_TXT] {
            let count = records.iter().filter(|rr| rr.rtype == rtype).count();
            ctx.metrics
                .set("records", &[("type", type_name(rtype))], count as f64);
        }
        ctx.metrics.set(
            "query_audit_entries",
            &[],
            engine.queries_snapshot().len() as f64,
        );
        ctx.metrics.set(
            "browse_instances",
            &[],
            engine.observed_snapshot().len() as f64,
        );
    } else {
        ctx.metrics.set("up", &[], 0.0);
    }
    Response {
        status: 200,
        content_type: "text/plain; version=0.0.4; charset=utf-8".to_string(),
        body: ctx.metrics.render(),
    }
}

fn view_config(req: &Request, ctx: &Context) -> Response {
    let data = format!("{{\"raw\":{}}}", json_quote(&ctx.config_raw));
    ok(req, &data)
}

fn view_auth(req: &Request, ctx: &Context) -> Response {
    let data = format!("{{\"token_required\":{}}}", !ctx.admin_token.is_empty());
    ok(req, &data)
}

fn view_index(_: &Request, ctx: &Context) -> Response {
    if ctx.webui {
        Response::html(200, INDEX_HTML.to_string())
    } else {
        Response::text(404, "webui disabled".to_string())
    }
}

fn engine_or_503(req: &Request) -> Result<&'static Arc<Engine>, Response> {
    ENGINE
        .get()
        .ok_or_else(|| err(req, "internal_error", "mDNS engine not running", 503))
}

fn view_records(req: &Request, _: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let items: Vec<String> = engine
        .records_snapshot()
        .iter()
        .map(|rr| {
            format!(
                "{{\"name\":{},\"type\":{},\"ttl\":{},\"value\":{},\"cache_flush\":{}}}",
                json_quote(&rr.name),
                json_quote(type_name(rr.rtype)),
                rr.ttl,
                json_quote(&engine::rdata_text(rr)),
                rr.cache_flush()
            )
        })
        .collect();
    let total = items.len();
    ok(
        req,
        &format!("{{\"records\":[{}],\"total\":{}}}", items.join(","), total),
    )
}

fn view_interfaces(req: &Request, _: &Context) -> Response {
    let items: Vec<String> = list_interfaces()
        .iter()
        .map(|info| {
            format!(
                "{{\"name\":{},\"addr\":{}}}",
                json_quote(&info.name),
                json_quote(&info.addr.to_string())
            )
        })
        .collect();
    let total = items.len();
    ok(
        req,
        &format!(
            "{{\"interfaces\":[{}],\"total\":{}}}",
            items.join(","),
            total
        ),
    )
}

fn view_queries(req: &Request, _: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let limit = query_param(&req.query, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let mut entries = engine.queries_snapshot();
    let total = entries.len();
    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }
    let items: Vec<String> = entries
        .iter()
        .map(|entry| {
            format!(
                "{{\"ts\":{:.3},\"source\":{},\"name\":{},\"qtype\":{}}}",
                entry.ts,
                json_quote(&entry.source),
                json_quote(&entry.name),
                json_quote(&entry.qtype)
            )
        })
        .collect();
    ok(
        req,
        &format!("{{\"queries\":[{}],\"total\":{}}}", items.join(","), total),
    )
}

fn view_browse(req: &Request, _: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let items: Vec<String> = engine
        .observed_snapshot()
        .iter()
        .map(|entry| {
            format!(
                "{{\"ts\":{:.3},\"source\":{},\"name\":{},\"type\":{},\"value\":{}}}",
                entry.ts,
                json_quote(&entry.source),
                json_quote(&entry.name),
                json_quote(type_name(entry.rtype)),
                json_quote(&entry.value)
            )
        })
        .collect();
    let total = items.len();
    ok(
        req,
        &format!("{{\"browse\":[{}],\"total\":{}}}", items.join(","), total),
    )
}

fn view_probe(req: &Request, ctx: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let name = json_string_field(&req.body, "name")
        .or_else(|| query_param(&req.query, "name"))
        .unwrap_or_default();
    if name.is_empty() {
        return err(req, "invalid_config", "probe name is required", 400);
    }
    if validate_name(&name).is_err() {
        return err(req, "invalid_config", "probe name is invalid", 400);
    }
    let type_text = json_string_field(&req.body, "type")
        .or_else(|| query_param(&req.query, "type"))
        .unwrap_or_else(|| "PTR".to_string());
    let qtype = parse_qtype(&type_text);
    let unicast = json_bool_field(&req.body, "unicast")
        .or_else(|| query_param(&req.query, "unicast").map(|v| v == "1" || v == "true"))
        .unwrap_or(false);
    match engine.probe(&name, qtype, unicast, PROBE_TIMEOUT_MS) {
        Ok(answers) => {
            let result = if answers.is_empty() { "empty" } else { "ok" };
            ctx.metrics.inc("probe_total", &[("result", result)], 1);
            let items: Vec<String> = answers
                .iter()
                .map(|rr| {
                    format!(
                        "{{\"name\":{},\"type\":{},\"ttl\":{},\"value\":{}}}",
                        json_quote(&rr.name),
                        json_quote(type_name(rr.rtype)),
                        rr.ttl,
                        json_quote(&engine::rdata_text(rr))
                    )
                })
                .collect();
            let total = items.len();
            ok(
                req,
                &format!(
                    "{{\"name\":{},\"type\":{},\"answers\":[{}],\"total\":{}}}",
                    json_quote(&name),
                    json_quote(type_name(qtype)),
                    items.join(","),
                    total
                ),
            )
        }
        Err(_) => {
            ctx.metrics.inc("probe_total", &[("result", "failed")], 1);
            err(req, "internal_error", "probe failed", 500)
        }
    }
}

// ----------------------------------------------------------- tiny parsers

fn parse_qtype(text: &str) -> u16 {
    match text.trim().to_ascii_uppercase().as_str() {
        "A" => TYPE_A,
        "AAAA" => TYPE_AAAA,
        "PTR" => TYPE_PTR,
        "SRV" => TYPE_SRV,
        "TXT" => TYPE_TXT,
        "ANY" => TYPE_ANY,
        other => other.parse::<u16>().unwrap_or(TYPE_PTR),
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hi = (bytes[index + 1] as char).to_digit(16);
            let lo = (bytes[index + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                index += 3;
                continue;
            }
        }
        out.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            Some(percent_decode(v))
        } else {
            None
        }
    })
}

fn json_value_start<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let colon = rest.find(':')?;
    Some(rest[colon + 1..].trim_start())
}

fn json_string_field(body: &str, key: &str) -> Option<String> {
    let rest = json_value_start(body, key)?;
    if !rest.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for ch in rest[1..].chars() {
        if escaped {
            out.push(match ch {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

fn json_bool_field(body: &str, key: &str) -> Option<bool> {
    let rest = json_value_start(body, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_body_and_query() {
        let body = r#"{"name":"nas.local","type":"A","unicast":true}"#;
        assert_eq!(
            json_string_field(body, "name").as_deref(),
            Some("nas.local")
        );
        assert_eq!(json_string_field(body, "type").as_deref(), Some("A"));
        assert_eq!(json_bool_field(body, "unicast"), Some(true));
        assert_eq!(parse_qtype("aaaa"), TYPE_AAAA);
        assert_eq!(parse_qtype("ANY"), TYPE_ANY);
        assert_eq!(parse_qtype("nonsense"), TYPE_PTR);
    }

    #[test]
    fn query_params_are_decoded() {
        let query = "name=_http._tcp.local&type=PTR&limit=10";
        assert_eq!(
            query_param(query, "name").as_deref(),
            Some("_http._tcp.local")
        );
        assert_eq!(query_param(query, "type").as_deref(), Some("PTR"));
        assert_eq!(query_param(query, "limit").as_deref(), Some("10"));
        assert_eq!(query_param(query, "missing"), None);
        assert_eq!(percent_decode("a%2Elocal+b"), "a.local b");
    }

    #[test]
    fn invalid_probe_names_are_rejected() {
        assert!(validate_name("nas.local").is_ok());
        assert!(validate_name("bad").is_err());
    }
}
