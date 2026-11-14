//! lan-mdns M2.3 — real mDNS / DNS-SD responder with observability, security,
//! configuration lifecycle, alerting, a static Host gateway and deployment
//! subcommands.
//!
//! M2.1 owns the protocol kernel (`common-dns` / `common-net` / `engine.rs`);
//! this file is composition, HTTP routing, config lifecycle and graceful
//! shutdown. See `docs/ADR/0003` and `docs/ADR/0004`.

mod config;
mod engine;
mod gateway;
mod state;
mod store;

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common_alert::{AlertConfig, AlertEvent, DefaultSender, Dispatcher};
use common_api::{err, json_quote, ok, App, Context, Request, Response, SseSpec};
use common_auth::{PasswordGuard, PasswordManager};
use common_config::{
    apply_flat, config_path_from_env, get_bool, get_str, get_u16, get_u32, get_u64, list_len,
    port_from_env, validate_text, Config,
};
use common_dns::{
    type_name, validate_name, TYPE_A, TYPE_AAAA, TYPE_ANY, TYPE_PTR, TYPE_SRV, TYPE_TXT,
};
use common_health::Health;
use common_log::{EventSource, LogBuffer, RotatingFile};
use common_metrics::Registry;
use common_net::{default_interface, list_interfaces};
use common_webui::INDEX_HTML;

use config::MdnsConfig;
use engine::Engine;
use gateway::Gateway;
use state::{HealthMonitor, State};
use store::{ConfigStore, Snapshot};

/// Compiled-in fallback so the single binary is usable without a config file.
const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");
const PROBE_TIMEOUT_MS: u64 = 800;
const DEFAULT_AGING_TTL_S: f64 = 300.0;
const HEALTH_INTERVAL_S: u64 = 15;

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
static STATE: OnceLock<Arc<State>> = OnceLock::new();
static LOGS: OnceLock<Arc<LogBuffer>> = OnceLock::new();
static ALERTS: OnceLock<Arc<Dispatcher>> = OnceLock::new();
static GATEWAY: OnceLock<Arc<Gateway>> = OnceLock::new();
/// Conflict identities already reported in this process; the dispatcher's
/// cooldown is the authoritative guard, this set only avoids re-scan churn.
static SEEN_CONFLICTS: OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
    OnceLock::new();

#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TERM: AtomicBool = AtomicBool::new(false);
    static HUP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_term(_: i32) {
        TERM.store(true, Ordering::Relaxed);
    }
    extern "C" fn on_hup(_: i32) {
        HUP.store(true, Ordering::Relaxed);
    }

    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }

    pub fn install() {
        unsafe {
            signal(2, on_term as *const () as usize); // SIGINT
            signal(15, on_term as *const () as usize); // SIGTERM
            signal(1, on_hup as *const () as usize); // SIGHUP -> config reload
        }
    }

    pub fn triggered() -> bool {
        TERM.load(Ordering::Relaxed)
    }

    pub fn take_hup() -> bool {
        HUP.swap(false, Ordering::Relaxed)
    }
}

