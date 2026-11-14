#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M4 runtime acceptance gate.

Starts lan-hosts / lan-wol in isolated temp directories and verifies:
  * the unified contract (PRD 3) on both components,
  * the config save -> snapshot -> rollback -> import -> reload loop,
  * lan-hosts static mapping lifecycle, rendering and atomic publish,
  * lan-wol registered-device-only wake, cooldown, magic packet and debounce,
  * five explicit "no overlap / no coupling" assertions (PRD 7.5).

Zero third-party dependency. Usage:
    cargo build --workspace
    python3 scripts/test_m4.py
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
COMPONENTS = [("lan-hosts", 18084), ("lan-wol", 18085)]
PASSWORD = "M4Acceptance123"
HDR = {"X-Admin-Password": PASSWORD}
RESULTS: list[tuple[str, bool, str]] = []
PROCS: list[subprocess.Popen] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, bool(ok), detail))
    print(("  PASS  " if ok else "  FAIL  ") + name + (("  -- " + detail) if detail and not ok else ""))


def call(base: str, path: str, method: str = "GET", body=None, headers=None):
    data = None if body is None else (
        json.dumps(body).encode() if not isinstance(body, (bytes, str))
        else (body.encode() if isinstance(body, str) else body)
    )
    hdr = {"Content-Type": "application/json"}
    hdr.update(headers or {})
    req = urllib.request.Request(base + path, data=data, method=method, headers=hdr)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
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
    check(f"{name} /api/config raw+structured", status == 200
          and f"component: {name}" in cfg.get("data", {}).get("raw", "")
          and isinstance(cfg.get("data", {}).get("structured"), dict))
    status, logs, _ = jcall(base, "/api/logs?limit=10")
    check(f"{name} /api/logs envelope", status == 200 and isinstance(logs.get("data", {}).get("logs"), list))
    status, alerts, _ = jcall(base, "/api/alerts")
    check(f"{name} /api/alerts envelope", status == 200 and isinstance(alerts.get("data", {}).get("by_kind"), list))
    status, auth, _ = jcall(base, "/api/auth")
    check(f"{name} /api/auth write protection", status == 200
          and auth.get("data", {}).get("write_protection") is True)
    status, hist, _ = jcall(base, "/api/config/history")
    check(f"{name} /api/config/history", status == 200 and isinstance(hist.get("data", {}).get("items"), list))
    status, html, _ = call(base, "/")
    forbidden = ["<script src=", "<link rel=\"stylesheet\"", "src=\"http", "url(http"]
    check(f"{name} offline six-area WebUI", status == 200 and "<style" in html
          and 'id="app"' in html and all(x not in html for x in forbidden)
          and "版本历史" in html, html[:120])
    status, denied, _ = jcall(base, "/api/config", "POST", {"text": "version: 1\n"})
    check(f"{name} write guard 409 setup_required", status == 409
          and denied.get("error", {}).get("code") == "setup_required", str(denied)[:160])


def setup_password(base: str, name: str):
    status, setup, _ = jcall(base, "/api/auth/password", "POST", {"new_password": PASSWORD})
    check(f"{name} admin password setup", status == 200
          and setup.get("data", {}).get("password_set") is True, str(setup)[:160])


def config_lifecycle(base: str, name: str):
    status, original, _ = jcall(base, "/api/config")
    raw = original.get("data", {}).get("raw", "")
    status, saved, _ = jcall(base, "/api/config", "POST", {"text": raw + "\n# m4-acceptance\n"}, HDR)
    check(f"{name} config save (validate->snapshot->atomic->reload)", status == 200
          and saved.get("data", {}).get("saved") is True, str(saved)[:160])
    status, hist, _ = jcall(base, "/api/config/history")
    items = hist.get("data", {}).get("items", [])
    check(f"{name} snapshot created on save", status == 200 and len(items) >= 1, str(items)[:160])
    status, snap, raw_snap = jcall(base, "/api/config/snapshot?reason=gate")
    s = snap.get("data", {})
    check(f"{name} snapshot contract shape", status == 200 and s.get("schema") == 1
          and s.get("component") == name and str(s.get("checksum", "")).startswith("sha256:"),
          raw_snap[:160])
    version_id = items[0]["id"] if items else ""
    status, rolled, _ = jcall(base, "/api/config/rollback", "POST", {"id": version_id}, HDR)
    check(f"{name} rollback", status == 200 and rolled.get("data", {}).get("rolled_back") is True,
          str(rolled)[:160])
    status, imported, _ = jcall(base, "/api/config/snapshot", "POST", raw_snap, HDR)
    check(f"{name} snapshot import (checksum verified)", status == 200
          and imported.get("data", {}).get("imported") is True, str(imported)[:160])
    status, reloaded, _ = jcall(base, "/api/reload", "POST", {}, HDR)
    check(f"{name} manual reload", status == 200 and reloaded.get("data", {}).get("reloaded") is True,
          str(reloaded)[:160])
    status, bad, _ = jcall(base, "/api/config", "POST", {"text": "version: 2\n"}, HDR)
    check(f"{name} invalid config rejected", status == 400
          and bad.get("error", {}).get("code") == "invalid_config", str(bad)[:160])


