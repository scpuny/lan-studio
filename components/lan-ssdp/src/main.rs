//! lan-ssdp — read-only SSDP / UPnP device discovery (M5 · PRD §4.9).
//!
//! READ-ONLY DISCOVERY: 本组件只发发现报文、只被动接收 NOTIFY / M-SEARCH 应答，
//! 把 `LOCATION` 登记为有界属性；**没有 HTTP 客户端、没有 SOAP 调用、不抓设备描述
//! XML、不调用任何控制接口**。所有业务原语在 `ssdp.rs`，运行时契约由
//! `common-component` 提供（统一端点 / 配置闭环 / 指标 / 告警 / 鉴权）。

mod ssdp;

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{json_str, runtime, Flat, Hooks};
use common_config::{get_bool, get_str, get_u16, get_u64};
use common_metrics::Registry;
use common_net::group_addr;

use ssdp::{
    m_search_text, now_rfc3339, parse_notify, DeviceTable, NotifyOutcome, ServiceRecord,
    SsdpConfig, M_SEARCH,
};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();
/// `Some(Some(..))` = multicast probe endpoint; `Some(None)` = unicast
/// test-isolation mode (never emits M-SEARCH); `None` = not started yet.
static PROBE: OnceLock<Option<ProbeEndpoint>> = OnceLock::new();
/// Set whenever the device table changes so the sweep thread can persist a
/// bounded snapshot without writing on every idle tick.
static DIRTY: AtomicBool = AtomicBool::new(false);

struct ProbeEndpoint {
    socket: Arc<UdpSocket>,
    target: SocketAddr,
}

struct AppState {
    cfg: SsdpConfig,
    table: DeviceTable,
    expired: u64,
    errors: u64,
    probes: u64,
}

fn config_from_flat(flat: &Flat) -> SsdpConfig {
    SsdpConfig {
        group: get_str(flat, "ssdp.group", "239.255.255.250"),
        port: get_u16(flat, "ssdp.port", 1900),
        search_interval_seconds: get_u64(flat, "ssdp.search_interval_seconds", 60),
        max_age_seconds: get_u64(flat, "ssdp.max_age_seconds", 1800),
        max_devices: get_u64(flat, "ssdp.max_devices", 512) as usize,
        socket_timeout_ms: get_u64(flat, "ssdp.socket_timeout_ms", 300),
        results_path: get_str(flat, "ssdp.results_path", "lan-ssdp.devices.json"),
        fetch_descriptions: get_bool(flat, "ssdp.fetch_descriptions", false),
    }
}

fn validate(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    if cfg.port == 0 {
        return Err("ssdp.port must be 1..65535".to_string());
    }
    if cfg.group.parse::<Ipv4Addr>().is_err() {
        return Err("ssdp.group must be a valid IPv4 address".to_string());
    }
    if !(5..=86_400).contains(&cfg.search_interval_seconds) {
        return Err("ssdp.search_interval_seconds must be 5..86400".to_string());
    }
    if !(10..=86_400).contains(&cfg.max_age_seconds) {
        return Err("ssdp.max_age_seconds must be 10..86400".to_string());
    }
    if !(1..=4_096).contains(&cfg.max_devices) {
        return Err("ssdp.max_devices must be 1..4096".to_string());
    }
    if !(50..=5_000).contains(&cfg.socket_timeout_ms) {
        return Err("ssdp.socket_timeout_ms must be 50..5000".to_string());
    }
    if cfg.results_path.contains("..") {
        return Err("ssdp.results_path must not contain '..'".to_string());
    }
    if cfg.fetch_descriptions {
        return Err("ssdp.fetch_descriptions must remain false (read-only discovery)".to_string());
    }
    Ok(())
}

fn state() -> &'static Arc<Mutex<AppState>> {
    STATE.get_or_init(|| {
        let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
        let cfg = config_from_flat(&flat);
        let table = DeviceTable::new(cfg.max_devices);
        Arc::new(Mutex::new(AppState {
            cfg,
            table,
            expired: 0,
            errors: 0,
            probes: 0,
        }))
    })
}

fn reload(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    {
        let mut s = state().lock().unwrap();
        s.table.set_max_devices(cfg.max_devices);
        s.cfg = cfg;
    }
    if let Some(rt) = runtime() {
        rt.emit("info", "lan-ssdp configuration reloaded");
    }
    Ok(())
}

