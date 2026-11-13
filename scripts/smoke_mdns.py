#!/usr/bin/env python3
"""M2.1 + M2.2 runtime smoke test for the lan-mdns single binary.

Pure standard library. Each instance gets its own temporary working directory
(config, auth digest, logs and snapshot history), so a single run never writes
to the repository and never inherits state from another instance. The binary
starts on an *isolated* multicast group/port so no real mDNS traffic leaves the
host.

Coverage:
  * M2.1 — API envelope, RR set, Prometheus, genuine query/answer/probe loopback,
    graceful shutdown and Admin Token regression.
  * M2.2 — admin-password write protection, config save/import/export/history/
    rollback/reload, bounded structured logs, SSE stream, health targets,
    conflict aggregation and the remaining metric families.

Environment:
  LAN_MDNS_BIN      path to binary   (default target/release/lan-mdns)
  LAN_STUDIO_PORT   HTTP port        (default 18080)
  LAN_MDNS_PORT     mDNS UDP port    (default 15353)
  LAN_MDNS_GROUP    multicast group  (default 239.1.2.3)
  LAN_STUDIO_CONFIG config template  (default components/lan-mdns/config.template.yaml)
"""

import json
import os
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, os.getenv("LAN_MDNS_BIN", "target/release/lan-mdns"))
TEMPLATE = os.path.join(
    ROOT, os.getenv("LAN_STUDIO_CONFIG", "components/lan-mdns/config.template.yaml")
)
HTTP_PORT = int(os.getenv("LAN_STUDIO_PORT", "18080"))
MDNS_PORT = int(os.getenv("LAN_MDNS_PORT", "15353"))
MDNS_GROUP = os.getenv("LAN_MDNS_GROUP", "239.1.2.3")
BASE = "http://127.0.0.1:{}".format(HTTP_PORT)

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))

RESULTS = []


def check(name, condition, detail=""):
    RESULTS.append((name, bool(condition), detail))
    flag = "PASS" if condition else "FAIL"
    print(
        "{:03d} {}  {}{}".format(
            len(RESULTS), flag, name, ("  | " + detail) if detail else ""
        )
    )