def replace_config(base: str, replacements: list[tuple[str, str]], name: str) -> bool:
    status, cfg, _ = jcall(base, "/api/config")
    raw = cfg.get("data", {}).get("raw", "")
    for old, new in replacements:
        raw = raw.replace(old, new)
    status, saved, _ = jcall(base, "/api/config", "POST", {"text": raw}, HDR)
    ok = status == 200 and saved.get("data", {}).get("saved") is True
    check(f"{name} config hot-reload applied", ok, str(saved)[:160])
    return ok


def hosts_acceptance(base: str, workdir: Path):
    status, st, _ = jcall(base, "/api/hosts/status")
    d = st.get("data", {})
    check("hosts empty status", status == 200 and d.get("mappings") == 0
          and d.get("publish_enabled") is False, str(d)[:160])

    status, up, _ = jcall(base, "/api/hosts/mapping", "POST",
                          {"name": "nas.lan", "ip": "192.168.5.10",
                           "aliases": ["storage.lan"], "comment": "NAS",
                           "tags": ["storage"], "enabled": True}, HDR)
    check("hosts upsert mapping", status == 200 and up.get("data", {}).get("total") == 1, str(up)[:160])

    status, denied, _ = jcall(base, "/api/hosts/mapping", "POST",
                              {"name": "printer.local", "ip": "192.168.5.20"}, HDR)
    check("hosts rejects .local suffix (no mDNS overlap)", status == 400
          and denied.get("error", {}).get("code") == "invalid_config", str(denied)[:160])

    status, conflict, _ = jcall(base, "/api/hosts/mapping", "POST",
                                {"name": "nas.lan", "ip": "192.168.5.99"}, HDR)
    check("hosts same-name different-IP conflict rejected", status == 400
          and conflict.get("error", {}).get("code") == "invalid_config", str(conflict)[:160])

    status, badip, _ = jcall(base, "/api/hosts/mapping", "POST",
                             {"name": "bad.lan", "ip": "999.1.1.1"}, HDR)
    check("hosts invalid IP rejected", status == 400, str(badip)[:160])

    status, import_result, _ = jcall(base, "/api/hosts/import", "POST",
                                     {"text": "# comment\n\n192.168.5.30 printer.lan alias.lan\n"
                                              "192.168.5.40 cam.lan\n"}, HDR)
    imp = import_result.get("data", {})
    check("hosts import standard hosts text", status == 200 and imp.get("imported", 0) >= 2, str(imp)[:160])

    status, r1, _ = jcall(base, "/api/hosts/render?format=hosts")
    status2, r2, _ = jcall(base, "/api/hosts/render?format=hosts")
    t1 = r1.get("data", {}).get("text", "")
    check("hosts render is stable and contains mappings", status == 200 and status2 == 200
          and t1 == r2.get("data", {}).get("text", "") and "192.168.5.10\tnas.lan" in t1
          and "storage.lan" in t1, t1[:160])

    status, listed, _ = jcall(base, "/api/hosts/mappings")
    check("hosts mappings list", status == 200 and listed.get("data", {}).get("total", 0) >= 3,
          str(listed.get("data", {}))[:160])

    status, blocked, _ = jcall(base, "/api/hosts/publish", "POST", {}, HDR)
    check("hosts publish blocked while disabled", status in (400, 409)
          and blocked.get("error", {}).get("code") == "invalid_config", str(blocked)[:160])

    replace_config(base, [("publish_enabled: false", "publish_enabled: true")], "hosts")
    status, pub, _ = jcall(base, "/api/hosts/publish", "POST", {}, HDR)
    data = pub.get("data", {})
    out = workdir / "lan-hosts.out"
    check("hosts atomic publish creates output file", status == 200
          and data.get("published") is True and out.exists()
          and "nas.lan" in out.read_text(encoding="utf-8", errors="replace"), str(data)[:160])

    before = out.read_text(encoding="utf-8", errors="replace") if out.exists() else ""
    replace_config(base, [("output_path: \"lan-hosts.out\"",
                           "output_path: \"no/such/dir/hosts.out\"")], "hosts")
    status, failed, _ = jcall(base, "/api/hosts/publish", "POST", {}, HDR)
    after = out.read_text(encoding="utf-8", errors="replace") if out.exists() else ""
    check("hosts failed publish keeps the old file intact", status in (400, 409)
          and before == after and before != "", str(failed)[:160])
    replace_config(base, [("output_path: \"no/such/dir/hosts.out\"",
                           "output_path: \"lan-hosts.out\"")], "hosts")

    store = workdir / "lan-hosts.mappings.json"
    check("hosts business data stays in its own file", store.exists() and store.stat().st_size > 0,
          str(store))

    status, removed, _ = jcall(base, "/api/hosts/remove", "POST", {"name": "cam.lan"}, HDR)
    check("hosts remove mapping", status == 200 and removed.get("data", {}).get("removed") is True,
          str(removed)[:160])


