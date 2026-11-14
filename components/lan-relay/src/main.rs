//! lan-relay — cross-segment / cross-VLAN mDNS relay (M3).
//!
//! Business logic lives in `loop_guard.rs`; this file composes the shared
//! component runtime, config validation, metrics, HTTP endpoints and the
//! multicast transport. See docs/PRD.md §4.3 and docs/M3-CHECKLIST.md.

mod loop_guard;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{runtime, Flat, Hooks};
use common_config::{get_bool, get_u16, get_u32, get_u64};
use common_metrics::Registry;
use common_net::{list_interfaces, local_ipv4_addrs, MdnsSocket};

use loop_guard::{decode_frame, encode_frame, GuardConfig, LoopGuard, Verdict};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static GUARD: OnceLock<Arc<Mutex<LoopGuard>>> = OnceLock::new();
static TRANSPORT: OnceLock<Mutex<String>> = OnceLock::new();
static LAST_LOOP_ALERT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
struct RelayConfig {
    group: Ipv4Addr,
    port: u16,
    interfaces: Vec<String>,
    allowed_networks: Vec<String>,
    fingerprint_window_ms: u64,
    max_hops: u8,
    dedup_capacity: usize,
    max_forward_per_second: u64,
    transport: bool,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            group: Ipv4Addr::new(224, 0, 0, 251),
            port: 5353,
            interfaces: vec!["eth0".into(), "eth1".into()],
            allowed_networks: Vec::new(),
            fingerprint_window_ms: 5_000,
            max_hops: 4,
            dedup_capacity: 4_096,
            max_forward_per_second: 2_000,
            transport: true,
        }
    }
}

impl RelayConfig {
    fn guard_config(&self) -> GuardConfig {
        GuardConfig {
            interfaces: self.interfaces.clone(),
            allowed_networks: self.allowed_networks.clone(),
            fingerprint_window_ms: self.fingerprint_window_ms,
            max_hops: self.max_hops,
            dedup_capacity: self.dedup_capacity,
            max_forward_per_second: self.max_forward_per_second,
        }
    }

    fn from_flat(flat: &Flat) -> Result<Self, String> {
        let mut cfg = Self::default();
        if let Some(value) = flat.get("relay.group") {
            cfg.group = value
                .parse::<Ipv4Addr>()
                .map_err(|_| "relay.group must be an IPv4 address".to_string())?;
        }
        cfg.port = get_u16(flat, "relay.port", cfg.port);
        let interfaces = list_values(flat, "relay.interfaces");
        if !interfaces.is_empty() {
            cfg.interfaces = interfaces;
        }
        let networks = list_values(flat, "relay.allowed_networks");
        cfg.allowed_networks = networks;
        cfg.fingerprint_window_ms = get_u64(
            flat,
            "relay.fingerprint_window_ms",
            cfg.fingerprint_window_ms,
        );
        cfg.max_hops = get_u32(flat, "relay.max_hops", cfg.max_hops as u32) as u8;
        cfg.dedup_capacity =
            get_u64(flat, "relay.dedup_capacity", cfg.dedup_capacity as u64) as usize;
        cfg.max_forward_per_second = get_u64(
            flat,
            "relay.max_forward_per_second",
            cfg.max_forward_per_second,
        );
        cfg.transport = get_bool(flat, "relay.transport", cfg.transport);
        Ok(cfg)
    }
}

/// Read `prefix[0]`, `prefix[1]`, ... in numeric order (common-config's
/// flattened list representation).
fn list_values(flat: &Flat, prefix: &str) -> Vec<String> {
    let mut items: Vec<(usize, String)> = flat
        .iter()
        .filter_map(|(key, value)| {
            let rest = key.strip_prefix(prefix)?;
            let inner = rest.strip_prefix('[')?.strip_suffix(']')?;
            let index: usize = inner.parse().ok()?;
            if value.is_empty() {
                return None;
            }
            Some((index, value.clone()))
        })
        .collect();
    items.sort_by_key(|(index, _)| *index);
    items.into_iter().map(|(_, value)| value).collect()
}

