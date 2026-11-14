//! Lan Studio unified web console — frontend + Rust BFF (M7).
//!
//! This binary is NOT a `lan-*` component and does not go through
//! `common_component::run` (that would bind `GET /` to the six-region console
//! and register the 18 standard endpoints). It is a thin aggregation layer:
//!
//! - `GET /`                  single-file frontend (capability-driven)
//! - `GET /healthz`           liveness (plain JSON)
//! - `GET /api/webui/status`  read-only aggregation metadata
//! - `GET|POST /p/<name>/api/...` bounded same-origin proxy (see `proxy.rs`)
//!
//! The set of modules the frontend renders is decided by `modules.yaml` and the
//! generated `capabilities.json`; the backend never hardcodes which components
//! exist (D-M7-1). Security boundary (D-M7-6): the proxy only forwards
//! `X-Admin-Password` / `X-Admin-Token`, it never stores an administrator
//! password and the DOM carries no `type="password"` input.

mod capabilities;
mod proxy;

use capabilities::{parse_modules, validate};
use common_api::{err, json_quote, ok, App, Context, Health, Registry, Request, Response};
use std::sync::Arc;
use std::time::Instant;

const DEFAULT_PORT: u16 = 8090;
const DEFAULT_CONFIG: &str = "lan-webui/modules.yaml";

const HELP: &str = "\
Lan Studio unified web console — frontend + Rust BFF (aggregation layer, not a component)

Usage: lan-webui [OPTIONS]

Options:
  --check     Parse modules.yaml + verify the embedded aggregate, then exit
  --version   Print the version and exit
  --help      Print this help and exit

Routes:
  GET  /                       capability-driven single-file frontend
  GET  /healthz                liveness
  GET  /api/webui/status       read-only aggregation metadata
  GET|POST /p/<name>/api/...   same-origin proxy, allowlisted + bounded