fn snapshot_json(s: &AppState) -> String {
    let records = s.table.records();
    let items: Vec<String> = records.iter().map(ServiceRecord::to_json).collect();
    format!(
        "{{\"items\":[{}],\"total\":{},\"max_devices\":{},\"generated_at\":{},\"fetch_descriptions\":false}}",
        items.join(","),
        items.len(),
        s.table.max_devices(),
        json_quote(&now_rfc3339()),
    )
}

fn status_json() -> String {
    let s = state().lock().unwrap();
    format!(
        "{{\"devices\":{},\"expired\":{},\"errors\":{},\"probes\":{},\"max_age_seconds\":{},\"max_devices\":{},\"fetch_descriptions\":false}}",
        s.table.count(),
        s.expired,
        s.errors,
        s.probes,
        s.cfg.max_age_seconds,
        s.cfg.max_devices,
    )
}

fn status() -> String {
    status_json()
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_gauge("devices", "Discovered SSDP devices (bounded)");
    metrics.declare_counter("expired_total", "Devices removed by TTL or byebye");
    metrics.declare_counter("discovery_errors_total", "Malformed SSDP replies");
    metrics.declare_counter("probe_total", "M-SEARCH packets sent");
    let s = state().lock().unwrap();
    metrics.set("devices", &[], s.table.count() as f64);
    metrics.set_counter("expired_total", &[], s.expired);
    metrics.set_counter("discovery_errors_total", &[], s.errors);
    metrics.set_counter("probe_total", &[], s.probes);
}

// ---------------------------------------------------------------------------
// Endpoints.
// ---------------------------------------------------------------------------

fn view_devices(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    let records = s.table.records();
    let items: Vec<String> = records.iter().map(ServiceRecord::to_json).collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{}}}",
            items.join(","),
            items.len()
        ),
    )
}

fn view_snapshot(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    ok(req, &snapshot_json(&s))
}

fn view_ssdp_status(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &status_json())
}

/// Trigger one bounded M-SEARCH. In unicast test-isolation mode the socket was
/// never registered for probing, so `sent` is 0 but the response is still 200.
fn view_refresh(req: &Request, _: &common_api::Context) -> Response {
    let st = match json_str(&req.body, "st") {
        Some(value) if value.len() <= 256 => value,
        Some(_) => return err(req, "invalid_config", "st must be <= 256 characters", 400),
        None => "ssdp:all".to_string(),
    };
    let packet = m_search_text(&st);
    let sent = match PROBE.get() {
        Some(Some(endpoint)) => match endpoint.socket.send_to(packet.as_bytes(), endpoint.target) {
            Ok(_) => 1u64,
            Err(_) => 0,
        },
        _ => 0,
    };
    if sent > 0 {
        let mut s = state().lock().unwrap();
        s.probes += sent;
    }
    ok(req, &format!("{{\"started\":true,\"sent\":{}}}", sent))
}

fn routes(app: App) -> App {
    app.route("GET", "/api/ssdp/devices", view_devices)
        .route("GET", "/api/ssdp/snapshot", view_snapshot)
        .route("GET", "/api/ssdp/status", view_ssdp_status)
        .route_write("POST", "/api/ssdp/refresh", view_refresh)
}

// ---------------------------------------------------------------------------
// UDP transport. Two low-frequency background threads; neither holds the state
// mutex while doing socket IO. The receive loop owns the socket, the sweep loop
// owns TTL expiry + bounded persistence.
// ---------------------------------------------------------------------------

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn emit_warning(message: &str) {
    if let Some(rt) = runtime() {
        rt.emit("warning", message);
    }
}

fn handle_packet(state: &Arc<Mutex<AppState>>, text: &str, address: String) {
    let now = now_rfc3339();
    let seen_epoch = epoch_secs();
    let mut alert = false;
    {
        let mut s = state.lock().unwrap();
        // Read the live config so a reload of max_age/port is honoured without
        // restarting the receive thread.
        let ttl = s.cfg.max_age_seconds.min(86_400);
        let port = s.cfg.port;
        match parse_notify(text, &address, port, ttl, &now) {
            Some(NotifyOutcome::Alive(record)) => {
                let _ = s.table.upsert(record, seen_epoch);
                DIRTY.store(true, Ordering::Relaxed);
            }
            Some(NotifyOutcome::ByeBye(usn)) => {
                if s.table.byebye(&usn) {
                    s.expired += 1;
                    DIRTY.store(true, Ordering::Relaxed);
                }
            }
            None => {
                s.errors += 1;
                // Throttle the alert so a malformed-packet flood cannot spam it.
                alert = s.errors == 1 || s.errors % 32 == 1;
            }
        }
    }
    if !alert {
        return;
    }
    if let Some(rt) = runtime() {
        rt.emit_alert(
            "info",
            "ssdp_discovery_error",
            "malformed SSDP reply ignored",
            "discovery_errors_total incremented",
        );
    }
}

