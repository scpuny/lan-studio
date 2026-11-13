//! mDNS responder engine (RFC 6762): multicast receive loop, query answering,
//! startup/periodic announcements, Goodbye, inbound response ingestion and
//! active probing.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_dns::{
    question_matches, suppressed, type_name, Message, Question, RData, ResourceRecord,
};
use common_net::{group_addr, local_ipv4_addrs, MdnsSocket};

use crate::config::MdnsConfig;

const MAX_BUFFER: usize = 9000;
const MAX_OBSERVED: usize = 512;
const MAX_QUERIES: usize = 512;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn rdata_text(rr: &ResourceRecord) -> String {
    match &rr.rdata {
        RData::A(ip) => ip.to_string(),
        RData::Aaaa(ip) => ip.to_string(),
        RData::Ptr(name) => name.clone(),
        RData::Srv { port, target, .. } => format!("{}:{}", target, port),
        RData::Txt(parts) => parts.join(";"),
        RData::Raw(_, bytes) => format!("raw:{}bytes", bytes.len()),
    }
}

#[derive(Debug, Clone)]
pub struct QueryEntry {
    pub ts: f64,
    pub source: String,
    pub name: String,
    pub qtype: String,
}

#[derive(Debug, Clone)]
pub struct ObservedEntry {
    pub ts: f64,
    pub source: String,
    pub name: String,
    pub rtype: u16,
    pub value: String,
    /// Record TTL as announced by the peer; used for F-B05 / F-D02 aging.
    pub ttl: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StatsSnapshot {
    pub queries: u64,
    pub responses: u64,
    pub ingested: u64,
    pub announces: u64,
    pub malformed: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Default)]
struct Stats {
    queries: AtomicU64,
    responses: AtomicU64,
    ingested: AtomicU64,
    announces: AtomicU64,
    malformed: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
}

fn push_bounded<T>(queue: &Mutex<VecDeque<T>>, item: T, cap: usize) {
    let mut guard = queue.lock().unwrap();
    while guard.len() >= cap {
        guard.pop_front();
    }
    guard.push_back(item);
}

pub struct Engine {
    socket: Arc<MdnsSocket>,
    records: Arc<RwLock<Vec<ResourceRecord>>>,
    stats: Arc<Stats>,
    queries: Arc<Mutex<VecDeque<QueryEntry>>>,
    observed: Arc<Mutex<VecDeque<ObservedEntry>>>,
    running: Arc<AtomicBool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    config: RwLock<MdnsConfig>,
    own_ips: Vec<IpAddr>,
}

