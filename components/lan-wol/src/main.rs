//! lan-wol — network wake-on-LAN and device power-state (M4, PRD §4.7).
//!
//! Composition root: config validation, global state, metrics, the seven
//! `/api/wol/*` endpoints and the low-frequency background probe loop. All
//! business primitives live in `wol.rs`; the shared `common_component` runtime
//! owns the standard endpoints and the unified API envelope.

mod wol;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{json_str, runtime, Flat, Hooks};
use common_config::{get_bool, get_str, get_u16, get_u64};
use common_metrics::Registry;
use common_probe::{DeviceState, TcpProbe};

use wol::{
    check_wake, now_rfc3339, parse_mac, send_magic, AuditEntry, AuditLog, Cooldown, Device,
    DeviceStates, DeviceStore, WakeReject, WolConfig,
};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();

struct AppState {
    cfg: WolConfig,
    store: DeviceStore,
    states: DeviceStates,
    cooldown: Cooldown,
    audit: AuditLog,
    sent: u64,
    rejected: u64,
    rejected_by_reason: HashMap<&'static str, u64>,
    probes: u64,
}

fn config_from_flat(flat: &Flat) -> WolConfig {
    WolConfig {
        send_enabled: get_bool(flat, "wol.send_enabled", false),
        broadcast: get_str(flat, "wol.broadcast", "255.255.255.255"),
        port: get_u16(flat, "wol.port", 9),
        devices_path: get_str(flat, "wol.devices_path", "lan-wol.devices.json"),
        history_path: get_str(flat, "wol.history_path", "lan-wol.history.json"),
        max_devices: get_u64(flat, "wol.max_devices", 512) as usize,
        cooldown_seconds: get_u64(flat, "wol.cooldown_seconds", 30),
        grace_seconds: get_u64(flat, "wol.grace_seconds", 90),
        probe_port_default: get_u16(flat, "wol.probe_port_default", 22),
        probe_timeout_ms: get_u64(flat, "wol.probe_timeout_ms", 300),
        probe_interval_seconds: get_u64(flat, "wol.probe_interval_seconds", 15),
        probe_success_threshold: get_u64(flat, "wol.probe_success_threshold", 3) as u32,
        probe_failure_threshold: get_u64(flat, "wol.probe_failure_threshold", 3) as u32,
        history_limit: get_u64(flat, "wol.history_limit", 200) as usize,
    }
}

fn validate(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    if cfg.port == 0 {
        return Err("wol.port must be 1..65535".to_string());
    }
    if !(50..=5_000).contains(&cfg.probe_timeout_ms) {
        return Err("wol.probe_timeout_ms must be 50..5000".to_string());
    }
    if !(1..=20).contains(&cfg.probe_success_threshold) {
        return Err("wol.probe_success_threshold must be 1..20".to_string());
    }
    if !(1..=20).contains(&cfg.probe_failure_threshold) {
        return Err("wol.probe_failure_threshold must be 1..20".to_string());
    }
    if cfg.probe_interval_seconds < 5 {
        return Err("wol.probe_interval_seconds must be >= 5".to_string());
    }
    if cfg.probe_port_default == 0 {
        return Err("wol.probe_port_default must be 1..65535".to_string());
    }
    if cfg.max_devices == 0 || cfg.max_devices > 4_096 {
        return Err("wol.max_devices must be 1..4096".to_string());
    }
    if cfg.cooldown_seconds > 86_400 {
        return Err("wol.cooldown_seconds must be <= 86400".to_string());
    }
    if cfg.grace_seconds > 86_400 {
        return Err("wol.grace_seconds must be <= 86400".to_string());
    }
    if cfg.history_limit == 0 || cfg.history_limit > 10_000 {
        return Err("wol.history_limit must be 1..10000".to_string());
    }
    for path in [&cfg.devices_path, &cfg.history_path] {
        if path.contains("..") {
            return Err(format!("{} must not contain '..'", path));
        }
    }
    if cfg.broadcast.parse::<Ipv4Addr>().is_err() {
        return Err("wol.broadcast must be an IPv4 address".to_string());
    }
    Ok(())
}

