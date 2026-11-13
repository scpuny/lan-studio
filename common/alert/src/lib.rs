//! Canonical alert events, platform adapters and a bounded dispatcher.
//!
//! Contract authority: `contracts/alert-event.json` + `docs/ALERT-SCHEMA.md`
//! (PRD 3.4). Components emit *canonical events only*; the five platform
//! adapters live here and nowhere else.
//!
//! Zero third-party crates. Plain `http://` webhooks are posted with the
//! standard library; `https://` webhooks (all public SaaS endpoints) are
//! bridged through the system `curl` binary. See `docs/ADR/0004`.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Bounded in-memory queue. A full queue drops the oldest event and counts it.
pub const QUEUE_CAPACITY: usize = 200;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Howard Hinnant's civil-from-days conversion, used instead of a date crate.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m as u32, d as u32)
}

/// RFC 3339 UTC timestamp, always ending in `Z` (alert-event.json `time`).
pub fn utc_rfc3339() -> String {
    let secs = unix_secs();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

pub fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn q(value: &str) -> String {
    format!("\"{}\"", json_escape(value))
}

/// Defence in depth: an event carrying a credential-looking field is replaced
/// wholesale rather than shipped to an external webhook (ALERT-SCHEMA §6).
pub fn scrub(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    for needle in [
        "password",
        "admin_token",
        "secret",
        "digest",
        "authorization",
    ] {
        if lower.contains(needle) {
            return "[redacted: event text contained a credential keyword]".to_string();
        }
    }
    text.to_string()
}

// ------------------------------------------------------------ canonical event

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertEvent {
    pub source: String,
    pub level: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub time: String,
    pub extra: Vec<(String, String)>,
    pub request_id: Option<String>,
    /// Cooldown identity (`kind + key`), intentionally NOT part of the wire
    /// contract. Callers use a stable identity such as a target alias or name.
    pub dedup_key: String,
    /// Per-kind cooldown override from `docs/ALERT-SCHEMA.md` §2. Also an
    /// internal field: it never appears in the canonical event JSON.
    pub cooldown_seconds: Option<u64>,
}

impl AlertEvent {
    /// `level` must be `info` / `warning` / `critical`; `kind` must be
    /// registered in `docs/ALERT-SCHEMA.md`.
    pub fn new(
        source: &str,
        level: &str,
        kind: &str,
        title: &str,
        detail: &str,
        dedup_key: &str,
    ) -> Self {
        Self {
            source: source.to_string(),
            level: level.to_string(),
            kind: kind.to_string(),
            title: scrub(title),
            detail: scrub(detail),
            time: utc_rfc3339(),
            extra: Vec::new(),
            request_id: None,
            dedup_key: dedup_key.to_string(),
            cooldown_seconds: None,
        }
    }

    pub fn with_extra(mut self, key: &str, value: &str) -> Self {
        self.extra.push((key.to_string(), scrub(value)));
        self
    }

    /// Override the global cooldown for kinds whose registry entry differs.
    pub fn with_cooldown(mut self, seconds: u64) -> Self {
        self.cooldown_seconds = Some(seconds);
        self
    }

    pub fn with_request_id(mut self, value: &str) -> Self {
        self.request_id = Some(value.to_string());
        self
    }

    /// Serialise to the canonical `alert-event.json` shape.
    pub fn to_json(&self) -> String {
        let mut parts = vec![
            format!("\"source\":{}", q(&self.source)),
            format!("\"level\":{}", q(&self.level)),
            format!("\"kind\":{}", q(&self.kind)),
            format!("\"title\":{}", q(&self.title)),
            format!("\"detail\":{}", q(&self.detail)),
            format!("\"time\":{}", q(&self.time)),
        ];
        if !self.extra.is_empty() {
            let fields: Vec<String> = self
                .extra
                .iter()
                .map(|(key, value)| format!("{}:{}", q(key), q(value)))
                .collect();
            parts.push(format!("\"extra\":{{{}}}", fields.join(",")));
        }
        if let Some(id) = &self.request_id {
            parts.push(format!("\"request_id\":{}", q(id)));
        }
        format!("{{{}}}", parts.join(","))
    }
}

// ------------------------------------------------------------ platform adapter

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    DingTalk,
    Feishu,
    Slack,
    WeCom,
    Generic,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DingTalk => "dingtalk",
            Self::Feishu => "feishu",
            Self::Slack => "slack",
            Self::WeCom => "wecom",
            Self::Generic => "generic",
        }
    }
}

