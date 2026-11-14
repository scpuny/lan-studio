//! lan-scan — read-only LAN asset / port discovery (M3).

mod scan;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{runtime, Flat, Hooks};
use common_config::{get_str, get_u64};
use common_metrics::Registry;

use scan::{expand_cidr, run_scan, ScanConfig, ScanOutcome, ScanStore, TcpProbe};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();

struct AppState {
    cfg: ScanConfig,
    store: ScanStore,
    running: bool,
    last: ScanOutcome,
    last_segment: String,
    last_error: Option<String>,
}

fn config_from_flat(flat: &Flat) -> ScanConfig {
    let mut cfg = ScanConfig {
        max_hosts: get_u64(flat, "scan.max_hosts", 4_096) as usize,
        max_ports: get_u64(flat, "scan.max_ports", 64) as usize,
        rate_per_second: get_u64(flat, "scan.rate_per_second", 200),
        concurrency: get_u64(flat, "scan.concurrency", 16) as usize,
        connect_timeout_ms: get_u64(flat, "scan.connect_timeout_ms", 300),
        banner_bytes: get_u64(flat, "scan.banner_bytes", 64) as usize,
        results_path: get_str(flat, "scan.results_path", "lan-scan.results.json"),
        max_results: get_u64(flat, "scan.max_results", 200) as usize,
        ..ScanConfig::default()
    };
    let ports = list_numbers(flat, "scan.ports");
    if !ports.is_empty() {
        cfg.ports = ports;
    }
    cfg
}

fn list_numbers(flat: &Flat, prefix: &str) -> Vec<u16> {
    let mut items: Vec<(usize, u16)> = flat
        .iter()
        .filter_map(|(key, value)| {
            let inner = key
                .strip_prefix(prefix)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            let index: usize = inner.parse().ok()?;
            let port: u16 = value.parse().ok()?;
            Some((index, port))
        })
        .collect();
    items.sort_by_key(|(index, _)| *index);
    items.into_iter().map(|(_, port)| port).collect()
}

fn validate(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    if !(1..=65_536).contains(&cfg.max_hosts) {
        return Err("scan.max_hosts must be 1..65536".to_string());
    }
    if !(1..=64).contains(&cfg.max_ports) {
        return Err("scan.max_ports must be 1..64".to_string());
    }
    if !(1..=1_000).contains(&cfg.rate_per_second) {
        return Err("scan.rate_per_second must be 1..1000".to_string());
    }
    if !(1..=64).contains(&cfg.concurrency) {
        return Err("scan.concurrency must be 1..64".to_string());
    }
    if !(50..=5_000).contains(&cfg.connect_timeout_ms) {
        return Err("scan.connect_timeout_ms must be 50..5000".to_string());
    }
    if cfg.banner_bytes > 256 {
        return Err("scan.banner_bytes must be <= 256".to_string());
    }
    if cfg.ports.is_empty() || cfg.ports.len() > cfg.max_ports {
        return Err("scan.ports must contain 1..max_ports entries".to_string());
    }
    if cfg.results_path.contains("..") {
        return Err("scan.results_path must not contain '..'".to_string());
    }
    Ok(())
}

fn state() -> &'static Arc<Mutex<AppState>> {
    STATE.get_or_init(|| {
        let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
        let cfg = config_from_flat(&flat);
        let store = ScanStore::open(&cfg.results_path, cfg.max_results);
        Arc::new(Mutex::new(AppState {
            cfg,
            store,
            running: false,
            last: ScanOutcome::default(),
            last_segment: String::new(),
            last_error: None,
        }))
    })
}

fn reload(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    state().lock().unwrap().cfg = cfg;
    if let Some(rt) = runtime() {
        rt.emit("info", "lan-scan configuration reloaded");
    }
    Ok(())
}

fn status() -> String {
    let s = state().lock().unwrap();
    format!(
        "{{\"running\":{},\"last_segment\":{},\"last_error\":{},\"scan\":{{\"hosts_found\":{},\"total_hosts\":{},\"probes\":{},\"throttled\":{},\"open_ports\":{},\"rate_per_second\":{},\"concurrency\":{},\"max_hosts\":{},\"results\":{}}}}}",
        s.running,
        json_quote(&s.last_segment),
        s.last_error.as_deref().map(json_quote).unwrap_or_else(|| "null".to_string()),
        s.last.hosts.len(),
        s.last.total_hosts,
        s.last.probes,
        s.last.throttled,
        s.last.open_ports,
        s.cfg.rate_per_second,
        s.cfg.concurrency,
        s.cfg.max_hosts,
        s.store.results.len(),
    )
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_counter("scan_hosts_total", "Hosts probed");
    metrics.declare_counter("scan_probes_total", "TCP probes performed");
    metrics.declare_counter(
        "scan_throttled_total",
        "Probes rejected by the rate limiter",
    );
    metrics.declare_gauge("scan_open_ports", "Open ports in the last scan");
    metrics.declare_gauge("scan_results", "Retained result rows");
    metrics.declare_gauge("scan_rate_per_second", "Configured probe ceiling");
    let s = state().lock().unwrap();
    metrics.set_counter("scan_hosts_total", &[], s.last.total_hosts as u64);
    metrics.set_counter("scan_probes_total", &[], s.last.probes);
    metrics.set_counter("scan_throttled_total", &[], s.last.throttled);
    metrics.set("scan_open_ports", &[], s.last.open_ports as f64);
    metrics.set("scan_results", &[], s.store.results.len() as f64);
    metrics.set("scan_rate_per_second", &[], s.cfg.rate_per_second as f64);
}