fn state() -> &'static Arc<Mutex<AppState>> {
    STATE.get_or_init(|| {
        let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
        let cfg = config_from_flat(&flat);
        let store = DeviceStore::open(&cfg.devices_path, cfg.max_devices);
        let states = DeviceStates::new(cfg.probe_success_threshold, cfg.probe_failure_threshold);
        let cooldown = Cooldown::new();
        let audit = AuditLog::open(&cfg.history_path, cfg.history_limit);
        Arc::new(Mutex::new(AppState {
            cfg,
            store,
            states,
            cooldown,
            audit,
            sent: 0,
            rejected: 0,
            rejected_by_reason: HashMap::new(),
            probes: 0,
        }))
    })
}

fn reload(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    {
        let mut s = state().lock().unwrap();
        s.states
            .reconfigure(cfg.probe_success_threshold, cfg.probe_failure_threshold);
        s.audit.set_limit(cfg.history_limit);
        s.cfg = cfg;
    }
    if let Some(rt) = runtime() {
        rt.emit("info", "lan-wol configuration reloaded");
    }
    Ok(())
}

fn status_json() -> String {
    let s = state().lock().unwrap();
    let (mut unknown, mut waking, mut online, mut offline) = (0u64, 0u64, 0u64, 0u64);
    for device in &s.store.devices {
        match s.states.state_of(&device.id) {
            DeviceState::Unknown => unknown += 1,
            DeviceState::Waking => waking += 1,
            DeviceState::Online => online += 1,
            DeviceState::Offline => offline += 1,
        }
    }
    format!(
        "{{\"devices\":{},\"states\":{{\"unknown\":{},\"waking\":{},\"online\":{},\"offline\":{}}},\"sent\":{},\"rejected\":{},\"probes\":{},\"send_enabled\":{},\"cooldown_seconds\":{}}}",
        s.store.devices.len(),
        unknown,
        waking,
        online,
        offline,
        s.sent,
        s.rejected,
        s.probes,
        s.cfg.send_enabled,
        s.cfg.cooldown_seconds,
    )
}

fn status() -> String {
    status_json()
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_gauge("devices", "Registered devices");
    metrics.declare_gauge("device_state", "Devices per power state");
    metrics.declare_counter("wake_sent_total", "Magic packets sent");
    metrics.declare_counter("wake_rejected_total", "Wake requests rejected");
    metrics.declare_counter("probe_total", "Read-only TCP probes performed");
    let s = state().lock().unwrap();
    metrics.set("devices", &[], s.store.devices.len() as f64);
    let mut counts = [0u64; 4];
    for device in &s.store.devices {
        match s.states.state_of(&device.id) {
            DeviceState::Unknown => counts[0] += 1,
            DeviceState::Waking => counts[1] += 1,
            DeviceState::Online => counts[2] += 1,
            DeviceState::Offline => counts[3] += 1,
        }
    }
    for (name, value) in [
        ("unknown", counts[0]),
        ("waking", counts[1]),
        ("online", counts[2]),
        ("offline", counts[3]),
    ] {
        metrics.set("device_state", &[("state", name)], value as f64);
    }
    metrics.set_counter("wake_sent_total", &[], s.sent);
    for reason in ["unregistered", "cooldown", "disabled", "invalid"] {
        let value = s.rejected_by_reason.get(reason).copied().unwrap_or(0);
        metrics.set_counter("wake_rejected_total", &[("reason", reason)], value);
    }
    metrics.set_counter("probe_total", &[], s.probes);
}

// ---------------------------------------------------------------------------
// JSON body helpers: strings come from common_component::json_str; small
// numbers and booleans are read here (see lan-relay's json_u8 / json_bool).
// ---------------------------------------------------------------------------

