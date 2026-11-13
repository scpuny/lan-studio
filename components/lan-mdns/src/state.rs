//! M2.2 runtime state: business health targets (F-D03), name-conflict
//! detection (F-D02) and the TTL-aware LAN service map (F-B05).
//!
//! Health probing runs on its own thread with a fixed TCP connect timeout.
//! Conflict detection and service aggregation are pure functions over the
//! engine's bounded snapshots, so every `/api/*` read is cheap and consistent.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_alert::{AlertEvent, Dispatcher};
use common_dns::{type_name, ResourceRecord, TYPE_PTR};

use crate::config::HealthTargetDef;
use crate::engine::{rdata_text, ObservedEntry};
use crate::gateway::Gateway;
use crate::store::ConfigStore;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn target_key(status: &TargetStatus) -> String {
    format!("{}|{}|{}", status.alias, status.host, status.port)
}

// ------------------------------------------------------------- F-D03 health

#[derive(Debug, Clone)]
pub struct TargetStatus {
    pub alias: String,
    pub host: String,
    pub port: u16,
    /// `up` / `down` / `unknown`. `down` is ordinary business data, never an
    /// API error code (docs/ERROR-CODES.md §4).
    pub state: String,
    pub checked_at: f64,
    pub latency_ms: f64,
}

pub struct HealthMonitor {
    targets: RwLock<Vec<HealthTargetDef>>,
    states: RwLock<Vec<TargetStatus>>,
    running: Arc<AtomicBool>,
    /// F-G02: up->down / down->up transitions become canonical events.
    on_transition: Arc<dyn Fn(AlertEvent) + Send + Sync>,
}

impl HealthMonitor {
    pub fn start(
        targets: Vec<HealthTargetDef>,
        interval_s: u64,
        on_transition: Arc<dyn Fn(AlertEvent) + Send + Sync>,
    ) -> Arc<Self> {
        let initial: Vec<TargetStatus> = targets
            .iter()
            .map(|target| TargetStatus {
                alias: target.alias.clone(),
                host: target.host.clone(),
                port: target.port,
                state: "unknown".to_string(),
                checked_at: 0.0,
                latency_ms: 0.0,
            })
            .collect();
        let monitor = Arc::new(Self {
            targets: RwLock::new(targets),
            states: RwLock::new(initial),
            running: Arc::new(AtomicBool::new(true)),
            on_transition,
        });
        let worker = Arc::clone(&monitor);
        thread::spawn(move || worker.run(interval_s.max(1)));
        monitor
    }

