//! Shared component runtime for M3 (`lan-relay` / `lan-ca` / `lan-scan`).
//!
//! Every Lan Studio component must expose the same contract (PRD §3): health
//! probes, status, config lifecycle with snapshots, structured logs + SSE,
//! Prometheus metrics, alert state and admin auth. This crate implements that
//! contract once; a component only supplies its business hooks (`Hooks`) and
//! its own endpoints. Zero third-party crates, single self-contained binary.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_api::{err, json_quote, ok, App, Context, Request, Response, SseSpec};
use common_auth::{PasswordGuard, PasswordManager};
use common_config::{
    get_bool, get_str, get_u16, get_u32, get_u64, parse_flat, port_from_env, validate_text,
};
use common_crypto::sha256_hex;
use common_health::Health;
use common_log::{EventSource, LogBuffer, RotatingFile};
use common_metrics::Registry;

pub mod capabilities;

/// Offline console: six first-level areas, inlined CSS/JS, zero external URLs.
/// `__CAPABILITIES__` is replaced at serve time with the component manifest.
pub const CONSOLE_HTML: &str = include_str!("assets/console.html");

/// Shared capability renderer, embedded into the component console and reused
/// by the Shell SPA. Injected at serve time via `__RENDERER_JS__`.
pub const RENDERER_JS: &str = include_str!("assets/console-renderer.js");

pub type Flat = HashMap<String, String>;

/// Business hooks supplied by each component. Function pointers keep the
/// runtime free of generics so the standard handlers can be plain `fn`s
/// compatible with `common_api::Handler`.
#[derive(Clone, Copy)]
pub struct Hooks {
    pub name: &'static str,
    pub version: &'static str,
    /// Embedded config template, written once on first boot, never overwritten.
    pub default_config: &'static str,
    /// Component-specific offline validation (called after the common schema).
    pub validate: fn(&Flat) -> Result<(), String>,
    /// Hot reload after a successful save / import / rollback / manual reload.
    pub reload: fn(&Flat) -> Result<(), String>,
    /// Extra `/api/status` fields, as a complete JSON object.
    pub status: fn() -> String,
    /// Refresh component-specific metrics before exposition.
    pub refresh_metrics: fn(&Registry),
    /// Register component-specific routes.
    pub routes: fn(App) -> App,
    /// Start the business loop (called after the runtime is installed globally).
    pub start: fn(),
}

pub fn noop_validate(_: &Flat) -> Result<(), String> {
    Ok(())
}
pub fn noop_reload(_: &Flat) -> Result<(), String> {
    Ok(())
}
pub fn noop_status() -> String {
    "{}".to_string()
}
pub fn noop_metrics(_: &Registry) {}
pub fn noop_routes(app: App) -> App {
    app
}
pub fn noop_start() {}

static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

/// Access the installed runtime from business code and component handlers.
pub fn runtime() -> Option<&'static Arc<Runtime>> {
    RUNTIME.get()
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: String,
    pub schema: u32,
    pub component: String,
    pub created_at: String,
    pub reason: String,
    pub config_text: String,
    pub checksum: String,
}

impl Snapshot {
    pub fn new(id: String, component: &str, reason: &str, text: &str) -> Self {
        Self {
            id,
            schema: 1,
            component: component.to_string(),
            created_at: common_alert::utc_rfc3339(),
            reason: reason.to_string(),
            config_text: text.to_string(),
            checksum: format!("sha256:{}", sha256_hex(text.as_bytes())),
        }
    }

