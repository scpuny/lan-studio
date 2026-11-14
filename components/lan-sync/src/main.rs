//! lan-sync — multi-node config synchronisation (M5, PRD §4.8).
//!
//! Composition root: config validation, global state, metrics, the eight
//! `/api/sync/*` endpoints and the `Hooks` wiring. All business primitives
//! (hand-written JSON, checksum, conflict decision, HMAC, the loopback HTTP
//! client) live in `sync.rs`; the shared `common_component` runtime owns the
//! standard endpoints and the unified API envelope.
//!
//! Hard boundary: **never silently overwrite**. A pull is always an explicit,
//! audited decision (`accepted|rejected|pending|overwritten`); only the
//! operator can resolve a `manual` decision via `/api/sync/confirm`. Credentials
//! (node token, shared_secret, admin password) never reach logs, metrics,
//! snapshots, status, history or error responses.

mod sync;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use common_api::{err, json_quote, ok, App, Request, Response};
use common_component::{json_str, runtime, Flat, Hooks};
use common_config::{get_str, get_u64};
use common_metrics::Registry;

use sync::{
    decide, http_get_signed, http_post_signed, json_bool_opt, now_rfc3339, valid_endpoint,
    AuditEntry, AuditLog, Decision, Node, NodeStore, SnapshotLite, Strategy, SyncConfig,
    STANDARD_COMPONENTS,
};

const DEFAULT_CONFIG: &str = include_str!("../config.template.yaml");

static STATE: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();

/// A `manual`-strategy pull waiting for `/api/sync/confirm`. Its `id` is the
/// full snapshot checksum returned by the pull response, so no extra field is
/// needed on the wire.
struct PendingItem {
    id: String,
    node_id: String,
    component: String,
    created_at: String,
    checksum: String,
}

struct AppState {
    cfg: SyncConfig,
    nodes: NodeStore,
    audit: AuditLog,
    /// component -> created_at of the last accepted/overwritten baseline.
    local: HashMap<String, String>,
    pending: Vec<PendingItem>,
    pulls: u64,
    pushes: u64,
    /// Conflict (rejected/pending) decisions, keyed by the fixed strategy enum.
    conflicts_by_strategy: HashMap<&'static str, u64>,
    shared_secret_configured: bool,
}

fn list_strings(flat: &Flat, prefix: &str) -> Vec<String> {
    let mut items: Vec<(usize, String)> = flat
        .iter()
        .filter_map(|(key, value)| {
            let inner = key
                .strip_prefix(prefix)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            let index: usize = inner.parse().ok()?;
            Some((index, value.clone()))
        })
        .collect();
    items.sort_by_key(|(index, _)| *index);
    items
        .into_iter()
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .collect()
}

fn config_from_flat(flat: &Flat) -> SyncConfig {
    let strategy =
        Strategy::parse(&get_str(flat, "sync.strategy", "reject")).unwrap_or(Strategy::Reject);
    let parsed = list_strings(flat, "sync.allowed_components");
    let allowed_components = if parsed.is_empty() {
        // Array absent / unparseable => allow only the 8 standard components.
        STANDARD_COMPONENTS
            .iter()
            .map(|name| String::from(*name))
            .collect()
    } else {
        parsed
    };
    SyncConfig {
        nodes_path: get_str(flat, "sync.nodes_path", "lan-sync.nodes.json"),
        history_path: get_str(flat, "sync.history_path", "lan-sync.history.json"),
        max_nodes: get_u64(flat, "sync.max_nodes", 64) as usize,
        strategy,
        request_timeout_ms: get_u64(flat, "sync.request_timeout_ms", 2000),
        shared_secret: get_str(flat, "sync.shared_secret", ""),
        history_limit: get_u64(flat, "sync.history_limit", 200) as usize,
        allowed_components,
    }
}

