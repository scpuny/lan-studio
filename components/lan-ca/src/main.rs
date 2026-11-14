//! lan-ca — lightweight internal certificate management (M3).

mod ca;

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{runtime, Flat, Hooks};
use common_config::{get_str, get_u64};
use common_metrics::Registry;

use ca::{now_secs, rfc3339, CaStore, Certificate};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static STORE: OnceLock<Arc<Mutex<CaStore>>> = OnceLock::new();
static CONFIG: OnceLock<RwLock<CaConfig>> = OnceLock::new();

#[derive(Debug, Clone)]
struct CaConfig {
    default_days: u64,
    max_days: u64,
    allowed_suffixes: Vec<String>,
    max_certificates: usize,
    root_key_path: String,
    store_path: String,
}

impl CaConfig {
    fn from_flat(flat: &Flat) -> Self {
        Self {
            default_days: get_u64(flat, "ca.default_days", 90),
            max_days: get_u64(flat, "ca.max_days", 3_650),
            allowed_suffixes: list_values(flat, "ca.allowed_suffixes"),
            max_certificates: get_u64(flat, "ca.max_certificates", 1_000) as usize,
            root_key_path: get_str(flat, "ca.root_key_path", "lan-ca.root.key"),
            store_path: get_str(flat, "ca.store_path", "lan-ca.store.json"),
        }
    }
}

fn list_values(flat: &Flat, prefix: &str) -> Vec<String> {
    let mut items: Vec<(usize, String)> = flat
        .iter()
        .filter_map(|(key, value)| {
            let inner = key
                .strip_prefix(prefix)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            let index: usize = inner.parse().ok()?;
            (!value.is_empty()).then_some((index, value.clone()))
        })
        .collect();
    items.sort_by_key(|(index, _)| *index);
    items.into_iter().map(|(_, value)| value).collect()
}

fn config() -> CaConfig {
    CONFIG
        .get_or_init(|| {
            let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
            RwLock::new(CaConfig::from_flat(&flat))
        })
        .read()
        .unwrap()
        .clone()
}

fn validate(flat: &Flat) -> Result<(), String> {
    let cfg = CaConfig::from_flat(flat);
    if cfg.default_days == 0 || cfg.default_days > cfg.max_days {
        return Err("ca.default_days must be 1..max_days".to_string());
    }
    if !(1..=3_650).contains(&cfg.max_days) {
        return Err("ca.max_days must be 1..3650".to_string());
    }
    if !(1..=100_000).contains(&cfg.max_certificates) {
        return Err("ca.max_certificates must be 1..100000".to_string());
    }
    if cfg.root_key_path.contains("..") || cfg.store_path.contains("..") {
        return Err("ca paths must not contain '..'".to_string());
    }
    for suffix in &cfg.allowed_suffixes {
        if !suffix.starts_with('.') || suffix.len() < 2 {
            return Err(format!("invalid internal suffix: {}", suffix));
        }
    }
    Ok(())
}

fn reload(flat: &Flat) -> Result<(), String> {
    let next = CaConfig::from_flat(flat);
    let slot = CONFIG.get_or_init(|| RwLock::new(next.clone()));
    *slot.write().unwrap() = next;
    if let Some(rt) = runtime() {
        rt.emit("info", "lan-ca configuration reloaded");
    }
    // Store paths are fixed for the process lifetime; a restart is required
    // to move the key/store files (documented in README).
    Ok(())
}

fn store() -> &'static Arc<Mutex<CaStore>> {
    STORE.get_or_init(|| {
        let cfg = config();
        let ca = CaStore::open(&cfg.root_key_path, &cfg.store_path, cfg.max_certificates)
            .unwrap_or_else(|error| panic!("cannot open CA store: {}", error));
        Arc::new(Mutex::new(ca))
    })
}

