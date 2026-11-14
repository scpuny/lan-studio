//! lan-hosts — static name mapping and distribution (M4 / PRD 4.6).
//!
//! Single source of truth for `name <-> ip` pairs. Renders a standard hosts
//! file and atomically publishes it on demand. No mDNS responder, no DNS
//! packets, no HTTP proxy, no cross-component push.

mod hosts;

use std::sync::{Arc, Mutex, OnceLock};

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{json_str, runtime, Flat, Hooks};
use common_config::{get_bool, get_str, get_u64};
use common_metrics::Registry;

use hosts::{HostsConfig, Mapping, MappingStore, RejectKind};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();

struct AppState {
    cfg: HostsConfig,
    store: MappingStore,
    conflicts: u64,
    published: u64,
    publish_failed: u64,
    render_bytes: usize,
    last_publish: Option<String>,
}

fn config_from_flat(flat: &Flat) -> HostsConfig {
    HostsConfig {
        allow_local_suffix: get_bool(flat, "hosts.allow_local_suffix", false),
        conflict_policy: get_str(flat, "hosts.conflict_policy", "reject"),
        mappings_path: get_str(flat, "hosts.mappings_path", "lan-hosts.mappings.json"),
        output_path: get_str(flat, "hosts.output_path", "lan-hosts.out"),
        max_mappings: get_u64(flat, "hosts.max_mappings", 2000) as usize,
        publish_enabled: get_bool(flat, "hosts.publish_enabled", false),
        include_header: get_bool(flat, "hosts.include_header", true),
    }
}

fn validate(flat: &Flat) -> Result<(), String> {
    config_from_flat(flat).validate()
}

fn state() -> &'static Arc<Mutex<AppState>> {
    STATE.get_or_init(|| {
        let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
        let cfg = config_from_flat(&flat);
        let store = MappingStore::open(&cfg.mappings_path, cfg.max_mappings);
        Arc::new(Mutex::new(AppState {
            cfg,
            store,
            conflicts: 0,
            published: 0,
            publish_failed: 0,
            render_bytes: 0,
            last_publish: None,
        }))
    })
}

fn reload(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    cfg.validate()?;
    let mut s = state().lock().unwrap();
    if s.store.path != cfg.mappings_path {
        s.store = MappingStore::open(&cfg.mappings_path, cfg.max_mappings);
    } else {
        s.store.max_mappings = cfg.max_mappings;
    }
    s.cfg = cfg;
    if let Some(rt) = runtime() {
        rt.emit("info", "lan-hosts configuration reloaded");
    }
    Ok(())
}

/// Complete JSON object merged into `/api/status` and returned verbatim by
/// `/api/hosts/status`.
fn status() -> String {
    let s = state().lock().unwrap();
    format!(
        "{{\"mappings\":{},\"conflicts\":{},\"published\":{},\"publish_failed\":{},\"render_bytes\":{},\"publish_enabled\":{},\"last_publish\":{}}}",
        s.store.items.len(),
        s.conflicts,
        s.published,
        s.publish_failed,
        s.render_bytes,
        s.cfg.publish_enabled,
        s.last_publish
            .as_deref()
            .map(json_quote)
            .unwrap_or_else(|| "null".to_string()),
    )
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_gauge("mappings", "Registered name mappings");
    metrics.declare_counter("conflicts_total", "Conflicts / reserved suffixes rejected");
    metrics.declare_counter("published_total", "Successful publishes");
    metrics.declare_counter("publish_failed_total", "Failed publishes");
    metrics.declare_gauge("render_bytes", "Bytes of the last render");
    let s = state().lock().unwrap();
    metrics.set("mappings", &[], s.store.items.len() as f64);
    metrics.set_counter("conflicts_total", &[], s.conflicts);
    metrics.set_counter("published_total", &[], s.published);
    metrics.set_counter("publish_failed_total", &[], s.publish_failed);
    metrics.set("render_bytes", &[], s.render_bytes as f64);
}

// ---------------------------------------------------------------------------
// Request helpers
// ---------------------------------------------------------------------------