fn validate(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    if Strategy::parse(&get_str(flat, "sync.strategy", "reject")).is_none() {
        return Err("sync.strategy must be reject|last_write_wins|manual".to_string());
    }
    if !(100..=30_000).contains(&cfg.request_timeout_ms) {
        return Err("sync.request_timeout_ms must be 100..30000".to_string());
    }
    if !(1..=4_096).contains(&cfg.max_nodes) {
        return Err("sync.max_nodes must be 1..4096".to_string());
    }
    if !(1..=10_000).contains(&cfg.history_limit) {
        return Err("sync.history_limit must be 1..10000".to_string());
    }
    for path in [&cfg.nodes_path, &cfg.history_path] {
        if path.contains("..") {
            return Err(format!("{} must not contain '..'", path));
        }
    }
    // Any config-declared endpoint must also be an http:// loopback address.
    for (key, value) in flat.iter() {
        if key.starts_with("sync.")
            && key.contains("endpoint")
            && !value.is_empty()
            && !valid_endpoint(value)
        {
            return Err(format!("{} must be an http:// loopback endpoint", key));
        }
    }
    for name in &cfg.allowed_components {
        if !STANDARD_COMPONENTS.contains(&name.as_str()) {
            return Err(format!(
                "sync.allowed_components contains unknown component {}",
                name
            ));
        }
    }
    Ok(())
}

fn state() -> &'static Arc<Mutex<AppState>> {
    STATE.get_or_init(|| {
        let flat = runtime().map(|rt| rt.flat()).unwrap_or_default();
        let cfg = config_from_flat(&flat);
        let nodes = NodeStore::open(&cfg.nodes_path, cfg.max_nodes);
        let audit = AuditLog::open(&cfg.history_path, cfg.history_limit);
        Arc::new(Mutex::new(AppState {
            shared_secret_configured: !cfg.shared_secret.is_empty(),
            cfg,
            nodes,
            audit,
            local: HashMap::new(),
            pending: Vec::new(),
            pulls: 0,
            pushes: 0,
            conflicts_by_strategy: HashMap::new(),
        }))
    })
}

fn reload(flat: &Flat) -> Result<(), String> {
    let cfg = config_from_flat(flat);
    {
        let mut s = state().lock().unwrap();
        s.audit.set_limit(cfg.history_limit);
        s.nodes.set_limit(cfg.max_nodes);
        s.shared_secret_configured = !cfg.shared_secret.is_empty();
        s.cfg = cfg;
    }
    if let Some(rt) = runtime() {
        rt.emit("info", "lan-sync configuration reloaded");
    }
    Ok(())
}

/// Extra `/api/status` fields as a complete JSON object. The shared_secret is
/// never printed — only a boolean saying whether one is configured.
fn status() -> String {
    let s = state().lock().unwrap();
    let conflicts: u64 = s.conflicts_by_strategy.values().sum();
    format!(
        "{{\"nodes\":{},\"strategy\":{},\"pulls\":{},\"pushes\":{},\"conflicts\":{},\"audit\":{},\"pending\":{},\"shared_secret_configured\":{}}}",
        s.nodes.len(),
        json_quote(s.cfg.strategy.as_str()),
        s.pulls,
        s.pushes,
        conflicts,
        s.audit.len(),
        s.pending.len(),
        s.shared_secret_configured,
    )
}

fn refresh_metrics(metrics: &Registry) {
    metrics.declare_gauge("nodes", "Registered sync nodes");
    metrics.declare_counter("pull_total", "Sync pull requests");
    metrics.declare_counter("push_total", "Sync push requests");
    metrics.declare_counter("conflicts_total", "Conflict decisions by strategy");
    metrics.declare_gauge("audit_entries", "Retained sync audit rows");
    let s = state().lock().unwrap();
    metrics.set("nodes", &[], s.nodes.len() as f64);
    metrics.set_counter("pull_total", &[], s.pulls);
    metrics.set_counter("push_total", &[], s.pushes);
    for strategy in ["reject", "last_write_wins", "manual"] {
        let value = s.conflicts_by_strategy.get(strategy).copied().unwrap_or(0);
        metrics.set_counter("conflicts_total", &[("strategy", strategy)], value);
    }
    metrics.set("audit_entries", &[], s.audit.len() as f64);
}

fn bump_conflict(counts: &mut HashMap<&'static str, u64>, strategy: Strategy) {
    *counts.entry(strategy.as_str()).or_insert(0) += 1;
}

fn emit_conflict_alert(reason: &str) {
    if let Some(rt) = runtime() {
        rt.emit_alert("warning", "sync_conflict", "sync conflict decision", reason);
    }
}

fn emit_unreachable_alert(node_id: &str) {
    if let Some(rt) = runtime() {
        rt.emit_alert(
            "warning",
            "sync_node_unreachable",
            "sync node unreachable",
            node_id,
        );
    }
}