def wol_acceptance(base: str, workdir: Path):
    tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    tcp.bind(("127.0.0.1", 0))
    tcp.listen(16)
    tcp_port = tcp.getsockname()[1]
    udp_port = 18089
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("127.0.0.1", udp_port))
    udp.settimeout(3)
    try:
        status, st, _ = jcall(base, "/api/wol/status")
        d = st.get("data", {})
        check("wol empty status", status == 200 and d.get("devices") == 0
              and d.get("send_enabled") is False, str(d)[:160])

        status, dev, _ = jcall(base, "/api/wol/device", "POST",
                               {"id": "dev-01", "name": "workstation",
                                "mac": "AA:BB:CC:DD:EE:FF", "ip": "127.0.0.1",
                                "probe_port": tcp_port, "enabled": True,
                                "note": "lab"}, HDR)
        check("wol register device", status == 200 and dev.get("data", {}).get("total") == 1,
              str(dev)[:160])

        status, bad, _ = jcall(base, "/api/wol/device", "POST",
                               {"id": "dev-bad", "name": "bad", "mac": "ZZ:11:22:33:44:55",
                                "ip": "127.0.0.1"}, HDR)
        check("wol rejects invalid MAC", status == 400, str(bad)[:160])

        status, missing, _ = jcall(base, "/api/wol/wake", "POST",
                                   {"device_id": "not-registered", "mac": "AA:BB:CC:DD:EE:FF"}, HDR)
        check("wol wake rejects unregistered target", status == 404
              and missing.get("error", {}).get("code") == "not_found", str(missing)[:160])

        status, disabled, _ = jcall(base, "/api/wol/wake", "POST", {"device_id": "dev-01"}, HDR)
        check("wol wake blocked while send_enabled=false", status == 409
              and disabled.get("error", {}).get("code") == "invalid_config", str(disabled)[:160])

        replace_config(base, [("send_enabled: false", "send_enabled: true"),
                              ("broadcast: \"255.255.255.255\"", "broadcast: \"127.0.0.1\""),
                              ("port: 9\n", f"port: {udp_port}\n")], "wol")
        status, sent, _ = jcall(base, "/api/wol/wake", "POST", {"device_id": "dev-01"}, HDR)
        data = sent.get("data", {})
        packet = b""
        try:
            packet, _ = udp.recvfrom(2048)
        except socket.timeout:
            pass
        expected_mac = bytes.fromhex("aabbccddeeff")
        check("wol magic packet is exactly 102 bytes with 16 MAC repeats",
              status == 200 and data.get("sent") is True and data.get("bytes") == 102
              and len(packet) == 102 and packet[:6] == b"\xff" * 6
              and packet[6:] == expected_mac * 16,
              f"resp={str(data)[:120]} bytes={len(packet)} head={packet[:8].hex()}")

        status, cooled, _ = jcall(base, "/api/wol/wake", "POST", {"device_id": "dev-01"}, HDR)
        check("wol per-device cooldown rejects repeated wake", status == 429
              and cooled.get("error", {}).get("code") == "rate_limited", str(cooled)[:160])

        states = []
        for _ in range(3):
            status, probed, _ = jcall(base, "/api/wol/probe", "POST", {"device_id": "dev-01"}, HDR)
            states.append(probed.get("data", {}))
        check("wol read-only probe reaches registered TCP port",
              all(s.get("reachable") is True for s in states), str(states)[:160])

        status, devices, _ = jcall(base, "/api/wol/devices")
        items = devices.get("data", {}).get("items", [])
        check("wol debounced state becomes online after 3 samples",
              status == 200 and any(i.get("state") == "online" for i in items),
              str(items)[:200])

        status, history, _ = jcall(base, "/api/wol/history")
        h = history.get("data", {})
        check("wol audit trail is bounded and populated", status == 200
              and h.get("total", 0) >= 1 and h.get("total", 0) <= h.get("limit", 0),
              str(h)[:160])

        store = workdir / "lan-wol.devices.json"
        check("wol device data stays in its own file", store.exists() and store.stat().st_size > 0,
              str(store))

        status, removed, _ = jcall(base, "/api/wol/remove", "POST", {"id": "dev-01"}, HDR)
        check("wol remove device", status == 200 and removed.get("data", {}).get("removed") is True,
              str(removed)[:160])
        status, gone, _ = jcall(base, "/api/wol/wake", "POST", {"device_id": "dev-01"}, HDR)
        check("wol removed device can no longer be woken", status == 404, str(gone)[:160])
    finally:
        udp.close()
        tcp.close()