#[cfg(not(unix))]
mod signals {
    pub fn install() {}
    pub fn triggered() -> bool {
        false
    }
    pub fn take_hup() -> bool {
        false
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = config_path_from_env();
    handle_subcommands(&args, &path);
    let (text, flat, common, mut mdns) = match load_and_validate(&path, true) {
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

    // F-F05: never expose an unauthenticated management API on a non-loopback
    // interface. An explicit admin_token is required to widen the bind.
    let bind_is_loopback = common
        .listen
        .parse::<IpAddr>()
        .map(|addr| addr.is_loopback())
        .unwrap_or(false);
    if !bind_is_loopback && common.admin_token.is_empty() {
        eprintln!(
            "{}",
            common_log::error(
                "lan-mdns",
                "refusing non-loopback listen without server.admin_token (F-F05)"
            )
        );
        std::process::exit(1);
    }

    // Test-isolation overrides are applied only to the running engine, so
    // `--cli` above always reports the config file's own values.
    apply_overrides(&mut mdns);

    // --- M2.2 / M2.3 runtime state -----------------------------------------
    let log_dir = env_or("LAN_STUDIO_LOG_DIR", "logs");
    let auth_file = env_or("LAN_STUDIO_AUTH_FILE", "auth.toml");
    let history_dir = env_or("LAN_STUDIO_HISTORY_DIR", "config_history");
    let max_bytes = get_u64(&flat, "logging.max_bytes", 1_048_576);
    let backup_count = get_u32(&flat, "logging.backup_count", 3);
    let history_limit = get_u32(&flat, "observability.history_limit", 30) as usize;
    let aging_ttl_s = std::env::var("LAN_MDNS_AGING_TTL_S")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(DEFAULT_AGING_TTL_S);

    let rotating = match RotatingFile::new(&log_dir, "lan-mdns.log", max_bytes, backup_count) {
        Ok(file) => Some(file),
        Err(error) => {
            eprintln!(
                "{}",
                common_log::warning("lan-mdns", &format!("disk logging disabled: {}", error))
            );
            None
        }
    };
    let log_buffer = Arc::new(LogBuffer::new(512, rotating));
    let _ = LOGS.set(Arc::clone(&log_buffer));

    // F-G01..G05: one bounded dispatcher owns every canonical alert event.
    let alerts = Dispatcher::start(alert_config_from_flat(&flat), Box::new(DefaultSender));
    let _ = ALERTS.set(Arc::clone(&alerts));
    let _ = SEEN_CONFLICTS.set(std::sync::Mutex::new(
        std::collections::BTreeSet::<String>::new(),
    ));

    let password = match PasswordManager::load(&auth_file) {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!(
                "{}",
                common_log::error(
                    "lan-mdns",
                    &format!("cannot load admin password file: {}", error)
                )
            );
            std::process::exit(1);
        }
    };
    let store = match ConfigStore::new(
        &path,
        &history_dir,
        history_limit,
        &common.component,
        text.clone(),
    ) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "{}",
                common_log::error(
                    "lan-mdns",
                    &format!("cannot init config history: {}", error)
                )
            );
            std::process::exit(1);
        }
    };
    let port = port_from_env(common.port);
    let addr = format!("{}:{}", common.listen, port);
    let admin_addr = format!("127.0.0.1:{}", port);

    let health = HealthMonitor::start(
        mdns.health_targets.clone(),
        HEALTH_INTERVAL_S,
        Arc::new(|event: AlertEvent| emit_alert(event)),
    );

    // F-H01..F-H04: the global switch decides whether a socket exists at all.
    let mut gateway_cfg = mdns.gateway.clone();
    if let Some(override_port) = std::env::var("LAN_MDNS_GATEWAY_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
    {
        if override_port > 0 {
            gateway_cfg.port = override_port;
        }
    }
    let gateway = match Gateway::start(&gateway_cfg, &admin_addr, |host| {
        emit_alert(
            AlertEvent::new(
                "lan-mdns",
                "info",
                "gateway_target_unreachable",
                "网关目标不可达",
                &format!("已登记域名 {} 的转发目标当前无法连接", host),
                &host,
            )
            .with_extra("host", &host)
            .with_cooldown(600),
        );
    }) {
        Ok(instance) => instance,
        Err(error) => {
            emit("warning", &format!("HTTP gateway disabled: {}", error));
            None
        }
    };
    if let Some(instance) = &gateway {
        let _ = GATEWAY.set(Arc::clone(instance));
    }

    let state = Arc::new(State {
        store,
        log_buffer: Arc::clone(&log_buffer),
        password: Arc::clone(&password),
        health: Arc::clone(&health),
        aging_ttl_s,
        alerts: Arc::clone(&alerts),
        gateway: gateway.clone(),
    });
    let _ = STATE.set(Arc::clone(&state));

    let engine = match Engine::start(mdns) {
        Ok(engine) => engine,
        Err(error) => {
            emit("error", &format!("mDNS engine failed to start: {}", error));
            std::process::exit(1);
        }
    };
    let group = engine.config().group;
    let mdns_port = engine.config().port;
    let _ = ENGINE.set(Arc::clone(&engine));

    let health_state = Arc::new(Health::new());
    let metrics = Registry::new("lan-mdns");
    metrics.set_build_info(version, commit, &target);
    declare_mdns_metrics(&metrics);

    // `Arc::clone(&x)` pins the receiver to `&Arc<Concrete>`; use `x.clone()`
    // so the unsize coercion to the trait object happens on the returned value.
    let events: Arc<dyn EventSource> = log_buffer.clone();
    let guard: Arc<dyn common_auth::PasswordGuard> = password.clone();
    let ctx = Arc::new(Context {
        component: common.component.clone(),
        version: version.to_string(),
        started: Instant::now(),
        metrics,
        health: Arc::clone(&health_state),
        admin_token: common.admin_token.clone(),
        config_raw: text,
        webui: common.webui,
        events: Some(events),
        password: Some(guard),
    });

    let app = App::new()
        .route("GET", "/", view_index)
        .route("GET", "/healthz", view_healthz)
        .route("GET", "/readyz", view_readyz)
        .route("GET", "/api/status", view_status)
        .route("GET", "/api/metrics", view_metrics)
        .route("GET", "/api/config", view_config)
        .route_write("POST", "/api/config", view_config_save)
        .route("GET", "/api/config/snapshot", view_snapshot_export)
        .route_write("POST", "/api/config/snapshot", view_snapshot_import)
        .route("GET", "/api/config/history", view_history)
        .route_write("POST", "/api/config/rollback", view_rollback)
        .route_write("POST", "/api/reload", view_reload)
        .route("GET", "/api/logs", view_logs)
        .route("GET", "/api/logs/stream", view_logs_stream)
        .route("GET", "/api/alerts", view_alerts)
        .route("GET", "/api/auth", view_auth)
        .route("POST", "/api/auth/password", view_auth_password)
        .route("POST", "/api/auth/verify", view_auth_verify)
        .route("GET", "/api/records", view_records)
        .route("GET", "/api/interfaces", view_interfaces)
        .route("GET", "/api/queries", view_queries)
        .route("GET", "/api/browse", view_browse)
        .route("GET", "/api/browse/unified", view_browse_unified)
        .route("GET", "/api/conflicts", view_conflicts)
        .route("GET", "/api/health/targets", view_health_targets)
        .route("POST", "/api/probe", view_probe);

    signals::install();
    spawn_watchdog();

    // First /api/health/targets read should carry real state, not "unknown".
    health.probe_now();
    health_state.mark_ready();
    let gateway_info = gateway.as_ref().map_or_else(
        || "gateway=off".to_string(),
        |item| format!("gateway=:{}", item.port),
    );
    emit(
        "info",
        &format!(
            "mDNS {}:{} ready, HTTP {}, {} records / {} services, password_set={}, {}, alert_webhooks={}",
            group,
            mdns_port,
            addr,
            record_count,
            service_count,
            password.is_set(),
            gateway_info,
            alerts.status().webhooks
        ),
    );
    if let Err(error) = app.serve(&addr, ctx) {
        emit("error", &format!("server failed: {}", error));
        std::process::exit(1);
    }
}

