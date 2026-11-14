#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5 ecosystem acceptance gate (PRD 8.2, six items).

Self-contained, zero third-party dependency (the fixtures are a compact copy
of scripts/test_m4.py / scripts/test_m5.py so the two gates never need to
import each other). Verifies:

  1. each of the 8 components has README.md + config.template.yaml + a passing
     offline --check (lan-mdns uses --validate),
  2. each component deploys standalone in an isolated temp dir (healthz,
     /api/config envelope, offline single-file WebUI), one at a time,
  3. contract zero-drift: 9 mandatory metric families, 8-code error enum,
     unified ok/data/request_id envelope, 409 setup_required write guard,
  4. Shell navigation to >= 2 actually running components (online matches
     the real /healthz; stopped components are false/null),
  5. release artifacts (workflow 8 names / 2 musl targets / sha256, install.sh
     passes `bash -n`),
  6. documentation consistency (README vs manifest, ports 8080-8087,
     M5-CHECKLIST, openapi proprietary endpoints, METRICS 3.7, ADR 0007,
     and no stale status wording in README).

Usage:
    cargo build --workspace
    python3 scripts/test_ecosystem.py
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN_DIR = ROOT / "target" / "debug"
COMPONENTS = [
    "lan-mdns", "lan-relay", "lan-ca", "lan-scan",
    "lan-hosts", "lan-wol", "lan-sync", "lan-ssdp",
]
# Distinct high-port range so this gate never collides with test_m5.py's
# 18080-18099 / the documented 8080-8087 defaults.
ECO_PORTS = {
    "lan-mdns": 18100, "lan-relay": 18101, "lan-ca": 18102,
    "lan-scan": 18103, "lan-hosts": 18104, "lan-wol": 18105,
    "lan-sync": 18106, "lan-ssdp": 18107,
}
SHELL_PORT = 18190
SSDP_UDP_PORT = 18189           # unicast-only SSDP socket (no real LAN multicast)
MDNS_UDP_PORT = "15354"
MDNS_GROUP = "239.1.2.6"

METRIC_FAMILIES = ["up", "uptime_seconds", "build_info", "config_reloads_total",
                   "config_saves_total", "http_requests_total", "alerts_sent_total",
                   "log_disk_bytes", "log_disk_limit_bytes"]
EXPECTED_CODES = {"invalid_config", "auth_required", "auth_failed",
                  "setup_required", "not_found", "target_unreachable",
                  "rate_limited", "internal_error"}

RESULTS: list[tuple[str, bool, str]] = []
PROCS: list[subprocess.Popen] = []

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


# ----------------------------------------------------------------- fixtures


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, bool(ok), detail))
    print(("  PASS  " if ok else "  FAIL  ") + name
          + (("  -- " + detail) if detail and not ok else ""))


def section(title: str) -> None:
    print(f"\n=== {title} ===")