/// Parse a flat JSON array of port numbers, e.g. `"ports":[80,443]`.
fn json_ports(body: &str, key: &str) -> Option<Vec<u16>> {
    let needle = format!("\"{}\"", key);
    let at = body.find(&needle)?;
    let rest = &body[at + needle.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let open = after.find('[')?;
    let close = after.find(']')?;
    if close < open {
        return None;
    }
    let mut out = Vec::new();
    for piece in after[open + 1..close].split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue; // `[]` is a legitimate empty selection
        }
        let value: u16 = piece.parse().ok()?;
        if value > 0 {
            out.push(value);
        }
    }
    Some(out)
}

fn view_scan_status(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &status())
}

fn view_results(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    let items: Vec<String> = s.store.results.iter().cloned().collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{},\"limit\":{}}}",
            items.join(","),
            items.len(),
            s.cfg.max_results
        ),
    )
}

fn view_snapshot(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &state().lock().unwrap().store.snapshot_json())
}

fn view_start(req: &Request, _: &common_api::Context) -> Response {
    let segment = match common_component::json_str(&req.body, "segment") {
        Some(value) => value,
        None => return err(req, "invalid_config", "segment (CIDR) is required", 400),
    };
    let ports = json_ports(&req.body, "ports").unwrap_or_default();
    let (cfg, already_running) = {
        let s = state().lock().unwrap();
        (s.cfg.clone(), s.running)
    };
    if already_running {
        return err(req, "rate_limited", "a scan is already running", 429);
    }
    let selected = if ports.is_empty() {
        cfg.ports.clone()
    } else {
        ports
    };
    if selected.len() > cfg.max_ports {
        return err(req, "invalid_config", "too many ports", 400);
    }
    let ips = match expand_cidr(&segment, cfg.max_hosts) {
        Ok(value) => value,
        Err(message) => return err(req, "invalid_config", &message, 400),
    };
    {
        let mut s = state().lock().unwrap();
        s.running = true;
        s.last_segment = segment.clone();
        s.last_error = None;
    }
    let host_count = ips.len();
    let port_count = selected.len();
    let thread_state = state().clone();
    let thread_segment = segment.clone();
    std::thread::spawn(move || {
        let probe = TcpProbe {
            timeout: Duration::from_millis(cfg.connect_timeout_ms),
            banner_bytes: cfg.banner_bytes,
        };
        let outcome = run_scan(&probe, ips, &selected, &cfg);
        {
            let mut s = thread_state.lock().unwrap();
            s.store.ingest(&outcome, &thread_segment);
            s.last = outcome.clone();
            s.running = false;
        }
        if let Some(rt) = runtime() {
            rt.emit(
                "info",
                &format!("scan finished: {} open ports", outcome.open_ports),
            );
            if outcome.throttled > 0 {
                rt.emit_alert(
                    "info",
                    "scan_anomaly",
                    "scan rate ceiling reached",
                    &thread_segment,
                );
            }
        }
    });
    ok(
        req,
        &format!(
            "{{\"started\":true,\"segment\":{},\"hosts\":{},\"ports\":{}}}",
            json_quote(&segment),
            host_count,
            port_count
        ),
    )
}

fn routes(app: App) -> App {
    app.route("GET", "/api/scan/status", view_scan_status)
        .route("GET", "/api/scan/results", view_results)
        .route("GET", "/api/scan/snapshot", view_snapshot)
        .route("POST", "/api/scan/start", view_start)
}

fn start() {
    let _ = state();
}

const HOOKS: Hooks = Hooks {
    name: "lan-scan",
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
    fn template_validates_with_bounded_defaults() {
        let flat = parse_flat(DEFAULT_CONFIG);
        assert_eq!(get_str(&flat, "component", ""), "lan-scan");
        validate(&flat).expect("template must validate");
        let cfg = config_from_flat(&flat);
        assert_eq!(cfg.ports, vec![22, 80, 443, 5353, 8080]);
        assert_eq!(cfg.rate_per_second, 200);
        assert_eq!(cfg.concurrency, 16);
    }

    #[test]
    fn validation_rejects_unbounded_scans() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("scan.rate_per_second".into(), "0".into());
        assert!(validate(&flat).is_err());
        flat.insert("scan.rate_per_second".into(), "200".into());
        flat.insert("scan.concurrency".into(), "999".into());
        assert!(validate(&flat).is_err());
        flat.insert("scan.concurrency".into(), "16".into());
        flat.insert("scan.results_path".into(), "../mdns.yaml".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn port_arrays_are_parsed() {
        assert_eq!(
            json_ports("{\"segment\":\"127.0.0.1/32\",\"ports\":[80,443]}", "ports"),
            Some(vec![80, 443])
        );
        assert_eq!(json_ports("{\"ports\":[]}", "ports"), Some(vec![]));
    }

    #[test]
    fn oversized_segment_is_rejected_before_socket_work() {
        let flat = parse_flat(DEFAULT_CONFIG);
        let cfg = config_from_flat(&flat);
        assert!(expand_cidr("10.0.0.0/8", cfg.max_hosts).is_err());
    }
}