pub fn infer_platform(url: &str) -> Platform {
    let lower = url.to_ascii_lowercase();
    if lower.contains("oapi.dingtalk.com") {
        Platform::DingTalk
    } else if lower.contains("open.feishu.cn") || lower.contains("open.larksuite.com") {
        Platform::Feishu
    } else if lower.contains("hooks.slack.com") {
        Platform::Slack
    } else if lower.contains("qyapi.weixin.qq.com") {
        Platform::WeCom
    } else {
        Platform::Generic
    }
}

/// Convert one canonical event into a platform-specific JSON body.
pub fn payload_for(platform: Platform, event: &AlertEvent) -> String {
    let heading = format!("[{}] {}", event.level.to_ascii_uppercase(), event.title);
    match platform {
        Platform::Generic => event.to_json(),
        Platform::DingTalk => format!(
            "{{\"msgtype\":\"markdown\",\"markdown\":{{\"title\":{},\"text\":{} }}}}",
            q(&event.title),
            q(&format!("### {}\n\n{}", heading, event.detail))
        ),
        Platform::WeCom => format!(
            "{{\"msgtype\":\"markdown\",\"markdown\":{{\"content\":{}}}}}",
            q(&format!("### {}\n\n{}", heading, event.detail))
        ),
        Platform::Slack => format!(
            "{{\"text\":{}}}",
            q(&format!("{}\n{}", heading, event.detail))
        ),
        Platform::Feishu => format!(
            "{{\"msg_type\":\"interactive\",\"card\":{{\"header\":{{\"title\":{{\"tag\":\"plain_text\",\"content\":{}}}}},\"elements\":[{{\"tag\":\"div\",\"text\":{{\"tag\":\"lark_md\",\"content\":{}}}}}]}}}}",
            q(&heading),
            q(&event.detail)
        ),
    }
}

// ------------------------------------------------------------------- senders

pub trait Sender: Send + Sync {
    fn post(&self, url: &str, payload: &str, timeout: Duration) -> Result<(), String>;
}

/// Standard-library HTTP/1.1 POST for plain `http://` endpoints (LAN generic
/// JSON receivers and the runtime smoke test).
pub struct HttpSender;

fn split_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "not an http:// url".to_string())?;
    let (authority, path) = match rest.split_once('/') {
        Some((head, tail)) => (head, format!("/{}", tail)),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            (head.to_string(), tail.parse::<u16>().unwrap_or(80))
        }
        _ => (authority.to_string(), 80),
    };
    if host.is_empty() || port == 0 {
        return Err("invalid webhook host".to_string());
    }
    Ok((host, port, path))
}

impl Sender for HttpSender {
    fn post(&self, url: &str, payload: &str, timeout: Duration) -> Result<(), String> {
        let (host, port, path) = split_http_url(url)?;
        let address = format!("{}:{}", host, port)
            .to_socket_addrs()
            .map_err(|err| err.to_string())?
            .next()
            .ok_or_else(|| "webhook host did not resolve".to_string())?;
        let mut stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|err| format!("connect: {}", err))?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|_| stream.set_write_timeout(Some(timeout)))
            .map_err(|err| err.to_string())?;
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            path,
            host,
            payload.len(),
            payload
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("write: {}", err))?;
        let mut buffer = [0u8; 256];
        let count = stream
            .read(&mut buffer)
            .map_err(|err| format!("read: {}", err))?;
        let head = String::from_utf8_lossy(&buffer[..count]);
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| "malformed webhook response".to_string())?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!("webhook returned HTTP {}", status))
        }
    }
}