    /// Canonical contract shape (contracts/snapshot.json). `id` is intentionally
    /// omitted: it is an internal version identifier, not part of the schema.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{},\"component\":{},\"created_at\":{},\"reason\":{},\"config_text\":{},\"checksum\":{}}}",
            self.schema,
            json_quote(&self.component),
            json_quote(&self.created_at),
            json_quote(&self.reason),
            json_quote(&self.config_text),
            json_quote(&self.checksum),
        )
    }

    fn to_line(&self) -> String {
        format!(
            "{{\"id\":{},{}}}",
            json_quote(&self.id),
            &self.to_json()[1..self.to_json().len() - 1]
        )
    }

    fn from_line(line: &str) -> Option<Self> {
        Some(Self {
            id: json_str(line, "id")?,
            schema: 1,
            component: json_str(line, "component")?,
            created_at: json_str(line, "created_at")?,
            reason: json_str(line, "reason")?,
            config_text: json_str(line, "config_text")?,
            checksum: json_str(line, "checksum")?,
        })
    }

    /// Import validation: checksum is mandatory and must match the text
    /// (contracts/snapshot.json). Credentials are never part of a snapshot.
    pub fn from_json(body: &str) -> Result<Self, String> {
        let text = json_str(body, "config_text").ok_or("config_text is required")?;
        let checksum = json_str(body, "checksum").ok_or("checksum is required")?;
        let expected = format!("sha256:{}", sha256_hex(text.as_bytes()));
        if checksum != expected {
            return Err("snapshot checksum mismatch".to_string());
        }
        let component = json_str(body, "component").unwrap_or_default();
        if !component.is_empty() && !component.starts_with("lan-") {
            return Err("snapshot component is invalid".to_string());
        }
        Ok(Self {
            id: String::new(),
            schema: 1,
            component,
            created_at: json_str(body, "created_at").unwrap_or_default(),
            reason: json_str(body, "reason").unwrap_or_else(|| "import".to_string()),
            config_text: text,
            checksum,
        })
    }
}

#[derive(Default)]
struct AlertState {
    sent: u64,
    failed: u64,
    dropped: u64,
    suppressed: u64,
    by_kind: BTreeMap<(String, String), u64>,
    recent: VecDeque<String>,
}

struct State {
    text: String,
    saves: u64,
    reloads: u64,
    history: Vec<Snapshot>,
    alerts: AlertState,
}

pub struct Runtime {
    pub hooks: Hooks,
    pub path: String,
    pub history_path: String,
    pub logs: Arc<LogBuffer>,
    pub metrics: Registry,
    pub health: Arc<Health>,
    pub password: Arc<PasswordManager>,
    pub admin_token: String,
    pub webui: bool,
    pub started: Instant,
    history_limit: usize,
    state: RwLock<State>,
    seq: AtomicU64,
}

impl Runtime {
    fn new(hooks: Hooks, path: String, text: String, flat: &Flat) -> Self {
        let history_path = format!("{}.history", hooks.name);
        let mut history: Vec<Snapshot> = fs::read_to_string(&history_path)
            .map(|body| body.lines().filter_map(Snapshot::from_line).collect())
            .unwrap_or_default();
        let history_limit = get_u32(flat, "observability.history_limit", 30) as usize;
        history.truncate(history_limit);

        let disk = RotatingFile::new(
            ".",
            &format!("{}.log", hooks.name),
            get_u64(flat, "logging.max_bytes", 1_048_576),
            get_u32(flat, "logging.backup_count", 3),
        )
        .ok();
        let logs = Arc::new(LogBuffer::new(2000, disk));
        let metrics = Registry::new(hooks.name);
        metrics.set_build_info(hooks.version, "dev", std::env::consts::OS);
        let password_path = format!("{}.password", hooks.name);
        let password = PasswordManager::load(password_path.as_str())
            .or_else(|_| PasswordManager::load_with_iterations(password_path.as_str(), 1_000))
            .unwrap_or_else(|error| panic!("password store unavailable: {}", error));

        Self {
            hooks,
            path,
            history_path,
            logs,
            metrics,
            health: Arc::new(Health::new()),
            password,
            admin_token: get_str(flat, "server.admin_token", ""),
            webui: get_bool(flat, "server.webui", true),
            started: Instant::now(),
            history_limit,
            state: RwLock::new(State {
                text,
                saves: 0,
                reloads: 0,
                history,
                alerts: AlertState::default(),
            }),
            seq: AtomicU64::new(1),
        }
    }

    fn context(&self) -> Context {
        let events: Arc<dyn EventSource> = self.logs.clone();
        let guard: Arc<dyn PasswordGuard> = self.password.clone();
        Context {
            component: self.hooks.name.to_string(),
            version: self.hooks.version.to_string(),
            started: self.started,
            metrics: self.metrics.clone(),
            health: Arc::clone(&self.health),
            admin_token: self.admin_token.clone(),
            config_raw: self.config_text(),
            webui: self.webui,
            events: Some(events),
            password: Some(guard),
        }
    }