def http(method, path, body=None, token=None, password=None, base=BASE, timeout=6):
    url = base + path
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token is not None:
        headers["X-Admin-Token"] = token
    if password is not None:
        headers["X-Admin-Password"] = password
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with OPENER.open(req, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, dict(resp.headers), raw
    except urllib.error.HTTPError as exc:
        return exc.code, dict(exc.headers), exc.read().decode()
    except (urllib.error.URLError, TimeoutError, OSError):
        # Server not up yet (or transient); wait_ready retries.
        return 0, {}, ""


def as_json(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return {}


def data_of(raw):
    return as_json(raw).get("data", {})


def err_of(raw):
    return as_json(raw).get("error", {})


def wait_ready(base, seconds=15):
    deadline = time.time() + seconds
    while time.time() < deadline:
        status, _, raw = http("GET", "/healthz", base=base)
        if status == 200 and as_json(raw).get("ok") is True:
            return True
        time.sleep(0.25)
    return False


def free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def start_instance(config_text, http_port, mdns_port):
    """Launch one fully isolated instance; return (proc, log, workdir, config_path)."""
    workdir = tempfile.mkdtemp(prefix="lanmdns-smoke-")
    config_path = os.path.join(workdir, "config.yaml")
    with open(config_path, "w") as handle:
        handle.write(config_text)
    env = dict(os.environ)
    env.update(
        {
            "LAN_STUDIO_CONFIG": config_path,
            "LAN_STUDIO_AUTH_FILE": os.path.join(workdir, "auth.toml"),
            "LAN_STUDIO_LOG_DIR": os.path.join(workdir, "logs"),
            "LAN_STUDIO_HISTORY_DIR": os.path.join(workdir, "history"),
            "LAN_STUDIO_PORT": str(http_port),
            "LAN_MDNS_PORT": str(mdns_port),
            "LAN_MDNS_GROUP": MDNS_GROUP,
        }
    )
    log = tempfile.NamedTemporaryFile(
        mode="w+", prefix="lanmdns-smoke-", suffix=".log", delete=False
    )
    proc = subprocess.Popen([BIN], cwd=ROOT, env=env, stdout=log, stderr=log)
    return proc, log, workdir, config_path


def stop_instance(proc, log, workdir):
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    log.flush()
    with open(log.name, "r", errors="replace") as handle:
        text = handle.read()
    shutil.rmtree(workdir, ignore_errors=True)
    try:
        os.unlink(log.name)
    except OSError:
        pass
    return text


def sse_probe(path, port=HTTP_PORT, timeout=1.5):
    """Read the SSE handshake, replayed data frames and at least one keepalive."""
    sock = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    sock.settimeout(timeout)
    request = (
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        "Accept: text/event-stream\r\nConnection: close\r\n\r\n".format(path)
    )
    sock.sendall(request.encode())
    out = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            chunk = sock.recv(2048)
        except socket.timeout:
            break
        if not chunk:
            break
        out += chunk
        if b"data:" in out and b": keepalive" in out:
            break
    sock.close()
    return out.decode(errors="replace")


def main():
    if not os.path.isfile(BIN):
        print("FAIL  binary missing: {}".format(BIN))
        return 1
    with open(TEMPLATE, "r") as handle:
        template_text = handle.read()

    proc, log, workdir, config_path = start_instance(
        template_text, HTTP_PORT, MDNS_PORT
    )
    workdir = workdir
    try:
        ready = wait_ready(BASE)
        check("server becomes ready", ready, BASE)
        if not ready:
            print(stop_instance(proc, log, workdir))
            workdir = None
            return 1

        # ======================= M2.1 baseline ============================
        status, _, raw = http("GET", "/healthz")
        check("/healthz 200", status == 200)
        check("/healthz ok:true", as_json(raw).get("ok") is True)

        status, _, raw = http("GET", "/readyz")
        body = as_json(raw)
        check("/readyz 200", status == 200)
        check("/readyz ok:true", body.get("ok") is True)
        check("/readyz ready:true", body.get("data", {}).get("ready") is True)

        status, _, raw = http("GET", "/api/status")
        body = as_json(raw)
        data = body.get("data", {})
        check("/api/status 200", status == 200)
        check("ok envelope", body.get("ok") is True)
        check("request_id present", bool(body.get("request_id")))
        check("protocol mdns/1", data.get("protocol") == "mdns/1")
        check("component lan-mdns", data.get("component") == "lan-mdns")
        check("records published", data.get("records") == 8,
              "records={}".format(data.get("records")))
        check("isolated group", data.get("group") == MDNS_GROUP,
              "group={}".format(data.get("group")))
        check("isolated port", data.get("mdns_port") == MDNS_PORT,
              "port={}".format(data.get("mdns_port")))

        status, _, raw = http("GET", "/api/records")
        body = as_json(raw)
        records = body.get("data", {}).get("records", [])
        types = {r.get("type") for r in records}
        check("/api/records 200", status == 200)
        check("records total 8", body.get("data", {}).get("total") == 8)
        check("A/AAAA/PTR/SRV/TXT all present",
              {"A", "AAAA", "PTR", "SRV", "TXT"}.issubset(types), str(sorted(types)))
        check("IPv4 reverse PTR published",
              any(r.get("name") == "10.5.168.192.in-addr.arpa" for r in records))
        check("service type enumeration published",
              any(r.get("name") == "_services._dns-sd._udp.local" for r in records))
        check("cache_flush bit on authoritative A",
              any(r.get("name") == "nas.local" and r.get("type") == "A"
                  and r.get("cache_flush") is True for r in records))

        status, _, raw = http("GET", "/api/interfaces")
        body = as_json(raw)
        ifaces = body.get("data", {}).get("interfaces", [])
        check("/api/interfaces 200", status == 200)
        check("at least one interface", len(ifaces) >= 1)
        check("interface shape", all("name" in i and "addr" in i for i in ifaces))

        status, headers, raw = http("GET", "/api/metrics")
        check("/api/metrics 200", status == 200)
        check("Prometheus 0.0.4 content-type", "0.0.4" in headers.get("Content-Type", ""))
        mandatory = [
            "lanstudio_mdns_up", "lanstudio_mdns_uptime_seconds",
            "lanstudio_mdns_build_info", "lanstudio_mdns_config_reloads_total",
            "lanstudio_mdns_config_saves_total",
            "lanstudio_mdns_http_requests_total",
            "lanstudio_mdns_alerts_sent_total", "lanstudio_mdns_log_disk_bytes",
            "lanstudio_mdns_log_disk_limit_bytes",
        ]
        missing = [m for m in mandatory if m not in raw]
        check("nine mandatory metric families", not missing, str(missing))
        mdns_specific = [
            "lanstudio_mdns_records", "lanstudio_mdns_queries_total",
            "lanstudio_mdns_responses_total", "lanstudio_mdns_announces_total",
            "lanstudio_mdns_probe_total", "lanstudio_mdns_query_audit_entries",
            "lanstudio_mdns_browse_instances",
        ]
        missing = [m for m in mdns_specific if m not in raw]
        check("seven mdns metric families", not missing, str(missing))
        check("no double-prefixed metrics", "lanstudio_mdns_mdns_" not in raw)
        check("records gauge labelled by type",
              'lanstudio_mdns_records{type="A"}' in raw)

        for path, key in [("/api/queries", "queries"), ("/api/browse", "browse")]:
            status, _, raw = http("GET", path)
            body = as_json(raw)
            check("{} 200".format(path), status == 200)
            check("{} bounded array".format(path),
                  isinstance(body.get("data", {}).get(key), list))

        status, _, raw = http("POST", "/api/probe", {"name": "nas.local", "type": "A"})
        answers = data_of(raw).get("answers", [])
        check("/api/probe A 200", status == 200)
        check("probe A returns our published A record",
              any(a.get("name") == "nas.local" and a.get("type") == "A"
                  and a.get("value") == "192.168.5.10" for a in answers), str(answers))

        status, _, raw = http("POST", "/api/probe",
                              {"name": "_http._tcp.local", "type": "PTR"})
        answers = data_of(raw).get("answers", [])
        check("probe PTR returns service instance",
              any(a.get("name") == "_http._tcp.local" and a.get("type") == "PTR"
                  for a in answers), str(answers))

        status, _, raw = http("POST", "/api/probe",
                              {"name": "nas.local", "type": "AAAA", "unicast": True})
        answers = data_of(raw).get("answers", [])
        check("QU unicast probe returns AAAA",
              any(a.get("name") == "nas.local" and a.get("type") == "AAAA"
                  and a.get("value") == "fe80::1" for a in answers), str(answers))

        status, _, raw = http("POST", "/api/probe", {"type": "A"})
        check("probe without name -> 400", status == 400)
        check("probe without name -> invalid_config",
              err_of(raw).get("code") == "invalid_config")

        status, _, raw = http("POST", "/api/probe", {"name": "bad"})
        check("invalid name -> 400", status == 400)
        check("invalid name -> invalid_config",
              err_of(raw).get("code") == "invalid_config")

        status, _, raw = http("GET", "/api/status")
        data = as_json(raw).get("data", {})
        check("queries counted", data.get("queries", 0) >= 3,
              "queries={}".format(data.get("queries")))
        check("responses counted", data.get("responses", 0) >= 3,
              "responses={}".format(data.get("responses")))

        status, _, raw = http("GET", "/api/queries?limit=1000")
        body = as_json(raw)
        check("query audit populated",
              body.get("data", {}).get("total", 0) >= 3,
              "total={}".format(body.get("data", {}).get("total")))
        check("query audit entries shaped",
              all("name" in e and "qtype" in e and "source" in e
                  for e in body.get("data", {}).get("queries", [])))

        status, _, raw = http("GET", "/")
        check("WebUI 200", status == 200)
        check("WebUI contains Lan Studio", "Lan Studio" in raw)

        status, _, raw = http("GET", "/api/config")
        check("/api/config 200", status == 200)
        check("config raw is the YAML",
              "component: lan-mdns" in data_of(raw).get("raw", ""))

        status, _, raw = http("GET", "/api/auth")
        check("/api/auth 200", status == 200)
        check("token_required false by default",
              data_of(raw).get("token_required") is False)

        status, _, _ = http("GET", "/api/does-not-exist")
        check("unknown API 404", status == 404)

        cli = subprocess.run(
            [BIN, "--cli"], cwd=ROOT, capture_output=True, text=True,
            env=dict(os.environ, LAN_STUDIO_CONFIG=TEMPLATE,
                     LAN_MDNS_PORT=str(MDNS_PORT), LAN_MDNS_GROUP=MDNS_GROUP),
        )
        check("--cli exits 0", cli.returncode == 0)
        check("--cli validates config offline",
              "component=lan-mdns" in cli.stdout and "records=8" in cli.stdout,
              cli.stdout.strip().splitlines()[1] if len(cli.stdout.splitlines()) > 1 else "")

        # ================= M2.2-A: read-only observability ================
        status, _, raw = http("GET", "/api/status")
        status_data = data_of(raw)
        check("status exposes security block",
              status_data.get("security", {}).get("write_protection") is True
              and status_data.get("security", {}).get("password_set") is False)
        check("status exposes config history block",
              "versions" in status_data.get("history", {})
              and "limit" in status_data.get("history", {}))
        check("status exposes business health block",
              all(k in status_data.get("health", {}) for k in ("up", "down", "unknown")))
        check("status exposes bounded log block",
              all(k in status_data.get("logs", {})
                  for k in ("buffered", "capacity", "disk_bytes", "disk_limit_bytes")))
        check("status exposes lifecycle counters",
              all(k in status_data for k in
                  ("config_saves", "config_reloads", "conflicts", "alerts")))

        status, _, raw = http("GET", "/api/logs")
        logs_data = data_of(raw)
        check("/api/logs 200", status == 200)
        check("logs returns structured records",
              isinstance(logs_data.get("logs"), list) and len(logs_data.get("logs", [])) >= 1)
        check("log records follow log-record.json",
              all(all(k in line for k in ("ts", "level", "source", "msg"))
                  for line in logs_data.get("logs", [])))
        check("log buffer capacity is bounded", logs_data.get("capacity") == 512,
              "capacity={}".format(logs_data.get("capacity")))
        check("disk hard limit = max_bytes * (backup_count + 1)",
              logs_data.get("disk_limit_bytes") == 1048576 * (3 + 1),
              "limit={}".format(logs_data.get("disk_limit_bytes")))
        check("disk bytes reported", logs_data.get("disk_bytes", 0) > 0)
        check("log file exists on disk",
              os.path.isfile(os.path.join(os.path.dirname(config_path), "logs", "lan-mdns.log")))

        status, _, raw = http("GET", "/api/logs?limit=2")
        check("/api/logs honours limit", len(data_of(raw).get("logs", [])) <= 2)

        stream = sse_probe("/api/logs/stream")
        check("SSE handshake is 200 event-stream",
              "HTTP/1.1 200" in stream and "text/event-stream" in stream)
        check("SSE emits retry hint", "retry: 1000" in stream)
        check("SSE replays buffered data frames", "data: {" in stream)
        check("SSE data frames are structured records",
              '"level"' in stream and '"source"' in stream and '"msg"' in stream)
        check("SSE emits bounded keepalive", ": keepalive" in stream)

        status, _, raw = http("GET", "/api/conflicts")
        check("/api/conflicts 200", status == 200)
        check("conflicts is an array", isinstance(data_of(raw).get("conflicts"), list))

        status, _, raw = http("GET", "/api/browse")
        browse = data_of(raw)
        check("/api/browse 200", status == 200)
        check("browse exposes aggregated services",
              isinstance(browse.get("services"), list) and "service_types" in browse)

        status, _, raw = http("GET", "/api/health/targets")
        health = data_of(raw)
        check("/api/health/targets 200", status == 200)
        check("health target counts are numeric",
              all(isinstance(health.get(k), int) for k in ("total", "up", "down", "unknown")))
        check("no business health targets by default", health.get("total") == 0)

        status, _, raw = http("GET", "/api/alerts")
        check("/api/alerts 200 (M2.3 placeholder)", status == 200
              and data_of(raw).get("enabled") is False)

        status, _, raw = http("GET", "/api/metrics")
        families = [
            "lanstudio_mdns_bytes_total", "lanstudio_mdns_health_targets",
            "lanstudio_mdns_conflicts", "lanstudio_mdns_config_history_versions",
            "lanstudio_mdns_auth_password_enabled", "lanstudio_mdns_gateway_enabled",
        ]
        missing = [m for m in families if m not in raw]
        check("six M2.2 metric families present", not missing, str(missing))
        check("bytes_total labelled by direction",
              'lanstudio_mdns_bytes_total{direction="in"}' in raw
              and 'lanstudio_mdns_bytes_total{direction="out"}' in raw)
        check("health_targets labelled by state",
              'lanstudio_mdns_health_targets{state="up"}' in raw)
        check("auth_password_enabled is 0 before setup",
              "lanstudio_mdns_auth_password_enabled 0" in raw)
        check("gateway_enabled is 0 (M2.3)", "lanstudio_mdns_gateway_enabled 0" in raw)

        status, _, raw = http("GET", "/api/config/snapshot")
        snap = data_of(raw)
        check("/api/config/snapshot 200", status == 200)
        check("snapshot has all six contract fields",
              all(k in snap for k in ("schema", "component", "created_at", "reason",
                                      "config_text", "checksum")))
        check("snapshot checksum is sha256:hex64",
              isinstance(snap.get("checksum"), str)
              and snap["checksum"].startswith("sha256:")
              and len(snap["checksum"]) == 71)
        check("snapshot carries current config text",
              "component: lan-mdns" in snap.get("config_text", ""))

        # ============ M2.2-B: write protection and password ===============
        for path in ("/api/config", "/api/reload", "/api/config/rollback",
                     "/api/config/snapshot"):
            status, _, raw = http("POST", path, {})
            check("write {} without password -> 409".format(path), status == 409)
            check("write {} -> setup_required".format(path),
                  err_of(raw).get("code") == "setup_required")

        status, _, raw = http("POST", "/api/auth/password", {"new_password": "short"})
        check("short password -> 400", status == 400)
        check("short password -> invalid_config",
              err_of(raw).get("code") == "invalid_config")

        password = "smoke-pass-123"
        status, _, raw = http("POST", "/api/auth/password",
                              {"new_password": password})
        check("first-time password setup -> 200", status == 200)
        check("password setup returns password_set",
              data_of(raw).get("password_set") is True)
        auth_file = os.path.join(os.path.dirname(config_path), "auth.toml")
        check("admin password digest is 0600",
              os.path.isfile(auth_file)
              and stat.S_IMODE(os.stat(auth_file).st_mode) == 0o600,
              auth_file)
        check("digest format is pbkdf2_sha256",
              os.path.isfile(auth_file)
              and open(auth_file).read().startswith("pbkdf2_sha256$"))

        status, _, raw = http("GET", "/api/auth")
        check("password_set true after setup", data_of(raw).get("password_set") is True)

        status, _, raw = http("POST", "/api/auth/verify", {"password": password})
        check("verify correct password -> 200", status == 200)
        check("verify returns valid:true", data_of(raw).get("valid") is True)
        status, _, raw = http("POST", "/api/auth/verify", {"password": "wrong-pass"})
        check("verify wrong password -> 403", status == 403)
        check("verify wrong password -> auth_failed",
              err_of(raw).get("code") == "auth_failed")

        status, _, raw = http("POST", "/api/config", {"text": template_text})
        check("write without password after setup -> 403", status == 403)
        check("write without password -> auth_failed",
              err_of(raw).get("code") == "auth_failed")
        status, _, raw = http("POST", "/api/config", {"text": template_text},
                              password="wrong-pass")
        check("write with wrong password -> 403", status == 403)
        status, _, raw = http("POST", "/api/config", {"text": template_text},
                              password="wrong-pass")
        check("write with wrong password -> auth_failed",
              err_of(raw).get("code") == "auth_failed")

        status, _, raw = http("POST", "/api/auth/password",
                              {"new_password": "another-pass-456"})
        check("change without current password -> 403", status == 403)
        new_password = "changed-pass-789"
        status, _, raw = http("POST", "/api/auth/password",
                              {"current_password": password,
                               "new_password": new_password})
        check("change with correct current password -> 200", status == 200)
        status, _, raw = http("POST", "/api/auth/verify", {"password": new_password})
        check("new password verifies", status == 200)
        status, _, raw = http("POST", "/api/auth/verify", {"password": password})
        check("old password no longer verifies", status == 403)
        password = new_password

        # ============ M2.2-C: config lifecycle =============================
        dead_port = free_port()
        health_block = (
            "  health_targets:\n"
            "    - alias: self\n"
            "      host: 127.0.0.1\n"
            "      port: {}\n"
            "    - alias: dead\n"
            "      host: 127.0.0.1\n"
            "      port: {}"
        ).format(HTTP_PORT, dead_port)
        new_config = template_text.replace("  health_targets: []", health_block)

        status, _, raw = http("POST", "/api/config", {"text": new_config},
                              password=password)
        check("save config with password -> 200", status == 200)
        check("save returns saved:true and a snapshot id",
              data_of(raw).get("saved") is True and bool(data_of(raw).get("snapshot")))
        check("config persisted atomically to disk",
              "alias: self" in open(config_path).read())
        status, _, raw = http("GET", "/api/config")
        check("/api/config reflects saved text",
              "alias: self" in data_of(raw).get("raw", ""))
        status, _, raw = http("GET", "/api/status")
        check("hot reload keeps the published RR set",
              data_of(raw).get("records") == 8,
              "records={}".format(data_of(raw).get("records")))

        status, _, raw = http("GET", "/api/config/history")
        history = data_of(raw)
        check("/api/config/history 200", status == 200)
        check("history has at least one snapshot", history.get("total", 0) >= 1)
        check("history declares bounded limit", history.get("limit") == 30)
        entries = history.get("history", [])
        check("history entries carry full shape",
              all(all(k in e for k in ("id", "created_at", "reason", "checksum", "size"))
                  for e in entries))
        check("history ids are whitelisted",
              all(e.get("id", "").startswith("config-") and e.get("id", "").endswith(".json")
                  for e in entries))

        snapshot_id = entries[0]["id"] if entries else ""
        status, _, raw = http("GET", "/api/config/snapshot?id=" + snapshot_id)
        check("?id= reads one retained snapshot", status == 200
              and data_of(raw).get("checksum", "").startswith("sha256:"))
        status, _, raw = http("GET", "/api/config/snapshot?id=../evil.json")
        check("?id= path traversal -> 400", status == 400)
        check("?id= path traversal -> invalid_config",
              err_of(raw).get("code") == "invalid_config")
        status, _, raw = http(
            "GET", "/api/config/snapshot?id=config-20200101T000000Z-aaaaaa-bbbb.json")
        check("?id= missing snapshot -> 404", status == 404)
        check("?id= missing snapshot -> not_found",
              err_of(raw).get("code") == "not_found")

        exported = data_of(http("GET", "/api/config/snapshot")[2])
        status, _, raw = http("POST", "/api/config/snapshot", exported, password=password)
        check("import a valid snapshot -> 200", status == 200)
        check("import returns imported:true", data_of(raw).get("imported") is True)
        tampered = dict(exported)
        tampered["checksum"] = "sha256:" + "0" * 64
        status, _, raw = http("POST", "/api/config/snapshot", tampered, password=password)
        check("import tampered checksum -> 400", status == 400)
        check("import tampered checksum -> invalid_config",
              err_of(raw).get("code") == "invalid_config")

        status, _, raw = http("POST", "/api/config",
                              {"text": "version: 2\ncomponent: lan-mdns\n"},
                              password=password)
        check("save invalid config -> 400", status == 400)
        check("save invalid config -> invalid_config",
              err_of(raw).get("code") == "invalid_config")

        status, _, raw = http("GET", "/api/health/targets")
        health = data_of(raw)
        check("health targets hot-swapped by reload", health.get("total") == 2,
              str(health))
        check("health probe reports up and down",
              health.get("up") == 1 and health.get("down") == 1, str(health))
        check("health target entries are shaped",
              all(all(k in t for k in ("alias", "host", "port", "state", "checked_at",
                                       "latency_ms"))
                  for t in health.get("targets", [])))

        status, _, raw = http("GET", "/api/status")
        counters = data_of(raw)
        check("config_saves counter advanced", counters.get("config_saves", 0) >= 2,
              "saves={}".format(counters.get("config_saves")))
        check("config_reloads counter advanced", counters.get("config_reloads", 0) >= 2,
              "reloads={}".format(counters.get("config_reloads")))
        check("history versions counter advanced",
              counters.get("history", {}).get("versions", 0) >= 1)

        status, _, raw = http("GET", "/api/config/history")
        entries = data_of(raw).get("history", [])
        original = next((e for e in entries
                         if e.get("reason", "").startswith("before-save")), None)
        check("before-save snapshot retained for rollback", original is not None)
        if original:
            status, _, raw = http("POST", "/api/config/rollback",
                                  {"id": original["id"]}, password=password)
            check("rollback -> 200", status == 200)
            check("rollback returns rolled_back:true",
                  data_of(raw).get("rolled_back") is True)
            status, _, raw = http("GET", "/api/config")
            check("rollback restores the original config",
                  "health_targets: []" in data_of(raw).get("raw", ""))

        status, _, raw = http("POST", "/api/reload", {}, password=password)
        check("manual reload -> 200", status == 200)
        check("reload returns reloaded:true", data_of(raw).get("reloaded") is True)

        status, _, raw = http("GET", "/api/metrics")
        check("auth_password_enabled is 1 after setup",
              "lanstudio_mdns_auth_password_enabled 1" in raw)
        check("config_history_versions tracks retained snapshots",
              "lanstudio_mdns_config_history_versions" in raw)

        text = stop_instance(proc, log, workdir)
        workdir = None
        check("graceful shutdown sends goodbye", "goodbye sent" in text,
              text.strip().splitlines()[-1] if text.strip() else "empty log")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)
        if workdir:
            shutil.rmtree(workdir, ignore_errors=True)
        try:
            os.unlink(log.name)
        except OSError:
            pass

    # ============== Admin Token instance (M1 auth regression) =============
    token_text = template_text.replace('admin_token: ""',
                                       'admin_token: "smoke-token"')
    proc2, log2, workdir2, _ = start_instance(
        token_text, HTTP_PORT + 1, MDNS_PORT + 1
    )
    try:
        base2 = "http://127.0.0.1:{}".format(HTTP_PORT + 1)
        check("token instance ready", wait_ready(base2))
        status, _, raw = http("GET", "/api/status", base=base2)
        check("no token -> 401", status == 401)
        check("no token -> auth_required", err_of(raw).get("code") == "auth_required")
        status, _, raw = http("GET", "/api/status", token="wrong", base=base2)
        check("wrong token -> 403", status == 403)
        check("wrong token -> auth_failed", err_of(raw).get("code") == "auth_failed")
        status, _, _ = http("GET", "/api/status", token="smoke-token", base=base2)
        check("correct token -> 200", status == 200)
        status, _, _ = http("GET", "/healthz", token="wrong", base=base2)
        check("healthz bypasses auth", status == 200)
        status, _, raw = http("POST", "/api/config", {"text": token_text},
                              token="smoke-token", base=base2)
        check("token does not replace password on write routes", status == 409,
              "status={}".format(status))
    finally:
        stop_instance(proc2, log2, workdir2)
        if proc2.poll() is None:
            proc2.kill()
            proc2.wait(timeout=5)

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    failed = [name for name, ok, _ in RESULTS if not ok]
    print("\n{} passed / {} failed".format(passed, len(failed)))
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("SMOKE OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