/// TLS bridge for the public SaaS webhooks. `curl` is only needed when a
/// `https://` webhook is configured; the component itself stays single-file.
pub struct CurlSender;

impl Sender for CurlSender {
    fn post(&self, url: &str, payload: &str, timeout: Duration) -> Result<(), String> {
        if !url.starts_with("https://") {
            return Err("unsupported webhook url scheme".to_string());
        }
        let seconds = timeout.as_secs().max(1).to_string();
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                &seconds,
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                "@-",
                url,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("curl unavailable: {}", err))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("curl wait: {}", err))?;
        let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && code.starts_with('2') {
            Ok(())
        } else {
            Err(format!("webhook returned HTTP {}", code))
        }
    }
}

/// Production sender: plain HTTP in-process, TLS through the optional bridge.
pub struct DefaultSender;

impl Sender for DefaultSender {
    fn post(&self, url: &str, payload: &str, timeout: Duration) -> Result<(), String> {
        if url.starts_with("https://") {
            CurlSender.post(url, payload, timeout)
        } else if url.starts_with("http://") {
            HttpSender.post(url, payload, timeout)
        } else {
            Err("unsupported webhook url scheme".to_string())
        }
    }
}

// --------------------------------------------------------------- configuration

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertConfig {
    pub enabled: bool,
    pub webhooks: Vec<String>,
    pub cooldown_seconds: u64,
    pub timeout: Duration,
    /// Upper bound is 2 (ALERT-SCHEMA §4 / F-G04).
    pub max_retries: u32,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            webhooks: Vec::new(),
            cooldown_seconds: 300,
            timeout: Duration::from_secs(5),
            max_retries: 2,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub queued: u64,
    pub sent: u64,
    pub failed: u64,
    pub dropped: u64,
    pub suppressed: u64,
    pub retries: u64,
    /// `(kind, result)` -> count, backing `alerts_sent_total{kind,result}`.
    pub by_kind: BTreeMap<(String, String), u64>,
}

impl Stats {
    fn record(&mut self, kind: &str, success: bool) {
        let result = if success { "ok" } else { "failed" };
        *self
            .by_kind
            .entry((kind.to_string(), result.to_string()))
            .or_insert(0) += 1;
        if success {
            self.sent += 1;
        } else {
            self.failed += 1;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub enabled: bool,
    pub webhooks: usize,
    pub queued: usize,
    pub sent: u64,
    pub failed: u64,
    pub dropped: u64,
    pub suppressed: u64,
    pub retries: u64,
    pub cooldown_seconds: u64,
    pub by_kind: BTreeMap<(String, String), u64>,
}

#[derive(Default)]
struct RunState {
    stats: Stats,
    cooldown: BTreeMap<String, f64>,
}

/// One delivery attempt (including retries) for every configured webhook.
/// Kept as a pure function so cooldown/retry behaviour is unit-testable
/// without threads.
fn dispatch_once(
    event: &AlertEvent,
    config: &AlertConfig,
    sender: &dyn Sender,
    state: &mut RunState,
) -> bool {
    let now = now_secs();
    let window = event.cooldown_seconds.unwrap_or(config.cooldown_seconds) as f64;
    if window > 0.0 {
        state.cooldown.retain(|_, last| now - *last < window);
        let key = format!("{}|{}", event.kind, event.dedup_key);
        if state
            .cooldown
            .get(&key)
            .is_some_and(|last| now - *last < window)
        {
            state.stats.suppressed += 1;
            return false;
        }
        state.cooldown.insert(key, now);
        // Hard bound even when a caller invents unbounded dedup keys.
        while state.cooldown.len() > 4096 {
            let oldest = state
                .cooldown
                .iter()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    state.cooldown.remove(&key);
                }
                None => break,
            }
        }
    }