/// F-I02 / F-I07 / F-E02: offline, non-listening subcommands. None of these
/// bind a socket (except `--healthcheck`, which only makes an outbound probe).
fn handle_subcommands(args: &[String], path: &str) {
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let version = env!("CARGO_PKG_VERSION");
    let commit = option_env!("GIT_COMMIT").unwrap_or("dev");

    if flag("--version") || flag("-V") {
        println!(
            "lan-mdns {} ({}) {}-{}",
            version,
            commit,
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        std::process::exit(0);
    }
    if flag("--print-config") {
        print!("{}", DEFAULT_CONFIG);
        std::process::exit(0);
    }
    if flag("--validate") {
        match load_and_validate(path, false) {
            Ok((_, _, _, mdns)) => {
                println!(
                    "{}",
                    common_log::info(
                        "lan-mdns",
                        &format!(
                            "config valid: {} ({} records / {} services)",
                            path,
                            mdns.records.len(),
                            mdns.services.len()
                        )
                    )
                );
                std::process::exit(0);
            }
            Err(message) => {
                eprintln!("{}", common_log::error("lan-mdns", &message));
                std::process::exit(1);
            }
        }
    }
    if flag("--healthcheck") {
        let port = match load_and_validate(path, false) {
            Ok((_, _, common, _)) => port_from_env(common.port),
            Err(_) => port_from_env(8080),
        };
        if probe_local_healthz(port).unwrap_or(false) {
            std::process::exit(0);
        }
        eprintln!("healthcheck failed on 127.0.0.1:{}", port);
        std::process::exit(1);
    }
}

/// Docker/systemd health probe: the single binary is its own probe client, so
/// the runtime image needs neither curl nor a shell (F-I01 / F-I07).
fn probe_local_healthz(port: u16) -> std::io::Result<bool> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
    // The management server writes the head and the body separately, so a
    // single `read` may observe only the status line. Drain until the liveness
    // marker is present (or the connection closes / times out).
    let mut collected = String::new();
    let mut buffer = [0u8; 512];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                collected.push_str(&String::from_utf8_lossy(&buffer[..count]));
                if collected.contains("\"ok\":true") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    Ok(collected.starts_with("HTTP/1.1 200") && collected.contains("\"ok\":true"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Parse the frozen `alerts:` segment into the dispatcher configuration.
fn alert_config_from_flat(flat: &HashMap<String, String>) -> AlertConfig {
    let enabled =
        get_bool(flat, "alerts.enabled", true) && get_bool(flat, "observability.alerts", true);
    let mut webhooks = Vec::new();
    fn push_url(list: &mut Vec<String>, value: &str) {
        let url = value.trim().trim_matches('"').trim_matches('\'').trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            list.push(url.to_string());
        }
    }
    for index in 0..common_config::list_len(flat, "alerts.webhooks") {
        push_url(
            &mut webhooks,
            &get_str(flat, &format!("alerts.webhooks[{}]", index), ""),
        );
    }
    // The minimal YAML reader expands `- item` lists but keeps inline arrays
    // as one scalar; accept both spellings so a documented example works.
    let inline = get_str(flat, "alerts.webhooks", "");
    if inline.starts_with('[') && inline.ends_with(']') {
        for item in inline[1..inline.len() - 1].split(',') {
            push_url(&mut webhooks, item);
        }
    }
    let timeout = get_str(flat, "alerts.timeout_seconds", "5.0")
        .parse::<f64>()
        .unwrap_or(5.0)
        .clamp(0.1, 30.0);
    AlertConfig {
        enabled,
        webhooks,
        cooldown_seconds: get_u64(flat, "alerts.cooldown_seconds", 300),
        timeout: Duration::from_millis((timeout * 1000.0) as u64),
        max_retries: get_u32(flat, "alerts.max_retries", 2).min(2),
    }
}

fn emit_alert(event: AlertEvent) {
    if let Some(dispatcher) = ALERTS.get() {
        dispatcher.emit(event);
    }
}

/// Emit one structured record to stderr and to the bounded SSE/disk buffer.
fn emit(level: &str, msg: &str) {
    let line = common_log::record(level, "lan-mdns", msg, &[], None);
    eprintln!("{}", line);
    if let Some(buffer) = LOGS.get() {
        buffer.append(line);
    }
}

/// Read, offline-validate and split a config into common + component
/// namespaces. No socket or file is written here (F-E02).
fn load_and_validate(
    path: &str,
    bootstrap: bool,
) -> Result<(String, HashMap<String, String>, Config, MdnsConfig), String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) if bootstrap => {
            // F-E09: write the compiled-in template exactly once. An existing
            // file is *never* overwritten, so upgrades keep user settings.
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|error| {
                        format!("cannot create {}: {}", parent.display(), error)
                    })?;
                }
            }
            fs::write(path, DEFAULT_CONFIG)
                .map_err(|error| format!("cannot write {}: {}", path, error))?;
            eprintln!(
                "{}",
                common_log::info(
                    "lan-mdns",
                    &format!("first boot: generated config template at {}", path)
                )
            );
            DEFAULT_CONFIG.to_string()
        }
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
    Ok((text, flat, common, mdns))
}

/// Test-isolation hooks (docs/ADR/0002). Production keeps the frozen YAML
/// schema untouched; these only keep CI smoke traffic off the real LAN.
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

/// F-E03: validate a config text, then hot-swap declarative records and
/// health targets. Socket-level changes still require a restart.
fn apply_config_text(text: &str, reason: &str) -> Result<(), String> {
    match apply_config_text_inner(text, reason) {
        Ok(()) => Ok(()),
        Err(message) => {
            emit_alert(
                AlertEvent::new(
                    "lan-mdns",
                    "warning",
                    "config_reload_failed",
                    "配置热重载失败，已保留旧配置",
                    &message,
                    reason,
                )
                .with_extra("reason", reason)
                .with_cooldown(60),
            );
            Err(message)
        }
    }
}

fn apply_config_text_inner(text: &str, reason: &str) -> Result<(), String> {
    let flat = validate_text(text).map_err(|e| format!("common config invalid: {}", e))?;
    let mut mdns = match MdnsConfig::from_flat(&flat) {
        Ok(value) => value,
        Err(error) => {
            emit_alert(
                AlertEvent::new(
                    "lan-mdns",
                    "warning",
                    "config_invalid",
                    "配置校验失败",
                    "热重载输入未通过离线校验",
                    reason,
                )
                .with_extra("reason", reason)
                .with_cooldown(60),
            );
            return Err(format!("mdns config invalid: {}", error));
        }
    };
    let state = STATE.get().ok_or("state not ready")?;
    let engine = ENGINE.get().ok_or("mDNS engine not ready")?;
    apply_overrides(&mut mdns);
    engine.reload(mdns.clone());
    state.health.replace_targets(mdns.health_targets.clone());
    // F-G05 / F-E03: alert webhooks and gateway routes are hot-swappable.
    if let Some(dispatcher) = ALERTS.get() {
        dispatcher.reconfigure(alert_config_from_flat(&flat));
    }
    if let Some(gateway) = GATEWAY.get() {
        gateway.reconfigure(&mdns.gateway);
    }
    state.store.set_raw(text);
    state.store.inc_reloads();
    emit("info", &format!("config reloaded ({})", reason));
    Ok(())
}

/// F-E03: reload because the on-disk file changed outside the process.
fn reload_from_disk() {
    let Some(state) = STATE.get() else {
        return;
    };
    let text = match fs::read_to_string(state.store.path()) {
        Ok(text) => text,
        Err(_) => return,
    };
    if text == state.store.raw() {
        return;
    }
    if let Err(error) = apply_config_text(&text, "file-change") {
        emit(
            "warning",
            &format!("config file reload rejected: {}", error),
        );
    }
}

fn spawn_watchdog() {
    std::thread::spawn(|| loop {
        if signals::triggered() {
            if let Some(engine) = ENGINE.get() {
                engine.shutdown();
            }
            if let Some(state) = STATE.get() {
                state.health.shutdown();
            }
            if let Some(dispatcher) = ALERTS.get() {
                dispatcher.shutdown();
            }
            if let Some(gateway) = GATEWAY.get() {
                gateway.shutdown();
            }
            emit("info", "shutdown signal received, goodbye sent");
            std::process::exit(0);
        }
        if signals::take_hup() {
            reload_from_disk();
        }
        // Cheap polling of the config file; content comparison prevents a
        // redundant reload right after our own atomic save.
        reload_from_disk();
        if let (Some(engine), Some(state)) = (ENGINE.get(), STATE.get()) {
            engine.purge_expired(unix_now(), 300.0);
            scan_conflicts(engine, state);
        }
        std::thread::sleep(Duration::from_millis(500));
    });
}

