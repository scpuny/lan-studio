#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""F-J03 · Runtime conformance test for the unified component API.

Contract authority: ``contracts/openapi.yaml`` (PRD 3.2) plus
``docs/ERROR-CODES.md``. ``scripts/verify_contracts.py`` only proves the
*documents* agree with each other; this script proves the *running binary*
agrees with the documents. The two are deliberately separate gates.

Zero third-party dependency. PyYAML is optional and only used to cross-check
the built-in OpenAPI scanner, so CI can run the same script with or without
it installed.

What is asserted
----------------
1. Operation inventory — every ``(method, path)`` in ``openapi.yaml`` is
   parsed, and the scanner is cross-checked against PyYAML when available.
2. Per-operation conformance — every documented operation is actually called
   once on the happy path, and the observed status code must be one the
   contract declares *for that operation*. No blanket "200 or 400" allowed.
3. Unified envelope — success is always ``{ok:true,data,request_id}``, errors
   are always ``{ok:false,error:{code,message},request_id}``, and
   ``X-Request-Id`` is echoed so operators can correlate logs.
4. Auth matrix — the six rows of ERROR-CODES §3, plus the exemption of the
   two probe endpoints and the fact that read routes stay readable while the
   admin password is set.
5. Error-code closure — every code observed at runtime belongs to the
   documented 8-value enum, the five API-reachable codes are all produced,
   and no error body leaks credentials, stack traces or filesystem paths.
6. Documented-vs-implemented drift — an operation the contract promises must
   not answer 404, and a route the binary serves must be documented.

Environment (all optional)
--------------------------
LAN_MDNS_BIN       path to the binary   (default target/release/lan-mdns)
LAN_STUDIO_CONFIG  config template      (default components/lan-mdns/config.template.yaml)
LAN_CONTRACT_MDNS_PORT / LAN_CONTRACT_MDNS_GROUP
                   isolated multicast coordinates used by the child process

Usage: python3 scripts/test_api_contract.py
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACTS = ROOT / "contracts"
OPENAPI = CONTRACTS / "openapi.yaml"
BIN = ROOT / os.getenv("LAN_MDNS_BIN", "target/release/lan-mdns")
TEMPLATE = ROOT / os.getenv(
    "LAN_STUDIO_CONFIG", "components/lan-mdns/config.template.yaml"
)
MDNS_PORT = os.getenv("LAN_CONTRACT_MDNS_PORT", "25353")
MDNS_GROUP = os.getenv("LAN_CONTRACT_MDNS_GROUP", "239.1.2.4")

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))

RESULTS: list[tuple[str, bool, str]] = []

# The eight codes of contracts/openapi.yaml -> ErrorCode.
ERROR_CODES = [
    "invalid_config",
    "auth_required",
    "auth_failed",
    "setup_required",
    "not_found",
    "target_unreachable",
    "rate_limited",
    "internal_error",
]

# Codes this component can actually emit on an /api/* route. The remaining
# three are documented below in CODE_NOTES so their absence is a recorded
# decision rather than a silent gap.
CODE_RUNTIME = ["invalid_config", "auth_required", "auth_failed",
                "setup_required", "not_found"]

CODE_NOTES = {
    "internal_error": (
        "only reachable when runtime state is absent (startup window) or a "
        "save fails after validation; covered by common-api unit tests"
    ),
    "target_unreachable": (
        "gateway-only: ERROR-CODES §2 assigns it to the friendly 503 page on "
        "the Host gateway, which is HTML not JSON; covered by smoke_mdns.py"
    ),
    "rate_limited": (
        "reserved vocabulary for components that throttle (lan-scan/lan-relay); "
        "lan-mdns registers no throttled route, so it never emits it"
    ),
}

CREDENTIAL_MARKERS = [
    "panicked at",
    "RUST_BACKTRACE",
    "stack backtrace",
    "note: run with",
]

# Literal secrets minted by this run; none of them may ever be echoed back.
SECRET_VALUES = ["contract-pass-1", "contract-pass-2", "contract-token"]

# Codes actually produced by the run, filled in by `assert_error_envelope`.
OBSERVED_CODES: set[str] = set()