fn recv_loop(
    socket: Arc<UdpSocket>,
    target: SocketAddr,
    multicast: bool,
    state: Arc<Mutex<AppState>>,
    interval: Duration,
) {
    let mut buf = [0u8; 2048];
    let mut next_probe = Instant::now();
    loop {
        // Only multicast mode emits periodic probes. The unicast isolation hook
        // keeps the LAN completely quiet so local fake devices are undisturbed.
        if multicast && Instant::now() >= next_probe {
            if socket.send_to(M_SEARCH.as_bytes(), target).is_ok() {
                let mut s = state.lock().unwrap();
                s.probes += 1;
            }
            next_probe = Instant::now() + interval;
        }
        // Read timeout or transient error: keep the loop alive.
        if let Ok((len, src)) = socket.recv_from(&mut buf) {
            let text = String::from_utf8_lossy(&buf[..len]).to_string();
            handle_packet(&state, &text, src.ip().to_string());
        }
    }
}

fn write_results(path: &str, body: &str) {
    if path.is_empty() || path.contains("..") {
        return;
    }
    let tmp = format!("{}.tmp", path);
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn sweep_loop(state: Arc<Mutex<AppState>>) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let pending: Option<(String, String)> = {
            let mut s = state.lock().unwrap();
            let max_age = s.cfg.max_age_seconds.min(86_400);
            let removed = s.table.sweep(epoch_secs(), max_age);
            s.expired += removed as u64;
            if DIRTY.swap(false, Ordering::Relaxed) || removed > 0 {
                Some((s.cfg.results_path.clone(), snapshot_json(&s)))
            } else {
                None
            }
        };
        // File IO happens after the mutex is released.
        if let Some((path, body)) = pending {
            write_results(&path, &body);
        }
    }
}

fn start() {
    let cfg = {
        let s = state().lock().unwrap();
        s.cfg.clone()
    };

    // TTL expiry + bounded persistence run even if the socket cannot be bound.
    let sweep_state = Arc::clone(state());
    std::thread::spawn(move || sweep_loop(sweep_state));

    let group = match cfg.group.parse::<Ipv4Addr>() {
        Ok(value) => value,
        Err(_) => {
            let _ = PROBE.set(None);
            return;
        }
    };
    let multicast = group.is_multicast();
    let socket = match UdpSocket::bind(("0.0.0.0", cfg.port)) {
        Ok(value) => value,
        Err(message) => {
            emit_warning(&format!("SSDP socket bind failed: {}", message));
            if let Some(rt) = runtime() {
                rt.emit_alert(
                    "info",
                    "ssdp_discovery_error",
                    "SSDP socket unavailable",
                    "bind failed",
                );
            }
            let _ = PROBE.set(None);
            return;
        }
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(cfg.socket_timeout_ms)));
    if multicast {
        let _ = socket.set_multicast_ttl_v4(2);
        if let Err(message) = socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED) {
            emit_warning(&format!("SSDP multicast join failed: {}", message));
        }
    }

    let shared = Arc::new(socket);
    let target = group_addr(group, cfg.port);
    if multicast {
        let _ = PROBE.set(Some(ProbeEndpoint {
            socket: Arc::clone(&shared),
            target,
        }));
    } else {
        // TEST ISOLATION: `ssdp.group` is a unicast address (e.g. 127.0.0.1).
        // Skip join_multicast entirely, receive only unicast UDP on loopback,
        // and register no probe endpoint so refresh never emits on the LAN.
        if let Some(rt) = runtime() {
            rt.emit(
                "info",
                "SSDP unicast isolation: no multicast join, no LAN M-SEARCH",
            );
        }
        let _ = PROBE.set(None);
    }
    let recv_state = Arc::clone(state());
    let interval = Duration::from_secs(cfg.search_interval_seconds.min(86_400));
    std::thread::spawn(move || recv_loop(shared, target, multicast, recv_state, interval));
}

