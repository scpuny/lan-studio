#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M3 runtime acceptance gate.

Starts the three P0 binaries in isolated temp directories, then verifies:
  * the unified contract (PRD 3) on every component,
  * the full config save -> snapshot -> rollback -> import -> reload loop,
  * lan-relay's four hard loop-detection acceptance items,
  * lan-ca issue/verify/revoke/audit and root-key secrecy,
  * lan-scan read-only discovery with real loopback TCP.

Zero third-party dependency. Usage:
    cargo build --workspace
    python3 scripts/test_m3.py
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN_DIR = ROOT / "target" / "debug"
COMPONENTS = [("lan-relay", 18081), ("lan-ca", 18082), ("lan-scan", 18083)]
PASSWORD = "M3Acceptance123"
RESULTS: list[tuple[str, bool, str]] = []
PROCS: list[subprocess.Popen] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, bool(ok), detail))
    print(("  PASS  " if ok else "  FAIL  ") + name + (("  -- " + detail) if detail and not ok else ""))


def call(base: str, path: str, method: str = "GET", body=None, headers=None):
    data = None if body is None else (json.dumps(body).encode() if not isinstance(body, (bytes, str)) else (body.encode() if isinstance(body, str) else body))
    hdr = {"Content-Type": "application/json"}
    hdr.update(headers or {})
    req = urllib.request.Request(base + path, data=data, method=method, headers=hdr)
    try:
        with urllib.request.urlopen(req, timeout=8) as resp:
            return resp.status, resp.read().decode("utf-8", "replace"), resp.headers.get("Content-Type", "")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace"), exc.headers.get("Content-Type", "")


def jcall(base: str, path: str, method: str = "GET", body=None, headers=None):
    status, raw, ctype = call(base, path, method, body, headers)
    try:
        return status, json.loads(raw), raw
    except json.JSONDecodeError:
        return status, {}, raw


