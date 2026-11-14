#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5 runtime acceptance gate (PRD 4.8 / 4.9 / 8.2).

Starts lan-sync / lan-ssdp / lan-shell in isolated temp directories, plus a
best-effort lan-mdns instance for the unified browse endpoint, and verifies:

  * the unified contract (common-component) on both M5 components,
  * the config save -> snapshot -> rollback -> import -> reload loop,
  * the lan-sync 8-endpoint lifecycle against a local in-process fake node,
  * the lan-ssdp 4-endpoint lifecycle with unicast-only UDP NOTIFY / byebye,
  * the lan-shell aggregation layer (navigation, offline WebUI, no proxy),
  * the lan-mdns /api/browse/unified shared ServiceRecord model,
  * M5-specific "no overlap / no coupling" and "no password echo" assertions.

Zero third-party dependency; only the Python standard library. Usage:
    cargo build --workspace
    python3 scripts/test_m5.py
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN_DIR = ROOT / "target" / "debug"
COMPONENTS = [
    "lan-mdns", "lan-relay", "lan-ca", "lan-scan",
    "lan-hosts", "lan-wol", "lan-sync", "lan-ssdp",
]
PASSWORD = "M5Acceptance456"
HDR = {"X-Admin-Password": PASSWORD}
RESULTS: list[tuple[str, bool, str]] = []
PROCS: list[subprocess.Popen] = []

# All high ports so the gate never collides with the documented 8080-8087
# defaults. SSDP traffic is unicast to 127.0.0.1 only (never a real LAN).
PORT_SYNC = 18086
PORT_SSDP = 18087
PORT_SHELL = 18090
PORT_MDNS = 18080
MDNS_UDP_PORT = "15353"
MDNS_GROUP = "239.1.2.5"
SSDP_UDP_PORT = 18089
FAKE_NODE_PORT = 18099          # in-process fake sync peer
DEAD_PORT = 18098               # nothing listens here -> target_unreachable
SSDP_DESC_PORT = 18097          # fake HTTP description server for LOCATION

# Fixed RFC3339 UTC timestamps for conflict decisions (lexicographic compare).
TS_BASE = "2026-01-01T00:00:00Z"
TS_OLDER = "2025-01-01T00:00:00Z"
TS_NEWER = "2027-01-01T00:00:00Z"

# Credentials must never appear as *field names* in any status / list response.
CRED_MARKERS = ['"token"', '"password"', '"secret"', '"shared_secret"']

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))

_RECORD_SCHEMA = None


# ----------------------------------------------------------------- fixtures


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, bool(ok), detail))
    print(("  PASS  " if ok else "  FAIL  ") + name
          + (("  -- " + detail) if detail and not ok else ""))


def call(base: str, path: str, method: str = "GET", body=None, headers=None):
    data = None if body is None else (
        json.dumps(body).encode() if not isinstance(body, (bytes, str))
        else (body.encode() if isinstance(body, str) else body)
    )
    hdr = {"Content-Type": "application/json"}
    hdr.update(headers or {})
    req = urllib.request.Request(base + path, data=data, method=method, headers=hdr)
    try:
        with OPENER.open(req, timeout=20) as resp:
            return resp.status, resp.read().decode("utf-8", "replace"), resp.headers.get("Content-Type", "")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace"), exc.headers.get("Content-Type", "")
    except urllib.error.URLError as exc:
        return 0, "", ""


def jcall(base: str, path: str, method: str = "GET", body=None, headers=None):
    status, raw, _ = call(base, path, method, body, headers)
    try:
        return status, json.loads(raw), raw
    except json.JSONDecodeError:
        return status, {}, raw


def data_of(parsed):
    """Return the ``data`` payload of a unified envelope, or the bare body.

    common-component wraps every business response in {ok,data,request_id}.
    Accepting both shapes keeps the gate robust to a bare implementation too.
    """
    if isinstance(parsed, dict) and isinstance(parsed.get("data"), dict):
        return parsed["data"]
    return parsed


def err_code(parsed) -> str:
    if not isinstance(parsed, dict):
        return ""
    err = parsed.get("error")
    if isinstance(err, dict):
        return str(err.get("code", ""))
    return str(parsed.get("code", ""))


def has_cred_fields(raw: str):
    return [m for m in CRED_MARKERS if m in raw]


def binary(name: str) -> Path:
    path = BIN_DIR / name
    if not path.exists():
        raise RuntimeError(f"missing binary: {path} (run `cargo build --workspace` first)")
    return path


def terminate_all() -> None:
    for proc in PROCS:
        if proc.poll() is None:
            proc.terminate()
    for proc in PROCS:
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    PROCS.clear()