fn status() -> String {
    let ca = store().lock().unwrap();
    format!(
        "{{\"trust_model\":\"single-self-signed-root\",\"algorithm\":\"hmac-sha256\",\"ca\":{}}}",
        ca.status_json()
    )
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_gauge("ca_certificates", "Certificates by status");
    metrics.declare_counter("ca_issued_total", "Certificates issued");
    metrics.declare_counter("ca_revoked_total", "Certificates revoked");
    metrics.declare_gauge("ca_audit_entries", "Audit entries retained");
    let ca = store().lock().unwrap();
    let active = ca
        .certificates
        .iter()
        .filter(|c| c.status == "active")
        .count();
    let revoked = ca
        .certificates
        .iter()
        .filter(|c| c.status == "revoked")
        .count();
    metrics.set("ca_certificates", &[("status", "active")], active as f64);
    metrics.set("ca_certificates", &[("status", "revoked")], revoked as f64);
    metrics.set_counter("ca_issued_total", &[], ca.certificates.len() as u64);
    metrics.set_counter("ca_revoked_total", &[], revoked as u64);
    metrics.set("ca_audit_entries", &[], ca.audit.len() as f64);
}

fn body_str(req: &Request, key: &str) -> Option<String> {
    common_component::json_str(&req.body, key)
        .or_else(|| common_component::query_param(&req.query, key))
}

fn body_u64(req: &Request, key: &str, default: u64) -> u64 {
    if let Some(text) = body_str(req, key) {
        if let Ok(value) = text.parse::<u64>() {
            return value;
        }
    }
    // JSON numbers are not quoted; `json_str` only reads string values.
    let needle = format!("\"{}\"", key);
    let digits = req.body.find(&needle).and_then(|at| {
        let rest = &req.body[at + needle.len()..];
        let colon = rest.find(':')?;
        Some(
            rest[colon + 1..]
                .trim_start()
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>(),
        )
    });
    digits
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(default)
}

fn view_root(req: &Request, _: &common_api::Context) -> Response {
    let cfg = config();
    ok(
        req,
        &format!(
            "{{\"fingerprint\":{},\"algorithm\":\"hmac-sha256\",\"trust_model\":\"single-self-signed-root\",\"key_path\":{},\"key_exported\":false}}",
            json_quote(&store().lock().unwrap().root.fingerprint),
            json_quote(&cfg.root_key_path)
        ),
    )
}

fn view_certificates(req: &Request, _: &common_api::Context) -> Response {
    let ca = store().lock().unwrap();
    let items: Vec<String> = ca.certificates.iter().map(Certificate::to_json).collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{}}}",
            items.join(","),
            items.len()
        ),
    )
}

fn view_crl(req: &Request, _: &common_api::Context) -> Response {
    let ca = store().lock().unwrap();
    let items: Vec<String> = ca.crl().iter().map(Certificate::to_json).collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{},\"generated_at\":{}}}",
            items.join(","),
            items.len(),
            json_quote(&rfc3339(now_secs()))
        ),
    )
}

fn view_audit(req: &Request, _: &common_api::Context) -> Response {
    let ca = store().lock().unwrap();
    let items: Vec<String> = ca.audit.iter().map(|entry| entry.to_json()).collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{}}}",
            items.join(","),
            items.len()
        ),
    )
}

fn view_export(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &store().lock().unwrap().export_json())
}

fn view_issue(req: &Request, _: &common_api::Context) -> Response {
    let subject = match body_str(req, "subject") {
        Some(value) => value,
        None => return err(req, "invalid_config", "subject is required", 400),
    };
    let sans = ca::parse_str_array(&req.body, "sans").unwrap_or_default();
    let cfg = config();
    let days = body_u64(req, "days", cfg.default_days);
    if days > cfg.max_days {
        return err(req, "invalid_config", "days exceeds ca.max_days", 400);
    }
    let result = {
        let mut ca = store().lock().unwrap();
        ca.issue(&subject, sans, days, &cfg.allowed_suffixes)
    };
    match result {
        Ok(cert) => {
            if let Some(rt) = runtime() {
                rt.emit_alert("info", "cert_issued", "certificate issued", &subject);
            }
            ok(
                req,
                &format!(
                    "{{\"certificate\":{},\"pem\":{}}}",
                    cert.to_json(),
                    json_quote(&cert.to_pem())
                ),
            )
        }
        Err(message) => err(req, "invalid_config", &message, 400),
    }
}

fn view_renew(req: &Request, _: &common_api::Context) -> Response {
    let serial = match body_str(req, "serial") {
        Some(value) => value,
        None => return err(req, "invalid_config", "serial is required", 400),
    };
    let cfg = config();
    let days = body_u64(req, "days", cfg.default_days);
    if days > cfg.max_days {
        return err(req, "invalid_config", "days exceeds ca.max_days", 400);
    }
    match store().lock().unwrap().renew(&serial, days) {
        Ok(cert) => ok(req, &format!("{{\"certificate\":{}}}", cert.to_json())),
        Err(message) => err(req, "invalid_config", &message, 400),
    }
}