impl Engine {
    pub fn start(config: MdnsConfig) -> std::io::Result<Arc<Self>> {
        let socket = Arc::new(MdnsSocket::bind(
            config.group,
            config.port,
            &config.interfaces,
            true,
        )?);
        let records = Arc::new(RwLock::new(config.build_records()));
        let mut own_ips: Vec<IpAddr> = local_ipv4_addrs().into_iter().map(IpAddr::V4).collect();
        own_ips.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let engine = Arc::new(Self {
            socket,
            records,
            stats: Arc::new(Stats::default()),
            queries: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_QUERIES))),
            observed: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_OBSERVED))),
            running: Arc::new(AtomicBool::new(true)),
            threads: Mutex::new(Vec::new()),
            config: RwLock::new(config),
            own_ips,
        });
        {
            let worker = Arc::clone(&engine);
            let handle = thread::spawn(move || worker.recv_loop());
            engine.threads.lock().unwrap().push(handle);
        }
        {
            let worker = Arc::clone(&engine);
            let handle = thread::spawn(move || worker.announce_loop());
            engine.threads.lock().unwrap().push(handle);
        }
        Ok(engine)
    }

    fn recv_loop(&self) {
        let mut buffer = [0u8; MAX_BUFFER];
        while self.running.load(Ordering::Relaxed) {
            match self.socket.recv(&mut buffer) {
                Ok((len, peer)) => self.handle(&buffer[..len], peer),
                Err(_) => continue, // read timeout or transient error
            }
        }
    }

    fn handle(&self, data: &[u8], peer: SocketAddr) {
        self.stats
            .bytes_in
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        let message = match Message::decode(data) {
            Ok(message) => message,
            Err(_) => {
                self.stats.malformed.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if message.is_query() {
            self.stats.queries.fetch_add(1, Ordering::Relaxed);
            for question in &message.questions {
                push_bounded(
                    &self.queries,
                    QueryEntry {
                        ts: now(),
                        source: peer.to_string(),
                        name: question.name.clone(),
                        qtype: type_name(question.qtype).to_string(),
                    },
                    MAX_QUERIES,
                );
            }
            self.answer(&message, peer);
        } else {
            self.ingest(&message, peer);
        }
    }

    fn answer(&self, query: &Message, peer: SocketAddr) {
        let records = self.records.read().unwrap();
        let matched: Vec<ResourceRecord> = records
            .iter()
            .filter(|rr| query.questions.iter().any(|q| question_matches(rr, q)))
            .filter(|rr| !suppressed(rr, &query.answers))
            .cloned()
            .collect();
        drop(records);
        if matched.is_empty() {
            return;
        }
        // QU questions want a unicast reply carrying the query id; otherwise
        // reply by multicast with id 0 (RFC 6762 5.4 / 6).
        let unicast = query.questions.iter().any(|q| q.wants_unicast());
        let mut response = Message::response(if unicast { query.id } else { 0 });
        response.answers = matched;
        let target = if unicast {
            peer
        } else {
            self.socket.group_addr()
        };
        let encoded = response.encode();
        if self.socket.send(&encoded, target).is_ok() {
            self.stats.responses.fetch_add(1, Ordering::Relaxed);
            self.stats
                .bytes_out
                .fetch_add(encoded.len() as u64, Ordering::Relaxed);
        }
    }

    fn ingest(&self, message: &Message, peer: SocketAddr) {
        // F-C04: never register our own answers as an external source.
        let own = self.own_ips.contains(&peer.ip());
        for rr in message
            .answers
            .iter()
            .chain(&message.authorities)
            .chain(&message.additionals)
        {
            self.stats.ingested.fetch_add(1, Ordering::Relaxed);
            if own {
                continue;
            }
            push_bounded(
                &self.observed,
                ObservedEntry {
                    ts: now(),
                    source: peer.to_string(),
                    name: rr.name.clone(),
                    rtype: rr.rtype,
                    value: rdata_text(rr),
                    ttl: rr.ttl,
                },
                MAX_OBSERVED,
            );
        }
    }

    fn announce_loop(&self) {
        let (count, interval_ms) = {
            let cfg = self.config.read().unwrap();
            (cfg.announce_count, cfg.announce_interval_ms)
        };
        for _ in 0..count {
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            self.announce();
            thread::sleep(Duration::from_millis(interval_ms));
        }
        let tick_ms = 250u64;
        let mut elapsed_ms = 0u64;
        while self.running.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(tick_ms));
            elapsed_ms += tick_ms;
            let period_s = self.config.read().unwrap().announce_period_s;
            if period_s == 0 {
                return;
            }
            if elapsed_ms >= period_s.saturating_mul(1000) {
                elapsed_ms = 0;
                self.announce();
            }
        }
    }

    fn announce(&self) {
        let mut response = Message::response(0);
        response.answers = self.records.read().unwrap().clone();
        if response.answers.is_empty() {
            return;
        }
        let encoded = response.encode();
        if self.socket.broadcast(&encoded).is_ok() {
            self.stats.announces.fetch_add(1, Ordering::Relaxed);
            self.stats
                .bytes_out
                .fetch_add(encoded.len() as u64, Ordering::Relaxed);
        }
    }

    /// F-A09: publish all records with TTL 0 before shutting down.
    pub fn goodbye(&self) {
        let mut response = Message::response(0);
        response.answers = self
            .records
            .read()
            .unwrap()
            .iter()
            .map(|rr| rr.goodbye())
            .collect();
        if !response.answers.is_empty() {
            let encoded = response.encode();
            if self.socket.broadcast(&encoded).is_ok() {
                self.stats
                    .bytes_out
                    .fetch_add(encoded.len() as u64, Ordering::Relaxed);
            }
        }
    }

    pub fn shutdown(&self) {
        self.goodbye();
        self.running.store(false, Ordering::Relaxed);
    }

    /// F-B06: send a probe on a separate socket and collect answers. Using a
    /// distinct socket (SO_REUSEPORT) makes this a genuine loopback E2E probe.
    /// `unicast` sets the QU bit so responders reply directly to this socket.
    pub fn probe(
        &self,
        name: &str,
        qtype: u16,
        unicast: bool,
        timeout_ms: u64,
    ) -> std::io::Result<Vec<ResourceRecord>> {
        // For a QU probe, bind an ephemeral port and send to the real mDNS
        // port. The unicast reply then has only one possible recipient (this
        // socket); with SO_REUSEPORT on the shared port the kernel could
        // otherwise deliver it to the engine socket instead.
        let (group, mdns_port, interfaces) = {
            let cfg = self.config.read().unwrap();
            (cfg.group, cfg.port, cfg.interfaces.clone())
        };
        let bind_port = if unicast { 0 } else { mdns_port };
        let socket = MdnsSocket::bind(group, bind_port, &interfaces, true)?;
        let mut question = Question::new(name, qtype);
        if unicast {
            question = question.with_qu();
        }
        let query = Message::query(0, vec![question.clone()]);
        let target = if unicast {
            group_addr(group, mdns_port)
        } else {
            socket.group_addr()
        };
        let encoded = query.encode();
        socket.send(&encoded, target)?;
        self.stats
            .bytes_out
            .fetch_add(encoded.len() as u64, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut buffer = [0u8; MAX_BUFFER];
        let mut found = Vec::new();
        while Instant::now() < deadline {
            match socket.recv(&mut buffer) {
                Ok((len, _)) => {
                    self.stats.bytes_in.fetch_add(len as u64, Ordering::Relaxed);
                    if let Ok(message) = Message::decode(&buffer[..len]) {
                        if message.is_response() {
                            for rr in message
                                .answers
                                .iter()
                                .chain(&message.authorities)
                                .chain(&message.additionals)
                            {
                                if question_matches(rr, &question) || rr.name == question.name {
                                    found.push(rr.clone());
                                }
                            }
                        }
                    }
                }
                Err(_) => continue,
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.rtype.cmp(&b.rtype)));
        found.dedup();
        Ok(found)
    }

    pub fn records_snapshot(&self) -> Vec<ResourceRecord> {
        self.records.read().unwrap().clone()
    }

    pub fn queries_snapshot(&self) -> Vec<QueryEntry> {
        self.queries.lock().unwrap().iter().cloned().collect()
    }

    pub fn observed_snapshot(&self) -> Vec<ObservedEntry> {
        self.observed.lock().unwrap().iter().cloned().collect()
    }

    pub fn stats_snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            queries: self.stats.queries.load(Ordering::Relaxed),
            responses: self.stats.responses.load(Ordering::Relaxed),
            ingested: self.stats.ingested.load(Ordering::Relaxed),
            announces: self.stats.announces.load(Ordering::Relaxed),
            malformed: self.stats.malformed.load(Ordering::Relaxed),
            bytes_in: self.stats.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.stats.bytes_out.load(Ordering::Relaxed),
        }
    }

    pub fn config(&self) -> MdnsConfig {
        self.config.read().unwrap().clone()
    }

    /// F-E03: swap the declarative config and the published RR set.
    /// Socket-level parameters (group / port / interfaces) belong to the
    /// already-bound UDP socket and therefore still require a restart; every
    /// other field applies on the next announce, probe or status read.
    pub fn reload(&self, config: MdnsConfig) {
        let records = config.build_records();
        *self.config.write().unwrap() = config;
        *self.records.write().unwrap() = records;
    }

    /// F-C06 / F-D01 / F-B05: aging of the bounded audit and observed rings.
    /// Query entries use `default_ttl_s`; observed entries honor their own RR
    /// TTL (falling back to the default when the peer advertised TTL 0).
    pub fn purge_expired(&self, now: f64, default_ttl_s: f64) {
        {
            let mut queue = self.queries.lock().unwrap();
            while queue
                .front()
                .is_some_and(|entry| now - entry.ts > default_ttl_s)
            {
                queue.pop_front();
            }
        }
        {
            let mut queue = self.observed.lock().unwrap();
            while queue.front().is_some_and(|entry| {
                let ttl = if entry.ttl == 0 {
                    default_ttl_s
                } else {
                    entry.ttl as f64
                };
                now - entry.ts > ttl
            }) {
                queue.pop_front();
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.running.swap(false, Ordering::Relaxed) {
            self.goodbye();
        }
        let handles: Vec<JoinHandle<()>> = self.threads.lock().unwrap().drain(..).collect();
        for handle in handles {
            let _ = handle.join();
        }
    }
}