def start(name: str, port: int, workdir: Path, config_text=None, extra_env=None) -> str:
    binpath = binary(name)
    cfg = workdir / f"{name}.yaml"
    if config_text is None:
        shutil.copy(ROOT / "components" / name / "config.template.yaml", cfg)
    else:
        cfg.write_text(config_text, encoding="utf-8")
    env = os.environ.copy()
    env["LAN_STUDIO_PORT"] = str(port)
    env["LAN_STUDIO_CONFIG"] = str(cfg)
    if extra_env:
        env.update(extra_env)
    proc = subprocess.Popen(
        [str(binpath)], cwd=str(workdir), env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    PROCS.append(proc)
    base = f"http://127.0.0.1:{port}"
    for _ in range(80):
        try:
            status, _, _ = call(base, "/healthz")
            if status == 200:
                return base
        except Exception:
            pass
        if proc.poll() is not None:
            raise RuntimeError(f"{name} exited with {proc.returncode}")
        time.sleep(0.15)
    raise RuntimeError(f"{name} did not become healthy on {base}")


def offline_check(name: str, workdir: Path, config_text=None, label: str = "") -> None:
    cfg = workdir / f"{name}.yaml"
    if config_text is not None:
        cfg.write_text(config_text, encoding="utf-8")
    env = os.environ.copy()
    env["LAN_STUDIO_CONFIG"] = str(cfg)
    proc = subprocess.run([str(binary(name)), "--check"], cwd=str(workdir), env=env,
                          capture_output=True, text=True, timeout=20)
    out = proc.stdout + proc.stderr
    check(label or f"{name} --check offline validation",
          proc.returncode == 0 and '"ok":true' in proc.stdout, out)


def mdns_offline_check(workdir: Path) -> None:
    cfg = workdir / "lan-mdns.yaml"
    cfg.write_text((ROOT / "components" / "lan-mdns" / "config.template.yaml").read_text(
        encoding="utf-8"), encoding="utf-8")
    env = os.environ.copy()
    env["LAN_STUDIO_CONFIG"] = str(cfg)
    proc = subprocess.run([str(binary("lan-mdns")), "--validate"], cwd=str(workdir), env=env,
                          capture_output=True, text=True, timeout=20)
    out = proc.stdout + proc.stderr
    # lan-mdns --validate prints a structured log line ("config valid: ..."),
    # not the {"ok":true} envelope used by common-component --check.
    check("lan-mdns --validate offline validation",
          proc.returncode == 0 and ("config valid" in out or '"ok":true' in out), out)


def strip_comments(text: str) -> str:
    """Drop Rust doc/line comments so prose describing a boundary does not
    false-positive the 'no SOAP / no HTTP client' source scan."""
    out = []
    in_block = False
    for line in text.splitlines():
        stripped = line.lstrip()
        if in_block:
            if "*/" in stripped:
                in_block = False
            continue
        if stripped.startswith("//"):
            continue
        if stripped.startswith("/*"):
            if "*/" not in stripped:
                in_block = True
            continue
        out.append(line)
    return "\n".join(out)


def record_schema():
    global _RECORD_SCHEMA
    if _RECORD_SCHEMA is None:
        _RECORD_SCHEMA = json.loads(
            (ROOT / "contracts" / "service-record.json").read_text(encoding="utf-8"))
    return _RECORD_SCHEMA


def validate_record(rec, expect_source: str):
    """Minimal, dependency-free validation of contracts/service-record.json."""
    schema = record_schema()
    required = set(schema["required"])
    allowed = set(schema["properties"].keys())
    if not isinstance(rec, dict):
        return False, "record is not an object"
    keys = set(rec.keys())
    missing = required - keys
    extra = keys - allowed
    if missing:
        return False, f"missing={sorted(missing)}"
    if extra:
        return False, f"extra_keys={sorted(extra)}"
    if "attributes" in rec and not isinstance(rec["attributes"], dict):
        return False, "attributes is not an object"
    for field in ("name", "type", "address", "last_seen", "source"):
        if not isinstance(rec.get(field), str):
            return False, f"{field} is not a string"
    for field in ("port", "ttl"):
        if not isinstance(rec.get(field), int) or isinstance(rec.get(field), bool):
            return False, f"{field} is not an integer"
    if not (1 <= len(rec["name"]) <= 253) or not (1 <= len(rec["type"]) <= 253):
        return False, "name/type length out of bounds"
    if len(rec["address"]) > 64:
        return False, "address too long"
    if not (0 <= rec["port"] <= 65535):
        return False, "port out of range"
    if not (0 <= rec["ttl"] <= 86400):
        return False, "ttl out of range"
    if not rec["last_seen"].endswith("Z"):
        return False, "last_seen is not Z-terminated"
    if rec["source"] != expect_source:
        return False, f"source={rec['source']} expected {expect_source}"
    return True, ""


def poll_until(fn, timeout: float = 3.0, interval: float = 0.15) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if fn():
                return True
        except Exception:
            pass
        time.sleep(interval)
    return False


# ------------------------------------------------------- in-process fakes


class _LocalServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class _FakeHTTP:
    """Threaded 127.0.0.1 HTTP server; counts GET/POST calls."""

    def __init__(self, port: int, handler_cls):
        self.gets = 0
        self.posts = 0
        self._server = _LocalServer(("127.0.0.1", port), handler_cls)
        self._server.owner = self
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        try:
            self._server.shutdown()
            self._server.server_close()
            self._thread.join(timeout=3)
        except Exception:
            pass


class SyncHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def _reply(self, status, payload, ctype="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        owner = self.server.owner
        owner.gets += 1
        if self.path.startswith("/api/config/snapshot"):
            # §3.6 bare snapshot JSON (no envelope).
            self._reply(200, json.dumps(owner.snapshot()).encode())
        else:
            self._reply(200, b'{"ok":true}')

    def do_POST(self):
        owner = self.server.owner
        length = int(self.headers.get("Content-Length", 0) or 0)
        if length:
            self.rfile.read(length)
        owner.posts += 1
        self._reply(200, b'{"ok":true,"data":{"imported":true}}')


class DescHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def do_GET(self):
        owner = self.server.owner
        owner.gets += 1
        body = b'<?xml version="1.0"?><root/>'
        self.send_response(200)
        self.send_header("Content-Type", "text/xml")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0) or 0)
        if length:
            self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()