/// F-G03: emit one canonical `name_conflict` event per newly observed
/// conflict. The dispatcher's cooldown is the second line of defence.
fn scan_conflicts(engine: &Engine, state: &State) {
    let Some(mutex) = SEEN_CONFLICTS.get() else {
        return;
    };
    let Ok(mut seen) = mutex.lock() else {
        return;
    };
    let conflicts = state.conflicts(engine);
    let current: std::collections::BTreeSet<String> = conflicts
        .iter()
        .map(|conflict| format!("{}|{}", conflict.name, conflict.rtype))
        .collect();
    for conflict in conflicts {
        let key = format!("{}|{}", conflict.name, conflict.rtype);
        if seen.insert(key.clone()) {
            let source = conflict
                .sources
                .first()
                .map(String::as_str)
                .unwrap_or("unknown");
            emit_alert(
                AlertEvent::new(
                    "lan-mdns",
                    "warning",
                    "name_conflict",
                    "mDNS 名称冲突",
                    &format!("{} 出现同名异值应答", conflict.name),
                    &key,
                )
                .with_extra("name", &conflict.name)
                .with_extra("source_ip", source)
                .with_cooldown(300),
            );
        }
    }
    seen.retain(|key| current.contains(key));
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn declare_mdns_metrics(metrics: &Registry) {
    metrics.declare_counter("queries_total", "mDNS queries received");
    metrics.declare_counter("responses_total", "mDNS responses sent");
    metrics.declare_counter("announces_total", "mDNS proactive announcements");
    metrics.declare_counter("probe_total", "mDNS active probes by result");
    metrics.declare_counter("bytes_total", "mDNS bytes by direction");
    metrics.declare_gauge("records", "Published mDNS records by type");
    metrics.declare_gauge("query_audit_entries", "Query audit ring buffer size");
    metrics.declare_gauge("browse_instances", "Observed LAN service instances");
    metrics.declare_gauge("health_targets", "Business health targets by state");
    metrics.declare_gauge("conflicts", "Name conflicts currently registered");
    metrics.declare_gauge("config_history_versions", "Retained config snapshots");
    metrics.declare_gauge("auth_password_enabled", "Admin password initialized");
    metrics.declare_gauge("gateway_enabled", "HTTP gateway global switch");
    metrics.declare_gauge("alerts_queued", "Canonical events waiting for delivery");
    metrics.declare_gauge("gateway_routes", "Registered static Host routes");
    metrics.declare_counter("gateway_requests_total", "Gateway requests by result");
}

// ---------------------------------------------------------------- helpers

fn engine_or_503(req: &Request) -> Result<&'static Arc<Engine>, Response> {
    ENGINE
        .get()
        .ok_or_else(|| err(req, "internal_error", "mDNS engine not running", 503))
}

fn state_or_503(req: &Request) -> Result<&'static Arc<State>, Response> {
    STATE
        .get()
        .ok_or_else(|| err(req, "internal_error", "runtime state not ready", 503))
}

fn json_str_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|item| json_quote(item)).collect();
    format!("[{}]", parts.join(","))
}

// ---------------------------------------------------------------- handlers

fn view_healthz(_: &Request, _: &Context) -> Response {
    Response::json(200, "{\"ok\":true}".to_string())
}

fn view_readyz(req: &Request, ctx: &Context) -> Response {
    if ctx.health.is_ready() && ENGINE.get().is_some() && STATE.get().is_some() {
        ok(req, "{\"ready\":true}")
    } else {
        err(req, "internal_error", "not ready", 503)
    }
}

fn view_status(req: &Request, ctx: &Context) -> Response {
    let uptime = ctx.started.elapsed().as_secs_f64();
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
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
    let (up, down, unknown) = state.health.counts();
    let conflicts = ENGINE
        .get()
        .map_or(0, |engine| state.conflicts(engine).len());
    let alert_status = state.alerts.status();
    let alerts_json = format!(
        "{{\"enabled\":{},\"webhooks\":{},\"queued\":{},\"sent\":{},\"failed\":{}}}",
        alert_status.enabled,
        alert_status.webhooks,
        alert_status.queued,
        alert_status.sent,
        alert_status.failed
    );
    let gateway_json = match &state.gateway {
        Some(gateway) => {
            let stats = gateway.stats();
            format!(
                "{{\"enabled\":true,\"port\":{},\"routes\":{},\"requests\":{},\"forwarded\":{}}}",
                gateway.port,
                gateway.routes().len(),
                stats.requests,
                stats.forwarded
            )
        }
        None => {
            "{\"enabled\":false,\"port\":0,\"routes\":0,\"requests\":0,\"forwarded\":0}".to_string()
        }
    };
    let data = format!(
        "{{\"component\":{},\"version\":{},\"running\":true,\"uptime\":{:.3},\"protocol\":{},\"records\":{},\"group\":{},\"mdns_port\":{},\"interface_mode\":{},\"queries\":{},\"responses\":{},\"ingested\":{},\"announces\":{},\"malformed\":{},\"bytes_in\":{},\"bytes_out\":{},\"security\":{{\"token_required\":{},\"password_set\":{},\"write_protection\":true}},\"history\":{{\"versions\":{},\"limit\":{}}},\"health\":{{\"up\":{},\"down\":{},\"unknown\":{}}},\"conflicts\":{},\"config_saves\":{},\"config_reloads\":{},\"alerts\":{},\"gateway\":{},\"logs\":{{\"buffered\":{},\"capacity\":{},\"disk_bytes\":{},\"disk_limit_bytes\":{}}}}}",
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
        stats.bytes_in,
        stats.bytes_out,
        !ctx.admin_token.is_empty(),
        state.password.is_set(),
        state.store.history_len(),
        state.store.history_limit(),
        up,
        down,
        unknown,
        conflicts,
        state.store.saves(),
        state.store.reloads(),
        alerts_json,
        gateway_json,
        state.log_buffer.len(),
        state.log_buffer.capacity(),
        state.log_buffer.disk_bytes(),
        state.log_buffer.disk_limit_bytes(),
    );
    ok(req, &data)
}