const HOOKS: Hooks = Hooks {
    name: "lan-ssdp",
    version: env!("CARGO_PKG_VERSION"),
    default_config: DEFAULT_CONFIG,
    validate,
    reload,
    status,
    refresh_metrics,
    routes,
    start,
};

fn main() {
    common_component::run(HOOKS);
}

// ---------------------------------------------------------------------------
// Offline unit tests. No multicast is ever sent to a real LAN; file IO uses a
// unique temp subdirectory and cleans it up.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use common_config::parse_flat;

    fn temp_subdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lan-ssdp-{}-{}", std::process::id(), name));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn template_validates_with_bounded_defaults() {
        let flat = parse_flat(DEFAULT_CONFIG);
        assert_eq!(get_str(&flat, "component", ""), "lan-ssdp");
        validate(&flat).expect("template must validate");
        let cfg = config_from_flat(&flat);
        assert_eq!(cfg.group, "239.255.255.250");
        assert_eq!(cfg.port, 1900);
        assert_eq!(cfg.search_interval_seconds, 60);
        assert_eq!(cfg.max_age_seconds, 1800);
        assert_eq!(cfg.max_devices, 512);
        assert_eq!(cfg.socket_timeout_ms, 300);
        assert_eq!(cfg.results_path, "lan-ssdp.devices.json");
        assert!(!cfg.fetch_descriptions);
    }

    #[test]
    fn validation_rejects_fetch_descriptions_true() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("ssdp.fetch_descriptions".into(), "true".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.fetch_descriptions".into(), "1".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.fetch_descriptions".into(), "false".into());
        assert!(validate(&flat).is_ok());
    }

    #[test]
    fn validation_rejects_path_traversal() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("ssdp.results_path".into(), "../evil.json".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.results_path".into(), "/etc/../passwd".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.results_path".into(), "lan-ssdp.devices.json".into());
        assert!(validate(&flat).is_ok());
    }

    #[test]
    fn validation_rejects_bad_ranges_and_non_ipv4_group() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("ssdp.port".into(), "0".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.port".into(), "1900".into());
        flat.insert("ssdp.group".into(), "not-an-ip".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.group".into(), "::1".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.group".into(), "127.0.0.1".into());
        assert!(validate(&flat).is_ok());
        flat.insert("ssdp.search_interval_seconds".into(), "1".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.search_interval_seconds".into(), "60".into());
        flat.insert("ssdp.max_age_seconds".into(), "9".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.max_age_seconds".into(), "1800".into());
        flat.insert("ssdp.max_devices".into(), "9999".into());
        assert!(validate(&flat).is_err());
        flat.insert("ssdp.max_devices".into(), "512".into());
        flat.insert("ssdp.socket_timeout_ms".into(), "5001".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn unicast_group_is_not_multicast() {
        assert!("239.255.255.250"
            .parse::<Ipv4Addr>()
            .unwrap()
            .is_multicast());
        assert!(!"127.0.0.1".parse::<Ipv4Addr>().unwrap().is_multicast());
        // The isolation hook is driven purely by Ipv4Addr::is_multicast.
        assert!(validate(&parse_flat(DEFAULT_CONFIG)).is_ok());
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("ssdp.group".into(), "127.0.0.1".into());
        assert!(validate(&flat).is_ok());
    }

    #[test]
    fn write_results_atomically_persists_a_bounded_snapshot() {
        let dir = temp_subdir("persist");
        let path = dir.join("devices.json");
        let path_text = path.to_string_lossy().to_string();
        let body = "{\"items\":[],\"total\":0,\"fetch_descriptions\":false}";
        write_results(&path_text, body);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        assert!(!std::path::Path::new(&format!("{}.tmp", path_text)).exists());
        // A traversal path is refused and never written.
        let traversal = dir
            .join("..")
            .join("lan-ssdp-traversal-must-not-exist.json");
        let traversal_text = traversal.to_string_lossy().to_string();
        write_results(&traversal_text, "boom");
        assert!(!traversal.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hooks_identity_is_stable() {
        assert_eq!(HOOKS.name, "lan-ssdp");
        assert!(HOOKS.default_config.contains("component: lan-ssdp"));
        assert!(HOOKS.default_config.contains("fetch_descriptions: false"));
        assert!(!HOOKS.default_config.contains("fetch_descriptions: true"));
    }
}