def start(name: str, port: int, workdir: Path):
    binary = BIN_DIR / name
    if not binary.exists():
        raise RuntimeError(f"missing binary: {binary} (run `cargo build --workspace` first)")
    cfg = workdir / f"{name}.yaml"
    shutil.copy(ROOT / "components" / name / "config.template.yaml", cfg)
    env = os.environ.copy()
    env["LAN_STUDIO_PORT"] = str(port)
    env["LAN_STUDIO_CONFIG"] = str(cfg)
    proc = subprocess.Popen(
        [str(binary)], cwd=str(workdir), env=env,
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


def offline_check(name: str, workdir: Path):
    cfg = workdir / f"{name}.yaml"
    env = os.environ.copy()
    env["LAN_STUDIO_CONFIG"] = str(cfg)
    proc = subprocess.run([str(BIN_DIR / name), "--check"], cwd=str(workdir), env=env,
                          capture_output=True, text=True, timeout=20)
    check(f"{name} --check offline validation", proc.returncode == 0 and '"ok":true' in proc.stdout,
          proc.stdout + proc.stderr)


def unified_contract(name: str, base: str):
    short = name.replace("lan-", "")
    status, health, _ = jcall(base, "/healthz")
    check(f"{name} /healthz is constant ok", status == 200 and health == {"ok": True}, str(health))
    status, ready, _ = jcall(base, "/readyz")
    check(f"{name} /readyz ready", status == 200 and ready.get("data", {}).get("ready") is True, str(ready))
    status, st, _ = jcall(base, "/api/status")
    d = st.get("data", {})
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
    check(f"{name} /api/config raw+structured", status == 200 and f"component: {name}" in cfg.get("data", {}).get("raw", "")
          and isinstance(cfg.get("data", {}).get("structured"), dict))
    status, logs, _ = jcall(base, "/api/logs?limit=10")
    check(f"{name} /api/logs envelope", status == 200 and isinstance(logs.get("data", {}).get("logs"), list))
    status, alerts, _ = jcall(base, "/api/alerts")
    check(f"{name} /api/alerts envelope", status == 200 and isinstance(alerts.get("data", {}).get("by_kind"), list))
    status, auth, _ = jcall(base, "/api/auth")
    check(f"{name} /api/auth write protection", status == 200 and auth.get("data", {}).get("write_protection") is True)
    status, hist, _ = jcall(base, "/api/config/history")
    check(f"{name} /api/config/history", status == 200 and isinstance(hist.get("data", {}).get("items"), list))
    status, html, ctype = call(base, "/")
    forbidden = ["<script src=", "<link rel=\"stylesheet\"", "src=\"http", "url(http"]
    check(f"{name} offline six-area WebUI", status == 200 and "<style" in html
          and 'id="app"' in html and all(x not in html for x in forbidden)
          and "版本历史" in html, ctype)
    # Write protection must be in force before the password is set.
    status, denied, _ = jcall(base, "/api/config", "POST", {"text": "version: 1\n"})
    check(f"{name} write guard 409 setup_required", status == 409
          and denied.get("error", {}).get("code") == "setup_required", str(denied)[:160])


def config_lifecycle(base: str):
    status, setup, _ = jcall(base, "/api/auth/password", "POST", {"new_password": PASSWORD})
    check("relay admin password setup", status == 200 and setup.get("data", {}).get("password_set") is True, str(setup)[:160])
    hdr = {"X-Admin-Password": PASSWORD}
    status, original, _ = jcall(base, "/api/config")
    raw = original["data"]["raw"]
    status, saved, _ = jcall(base, "/api/config", "POST", {"text": raw + "\n# m3-acceptance\n"}, hdr)
    check("relay config save (validate->snapshot->atomic->reload)", status == 200
          and saved.get("data", {}).get("saved") is True, str(saved)[:160])
    status, hist, _ = jcall(base, "/api/config/history")
    items = hist.get("data", {}).get("items", [])
    check("relay snapshot created on save", status == 200 and len(items) >= 1, str(items)[:160])
    status, snap, raw_snap = jcall(base, "/api/config/snapshot?reason=gate")
    s = snap.get("data", {})
    check("relay snapshot contract shape", status == 200 and s.get("schema") == 1
          and s.get("component") == "lan-relay" and str(s.get("checksum", "")).startswith("sha256:"), raw_snap[:160])
    version_id = items[0]["id"] if items else ""
    status, rolled, _ = jcall(base, "/api/config/rollback", "POST", {"id": version_id}, hdr)
    check("relay rollback", status == 200 and rolled.get("data", {}).get("rolled_back") is True, str(rolled)[:160])
    status, imported, _ = jcall(base, "/api/config/snapshot", "POST", raw_snap, hdr)
    check("relay snapshot import (checksum verified)", status == 200
          and imported.get("data", {}).get("imported") is True, str(imported)[:160])
    status, reloaded, _ = jcall(base, "/api/reload", "POST", {}, hdr)
    check("relay manual reload", status == 200 and reloaded.get("data", {}).get("reloaded") is True, str(reloaded)[:160])
    status, bad, _ = jcall(base, "/api/config", "POST", {"text": "version: 2\n"}, hdr)
    check("relay invalid config rejected", status == 400 and bad.get("error", {}).get("code") == "invalid_config",
          str(bad)[:160])


def dns_query(tag: int = 0) -> str:
    packet = bytearray.fromhex("000000000001000000000000")
    packet[0] = tag & 0xFF
    packet += b"\x03nas\x05local\x00" + bytes.fromhex("00010001")
    return packet.hex()


def relay_acceptance(base: str):
    payload = dns_query(1)
    status, first, _ = jcall(base, "/api/relay/inject", "POST",
                             {"payload_hex": payload, "ingress": "eth0", "source_ip": "192.168.1.5"})
    d = first.get("data", {})
    check("relay G2 ingress isolation + normal forward", status == 200 and d.get("verdict") == "forward"
          and "eth0" not in d.get("targets", []) and d.get("ttl") == 3, str(d)[:200])
    status, second, _ = jcall(base, "/api/relay/inject", "POST",
                              {"payload_hex": payload, "ingress": "eth1", "source_ip": "192.168.1.5"})
    check("relay G1 fingerprint duplicate", status == 200
          and second.get("data", {}).get("verdict") == "duplicate", str(second)[:160])
    status, zero, _ = jcall(base, "/api/relay/inject", "POST",
                            {"payload_hex": dns_query(2), "ingress": "eth0", "ttl": 0,
                             "source_ip": "192.168.1.5"})
    check("relay G3 ttl==0 dropped", status == 200
          and zero.get("data", {}).get("verdict") == "ttl_exceeded", str(zero)[:160])
    status, selfy, _ = jcall(base, "/api/relay/inject", "POST",
                             {"payload_hex": dns_query(3), "ingress": "eth0",
                              "self_generated": True, "source_ip": "192.168.1.5"})
    check("relay G4 self-generated dropped", status == 200
          and selfy.get("data", {}).get("verdict") == "self_generated", str(selfy)[:160])
    local = dns_query(4)
    jcall(base, "/api/relay/mark-local", "POST", {"payload_hex": local})
    status, marked, _ = jcall(base, "/api/relay/inject", "POST",
                              {"payload_hex": local, "ingress": "eth0", "source_ip": "192.168.1.5"})
    check("relay G4 mark-local reflection dropped", status == 200
          and marked.get("data", {}).get("verdict") == "self_generated", str(marked)[:160])
    status, outside, _ = jcall(base, "/api/relay/inject", "POST",
                               {"payload_hex": dns_query(5), "ingress": "eth0", "source_ip": "203.0.113.7"})
    check("relay source whitelist enforced", status == 200
          and outside.get("data", {}).get("verdict") == "outside_whitelist", str(outside)[:160])
    # Switch to a single-NIC config to prove there is no legal egress.
    status, cfg, _ = jcall(base, "/api/config")
    raw = cfg["data"]["raw"]
    single = raw.replace('    - "eth1"\n', "")
    hdr = {"X-Admin-Password": PASSWORD}
    status, saved, _ = jcall(base, "/api/config", "POST", {"text": single}, hdr)
    status, alone, _ = jcall(base, "/api/relay/inject", "POST",
                             {"payload_hex": dns_query(6), "ingress": "eth0", "source_ip": "192.168.1.5"})
    check("relay G2 single-NIC same_interface", status == 200
          and alone.get("data", {}).get("verdict") == "same_interface", str(alone)[:160])
    jcall(base, "/api/config", "POST", {"text": raw}, hdr)  # restore
    # Storm: 200 identical reflections must be absorbed, not amplified.
    _, before, _ = jcall(base, "/api/relay/stats")
    storm = dns_query(7)
    for _ in range(200):
        jcall(base, "/api/relay/inject", "POST",
              {"payload_hex": storm, "ingress": "eth0", "source_ip": "192.168.1.5"})
    _, after, _ = jcall(base, "/api/relay/stats")
    b, a = before.get("data", {}), after.get("data", {})
    dup = a.get("duplicate", 0) - b.get("duplicate", 0)
    fwd = a.get("forwarded", 0) - b.get("forwarded", 0)
    check("relay 60s-style loop storm absorbed (no amplification)", dup >= 195 and fwd <= 5
          and a.get("dedup_entries", 0) <= a.get("dedup_capacity", 1),
          f"duplicate_delta={dup} forward_delta={fwd} entries={a.get('dedup_entries')}")


def ca_acceptance(base: str, workdir: Path):
    status, root, _ = jcall(base, "/api/ca/root")
    d = root.get("data", {})
    check("ca root fingerprint without key", status == 200 and d.get("fingerprint")
          and d.get("key_exported") is False, str(d)[:160])
    status, issued, raw = jcall(base, "/api/ca/issue", "POST",
                                {"subject": "nas.lan", "sans": ["nas.lan", "192.168.1.5"], "days": 90})
    cert = issued.get("data", {}).get("certificate", {})
    serial = cert.get("serial", "")
    check("ca issue internal certificate", status == 200 and serial
          and "BEGIN LAN STUDIO CERTIFICATE" in issued.get("data", {}).get("pem", ""), raw[:160])
    status, verified, _ = jcall(base, "/api/ca/verify", "POST", {"serial": serial})
    check("ca signature verifies", status == 200 and verified.get("data", {}).get("valid") is True, str(verified)[:160])
    status, bad, _ = jcall(base, "/api/ca/issue", "POST", {"subject": "*.lan"})
    check("ca rejects wildcard subject", status == 400 and bad.get("error", {}).get("code") == "invalid_config",
          str(bad)[:160])
    jcall(base, "/api/ca/renew", "POST", {"serial": serial, "days": 365})
    status, revoked, _ = jcall(base, "/api/ca/revoke", "POST", {"serial": serial, "reason": "key-compromise"})
    check("ca revoke", status == 200 and revoked.get("data", {}).get("certificate", {}).get("status") == "revoked",
          str(revoked)[:160])
    status, crl, _ = jcall(base, "/api/ca/crl")
    check("ca CRL contains revoked serial", status == 200
          and any(c.get("serial") == serial for c in crl.get("data", {}).get("items", [])), str(crl)[:160])
    status, audit, _ = jcall(base, "/api/ca/audit")
    actions = [a.get("action") for a in audit.get("data", {}).get("items", [])]
    check("ca issue/renew/revoke audit trail", status == 200 and {"issue", "renew", "revoke"}.issubset(set(actions)),
          str(actions)[:160])
    status, exported, export_raw = jcall(base, "/api/ca/export")
    key_file = workdir / "lan-ca.root.key"
    key_text = key_file.read_text().strip() if key_file.exists() else ""
    check("ca root key never in export/snapshot", status == 200 and key_text
          and key_text not in export_raw and key_text not in json.dumps(root), "key leaked")


def scan_acceptance(base: str, workdir: Path):
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    port = listener.getsockname()[1]
    try:
        status, started, _ = jcall(base, "/api/scan/start", "POST",
                                   {"segment": "127.0.0.1/32", "ports": [port]})
        check("scan starts a bounded read-only job", status == 200
              and started.get("data", {}).get("started") is True
              and started.get("data", {}).get("hosts") == 1, str(started)[:160])
        for _ in range(80):
            _, state, _ = jcall(base, "/api/scan/status")
            if not state.get("data", {}).get("running", True):
                break
            time.sleep(0.1)
        status, results, raw = jcall(base, "/api/scan/results")
        items = results.get("data", {}).get("items", [])
        hit = any(any(h.get("port") == port for h in item.get("hits", [])) for item in items)
        check("scan discovers open loopback port", status == 200 and hit, raw[:200])
        status, oversize, _ = jcall(base, "/api/scan/start", "POST", {"segment": "10.0.0.0/8"})
        check("scan rejects oversized segment", status == 400
              and oversize.get("error", {}).get("code") == "invalid_config", str(oversize)[:160])
        status, snap, _ = jcall(base, "/api/scan/snapshot")
        check("scan snapshot is self-contained", status == 200
              and snap.get("data", {}).get("component") == "lan-scan"
              and isinstance(snap.get("data", {}).get("runs"), list), str(snap)[:160])
        results_file = workdir / "lan-scan.results.json"
        disk = results_file.read_text() if results_file.exists() else ""
        check("scan writes only its own results file", bool(disk)
              and "lan-mdns" not in disk and "lan-relay" not in disk, disk[:120])
        _, state, _ = jcall(base, "/api/scan/status")
        sc = state.get("data", {}).get("scan", {})
        check("scan rate/concurrency ceilings exposed", sc.get("rate_per_second", 0) <= 1000
              and sc.get("concurrency", 0) <= 64 and sc.get("max_hosts", 0) <= 65536, str(sc)[:160])
    finally:
        listener.close()


def main() -> int:
    global BIN_DIR
    if (ROOT / "target" / "release").exists() and (ROOT / "target" / "release" / "lan-relay").exists():
        BIN_DIR = ROOT / "target" / "release"
    tmp = Path(tempfile.mkdtemp(prefix="lan-studio-m3-"))
    bases: dict[str, str] = {}
    dirs: dict[str, Path] = {}
    try:
        print(f"M3 acceptance workspace: {tmp}")
        for name, port in COMPONENTS:
            workdir = tmp / name
            workdir.mkdir(parents=True, exist_ok=True)
            dirs[name] = workdir
            offline_check(name, workdir)
            bases[name] = start(name, port, workdir)
            print(f"  {name} ready on {bases[name]}")
        for name, _ in COMPONENTS:
            unified_contract(name, bases[name])
        config_lifecycle(bases["lan-relay"])
        relay_acceptance(bases["lan-relay"])
        ca_acceptance(bases["lan-ca"], dirs["lan-ca"])
        scan_acceptance(bases["lan-scan"], dirs["lan-scan"])
    except Exception as exc:  # noqa: BLE001 - gate must report, not hide
        check("M3 acceptance harness", False, f"{type(exc).__name__}: {exc}")
    finally:
        for proc in PROCS:
            proc.terminate()
        for proc in PROCS:
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(tmp, ignore_errors=True)

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    failed = [name for name, ok, _ in RESULTS if not ok]
    print(f"\nM3 RUNTIME GATE: {passed}/{len(RESULTS)} passed")
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("M3 RUNTIME OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