def call(base: str, path: str, method: str = "GET", body=None, headers=None):
    data = None if body is None else (
        __import__("json").dumps(body).encode() if not isinstance(body, (bytes, str))
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
    except urllib.error.URLError:
        return 0, "", ""


def jcall(base: str, path: str, method: str = "GET", body=None, headers=None):
    status, raw, _ = call(base, path, method, body, headers)
    try:
        return status, __import__("json").loads(raw), raw
    except ValueError:
        return status, {}, raw


def data_of(parsed):
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


def isolated_config(name: str):
    """Return a LAN-quiet override for components that otherwise emit real
    multicast at startup; None means the stock template is safe to use."""
    text = (ROOT / "components" / name / "config.template.yaml").read_text(
        encoding="utf-8")
    if name == "lan-relay":
        # transport is best-effort; disabling it keeps the gate off the LAN
        # while still exercising the full API surface.
        return text.replace("  transport: true\n", "  transport: false\n")
    if name == "lan-ssdp":
        # A unicast group means the isolation hook never joins a multicast
        # group and never emits M-SEARCH (see lan-ssdp start()).
        text = text.replace('  group: "239.255.255.250"', '  group: "127.0.0.1"')
        text = text.replace("  port: 1900\n", f"  port: {SSDP_UDP_PORT}\n")
        return text
    return None


def start(name: str, port: int, workdir: Path, extra_env=None, config_text=None) -> str:
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


def start_shell(workdir: Path, manifest_text: str, port: int = SHELL_PORT) -> str:
    manifest = workdir / "components.yaml"
    manifest.write_text(manifest_text, encoding="utf-8")
    env = os.environ.copy()
    env["LAN_SHELL_CONFIG"] = str(manifest)
    env["LAN_STUDIO_PORT"] = str(port)
    env.pop("LAN_SHELL_TOKEN", None)
    proc = subprocess.Popen([str(binary("lan-shell"))], cwd=str(workdir), env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
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
            raise RuntimeError(f"lan-shell exited with {proc.returncode}")
        time.sleep(0.15)
    raise RuntimeError(f"lan-shell did not become healthy on {base}")


def poll_until(fn, timeout: float = 20.0, interval: float = 0.5) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if fn():
                return True
        except Exception:
            pass
        time.sleep(interval)
    return False


def safe_phase(label: str, fn) -> None:
    try:
        fn()
    except Exception as exc:  # noqa: BLE001 - gate must report, not hide
        check(label, False, f"{type(exc).__name__}: {exc}")


# --------------------------------------------- 1. per-component deliverables


def section1(workdir: Path) -> None:
    section("1. 8 components deliverables (README / template / offline check)")
    for name in COMPONENTS:
        check(f"ecosystem {name} README.md exists",
              (ROOT / "components" / name / "README.md").exists(), str(ROOT / "components" / name))
        check(f"ecosystem {name} config.template.yaml exists",
              (ROOT / "components" / name / "config.template.yaml").exists(),
              str(ROOT / "components" / name))
        flag = "--validate" if name == "lan-mdns" else "--check"
        wd = workdir / name
        wd.mkdir(parents=True, exist_ok=True)
        cfg = wd / f"{name}.yaml"
        try:
            cfg.write_text((ROOT / "components" / name / "config.template.yaml").read_text(
                encoding="utf-8"), encoding="utf-8")
            env = os.environ.copy()
            env["LAN_STUDIO_CONFIG"] = str(cfg)
            proc = subprocess.run([str(binary(name)), flag], cwd=str(wd), env=env,
                                  capture_output=True, text=True, timeout=20)
            out = proc.stdout + proc.stderr
            # common-component --check -> {"ok":true}; lan-mdns --validate ->
            # a structured log line containing "config valid".
            ok = proc.returncode == 0 and ('"ok":true' in out or "config valid" in out)
            check(f"ecosystem {name} {flag} offline validation", ok, out)
        except Exception as exc:
            check(f"ecosystem {name} {flag} offline validation", False,
                  f"{type(exc).__name__}: {exc}")


# --------------------------------- 2 + 3. standalone deployment + contract


def openapi_error_codes():
    try:
        text = (ROOT / "contracts" / "openapi.yaml").read_text(encoding="utf-8")
    except OSError:
        return set()
    block = text.split("ErrorCode:", 1)[1] if "ErrorCode:" in text else ""
    codes = set()
    started = False
    for line in block.splitlines():
        stripped = line.strip()
        if stripped.startswith("- "):
            value = stripped[2:].strip()
            if re.fullmatch(r"[a-z_]+", value):
                codes.add(value)
                started = True
        elif started:
            break
    return codes


def error_code_consistency() -> None:
    api_codes = openapi_error_codes()
    try:
        doc = (ROOT / "docs" / "ERROR-CODES.md").read_text(encoding="utf-8")
    except OSError:
        doc = ""
    doc_codes = {c for c in EXPECTED_CODES if f"`{c}`" in doc}
    check("ecosystem 8 error codes: openapi enum == ERROR-CODES.md",
          api_codes == EXPECTED_CODES and doc_codes == EXPECTED_CODES,
          f"api={sorted(api_codes)} doc={sorted(doc_codes)}")


def per_component(name: str, port: int, workdir: Path) -> None:
    suffixes = ["standalone /healthz", "standalone /api/config envelope",
                "standalone offline WebUI", "9 mandatory metric families",
                "unified envelope ok/data/request_id", "write protection 409 setup_required"]
    try:
        extra = None
        if name == "lan-mdns":
            extra = {
                "LAN_MDNS_PORT": MDNS_UDP_PORT,
                "LAN_MDNS_GROUP": MDNS_GROUP,
                "LAN_STUDIO_AUTH_FILE": str(workdir / "auth.toml"),
                "LAN_STUDIO_LOG_DIR": str(workdir / "logs"),
                "LAN_STUDIO_HISTORY_DIR": str(workdir / "history"),
            }
        base = start(name, port, workdir, extra_env=extra,
                     config_text=isolated_config(name))
    except Exception as exc:
        for suffix in suffixes:
            check(f"ecosystem {name} {suffix}", False,
                  f"start failed: {type(exc).__name__}: {exc}")
        return

    # Section 2: standalone liveness, config envelope, offline WebUI.
    status, health, _ = jcall(base, "/healthz")
    check(f"ecosystem {name} standalone /healthz", status == 200
          and health == {"ok": True}, str(health))
    status, body, raw = jcall(base, "/api/config")
    cd = data_of(body)
    check(f"ecosystem {name} standalone /api/config envelope", status == 200
          and "data" in body and isinstance(cd.get("raw"), str), raw[:160])
    status, html, _ = call(base, "/")
    forbidden = ["<script src=", "src=\"http", "url(http", "<link rel=\"stylesheet\""]
    check(f"ecosystem {name} standalone offline WebUI", status == 200
          and "<style" in html and all(x not in html for x in forbidden), html[:160])

    # Section 3: metric families, envelope fields, write guard.
    short = name.replace("lan-", "")
    status, mtext, ctype = call(base, "/api/metrics")
    missing = [f for f in METRIC_FAMILIES if f"lanstudio_{short}_{f}" not in mtext]
    check(f"ecosystem {name} 9 mandatory metric families", status == 200
          and not missing, f"missing={missing} ctype={ctype}")
    check(f"ecosystem {name} unified envelope ok/data/request_id", status == 200
          and all(k in body for k in ("ok", "data", "request_id")), raw[:160])
    status, denied, _ = jcall(base, "/api/config", "POST", {"text": "version: 1\n"})
    check(f"ecosystem {name} write protection 409 setup_required", status == 409
          and err_code(denied) == "setup_required", str(denied)[:160])


# ------------------------------------------- 4. Shell navigation to running


def shell_probe_manifest(ports: dict) -> str:
    """Derive a probe_enabled=true manifest from shell/components.yaml but with
    isolated high ports, so the shell's health probes hit the ports this gate
    actually starts components on."""
    src = (ROOT / "shell" / "components.yaml").read_text(encoding="utf-8")
    entries = []
    cur = {}
    for line in src.splitlines():
        stripped = line.strip()
        if stripped.startswith("#") or not stripped:
            continue
        if stripped.startswith("- name:"):
            if cur:
                entries.append(cur)
            cur = {"name": stripped.split(":", 1)[1].strip()}
        elif ":" in stripped and cur:
            key, value = stripped.split(":", 1)
            cur[key.strip()] = value.strip().strip('"')
    if cur:
        entries.append(cur)
    lines = ["version: 1", "probe_enabled: true", "probe_interval_seconds: 1",
             "probe_timeout_ms: 300", "components:"]
    for entry in entries:
        name = entry.get("name", "")
        port = ports.get(name, entry.get("port", "0"))
        lines += [f"  - name: {name}", f"    label: {entry.get('label', '')}",
                  f"    port: {port}", f"    icon: {entry.get('icon', '')}",
                  f"    tone: {entry.get('tone', '')}"]
    return "\n".join(lines) + "\n"


def shell_navigation(workdir: Path) -> None:
    section("4. Shell navigation to already-running components")
    chosen = ["lan-hosts", "lan-wol"]
    for name in chosen:
        wd = workdir / name
        wd.mkdir(parents=True, exist_ok=True)
        try:
            start(name, ECO_PORTS[name], wd)
        except Exception as exc:
            check(f"ecosystem shell nav: start {name}", False,
                  f"{type(exc).__name__}: {exc}")
    shell_wd = workdir / "shell"
    shell_wd.mkdir(parents=True, exist_ok=True)
    try:
        shell_base = start_shell(shell_wd, shell_probe_manifest(ECO_PORTS))
    except Exception as exc:
        check("ecosystem shell nav: start lan-shell", False,
              f"{type(exc).__name__}: {exc}")
        return

    def both_online():
        _, body, _ = jcall(shell_base, "/api/shell/status")
        comps = data_of(body).get("components", [])
        return all(any(isinstance(c, dict) and c.get("name") == n
                       and c.get("online") is True for c in comps)
                   for n in chosen)

    ready = poll_until(both_online, 20.0, 0.5)
    status, body, raw = jcall(shell_base, "/api/shell/status")
    data = data_of(body)
    comps = data.get("components", [])
    by_name = {c.get("name"): c for c in comps if isinstance(c, dict)}
    online_ok = ready and all(isinstance(by_name.get(n), dict)
                              and by_name[n].get("online") is True for n in chosen)
    offline_ok = all((by_name.get(n, {}).get("online") is False
                      or by_name.get(n, {}).get("online") is None)
                     for n in COMPONENTS if n not in chosen)
    check("ecosystem shell nav: running components online=true", online_ok, raw[:220])
    check("ecosystem shell nav: stopped components online=false/null", offline_ok,
          raw[:220])
    check("ecosystem shell nav: status lists all 8 components", data.get("total") == 8
          and len(comps) == 8, raw[:160])

    consistent = True
    for name in COMPONENTS:
        health_status, _, _ = jcall(f"http://127.0.0.1:{ECO_PORTS[name]}", "/healthz")
        reachable = health_status == 200
        shell_online = by_name.get(name, {}).get("online")
        if name in chosen:
            consistent = consistent and reachable and shell_online is True
        else:
            consistent = consistent and (not reachable) and (
                shell_online is False or shell_online is None)
    check("ecosystem shell nav: online flags match real /healthz", consistent,
          str({n: by_name.get(n, {}).get("online") for n in COMPONENTS}))


# ------------------------------------------------------ 5. release artifacts


def release_artifacts() -> None:
    section("5. Release artifacts")
    rel_path = ROOT / ".github" / "workflows" / "release.yml"
    try:
        rel = rel_path.read_text(encoding="utf-8")
    except OSError as exc:
        rel = ""
        check("ecosystem release.yml readable", False, str(exc))
    check("ecosystem release.yml lists all 8 component names",
          all(n in rel for n in COMPONENTS),
          str([n for n in COMPONENTS if n not in rel]))
    check("ecosystem release.yml builds 2 musl targets (x86_64 + aarch64)",
          "x86_64" in rel and "aarch64" in rel and "musl" in rel, rel[:160])
    check("ecosystem release.yml emits a sha256 checksum manifest", "sha256" in rel,
          rel[:160])
    install = ROOT / "scripts" / "install.sh"
    check("ecosystem scripts/install.sh exists", install.exists(), str(install))
    if install.exists():
        proc = subprocess.run(["bash", "-n", str(install)], capture_output=True,
                              text=True, timeout=20)
        check("ecosystem scripts/install.sh is valid bash (bash -n)",
              proc.returncode == 0, proc.stderr)


# ---------------------------------------------------- 6. documentation DoD


def doc_consistency() -> None:
    section("6. Documentation consistency")
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    manifest_text = (ROOT / "shell" / "components.yaml").read_text(encoding="utf-8")
    manifest_names = re.findall(r"^\s*- name:\s*(lan-[a-z0-9-]+)", manifest_text, re.M)
    check("ecosystem manifest lists exactly the 8 component names",
          len(manifest_names) == 8 and set(manifest_names) == set(COMPONENTS),
          str(manifest_names))
    check("ecosystem README lists all 8 component names",
          all(n in readme for n in manifest_names),
          str([n for n in manifest_names if n not in readme]))

    manifest_ports = [int(p) for p in re.findall(r"^\s*port:\s*(\d+)", manifest_text, re.M)]
    check("ecosystem manifest ports are exactly 8080..8087",
          sorted(manifest_ports) == list(range(8080, 8088)), str(manifest_ports))
    check("ecosystem README documents ports 8080..8087",
          all(str(p) in readme for p in range(8080, 8088)),
          str([p for p in range(8080, 8088) if str(p) not in readme]))

    check("ecosystem docs/M5-CHECKLIST.md exists",
          (ROOT / "docs" / "M5-CHECKLIST.md").exists(),
          str(ROOT / "docs" / "M5-CHECKLIST.md"))

    api = (ROOT / "contracts" / "openapi.yaml").read_text(encoding="utf-8")
    endpoint_groups = {
        "lan-mdns": ["/api/records", "/api/probe", "/api/browse/unified"],
        "lan-relay": ["/api/relay/stats", "/api/relay/inject"],
        "lan-ca": ["/api/ca/root", "/api/ca/issue"],
        "lan-scan": ["/api/scan/status", "/api/scan/start"],
        "lan-hosts": ["/api/hosts/mappings", "/api/hosts/publish"],
        "lan-wol": ["/api/wol/devices", "/api/wol/wake"],
        "lan-sync": ["/api/sync/nodes", "/api/sync/pull"],
        "lan-ssdp": ["/api/ssdp/devices", "/api/ssdp/refresh"],
    }
    missing = [p for paths in endpoint_groups.values() for p in paths if p not in api]
    check("ecosystem openapi.yaml documents all 8 components' proprietary endpoints",
          not missing, f"missing={missing}")

    metrics = (ROOT / "docs" / "METRICS.md").read_text(encoding="utf-8")
    check("ecosystem docs/METRICS.md contains section 3.7", "3.7" in metrics,
          metrics[:120])
    adr7 = list((ROOT / "docs" / "ADR").glob("0007-*"))
    check("ecosystem docs/ADR/0007-* exists", len(adr7) >= 1,
          str([p.name for p in adr7]))
    stale = [w for w in ("待启动", "尚未提交") if w in readme]
    check("ecosystem README has no stale status wording", not stale,
          f"stale={stale}")


def main() -> int:
    global BIN_DIR
    if ((ROOT / "target" / "release" / "lan-sync").exists()
            and (ROOT / "target" / "release" / "lan-ssdp").exists()):
        BIN_DIR = ROOT / "target" / "release"
    tmp = tempfile.TemporaryDirectory(prefix="lan-studio-eco-")
    root = Path(tmp.name)
    try:
        print(f"ECOSYSTEM acceptance workspace: {root}")
        section1(root / "deliverables")
        error_code_consistency()

        section("2 + 3. Standalone deployment (serial) + contract zero-drift")
        solo = root / "solo"
        solo.mkdir()
        for name in COMPONENTS:
            wd = solo / name
            wd.mkdir()
            # one component at a time: start, assert, stop, then the next
            safe_phase(f"ecosystem solo {name}",
                       lambda n=name, p=ECO_PORTS[name], d=wd: per_component(n, p, d))
            terminate_all()

        shell_navigation(root / "nav")
        terminate_all()

        release_artifacts()
        doc_consistency()
    except Exception as exc:  # noqa: BLE001
        check("ECOSYSTEM acceptance harness", False, f"{type(exc).__name__}: {exc}")
    finally:
        terminate_all()
        tmp.cleanup()

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    failed = [n for n, ok, _ in RESULTS if not ok]
    print(f"\nECOSYSTEM GATE: {passed} passed / {len(failed)} failed")
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("ECOSYSTEM OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