def scan_leaks(raw: str, extra: list[str]) -> list[str]:
    """Report credential echoes, stack traces or internal paths in a body."""
    found = []
    for marker in CREDENTIAL_MARKERS:
        if marker.lower() in raw.lower():
            found.append(marker)
    for secret in SECRET_VALUES:
        if secret in raw:
            found.append("secret:{}".format(secret))
    for path in extra:
        if path and path in raw:
            found.append("path:{}".format(path))
    return found


# ----------------------------------------------------------------- reporting


def check(name: str, ok: bool, detail: str = "") -> bool:
    RESULTS.append((name, bool(ok), detail))
    flag = "PASS" if ok else "FAIL"
    print("{:03d} {}  {}{}".format(
        len(RESULTS), flag, name, ("  | " + detail) if detail else ""))
    return bool(ok)


def section(title: str) -> None:
    print("\n=== {} ===".format(title))


# ---------------------------------------------------------- OpenAPI scanning

METHODS = {"get", "post", "put", "patch", "delete", "head", "options"}


class Operation:
    """One documented (method, path) pair plus the parts of the contract that
    the runtime test needs: whether it is auth-exempt and which status codes
    are declared for it."""

    __slots__ = ("path", "method", "exempt", "codes", "envelope")

    def __init__(self, path: str, method: str) -> None:
        self.path = path
        self.method = method
        self.exempt = False
        self.codes: set[int] = set()
        self.envelope = False

    @property
    def key(self) -> tuple[str, str]:
        return (self.method, self.path)

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return "{} {} {}".format(self.method, self.path, sorted(self.codes))


STATUS_LINE = re.compile(r'^"(\d{3})":$')


def parse_openapi(text: str) -> list[Operation]:
    """Extract operations from the ``paths:`` block without a YAML library.

    The scanner is deliberately narrow instead of clever: it relies on the
    fixed two-space indentation of this document and stops tracking as soon as
    a new top-level key appears. Anything it cannot classify is reported by the
    PyYAML cross-check rather than guessed at.
    """
    ops: list[Operation] = []
    current: Operation | None = None
    path_key: str | None = None
    in_paths = False

    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if not line.startswith(" "):
            in_paths = line.split(":", 1)[0].strip() == "paths"
            path_key, current = None, None
            continue
        if not in_paths:
            continue

        indent = len(line) - len(line.lstrip(" "))
        if indent == 2 and stripped.endswith(":"):
            path_key, current = stripped[:-1].strip(), None
        elif indent == 4 and stripped.endswith(":") and stripped[:-1].strip() in METHODS:
            if path_key is None:
                continue
            current = Operation(path_key, stripped[:-1].strip().upper())
            ops.append(current)
        elif current is not None and indent >= 6:
            if stripped == "security: []":
                current.exempt = True
            match = STATUS_LINE.match(stripped)
            if match:
                current.codes.add(int(match.group(1)))
            if "schemas/ApiOk" in stripped:
                current.envelope = True
    return ops


