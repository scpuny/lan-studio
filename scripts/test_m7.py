#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M7 gate: lan-webui is the single source of truth for which modules render.

The unified console (frontend) renders ONLY what `lan-webui/capabilities.json`
declares, and that manifest is generated from `lan-webui/modules.yaml`. This gate
asserts the chain is intact:

  1. `modules.yaml` declares the enabled components (and libs).
  2. `capabilities.json` is the *aggregate* identity and lists exactly those
     components — no more, no less. Removing a component from `modules.yaml`
     therefore removes it from the frontend DOM (the "backend controls the
     frontend" property the user asked for).
  3. `capabilities.allowlist.txt` never references an undeclared component, so
     the bounded same-origin proxy cannot reach a component the console does not
     show.
  4. `gen_capabilities.py --check` is green (no drift, all 8 manifests match the
     backend route registrations).

A failure here means the console would show a module the backend does not serve,
or vice versa.
"""

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AGGREGATE = ROOT / "lan-webui" / "capabilities.json"
ALLOWLIST = ROOT / "lan-webui" / "capabilities.allowlist.txt"
MODULES = ROOT / "lan-webui" / "modules.yaml"
GENERATOR = ROOT / "scripts" / "gen_capabilities.py"


def parse_modules_yaml(text: str) -> tuple[list[str], list[str]]:
    components: list[str] = []
    libs: list[str] = []
    in_libs = False
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line == "enabled_components:":
            in_libs = False
            continue
        if line == "libs:":
            in_libs = True
            continue
        if in_libs:
            if line.startswith("- "):
                lib = line[2:].strip()
                if lib:
                    libs.append(lib)
            continue
        if line.startswith("- name:"):
            name = line.split(":", 1)[1].strip()
            components.append(name)
    return components, libs


def fail(message: str) -> "NoReturn":  # type: ignore[name-defined]
    print(f"FAIL {message}")
    sys.exit(1)


def main() -> int:
    if not MODULES.exists():
        fail("lan-webui/modules.yaml missing")
    text = MODULES.read_text(encoding="utf-8")
    enabled, libs = parse_modules_yaml(text)
    if not enabled:
        fail("modules.yaml declares no enabled_components")

    if not AGGREGATE.exists():
        fail("lan-webui/capabilities.json missing (run gen_capabilities.py --aggregate)")
    aggregate = json.loads(AGGREGATE.read_text(encoding="utf-8"))
    if aggregate.get("component") != "lan-webui":
        fail(f"aggregate identity is not lan-webui: {aggregate.get('component')}")

    component_names = sorted(item["name"] for item in aggregate["components"])
    if component_names != sorted(enabled):
        fail(
            f"aggregate components {component_names} != modules.yaml enabled {sorted(enabled)}"
        )

    if not ALLOWLIST.exists():
        fail("lan-webui/capabilities.allowlist.txt missing")
    for line in ALLOWLIST.read_text(encoding="utf-8").splitlines():
        name = line.split("|", 1)[0]
        if name not in enabled:
            fail(f"proxy allowlist references undeclared component {name}")

    for lib in libs:
        if lib not in aggregate.get("libs", []):
            fail(f"lib {lib} missing from aggregate libs")

    result = subprocess.run(
        [sys.executable, str(GENERATOR), "--check"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(result.stdout)
        print(result.stderr)
        fail("gen_capabilities.py --check failed (backend/manifest drift)")

    print(
        f"M7 OK: {len(enabled)} components, {len(libs)} libs, "
        "aggregate == modules.yaml, proxy allowlist bounded"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