    pub fn config_text(&self) -> String {
        self.state.read().unwrap().text.clone()
    }

    pub fn flat(&self) -> Flat {
        parse_flat(&self.config_text())
    }

    pub fn emit(&self, level: &str, msg: &str) {
        self.logs.emit(level, self.hooks.name, msg);
    }

    /// Produce one canonical alert event (contracts/alert-event.json), record it
    /// in the bounded recent list and expose it through `/api/alerts`.
    pub fn emit_alert(&self, level: &str, kind: &str, title: &str, detail: &str) {
        let event =
            common_alert::AlertEvent::new(self.hooks.name, level, kind, title, detail, kind)
                .to_json();
        {
            let mut state = self.state.write().unwrap();
            state.alerts.sent += 1;
            *state
                .alerts
                .by_kind
                .entry((kind.to_string(), "success".to_string()))
                .or_insert(0) += 1;
            state.alerts.recent.push_front(event);
            while state.alerts.recent.len() > 50 {
                state.alerts.recent.pop_back();
            }
        }
        self.metrics.inc(
            "alerts_sent_total",
            &[("kind", kind), ("result", "success")],
            1,
        );
        self.emit(level, &format!("alert {}", kind));
    }

    fn next_id(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("s{}-{}", millis, seq)
    }

    fn make_snapshot(&self, reason: &str) -> Snapshot {
        Snapshot::new(self.next_id(), self.hooks.name, reason, &self.config_text())
    }

    fn persist_history(&self, history: &[Snapshot]) {
        let lines: Vec<String> = history.iter().map(Snapshot::to_line).collect();
        let tmp = format!("{}.tmp", self.history_path);
        if fs::write(&tmp, lines.join("\n")).is_ok() {
            let _ = fs::rename(&tmp, &self.history_path);
        }
    }

    /// PRD §3.1 rule 5: validate → snapshot → atomic replace → hot reload.
    pub fn save(&self, text: &str, reason: &str) -> Result<String, ApiError> {
        let flat = validate_document(text, self.hooks).map_err(|message| ApiError {
            code: "invalid_config",
            message,
        })?;
        let snapshot = self.make_snapshot(reason);
        let tmp = format!("{}.tmp", self.path);
        fs::write(&tmp, text).map_err(|error| ApiError {
            code: "internal_error",
            message: error.to_string(),
        })?;
        fs::rename(&tmp, &self.path).map_err(|error| ApiError {
            code: "internal_error",
            message: error.to_string(),
        })?;
        {
            let mut state = self.state.write().unwrap();
            state.text = text.to_string();
            state.saves += 1;
            state.history.insert(0, snapshot.clone());
            state.history.truncate(self.history_limit);
            self.persist_history(&state.history);
        }
        (self.hooks.reload)(&flat).map_err(|message| ApiError {
            code: "internal_error",
            message,
        })?;
        self.metrics.inc("config_saves_total", &[], 1);
        self.emit("info", &format!("config saved ({})", reason));
        Ok(snapshot.id)
    }

    pub fn reload_from_disk(&self) -> Result<(), ApiError> {
        let text = fs::read_to_string(&self.path).map_err(|error| ApiError {
            code: "internal_error",
            message: error.to_string(),
        })?;
        let flat = validate_document(&text, self.hooks).map_err(|message| ApiError {
            code: "invalid_config",
            message,
        })?;
        (self.hooks.reload)(&flat).map_err(|message| ApiError {
            code: "internal_error",
            message,
        })?;
        {
            let mut state = self.state.write().unwrap();
            state.text = text;
            state.reloads += 1;
        }
        self.metrics.inc("config_reloads_total", &[], 1);
        self.emit("info", "config reloaded");
        Ok(())
    }

    pub fn history(&self) -> Vec<Snapshot> {
        self.state.read().unwrap().history.clone()
    }

    pub fn find_snapshot(&self, id: &str) -> Option<Snapshot> {
        if !valid_version_id(id) {
            return None;
        }
        self.history().into_iter().find(|snap| snap.id == id)
    }

    pub fn counters(&self) -> (u64, u64) {
        let state = self.state.read().unwrap();
        (state.saves, state.reloads)
    }