fn view_revoke(req: &Request, _: &common_api::Context) -> Response {
    let serial = match body_str(req, "serial") {
        Some(value) => value,
        None => return err(req, "invalid_config", "serial is required", 400),
    };
    let reason = body_str(req, "reason").unwrap_or_else(|| "unspecified".to_string());
    match store().lock().unwrap().revoke(&serial, &reason) {
        Ok(cert) => {
            if let Some(rt) = runtime() {
                rt.emit_alert("info", "cert_revoked", "certificate revoked", &serial);
            }
            ok(req, &format!("{{\"certificate\":{}}}", cert.to_json()))
        }
        Err(message) => err(req, "invalid_config", &message, 400),
    }
}

fn view_verify(req: &Request, _: &common_api::Context) -> Response {
    let serial = match body_str(req, "serial") {
        Some(value) => value,
        None => return err(req, "invalid_config", "serial is required", 400),
    };
    let ca = store().lock().unwrap();
    let found = ca.certificates.iter().find(|cert| cert.serial == serial);
    match found {
        Some(cert) => ok(
            req,
            &format!(
                "{{\"serial\":{},\"valid\":{},\"status\":{}}}",
                json_quote(&serial),
                ca.verify(cert),
                json_quote(&cert.status)
            ),
        ),
        None => err(req, "not_found", "serial not found", 404),
    }
}

fn routes(app: App) -> App {
    app.route("GET", "/api/ca/root", view_root)
        .route("GET", "/api/ca/certificates", view_certificates)
        .route("GET", "/api/ca/crl", view_crl)
        .route("GET", "/api/ca/audit", view_audit)
        .route("GET", "/api/ca/export", view_export)
        .route("POST", "/api/ca/issue", view_issue)
        .route("POST", "/api/ca/renew", view_renew)
        .route("POST", "/api/ca/revoke", view_revoke)
        .route("POST", "/api/ca/verify", view_verify)
}

fn start() {
    let _ = store();
    // Warn on certificates expiring within 7 days (ISO strings sort by time).
    let threshold = rfc3339(now_secs() + 7 * 86_400);
    let expiring = {
        let ca = store().lock().unwrap();
        ca.certificates
            .iter()
            .filter(|cert| cert.status == "active" && cert.not_after < threshold)
            .count()
    };
    if expiring > 0 {
        if let Some(rt) = runtime() {
            rt.emit_alert(
                "warning",
                "cert_expiring",
                "certificates expiring within 7 days",
                &expiring.to_string(),
            );
        }
    }
}

const HOOKS: Hooks = Hooks {
    name: "lan-ca",
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
    fn template_validates_and_exposes_allowed_suffixes() {
        let flat = parse_flat(DEFAULT_CONFIG);
        assert_eq!(get_str(&flat, "component", ""), "lan-ca");
        validate(&flat).expect("template must validate");
        let cfg = CaConfig::from_flat(&flat);
        assert_eq!(cfg.default_days, 90);
        assert_eq!(cfg.allowed_suffixes, vec![".lan", ".internal", ".local"]);
    }

    #[test]
    fn validation_rejects_bad_paths_and_day_ranges() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("ca.root_key_path".into(), "../etc/key".into());
        assert!(validate(&flat).is_err());
        flat.insert("ca.root_key_path".into(), "lan-ca.root.key".into());
        flat.insert("ca.default_days".into(), "9999".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn body_parsing_handles_string_and_number_fields() {
        let req = Request {
            method: "POST".into(),
            path: "/api/ca/issue".into(),
            query: String::new(),
            headers: Default::default(),
            body: "{\"subject\":\"nas.lan\",\"days\":30,\"sans\":[\"nas.lan\",\"192.168.1.5\"]}"
                .into(),
            peer: "127.0.0.1:1".parse().unwrap(),
        };
        assert_eq!(body_str(&req, "subject"), Some("nas.lan".into()));
        assert_eq!(body_u64(&req, "days", 90), 30);
        assert_eq!(
            ca::parse_str_array(&req.body, "sans"),
            Some(vec!["nas.lan".into(), "192.168.1.5".into()])
        );
    }
}