fn json_u16(body: &str, key: &str) -> Option<u16> {
    if let Some(text) = json_str(body, key) {
        if let Ok(value) = text.parse::<u16>() {
            return Some(value);
        }
    }
    let needle = format!("\"{}\"", key);
    let at = body.find(&needle)?;
    let rest = &body[at + needle.len()..];
    let colon = rest.find(':')?;
    let digits: String = rest[colon + 1..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u16>().ok()
}

fn json_bool_opt(body: &str, key: &str) -> Option<bool> {
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.contains(&format!("\"{}\":true", key)) {
        Some(true)
    } else if compact.contains(&format!("\"{}\":false", key)) {
        Some(false)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Endpoints.
// ---------------------------------------------------------------------------

fn view_devices(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    let items: Vec<String> = s
        .store
        .devices
        .iter()
        .map(|device| {
            let base = device.to_json();
            format!(
                "{},\"state\":{}}}",
                &base[..base.len() - 1],
                json_quote(s.states.state_of(&device.id).as_str())
            )
        })
        .collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{}}}",
            items.join(","),
            items.len()
        ),
    )
}

fn view_upsert(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "id is required", 400),
    };
    let mac = match json_str(&req.body, "mac").and_then(|text| parse_mac(&text)) {
        Some(value) => value,
        None => return err(req, "invalid_config", "mac must be 6 hex groups", 400),
    };
    let ip = match json_str(&req.body, "ip") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "ip is required", 400),
    };
    if ip.parse::<IpAddr>().is_err() {
        return err(req, "invalid_config", "ip must be a valid IP address", 400);
    }
    let name = json_str(&req.body, "name").unwrap_or_default();
    let note = json_str(&req.body, "note").unwrap_or_default();
    let broadcast = json_str(&req.body, "broadcast").unwrap_or_default();
    let enabled = json_bool_opt(&req.body, "enabled").unwrap_or(true);
    let mut s = state().lock().unwrap();
    let default_port = s.cfg.probe_port_default;
    let probe_port = json_u16(&req.body, "probe_port").unwrap_or(default_port);
    let device = Device {
        id: id.clone(),
        name,
        mac,
        ip,
        probe_port,
        broadcast,
        enabled,
        note,
        updated_at: now_rfc3339(),
    };
    match s.store.upsert(device) {
        Ok(total) => ok(
            req,
            &format!("{{\"id\":{},\"total\":{}}}", json_quote(&id), total),
        ),
        Err(message) => err(req, "invalid_config", &message, 400),
    }
}

fn view_remove(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "id is required", 400),
    };
    let mut s = state().lock().unwrap();
    let removed = s.store.remove(&id);
    if removed {
        s.states.forget(&id);
    }
    ok(req, &format!("{{\"removed\":{}}}", removed))
}

/// Wake is only ever resolved against the registered device table: any mac/ip
/// carried in the request body is deliberately ignored.
fn view_wake(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "device_id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "device_id is required", 400),
    };

    // Phase 1: pure decision under the lock; clone what the socket call needs.
    let (device, cfg) = {
        let mut s = state().lock().unwrap();
        let now = Instant::now();
        let cooling = s.cooldown.is_cooling(&id, now, s.cfg.cooldown_seconds);
        let decision = check_wake(s.store.get(&id), s.cfg.send_enabled, cooling);
        if let Err(reason) = decision {
            let (code, message, status_code, reason_label) = match reason {
                WakeReject::Unregistered => (
                    "not_found",
                    "device is not registered",
                    404u16,
                    "unregistered",
                ),
                WakeReject::Disabled => (
                    "invalid_config",
                    "wake sending is disabled",
                    409u16,
                    "disabled",
                ),
                WakeReject::Cooldown => {
                    ("rate_limited", "device is in cooldown", 429u16, "cooldown")
                }
            };
            s.rejected += 1;
            *s.rejected_by_reason.entry(reason_label).or_insert(0) += 1;
            s.audit
                .push(AuditEntry::new("wake", &id, "rejected", reason_label));
            return err(req, code, message, status_code);
        }
        (s.store.get(&id).unwrap().clone(), s.cfg.clone())
    };

    // Phase 2: UDP network IO with no lock held.
    let bytes = match send_magic(&device, &cfg) {
        Ok(value) => value,
        Err(message) => {
            let mut s = state().lock().unwrap();
            s.rejected += 1;
            *s.rejected_by_reason.entry("invalid").or_insert(0) += 1;
            s.audit
                .push(AuditEntry::new("wake", &id, "failed", &message));
            return err(req, "internal_error", &message, 500);
        }
    };

    // Phase 3: commit state under the lock, then alert without it.
    {
        let mut s = state().lock().unwrap();
        let now = Instant::now();
        let grace = Duration::from_secs(s.cfg.grace_seconds);
        s.cooldown.mark(&id, now);
        s.states.mark_waking(&id, grace, now);
        s.audit
            .push(AuditEntry::new("wake", &id, "sent", "magic packet sent"));
        s.sent += 1;
    }
    if let Some(rt) = runtime() {
        rt.emit_alert(
            "info",
            "wol_wake_sent",
            "wake packet sent to registered device",
            &id,
        );
    }
    ok(
        req,
        &format!(
            "{{\"sent\":true,\"bytes\":{},\"device_id\":{},\"state\":\"waking\"}}",
            bytes,
            json_quote(&id)
        ),
    )
}