    fn alerts_json(&self) -> String {
        let state = self.state.read().unwrap();
        let kinds: Vec<String> = state
            .alerts
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
        let recent: Vec<String> = state.alerts.recent.iter().cloned().collect();
        format!(
            "{{\"enabled\":true,\"webhooks\":0,\"queued\":0,\"queue_capacity\":200,\"sent\":{},\"failed\":{},\"dropped\":{},\"suppressed\":{},\"retries\":0,\"cooldown_seconds\":300,\"by_kind\":[{}],\"recent\":[{}]}}",
            state.alerts.sent,
            state.alerts.failed,
            state.alerts.dropped,
            state.alerts.suppressed,
            kinds.join(","),
            recent.join(","),
        )
    }
}

/// Version IDs are whitelisted to prevent path traversal (PRD §3.6).
pub fn valid_version_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn validate_document(text: &str, hooks: Hooks) -> Result<Flat, String> {
    let flat = validate_text(text)
        .map_err(|error| format!("{}: {} ({})", error.code, error.field, error.detail))?;
    let component = get_str(&flat, "component", "");
    if component != hooks.name {
        return Err(format!("component must be {}", hooks.name));
    }
    (hooks.validate)(&flat)?;
    Ok(flat)
}

// ---------------------------------------------------------------------------
// Small JSON / query helpers (no serde).
// ---------------------------------------------------------------------------

pub fn json_str(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let at = body.find(&needle)?;
    let rest = &body[at + needle.len()..];
    let colon = rest.find(':')?;
    parse_json_string(rest[colon + 1..].trim_start())
}

fn parse_json_string(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(out),
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    return None;
                }
                match bytes[i] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'u' => {
                        if i + 4 >= bytes.len() {
                            return None;
                        }
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 5]).ok()?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        i += 4;
                    }
                    _ => return None,
                }
            }
            _ => {
                let ch = input[i..].chars().next()?;
                out.push(ch);
                i += ch.len_utf8() - 1;
            }
        }
        i += 1;
    }
    None
}

pub fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn rt(req: &Request) -> Result<&'static Arc<Runtime>, Response> {
    runtime().ok_or_else(|| err(req, "internal_error", "runtime not ready", 503))
}

// ---------------------------------------------------------------------------
// Standard contract handlers (PRD §3.2).
// ---------------------------------------------------------------------------

fn view_index(_: &Request, ctx: &Context) -> Response {
    if !ctx.webui {
        return Response::text(404, "webui disabled".to_string());
    }
    let name = RUNTIME
        .get()
        .map(|runtime| runtime.hooks.name)
        .unwrap_or("component");
    let caps = capabilities::for_component(name).unwrap_or("null");
    Response::html(
        200,
        CONSOLE_HTML
            .replace("__CAPABILITIES__", caps)
            .replace("__RENDERER_JS__", RENDERER_JS),
    )
}

fn view_healthz(_: &Request, _: &Context) -> Response {
    Response::json(200, "{\"ok\":true}".to_string())
}

fn view_readyz(req: &Request, ctx: &Context) -> Response {
    if ctx.health.is_ready() {
        ok(req, "{\"ready\":true}")
    } else {
        err(req, "internal_error", "not ready", 503)
    }
}

fn view_status(req: &Request, ctx: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let (saves, reloads) = runtime.counters();
    let base = format!(
        "\"component\":{},\"version\":{},\"uptime_seconds\":{:.3},\"ready\":{},\"config_saves\":{},\"config_reloads\":{},\"webui\":{},\"history\":{}",
        json_quote(runtime.hooks.name),
        json_quote(runtime.hooks.version),
        ctx.started.elapsed().as_secs_f64(),
        ctx.health.is_ready(),
        saves,
        reloads,
        ctx.webui,
        runtime.history().len(),
    );
    let extra = (runtime.hooks.status)();
    let body = if extra.len() > 2 && extra.starts_with('{') && extra.ends_with('}') {
        format!("{{{},{}}}", base, &extra[1..extra.len() - 1])
    } else {
        format!("{{{}}}", base)
    };
    ok(req, &body)
}

