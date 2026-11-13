//! Prometheus metric registry implementing docs/METRICS.md.
//! Text exposition format 0.0.4, no third-party crates.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn metric_name(sample_key: &str) -> String {
    sample_key
        .split('{')
        .next()
        .unwrap_or(sample_key)
        .to_string()
}

struct Inner {
    prefix: String,
    gauges: BTreeMap<String, f64>,
    counters: BTreeMap<String, u64>,
    help: BTreeMap<String, String>,
    types: BTreeMap<String, String>,
}

impl Inner {
    fn name(&self, short: &str) -> String {
        format!("{}{}", self.prefix, short)
    }

    fn sample_key(&self, short: &str, labels: &[(&str, &str)]) -> String {
        let name = self.name(short);
        if labels.is_empty() {
            return name;
        }
        let parts: Vec<String> = labels
            .iter()
            .map(|(key, value)| format!("{}=\"{}\"", key, escape_label(value)))
            .collect();
        format!("{}{{{}}}", name, parts.join(","))
    }

    fn declare(&mut self, short: &str, metric_type: &str, help: &str) {
        let name = self.name(short);
        self.types.insert(name.clone(), metric_type.to_string());
        self.help.insert(name, help.to_string());
    }

    fn set(&mut self, short: &str, labels: &[(&str, &str)], value: f64) {
        let key = self.sample_key(short, labels);
        self.gauges.insert(key, value);
    }

    fn inc(&mut self, short: &str, labels: &[(&str, &str)], delta: u64) {
        let key = self.sample_key(short, labels);
        *self.counters.entry(key).or_insert(0) += delta;
    }
}

#[derive(Clone)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
}

impl Registry {
    pub fn new(component: &str) -> Self {
        let suffix = component.strip_prefix("lan-").unwrap_or(component);
        let prefix = format!("lanstudio_{}_", suffix);
        let mut inner = Inner {
            prefix,
            gauges: BTreeMap::new(),
            counters: BTreeMap::new(),
            help: BTreeMap::new(),
            types: BTreeMap::new(),
        };
        inner.declare("up", "gauge", "Main loop running");
        inner.declare("uptime_seconds", "gauge", "Process uptime in seconds");
        inner.declare("build_info", "gauge", "Build information, always 1");
        inner.declare("config_reloads_total", "counter", "Config reloads");
        inner.declare("config_saves_total", "counter", "Config saves");
        inner.declare("http_requests_total", "counter", "API requests");
        inner.declare("alerts_sent_total", "counter", "Alert deliveries");
        inner.declare("log_disk_bytes", "gauge", "Log disk usage in bytes");
        inner.declare(
            "log_disk_limit_bytes",
            "gauge",
            "Log disk upper bound in bytes",
        );
        inner.set("up", &[], 1.0);
        inner.set("uptime_seconds", &[], 0.0);
        inner.set("log_disk_bytes", &[], 0.0);
        inner.set("log_disk_limit_bytes", &[], 0.0);
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    pub fn set_build_info(&self, version: &str, commit: &str, target: &str) {
        let mut inner = self.inner.lock().unwrap();
        let build_name = inner.name("build_info");
        inner.gauges.retain(|key, _| metric_name(key) != build_name);
        inner.set(
            "build_info",
            &[("version", version), ("commit", commit), ("target", target)],
            1.0,
        );
    }

    /// Let a component register additional families from docs/METRICS.md
    /// without hard-coding component names into this common crate.
    pub fn declare_gauge(&self, short: &str, help: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.declare(short, "gauge", help);
    }

    pub fn declare_counter(&self, short: &str, help: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.declare(short, "counter", help);
    }

    pub fn set(&self, short: &str, labels: &[(&str, &str)], value: f64) {
        let mut inner = self.inner.lock().unwrap();
        inner.set(short, labels, value);
    }

    /// Publish an absolute snapshot of an internal counter (the engine owns
    /// the authoritative value, so we cannot use incremental `inc` here).
    pub fn set_counter(&self, short: &str, labels: &[(&str, &str)], value: u64) {
        let mut inner = self.inner.lock().unwrap();
        let key = inner.sample_key(short, labels);
        inner.counters.insert(key, value);
    }

    pub fn inc(&self, short: &str, labels: &[(&str, &str)], delta: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.inc(short, labels, delta);
    }

    pub fn render(&self) -> String {
        let inner = self.inner.lock().unwrap();
        let mut samples: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (key, value) in &inner.gauges {
            let name = metric_name(key);
            samples
                .entry(name)
                .or_default()
                .push((key.clone(), format!("{}", value)));
        }
        for (key, value) in &inner.counters {
            let name = metric_name(key);
            samples
                .entry(name)
                .or_default()
                .push((key.clone(), value.to_string()));
        }
        let mut out = String::new();
        for (name, metric_type) in &inner.types {
            if let Some(help) = inner.help.get(name) {
                out.push_str(&format!("# HELP {} {}\n", name, help));
            }
            out.push_str(&format!("# TYPE {} {}\n", name, metric_type));
            match samples.get(name) {
                Some(list) => {
                    for (key, value) in list {
                        out.push_str(&format!("{} {}\n", key, value));
                    }
                }
                None => out.push_str(&format!("{} 0\n", name)),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_nine_mandatory_families() {
        let registry = Registry::new("lan-mdns");
        registry.set_build_info("0.1.0", "abc123", "x86_64-linux");
        let text = registry.render();
        for short in [
            "up",
            "uptime_seconds",
            "build_info",
            "config_reloads_total",
            "config_saves_total",
            "http_requests_total",
            "alerts_sent_total",
            "log_disk_bytes",
            "log_disk_limit_bytes",
        ] {
            assert!(
                text.contains(&format!("lanstudio_mdns_{}", short)),
                "missing {}",
                short
            );
        }
        assert!(text.contains("# TYPE lanstudio_mdns_up gauge"));
        assert!(text.contains("lanstudio_mdns_build_info{version=\"0.1.0\""));
    }

    #[test]
    fn counters_accumulate_with_labels() {
        let registry = Registry::new("lan-mdns");
        registry.inc(
            "http_requests_total",
            &[("method", "GET"), ("path", "/healthz"), ("code", "200")],
            1,
        );
        let text = registry.render();
        assert!(text.contains(
            "lanstudio_mdns_http_requests_total{method=\"GET\",path=\"/healthz\",code=\"200\"} 1"
        ));
    }
}