fn view_probe(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "device_id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "device_id is required", 400),
    };
    let (device, timeout_ms) = {
        let s = state().lock().unwrap();
        match s.store.get(&id) {
            Some(device) => (device.clone(), s.cfg.probe_timeout_ms),
            None => return err(req, "not_found", "device is not registered", 404),
        }
    };
    let ip = match device.probe_ip() {
        Some(value) => value,
        None => return err(req, "invalid_config", "device ip is invalid", 400),
    };
    let probe = TcpProbe {
        timeout: Duration::from_millis(timeout_ms),
    };
    let reachable = probe.probe(ip, device.probe_port);
    let (state_name, transition) = {
        let mut s = state().lock().unwrap();
        let (before, transition) = s.states.observe(&id, reachable, Instant::now());
        s.probes += 1;
        if let Some(next) = transition {
            let detail = format!("{}->{}", before.as_str(), next.as_str());
            s.audit
                .push(AuditEntry::new("state", &id, next.as_str(), &detail));
        }
        (
            s.states.state_of(&id).as_str().to_string(),
            transition.map(|value| value.as_str().to_string()),
        )
    };
    let transition_json = transition
        .map(|value| json_quote(&value))
        .unwrap_or_else(|| "null".to_string());
    ok(
        req,
        &format!(
            "{{\"reachable\":{},\"state\":{},\"transition\":{}}}",
            reachable,
            json_quote(&state_name),
            transition_json
        ),
    )
}

fn view_status(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &status_json())
}

fn view_history(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    ok(
        req,
        &format!(
            "{{\"items\":{},\"total\":{},\"limit\":{}}}",
            s.audit.items_json(),
            s.audit.entries.len(),
            s.audit.limit
        ),
    )
}

fn routes(app: App) -> App {
    app.route("GET", "/api/wol/devices", view_devices)
        .route_write("POST", "/api/wol/device", view_upsert)
        .route_write("POST", "/api/wol/remove", view_remove)
        .route_write("POST", "/api/wol/wake", view_wake)
        .route_write("POST", "/api/wol/probe", view_probe)
        .route("GET", "/api/wol/status", view_status)
        .route("GET", "/api/wol/history", view_history)
}

// ---------------------------------------------------------------------------
// Low-frequency background probe loop. It clones the device list under the
// lock and releases it before any network IO, so no socket work ever runs
// while the state mutex is held.
// ---------------------------------------------------------------------------