fn validate(flat: &Flat) -> Result<(), String> {
    let cfg = RelayConfig::from_flat(flat)?;
    if !cfg.group.is_multicast() {
        return Err("relay.group must be a multicast IPv4 address".to_string());
    }
    if cfg.port == 0 {
        return Err("relay.port must be 1..65535".to_string());
    }
    if !(1..=16).contains(&cfg.max_hops) {
        return Err("relay.max_hops must be 1..16".to_string());
    }
    if !(100..=60_000).contains(&cfg.fingerprint_window_ms) {
        return Err("relay.fingerprint_window_ms must be 100..60000".to_string());
    }
    if !(1..=100_000).contains(&cfg.dedup_capacity) {
        return Err("relay.dedup_capacity must be 1..100000".to_string());
    }
    if !(1..=100_000).contains(&cfg.max_forward_per_second) {
        return Err("relay.max_forward_per_second must be 1..100000".to_string());
    }
    if cfg.interfaces.is_empty() || cfg.interfaces.len() > 32 {
        return Err("relay.interfaces must contain 1..32 entries".to_string());
    }
    if cfg.allowed_networks.len() > 64 {
        return Err("relay.allowed_networks must contain at most 64 entries".to_string());
    }
    for network in &cfg.allowed_networks {
        if loop_guard::Cidr::parse(network).is_none() {
            return Err(format!("invalid CIDR: {}", network));
        }
    }
    Ok(())
}

fn guard() -> &'static Arc<Mutex<LoopGuard>> {
    GUARD.get_or_init(|| {
        let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
        let cfg = RelayConfig::from_flat(&flat).unwrap_or_default();
        Arc::new(Mutex::new(LoopGuard::new(cfg.guard_config())))
    })
}

fn reload(flat: &Flat) -> Result<(), String> {
    let cfg = RelayConfig::from_flat(flat)?;
    guard().lock().unwrap().reconfigure(cfg.guard_config());
    if let Some(rt) = runtime() {
        rt.emit("info", "relay loop guard reconfigured");
    }
    Ok(())
}

fn transport_state() -> String {
    TRANSPORT
        .get_or_init(|| Mutex::new("starting".to_string()))
        .lock()
        .unwrap()
        .clone()
}

fn status() -> String {
    let json = guard().lock().unwrap().status_json();
    format!(
        "{{\"transport\":{},\"loop_guard\":{}}}",
        json_quote(&transport_state()),
        json
    )
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_counter(
        "relay_packets_received_total",
        "Packets observed by the loop guard",
    );
    metrics.declare_counter(
        "relay_packets_forwarded_total",
        "Packets forwarded to other NICs",
    );
    metrics.declare_counter("relay_loop_dropped_total", "Packets dropped by loop guards");
    metrics.declare_counter("relay_bytes_forwarded_total", "Bytes forwarded");
    metrics.declare_gauge(
        "relay_fingerprint_entries",
        "Entries in the fingerprint window",
    );
    let guard = guard().lock().unwrap();
    let c = guard.counters;
    metrics.set_counter("relay_packets_received_total", &[], c.received);
    metrics.set_counter("relay_packets_forwarded_total", &[], c.forwarded);
    metrics.set_counter("relay_bytes_forwarded_total", &[], c.bytes_forwarded);
    for (reason, value) in [
        ("duplicate", c.duplicate),
        ("same_interface", c.same_interface),
        ("ttl_exceeded", c.ttl_exceeded),
        ("self_generated", c.self_generated),
        ("outside_whitelist", c.outside_whitelist),
        ("rate_limited", c.rate_limited),
        ("malformed", c.malformed),
    ] {
        metrics.set_counter("relay_loop_dropped_total", &[("reason", reason)], value);
    }
    metrics.set("relay_fingerprint_entries", &[], guard.dedup_len() as f64);
}

// ---------------------------------------------------------------------------
// Component endpoints
// ---------------------------------------------------------------------------

fn json_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|item| json_quote(item)).collect();
    format!("[{}]", parts.join(","))
}

fn view_stats(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &guard().lock().unwrap().status_json())
}

