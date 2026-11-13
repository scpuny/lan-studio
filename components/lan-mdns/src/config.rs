//! Component-owned `mdns:` configuration namespace.
//!
//! The common segments are validated by `common_config::validate_text`; this
//! module parses and validates the `lan-mdns` namespace and compiles the
//! declarative records/services into DNS resource records.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use common_config::{get_bool, get_str, get_u16, get_u32, get_u64, list_len, ConfigError};
use common_dns::{
    is_local, reverse_ptr_name, validate_name, RData, ResourceRecord, TYPE_A, TYPE_AAAA, TYPE_PTR,
    TYPE_SRV, TYPE_TXT,
};
use common_net::local_ipv4_addrs;

const BOUNDED_RECORDS: usize = 512;
const SERVICE_TYPE_ENUM: &str = "_services._dns-sd._udp.local";

fn error(field: &str, detail: &str) -> ConfigError {
    ConfigError {
        code: "invalid_config",
        field: field.to_string(),
        detail: detail.to_string(),
    }
}

fn parse_type(text: &str) -> Option<u16> {
    match text.to_ascii_uppercase().as_str() {
        "A" => Some(TYPE_A),
        "AAAA" => Some(TYPE_AAAA),
        "PTR" => Some(TYPE_PTR),
        "SRV" => Some(TYPE_SRV),
        "TXT" => Some(TYPE_TXT),
        other => other.parse::<u16>().ok(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDef {
    pub name: String,
    pub rtype: u16,
    pub value: String,
    pub ttl: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDef {
    pub instance: String,
    pub service_type: String,
    pub host: String,
    pub port: u16,
    pub txt: Vec<String>,
    /// F-H02 per-service gateway switch; only consulted when the global
    /// `mdns.gateway.enabled` switch is on.
    pub proxy: bool,
}

/// One static Host -> `ip:port` gateway route (PRD 1.4: no dynamic routing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub host: String,
    pub target: String,
    pub proxy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfig {
    /// Global switch. Off means **no listener is created at all** (F-H02).
    pub enabled: bool,
    pub port: u16,
    pub listen: String,
    pub admin_host: String,
    pub timeout: Duration,
    pub routes: Vec<GatewayRoute>,
}

/// TCP business health target (F-D03). `alias` is optional and, when present,
/// is the only value allowed as a per-target metric label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthTargetDef {
    pub alias: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct MdnsConfig {
    pub group: Ipv4Addr,
    pub port: u16,
    pub ttl: u32,
    pub announce_count: u32,
    pub announce_interval_ms: u64,
    pub announce_period_s: u64,
    pub interface_mode: String,
    pub interfaces: Vec<Ipv4Addr>,
    pub reverse_ptr: bool,
    pub records: Vec<RecordDef>,
    pub services: Vec<ServiceDef>,
    pub health_targets: Vec<HealthTargetDef>,
    pub gateway: GatewayConfig,
}

impl MdnsConfig {
    pub fn from_flat(flat: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let group_text = get_str(flat, "mdns.group", "224.0.0.251");
        let group: Ipv4Addr = group_text
            .parse()
            .map_err(|_| error("mdns.group", "invalid IPv4 address"))?;
        if !group.is_multicast() {
            return Err(error("mdns.group", "must be an IPv4 multicast address"));
        }
        let port = get_u16(flat, "mdns.port", 5353);
        if port == 0 {
            return Err(error("mdns.port", "must be 1..65535"));
        }
        let ttl = get_u32(flat, "mdns.ttl", 120);
        let announce_count = get_u32(flat, "mdns.announce_count", 3);
        let announce_interval_ms = get_u64(flat, "mdns.announce_interval_ms", 250);
        let announce_period_s = get_u64(flat, "mdns.announce_period_s", 60);
        let reverse_ptr = get_bool(flat, "mdns.reverse_ptr", true);
        let interface_mode = get_str(flat, "mdns.interface_mode", "auto");

        let mut interfaces = Vec::new();
        for index in 0..list_len(flat, "mdns.interfaces") {
            let key = format!("mdns.interfaces[{}]", index);
            let value = get_str(flat, &key, "");
            let addr: Ipv4Addr = value
                .parse()
                .map_err(|_| error(&key, "invalid IPv4 address"))?;
            interfaces.push(addr);
        }
        match interface_mode.as_str() {
            "auto" => {}
            "all" => interfaces = local_ipv4_addrs(),
            "specified" => {
                if interfaces.is_empty() {
                    return Err(error("mdns.interfaces", "specified mode needs entries"));
                }
            }
            other => {
                return Err(error(
                    "mdns.interface_mode",
                    &format!("unknown mode {}", other),
                ))
            }
        }

        let mut records = Vec::new();
        for index in 0..list_len(flat, "mdns.records") {
            let base = format!("mdns.records[{}]", index);
            let name = get_str(flat, &format!("{}.name", base), "");
            let type_text = get_str(flat, &format!("{}.type", base), "A");
            let value = get_str(flat, &format!("{}.value", base), "");
            let record_ttl = get_u32(flat, &format!("{}.ttl", base), ttl);
            let rtype = parse_type(&type_text)
                .ok_or_else(|| error(&format!("{}.type", base), "unknown record type"))?;
            validate_name(&name)
                .map_err(|err| error(&format!("{}.name", base), &err.to_string()))?;
            if !is_local(&name) && !name.ends_with(".in-addr.arpa") {
                return Err(error(&format!("{}.name", base), "must be a .local name"));
            }
            match rtype {
                TYPE_A => {
                    value
                        .parse::<Ipv4Addr>()
                        .map_err(|_| error(&format!("{}.value", base), "invalid IPv4"))?;
                }
                TYPE_AAAA => {
                    value
                        .parse::<Ipv6Addr>()
                        .map_err(|_| error(&format!("{}.value", base), "invalid IPv6"))?;
                }
                TYPE_PTR => {
                    validate_name(&value)
                        .map_err(|err| error(&format!("{}.value", base), &err.to_string()))?;
                }
                TYPE_TXT | TYPE_SRV => {}
                other => {
                    return Err(error(
                        &format!("{}.type", base),
                        &format!("type {} is not publishable", other),
                    ))
                }
            }
            records.push(RecordDef {
                name,
                rtype,
                value,
                ttl: record_ttl,
            });
        }
        if records.len() > BOUNDED_RECORDS {
            return Err(error("mdns.records", "too many records"));
        }

        let mut services = Vec::new();
        for index in 0..list_len(flat, "mdns.services") {
            let base = format!("mdns.services[{}]", index);
            let instance = get_str(flat, &format!("{}.instance", base), "");
            let service_type = get_str(flat, &format!("{}.type", base), "");
            let host = get_str(flat, &format!("{}.host", base), "");
            let service_port = get_u16(flat, &format!("{}.port", base), 80);
            if instance.is_empty() {
                return Err(error(&format!("{}.instance", base), "must not be empty"));
            }
            validate_name(&service_type)
                .map_err(|err| error(&format!("{}.type", base), &err.to_string()))?;
            if !is_local(&service_type) {
                return Err(error(&format!("{}.type", base), "must be a .local type"));
            }
            validate_name(&host)
                .map_err(|err| error(&format!("{}.host", base), &err.to_string()))?;
            if service_port == 0 {
                return Err(error(&format!("{}.port", base), "must be 1..65535"));
            }
            let txt_prefix = format!("{}.txt.", base);
            let mut txt: Vec<String> = flat
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix(&txt_prefix)
                        .map(|name| format!("{}={}", name, value))
                })
                .collect();
            txt.sort();
            let proxy = get_bool(flat, &format!("{}.proxy", base), true);
            services.push(ServiceDef {
                instance,
                service_type,
                host,
                port: service_port,
                txt,
                proxy,
            });
        }

        let mut health_targets = Vec::new();
        for index in 0..list_len(flat, "mdns.health_targets") {
            let base = format!("mdns.health_targets[{}]", index);
            let alias = get_str(flat, &format!("{}.alias", base), "");
            let host = get_str(flat, &format!("{}.host", base), "");
            let target_port = get_u16(flat, &format!("{}.port", base), 0);
            if host.is_empty() {
                return Err(error(&format!("{}.host", base), "must not be empty"));
            }
            if target_port == 0 {
                return Err(error(&format!("{}.port", base), "must be 1..65535"));
            }
            let valid_alias = alias.is_empty()
                || (alias.len() <= 64
                    && alias
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));
            if !valid_alias {
                return Err(error(&format!("{}.alias", base), "invalid alias"));
            }
            health_targets.push(HealthTargetDef {
                alias,
                host,
                port: target_port,
            });
        }
        if health_targets.len() > 64 {
            return Err(error("mdns.health_targets", "at most 64 targets"));
        }

        // ---- F-H01..F-H03: static Host gateway ---------------------------
        let gateway_enabled = get_bool(flat, "mdns.gateway.enabled", false);
        let gateway_port = get_u16(flat, "mdns.gateway.port", 80);
        if gateway_port == 0 {
            return Err(error("mdns.gateway.port", "must be 1..65535"));
        }
        let gateway_listen = get_str(flat, "mdns.gateway.listen", "0.0.0.0");
        let gateway_admin_host =
            get_str(flat, "mdns.gateway.admin_host", "mdns.local").to_ascii_lowercase();
        let timeout_text = get_str(flat, "mdns.gateway.timeout_seconds", "6.0");
        let timeout_value: f64 = timeout_text
            .parse()
            .map_err(|_| error("mdns.gateway.timeout_seconds", "must be a number"))?;
        if !timeout_value.is_finite() || timeout_value <= 0.0 || timeout_value > 60.0 {
            return Err(error("mdns.gateway.timeout_seconds", "must be in (0, 60]"));
        }

        let mut routes: Vec<GatewayRoute> = Vec::new();
        // A published service is automatically a registered gateway name when
        // its host resolves to one of our own A records. PRD 1.4: no other
        // names are ever routable.
        for service in &services {
            let address = records.iter().find_map(|record| {
                (record.rtype == TYPE_A && record.name.eq_ignore_ascii_case(&service.host))
                    .then(|| record.value.clone())
            });
            if let Some(address) = address {
                routes.push(GatewayRoute {
                    host: service.host.to_ascii_lowercase(),
                    target: format!("{}:{}", address, service.port),
                    proxy: service.proxy,
                });
            }
        }
        for index in 0..list_len(flat, "mdns.gateway.hosts") {
            let base = format!("mdns.gateway.hosts[{}]", index);
            let host = get_str(flat, &format!("{}.host", base), "").to_ascii_lowercase();
            let target = get_str(flat, &format!("{}.target", base), "");
            let proxy = get_bool(flat, &format!("{}.proxy", base), true);
            if host.is_empty() || target.is_empty() {
                return Err(error(&base, "host and target are required"));
            }
            let valid_target = target
                .rsplit_once(':')
                .is_some_and(|(head, port)| !head.is_empty() && port.parse::<u16>().is_ok());
            if !valid_target {
                return Err(error(&format!("{}.target", base), "expected host:port"));
            }
            let entry = GatewayRoute {
                host: host.clone(),
                target,
                proxy,
            };
            match routes.iter_mut().find(|route| route.host == host) {
                Some(existing) => *existing = entry,
                None => routes.push(entry),
            }
        }
        if routes.len() > 256 {
            return Err(error("mdns.gateway.hosts", "at most 256 routes"));
        }
        let gateway = GatewayConfig {
            enabled: gateway_enabled,
            port: gateway_port,
            listen: gateway_listen,
            admin_host: gateway_admin_host,
            timeout: Duration::from_millis((timeout_value * 1000.0) as u64),
            routes,
        };

        Ok(Self {
            group,
            port,
            ttl,
            announce_count,
            announce_interval_ms,
            announce_period_s,
            interface_mode,
            interfaces,
            reverse_ptr,
            records,
            services,
            health_targets,
            gateway,
        })
    }

    /// Compile the declarative configuration into the RR set to publish.
    pub fn build_records(&self) -> Vec<ResourceRecord> {
        let mut out = Vec::new();
        for def in &self.records {
            let rdata = match def.rtype {
                TYPE_A => RData::A(def.value.parse().unwrap_or(Ipv4Addr::UNSPECIFIED)),
                TYPE_AAAA => RData::Aaaa(def.value.parse().unwrap_or(Ipv6Addr::UNSPECIFIED)),
                TYPE_PTR => RData::Ptr(def.value.clone()),
                TYPE_TXT => RData::Txt(vec![def.value.clone()]),
                TYPE_SRV => RData::Raw(TYPE_SRV, Vec::new()),
                _ => continue,
            };
            out.push(ResourceRecord::new(&def.name, rdata, def.ttl, true));
            if self.reverse_ptr && def.rtype == TYPE_A {
                if let Ok(ip) = def.value.parse::<Ipv4Addr>() {
                    out.push(ResourceRecord::new(
                        &reverse_ptr_name(ip),
                        RData::Ptr(def.name.clone()),
                        def.ttl,
                        false,
                    ));
                }
            }
        }

        let mut types: Vec<String> = Vec::new();
        for service in &self.services {
            let instance_name = format!("{}.{}", service.instance, service.service_type);
            out.push(ResourceRecord::new(
                &service.service_type,
                RData::Ptr(instance_name.clone()),
                4500,
                false,
            ));
            out.push(ResourceRecord::new(
                &instance_name,
                RData::Srv {
                    priority: 0,
                    weight: 0,
                    port: service.port,
                    target: service.host.clone(),
                },
                self.ttl,
                true,
            ));
            let txt = if service.txt.is_empty() {
                vec![String::new()]
            } else {
                service.txt.clone()
            };
            out.push(ResourceRecord::new(
                &instance_name,
                RData::Txt(txt),
                self.ttl,
                true,
            ));
            if !types.contains(&service.service_type) {
                types.push(service.service_type.clone());
            }
        }
        for service_type in types {
            out.push(ResourceRecord::new(
                SERVICE_TYPE_ENUM,
                RData::Ptr(service_type),
                4500,
                false,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common_config::parse_flat;

    const TEMPLATE: &str = "\
version: 1
component: lan-mdns
mdns:
  group: 224.0.0.251
  port: 15353
  ttl: 120
  reverse_ptr: true
  records:
    - name: nas.local
      type: A
      value: 192.168.5.10
    - name: nas.local
      type: AAAA
      value: fe80::1
    - name: box.local
      type: TXT
      value: env=lab
  services:
    - instance: Web Server
      type: _http._tcp.local
      host: nas.local
      port: 8080
      txt:
        path: /
        version: \"1\"
";

    #[test]
    fn builds_full_rr_set_from_template() {
        let flat = parse_flat(TEMPLATE);
        let cfg = MdnsConfig::from_flat(&flat).unwrap();
        assert_eq!(cfg.records.len(), 3);
        assert_eq!(cfg.services.len(), 1);
        assert_eq!(cfg.services[0].txt, vec!["path=/", "version=1"]);
        let rrs = cfg.build_records();
        let count = |t: u16| rrs.iter().filter(|rr| rr.rtype == t).count();
        assert_eq!(count(TYPE_A), 1);
        assert_eq!(count(TYPE_AAAA), 1);
        assert_eq!(count(TYPE_TXT), 2); // static TXT + service TXT
        assert_eq!(count(TYPE_SRV), 1);
        assert_eq!(count(TYPE_PTR), 3); // reverse + instance + type enum
        assert!(rrs.iter().any(|rr| rr.name == "10.5.168.192.in-addr.arpa"));
        assert!(rrs
            .iter()
            .any(|rr| rr.name == "_services._dns-sd._udp.local"));
    }

    #[test]
    fn parses_health_targets_and_rejects_invalid_alias() {
        let text = "\
version: 1
component: lan-mdns
mdns:
  health_targets:
    - alias: self
      host: 127.0.0.1
      port: 8080
    - alias: \"\"
      host: 127.0.0.1
      port: 1
";
        let cfg = MdnsConfig::from_flat(&parse_flat(text)).unwrap();
        assert_eq!(cfg.health_targets.len(), 2);
        assert_eq!(cfg.health_targets[0].alias, "self");
        assert_eq!(cfg.health_targets[0].port, 8080);
        assert!(cfg.health_targets[1].alias.is_empty());

        let bad = parse_flat(
            "version: 1\ncomponent: lan-mdns\nmdns:\n  health_targets:\n    - alias: bad/name\n      host: 127.0.0.1\n      port: 80\n",
        );
        assert_eq!(
            MdnsConfig::from_flat(&bad).unwrap_err().field,
            "mdns.health_targets[0].alias"
        );
    }

    #[test]
    fn rejects_invalid_component_config() {
        let cases = [
            ("version: 1\ncomponent: lan-mdns\nmdns:\n  group: 192.168.5.1\n", "mdns.group"),
            ("version: 1\ncomponent: lan-mdns\nmdns:\n  port: 0\n", "mdns.port"),
            (
                "version: 1\ncomponent: lan-mdns\nmdns:\n  records:\n    - name: bad\n      type: A\n      value: 10.0.0.1\n",
                "mdns.records[0].name",
            ),
            (
                "version: 1\ncomponent: lan-mdns\nmdns:\n  records:\n    - name: nas.local\n      type: A\n      value: nope\n",
                "mdns.records[0].value",
            ),
            (
                "version: 1\ncomponent: lan-mdns\nmdns:\n  interface_mode: specified\n  interfaces: []\n",
                "mdns.interfaces",
            ),
            (
                "version: 1\ncomponent: lan-mdns\nmdns:\n  services:\n    - instance: X\n      type: _http._tcp.local\n      host: nas.local\n      port: 0\n",
                "mdns.services[0].port",
            ),
        ];
        for (text, field) in cases {
            let flat = parse_flat(text);
            let err = MdnsConfig::from_flat(&flat).unwrap_err();
            assert_eq!(err.code, "invalid_config");
            assert_eq!(err.field, field);
        }
    }

    #[test]
    fn gateway_is_off_by_default_and_derives_routes_from_services() {
        let cfg = MdnsConfig::from_flat(&parse_flat(TEMPLATE)).unwrap();
        assert!(
            !cfg.gateway.enabled,
            "global switch must default to off (F-H02)"
        );
        assert_eq!(cfg.gateway.port, 80);
        assert_eq!(cfg.gateway.admin_host, "mdns.local");
        assert_eq!(cfg.gateway.routes.len(), 1);
        assert_eq!(cfg.gateway.routes[0].host, "nas.local");
        assert_eq!(cfg.gateway.routes[0].target, "192.168.5.10:8080");
        assert!(cfg.gateway.routes[0].proxy);
    }

    #[test]
    fn explicit_hosts_override_and_per_service_switch() {
        let text = "\
version: 1
component: lan-mdns
mdns:
  records:
    - name: nas.local
      type: A
      value: 192.168.5.10
  services:
    - instance: Web
      type: _http._tcp.local
      host: nas.local
      port: 8080
      proxy: false
    - instance: Box
      type: _http._tcp.local
      host: box.local
      port: 9000
  gateway:
    enabled: true
    port: 8081
    admin_host: Console.LOCAL
    timeout_seconds: 2.5
    hosts:
      - host: NAS.local
        target: \"192.168.5.99:80\"
        proxy: true
      - host: printer.local
        target: \"192.168.5.30:631\"
";
        let cfg = MdnsConfig::from_flat(&parse_flat(text)).unwrap();
        assert!(cfg.gateway.enabled);
        assert_eq!(cfg.gateway.port, 8081);
        assert_eq!(cfg.gateway.admin_host, "console.local");
        assert_eq!(cfg.gateway.timeout, Duration::from_millis(2500));
        // nas.local (derived) is overridden by the explicit entry, and
        // printer.local is added: 2 routes, not 3.
        assert_eq!(cfg.gateway.routes.len(), 2);
        assert!(cfg
            .gateway
            .routes
            .iter()
            .any(|route| route.host == "printer.local"));
        let nas = cfg
            .gateway
            .routes
            .iter()
            .find(|route| route.host == "nas.local")
            .unwrap();
        assert_eq!(nas.target, "192.168.5.99:80", "explicit host must win");
        assert!(nas.proxy, "explicit entry re-enables a disabled service");
        // box.local has no A record, so it must NOT become a route (PRD 1.4).
        assert!(!cfg.gateway.routes.iter().any(|r| r.host == "box.local"));
        assert!(!cfg.services[0].proxy, "per-service switch is parsed");
    }

    #[test]
    fn gateway_target_and_timeout_are_validated_offline() {
        let base = "version: 1\ncomponent: lan-mdns\nmdns:\n  gateway:\n    enabled: true\n";
        let bad_target = format!(
            "{}    hosts:\n      - host: nas.local\n        target: not-a-socket\n",
            base
        );
        let err = MdnsConfig::from_flat(&parse_flat(&bad_target)).unwrap_err();
        assert_eq!(err.field, "mdns.gateway.hosts[0].target");

        let bad_timeout = format!("{}    timeout_seconds: 0\n", base);
        let err = MdnsConfig::from_flat(&parse_flat(&bad_timeout)).unwrap_err();
        assert_eq!(err.field, "mdns.gateway.timeout_seconds");
    }
}