fn view_metrics(req: &Request, ctx: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    ctx.metrics
        .set("uptime_seconds", &[], ctx.started.elapsed().as_secs_f64());
    ctx.metrics.set("up", &[], 1.0);
    ctx.metrics
        .set("log_disk_bytes", &[], runtime.logs.disk_bytes() as f64);
    ctx.metrics.set(
        "log_disk_limit_bytes",
        &[],
        runtime.logs.disk_limit_bytes() as f64,
    );
    (runtime.hooks.refresh_metrics)(&ctx.metrics);
    Response::body(
        200,
        "text/plain; version=0.0.4; charset=utf-8",
        ctx.metrics.render(),
    )
}

fn view_config(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let raw = runtime.config_text();
    let mut entries: Vec<String> = parse_flat(&raw)
        .iter()
        .map(|(key, value)| format!("{}:{}", json_quote(key), json_quote(value)))
        .collect();
    entries.sort();
    ok(
        req,
        &format!(
            "{{\"raw\":{},\"structured\":{{{}}}}}",
            json_quote(&raw),
            entries.join(",")
        ),
    )
}

fn view_config_save(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let text = match json_str(&req.body, "text") {
        Some(value) => value,
        None => return err(req, "invalid_config", "text is required", 400),
    };
    match runtime.save(&text, "save") {
        Ok(id) => {
            let (saves, reloads) = runtime.counters();
            ok(
                req,
                &format!(
                    "{{\"saved\":true,\"snapshot\":{},\"config_saves\":{},\"config_reloads\":{}}}",
                    json_quote(&id),
                    saves,
                    reloads
                ),
            )
        }
        Err(error) => err(
            req,
            error.code,
            &error.message,
            if error.code == "invalid_config" {
                400
            } else {
                500
            },
        ),
    }
}

fn view_snapshot_export(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(id) = query_param(&req.query, "id") {
        return match runtime.find_snapshot(&id) {
            Some(snap) => ok(req, &snap.to_json()),
            None => err(req, "not_found", "snapshot not found", 404),
        };
    }
    let reason = query_param(&req.query, "reason").unwrap_or_else(|| "manual".to_string());
    ok(req, &runtime.make_snapshot(&reason).to_json())
}

fn view_snapshot_import(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let snap = match Snapshot::from_json(&req.body) {
        Ok(value) => value,
        Err(message) => return err(req, "invalid_config", &message, 400),
    };
    if !snap.component.is_empty() && snap.component != runtime.hooks.name {
        return err(
            req,
            "invalid_config",
            "snapshot belongs to another component",
            400,
        );
    }
    match runtime.save(&snap.config_text, "import") {
        Ok(_) => ok(req, "{\"imported\":true}"),
        Err(error) => err(req, error.code, &error.message, 400),
    }
}

fn view_history(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let entries: Vec<String> = runtime
        .history()
        .iter()
        .map(|snap| {
            format!(
                "{{\"id\":{},\"created_at\":{},\"reason\":{},\"size\":{},\"checksum\":{}}}",
                json_quote(&snap.id),
                json_quote(&snap.created_at),
                json_quote(&snap.reason),
                snap.config_text.len(),
                json_quote(&snap.checksum),
            )
        })
        .collect();
    ok(
        req,
        &format!(
            "{{\"items\":[{}],\"total\":{},\"limit\":{}}}",
            entries.join(","),
            entries.len(),
            get_u32(&runtime.flat(), "observability.history_limit", 30)
        ),
    )
}

fn view_rollback(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let id = match json_str(&req.body, "id").or_else(|| query_param(&req.query, "id")) {
        Some(value) => value,
        None => return err(req, "invalid_config", "id is required", 400),
    };
    let snap = match runtime.find_snapshot(&id) {
        Some(value) => value,
        None => return err(req, "not_found", "snapshot not found", 404),
    };
    // save() snapshots the pre-rollback config before replacing it (§3.6).
    match runtime.save(&snap.config_text, "rollback") {
        Ok(_) => ok(
            req,
            &format!("{{\"rolled_back\":true,\"id\":{}}}", json_quote(&id)),
        ),
        Err(error) => err(req, error.code, &error.message, 400),
    }
}