def no_overlap_assertions(bases: dict[str, str]):
    """M4 acceptance: no functional overlap with lan-mdns, no strong coupling."""
    manifests = {
        name: (ROOT / "components" / name / "Cargo.toml").read_text(encoding="utf-8")
        for name, _ in COMPONENTS
    }
    check("M4 crates do not depend on any other component crate",
          all("components/lan-" not in text for text in manifests.values()),
          str({k: [l for l in v.splitlines() if "components/" in l] for k, v in manifests.items()}))
    sources = []
    for name, _ in COMPONENTS:
        for path in (ROOT / "components" / name / "src").glob("*.rs"):
            sources.append(path.read_text(encoding="utf-8"))
    joined = "\n".join(sources)
    check("M4 sources contain no mDNS multicast or DNS packet code",
          "224.0.0.251" not in joined and "common_dns" not in joined
          and "MDNS_PORT" not in joined, joined[:120])
    api = (ROOT / "contracts" / "openapi.yaml").read_text(encoding="utf-8")
    check("openapi declares all M4 endpoints",
          all(f"/api/hosts/{p}" in api for p in
              ["mappings", "mapping", "remove", "import", "render", "publish", "status"])
          and all(f"/api/wol/{p}" in api for p in
                  ["devices", "device", "remove", "wake", "probe", "status", "history"]))
    status, cfg, _ = jcall(bases["lan-hosts"], "/api/config")
    raw = cfg.get("data", {}).get("raw", "")
    # Strip comments: prose describing the boundary may legitimately mention
    # lan-mdns, but no effective YAML key may reference the mdns namespace.
    effective = "\n".join(line.split("#", 1)[0] for line in raw.splitlines())
    check("lan-hosts config has no mdns namespace",
          "mdns." not in effective and "mdns:" not in effective
          and "lan-mdns" not in effective, effective[:160])
    for name, base in bases.items():
        status, _, _ = jcall(base, "/api/records")
        check(f"{name} does not expose lan-mdns /api/records", status == 404, str(status))


def main() -> int:
    global BIN_DIR
    if (ROOT / "target" / "release").exists() and (ROOT / "target" / "release" / "lan-hosts").exists():
        BIN_DIR = ROOT / "target" / "release"
    tmp = Path(tempfile.mkdtemp(prefix="lan-studio-m4-"))
    bases: dict[str, str] = {}
    dirs: dict[str, Path] = {}
    try:
        print(f"M4 acceptance workspace: {tmp}")
        for name, port in COMPONENTS:
            workdir = tmp / name
            workdir.mkdir(parents=True, exist_ok=True)
            dirs[name] = workdir
            offline_check(name, workdir)
            bases[name] = start(name, port, workdir)
            print(f"  {name} ready on {bases[name]}")
        for name, _ in COMPONENTS:
            unified_contract(name, bases[name])
            setup_password(bases[name], name)
            config_lifecycle(bases[name], name)
        hosts_acceptance(bases["lan-hosts"], dirs["lan-hosts"])
        wol_acceptance(bases["lan-wol"], dirs["lan-wol"])
        no_overlap_assertions(bases)
    except Exception as exc:  # noqa: BLE001 - gate must report, not hide
        check("M4 acceptance harness", False, f"{type(exc).__name__}: {exc}")
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
    print(f"\nM4 RUNTIME GATE: {passed}/{len(RESULTS)} passed")
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("M4 RUNTIME OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