// ---------------------------------------------------------------------------
// Endpoints.
// ---------------------------------------------------------------------------

/// GET /api/sync/nodes — never returns the token field.
fn view_nodes(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    let items: Vec<String> = s.nodes.nodes.iter().map(Node::public_json).collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{}}}",
            items.join(","),
            items.len()
        ),
    )
}

/// POST /api/sync/node — register / replace a node.
fn view_upsert(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "id is required", 400),
    };
    let endpoint = match json_str(&req.body, "endpoint") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "endpoint is required", 400),
    };
    if !valid_endpoint(&endpoint) {
        return err(
            req,
            "invalid_config",
            "endpoint must be an http:// loopback address",
            400,
        );
    }
    let enabled = json_bool_opt(&req.body, "enabled").unwrap_or(true);
    let note = json_str(&req.body, "note").unwrap_or_default();
    let registered_at = now_rfc3339();
    let mut s = state().lock().unwrap();
    // A body without `token` preserves any previously stored credential.
    let token = match json_str(&req.body, "token") {
        Some(value) => value,
        None => s
            .nodes
            .get(&id)
            .map(|node| node.token.clone())
            .unwrap_or_default(),
    };
    let node = Node {
        id: id.clone(),
        endpoint,
        token,
        enabled,
        note,
        registered_at,
    };
    match s.nodes.upsert(node) {
        Ok(total) => ok(
            req,
            &format!("{{\"id\":{},\"total\":{}}}", json_quote(&id), total),
        ),
        Err(message) => err(req, "invalid_config", &message, 400),
    }
}

/// POST /api/sync/remove
fn view_remove(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "id is required", 400),
    };
    let mut s = state().lock().unwrap();
    let removed = s.nodes.remove(&id);
    ok(req, &format!("{{\"removed\":{}}}", removed))
}

/// POST /api/sync/pull — fetch the peer's standard snapshot, decide, audit.
/// All socket work happens with no lock held (node + cfg are cloned first).
fn view_pull(req: &Request, _: &common_api::Context) -> Response {
    let node_id = match json_str(&req.body, "node_id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "node_id is required", 400),
    };
    let component = json_str(&req.body, "component")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "lan-sync".to_string());

    // Phase 1: pure state read; clone everything the socket call needs.
    let (node, cfg, local_created_at) = {
        let mut s = state().lock().unwrap();
        let node = match s.nodes.get(&node_id) {
            Some(value) => value.clone(),
            None => return err(req, "not_found", "node is not registered", 404),
        };
        s.pulls += 1;
        (node, s.cfg.clone(), s.local.get(&component).cloned())
    };

    if !cfg.allowed_components.iter().any(|name| name == &component) {
        return err(
            req,
            "invalid_config",
            "component is not in sync.allowed_components",
            400,
        );
    }
    if !node.enabled {
        return err(req, "invalid_config", "node is disabled", 400);
    }

    // Phase 2: one standard-contract GET, no lock held.
    let base = node.endpoint.trim_end_matches('/');
    let url = format!("{}/api/config/snapshot?reason=sync", base);
    let body_text = match http_get_signed(
        &url,
        cfg.request_timeout_ms,
        &node.token,
        &cfg.shared_secret,
    ) {
        Ok(text) => text,
        Err(message) => {
            emit_unreachable_alert(&node_id);
            return err(req, "target_unreachable", &message, 502);
        }
    };

    let snapshot = match SnapshotLite::from_json(&body_text) {
        Some(value) => value,
        None => {
            let mut s = state().lock().unwrap();
            s.audit.push(AuditEntry::new(
                "pull",
                &node_id,
                &component,
                "",
                "rejected",
                "malformed_snapshot",
            ));
            return err(req, "invalid_config", "remote snapshot is malformed", 400);
        }
    };

    let (decision, reason) = decide(local_created_at.as_deref(), &snapshot, cfg.strategy);

    // Bad checksum is the one decision that is also an HTTP error (400).
    if reason == "bad_checksum" {
        {
            let mut s = state().lock().unwrap();
            bump_conflict(&mut s.conflicts_by_strategy, cfg.strategy);
            s.audit.push(AuditEntry::new(
                "pull",
                &node_id,
                &component,
                &snapshot.checksum,
                decision.as_str(),
                reason,
            ));
        }
        return err(req, "invalid_config", "snapshot checksum mismatch", 400);
    }

    // Phase 3: commit the decision under the lock; never silently overwrite.
    let checksum = snapshot.checksum.clone();
    {
        let mut s = state().lock().unwrap();
        match decision {
            Decision::Accepted | Decision::Overwritten => {
                s.local
                    .insert(component.clone(), snapshot.created_at.clone());
            }
            Decision::Pending => {
                bump_conflict(&mut s.conflicts_by_strategy, cfg.strategy);
                s.pending.push(PendingItem {
                    id: checksum.clone(),
                    node_id: node_id.clone(),
                    component: component.clone(),
                    created_at: snapshot.created_at.clone(),
                    checksum: checksum.clone(),
                });
            }
            Decision::Rejected => bump_conflict(&mut s.conflicts_by_strategy, cfg.strategy),
        }
        s.audit.push(AuditEntry::new(
            "pull",
            &node_id,
            &component,
            &checksum,
            decision.as_str(),
            reason,
        ));
    }

    if decision == Decision::Rejected || decision == Decision::Pending {
        emit_conflict_alert(reason);
    }

    ok(
        req,
        &format!(
            "{{\"decision\":{},\"reason\":{},\"node_id\":{},\"component\":{},\"checksum\":{}}}",
            json_quote(decision.as_str()),
            json_quote(reason),
            json_quote(&node_id),
            json_quote(&component),
            json_quote(&checksum),
        ),
    )
}