fn view_reload(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match runtime.reload_from_disk() {
        Ok(()) => {
            let (_, reloads) = runtime.counters();
            ok(
                req,
                &format!("{{\"reloaded\":true,\"config_reloads\":{}}}", reloads),
            )
        }
        Err(error) => err(req, error.code, &error.message, 400),
    }
}

fn view_logs(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let limit = query_param(&req.query, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let lines = runtime.logs.recent(limit);
    let total = lines.len();
    ok(
        req,
        &format!(
            "{{\"logs\":[{}],\"total\":{},\"capacity\":{},\"disk_bytes\":{},\"disk_limit_bytes\":{}}}",
            lines.join(","),
            total,
            runtime.logs.capacity(),
            runtime.logs.disk_bytes(),
            runtime.logs.disk_limit_bytes()
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
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    ok(req, &runtime.alerts_json())
}

fn view_auth(req: &Request, ctx: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    ok(
        req,
        &format!(
            "{{\"token_required\":{},\"password_set\":{},\"write_protection\":true}}",
            !ctx.admin_token.is_empty(),
            runtime.password.is_set()
        ),
    )
}

fn view_auth_password(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let new_password = match json_str(&req.body, "new_password") {
        Some(value) => value,
        None => return err(req, "invalid_config", "new_password is required", 400),
    };
    if let Err(error) = common_auth::validate_password(&new_password) {
        return err(req, "invalid_config", &error.to_string(), 400);
    }
    if runtime.password.is_set() {
        let current = json_str(&req.body, "current_password")
            .or_else(|| req.headers.get("x-admin-password").cloned())
            .unwrap_or_default();
        if !runtime.password.verify(&current) {
            return err(req, "auth_failed", "current password does not match", 403);
        }
        if let Err(error) = runtime.password.change(&current, &new_password) {
            return err(req, "auth_failed", &error.to_string(), 403);
        }
    } else if let Err(error) = runtime.password.set(&new_password) {
        return err(req, "auth_failed", &error.to_string(), 403);
    }
    runtime.emit("info", "admin password updated");
    ok(req, "{\"password_set\":true,\"changed\":true}")
}

fn view_auth_verify(req: &Request, _: &Context) -> Response {
    let runtime = match rt(req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !runtime.password.is_set() {
        return err(req, "auth_failed", "admin password not initialized", 403);
    }
    let password = json_str(&req.body, "password")
        .or_else(|| req.headers.get("x-admin-password").cloned())
        .unwrap_or_default();
    if !runtime.password.verify(&password) {
        return err(req, "auth_failed", "invalid admin password", 403);
    }
    ok(req, "{\"valid\":true}")
}

/// Register every mandatory endpoint on an `App`. Component routes are added
/// afterwards by `Hooks::routes`.
pub fn standard_routes(app: App) -> App {
    app.route("GET", "/", view_index)
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
}

fn help_text(hooks: Hooks) -> String {
    format!(
        "{} {}\n\nUsage:\n  {} [--check] [--config PATH]\n\nSubcommands:\n  --check      Offline config validation, no network sockets\n  --version    Print version\n  --help       Print this help\n\nEnvironment:\n  LAN_STUDIO_CONFIG   Config file path (default: {}.yaml)\n  LAN_STUDIO_PORT     Override server.port\n",
        hooks.name, hooks.version, hooks.name, hooks.name
    )
}

/// Entry point for every M3 component binary.
pub fn run(hooks: Hooks) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-V" || arg == "--version") {
        println!("{} {}", hooks.name, hooks.version);
        return;
    }
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{}", help_text(hooks));
        return;
    }
    let check_only = args.iter().any(|arg| arg == "--check" || arg == "check");
    let path = args
        .iter()
        .position(|arg| arg == "--config")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .or_else(|| std::env::var("LAN_STUDIO_CONFIG").ok())
        .unwrap_or_else(|| format!("{}.yaml", hooks.name));

    let text = match fs::read_to_string(&path) {
        Ok(value) => value,
        Err(_) => {
            if !check_only {
                // PRD §3.1 rule 3: generate once, never overwrite afterwards.
                let _ = fs::write(&path, hooks.default_config);
            }
            hooks.default_config.to_string()
        }
    };
    let flat = match validate_document(&text, hooks) {
        Ok(value) => value,
        Err(message) => {
            eprintln!(
                "{{\"ok\":false,\"error\":{{\"code\":\"invalid_config\",\"message\":{}}}}}",
                json_quote(&message)
            );
            std::process::exit(2);
        }
    };
    if check_only {
        println!(
            "{{\"ok\":true,\"component\":{},\"path\":{},\"offline\":true}}",
            json_quote(hooks.name),
            json_quote(&path)
        );
        return;
    }

    let listen = get_str(&flat, "server.listen", "127.0.0.1");
    let token = get_str(&flat, "server.admin_token", "");
    let non_loopback = listen
        .parse::<IpAddr>()
        .map(|addr| !addr.is_loopback())
        .unwrap_or(true);
    if non_loopback && token.is_empty() {
        eprintln!("refusing to start: non-loopback bind requires server.admin_token (F-F05)");
        std::process::exit(2);
    }
    let port = port_from_env(get_u16(&flat, "server.port", 8080));
    let addr = format!("{}:{}", listen, port);
    let runtime = Arc::new(Runtime::new(hooks, path.clone(), text, &flat));
    let _ = RUNTIME.set(Arc::clone(&runtime));
    (hooks.refresh_metrics)(&runtime.metrics);
    runtime.emit("info", "component starting");
    (hooks.start)();
    runtime.health.mark_ready();
    runtime.emit("info", &format!("listening on {}", addr));

    let ctx = Arc::new(runtime.context());
    let app = (hooks.routes)(standard_routes(App::new()));
    if let Err(error) = app.serve(&addr, ctx) {
        eprintln!("server stopped: {}", error);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_str_handles_escapes_and_utf8() {
        let body = "{\"text\":\"a\\nb\\\"c\\\\d\",\"中文\":\"值\"}";
        assert_eq!(json_str(body, "text"), Some("a\nb\"c\\d".to_string()));
        assert_eq!(json_str(body, "中文"), Some("值".to_string()));
        assert_eq!(json_str(body, "missing"), None);
    }

    #[test]
    fn snapshot_checksum_roundtrip() {
        let snap = Snapshot::new("s1".into(), "lan-relay", "before-save", "version: 1\n");
        let parsed = Snapshot::from_json(&snap.to_json()).expect("valid snapshot");
        assert_eq!(parsed.config_text, "version: 1\n");
        assert_eq!(parsed.checksum, snap.checksum);
        assert!(
            Snapshot::from_json("{\"config_text\":\"x\",\"checksum\":\"sha256:bad\"}").is_err()
        );
    }

    #[test]
    fn version_ids_are_whitelisted() {
        assert!(valid_version_id("s123-4"));
        assert!(!valid_version_id("../etc/passwd"));
        assert!(!valid_version_id("a/b"));
        assert!(!valid_version_id(""));
    }

    #[test]
    fn query_params_are_decoded() {
        assert_eq!(
            query_param("a=1&id=s123-4&x=a%20b", "id"),
            Some("s123-4".into())
        );
        assert_eq!(query_param("x=a%20b+c", "x"), Some("a b c".into()));
    }

    #[test]
    fn offline_console_has_no_external_resources() {
        for forbidden in [
            "<script src=",
            "<link rel=\"stylesheet\"",
            "src=\"http",
            "url(http",
        ] {
            assert!(
                !CONSOLE_HTML.contains(forbidden),
                "offline rule: {forbidden}"
            );
        }
        assert!(CONSOLE_HTML.contains("id=\"app\""));
        assert!(CONSOLE_HTML.contains("<style"));
    }

    #[test]
    fn standard_routes_dispatch_without_runtime() {
        let req = Request {
            method: "GET".into(),
            path: "/healthz".into(),
            query: String::new(),
            headers: HashMap::new(),
            body: String::new(),
            peer: "127.0.0.1:1".parse().unwrap(),
        };
        let health = Arc::new(Health::new());
        let ctx = Context {
            component: "lan-relay".into(),
            version: "0.1.0".into(),
            started: Instant::now(),
            metrics: Registry::new("lan-relay"),
            health,
            admin_token: String::new(),
            config_raw: String::new(),
            webui: true,
            events: None,
            password: None,
        };
        let app = standard_routes(App::new());
        let response = app.dispatch(&req, &ctx);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
    }
}