    for url in &config.webhooks {
        let payload = payload_for(infer_platform(url), event);
        let mut success = false;
        for attempt in 0..=config.max_retries.min(2) {
            if attempt > 0 {
                state.stats.retries += 1;
                thread::sleep(Duration::from_millis(50u64 << (attempt - 1).min(4)));
            }
            if sender.post(url, &payload, config.timeout).is_ok() {
                success = true;
                break;
            }
        }
        state.stats.record(&event.kind, success);
    }
    true
}

struct Inner {
    config: RwLock<AlertConfig>,
    queue: Mutex<VecDeque<AlertEvent>>,
    changed: Condvar,
    state: Mutex<RunState>,
    sender: Box<dyn Sender>,
    running: AtomicBool,
}

/// Bounded, single-worker alert dispatcher (F-G04 / F-G05).
pub struct Dispatcher {
    inner: Arc<Inner>,
}

impl Dispatcher {
    /// Start the delivery worker. `emit()` is safe from any thread.
    pub fn start(config: AlertConfig, sender: Box<dyn Sender>) -> Arc<Self> {
        let dispatcher = Arc::new(Self {
            inner: Arc::new(Inner {
                config: RwLock::new(config),
                queue: Mutex::new(VecDeque::with_capacity(QUEUE_CAPACITY)),
                changed: Condvar::new(),
                state: Mutex::new(RunState::default()),
                sender,
                running: AtomicBool::new(true),
            }),
        });
        let worker = Arc::clone(&dispatcher.inner);
        thread::spawn(move || Self::run(&worker));
        dispatcher
    }

    fn run(inner: &Arc<Inner>) {
        loop {
            let event = {
                let mut queue = inner.queue.lock().unwrap();
                loop {
                    if let Some(event) = queue.pop_front() {
                        break Some(event);
                    }
                    if !inner.running.load(Ordering::Relaxed) {
                        break None;
                    }
                    let (guard, _) = inner
                        .changed
                        .wait_timeout(queue, Duration::from_millis(200))
                        .unwrap();
                    queue = guard;
                }
            };
            let Some(event) = event else { break };
            let config = inner.config.read().unwrap().clone();
            let mut state = inner.state.lock().unwrap();
            dispatch_once(&event, &config, inner.sender.as_ref(), &mut state);
        }
    }

    /// Enqueue one canonical event. Never blocks, never grows unbounded.
    pub fn emit(&self, event: AlertEvent) {
        let deliverable = {
            let config = self.inner.config.read().unwrap();
            config.enabled && !config.webhooks.is_empty()
        };
        if !deliverable {
            let mut state = self.inner.state.lock().unwrap();
            state.stats.suppressed += 1;
            return;
        }
        let mut queue = self.inner.queue.lock().unwrap();
        if queue.len() >= QUEUE_CAPACITY {
            queue.pop_front();
            self.inner.state.lock().unwrap().stats.dropped += 1;
        }
        queue.push_back(event);
        self.inner.state.lock().unwrap().stats.queued += 1;
        drop(queue);
        self.inner.changed.notify_one();
    }

    /// F-G05: hot-swap webhook list / cooldown / retry settings, no restart.
    pub fn reconfigure(&self, config: AlertConfig) {
        *self.inner.config.write().unwrap() = config;
        self.inner.changed.notify_all();
    }

    pub fn status(&self) -> Snapshot {
        let config = self.inner.config.read().unwrap().clone();
        let state = self.inner.state.lock().unwrap();
        Snapshot {
            enabled: config.enabled,
            webhooks: config.webhooks.len(),
            queued: self.inner.queue.lock().unwrap().len(),
            sent: state.stats.sent,
            failed: state.stats.failed,
            dropped: state.stats.dropped,
            suppressed: state.stats.suppressed,
            retries: state.stats.retries,
            cooldown_seconds: config.cooldown_seconds,
            by_kind: state.stats.by_kind.clone(),
        }
    }

