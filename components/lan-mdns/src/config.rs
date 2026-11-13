//! Component-owned `mdns:` configuration namespace.
//!
//! The common segments are validated by `common_config::validate_text`; this
//! module parses and validates the `lan-mdns` namespace and compiles the
//! declarative records/services into DNS resource records.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

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
            services.push(ServiceDef {
                instance,
                service_type,
                host,
                port: service_port,
                txt,
            });
        }

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
}