/// Emit a warning-level alert through the installed runtime (a no-op in
/// `--check` / unit-test contexts where no runtime exists).
fn raise_alert(kind: &str, title: &str, detail: &str) {
    if let Some(rt) = runtime() {
        rt.emit_alert("warning", kind, title, detail);
    }
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name == key {
            Some(value.to_string())
        } else {
            None
        }
    })
}

fn mappings_json(items: &[Mapping]) -> String {
    let rows: Vec<String> = items.iter().map(|item| item.to_json()).collect();
    format!(
        "{{\"items\":[{}],\"total\":{}}}",
        rows.join(","),
        rows.len()
    )
}

// ---------------------------------------------------------------------------
// Endpoints (writes reuse route_write admin-password guard)
// ---------------------------------------------------------------------------

fn view_mappings(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{},\"conflicts\":{}}}",
            s.store
                .items
                .iter()
                .map(|item| item.to_json())
                .collect::<Vec<String>>()
                .join(","),
            s.store.items.len(),
            s.conflicts
        ),
    )
}

fn view_upsert(req: &Request, _: &common_api::Context) -> Response {
    let name = match json_str(&req.body, "name") {
        Some(value) => value,
        None => return err(req, "invalid_config", "name is required", 400),
    };
    let ip = match json_str(&req.body, "ip") {
        Some(value) => value,
        None => return err(req, "invalid_config", "ip is required", 400),
    };
    let mapping = Mapping {
        name: name.clone(),
        ip,
        aliases: hosts::parse_str_array(&req.body, "aliases").unwrap_or_default(),
        comment: json_str(&req.body, "comment").unwrap_or_default(),
        tags: hosts::parse_str_array(&req.body, "tags").unwrap_or_default(),
        enabled: hosts::parse_bool(&req.body, "enabled").unwrap_or(true),
        updated_at: hosts::now_rfc3339(),
    };

    let mut conflict_alert: Option<String> = None;
    let response = {
        let mut s = state().lock().unwrap();
        let cfg = s.cfg.clone();
        match s.store.upsert(&cfg, mapping) {
            Ok(total) => ok(
                req,
                &format!("{{\"name\":{},\"total\":{}}}", json_quote(&name), total),
            ),
            Err(error) => {
                if error.kind == RejectKind::Conflict {
                    s.conflicts += 1;
                    conflict_alert = Some(error.message.clone());
                }
                let (code, status) = match error.kind {
                    RejectKind::Io => ("internal_error", 500),
                    _ => ("invalid_config", 400),
                };
                err(req, code, &error.message, status)
            }
        }
    };
    if let Some(message) = conflict_alert {
        raise_alert(
            "hosts_conflict_detected",
            "mapping conflict rejected",
            &message,
        );
    }
    response
}

fn view_remove(req: &Request, _: &common_api::Context) -> Response {
    let name = match json_str(&req.body, "name") {
        Some(value) => value,
        None => return err(req, "invalid_config", "name is required", 400),
    };
    let mut s = state().lock().unwrap();
    match s.store.remove(&name) {
        Ok(removed) => ok(req, &format!("{{\"removed\":{}}}", removed)),
        Err(message) => err(req, "internal_error", &message, 500),
    }
}

fn view_import(req: &Request, _: &common_api::Context) -> Response {
    let text = match json_str(&req.body, "text") {
        Some(value) => value,
        None => return err(req, "invalid_config", "text is required", 400),
    };
    let outcome = {
        let mut s = state().lock().unwrap();
        let cfg = s.cfg.clone();
        match s.store.import(&cfg, &text) {
            Ok(result) => {
                s.conflicts += result.conflicts;
                Ok(result)
            }
            Err(message) => Err(message),
        }
    };
    let result = match outcome {
        Ok(result) => result,
        Err(message) => return err(req, "internal_error", &message, 500),
    };
    if result.conflicts > 0 {
        raise_alert(
            "hosts_conflict_detected",
            "import saw conflicting mappings",
            &format!("{} rows rejected by conflict policy", result.conflicts),
        );
    }
    ok(
        req,
        &format!(
            "{{\"imported\":{},\"skipped\":{},\"conflicts\":{}}}",
            result.imported, result.skipped, result.conflicts
        ),
    )
}