    pub fn shutdown(&self) {
        self.inner.running.store(false, Ordering::Relaxed);
        self.inner.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, key: &str) -> AlertEvent {
        AlertEvent::new("lan-mdns", "warning", kind, "标题", "详情", key)
            .with_extra("name", "nas.local")
    }

    #[derive(Default)]
    struct FakeSender {
        calls: Mutex<Vec<String>>,
        failures: u32,
    }

    impl Sender for FakeSender {
        fn post(&self, url: &str, payload: &str, _: Duration) -> Result<(), String> {
            let mut calls = self.calls.lock().unwrap();
            let index = calls.len() as u32;
            calls.push(format!("{}|{}", url, payload));
            if index < self.failures {
                Err("injected failure".to_string())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn canonical_event_matches_the_frozen_schema_shape() {
        let json = event("name_conflict", "nas.local").to_json();
        for field in [
            "source", "level", "kind", "title", "detail", "time", "extra",
        ] {
            assert!(json.contains(&format!("\"{}\":", field)), "{}", json);
        }
        assert!(!json.contains("dedup_key"), "internal field leaked");
        assert!(json.contains("\"time\":\"20"));
        assert!(json.contains("Z\""));
        assert_eq!(utc_rfc3339().len(), 20);
        assert!(utc_rfc3339().ends_with('Z'));
    }

    #[test]
    fn credentials_never_reach_a_webhook() {
        let leaked = AlertEvent::new(
            "lan-mdns",
            "warning",
            "config_invalid",
            "config saved",
            "admin_token=supersecret",
            "cfg",
        );
        assert!(leaked.detail.contains("[redacted"));
        let payload = payload_for(Platform::Generic, &leaked);
        assert!(!payload.contains("supersecret"));
    }

    #[test]
    fn all_five_platforms_are_recognised_and_adapted() {
        let cases = [
            ("https://oapi.dingtalk.com/robot/send", Platform::DingTalk),
            (
                "https://open.feishu.cn/open-apis/bot/v2/hook/x",
                Platform::Feishu,
            ),
            (
                "https://open.larksuite.com/open-apis/bot/v2/hook/x",
                Platform::Feishu,
            ),
            ("https://hooks.slack.com/services/T/B/X", Platform::Slack),
            (
                "https://qyapi.weixin.qq.com/cgi-bin/webhook/send",
                Platform::WeCom,
            ),
            ("http://127.0.0.1:9000/hook", Platform::Generic),
        ];
        for (url, expected) in cases {
            let platform = infer_platform(url);
            assert_eq!(platform, expected, "{}", url);
            let body = payload_for(platform, &event("name_conflict", "k"));
            assert!(!body.is_empty());
            assert!(body.starts_with('{') && body.ends_with('}'), "{}", body);
            assert!(!body.contains("supersecret"));
        }
        assert!(payload_for(Platform::DingTalk, &event("k", "k")).contains("msgtype"));
        assert!(payload_for(Platform::Feishu, &event("k", "k")).contains("interactive"));
        assert!(payload_for(Platform::Slack, &event("k", "k")).contains("\"text\""));
        assert!(payload_for(Platform::WeCom, &event("k", "k")).contains("markdown"));
        assert!(payload_for(Platform::Generic, &event("k", "k")).contains("\"kind\""));
    }

    #[test]
    fn cooldown_suppresses_a_storm() {
        let config = AlertConfig {
            webhooks: vec!["http://127.0.0.1:1/h".to_string()],
            cooldown_seconds: 300,
            max_retries: 0,
            ..AlertConfig::default()
        };
        let sender = FakeSender::default();
        let mut state = RunState::default();
        assert!(dispatch_once(
            &event("name_conflict", "nas.local"),
            &config,
            &sender,
            &mut state
        ));
        assert!(!dispatch_once(
            &event("name_conflict", "nas.local"),
            &config,
            &sender,
            &mut state
        ));
        assert_eq!(state.stats.suppressed, 1);
        assert_eq!(sender.calls.lock().unwrap().len(), 1);
        // A different identity within the same kind is still delivered.
        assert!(dispatch_once(
            &event("name_conflict", "box.local"),
            &config,
            &sender,
            &mut state
        ));
        assert_eq!(sender.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn retries_are_bounded_by_two_and_counted() {
        let config = AlertConfig {
            webhooks: vec!["http://127.0.0.1:1/h".to_string()],
            cooldown_seconds: 0,
            max_retries: 2,
            ..AlertConfig::default()
        };
        let sender = FakeSender {
            failures: 2,
            ..FakeSender::default()
        };
        let mut state = RunState::default();
        dispatch_once(
            &event("health_transition", "t"),
            &config,
            &sender,
            &mut state,
        );
        assert_eq!(sender.calls.lock().unwrap().len(), 3);
        assert_eq!(state.stats.retries, 2);
        assert_eq!(state.stats.sent, 1);
        assert_eq!(state.stats.failed, 0);
    }

    #[test]
    fn exhausted_retries_are_recorded_as_failed() {
        let config = AlertConfig {
            webhooks: vec!["http://127.0.0.1:1/h".to_string()],
            cooldown_seconds: 0,
            max_retries: 2,
            ..AlertConfig::default()
        };
        let sender = FakeSender {
            failures: 99,
            ..FakeSender::default()
        };
        let mut state = RunState::default();
        dispatch_once(
            &event("health_transition", "t"),
            &config,
            &sender,
            &mut state,
        );
        assert_eq!(state.stats.failed, 1);
        assert_eq!(
            state
                .stats
                .by_kind
                .get(&("health_transition".into(), "failed".into())),
            Some(&1)
        );
    }

    #[test]
    fn queue_is_bounded_and_hot_reconfiguration_is_visible() {
        let dispatcher = Dispatcher::start(AlertConfig::default(), Box::new(FakeSender::default()));
        // No webhooks configured: events are suppressed, never queued.
        for index in 0..(QUEUE_CAPACITY + 20) {
            dispatcher.emit(event("name_conflict", &format!("k{}", index)));
        }
        let status = dispatcher.status();
        assert_eq!(status.queued, 0);
        assert!(status.suppressed > 0);

        dispatcher.reconfigure(AlertConfig {
            webhooks: vec!["http://127.0.0.1:1/h".to_string()],
            cooldown_seconds: 0,
            max_retries: 0,
            ..AlertConfig::default()
        });
        assert_eq!(dispatcher.status().webhooks, 1);
        for index in 0..(QUEUE_CAPACITY + 20) {
            dispatcher.emit(event("name_conflict", &format!("q{}", index)));
        }
        assert!(dispatcher.status().queued <= QUEUE_CAPACITY);
        dispatcher.shutdown();
    }

    #[test]
    fn plain_http_url_parser_handles_default_port_and_path() {
        assert_eq!(
            split_http_url("http://127.0.0.1/hook").unwrap(),
            ("127.0.0.1".to_string(), 80, "/hook".to_string())
        );
        assert_eq!(
            split_http_url("http://localhost:19090/a/b?x=1").unwrap(),
            ("localhost".to_string(), 19090, "/a/b?x=1".to_string())
        );
        assert!(split_http_url("ftp://host/hook").is_err());
    }

    #[test]
    fn unsupported_schemes_are_rejected_before_any_process_spawn() {
        assert!(DefaultSender
            .post("file:///etc/passwd", "{}", Duration::from_millis(10))
            .is_err());
    }
}