/// POST /api/sync/push — export the local snapshot and POST it to the peer's
/// standard import endpoint.
fn view_push(req: &Request, _: &common_api::Context) -> Response {
    let node_id = match json_str(&req.body, "node_id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "node_id is required", 400),
    };
    let (node, cfg) = {
        let mut s = state().lock().unwrap();
        let node = match s.nodes.get(&node_id) {
            Some(value) => value.clone(),
            None => return err(req, "not_found", "node is not registered", 404),
        };
        s.pushes += 1;
        (node, s.cfg.clone())
    };
    if !node.enabled {
        return err(req, "invalid_config", "node is disabled", 400);
    }

    let text = runtime().map(|rt| rt.config_text()).unwrap_or_default();
    let snapshot = SnapshotLite::from_config("lan-sync", "sync", &text);
    let body = snapshot.to_json();
    let checksum = snapshot.checksum.clone();

    let base = node.endpoint.trim_end_matches('/');
    let url = format!("{}/api/config/snapshot", base);
    match http_post_signed(
        &url,
        &body,
        cfg.request_timeout_ms,
        &node.token,
        &cfg.shared_secret,
    ) {
        Ok(_) => {
            let mut s = state().lock().unwrap();
            s.audit.push(AuditEntry::new(
                "push",
                &node_id,
                "lan-sync",
                &checksum,
                "accepted",
                "push_delivered",
            ));
            ok(
                req,
                &format!(
                    "{{\"pushed\":true,\"checksum\":{},\"node_id\":{}}}",
                    json_quote(&checksum),
                    json_quote(&node_id),
                ),
            )
        }
        Err(message) => {
            emit_unreachable_alert(&node_id);
            let mut s = state().lock().unwrap();
            s.audit.push(AuditEntry::new(
                "push",
                &node_id,
                "lan-sync",
                &checksum,
                "rejected",
                "unreachable",
            ));
            err(req, "target_unreachable", &message, 502)
        }
    }
}

/// POST /api/sync/confirm — resolve a `manual`-strategy pending item.
fn view_confirm(req: &Request, _: &common_api::Context) -> Response {
    let id = match json_str(&req.body, "id") {
        Some(value) if !value.is_empty() => value,
        _ => return err(req, "invalid_config", "id is required", 400),
    };
    let accept = json_bool_opt(&req.body, "accept").unwrap_or(false);
    let mut s = state().lock().unwrap();
    // `id` is the checksum from the pull response; also accept the 16-char
    // prefix shown in the bounded audit log.
    let position = s
        .pending
        .iter()
        .position(|item| item.id == id || item.id.starts_with(id.as_str()));
    let item = match position {
        Some(index) => s.pending.remove(index),
        None => return err(req, "not_found", "no pending item with that id", 404),
    };
    let (decision, applied) = if accept {
        // Accepting records the snapshot as the new local baseline; it does NOT
        // rewrite the config file (no silent overwrite).
        s.local
            .insert(item.component.clone(), item.created_at.clone());
        ("accepted", true)
    } else {
        ("rejected", false)
    };
    let reason = if accept { "confirmed" } else { "discarded" };
    s.audit.push(AuditEntry::new(
        "confirm",
        &item.node_id,
        &item.component,
        &item.checksum,
        decision,
        reason,
    ));
    ok(
        req,
        &format!(
            "{{\"decision\":{},\"applied\":{}}}",
            json_quote(decision),
            applied
        ),
    )
}