fn view_render(req: &Request, _: &common_api::Context) -> Response {
    let format = match query_value(&req.query, "format").as_deref() {
        Some("json") => "json",
        _ => "hosts",
    };
    let mut s = state().lock().unwrap();
    let text = if format == "json" {
        mappings_json(&s.store.items)
    } else {
        hosts::render(&s.store, s.cfg.include_header)
    };
    let bytes = text.len();
    let lines = if format == "json" {
        s.store.items.len()
    } else {
        text.matches('\n').count()
    };
    s.render_bytes = bytes;
    ok(
        req,
        &format!(
            "{{\"format\":{},\"text\":{},\"bytes\":{},\"lines\":{}}}",
            json_quote(format),
            json_quote(&text),
            bytes,
            lines
        ),
    )
}

fn view_publish(req: &Request, _: &common_api::Context) -> Response {
    let (cfg, text) = {
        let s = state().lock().unwrap();
        (s.cfg.clone(), hosts::render(&s.store, s.cfg.include_header))
    };
    let bytes = text.len();
    match hosts::publish(&cfg, &text) {
        Ok(()) => {
            {
                let mut s = state().lock().unwrap();
                s.published += 1;
                s.render_bytes = bytes;
                s.last_publish = Some(hosts::now_rfc3339());
            }
            ok(
                req,
                &format!(
                    "{{\"published\":true,\"bytes\":{},\"path\":{}}}",
                    bytes,
                    json_quote(&cfg.output_path)
                ),
            )
        }
        Err(message) => {
            {
                let mut s = state().lock().unwrap();
                s.publish_failed += 1;
                s.render_bytes = bytes;
            }
            raise_alert("hosts_publish_failed", "hosts publish failed", &message);
            err(req, "invalid_config", &message, 409)
        }
    }
}

fn view_hosts_status(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &status())
}

fn routes(app: App) -> App {
    app.route("GET", "/api/hosts/mappings", view_mappings)
        .route_write("POST", "/api/hosts/mapping", view_upsert)
        .route_write("POST", "/api/hosts/remove", view_remove)
        .route_write("POST", "/api/hosts/import", view_import)
        .route("GET", "/api/hosts/render", view_render)
        .route_write("POST", "/api/hosts/publish", view_publish)
        .route("GET", "/api/hosts/status", view_hosts_status)
}

fn start() {
    let _ = state();
}

const HOOKS: Hooks = Hooks {
    name: "lan-hosts",
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
        assert_eq!(get_str(&flat, "component", ""), "lan-hosts");
        validate(&flat).expect("template must validate");
        let cfg = config_from_flat(&flat);
        assert!(!cfg.allow_local_suffix);
        assert_eq!(cfg.conflict_policy, "reject");
        assert_eq!(cfg.max_mappings, 2000);
        assert!(!cfg.publish_enabled);
        assert!(cfg.include_header);
        assert_eq!(cfg.mappings_path, "lan-hosts.mappings.json");
        assert_eq!(cfg.output_path, "lan-hosts.out");
    }

    #[test]
    fn validation_rejects_bad_hosts_fields() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("hosts.conflict_policy".into(), "merge".into());
        assert!(validate(&flat).is_err());
        flat.insert("hosts.conflict_policy".into(), "upsert".into());
        flat.insert("hosts.output_path".into(), "../etc/hosts".into());
        assert!(validate(&flat).is_err());
        flat.insert("hosts.output_path".into(), "no/such/dir/hosts.out".into());
        assert!(validate(&flat).is_ok());
        flat.insert("hosts.max_mappings".into(), "0".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn query_value_reads_format_parameter() {
        assert_eq!(
            query_value("format=json", "format"),
            Some("json".to_string())
        );
        assert_eq!(
            query_value("a=1&format=hosts", "format"),
            Some("hosts".to_string())
        );
        assert_eq!(query_value("", "format"), None);
    }
}