    fn run(&self, interval_s: u64) {
        while self.running.load(Ordering::Relaxed) {
            self.probe_all();
            // Sleep in short slices so shutdown stays responsive.
            for _ in 0..interval_s.saturating_mul(2) {
                if !self.running.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
    }

    /// Hot-swap the target list after a config reload (F-E03).
    pub fn replace_targets(&self, targets: Vec<HealthTargetDef>) {
        *self.targets.write().unwrap() = targets;
        self.probe_all();
    }

    /// Synchronous probe, used by tests and immediately after a reload.
    pub fn probe_now(&self) {
        self.probe_all();
    }

    fn probe_all(&self) {
        let targets = self.targets.read().unwrap().clone();
        let previous: BTreeMap<String, String> = self
            .states
            .read()
            .unwrap()
            .iter()
            .map(|status| (target_key(status), status.state.clone()))
            .collect();
        let mut next = Vec::with_capacity(targets.len());
        for target in &targets {
            let address = format!("{}:{}", target.host, target.port);
            let start = Instant::now();
            let state = match address.to_socket_addrs() {
                Ok(mut addresses) => match addresses.next() {
                    Some(socket) => {
                        match TcpStream::connect_timeout(&socket, Duration::from_millis(800)) {
                            Ok(_) => "up",
                            Err(_) => "down",
                        }
                    }
                    None => "down",
                },
                Err(_) => "down",
            };
            next.push(TargetStatus {
                alias: target.alias.clone(),
                host: target.host.clone(),
                port: target.port,
                state: state.to_string(),
                checked_at: now(),
                latency_ms: start.elapsed().as_secs_f64() * 1000.0,
            });
        }
        let mut transitions = Vec::new();
        for status in &next {
            if let Some(from) = previous.get(&target_key(status)) {
                // `unknown` is the initial probe, not a business transition
                // (F-G02 requires up->down / down->up only).
                if from != &status.state && from != "unknown" && status.state != "unknown" {
                    let label = if status.alias.is_empty() {
                        format!("{}:{}", status.host, status.port)
                    } else {
                        status.alias.clone()
                    };
                    transitions.push(
                        AlertEvent::new(
                            "lan-mdns",
                            "warning",
                            "health_transition",
                            &format!("健康状态翻转：{}", label),
                            &format!("{} 从 {} 变为 {}", label, from, status.state),
                            &target_key(status),
                        )
                        .with_extra("target_alias", &status.alias)
                        .with_extra("from", from)
                        .with_extra("to", &status.state)
                        .with_cooldown(300),
                    );
                }
            }
        }
        *self.states.write().unwrap() = next;
        for event in transitions {
            (self.on_transition)(event);
        }
    }

    pub fn snapshot(&self) -> Vec<TargetStatus> {
        self.states.read().unwrap().clone()
    }

    /// `(up, down, unknown)` counts for the `health_targets{state}` gauge.
    pub fn counts(&self) -> (usize, usize, usize) {
        let states = self.states.read().unwrap();
        let mut up = 0;
        let mut down = 0;
        let mut unknown = 0;
        for status in states.iter() {
            match status.state.as_str() {
                "up" => up += 1,
                "down" => down += 1,
                _ => unknown += 1,
            }
        }
        (up, down, unknown)
    }

    pub fn shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------- F-D02 conflicts

#[derive(Debug, Clone)]
pub struct Conflict {
    pub name: String,
    pub rtype: u16,
    pub rtype_name: String,
    pub values: Vec<String>,
    pub sources: Vec<String>,
}

/// A conflict is one `(name, type)` published with at least two different
/// values. Own records are registered as source `self`; external answers come
/// from the bounded ingest ring and are dropped once their TTL has elapsed.
pub fn detect_conflicts(
    own: &[ResourceRecord],
    observed: &[ObservedEntry],
    now: f64,
    default_ttl_s: f64,
) -> Vec<Conflict> {
    let mut seen: BTreeMap<(String, u16), BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for rr in own {
        seen.entry((rr.name.to_ascii_lowercase(), rr.rtype))
            .or_default()
            .entry(rdata_text(rr))
            .or_default()
            .insert("self".to_string());
    }
    for entry in observed {
        let ttl = if entry.ttl == 0 {
            default_ttl_s
        } else {
            entry.ttl as f64
        };
        if now - entry.ts > ttl {
            continue;
        }
        seen.entry((entry.name.to_ascii_lowercase(), entry.rtype))
            .or_default()
            .entry(entry.value.clone())
            .or_default()
            .insert(entry.source.clone());
    }
    seen.into_iter()
        .filter(|(_, values)| values.len() >= 2)
        .map(|((name, rtype), values)| {
            let mut sources = BTreeSet::new();
            for group in values.values() {
                sources.extend(group.iter().cloned());
            }
            Conflict {
                name,
                rtype,
                rtype_name: type_name(rtype).to_string(),
                values: values.keys().cloned().collect(),
                sources: sources.into_iter().collect(),
            }
        })
        .collect()
}

// ------------------------------------------------------------- F-B05 browse

#[derive(Debug, Clone)]
pub struct BrowseInstance {
    pub name: String,
    pub value: String,
    pub source: String,
    pub ts: f64,
}

#[derive(Debug, Clone)]
pub struct ServiceGroup {
    pub service_type: String,
    pub instances: Vec<BrowseInstance>,
    pub count: usize,
}

fn is_service_type(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with("._tcp.local")
        || lower.ends_with("._udp.local")
        || lower == "_services._dns-sd._udp.local"
}

/// Aggregate external PTR answers into a per-service-type map with TTL aging.
pub fn aggregate_browse(
    observed: &[ObservedEntry],
    now: f64,
    default_ttl_s: f64,
) -> Vec<ServiceGroup> {
    let mut grouped: BTreeMap<String, Vec<BrowseInstance>> = BTreeMap::new();
    for entry in observed {
        let ttl = if entry.ttl == 0 {
            default_ttl_s
        } else {
            entry.ttl as f64
        };
        if now - entry.ts > ttl || entry.rtype != TYPE_PTR || !is_service_type(&entry.name) {
            continue;
        }
        let list = grouped.entry(entry.name.clone()).or_default();
        if list
            .iter()
            .any(|item| item.value == entry.value && item.source == entry.source)
        {
            continue;
        }
        list.push(BrowseInstance {
            name: entry.name.clone(),
            value: entry.value.clone(),
            source: entry.source.clone(),
            ts: entry.ts,
        });
    }
    grouped
        .into_iter()
        .map(|(service_type, instances)| {
            let count = instances.len();
            ServiceGroup {
                service_type,
                instances,
                count,
            }
        })
        .collect()
}

// ------------------------------------------------------------- shared state

/// Everything the HTTP handlers need beyond the mDNS engine itself.
pub struct State {
    pub store: ConfigStore,
    pub log_buffer: Arc<common_log::LogBuffer>,
    pub password: Arc<common_auth::PasswordManager>,
    pub health: Arc<HealthMonitor>,
    /// Aging window for query audit / observed rings (F-C06).
    pub aging_ttl_s: f64,
    /// Canonical-event dispatcher (F-G01..F-G05).
    pub alerts: Arc<Dispatcher>,
    /// `None` when the global gateway switch is off (F-H02).
    pub gateway: Option<Arc<Gateway>>,
}

impl State {
    pub fn conflicts(&self, engine: &crate::engine::Engine) -> Vec<Conflict> {
        detect_conflicts(
            &engine.records_snapshot(),
            &engine.observed_snapshot(),
            now(),
            self.aging_ttl_s,
        )
    }

    pub fn service_map(&self, engine: &crate::engine::Engine) -> Vec<ServiceGroup> {
        aggregate_browse(&engine.observed_snapshot(), now(), self.aging_ttl_s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common_dns::{RData, TYPE_A};
    use std::net::TcpListener;

    fn rr(name: &str, ip: &str) -> ResourceRecord {
        ResourceRecord::new(name, RData::A(ip.parse().unwrap()), 120, true)
    }

    fn observed(name: &str, value: &str, source: &str, ts: f64, ttl: u32) -> ObservedEntry {
        ObservedEntry {
            ts,
            source: source.to_string(),
            name: name.to_string(),
            rtype: TYPE_A,
            value: value.to_string(),
            ttl,
        }
    }

    #[test]
    fn detects_same_name_different_values() {
        let own = vec![rr("nas.local", "192.168.5.10")];
        let ext = vec![
            observed("nas.local", "192.168.5.10", "peer-a:5353", 100.0, 120),
            observed("nas.local", "192.168.5.99", "peer-b:5353", 100.0, 120),
        ];
        let conflicts = detect_conflicts(&own, &ext, 150.0, 300.0);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "nas.local");
        assert_eq!(conflicts[0].rtype, TYPE_A);
        assert_eq!(conflicts[0].values.len(), 2);
        assert!(conflicts[0].sources.contains(&"self".to_string()));
    }

    #[test]
    fn no_conflict_when_values_agree_or_ttl_expired() {
        let own = vec![rr("nas.local", "192.168.5.10")];
        let agreeing = vec![observed("nas.local", "192.168.5.10", "peer", 100.0, 120)];
        assert!(detect_conflicts(&own, &agreeing, 150.0, 300.0).is_empty());
        let expired = vec![observed("nas.local", "192.168.5.99", "peer", 100.0, 10)];
        assert!(detect_conflicts(&own, &expired, 150.0, 300.0).is_empty());
    }

    #[test]
    fn aggregates_ptr_answers_by_service_type_with_aging() {
        let entries = vec![
            ObservedEntry {
                ts: 100.0,
                source: "peer:5353".into(),
                name: "_http._tcp.local".into(),
                rtype: TYPE_PTR,
                value: "Web Server._http._tcp.local".into(),
                ttl: 120,
            },
            ObservedEntry {
                ts: 100.0,
                source: "peer:5353".into(),
                name: "_http._tcp.local".into(),
                rtype: TYPE_PTR,
                value: "Web Server._http._tcp.local".into(),
                ttl: 120,
            },
            ObservedEntry {
                ts: 100.0,
                source: "other:5353".into(),
                name: "_http._tcp.local".into(),
                rtype: TYPE_PTR,
                value: "API._http._tcp.local".into(),
                ttl: 120,
            },
            ObservedEntry {
                ts: 100.0,
                source: "peer:5353".into(),
                name: "nas.local".into(),
                rtype: TYPE_A,
                value: "192.168.5.10".into(),
                ttl: 120,
            },
        ];
        let groups = aggregate_browse(&entries, 150.0, 300.0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].service_type, "_http._tcp.local");
        assert_eq!(groups[0].count, 2, "duplicate instance must be collapsed");
        let expired = aggregate_browse(&entries, 400.0, 300.0);
        assert!(expired.is_empty());
    }

    #[test]
    fn health_monitor_reports_up_down_and_counts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = listener.local_addr().unwrap().port();
        let unused = TcpListener::bind("127.0.0.1:0").unwrap();
        let down_port = unused.local_addr().unwrap().port();
        drop(unused);

        let targets = vec![
            HealthTargetDef {
                alias: "self".into(),
                host: "127.0.0.1".into(),
                port: up_port,
            },
            HealthTargetDef {
                alias: "dead".into(),
                host: "127.0.0.1".into(),
                port: down_port,
            },
        ];
        let events: Arc<std::sync::Mutex<Vec<AlertEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let monitor = HealthMonitor::start(
            targets,
            60,
            Arc::new(move |event: AlertEvent| sink.lock().unwrap().push(event)),
        );
        monitor.probe_now();
        let states = monitor.snapshot();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].state, "up", "{:?}", states);
        assert_eq!(states[1].state, "down");
        assert_eq!(monitor.counts(), (1, 1, 0));
        assert!(states[0].checked_at > 0.0);
        // The initial unknown->up probe must NOT produce an alert (F-G02).
        assert!(events.lock().unwrap().is_empty());
        // up->down is a real transition and must produce a canonical event.
        drop(listener);
        monitor.probe_now();
        let recorded = events.lock().unwrap();
        assert_eq!(recorded.len(), 1, "{:?}", recorded);
        assert_eq!(recorded[0].kind, "health_transition");
        assert_eq!(recorded[0].level, "warning");
        assert!(recorded[0].time.ends_with('Z'));
        drop(recorded);
        monitor.shutdown();
    }
}