fn view_metrics(_: &Request, ctx: &Context) -> Response {
    ctx.metrics
        .set("uptime_seconds", &[], ctx.started.elapsed().as_secs_f64());
    let state = STATE.get();
    if let (Some(engine), Some(state)) = (ENGINE.get(), state) {
        ctx.metrics.set("up", &[], 1.0);
        let stats = engine.stats_snapshot();
        ctx.metrics.set_counter("queries_total", &[], stats.queries);
        ctx.metrics
            .set_counter("responses_total", &[], stats.responses);
        ctx.metrics
            .set_counter("announces_total", &[], stats.announces);
        ctx.metrics
            .set_counter("bytes_total", &[("direction", "in")], stats.bytes_in);
        ctx.metrics
            .set_counter("bytes_total", &[("direction", "out")], stats.bytes_out);
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
        let (up, down, unknown) = state.health.counts();
        ctx.metrics
            .set("health_targets", &[("state", "up")], up as f64);
        ctx.metrics
            .set("health_targets", &[("state", "down")], down as f64);
        ctx.metrics
            .set("health_targets", &[("state", "unknown")], unknown as f64);
        ctx.metrics
            .set("conflicts", &[], state.conflicts(engine).len() as f64);
        ctx.metrics.set(
            "config_history_versions",
            &[],
            state.store.history_len() as f64,
        );
        ctx.metrics.set(
            "auth_password_enabled",
            &[],
            if state.password.is_set() { 1.0 } else { 0.0 },
        );
        ctx.metrics.set(
            "gateway_enabled",
            &[],
            if state.gateway.is_some() { 1.0 } else { 0.0 },
        );
        ctx.metrics
            .set_counter("config_saves_total", &[], state.store.saves());
        ctx.metrics
            .set_counter("config_reloads_total", &[], state.store.reloads());
    } else {
        ctx.metrics.set("up", &[], 0.0);
    }
    if let Some(state) = state {
        ctx.metrics
            .set("log_disk_bytes", &[], state.log_buffer.disk_bytes() as f64);
        ctx.metrics.set(
            "log_disk_limit_bytes",
            &[],
            state.log_buffer.disk_limit_bytes() as f64,
        );
        let alerts = state.alerts.status();
        ctx.metrics.set("alerts_queued", &[], alerts.queued as f64);
        for ((kind, result), count) in &alerts.by_kind {
            ctx.metrics.set_counter(
                "alerts_sent_total",
                &[("kind", kind), ("result", result)],
                *count,
            );
        }
        if let Some(gateway) = &state.gateway {
            let stats = gateway.stats();
            ctx.metrics
                .set("gateway_routes", &[], gateway.routes().len() as f64);
            ctx.metrics
                .set_counter("gateway_requests_total", &[], stats.requests);
        } else {
            ctx.metrics.set("gateway_routes", &[], 0.0);
            ctx.metrics.set_counter("gateway_requests_total", &[], 0);
        }
    }
    Response::body(
        200,
        "text/plain; version=0.0.4; charset=utf-8",
        ctx.metrics.render(),
    )
}

/// Full typed view of the on-disk configuration (F-E04 form mode input).
///
/// The form never writes this structure back directly: it renders controls from
/// it and then serialises the whole document to YAML text, so the single source
/// of truth stays the raw text that `POST /api/config` validates offline.
fn config_model_json(flat: &HashMap<String, String>) -> String {
    let records: Vec<String> = (0..list_len(flat, "mdns.records"))
        .map(|index| {
            let base = format!("mdns.records[{}]", index);
            format!(
                "{{\"name\":{},\"type\":{},\"value\":{},\"ttl\":{}}}",
                json_quote(&get_str(flat, &format!("{}.name", base), "")),
                json_quote(&get_str(flat, &format!("{}.type", base), "A")),
                json_quote(&get_str(flat, &format!("{}.value", base), "")),
                get_u32(flat, &format!("{}.ttl", base), 0)
            )
        })
        .collect();

    let services: Vec<String> = (0..list_len(flat, "mdns.services"))
        .map(|index| {
            let base = format!("mdns.services[{}]", index);
            let txt_prefix = format!("{}.txt.", base);
            let mut txt: Vec<String> = flat
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix(&txt_prefix)
                        .map(|name| format!("{}={}", name, value))
                })
                .collect();
            txt.sort();
            format!(
                "{{\"instance\":{},\"type\":{},\"host\":{},\"port\":{},\"proxy\":{},\"txt\":{}}}",
                json_quote(&get_str(flat, &format!("{}.instance", base), "")),
                json_quote(&get_str(flat, &format!("{}.type", base), "")),
                json_quote(&get_str(flat, &format!("{}.host", base), "")),
                get_u16(flat, &format!("{}.port", base), 80),
                get_bool(flat, &format!("{}.proxy", base), true),
                json_str_array(&txt)
            )
        })
        .collect();

    let targets: Vec<String> = (0..list_len(flat, "mdns.health_targets"))
        .map(|index| {
            let base = format!("mdns.health_targets[{}]", index);
            format!(
                "{{\"alias\":{},\"host\":{},\"port\":{}}}",
                json_quote(&get_str(flat, &format!("{}.alias", base), "")),
                json_quote(&get_str(flat, &format!("{}.host", base), "")),
                get_u16(flat, &format!("{}.port", base), 0)
            )
        })
        .collect();

    let hosts: Vec<String> = (0..list_len(flat, "mdns.gateway.hosts"))
        .map(|index| {
            let base = format!("mdns.gateway.hosts[{}]", index);
            format!(
                "{{\"host\":{},\"target\":{},\"proxy\":{}}}",
                json_quote(&get_str(flat, &format!("{}.host", base), "")),
                json_quote(&get_str(flat, &format!("{}.target", base), "")),
                get_bool(flat, &format!("{}.proxy", base), true)
            )
        })
        .collect();

    let webhooks: Vec<String> = (0..list_len(flat, "alerts.webhooks"))
        .map(|index| json_quote(&get_str(flat, &format!("alerts.webhooks[{}]", index), "")))
        .collect();

    let interfaces: Vec<String> = (0..list_len(flat, "mdns.interfaces"))
        .map(|index| json_quote(&get_str(flat, &format!("mdns.interfaces[{}]", index), "")))
        .collect();

    format!(
        "{{\"version\":{},\"component\":{},\"server\":{{\"listen\":{},\"port\":{},\"webui\":{},\"admin_token_set\":{},\"log_level\":{}}},\"observability\":{{\"metrics\":{},\"alerts\":{},\"history_limit\":{}}},\"logging\":{{\"max_bytes\":{},\"backup_count\":{},\"level\":{}}},\"alerts\":{{\"enabled\":{},\"webhooks\":[{}],\"cooldown_seconds\":{},\"timeout_seconds\":{},\"max_retries\":{}}},\"mdns\":{{\"group\":{},\"port\":{},\"ttl\":{},\"announce_count\":{},\"announce_interval_ms\":{},\"announce_period_s\":{},\"reverse_ptr\":{},\"interface_mode\":{},\"interfaces\":[{}],\"records\":[{}],\"services\":[{}],\"health_targets\":[{}],\"gateway\":{{\"enabled\":{},\"port\":{},\"listen\":{},\"admin_host\":{},\"timeout_seconds\":{},\"hosts\":[{}]}}}}}}",
        get_u32(flat, "version", 1),
        json_quote(&get_str(flat, "component", "")),
        json_quote(&get_str(flat, "server.listen", "127.0.0.1")),
        get_u16(flat, "server.port", 8080),
        get_bool(flat, "server.webui", true),
        !get_str(flat, "server.admin_token", "").is_empty(),
        json_quote(&get_str(flat, "server.log_level", "info")),
        get_bool(flat, "observability.metrics", true),
        get_bool(flat, "observability.alerts", true),
        get_u32(flat, "observability.history_limit", 30),
        get_u64(flat, "logging.max_bytes", 1_048_576),
        get_u32(flat, "logging.backup_count", 3),
        json_quote(&get_str(flat, "logging.level", "info")),
        get_bool(flat, "alerts.enabled", true),
        webhooks.join(","),
        get_u32(flat, "alerts.cooldown_seconds", 300),
        get_str(flat, "alerts.timeout_seconds", "5.0"),
        get_u32(flat, "alerts.max_retries", 2),
        json_quote(&get_str(flat, "mdns.group", "224.0.0.251")),
        get_u16(flat, "mdns.port", 5353),
        get_u32(flat, "mdns.ttl", 120),
        get_u32(flat, "mdns.announce_count", 3),
        get_u64(flat, "mdns.announce_interval_ms", 250),
        get_u64(flat, "mdns.announce_period_s", 60),
        get_bool(flat, "mdns.reverse_ptr", true),
        json_quote(&get_str(flat, "mdns.interface_mode", "auto")),
        interfaces.join(","),
        records.join(","),
        services.join(","),
        targets.join(","),
        get_bool(flat, "mdns.gateway.enabled", false),
        get_u16(flat, "mdns.gateway.port", 80),
        json_quote(&get_str(flat, "mdns.gateway.listen", "0.0.0.0")),
        json_quote(&get_str(flat, "mdns.gateway.admin_host", "mdns.local")),
        get_str(flat, "mdns.gateway.timeout_seconds", "6.0"),
        hosts.join(",")
    )
}