fn view_interfaces(req: &Request, _: &common_api::Context) -> Response {
    let physical: Vec<String> = list_interfaces()
        .iter()
        .map(|info| {
            format!(
                "{{\"name\":{},\"addr\":{}}}",
                json_quote(&info.name),
                json_quote(&info.addr.to_string())
            )
        })
        .collect();
    let logical = json_array(&guard().lock().unwrap().cfg.interfaces);
    ok(
        req,
        &format!(
            "{{\"logical\":{},\"physical\":[{}],\"transport\":{}}}",
            logical,
            physical.join(","),
            json_quote(&transport_state())
        ),
    )
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks(2) {
        let hex = std::str::from_utf8(chunk).ok()?;
        out.push(u8::from_str_radix(hex, 16).ok()?);
    }
    Some(out)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Read a small JSON integer whether it was quoted or written as a number.
fn json_u8(body: &str, key: &str) -> Option<u8> {
    if let Some(text) = common_component::json_str(body, key) {
        if let Ok(value) = text.parse::<u8>() {
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
    digits.parse::<u8>().ok()
}

fn json_bool(body: &str, key: &str) -> bool {
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    compact.contains(&format!("\"{}\":true", key))
}

/// Deterministic injection hook used by the M3 acceptance script to exercise
/// all four loop guards without requiring real multicast hardware.
fn view_inject(req: &Request, _: &common_api::Context) -> Response {
    let payload = match common_component::json_str(&req.body, "payload_hex")
        .and_then(|text| hex_decode(&text))
    {
        Some(value) if !value.is_empty() => value,
        _ => {
            return err(
                req,
                "invalid_config",
                "payload_hex is required (even-length hex)",
                400,
            )
        }
    };
    let ingress =
        common_component::json_str(&req.body, "ingress").unwrap_or_else(|| "eth0".to_string());
    let source = common_component::json_str(&req.body, "source_ip")
        .and_then(|text| text.parse::<Ipv4Addr>().ok())
        .unwrap_or(Ipv4Addr::new(192, 168, 1, 5));
    let self_claimed = json_bool(&req.body, "self_generated");
    let frame = match json_u8(&req.body, "ttl") {
        Some(ttl) => {
            let base = decode_frame(&payload)
                .map(|frame| frame.base)
                .unwrap_or(payload);
            match encode_frame(&base, ttl) {
                Ok(value) => value,
                Err(()) => return err(req, "invalid_config", "payload is not a DNS message", 400),
            }
        }
        None => payload,
    };
    let (verdict, snapshot) = {
        let mut guard = guard().lock().unwrap();
        let verdict = guard.decide(&frame, &ingress, source, self_claimed, Instant::now());
        (verdict, guard.status_json())
    };
    let (ttl, targets, frame_hex) = match &verdict {
        Verdict::Forward {
            ttl,
            targets,
            frame,
        } => (*ttl, json_array(targets), json_quote(&hex_encode(frame))),
        _ => (0, "[]".to_string(), "\"\"".to_string()),
    };
    ok(
        req,
        &format!(
            "{{\"verdict\":{},\"ttl\":{},\"targets\":{},\"frame_hex\":{},\"loop_guard\":{}}}",
            json_quote(verdict.as_str()),
            ttl,
            targets,
            frame_hex,
            snapshot
        ),
    )
}

/// Register a locally generated packet; it must never be relayed.
fn view_mark_local(req: &Request, _: &common_api::Context) -> Response {
    let payload = match common_component::json_str(&req.body, "payload_hex")
        .and_then(|text| hex_decode(&text))
    {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "payload_hex is required", 400),
    };
    guard()
        .lock()
        .unwrap()
        .mark_self_generated(&payload, Instant::now());
    ok(req, "{\"registered\":true}")
}

fn routes(app: App) -> App {
    app.route("GET", "/api/relay/stats", view_stats)
        .route("GET", "/api/relay/interfaces", view_interfaces)
        .route("POST", "/api/relay/inject", view_inject)
        .route("POST", "/api/relay/mark-local", view_mark_local)
}

// ---------------------------------------------------------------------------
// Multicast transport (best effort; the loop guard is always available)
// ---------------------------------------------------------------------------

fn resolve_interfaces(cfg: &RelayConfig) -> Vec<(String, Ipv4Addr)> {
    let physical = list_interfaces();
    let mut out = Vec::new();
    for name in &cfg.interfaces {
        if let Ok(addr) = name.parse::<Ipv4Addr>() {
            out.push((name.clone(), addr));
            continue;
        }
        if let Some(info) = physical.iter().find(|item| item.name == *name) {
            out.push((name.clone(), info.addr));
        }
    }
    if out.is_empty() {
        for (index, addr) in local_ipv4_addrs()
            .into_iter()
            .take(cfg.interfaces.len().max(1))
            .enumerate()
        {
            let name = cfg
                .interfaces
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("if{}", index));
            out.push((name, addr));
        }
    }
    out
}

fn maybe_emit_loop_alert(reason: &str) {
    let second = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if LAST_LOOP_ALERT.swap(second, Ordering::Relaxed) == second {
        return;
    }
    if let Some(rt) = runtime() {
        let (level, kind) = if reason == "ttl_exceeded" {
            ("warning", "relay_ttl_exceeded")
        } else {
            ("critical", "relay_loop_detected")
        };
        rt.emit_alert(level, kind, "mDNS relay loop detected", reason);
    }
}

fn relay_loop(name: String, socket: Arc<MdnsSocket>, all: Vec<(String, Arc<MdnsSocket>)>) {
    let mut buf = [0u8; 9000];
    loop {
        match socket.recv(&mut buf) {
            Ok((len, from)) => {
                let source = match from.ip() {
                    IpAddr::V4(addr) => addr,
                    _ => Ipv4Addr::UNSPECIFIED,
                };
                let verdict = guard().lock().unwrap().decide(
                    &buf[..len],
                    &name,
                    source,
                    false,
                    Instant::now(),
                );
                let reason = verdict.as_str();
                match verdict {
                    Verdict::Forward { frame, targets, .. } => {
                        for target in targets {
                            if target == name {
                                continue; // guard 2, re-checked at the socket layer
                            }
                            if let Some((_, peer)) =
                                all.iter().find(|(candidate, _)| *candidate == target)
                            {
                                let _ = peer.broadcast(&frame);
                            }
                        }
                    }
                    Verdict::Duplicate | Verdict::TtlExceeded => maybe_emit_loop_alert(reason),
                    _ => {}
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn start_transport(cfg: &RelayConfig) -> String {
    if !cfg.transport {
        return "disabled".to_string();
    }
    let mut bound: Vec<(String, Arc<MdnsSocket>)> = Vec::new();
    for (name, addr) in resolve_interfaces(cfg) {
        match MdnsSocket::bind(cfg.group, cfg.port, &[addr], false) {
            Ok(socket) => bound.push((name, Arc::new(socket))),
            Err(error) => {
                if let Some(rt) = runtime() {
                    rt.emit("warning", &format!("cannot bind {}: {}", name, error));
                }
            }
        }
    }
    if bound.is_empty() {
        return "unavailable: no multicast socket could be bound".to_string();
    }
    let count = bound.len();
    for (name, socket) in &bound {
        let name = name.clone();
        let socket = Arc::clone(socket);
        let all = bound.clone();
        std::thread::spawn(move || relay_loop(name, socket, all));
    }
    format!("active: {} socket(s)", count)
}

fn start() {
    // Force initialisation with the on-disk configuration before any endpoint
    // or metrics exposition touches the guard.
    let _ = guard();
    let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
    let cfg = RelayConfig::from_flat(&flat).unwrap_or_default();
    let state = start_transport(&cfg);
    let slot = TRANSPORT.get_or_init(|| Mutex::new(String::new()));
    *slot.lock().unwrap() = state;
}

const HOOKS: Hooks = Hooks {
    name: "lan-relay",
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
        assert_eq!(common_config::get_str(&flat, "component", ""), "lan-relay");
        validate(&flat).expect("template must validate");
        let cfg = RelayConfig::from_flat(&flat).unwrap();
        assert_eq!(cfg.max_hops, 4);
        assert_eq!(cfg.fingerprint_window_ms, 5_000);
        assert_eq!(cfg.interfaces, vec!["eth0", "eth1"]);
        assert_eq!(cfg.allowed_networks.len(), 3);
    }

    #[test]
    fn list_values_are_ordered() {
        let flat = parse_flat("relay:\n  interfaces:\n    - \"eth0\"\n    - \"eth1\"\n");
        assert_eq!(list_values(&flat, "relay.interfaces"), vec!["eth0", "eth1"]);
    }

    #[test]
    fn validation_rejects_unsafe_hop_and_window_settings() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("relay.max_hops".into(), "0".into());
        assert!(validate(&flat).is_err());
        flat.insert("relay.max_hops".into(), "4".into());
        flat.insert("relay.allowed_networks[9]".into(), "not-a-cidr".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = vec![0x00, 0x29, 0xff, 0x10];
        assert_eq!(hex_decode(&hex_encode(&bytes)), Some(bytes));
    }

    #[test]
    fn inject_body_helpers_accept_numbers_and_bools() {
        assert_eq!(json_u8("{\"ttl\":0}", "ttl"), Some(0));
        assert_eq!(json_u8("{\"ttl\":\"3\"}", "ttl"), Some(3));
        assert!(json_bool("{ \"self_generated\" : true }", "self_generated"));
        assert!(!json_bool("{\"self_generated\": false}", "self_generated"));
    }

    #[test]
    fn hooks_expose_expected_identity() {
        assert_eq!(HOOKS.name, "lan-relay");
        assert!(HOOKS.default_config.contains("component: lan-relay"));
    }
}