fn view_status(req: &Request, _: &common_api::Context) -> Response {
    ok(req, &status())
}

fn view_history(req: &Request, _: &common_api::Context) -> Response {
    let s = state().lock().unwrap();
    ok(
        req,
        &format!(
            "{{\"items\":{},\"total\":{},\"limit\":{}}}",
            s.audit.items_json(),
            s.audit.len(),
            s.audit.limit
        ),
    )
}

fn routes(app: App) -> App {
    app.route("GET", "/api/sync/nodes", view_nodes)
        .route_write("POST", "/api/sync/node", view_upsert)
        .route_write("POST", "/api/sync/remove", view_remove)
        .route_write("POST", "/api/sync/pull", view_pull)
        .route_write("POST", "/api/sync/push", view_push)
        .route_write("POST", "/api/sync/confirm", view_confirm)
        .route("GET", "/api/sync/status", view_status)
        .route("GET", "/api/sync/history", view_history)
}

fn start() {
    let _ = state();
}

const HOOKS: Hooks = Hooks {
    name: "lan-sync",
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
// Unit tests (no network, no repository files touched).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use common_config::parse_flat;

    #[test]
    fn template_validates_with_defaults() {
        let flat = parse_flat(DEFAULT_CONFIG);
        assert_eq!(get_str(&flat, "component", ""), "lan-sync");
        validate(&flat).expect("template must validate");
        let cfg = config_from_flat(&flat);
        assert_eq!(cfg.strategy, Strategy::Reject);
        assert_eq!(cfg.max_nodes, 64);
        assert_eq!(cfg.request_timeout_ms, 2000);
        assert_eq!(cfg.history_limit, 200);
        assert_eq!(cfg.allowed_components.len(), 8);
        assert!(cfg.shared_secret.is_empty());
    }

    #[test]
    fn validation_rejects_bad_strategy() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("sync.strategy".into(), "overwrite_always".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn validation_rejects_path_traversal() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("sync.nodes_path".into(), "../evil.json".into());
        assert!(validate(&flat).is_err());
        flat.insert("sync.nodes_path".into(), "lan-sync.nodes.json".into());
        flat.insert("sync.history_path".into(), "/etc/../passwd".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn validation_rejects_unknown_allowed_component() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("sync.allowed_components[0]".into(), "lan-evil".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn validation_rejects_bad_ranges() {
        let mut flat = parse_flat(DEFAULT_CONFIG);
        flat.insert("sync.request_timeout_ms".into(), "50".into());
        assert!(validate(&flat).is_err());
        flat.insert("sync.request_timeout_ms".into(), "2000".into());
        flat.insert("sync.max_nodes".into(), "9999".into());
        assert!(validate(&flat).is_err());
        flat.insert("sync.max_nodes".into(), "64".into());
        flat.insert("sync.history_limit".into(), "99999".into());
        assert!(validate(&flat).is_err());
    }

    #[test]
    fn list_strings_reads_template_array() {
        let flat = parse_flat(DEFAULT_CONFIG);
        let items = list_strings(&flat, "sync.allowed_components");
        assert_eq!(items.len(), 8);
        assert_eq!(items[0], "lan-mdns");
        assert_eq!(items[7], "lan-ssdp");
    }

    #[test]
    fn json_bool_opt_reads_request_bools() {
        assert_eq!(json_bool_opt("{\"enabled\":true}", "enabled"), Some(true));
        assert_eq!(
            json_bool_opt("{ \"accept\" : false }", "accept"),
            Some(false)
        );
        assert_eq!(json_bool_opt("{}", "enabled"), None);
    }

    #[test]
    fn hooks_identity_is_stable() {
        assert_eq!(HOOKS.name, "lan-sync");
        assert!(HOOKS.default_config.contains("component: lan-sync"));
    }
}