fn view_config(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let raw = state.store.raw();
    let structured = match validate_text(&raw) {
        Ok(flat) => config_model_json(&flat),
        Err(error) => format!(
            "{{\"invalid\":true,\"code\":{},\"field\":{},\"detail\":{}}}",
            json_quote(error.code),
            json_quote(&error.field),
            json_quote(&error.detail)
        ),
    };
    ok(
        req,
        &format!(
            "{{\"raw\":{},\"structured\":{}}}",
            json_quote(&raw),
            structured
        ),
    )
}

fn view_config_save(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let text = match store::json_str(&req.body, "text") {
        Some(text) => text,
        None => return err(req, "invalid_config", "config text is required", 400),
    };
    let snapshot_id = match state.store.save(&text, "save") {
        Ok(id) => id,
        Err(error) => return err(req, error.code, &error.message, 400),
    };
    if let Err(error) = apply_config_text(&text, "api-save") {
        return err(req, "internal_error", &error, 500);
    }
    ok(
        req,
        &format!(
            "{{\"saved\":true,\"snapshot\":{},\"config_saves\":{},\"config_reloads\":{}}}",
            json_quote(&snapshot_id),
            state.store.saves(),
            state.store.reloads()
        ),
    )
}

fn view_snapshot_export(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    // `?id=` reads one retained snapshot; without it we export the live config.
    if let Some(id) = query_param(&req.query, "id") {
        return match state.store.read_snapshot(&id) {
            Ok(snap) => ok(req, &snap.to_json()),
            Err(error) => {
                let status = if error.code == "not_found" { 404 } else { 400 };
                err(req, error.code, &error.message, status)
            }
        };
    }
    let reason = store::json_str(&req.body, "reason")
        .or_else(|| query_param(&req.query, "reason"))
        .unwrap_or_else(|| "manual".to_string());
    let snap = state.store.export(&reason);
    ok(req, &snap.to_json())
}

fn view_snapshot_import(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let snap = match Snapshot::from_json(&req.body) {
        Ok(snap) => snap,
        Err(error) => return err(req, error.code, &error.message, 400),
    };
    let text = match state.store.import(&snap) {
        Ok(text) => text,
        Err(error) => return err(req, error.code, &error.message, 400),
    };
    if let Err(error) = apply_config_text(&text, "api-import") {
        return err(req, "internal_error", &error, 500);
    }
    ok(
        req,
        &format!(
            "{{\"imported\":true,\"reason\":{},\"config_saves\":{}}}",
            json_quote(&snap.reason),
            state.store.saves()
        ),
    )
}

fn view_history(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let entries = state.store.list();
    let items: Vec<String> = entries
        .iter()
        .map(|entry| {
            format!(
                "{{\"id\":{},\"created_at\":{},\"reason\":{},\"checksum\":{},\"size\":{}}}",
                json_quote(&entry.id),
                json_quote(&entry.created_at),
                json_quote(&entry.reason),
                json_quote(&entry.checksum),
                entry.size
            )
        })
        .collect();
    let total = items.len();
    ok(
        req,
        &format!(
            "{{\"history\":[{}],\"total\":{},\"limit\":{}}}",
            items.join(","),
            total,
            state.store.history_limit()
        ),
    )
}

fn view_rollback(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let id = store::json_str(&req.body, "id")
        .or_else(|| query_param(&req.query, "id"))
        .unwrap_or_default();
    if id.is_empty() {
        return err(req, "invalid_config", "snapshot id is required", 400);
    }
    let text = match state.store.rollback(&id) {
        Ok(text) => text,
        Err(error) => {
            let status = if error.code == "not_found" { 404 } else { 400 };
            return err(req, error.code, &error.message, status);
        }
    };
    if let Err(error) = apply_config_text(&text, "api-rollback") {
        return err(req, "internal_error", &error, 500);
    }
    ok(
        req,
        &format!(
            "{{\"rolled_back\":true,\"id\":{},\"config_saves\":{}}}",
            json_quote(&id),
            state.store.saves()
        ),
    )
}

fn view_reload(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let text = match fs::read_to_string(state.store.path()) {
        Ok(text) => text,
        Err(error) => return err(req, "internal_error", &error.to_string(), 500),
    };
    if let Err(error) = apply_config_text(&text, "manual-reload") {
        return err(req, "invalid_config", &error, 400);
    }
    ok(
        req,
        &format!(
            "{{\"reloaded\":true,\"config_reloads\":{}}}",
            state.store.reloads()
        ),
    )
}

