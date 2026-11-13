//! lan-mdns M1 skeleton.
//!
//! Ships the shared API surface, embedded WebUI, liveness/readiness probes and
//! Prometheus metrics. The mDNS protocol itself is intentionally NOT
//! implemented here (PRD 7.5, M2).

use std::sync::Arc;
use std::time::Instant;

use common_api::{err, json_quote, ok, App, Context, Request, Response};
use common_config::{config_path_from_env, load, port_from_env};
use common_health::Health;
use common_metrics::Registry;
use common_webui::INDEX_HTML;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg = load(&config_path_from_env());
    let version = env!("CARGO_PKG_VERSION");
    let commit = option_env!("GIT_COMMIT").unwrap_or("dev");
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);

    if args.iter().any(|arg| arg == "--cli") {
        println!(
            "component=lan-mdns version={} commit={} target={}",
            version, commit, target
        );
        println!(
            "{}",
            common_log::info("lan-mdns", "cli mode, no listener started")
        );
        return;
    }

    let health = Arc::new(Health::new());
    let metrics = Registry::new("lan-mdns");
    metrics.set_build_info(version, commit, &target);

    let ctx = Arc::new(Context {
        component: "lan-mdns".to_string(),
        version: version.to_string(),
        started: Instant::now(),
        metrics,
        health: Arc::clone(&health),
        admin_token: cfg.admin_token.clone(),
        config_raw: cfg.raw.clone(),
        webui: cfg.webui,
    });

    let app = App::new()
        .route("GET", "/", view_index)
        .route("GET", "/healthz", view_healthz)
        .route("GET", "/readyz", view_readyz)
        .route("GET", "/api/status", view_status)
        .route("GET", "/api/metrics", view_metrics)
        .route("GET", "/api/config", view_config)
        .route("GET", "/api/auth", view_auth);

    let addr = format!("{}:{}", cfg.listen, port_from_env());
    health.mark_ready();
    eprintln!(
        "{}",
        common_log::info("lan-mdns", &format!("listening on {}", addr))
    );
    if let Err(error) = app.serve(&addr, ctx) {
        eprintln!(
            "{}",
            common_log::error("lan-mdns", &format!("server failed: {}", error))
        );
        std::process::exit(1);
    }
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
    let uptime = ctx.started.elapsed().as_secs_f64();
    let data = format!(
        "{{\"component\":{},\"version\":{},\"running\":true,\"uptime\":{:.3},\"protocol\":{}}}",
        json_quote(&ctx.component),
        json_quote(&ctx.version),
        uptime,
        json_quote("not-implemented-m1")
    );
    ok(req, &data)
}

fn view_metrics(_: &Request, ctx: &Context) -> Response {
    ctx.metrics
        .set("uptime_seconds", &[], ctx.started.elapsed().as_secs_f64());
    Response {
        status: 200,
        content_type: "text/plain; version=0.0.4; charset=utf-8".to_string(),
        body: ctx.metrics.render(),
    }
}

fn view_config(req: &Request, ctx: &Context) -> Response {
    let data = format!("{{\"raw\":{}}}", json_quote(&ctx.config_raw));
    ok(req, &data)
}

fn view_auth(req: &Request, ctx: &Context) -> Response {
    let data = format!("{{\"token_required\":{}}}", !ctx.admin_token.is_empty());
    ok(req, &data)
}

fn view_index(_: &Request, ctx: &Context) -> Response {
    if ctx.webui {
        Response::html(200, INDEX_HTML.to_string())
    } else {
        Response::text(404, "webui disabled".to_string())
    }
}
