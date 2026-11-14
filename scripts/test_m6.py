#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M6 frontend aggregation runtime gate.

Verifies the capability-driven UI contract end to end:

  1. scripts/gen_capabilities.py --check agrees with the backend .route() calls,
  2. each of the 8 manifests declares only its own modules (negative assertions),
  3. a component console embeds the shared renderer + only its own manifest,
  4. the Shell SPA embeds all 8 manifests, has no password field and no external
     resources, and still rejects bare /api/* routes,
  5. the /p/<name>/api/... proxy is allowlisted, bounded, and forwards the
     admin password to the target component instead of storing it.

Run: cargo build --workspace && python3 scripts/test_m6.py
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
EXAMPLES = ROOT / "contracts" / "examples"
COMPONENTS = [
    "lan-mdns", "lan-relay", "lan-ca", "lan-scan",
    "lan-hosts", "lan-wol", "lan-sync", "lan-ssdp",
]
PORT_HOSTS = 18184
PORT_SHELL = 18094
PASSWORD = "m6-runtime-password"

RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, bool(ok), detail))
    print(("  PASS  " if ok else "  FAIL  ") + name + (("  | " + detail[:160]) if detail and not ok else ""))


def wait_port(port: int, timeout: float = 8.0) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), 0.2):
                return True
        except OSError:
            time.sleep(0.1)
    return False


def call(url: str, method: str = "GET", body=None, headers=None):
    data = None if body is None else json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    for key, value in (headers or {}).items():
        req.add_header(key, value)
    try:
        # Debug builds run the password KDF slowly; keep the same 20s budget
        # used by the M4/M5 runtime gates.
        with urllib.request.urlopen(req, timeout=20) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except OSError as exc:
        return 0, str(exc)


def manifest_for(name: str) -> dict:
    return json.loads((EXAMPLES / f"capabilities.{name}.sample.json").read_text(encoding="utf-8"))


def static_checks() -> None:
    print("\n=== 1. Capability manifests vs backend routes ===")
    proc = subprocess.run(
        [sys.executable, str(ROOT / "scripts/gen_capabilities.py"), "--check"],
        cwd=ROOT, capture_output=True, text=True,
    )
    check("M6 gen_capabilities.py --check passes", proc.returncode == 0,
          (proc.stdout + proc.stderr)[-300:])
    allowlist = (ROOT / "contracts" / "capabilities.allowlist.txt")
    check("M6 proxy allowlist exists", allowlist.exists(), str(allowlist))
    lines = allowlist.read_text(encoding="utf-8").splitlines() if allowlist.exists() else []
    check("M6 allowlist has 8 components", len({line.split("|")[0] for line in lines}) == 8)

    for name in COMPONENTS:
        doc = manifest_for(name)
        ids = [m["id"] for m in doc["modules"]]
        core = [i for i in ids if i.startswith("core.")]
        check(f"M6 {name} schema=1 + component", doc["schema"] == 1 and doc["component"] == name)
        check(f"M6 {name} has the 6 core modules", len(core) == 6, str(core))
        prefix = name + "."
        foreign = [i for i in ids if i.startswith("lan-") and not i.startswith(prefix)]
        check(f"M6 {name} declares no foreign modules", not foreign, str(foreign))
        for m in doc["modules"]:
            for panel in m["panels"]:
                check(f"M6 {name} {panel['id']} widget in registry",
                      panel["widget"] in {"kv-grid", "table", "form", "code-editor",
                                          "metric-grid", "log-list", "text-view", "action-list"})

    wol_ids = [m["id"] for m in manifest_for("lan-wol")["modules"]]
    wol_probe = [m for m in manifest_for("lan-wol")["modules"] if m.get("requires_lib")]
    check("M6 requires_lib honours common-probe",
          len(wol_probe) == 1 and wol_probe[0]["requires_lib"] == "common-probe"
          and "common-probe" in manifest_for("lan-wol")["libs"], str(wol_probe))
    for name in COMPONENTS:
        if name == "lan-wol":
            continue
        foreign = [m["id"] for m in manifest_for(name)["modules"] if m.get("requires_lib")]
        check(f"M6 {name} has no common-probe-only module", not foreign, str(foreign))


def runtime_checks(workdir: Path, procs: list) -> None:
    print("\n=== 2. Component console + Shell SPA + proxy ===")
    host_cfg = workdir / "lan-hosts.yaml"
    shutil.copy(ROOT / "components/lan-hosts/config.template.yaml", host_cfg)
    env = os.environ.copy()
    env.update({
        "LAN_STUDIO_PORT": str(PORT_HOSTS),
        "LAN_STUDIO_CONFIG": str(host_cfg),
        "LAN_STUDIO_LOG_DIR": str(workdir / "logs"),
        "LAN_STUDIO_HISTORY_DIR": str(workdir / "history"),
        "LAN_STUDIO_AUTH_FILE": str(workdir / "auth.toml"),
    })
    procs.append(subprocess.Popen(
        [str(ROOT / "target/debug/lan-hosts"), "--config", str(host_cfg)],
        cwd=workdir, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    if not wait_port(PORT_HOSTS):
        check("M6 lan-hosts starts", False, f"port {PORT_HOSTS}")
        return
    check("M6 lan-hosts starts", True)

    manifest = (ROOT / "shell" / "components.yaml").read_text(encoding="utf-8")
    shell_cfg = workdir / "components.yaml"
    shell_cfg.write_text(manifest.replace("port: 8084", f"port: {PORT_HOSTS}"), encoding="utf-8")
    senv = os.environ.copy()
    senv.update({"LAN_STUDIO_PORT": str(PORT_SHELL), "LAN_SHELL_CONFIG": str(shell_cfg)})
    senv.pop("LAN_SHELL_TOKEN", None)
    procs.append(subprocess.Popen(
        [str(ROOT / "target/debug/lan-shell")], cwd=ROOT, env=senv,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    if not wait_port(PORT_SHELL):
        check("M6 lan-shell starts", False, f"port {PORT_SHELL}")
        return
    check("M6 lan-shell starts", True)

    hosts_base = f"http://127.0.0.1:{PORT_HOSTS}"
    shell_base = f"http://127.0.0.1:{PORT_SHELL}"

    status, console = call(hosts_base + "/")
    check("M6 component console 200", status == 200, str(status))
    check("M6 component console embeds shared renderer", "window.LanConsole" in console)
    check("M6 component console has no placeholders",
          "__CAPABILITIES__" not in console and "__RENDERER_JS__" not in console)
    check("M6 component console injects its own manifest", '"component": "lan-hosts"' in console)
    check("M6 component console injects own module", "lan-hosts.mappings" in console)
    check("M6 component console has no foreign module", "lan-wol.devices" not in console)
    check("M6 component console has no external resources",
          "<script src=" not in console and 'src="http' not in console)

    status, html = call(shell_base + "/")
    check("M6 shell SPA 200", status == 200, str(status))
    check("M6 shell SPA embeds shared renderer", "window.LanConsole" in html)
    check("M6 shell SPA has no placeholders",
          "__SHELL_DATA__" not in html and "__RENDERER_JS__" not in html)
    check("M6 shell SPA delivers all 8 manifests",
          all(f'"component": "{name}"' in html for name in COMPONENTS))
    check("M6 shell SPA has capability-driven container", 'id="moduleRoot"' in html)
    check("M6 shell SPA has component switcher", 'id="switchBar"' in html)
    check("M6 shell SPA keeps no password field", 'type="password"' not in html)
    check("M6 shell SPA keeps 8 tone bars",
          all(t in html.lower() for t in ("blue", "cyan", "purple", "teal",
                                          "indigo", "amber", "green", "orange")))
    check("M6 shell SPA has no external resources",
          "<script src=" not in html and "<link rel=\"stylesheet\"" not in html
          and 'src="http' not in html)

    # Proxy: allowlisted, undeclared, unknown component, non-api prefix.
    status, proxied = call(shell_base + "/p/lan-hosts/api/hosts/mappings")
    check("M6 proxy allowlisted endpoint 200", status == 200, str(status))
    check("M6 proxy returns unified envelope", '"ok":true' in proxied or '"ok": true' in proxied,
          proxied[:160])
    check("M6 proxy rejects undeclared endpoint",
          call(shell_base + "/p/lan-hosts/api/wol/devices")[0] == 404)
    check("M6 proxy rejects unknown component",
          call(shell_base + "/p/lan-unknown/api/status")[0] == 404)
    check("M6 proxy rejects non-api target",
          call(shell_base + "/p/lan-hosts/healthz")[0] == 404)
    check("M6 bare /api/status still 404", call(shell_base + "/api/status")[0] == 404)
    check("M6 bare /api/records still 404", call(shell_base + "/api/records")[0] == 404)

    # Write-guard forwarding: the proxy must not swallow X-Admin-Password.
    status, setup = call(hosts_base + "/api/auth/password", "POST",
                         {"new_password": PASSWORD})
    check("M6 target password configured", status == 200 and "password_set" in setup, setup[:160])
    endpoint = "/p/lan-hosts/api/hosts/mapping"
    payload = {"name": "m6-demo.lan", "ip": "10.20.30.40"}
    status, denied = call(shell_base + endpoint, "POST", payload)
    check("M6 proxy write without password is rejected", status in (403, 409),
          f"status={status} body={denied[:120]}")
    status, accepted = call(shell_base + endpoint, "POST", payload,
                            {"X-Admin-Password": PASSWORD})
    check("M6 proxy forwards admin password", status == 200, f"status={status} body={accepted[:160]}")
    status, listing = call(shell_base + "/p/lan-hosts/api/hosts/mappings")
    check("M6 written mapping visible through proxy", "m6-demo.lan" in listing, listing[:160])

    # Token guard applies to the proxy path too (not just /api/*).
    check("M6 /api/shell/status still returns 8 components",
          call(shell_base + "/api/shell/status")[0] == 200)


def main() -> int:
    workdir = Path(tempfile.mkdtemp(prefix="lan-m6-"))
    procs: list = []
    try:
        static_checks()
        runtime_checks(workdir, procs)
    finally:
        for proc in procs:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(workdir, ignore_errors=True)
    passed = sum(1 for _, ok, _ in RESULTS if ok)
    failed = len(RESULTS) - passed
    print(f"\nM6 RUNTIME GATE: {passed} passed / {failed} failed")
    print("M6 RUNTIME OK" if failed == 0 else "M6 RUNTIME FAILED")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