fn probe_loop(state: Arc<Mutex<AppState>>, cfg: WolConfig, interval: Duration) {
    let probe = TcpProbe {
        timeout: Duration::from_millis(cfg.probe_timeout_ms),
    };
    loop {
        std::thread::sleep(interval);
        let snapshot: Vec<(String, String, u16)> = {
            let s = state.lock().unwrap();
            s.store
                .devices
                .iter()
                .filter(|device| device.enabled)
                .map(|device| (device.id.clone(), device.ip.clone(), device.probe_port))
                .collect()
        };
        let mut alerts: Vec<String> = Vec::new();
        for (id, ip_text, port) in snapshot {
            let ip = match ip_text.parse::<IpAddr>() {
                Ok(value) => value,
                Err(_) => continue,
            };
            let reachable = probe.probe(ip, port); // network IO, no lock held
            let now = Instant::now();
            let mut s = state.lock().unwrap();
            let (before, transition) = s.states.observe(&id, reachable, now);
            s.probes += 1;
            if let Some(next) = transition {
                let detail = format!("{}->{}", before.as_str(), next.as_str());
                s.audit
                    .push(AuditEntry::new("state", &id, next.as_str(), &detail));
                if before == DeviceState::Waking && next == DeviceState::Offline {
                    alerts.push(id.clone());
                }
            }
        }
        for id in alerts {
            if let Some(rt) = runtime() {
                rt.emit_alert(
                    "warning",
                    "device_wake_failed",
                    "device did not come online after wake",
                    &id,
                );
            }
        }
    }
}

fn start() {
    let _ = state();
    let cfg = state().lock().unwrap().cfg.clone();
    let interval = Duration::from_secs(cfg.probe_interval_seconds);
    let handle = Arc::clone(state());
    std::thread::spawn(move || probe_loop(handle, cfg, interval));
}

const HOOKS: Hooks = Hooks {
    name: "lan-wol",
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

#[cfg(test)]
mod tests {
    use super::*;
    use common_config::parse_flat;

    #[test]
    fn template_validates_offline() {
        let flat = parse_flat(DEFAULT_CONFIG);
        assert_eq!(get_str(&flat, "component", ""), "lan-wol");
        validate(&flat).expect("template must validate");
        let cfg = config_from_flat(&flat);
        assert!(!cfg.send_enabled);
        assert_eq!(cfg.port, 9);
        assert_eq!(cfg.max_devices, 512);
        assert_eq!(cfg.cooldown_seconds, 30);
        assert_eq!(cfg.grace_seconds, 90);
        assert_eq!(cfg.probe_interval_seconds, 15);
    }

    #[test]
    fn validation_rejects_path_traversal() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("wol.devices_path".into(), "../evil.json".into());
        assert!(validate(&flat).is_err());
        flat.insert("wol.devices_path".into(), "lan-wol.devices.json".into());
        flat.insert("wol.history_path".into(), "/etc/../passwd".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn validation_rejects_bad_ranges() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("wol.probe_timeout_ms".into(), "10".into());
        assert!(validate(&flat).is_err());
        flat.insert("wol.probe_timeout_ms".into(), "300".into());
        flat.insert("wol.probe_interval_seconds".into(), "1".into());
        assert!(validate(&flat).is_err());
        flat.insert("wol.probe_interval_seconds".into(), "15".into());
        flat.insert("wol.probe_success_threshold".into(), "99".into());
        assert!(validate(&flat).is_err());
        flat.insert("wol.probe_success_threshold".into(), "3".into());
        flat.insert("wol.max_devices".into(), "9999".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn validation_rejects_non_ipv4_broadcast() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("wol.broadcast".into(), "not-an-ip".into());
        assert!(validate(&flat).is_err());
        flat.insert("wol.broadcast".into(), "::1".into());
        assert!(validate(&flat).is_err());
        flat.insert("wol.broadcast".into(), "127.0.0.1".into());
        assert!(validate(&flat).is_ok());
    }

    #[test]
    fn json_helpers_accept_numbers_and_bools() {
        assert_eq!(json_u16("{\"probe_port\":8080}", "probe_port"), Some(8080));
        assert_eq!(json_u16("{\"probe_port\":\"9\"}", "probe_port"), Some(9));
        assert_eq!(json_u16("{}", "probe_port"), None);
        assert_eq!(
            json_bool_opt("{ \"enabled\" : false }", "enabled"),
            Some(false)
        );
        assert_eq!(json_bool_opt("{\"enabled\":true}", "enabled"), Some(true));
        assert_eq!(json_bool_opt("{}", "enabled"), None);
    }

    #[test]
    fn hooks_identity_is_stable() {
        assert_eq!(HOOKS.name, "lan-wol");
        assert!(HOOKS.default_config.contains("component: lan-wol"));
        assert!(!HOOKS.default_config.contains("send_enabled: true"));
    }
}