Environment:
  LAN_WEBUI_CONFIG  modules.yaml path   (default: lan-webui/modules.yaml)
  LAN_STUDIO_PORT   listen port          (default: 8090)
  LAN_WEBUI_TOKEN   optional admin token for /api/* and /p/* (default: empty)
";

fn component_json(item: &capabilities::Component) -> String {
    format!(
        "{{\"name\":{},\"label\":{},\"port\":{},\"icon\":{},\"tone\":{}}}",
        json_quote(&item.name),
        json_quote(&item.label),
        item.port,
        json_quote(&item.icon),
        json_quote(&item.tone)
    )
}

fn view_index(_req: &Request, ctx: &Context) -> Response {
    if !ctx.webui {
        return Response::text(404, "webui disabled".to_string());
    }
    // Inject the aggregated manifest; the frontend reads
    // window.__LAN_CAPABILITIES__ and renders only declared modules.
    let script = format!(
        "<script>window.__LAN_CAPABILITIES__={};</script>",
        capabilities::AGGREGATE_JSON.replace("</", "<\\/")
    );
    Response::html(
        200,
        capabilities::INDEX_HTML.replacen("</head>", &format!("{script}</head>"), 1),
    )
}

fn view_healthz(_req: &Request, _ctx: &Context) -> Response {
    Response::json(200, "{\"ok\":true}".to_string())
}

fn view_status(req: &Request, ctx: &Context) -> Response {
    let modules = match parse_modules(&ctx.config_raw) {
        Ok(modules) => modules,
        Err(error) => return err(req, "internal_error", &error, 500),
    };
    let items: Vec<String> = modules
        .components
        .iter()
        .map(component_json)
        .collect();
    ok(
        req,
        &format!(
            "{{\"components\":[{}],\"total\":{},\"modules\":{},\"libs\":[{}],\"token_hint\":{},\"proxy_timeout_ms\":{}}}",
            items.join(","),
            items.len(),
            capabilities::module_count(),
            modules
                .libs
                .iter()
                .map(|l| json_quote(l))
                .collect::<Vec<_>>()
                .join(","),
            !ctx.admin_token.is_empty(),
            proxy::PROXY_TIMEOUT_MS,
        ),
    )
}

fn test_context(text: &str) -> Context {
    Context {
        component: "lan-webui".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        started: Instant::now(),
        metrics: Registry::new("lan-webui"),
        health: Arc::new(Health::new()),
        admin_token: String::new(),
        config_raw: text.to_string(),
        webui: true,
        events: None,
        password: None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let version = env!("CARGO_PKG_VERSION");
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("lan-webui {version}");
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return;
    }
    let path = std::env::var("LAN_WEBUI_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("cannot read modules {path}: {error}");
            std::process::exit(2);
        }
    };
    let modules = match parse_modules(&text) {
        Ok(modules) => modules,
        Err(error) => {
            eprintln!("invalid modules {path}: {error}");
            std::process::exit(2);
        }
    };
    if args.iter().any(|a| a == "--check") {
        if let Err(error) = validate(&modules) {
            eprintln!("capability consistency: {error}");
            std::process::exit(2);
        }
        println!(
            "{{\"ok\":true,\"version\":{},\"components\":{},\"libs\":{}}}",
            modules.version,
            modules.components.len(),
            modules.libs.len()
        );
        return;
    }
    let port = std::env::var("LAN_STUDIO_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let token = std::env::var("LAN_WEBUI_TOKEN").unwrap_or_default();
    // Validate the embedded aggregate against modules.yaml before listening.
    if let Err(error) = validate(&modules) {
        eprintln!("capability consistency: {error}");
        std::process::exit(2);
    }
    let ctx = Arc::new(Context {
        admin_token: token,
        ..test_context(&text)
    });
    let app = App::new()
        .route("GET", "/", view_index)
        .route("GET", "/healthz", view_healthz)
        .route("GET", "/api/webui/status", view_status)
        .route_prefix("GET", "/p/", proxy::view_proxy)
        .route_prefix("POST", "/p/", proxy::view_proxy);
    let addr = format!("127.0.0.1:{port}");
    if let Err(error) = app.serve(&addr, ctx) {
        eprintln!("lan-webui failed to listen on {addr}: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULES: &str = include_str!("../modules.yaml");

    fn ctx() -> Context {
        test_context(MODULES)
    }

    fn req(path: &str) -> Request {
        use std::net::SocketAddr;
        Request {
            method: "GET".to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: std::collections::HashMap::new(),
            body: String::new(),
            peer: "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
        }
    }

    #[test]
    fn modules_parse_and_validate() {
        let m = parse_modules(MODULES).unwrap();
        assert_eq!(m.components.len(), 8);
        assert!(m.libs.contains(&"common-probe".to_string()));
        assert!(validate(&m).is_ok());
        // every enabled component reappears in the embedded aggregate
        for c in &m.components {
            assert!(capabilities::AGGREGATE_JSON.contains(&format!("\"{}\"", c.name)));
        }
    }

    #[test]
    fn index_injects_aggregate_and_is_offline() {
        let response = view_index(&req("/"), &ctx());
        assert_eq!(response.status, 200);
        let html = response.body;
        assert!(html.contains("window.__LAN_CAPABILITIES__"));
        assert!(html.contains("\"component\": \"lan-webui\""));
        for forbidden in [
            "<script src=",
            "src=\"http",
            "url(http",
            "<link rel=\"stylesheet\"",
            "type=\"password\"",
        ] {
            assert!(!html.contains(forbidden), "leaked: {forbidden}");
        }
    }

    #[test]
    fn healthz_is_plain_ok() {
        let response = view_healthz(&req("/healthz"), &ctx());
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
    }

    #[test]
    fn status_reports_components_without_secrets() {
        let response = view_status(&req("/api/webui/status"), &ctx());
        assert_eq!(response.status, 200);
        assert!(response.body.contains("\"total\":8"));
        assert!(response.body.contains("lan-ssdp"));
        for forbidden in ["password", "shared_secret", "\"token\"", "admin_token"] {
            assert!(!response.body.contains(forbidden), "leaked {forbidden}");
        }
    }
}