fn view_logs(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let limit = query_param(&req.query, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let lines = state.log_buffer.recent(limit);
    let total = lines.len();
    ok(
        req,
        &format!(
            "{{\"logs\":[{}],\"total\":{},\"capacity\":{},\"disk_bytes\":{},\"disk_limit_bytes\":{}}}",
            lines.join(","),
            total,
            state.log_buffer.capacity(),
            state.log_buffer.disk_bytes(),
            state.log_buffer.disk_limit_bytes()
        ),
    )
}

fn view_logs_stream(_: &Request, _: &Context) -> Response {
    Response::event_stream(SseSpec {
        replay: 5,
        idle_timeout: Duration::from_millis(400),
        max_duration: Duration::from_secs(30),
        max_events: 200,
    })
}

fn view_alerts(req: &Request, _: &Context) -> Response {
    let Some(dispatcher) = ALERTS.get() else {
        return err(req, "internal_error", "alert dispatcher not ready", 503);
    };
    let status = dispatcher.status();
    let kinds: Vec<String> = status
        .by_kind
        .iter()
        .map(|((kind, result), count)| {
            format!(
                "{{\"kind\":{},\"result\":{},\"count\":{}}}",
                json_quote(kind),
                json_quote(result),
                count
            )
        })
        .collect();
    let (gateway_enabled, gateway_routes, gateway_stats) = match GATEWAY.get() {
        Some(gateway) => {
            let stats = gateway.stats();
            (true, gateway.routes().len(), stats)
        }
        None => (false, 0, gateway::GatewayStats::default()),
    };
    ok(
        req,
        &format!(
            "{{\"enabled\":{},\"webhooks\":{},\"queue_capacity\":{},\"queued\":{},\"sent\":{},\"failed\":{},\"dropped\":{},\"suppressed\":{},\"retries\":{},\"cooldown_seconds\":{},\"by_kind\":[{}],\"gateway\":{{\"enabled\":{},\"routes\":{},\"requests\":{},\"forwarded\":{},\"unreachable\":{}}}}}",
            status.enabled,
            status.webhooks,
            common_alert::QUEUE_CAPACITY,
            status.queued,
            status.sent,
            status.failed,
            status.dropped,
            status.suppressed,
            status.retries,
            status.cooldown_seconds,
            kinds.join(","),
            gateway_enabled,
            gateway_routes,
            gateway_stats.requests,
            gateway_stats.forwarded,
            gateway_stats.unreachable,
        ),
    )
}

fn view_auth(req: &Request, ctx: &Context) -> Response {
    let password_set = STATE.get().is_some_and(|state| state.password.is_set());
    ok(
        req,
        &format!(
            "{{\"token_required\":{},\"password_set\":{},\"write_protection\":true}}",
            !ctx.admin_token.is_empty(),
            password_set
        ),
    )
}

fn view_auth_password(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let new_password = match store::json_str(&req.body, "new_password") {
        Some(value) => value,
        None => return err(req, "invalid_config", "new_password is required", 400),
    };
    if let Err(error) = common_auth::validate_password(&new_password) {
        return err(req, "invalid_config", &error.to_string(), 400);
    }
    if state.password.is_set() {
        let current = store::json_str(&req.body, "current_password")
            .or_else(|| req.headers.get("x-admin-password").cloned())
            .unwrap_or_default();
        if !state.password.verify(&current) {
            return err(req, "auth_failed", "current password does not match", 403);
        }
        if let Err(error) = state.password.change(&current, &new_password) {
            return err(req, "auth_failed", &error.to_string(), 403);
        }
    } else if let Err(error) = state.password.set(&new_password) {
        return err(req, "auth_failed", &error.to_string(), 403);
    }
    emit("info", "admin password updated");
    ok(req, "{\"password_set\":true,\"changed\":true}")
}

fn view_auth_verify(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    if !state.password.is_set() {
        return err(req, "auth_failed", "admin password not initialized", 403);
    }
    let password = store::json_str(&req.body, "password")
        .or_else(|| req.headers.get("x-admin-password").cloned())
        .unwrap_or_default();
    if !state.password.verify(&password) {
        return err(req, "auth_failed", "invalid admin password", 403);
    }
    ok(req, "{\"valid\":true}")
}

fn view_index(_: &Request, ctx: &Context) -> Response {
    if ctx.webui {
        Response::html(200, INDEX_HTML.to_string())
    } else {
        Response::text(404, "webui disabled".to_string())
    }
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
    // F-A12: report the configured mode, the addresses actually joined to the
    // group, and which enumerated interface currently takes part. Changing the
    // interface set rebuilds the multicast socket, so it is explicitly NOT
    // hot-reloadable — the console shows restart_required instead of pretending.
    let (mode, selected) = match ENGINE.get() {
        Some(engine) => {
            let cfg = engine.config();
            let selected: Vec<String> =
                cfg.interfaces.iter().map(|addr| addr.to_string()).collect();
            (cfg.interface_mode.clone(), selected)
        }
        None => (String::from("auto"), Vec::new()),
    };
    let default_dev = default_interface();
    let items: Vec<String> = list_interfaces()
        .iter()
        .map(|info| {
            let addr = info.addr.to_string();
            let joined = match mode.as_str() {
                "all" => true,
                "specified" => selected.iter().any(|item| item == &addr),
                // `auto` joins on the kernel-selected interface only.
                _ => default_dev.as_deref() == Some(info.name.as_str()),
            };
            format!(
                "{{\"name\":{},\"addr\":{},\"joined\":{}}}",
                json_quote(&info.name),
                json_quote(&addr),
                joined
            )
        })
        .collect();
    let total = items.len();
    ok(
        req,
        &format!(
            "{{\"interfaces\":[{}],\"total\":{},\"mode\":{},\"selected\":{},\"default_interface\":{},\"restart_required\":true}}",
            items.join(","),
            total,
            json_quote(&mode),
            json_str_array(&selected),
            match &default_dev {
                Some(name) => json_quote(name),
                None => "null".to_string(),
            }
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
    if query_param(&req.query, "export").as_deref() == Some("csv") {
        let mut csv = String::from("ts,source,name,qtype\n");
        for entry in &entries {
            csv.push_str(&format!(
                "{:.3},{},{},{}\n",
                entry.ts,
                csv_field(&entry.source),
                csv_field(&entry.name),
                csv_field(&entry.qtype)
            ));
        }
        return Response::text(200, csv);
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
        &format!(
            "{{\"queries\":[{}],\"total\":{},\"limit\":{}}}",
            items.join(","),
            total,
            limit
        ),
    )
}

fn view_browse(req: &Request, _: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let items: Vec<String> = engine
        .observed_snapshot()
        .iter()
        .map(|entry| {
            format!(
                "{{\"ts\":{:.3},\"source\":{},\"name\":{},\"type\":{},\"value\":{},\"ttl\":{}}}",
                entry.ts,
                json_quote(&entry.source),
                json_quote(&entry.name),
                json_quote(type_name(entry.rtype)),
                json_quote(&entry.value),
                entry.ttl
            )
        })
        .collect();
    let total = items.len();
    let groups = state.service_map(engine);
    let group_json: Vec<String> = groups
        .iter()
        .map(|group| {
            let instances: Vec<String> = group
                .instances
                .iter()
                .map(|item| {
                    format!(
                        "{{\"name\":{},\"value\":{},\"source\":{},\"ts\":{:.3}}}",
                        json_quote(&item.name),
                        json_quote(&item.value),
                        json_quote(&item.source),
                        item.ts
                    )
                })
                .collect();
            format!(
                "{{\"service_type\":{},\"instances\":[{}],\"count\":{}}}",
                json_quote(&group.service_type),
                instances.join(","),
                group.count
            )
        })
        .collect();
    ok(
        req,
        &format!(
            "{{\"browse\":[{}],\"total\":{},\"services\":[{}],\"service_types\":{}}}",
            items.join(","),
            total,
            group_json.join(","),
            groups.len()
        ),
    )
}

/// M5 unified service-record helpers. Read-only: the legacy `/api/browse`
/// response above is intentionally left untouched (D-M5-7 / M5-PLAN 5.2).
fn bounded_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Days since 1970-01-01 -> (year, month, day), Howard Hinnant's civil calendar.
fn civil_from_days_utc(days: i64) -> (i64, i64, i64) {
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

fn rfc3339_from_epoch(ts: f64) -> String {
    let secs = if ts.is_finite() && ts > 0.0 {
        ts as i64
    } else {
        0
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days_utc(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Split an observed RDATA into the unified `address` / `port` pair.
/// SRV RDATA is `priority weight port target`; A/AAAA/TXT become address-only.
fn unified_target(value: &str) -> (String, u16) {
    let text = value.trim();
    if let Ok(ip) = text.parse::<std::net::IpAddr>() {
        return (bounded_text(&ip.to_string(), 64), 0);
    }
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() >= 4 {
        if let Ok(port) = parts[2].parse::<u16>() {
            return (bounded_text(parts[3].trim_end_matches('.'), 64), port);
        }
    }
    (bounded_text(text.trim_end_matches('.'), 64), 0)
}

/// `GET /api/browse/unified` — the same `contracts/service-record.json` shape
/// emitted by `lan-ssdp`, with `source: "mdns"`.
fn view_browse_unified(req: &Request, _: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let items: Vec<String> = engine
        .observed_snapshot()
        .iter()
        .map(|entry| {
            let owner = bounded_text(entry.name.trim_end_matches('.'), 253);
            let name = if owner.is_empty() {
                "unnamed".to_string()
            } else {
                owner.clone()
            };
            let rtype = type_name(entry.rtype);
            let service_type = if matches!(entry.rtype, 12 | 33) {
                owner
            } else {
                rtype.to_string()
            };
            let service_type = if service_type.is_empty() {
                rtype.to_string()
            } else {
                bounded_text(&service_type, 253)
            };
            let (address, port) = unified_target(&entry.value);
            let ttl = u64::from(entry.ttl).min(86_400);
            let attributes = format!(
                "{{\"record\":{},\"owner\":{},\"value\":{}}}",
                json_quote(rtype),
                json_quote(&bounded_text(&entry.name, 253)),
                json_quote(&bounded_text(&entry.value, 256))
            );
            let last_seen = rfc3339_from_epoch(entry.ts);
            format!(
                "{{\"name\":{},\"type\":{},\"address\":{},\"port\":{port},\"attributes\":{attributes},\"ttl\":{ttl},\"last_seen\":{},\"source\":\"mdns\"}}",
                json_quote(&name),
                json_quote(&service_type),
                json_quote(&address),
                json_quote(&last_seen)
            )
        })
        .collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{},\"source\":\"mdns\"}}",
            items.join(","),
            items.len()
        ),
    )
}

fn view_conflicts(req: &Request, _: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let conflicts = state.conflicts(engine);
    let items: Vec<String> = conflicts
        .iter()
        .map(|conflict| {
            format!(
                "{{\"name\":{},\"type\":{},\"rtype\":{},\"values\":{},\"sources\":{}}}",
                json_quote(&conflict.name),
                json_quote(&conflict.rtype_name),
                conflict.rtype,
                json_str_array(&conflict.values),
                json_str_array(&conflict.sources)
            )
        })
        .collect();
    let total = items.len();
    ok(
        req,
        &format!(
            "{{\"conflicts\":[{}],\"total\":{}}}",
            items.join(","),
            total
        ),
    )
}

fn view_health_targets(req: &Request, _: &Context) -> Response {
    let state = match state_or_503(req) {
        Ok(state) => state,
        Err(response) => return response,
    };
    let targets = state.health.snapshot();
    let items: Vec<String> = targets
        .iter()
        .map(|status| {
            format!(
                "{{\"alias\":{},\"host\":{},\"port\":{},\"state\":{},\"checked_at\":{:.3},\"latency_ms\":{:.3}}}",
                json_quote(&status.alias),
                json_quote(&status.host),
                status.port,
                json_quote(&status.state),
                status.checked_at,
                status.latency_ms
            )
        })
        .collect();
    let (up, down, unknown) = state.health.counts();
    let total = items.len();
    ok(
        req,
        &format!(
            "{{\"targets\":[{}],\"total\":{},\"up\":{},\"down\":{},\"unknown\":{}}}",
            items.join(","),
            total,
            up,
            down,
            unknown
        ),
    )
}

fn view_probe(req: &Request, ctx: &Context) -> Response {
    let engine = match engine_or_503(req) {
        Ok(engine) => engine,
        Err(response) => return response,
    };
    let name = store::json_str(&req.body, "name")
        .or_else(|| query_param(&req.query, "name"))
        .unwrap_or_default();
    if name.is_empty() {
        return err(req, "invalid_config", "probe name is required", 400);
    }
    if validate_name(&name).is_err() {
        return err(req, "invalid_config", "probe name is invalid", 400);
    }
    let type_text = store::json_str(&req.body, "type")
        .or_else(|| query_param(&req.query, "type"))
        .unwrap_or_else(|| "PTR".to_string());
    let qtype = parse_qtype(&type_text);
    let unicast = store::json_str(&req.body, "unicast")
        .map(|value| value == "true" || value == "1")
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

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_body_and_query() {
        let body = r#"{"name":"nas.local","type":"A","unicast":true}"#;
        assert_eq!(store::json_str(body, "name").as_deref(), Some("nas.local"));
        assert_eq!(store::json_str(body, "type").as_deref(), Some("A"));
        assert_eq!(parse_qtype("aaaa"), TYPE_AAAA);
        assert_eq!(parse_qtype("ANY"), TYPE_ANY);
        assert_eq!(parse_qtype("nonsense"), TYPE_PTR);
    }

    #[test]
    fn json_string_parser_handles_yaml_newlines() {
        let body = r#"{"text":"version: 1\ncomponent: lan-mdns\n","mode":"raw"}"#;
        assert_eq!(
            store::json_str(body, "text").as_deref(),
            Some("version: 1\ncomponent: lan-mdns\n")
        );
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

    #[test]
    fn csv_field_quotes_separators() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    }
}