class SyncFake(_FakeHTTP):
    def __init__(self, port: int):
        self.mode = "valid"          # valid | tampered | older | newer
        self.config_text = "version: 1\ncomponent: lan-sync\n# m5 fake node\n"
        super().__init__(port, SyncHandler)

    def snapshot(self):
        created = {"valid": TS_BASE, "tampered": TS_BASE,
                   "older": TS_OLDER, "newer": TS_NEWER}[self.mode]
        checksum = "sha256:" + hashlib.sha256(self.config_text.encode()).hexdigest()
        if self.mode == "tampered":
            checksum = "sha256:" + "0" * 64
        return {
            "schema": 1,
            "component": "lan-sync",
            "created_at": created,
            "reason": "sync",
            "config_text": self.config_text,
            "checksum": checksum,
        }


class DescFake(_FakeHTTP):
    def __init__(self, port: int):
        super().__init__(port, DescHandler)


def send_ssdp(packet: str) -> None:
    """Unicast only; never a real LAN multicast."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.sendto(packet.encode(), ("127.0.0.1", SSDP_UDP_PORT))
    finally:
        sock.close()


# ------------------------------------------------------------ config helpers


def isolated_ssdp_config() -> str:
    text = (ROOT / "components" / "lan-ssdp" / "config.template.yaml").read_text(
        encoding="utf-8")
    # Only the ssdp: port is 1900; server.port is 8087. Replace the whole
    # indented line so the server port is never touched.
    text = text.replace('  group: "239.255.255.250"', '  group: "127.0.0.1"')
    text = text.replace("  port: 1900\n", f"  port: {SSDP_UDP_PORT}\n")
    text = text.replace("  max_age_seconds: 1800\n", "  max_age_seconds: 10\n")
    return text


def setup_password(base: str, name: str) -> None:
    status, setup, _ = jcall(base, "/api/auth/password", "POST",
                             {"new_password": PASSWORD})
    check(f"{name} admin password setup", status == 200
          and data_of(setup).get("password_set") is True, str(setup)[:160])


def replace_config(base: str, replacements, name: str) -> bool:
    status, cfg, _ = jcall(base, "/api/config")
    raw = data_of(cfg).get("raw", "")
    for old, new in replacements:
        raw = raw.replace(old, new)
    status, saved, _ = jcall(base, "/api/config", "POST", {"text": raw}, HDR)
    ok = status == 200 and data_of(saved).get("saved") is True
    check(f"{name} config hot-reload applied", ok, str(saved)[:160])
    return ok


def config_lifecycle(base: str, name: str) -> None:
    status, original, _ = jcall(base, "/api/config")
    raw = data_of(original).get("raw", "")
    status, saved, _ = jcall(base, "/api/config", "POST",
                             {"text": raw + "\n# m5-acceptance\n"}, HDR)
    check(f"{name} config save (validate->snapshot->atomic->reload)",
          status == 200 and data_of(saved).get("saved") is True, str(saved)[:160])
    status, hist, _ = jcall(base, "/api/config/history")
    items = data_of(hist).get("items", [])
    check(f"{name} snapshot created on save", status == 200 and len(items) >= 1,
          str(items)[:160])
    status, snap, raw_snap = jcall(base, "/api/config/snapshot?reason=gate")
    s = data_of(snap)
    check(f"{name} snapshot contract shape", status == 200 and s.get("schema") == 1
          and s.get("component") == name
          and str(s.get("checksum", "")).startswith("sha256:"), raw_snap[:160])
    version_id = items[0]["id"] if items else ""
    status, rolled, _ = jcall(base, "/api/config/rollback", "POST",
                              {"id": version_id}, HDR)
    check(f"{name} rollback", status == 200
          and data_of(rolled).get("rolled_back") is True, str(rolled)[:160])
    status, imported, _ = jcall(base, "/api/config/snapshot", "POST", raw_snap, HDR)
    check(f"{name} snapshot import (checksum verified)", status == 200
          and data_of(imported).get("imported") is True, str(imported)[:160])
    status, reloaded, _ = jcall(base, "/api/reload", "POST", {}, HDR)
    check(f"{name} manual reload", status == 200
          and data_of(reloaded).get("reloaded") is True, str(reloaded)[:160])
    status, bad, _ = jcall(base, "/api/config", "POST", {"text": "version: 2\n"}, HDR)
    check(f"{name} invalid config rejected", status == 400
          and err_code(bad) == "invalid_config", str(bad)[:160])


def unified_contract(name: str, base: str) -> None:
    short = name.replace("lan-", "")
    status, health, _ = jcall(base, "/healthz")
    check(f"{name} /healthz is constant ok",
          status == 200 and health == {"ok": True}, str(health))
    status, ready, _ = jcall(base, "/readyz")
    check(f"{name} /readyz ready", status == 200
          and data_of(ready).get("ready") is True, str(ready)[:160])
    status, st, _ = jcall(base, "/api/status")
    d = data_of(st)
    check(f"{name} /api/status identity", status == 200 and d.get("component") == name
          and "uptime_seconds" in d and d.get("ready") is True, str(d)[:160])
    status, text, ctype = call(base, "/api/metrics")
    families = ["up", "uptime_seconds", "build_info", "config_reloads_total",
                "config_saves_total", "http_requests_total", "alerts_sent_total",
                "log_disk_bytes", "log_disk_limit_bytes"]
    missing = [f for f in families if f"lanstudio_{short}_{f}" not in text]
    check(f"{name} 9 mandatory metric families", status == 200 and not missing
          and "version=0.0.4" in ctype, f"missing={missing} ctype={ctype}")
    status, cfg, _ = jcall(base, "/api/config")
    cd = data_of(cfg)
    check(f"{name} /api/config raw+structured envelope", status == 200
          and f"component: {name}" in cd.get("raw", "")
          and isinstance(cd.get("structured"), dict)
          and all(k in cfg for k in ("ok", "data", "request_id")), str(cfg)[:160])
    status, logs, _ = jcall(base, "/api/logs?limit=10")
    check(f"{name} /api/logs envelope", status == 200
          and isinstance(data_of(logs).get("logs"), list))
    status, alerts, _ = jcall(base, "/api/alerts")
    check(f"{name} /api/alerts envelope", status == 200
          and isinstance(data_of(alerts).get("by_kind"), list))
    status, auth, _ = jcall(base, "/api/auth")
    check(f"{name} /api/auth write protection", status == 200
          and data_of(auth).get("write_protection") is True)
    status, hist, _ = jcall(base, "/api/config/history")
    check(f"{name} /api/config/history", status == 200
          and isinstance(data_of(hist).get("items"), list))
    status, html, _ = call(base, "/")
    forbidden = ["<script src=", "<link rel=\"stylesheet\"", "src=\"http", "url(http"]
    check(f"{name} offline six-area WebUI", status == 200 and "<style" in html
          and 'id="app"' in html and all(x not in html for x in forbidden)
          and "版本历史" in html, html[:120])
    status, denied, _ = jcall(base, "/api/config", "POST", {"text": "version: 1\n"})
    check(f"{name} write guard 409 setup_required", status == 409
          and err_code(denied) == "setup_required", str(denied)[:160])


# ----------------------------------------------------------- lan-sync tests


def sync_business(base: str, fake: SyncFake) -> None:
    # Empty node list, no token field.
    status, body, raw = jcall(base, "/api/sync/nodes")
    d = data_of(body)
    check("sync nodes empty list", status == 200 and d.get("total") == 0
          and isinstance(d.get("items"), list) and not has_cred_fields(raw), raw[:160])

    # Register the fake peer; first node must report total == 1.
    endpoint = f"http://127.0.0.1:{FAKE_NODE_PORT}"
    status, body, _ = jcall(base, "/api/sync/node", "POST",
                            {"id": "n1", "endpoint": endpoint, "note": "fake"}, HDR)
    d = data_of(body)
    check("sync register node n1 -> total 1", status == 200
          and d.get("id") == "n1" and d.get("total") == 1, str(d)[:160])

    # https and non-loopback endpoints are rejected by valid_endpoint().
    status, body, _ = jcall(base, "/api/sync/node", "POST",
                            {"id": "bad-https", "endpoint": "https://127.0.0.1:18099"}, HDR)
    check("sync rejects https endpoint", status == 400
          and err_code(body) == "invalid_config", str(body)[:160])
    status, body, _ = jcall(base, "/api/sync/node", "POST",
                            {"id": "bad-wan", "endpoint": "http://192.168.5.10:8086"}, HDR)
    check("sync rejects non-loopback endpoint", status == 400
          and err_code(body) == "invalid_config", str(body)[:160])

    # Node list must never expose the token field.
    status, body, raw = jcall(base, "/api/sync/nodes")
    d = data_of(body)
    items = d.get("items", [])
    check("sync nodes list exposes no token field", status == 200
          and d.get("total") == 1
          and all("token" not in it for it in items if isinstance(it, dict))
          and not has_cred_fields(raw), raw[:160])

    # Unregistered node -> 404 not_found.
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "ghost", "component": "lan-sync"}, HDR)
    check("sync pull unregistered -> 404 not_found", status == 404
          and err_code(body) == "not_found", str(body)[:160])

    # Register a node whose endpoint has no listener -> 502 target_unreachable.
    jcall(base, "/api/sync/node", "POST",
          {"id": "dead", "endpoint": f"http://127.0.0.1:{DEAD_PORT}"}, HDR)
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "dead", "component": "lan-sync"}, HDR)
    check("sync pull unreachable -> 502 target_unreachable", status == 502
          and err_code(body) == "target_unreachable", str(body)[:160])

    # Successful pull on a fresh baseline -> accepted / no_conflict (HTTP 200).
    fake.mode = "valid"
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "n1", "component": "lan-sync"}, HDR)
    d = data_of(body)
    check("sync pull accepted (HTTP 200, decision field)", status == 200
          and d.get("decision") == "accepted" and "reason" in d
          and d.get("node_id") == "n1" and d.get("component") == "lan-sync"
          and str(d.get("checksum", "")).startswith("sha256:"), str(d)[:200])

    # Older remote snapshot with a newer/equal local baseline -> reject conflict.
    fake.mode = "older"
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "n1", "component": "lan-sync"}, HDR)
    d = data_of(body)
    check("sync reject conflict is HTTP 200 decision=rejected (not 409)",
          status == 200 and d.get("decision") == "rejected" and "reason" in d
          and "checksum" in d, str(d)[:200])

    # Tampered checksum -> 400 (the only decision that is also an HTTP error).
    fake.mode = "tampered"
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "n1", "component": "lan-sync"}, HDR)
    check("sync tampered checksum -> 400", status == 400, str(body)[:160])

    # manual strategy -> pending (HTTP 200), then confirm applies it.
    replace_config(base, [("strategy: reject", "strategy: manual")], "lan-sync")
    fake.mode = "valid"
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "n1", "component": "lan-sync"}, HDR)
    d = data_of(body)
    pending_id = str(d.get("checksum", ""))
    check("sync manual pending is HTTP 200 decision=pending (not 409)",
          status == 200 and d.get("decision") == "pending" and pending_id.startswith("sha256:"),
          str(d)[:200])
    status, body, _ = jcall(base, "/api/sync/confirm", "POST",
                            {"id": pending_id, "accept": True}, HDR)
    d = data_of(body)
    check("sync confirm accept -> decision=accepted applied=true", status == 200
          and d.get("decision") == "accepted" and d.get("applied") is True, str(d)[:160])

    # last_write_wins strategy -> overwritten (HTTP 200).
    replace_config(base, [("strategy: manual", "strategy: last_write_wins")], "lan-sync")
    status, body, _ = jcall(base, "/api/sync/pull", "POST",
                            {"node_id": "n1", "component": "lan-sync"}, HDR)
    d = data_of(body)
    check("sync last_write_wins -> decision=overwritten", status == 200
          and d.get("decision") == "overwritten" and "reason" in d, str(d)[:200])

    # Push posts the standard snapshot to the fake peer.
    status, body, _ = jcall(base, "/api/sync/push", "POST", {"node_id": "n1"}, HDR)
    d = data_of(body)
    check("sync push -> pushed true + checksum + node_id", status == 200
          and d.get("pushed") is True and d.get("node_id") == "n1"
          and str(d.get("checksum", "")).startswith("sha256:"), str(d)[:160])

    # Status carries counters but never the shared_secret value.
    status, body, raw = jcall(base, "/api/sync/status")
    d = data_of(body)
    keys = ["nodes", "strategy", "pulls", "pushes", "conflicts", "audit",
            "pending", "shared_secret_configured"]
    check("sync status fields (no shared_secret value)", status == 200
          and all(k in d for k in keys) and not has_cred_fields(raw)
          and "shared_secret\"" not in raw, raw[:200])

    # History is a bounded, credential-free audit list.
    status, body, raw = jcall(base, "/api/sync/history")
    d = data_of(body)
    check("sync history shape {items,total,limit}", status == 200
          and isinstance(d.get("items"), list) and isinstance(d.get("total"), int)
          and isinstance(d.get("limit"), int) and not has_cred_fields(raw), raw[:160])

    # Remove the node.
    status, body, _ = jcall(base, "/api/sync/remove", "POST", {"id": "n1"}, HDR)
    check("sync remove node", status == 200
          and data_of(body).get("removed") is True, str(body)[:160])


# ----------------------------------------------------------- lan-ssdp tests


def ssdp_devices(base: str):
    status, body, raw = jcall(base, "/api/ssdp/devices")
    return status, data_of(body).get("items", []), raw


def ssdp_business(base: str, desc: DescFake) -> None:
    status, body, _ = jcall(base, "/api/ssdp/snapshot")
    d = data_of(body)
    check("ssdp snapshot shape", status == 200
          and all(k in d for k in ("items", "total", "max_devices", "generated_at",
                                   "fetch_descriptions"))
          and d.get("fetch_descriptions") is False, str(d)[:160])

    status, body, _ = jcall(base, "/api/ssdp/status")
    d = data_of(body)
    check("ssdp status shape", status == 200
          and all(k in d for k in ("devices", "expired", "errors", "probes",
                                   "max_age_seconds", "max_devices",
                                   "fetch_descriptions"))
          and d.get("fetch_descriptions") is False, str(d)[:160])

    # Unicast isolation: refresh returns 200 with sent == 0.
    status, body, _ = jcall(base, "/api/ssdp/refresh", "POST", {}, HDR)
    d = data_of(body)
    check("ssdp refresh unicast isolation -> started true, sent 0", status == 200
          and d.get("started") is True and d.get("sent") == 0, str(d)[:160])

    alive = (
        "NOTIFY * HTTP/1.1\r\n"
        f"HOST: 127.0.0.1:{SSDP_UDP_PORT}\r\n"
        "NT: urn:schemas-upnp-org:device:MediaServer:1\r\n"
        "NTS: ssdp:alive\r\n"
        "USN: uuid:m5-test-1\r\n"
        "SERVER: M5-Acceptance/1.0 UPnP/1.0\r\n"
        "CACHE-CONTROL: max-age=10\r\n"
        f"LOCATION: http://127.0.0.1:{SSDP_DESC_PORT}/desc.xml\r\n"
        "\r\n"
    )
    byebye = (
        "NOTIFY * HTTP/1.1\r\n"
        f"HOST: 127.0.0.1:{SSDP_UDP_PORT}\r\n"
        "NT: urn:schemas-upnp-org:device:MediaServer:1\r\n"
        "NTS: ssdp:byebye\r\n"
        "USN: uuid:m5-test-1\r\n"
        "\r\n"
    )

    def same_usn(it):
        if not isinstance(it, dict):
            return False
        attrs = it.get("attributes") if isinstance(it.get("attributes"), dict) else {}
        return str(attrs.get("usn", "")) == "uuid:m5-test-1"

    found = [None]

    def find_alive():
        _, items, _ = ssdp_devices(base)
        for it in items:
            if same_usn(it):
                found[0] = it
                return True
        return False

    send_ssdp(alive)
    check("ssdp alive NOTIFY registers a device", poll_until(find_alive, 3.0),
          "device uuid:m5-test-1 never appeared within 3s")

    rec = found[0] if isinstance(found[0], dict) else {}
    ok, why = validate_record(rec, "ssdp")
    check("ssdp ServiceRecord conforms to service-record.json", ok, why)
    attrs = rec.get("attributes") if isinstance(rec.get("attributes"), dict) else {}
    check("ssdp records LOCATION as a bounded attribute", "location" in attrs
          and "/desc.xml" in str(attrs.get("location", "")), str(attrs)[:160])
    check("ssdp never issues an HTTP GET for LOCATION", desc.gets == 0,
          f"fake description server received {desc.gets} GET(s)")

    # Re-announce to refresh the TTL window, then byebye must remove it.
    send_ssdp(alive)
    time.sleep(0.2)

    def gone():
        _, items, _ = ssdp_devices(base)
        return not any(same_usn(it) for it in items)

    send_ssdp(byebye)
    check("ssdp byebye removes the device", poll_until(gone, 3.0),
          "device uuid:m5-test-1 still present after byebye")


# ---------------------------------------------------------- lan-shell tests


def shell_manifest_text() -> str:
    text = (ROOT / "shell" / "components.yaml").read_text(encoding="utf-8")
    return text.replace("probe_enabled: true", "probe_enabled: false")


def shell_offline_check(workdir: Path) -> None:
    manifest = workdir / "components.yaml"
    manifest.write_text(shell_manifest_text(), encoding="utf-8")
    env = os.environ.copy()
    env["LAN_SHELL_CONFIG"] = str(manifest)
    env["LAN_STUDIO_PORT"] = str(PORT_SHELL)
    env.pop("LAN_SHELL_TOKEN", None)
    proc = subprocess.run([str(binary("lan-shell")), "--check"], cwd=str(workdir),
                          env=env, capture_output=True, text=True, timeout=20)
    out = proc.stdout + proc.stderr
    check("lan-shell --check offline validation", proc.returncode == 0
          and '"ok":true' in proc.stdout, out)
    listening = False
    try:
        conn = socket.create_connection(("127.0.0.1", PORT_SHELL), timeout=0.5)
        conn.close()
        listening = True
    except OSError:
        pass
    check("lan-shell --check does not listen on a port", not listening,
          f"port {PORT_SHELL} is listening after --check")


def start_shell(workdir: Path) -> str:
    manifest = workdir / "components.yaml"
    manifest.write_text(shell_manifest_text(), encoding="utf-8")
    env = os.environ.copy()
    env["LAN_SHELL_CONFIG"] = str(manifest)
    env["LAN_STUDIO_PORT"] = str(PORT_SHELL)
    env.pop("LAN_SHELL_TOKEN", None)
    proc = subprocess.Popen([str(binary("lan-shell"))], cwd=str(workdir), env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    PROCS.append(proc)
    base = f"http://127.0.0.1:{PORT_SHELL}"
    for _ in range(80):
        try:
            status, _, _ = call(base, "/healthz")
            if status == 200:
                return base
        except Exception:
            pass
        if proc.poll() is not None:
            raise RuntimeError(f"lan-shell exited with {proc.returncode}")
        time.sleep(0.15)
    raise RuntimeError(f"lan-shell did not become healthy on {base}")


def shell_checks(base: str) -> None:
    status, health, raw = jcall(base, "/healthz")
    check("shell /healthz is bare {ok:true} (not an envelope)", status == 200
          and health == {"ok": True} and "data" not in health
          and "request_id" not in health, raw[:160])

    status, body, raw = jcall(base, "/api/shell/status")
    d = data_of(body)
    comps = d.get("components", [])
    env_ok = status == 200 and all(k in body for k in ("ok", "data", "request_id"))
    shape_ok = (isinstance(comps, list) and d.get("total") == 8 and len(comps) == 8
                and isinstance(d.get("online"), int)
                and isinstance(d.get("probe_enabled"), bool)
                and d.get("token_hint") is False)
    fields_ok = all(isinstance(c, dict) and all(k in c for k in
                    ("name", "label", "port", "icon", "tone", "online", "checked_at"))
                    for c in comps)
    online_ok = all((c.get("online") is None or isinstance(c.get("online"), bool))
                    for c in comps if isinstance(c, dict))
    check("shell /api/shell/status envelope + 8 components", env_ok and shape_ok
          and fields_ok and online_ok, raw[:220])
    leaked = has_cred_fields(raw)
    check("shell /api/shell/status has no credential field names", not leaked,
          f"leaked={leaked}")

    status, html, ctype = call(base, "/")
    forbidden = ["<script src=", "src=\"http", "url(http",
                 "<link rel=\"stylesheet\"", "type=\"password\""]
    names_ok = all(n in html for n in COMPONENTS)
    check("shell / self-contained single-file WebUI", status == 200
          and "text/html" in ctype and names_ok
          and all(x not in html for x in forbidden), html[:160])
    theme_kw = any(k in html.lower() for k in ("theme", "主题", "切换", "dark"))
    check("shell WebUI contains a theme-toggle keyword", theme_kw, html[:160])
    tones = ["blue", "cyan", "purple", "teal", "indigo", "amber", "green", "orange"]
    check("shell WebUI has 8 semantic tone color bars",
          all(t in html.lower() for t in tones), html[:160])

    status, _, _ = jcall(base, "/api/records")
    check("shell is not a reverse proxy (/api/records -> 404)", status == 404, str(status))
    status, _, _ = jcall(base, "/api/status")
    check("shell has no unified /api/status (no 18 standard endpoints)",
          status == 404, str(status))


# ----------------------------------------------------------- lan-mdns tests


def mdns_unified(workdir: Path) -> None:
    names = [
        "lan-mdns /healthz reachable",
        "lan-mdns /api/browse/unified envelope + source=mdns",
        "lan-mdns unified ServiceRecord conforms to service-record.json",
    ]
    try:
        base = start("lan-mdns", PORT_MDNS, workdir, extra_env={
            "LAN_MDNS_PORT": MDNS_UDP_PORT,
            "LAN_MDNS_GROUP": MDNS_GROUP,
            "LAN_STUDIO_AUTH_FILE": str(workdir / "auth.toml"),
            "LAN_STUDIO_LOG_DIR": str(workdir / "logs"),
            "LAN_STUDIO_HISTORY_DIR": str(workdir / "history"),
        })
    except Exception as exc:  # start failure/timeout must be a recorded failure
        for name in names:
            check(name, False, f"lan-mdns start failed: {type(exc).__name__}: {exc}")
        return
    status, health, _ = jcall(base, "/healthz")
    check(names[0], status == 200 and health == {"ok": True}, str(health))
    status, body, raw = jcall(base, "/api/browse/unified")
    d = data_of(body)
    items = d.get("items", [])
    env_ok = status == 200 and all(k in body for k in ("ok", "data", "request_id"))
    check(names[1], env_ok and d.get("source") == "mdns"
          and isinstance(items, list) and d.get("total") == len(items), raw[:220])
    problems = []
    for it in items:
        ok, why = validate_record(it, "mdns")
        if not ok:
            problems.append(why)
    check(names[2], not problems and all(isinstance(i, dict) for i in items),
          f"items={len(items)} problems={problems[:3]}")


# ------------------------------------------------- M5-specific assertions


def no_overlap() -> None:
    sync_src = "\n".join(p.read_text(encoding="utf-8")
                         for p in (ROOT / "components" / "lan-sync" / "src").glob("*.rs"))
    ssdp_src = "\n".join(p.read_text(encoding="utf-8")
                         for p in (ROOT / "components" / "lan-ssdp" / "src").glob("*.rs"))
    ssdp_code = strip_comments(ssdp_src)
    proprietary = ["/api/relay/", "/api/ca/", "/api/scan/", "/api/hosts/",
                   "/api/wol/", "/api/ssdp/"]
    check("no_overlap: lan-sync calls no component-specific endpoint",
          all(p not in sync_src for p in proprietary),
          str([p for p in proprietary if p in sync_src]))
    forbidden = ["TcpStream", "soap", "SOAP", "reqwest", "hyper", "HttpClient"]
    hits = [f for f in forbidden if f in ssdp_code]
    check("no_overlap: lan-ssdp has no TCP/SOAP/HTTP-client to LOCATION",
          not hits, f"hits={hits}")
    for name in ("lan-sync", "lan-ssdp"):
        cargo = (ROOT / "components" / name / "Cargo.toml").read_text(encoding="utf-8")
        check(f"no_overlap: {name} Cargo.toml has no components/lan-* dependency",
              "components/lan-" not in cargo, cargo[:160])


def no_password_echo(sync_base: str, shell_base: str) -> None:
    for label, base, path in (
        ("sync /api/sync/nodes", sync_base, "/api/sync/nodes"),
        ("sync /api/sync/status", sync_base, "/api/sync/status"),
        ("sync /api/sync/history", sync_base, "/api/sync/history"),
        ("shell /api/shell/status", shell_base, "/api/shell/status"),
    ):
        status, _, raw = jcall(base, path)
        leaked = has_cred_fields(raw)
        check(f"no_password_echo: {label}", status == 200 and not leaked,
              f"status={status} leaked={leaked}")


def safe_phase(label: str, fn) -> None:
    try:
        fn()
    except Exception as exc:  # noqa: BLE001 - gate must report, not hide
        check(label, False, f"{type(exc).__name__}: {exc}")


def main() -> int:
    global BIN_DIR
    if ((ROOT / "target" / "release" / "lan-sync").exists()
            and (ROOT / "target" / "release" / "lan-ssdp").exists()):
        BIN_DIR = ROOT / "target" / "release"
    tmp = tempfile.TemporaryDirectory(prefix="lan-studio-m5-")
    root = Path(tmp.name)
    sync_fake = None
    desc = None
    sync_base = None
    shell_base = None
    try:
        print(f"M5 acceptance workspace: {root}")
        offline = root / "offline"
        offline.mkdir()
        dirs = {}
        for name in ("lan-sync", "lan-ssdp", "lan-shell", "lan-mdns"):
            dirs[name] = root / name
            dirs[name].mkdir()

        isolated = isolated_ssdp_config()

        # ---- offline / static phase (no long-running processes) ----
        safe_phase("no_overlap phase", no_overlap)
        safe_phase("lan-sync offline check", lambda: offline_check("lan-sync", offline))
        safe_phase("lan-ssdp offline check", lambda: offline_check("lan-ssdp", offline))
        check("ssdp isolation config replacements applied",
              'group: "127.0.0.1"' in isolated and f"port: {SSDP_UDP_PORT}" in isolated
              and "max_age_seconds: 10" in isolated, isolated[:160])
        safe_phase("ssdp TTL validate", lambda: offline_check(
            "lan-ssdp", offline, config_text=isolated,
            label="ssdp validate accepts max_age_seconds=10 (TTL) / unicast isolation"))
        safe_phase("lan-mdns offline check", lambda: mdns_offline_check(offline))
        safe_phase("lan-shell offline check", lambda: shell_offline_check(dirs["lan-shell"]))

        # ---- fake servers ----
        sync_fake = SyncFake(FAKE_NODE_PORT)
        sync_fake.start()
        desc = DescFake(SSDP_DESC_PORT)
        desc.start()

        # ---- lan-sync runtime ----
        def sync_phase():
            nonlocal sync_base
            sync_base = start("lan-sync", PORT_SYNC, dirs["lan-sync"])
            print(f"  lan-sync ready on {sync_base}")
            unified_contract("lan-sync", sync_base)
            setup_password(sync_base, "lan-sync")
            config_lifecycle(sync_base, "lan-sync")
            sync_business(sync_base, sync_fake)
        safe_phase("lan-sync runtime phase", sync_phase)

        # ---- lan-ssdp runtime (run the delicate UDP test before config churn) ----
        def ssdp_phase():
            base = start("lan-ssdp", PORT_SSDP, dirs["lan-ssdp"], config_text=isolated)
            print(f"  lan-ssdp ready on {base}")
            unified_contract("lan-ssdp", base)
            setup_password(base, "lan-ssdp")
            ssdp_business(base, desc)
            config_lifecycle(base, "lan-ssdp")
        safe_phase("lan-ssdp runtime phase", ssdp_phase)

        # ---- lan-shell runtime ----
        def shell_phase():
            nonlocal shell_base
            shell_base = start_shell(dirs["lan-shell"])
            print(f"  lan-shell ready on {shell_base}")
            shell_checks(shell_base)
        safe_phase("lan-shell runtime phase", shell_phase)

        # ---- lan-mdns unified (best effort) ----
        safe_phase("lan-mdns unified phase", lambda: mdns_unified(dirs["lan-mdns"]))

        # ---- aggregated credential echo ----
        if sync_base and shell_base:
            safe_phase("no_password_echo phase",
                       lambda: no_password_echo(sync_base, shell_base))
    except Exception as exc:  # noqa: BLE001 - gate must report, not hide
        check("M5 acceptance harness", False, f"{type(exc).__name__}: {exc}")
    finally:
        if sync_fake:
            sync_fake.stop()
        if desc:
            desc.stop()
        terminate_all()
        tmp.cleanup()

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    failed = [n for n, ok, _ in RESULTS if not ok]
    print(f"\nM5 RUNTIME GATE: {passed} passed / {len(failed)} failed")
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("M5 RUNTIME OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