def yaml_operations() -> list[tuple[str, str]] | None:
    """Independent extraction via PyYAML; ``None`` when PyYAML is absent."""
    try:
        import yaml  # type: ignore
    except ImportError:
        return None
    try:
        doc = yaml.safe_load(OPENAPI.read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001 - a broken contract is reported elsewhere
        return None
    pairs: list[tuple[str, str]] = []
    for path, item in (doc.get("paths") or {}).items():
        for method in item:
            if str(method).lower() in METHODS:
                pairs.append((str(method).upper(), str(path)))
    return pairs


# ------------------------------------------------------------------- runtime


class Client:
    """HTTP client that records every response under (method, path, tag)."""

    def __init__(self, base: str) -> None:
        self.base = base
        self.token: str | None = None
        self.password: str | None = None
        self.seen: dict[tuple[str, str, str], tuple[int, dict, str]] = {}

    def call(self, method: str, path: str, body=None, tag: str = "happy",
             token: str | None = None, password: str | None = None,
             headers: dict | None = None, record: bool = True):
        hdrs = dict(headers or {})
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            hdrs["Content-Type"] = "application/json"
        use_token = self.token if token is None else token
        use_password = self.password if password is None else password
        if use_token:
            hdrs["X-Admin-Token"] = use_token
        if use_password:
            hdrs["X-Admin-Password"] = use_password
        request = urllib.request.Request(
            self.base + path, data=data, headers=hdrs, method=method)
        try:
            with OPENER.open(request, timeout=8) as response:
                result = (response.status, dict(response.headers),
                          response.read().decode(errors="replace"))
        except urllib.error.HTTPError as exc:
            result = (exc.code, dict(exc.headers),
                      exc.read().decode(errors="replace"))
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            result = (0, {}, "transport: {}".format(exc))
        if record:
            self.seen.setdefault((method, path, tag), result)
        return result


def free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


class Instance:
    """One isolated child process with its own config, auth digest, log and
    history directory, so a run never touches the repository or another
    instance's state."""

    def __init__(self, name: str, config_text: str) -> None:
        self.name = name
        self.config_text = config_text
        self.workdir = tempfile.mkdtemp(prefix="lanstudio-contract-")
        self.config_path = os.path.join(self.workdir, "config.yaml")
        self.port = free_port()
        self.proc = None
        self.log = None

    @property
    def base(self) -> str:
        return "http://127.0.0.1:{}".format(self.port)

    def __enter__(self) -> "Instance":
        with open(self.config_path, "w", encoding="utf-8") as handle:
            handle.write(self.config_text)
        env = dict(os.environ)
        env.update({
            "LAN_STUDIO_CONFIG": self.config_path,
            "LAN_STUDIO_AUTH_FILE": os.path.join(self.workdir, "auth.toml"),
            "LAN_STUDIO_LOG_DIR": os.path.join(self.workdir, "logs"),
            "LAN_STUDIO_HISTORY_DIR": os.path.join(self.workdir, "history"),
            "LAN_STUDIO_PORT": str(self.port),
            "LAN_MDNS_PORT": MDNS_PORT,
            "LAN_MDNS_GROUP": MDNS_GROUP,
        })
        self.log = tempfile.NamedTemporaryFile(
            mode="w+", prefix="lanstudio-contract-", suffix=".log", delete=False)
        self.proc = subprocess.Popen(
            [str(BIN)], cwd=str(ROOT), env=env, stdout=self.log, stderr=self.log)
        return self

    def wait_ready(self, seconds: float = 20.0) -> bool:
        deadline = time.time() + seconds
        while time.time() < deadline:
            if self.proc.poll() is not None:
                return False
            status, _, raw = Client(self.base).call(
                "GET", "/healthz", record=False)
            if status == 200 and raw.strip() == '{"ok":true}':
                return True
            time.sleep(0.2)
        return False

    def __exit__(self, *_exc) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        if self.log:
            self.log.close()
            try:
                os.unlink(self.log.name)
            except OSError:
                pass
        shutil.rmtree(self.workdir, ignore_errors=True)


def sse_handshake(port: int, path: str = "/api/logs/stream",
                  timeout: float = 2.0) -> tuple[int, str, str]:
    """Read the SSE response head plus the first frames over a raw socket.

    ``urllib`` blocks until the connection closes, which an open event stream
    never does, so the handshake is done by hand. Returns (status, headers,
    payload) using the raw HTTP response text.
    """
    sock = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    sock.settimeout(timeout)
    sock.sendall(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\n"
        "Accept: text/event-stream\r\nConnection: close\r\n\r\n".format(
            path).encode())
    out = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            chunk = sock.recv(4096)
        except (socket.timeout, OSError):
            break
        if not chunk:
            break
        out += chunk
        if b"data:" in out and b": keepalive" in out:
            break
    sock.close()
    text = out.decode(errors="replace")
    head, _, payload = text.partition("\r\n\r\n")
    status = 0
    first = head.splitlines()[0] if head else ""
    parts = first.split()
    if len(parts) >= 2 and parts[1].isdigit():
        status = int(parts[1])
    return status, head, payload


# --------------------------------------------------------------- assertions


def envelope_of(raw: str) -> dict | None:
    try:
        value = json.loads(raw)
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def assert_success_envelope(label: str, status: int, raw: str) -> dict | None:
    body = envelope_of(raw)
    if body is None:
        return check(label, False, "body is not a JSON object: {!r}".format(raw[:80]))
    ok = (body.get("ok") is True
          and isinstance(body.get("data"), (dict, list))
          and isinstance(body.get("request_id"), str)
          and len(body["request_id"]) >= 6
          and "error" not in body)
    return check(label, ok, "" if ok else "unexpected envelope: {}".format(
        json.dumps(body)[:120]))


def assert_error_envelope(label: str, status: int, raw: str,
                          expected_code: str | None = None,
                          expected_status: int | None = None) -> None:
    body = envelope_of(raw)
    if body is None:
        check(label, False, "body is not a JSON object: {!r}".format(raw[:80]))
        return
    error = body.get("error") or {}
    code = error.get("code")
    if isinstance(code, str):
        OBSERVED_CODES.add(code)
    problems = []
    if body.get("ok") is not False:
        problems.append("ok != false")
    if code not in ERROR_CODES:
        problems.append("code {!r} outside enum".format(code))
    if not isinstance(error.get("message"), str) or not error["message"]:
        problems.append("message missing")
    if not isinstance(body.get("request_id"), str) or len(body["request_id"]) < 6:
        problems.append("request_id missing")
    if "data" in body:
        problems.append("error body must not carry data")
    if expected_code and code != expected_code:
        problems.append("expected code {!r}".format(expected_code))
    if expected_status is not None and status != expected_status:
        problems.append("expected HTTP {}".format(expected_status))
    check(label, not problems, "; ".join(problems))


# ---------------------------------------------------------------------- main


def main() -> int:
    if not BIN.exists():
        print("missing binary: {}\nrun: cargo build --release -p lan-mdns".format(BIN))
        return 2
    if not TEMPLATE.exists():
        print("missing config template: {}".format(TEMPLATE))
        return 2
    config_text = TEMPLATE.read_text(encoding="utf-8")
    openapi_text = OPENAPI.read_text(encoding="utf-8")

    # ---------------------------------------------------------- 1 · inventory
    section("1 · 契约操作清单")
    ops = parse_openapi(openapi_text)
    by_key = {op.key: op for op in ops}
    check("openapi paths block parses", bool(ops), "{} operations".format(len(ops)))
    # M3/M4/M5 append component-specific paths to the same document. This gate
    # owns the M2 surface only: assert its full inventory is still documented,
    # rather than pinning the total path count (which grows every milestone).
    m2_paths = {
        "/healthz", "/readyz", "/api/status", "/api/metrics", "/api/config",
        "/api/config/snapshot", "/api/config/history", "/api/config/rollback",
        "/api/reload", "/api/logs", "/api/logs/stream", "/api/alerts",
        "/api/auth", "/api/auth/password", "/api/auth/verify",
        "/api/records", "/api/probe", "/api/interfaces",
        "/api/health/targets", "/api/queries", "/api/browse",
        "/api/browse/unified", "/api/conflicts",
    }
    documented = {op.path for op in ops}
    check("all 23 M2 paths remain documented in openapi",
          m2_paths <= documented, str(sorted(m2_paths - documented))[:160])
    # Endpoints owned by the other 7 components are covered by their own
    # runtime gates (test_m3/m4/m5.py); this binary cannot serve them.
    component_prefixes = (
        "/api/relay/", "/api/ca/", "/api/scan/", "/api/hosts/",
        "/api/wol/", "/api/sync/", "/api/ssdp/", "/api/shell/",
    )

    referenced = yaml_operations()
    if referenced is None:
        print("SKIP  PyYAML cross-check (not installed; scanner is authoritative)")
    else:
        scanned = sorted(op.key for op in ops)
        check("scanner matches PyYAML", scanned == sorted(referenced),
              "" if scanned == sorted(referenced) else
              "scanned {} vs yaml {}".format(scanned, sorted(referenced)))

    exempt = sorted(op.path for op in ops if op.exempt)
    check("exactly /healthz and /readyz are auth-exempt",
          exempt == ["/healthz", "/readyz"], str(exempt))

    # ------------------------------------------------- 2 · happy-path traffic
    section("2 · 端点运行态一致性")
    with Instance("primary", config_text) as primary:
        if not primary.wait_ready():
            check("primary instance becomes ready", False,
                  "see /healthz before timeout")
            return 1
        client = Client(primary.base)

        status, headers, raw = client.call("GET", "/healthz")
        check("GET /healthz returns {\"ok\":true} only",
              status == 200 and raw.strip() == '{"ok":true}', raw[:60])

        status, headers, raw = client.call("GET", "/api/metrics")
        metric_body = raw
        check("GET /api/metrics is Prometheus text", status == 200
              and "text/plain" in headers.get("Content-Type", ""),
              headers.get("Content-Type", ""))
        # `<c>` in METRICS.md is the component suffix, i.e. `mdns` for lan-mdns.
        check("metrics carry the mandatory lanstudio_mdns_* families",
              all(family in metric_body for family in (
                  "lanstudio_mdns_up",
                  "lanstudio_mdns_uptime_seconds",
                  "lanstudio_mdns_build_info",
                  "lanstudio_mdns_http_requests_total",
                  "lanstudio_mdns_config_saves_total",
                  "lanstudio_mdns_config_reloads_total",
                  "lanstudio_mdns_alerts_sent_total",
                  "lanstudio_mdns_log_disk_bytes",
                  "lanstudio_mdns_log_disk_limit_bytes",
              )), "")

        sse_status, sse_head, sse_payload = sse_handshake(primary.port)
        client.seen[("GET", "/api/logs/stream", "happy")] = (
            sse_status, {"Content-Type": "text/event-stream"}, sse_payload)
        check("GET /api/logs/stream is text/event-stream", sse_status == 200
              and "text/event-stream" in sse_head,
              sse_head.splitlines()[0] if sse_head else "no response")
        check("SSE stream replays framed data records", "data:" in sse_payload,
              sse_payload[:60].replace("\n", " "))

        for path in ("/readyz", "/api/status", "/api/config", "/api/logs",
                     "/api/alerts", "/api/auth", "/api/records",
                     "/api/interfaces", "/api/queries", "/api/browse",
                     "/api/conflicts", "/api/health/targets",
                     "/api/config/history"):
            client.call("GET", path)

        status, _, raw = client.call("GET", "/api/config/snapshot")
        live_snapshot = envelope_of(raw) or {}
        snapshot_data = live_snapshot.get("data") or {}
        check("GET /api/config/snapshot yields a Schema-1 snapshot",
              snapshot_data.get("schema") == 1
              and snapshot_data.get("component") == "lan-mdns"
              and str(snapshot_data.get("checksum", "")).startswith("sha256:"),
              str(sorted(snapshot_data))[:120])

        # QU unicast probe: binds an ephemeral port instead of re-binding the
        # multicast socket, so the gate is portable across hosts where the
        # multicast stack is restricted (e.g. macOS / containers). The M2
        # implementation accepts the `"true"` string form for this flag.
        client.call("POST", "/api/probe",
                    {"name": "nas.local", "type": "A", "unicast": "true"})
        # M5 unified service-record view: same envelope, source fixed to mdns.
        client.call("GET", "/api/browse/unified")

        # A write before any password exists must hit the setup flow.
        for path, body in (("/api/config", {"text": config_text}),
                           ("/api/config/snapshot", snapshot_data),
                           ("/api/config/rollback", {"id": "config-19700101T000000Z.json"}),
                           ("/api/reload", {})):
            client.call("POST", path, body, tag="guard")

        # First-time password setup unlocks the write surface.
        status, _, raw = client.call(
            "POST", "/api/auth/password", {"new_password": "contract-pass-1"})
        assert_success_envelope("POST /api/auth/password sets the first password",
                                status, raw)
        client.password = "contract-pass-1"

        status, _, raw = client.call(
            "POST", "/api/auth/verify", {"password": "contract-pass-1"})
        verify_body = envelope_of(raw) or {}
        check("POST /api/auth/verify confirms the password",
              status == 200 and (verify_body.get("data") or {}).get("valid") is True,
              raw[:80])

        status, _, raw = client.call(
            "POST", "/api/config", {"text": config_text, "mode": "raw"})
        save_body = envelope_of(raw) or {}
        check("POST /api/config saves the live config back",
              status == 200 and (save_body.get("data") or {}).get("saved") is True,
              raw[:100])

        status, _, raw = client.call("GET", "/api/config/history")
        history = (envelope_of(raw) or {}).get("data") or {}
        entries = history.get("history") or []
        check("history lists the snapshot just written",
              status == 200 and bool(entries),
              "total={}".format(history.get("total")))

        snapshot_id = entries[0].get("id") if entries else ""
        status, _, raw = client.call(
            "POST", "/api/config/rollback", {"id": snapshot_id})
        rollback_body = envelope_of(raw) or {}
        check("POST /api/config/rollback restores a listed snapshot",
              status == 200
              and (rollback_body.get("data") or {}).get("rolled_back") is True,
              raw[:100])

        status, _, raw = client.call("POST", "/api/config/snapshot", snapshot_data)
        import_body = envelope_of(raw) or {}
        check("POST /api/config/snapshot imports a snapshot",
              status == 200
              and (import_body.get("data") or {}).get("imported") is True,
              raw[:100])

        status, _, raw = client.call("POST", "/api/reload", {})
        reload_body = envelope_of(raw) or {}
        check("POST /api/reload reloads from disk",
              status == 200
              and (reload_body.get("data") or {}).get("reloaded") is True,
              raw[:100])

        # Per-operation conformance: the observed 2xx must be one the contract
        # declares for that specific operation.
        section("3 · 逐操作状态码")
        for op in sorted(ops, key=lambda item: (item.path, item.method)):
            if op.path.startswith(component_prefixes):
                continue
            observed = client.seen.get((op.method, op.path, "happy"))
            label = "{} {} conforms".format(op.method, op.path)
            if observed is None:
                check(label, False, "operation never exercised")
                continue
            status, _headers, raw = observed
            problems = []
            if status == 0:
                problems.append("no HTTP response")
            elif status not in op.codes:
                problems.append("HTTP {} not declared {}".format(
                    status, sorted(op.codes)))
            elif not 200 <= status < 300:
                problems.append("happy path returned {}".format(status))
            if not problems and op.envelope:
                if (envelope_of(raw) or {}).get("ok") is not True:
                    problems.append("missing ApiOk envelope")
            if not problems and not op.envelope:
                # Non-envelope operations: metrics (text), stream (SSE),
                # /healthz (bare object). They must still not be an error
                # wrapper, which is what a regression would look like.
                if (envelope_of(raw) or {}).get("ok") is False:
                    problems.append("non-envelope route returned ApiError")
            check(label, not problems, "; ".join(problems))

        # ------------------------------------------- 4 · error code closure
        section("4 · 错误码封闭性与鉴权矩阵")
        status, _, raw = client.call("GET", "/api/does-not-exist", tag="error")
        assert_error_envelope("unknown API path -> 404 not_found", status, raw,
                              "not_found", 404)

        client.call("DELETE", "/api/status", tag="error", record=False)
        status, _, raw = client.call(
            "POST", "/api/config",
            {"text": "version: 1\ncomponent: nope\n"}, tag="error")
        assert_error_envelope("invalid component name -> 400 invalid_config",
                              status, raw, "invalid_config", 400)

        status, _, raw = client.call(
            "POST", "/api/config",
            {"text": "version: 1\ncomponent: lan-mdns\nserver:\n  port: 99999999\n"},
            tag="error")
        assert_error_envelope("out-of-range server.port -> 400 invalid_config",
                              status, raw, "invalid_config", 400)

        # Regression: an inline section value flattens to a leaf key, which used
        # to drop the whole section while reporting success.
        status, _, raw = client.call(
            "POST", "/api/config",
            {"text": "version: 1\ncomponent: lan-mdns\nserver: {port: 8080}\n"},
            tag="error")
        assert_error_envelope("inline section value -> 400 invalid_config",
                              status, raw, "invalid_config", 400)

        status, _, raw = client.call("POST", "/api/config",
                                     {"text": config_text}, tag="error",
                                     password="wrong-password")
        assert_error_envelope("wrong admin password -> 403 auth_failed",
                              status, raw, "auth_failed", 403)

        status, _, raw = client.call("POST", "/api/config",
                                     {"text": config_text}, tag="error",
                                     password="")
        assert_error_envelope("missing admin password -> 403 auth_failed",
                              status, raw, "auth_failed", 403)

        status, _, raw = client.call("POST", "/api/config/rollback",
                                     {"id": "../../etc/passwd"}, tag="error")
        assert_error_envelope("path traversal snapshot id -> 400 invalid_config",
                              status, raw, "invalid_config", 400)

        status, _, raw = client.call("GET",
                                     "/api/config/snapshot?id=config-19700101T000000Z.json",
                                     tag="error")
        assert_error_envelope("unknown snapshot id -> 404 not_found", status, raw,
                              "not_found", 404)

        status, _, raw = client.call("POST", "/api/auth/verify",
                                     {"password": "not-the-password"}, tag="error")
        assert_error_envelope("wrong password verification -> 403 auth_failed",
                              status, raw, "auth_failed", 403)

        status, _, raw = client.call("POST", "/api/auth/password",
                                     {"new_password": "contract-pass-2",
                                      "current_password": "wrong"}, tag="error")
        assert_error_envelope("wrong current_password -> 403 auth_failed",
                              status, raw, "auth_failed", 403)

        # setup_required was captured before the password existed.
        for path in ("/api/config", "/api/config/snapshot",
                     "/api/config/rollback", "/api/reload"):
            status, _, raw = client.seen[(("POST", path, "guard"))]
            assert_error_envelope(
                "write {} before setup -> 409 setup_required".format(path),
                status, raw, "setup_required", 409)

        # Read routes stay open once the password exists; only writes are guarded.
        status, _, raw = client.call("GET", "/api/config",
                                     password="", record=False)
        check("read routes are not password-guarded", status == 200,
              "HTTP {}".format(status))

        # X-Request-Id must be echoed so a report can be traced to a log line.
        status, _, raw = client.call(
            "GET", "/api/status", record=False,
            headers={"X-Request-Id": "contract-trace-0001"})
        echoed = (envelope_of(raw) or {}).get("request_id")
        check("X-Request-Id is echoed as request_id",
              echoed == "contract-trace-0001", str(echoed))

        # No payload — success or error — may echo a credential, a stack trace
        # or an internal filesystem path (ERROR-CODES §1 and §6).
        leaked = []
        for (method, path, tag), (status, _headers, raw) in client.seen.items():
            if status == 0:
                continue
            for hit in scan_leaks(raw, [str(ROOT), primary.workdir]):
                leaked.append("{} {} [{}]: {}".format(method, path, tag, hit))
        check("no payload leaks credentials, stacks or internal paths",
              not leaked, "; ".join(leaked[:4]))

        # PRD 8.1 #6: the structured log must never carry a credential, and the
        # on-disk log must stay inside the hard bound it advertises. Both were
        # only unit-tested (common-log rotation, common-alert redaction) before.
        status, _, raw = client.call("GET", "/api/logs?limit=1000", record=False)
        logs_data = (envelope_of(raw) or {}).get("data") or {}
        log_text = json.dumps(logs_data)
        log_leaks = [secret for secret in SECRET_VALUES if secret in log_text]
        check("log records carry no credentials", status == 200 and not log_leaks,
              str(log_leaks))
        disk, limit = logs_data.get("disk_bytes", 0), logs_data.get("disk_limit_bytes", 0)
        check("log disk usage stays within its advertised hard bound",
              limit > 0 and disk <= limit, "{} <= {}".format(disk, limit))
        check("log records are bounded", 0 < logs_data.get("capacity", 0) <= 4096,
              "capacity={}".format(logs_data.get("capacity")))

    # ------------------------------------------------- 5 · token-auth matrix
    section("5 · Admin Token 矩阵")
    token_config = config_text.replace('admin_token: ""', 'admin_token: "contract-token"')
    if token_config == config_text:
        check("template exposes server.admin_token", False,
              "cannot inject a token; template changed shape")
        token_config = config_text
    with Instance("token", token_config) as guarded:
        if not guarded.wait_ready():
            check("token instance becomes ready", False, "timeout")
            return 1
        client = Client(guarded.base)

        # Both probes live outside `/api/`, so the auth middleware never runs for
        # them; a configured token must not break them.
        status, _headers, raw = client.call("GET", "/healthz", record=False)
        check("/healthz bypasses the admin token",
              status == 200 and raw.strip() == '{"ok":true}', "HTTP {}".format(status))
        status, _, raw = client.call("GET", "/readyz", record=False)
        check("/readyz bypasses the admin token", status == 200,
              "HTTP {}".format(status))

        status, _, raw = client.call("GET", "/api/status", tag="noauth")
        assert_error_envelope("missing token -> 401 auth_required", status, raw,
                              "auth_required", 401)

        status, _, raw = client.call("GET", "/api/status", tag="noauth",
                                     token="wrong-token")
        assert_error_envelope("wrong token -> 403 auth_failed", status, raw,
                              "auth_failed", 403)

        client.token = "contract-token"
        status, _, raw = client.call("GET", "/api/status", tag="noauth")
        check("correct token is accepted",
              status == 200 and (envelope_of(raw) or {}).get("ok") is True,
              "HTTP {}".format(status))
        check("status reports token_required once a token exists",
              ((envelope_of(raw) or {}).get("data") or {})
              .get("security", {}).get("token_required") is True, "")

        status, headers, raw = client.call("GET", "/", record=False)
        check("GET / serves the embedded console as HTML",
              status == 200 and "text/html" in headers.get("Content-Type", "")
              and 'id="app"' in raw, headers.get("Content-Type", ""))

    # ------------------------------------------- 5b · error code closure
    # Collected across every instance, so it must run after the token matrix:
    # `auth_required` only exists once a token is configured.
    section("5b · 错误码闭合性（全局）")
    missing = [code for code in CODE_RUNTIME if code not in OBSERVED_CODES]
    check("every API-reachable error code was produced at runtime",
          not missing, "missing {}".format(missing))
    undeclared = sorted(code for code in OBSERVED_CODES if code not in ERROR_CODES)
    check("no runtime error code escapes the documented enum",
          not undeclared, str(undeclared))
    check("error code enum is exactly the eight documented values",
          len(ERROR_CODES) == 8 and len(set(ERROR_CODES)) == 8, str(ERROR_CODES))
    for code, reason in CODE_NOTES.items():
        print("NOTE  {} not exercised here: {}".format(code, reason))

    # --------------------------------------- 6 · documented vs implemented
    section("6 · 文档与实现漂移")
    with Instance("drift", config_text) as plain:
        if not plain.wait_ready():
            check("drift instance becomes ready", False, "timeout")
            return 1
        client = Client(plain.base)
        # Every route the binary registers, taken from the router table in
        # `components/lan-mdns/src/main.rs`, so both directions are checked:
        # documented-but-missing and implemented-but-undocumented.
        implemented = [
            ("GET", "/"), ("GET", "/healthz"), ("GET", "/readyz"),
            ("GET", "/api/status"), ("GET", "/api/metrics"),
            ("GET", "/api/config"), ("POST", "/api/config"),
            ("GET", "/api/config/snapshot"), ("POST", "/api/config/snapshot"),
            ("GET", "/api/config/history"), ("POST", "/api/config/rollback"),
            ("POST", "/api/reload"), ("GET", "/api/logs"),
            ("GET", "/api/logs/stream"), ("GET", "/api/alerts"),
            ("GET", "/api/auth"), ("POST", "/api/auth/password"),
            ("POST", "/api/auth/verify"), ("GET", "/api/records"),
            ("POST", "/api/probe"), ("GET", "/api/interfaces"),
            ("GET", "/api/health/targets"), ("GET", "/api/queries"),
            ("GET", "/api/browse"), ("GET", "/api/browse/unified"),
            ("GET", "/api/conflicts"),
        ]
        missing_routes = []      # documented, but the binary does not serve it
        extra_routes = []        # served, but the contract never mentions it
        for method, path in implemented:
            if path == "/api/logs/stream":
                continue  # exercised in section 2; it never closes on its own
            status, _, _ = client.call(method, path, record=False,
                                       body={} if method == "POST" else None)
            if (method, path) in by_key:
                if status == 404:
                    missing_routes.append("{} {}".format(method, path))
            elif path != "/":
                # `/` is the console itself, not an API surface.
                extra_routes.append("{} {}".format(method, path))
        check("every documented operation is served (no silent 404)",
              not missing_routes, "; ".join(missing_routes[:6]))
        check("no undocumented API route is exposed",
              not extra_routes, "; ".join(extra_routes[:6]))
        # Paths owned by other components are intentionally not served by the
        # M2 binary; only M2-namespace operations must match its router table.
        untouched = sorted(
            "{} {}".format(op.method, op.path) for op in ops
            if (op.method, op.path) not in set(implemented)
            and not op.path.startswith(component_prefixes))
        check("contract has no M2 operation the router forgot", not untouched,
              "; ".join(untouched[:6]))

    # -------------------------------------------------------------- report
    failed = [row for row in RESULTS if not row[1]]
    print("\n{} passed / {} failed".format(len(RESULTS) - len(failed), len(failed)))
    if failed:
        print("\nFAILED:")
        for name, _ok, detail in failed:
            print("  - {}{}".format(name, ("  | " + detail) if detail else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
