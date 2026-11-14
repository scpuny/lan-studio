#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M0 contract verifier for Lan Studio.

Zero mandatory third-party dependency. PyYAML is optional: when present the
OpenAPI document and the sample YAML config are also parsed and validated.
Without PyYAML the 8 YAML-based checks are skipped with a prominent warning.
Run: python3 scripts/verify_contracts.py
"""
from __future__ import annotations

import datetime
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACTS = ROOT / "contracts"
EXAMPLES = CONTRACTS / "examples"
DOCS = ROOT / "docs"

RESULTS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((name, bool(ok), detail))


TYPE_MAP = {
    "object": dict,
    "array": list,
    "string": str,
    "boolean": bool,
}

DATETIME_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$")


def validate(inst, schema: dict, path: str = "$") -> list[str]:
    """Small JSON Schema subset: type, const, enum, pattern, length, bounds,
    required, properties, additionalProperties=false, items, date-time."""
    errs: list[str] = []
    kind = schema.get("type")

    if kind == "integer":
        if isinstance(inst, bool) or not isinstance(inst, int):
            errs.append(f"{path}: expected integer")
    elif kind == "number":
        if isinstance(inst, bool) or not isinstance(inst, (int, float)):
            errs.append(f"{path}: expected number")
    elif kind in TYPE_MAP and not isinstance(inst, TYPE_MAP[kind]):
        errs.append(f"{path}: expected {kind}")

    if "const" in schema and inst != schema["const"]:
        errs.append(f"{path}: must equal {schema['const']!r}")
    if "enum" in schema and inst not in schema["enum"]:
        errs.append(f"{path}: not in enum {schema['enum']!r}")

    if isinstance(inst, str):
        if "pattern" in schema and not re.search(schema["pattern"], inst):
            errs.append(f"{path}: does not match {schema['pattern']!r}")
        if "minLength" in schema and len(inst) < schema["minLength"]:
            errs.append(f"{path}: shorter than minLength")
        if "maxLength" in schema and len(inst) > schema["maxLength"]:
            errs.append(f"{path}: longer than maxLength")
        if schema.get("format") == "date-time":
            if not DATETIME_RE.match(inst):
                errs.append(f"{path}: not RFC3339 UTC (must end with Z)")
            else:
                try:
                    datetime.datetime.fromisoformat(inst.replace("Z", "+00:00"))
                except ValueError as exc:
                    errs.append(f"{path}: invalid date-time: {exc}")

    if isinstance(inst, (int, float)) and not isinstance(inst, bool):
        if "minimum" in schema and inst < schema["minimum"]:
            errs.append(f"{path}: below minimum")
        if "exclusiveMinimum" in schema and inst <= schema["exclusiveMinimum"]:
            errs.append(f"{path}: must be > exclusiveMinimum")
        if "maximum" in schema and inst > schema["maximum"]:
            errs.append(f"{path}: above maximum")

    if isinstance(inst, dict):
        for required in schema.get("required", []):
            if required not in inst:
                errs.append(f"{path}: missing required '{required}'")
        props = schema.get("properties", {})
        for key, value in inst.items():
            if key in props:
                errs.extend(validate(value, props[key], f"{path}.{key}"))
            elif schema.get("additionalProperties") is False:
                errs.append(f"{path}: unexpected property '{key}'")

    if isinstance(inst, list) and isinstance(schema.get("items"), dict):
        for index, value in enumerate(inst):
            errs.extend(validate(value, schema["items"], f"{path}[{index}]"))

    return errs


SCHEMA_FILES = {
    "config.schema": CONTRACTS / "config.schema.json",
    "alert-event": CONTRACTS / "alert-event.json",
    "log-record": CONTRACTS / "log-record.json",
    "snapshot": CONTRACTS / "snapshot.json",
}

SAMPLE_PAIRS = [
    ("alert-event", "alert-event.sample.json"),
    ("log-record", "log-record.sample.json"),
    ("snapshot", "snapshot.sample.json"),
]

REQUIRED_PATHS = [
    "/healthz", "/readyz", "/api/status", "/api/config",
    "/api/config/snapshot", "/api/config/history", "/api/config/rollback",
    "/api/reload", "/api/logs", "/api/logs/stream", "/api/metrics",
    "/api/alerts", "/api/auth", "/api/auth/password", "/api/auth/verify",
    "/api/records", "/api/probe", "/api/interfaces", "/api/health/targets",
    "/api/queries", "/api/browse", "/api/conflicts",
    # M4 component endpoints (PRD 4.6 / 4.7)
    "/api/hosts/mappings", "/api/hosts/mapping", "/api/hosts/remove",
    "/api/hosts/import", "/api/hosts/render", "/api/hosts/publish",
    "/api/hosts/status",
    "/api/wol/devices", "/api/wol/device", "/api/wol/remove",
    "/api/wol/wake", "/api/wol/probe", "/api/wol/status", "/api/wol/history",
]

MANDATORY_METRICS = [
    "lanstudio_<c>_up",
    "lanstudio_<c>_uptime_seconds",
    "lanstudio_<c>_build_info",
    "lanstudio_<c>_config_reloads_total",
    "lanstudio_<c>_config_saves_total",
    "lanstudio_<c>_http_requests_total",
    "lanstudio_<c>_alerts_sent_total",
    "lanstudio_<c>_log_disk_bytes",
    "lanstudio_<c>_log_disk_limit_bytes",
]

REGISTERED_KINDS = [
    "health_transition", "name_conflict", "config_invalid",
    "config_reload_failed", "gateway_target_unreachable",
    "relay_loop_detected", "relay_ttl_exceeded", "cert_expiring",
    "cert_revoked", "cert_issued", "scan_anomaly", "sync_conflict",
    "device_wake_failed", "wol_wake_sent", "ssdp_discovery_error",
    "hosts_publish_failed", "hosts_conflict_detected",
]


def load_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    schemas: dict[str, dict] = {}
    for name, path in SCHEMA_FILES.items():
        try:
            obj = load_json(path)
            schemas[name] = obj
            check(f"schema {name}.json parses", True)
            check(f"schema {name}.json declares $schema/$id/type",
                  all(k in obj for k in ("$schema", "$id", "type")),
                  str(sorted(obj.keys())))
        except (OSError, json.JSONDecodeError) as exc:
            check(f"schema {name}.json parses", False, str(exc))

    for schema_name, sample_name in SAMPLE_PAIRS:
        if schema_name not in schemas:
            continue
        try:
            sample = load_json(EXAMPLES / sample_name)
            errs = validate(sample, schemas[schema_name])
            check(f"example {sample_name} passes {schema_name}.json",
                  not errs, "; ".join(errs[:6]))
        except (OSError, json.JSONDecodeError) as exc:
            check(f"example {sample_name} passes {schema_name}.json", False, str(exc))

    yaml_full = True
    try:
        import yaml
    except ImportError:
        yaml_full = False
        print("WARN  PyYAML not installed: 8 YAML-based checks SKIPPED "
              "(pip install pyyaml for the full 21-check run)")
    else:
        if "config.schema" in schemas:
            try:
                cfg = yaml.safe_load((EXAMPLES / "config.sample.yaml").read_text(encoding="utf-8"))
                errs = validate(cfg, schemas["config.schema"])
                check("example config.sample.yaml passes config.schema.json",
                      not errs, "; ".join(errs[:6]))
            except (OSError, yaml.YAMLError) as exc:
                check("example config.sample.yaml parses", False, str(exc))

        try:
            oapi = yaml.safe_load((CONTRACTS / "openapi.yaml").read_text(encoding="utf-8"))
            check("openapi.yaml parses", True)
            check("openapi version is 3.0.3", oapi.get("openapi") == "3.0.3",
                  str(oapi.get("openapi")))
            paths = oapi.get("paths", {})
            missing = [p for p in REQUIRED_PATHS if p not in paths]
            check("openapi covers all mandatory endpoints", not missing, str(missing))
            enum = oapi.get("components", {}).get("schemas", {}).get("ErrorCode", {}).get("enum", [])
            check("openapi ErrorCode has 8 values", len(enum) == 8, str(enum))
            sec = oapi.get("components", {}).get("securitySchemes", {})
            check("openapi declares both auth schemes",
                  "AdminToken" in sec and "AdminPassword" in sec, str(sorted(sec)))
        except (OSError, yaml.YAMLError) as exc:
            check("openapi.yaml parses", False, str(exc))
            enum = []

        errdoc = (DOCS / "ERROR-CODES.md").read_text(encoding="utf-8")
        missing_codes = [c for c in enum if c not in errdoc]
        check("ERROR-CODES.md covers every ErrorCode", not missing_codes,
              str(missing_codes))

        metdoc = (DOCS / "METRICS.md").read_text(encoding="utf-8")
        missing_metrics = [m for m in MANDATORY_METRICS if m not in metdoc]
        check("METRICS.md covers all 9 mandatory metric families",
              not missing_metrics, str(missing_metrics))

        altdoc = (DOCS / "ALERT-SCHEMA.md").read_text(encoding="utf-8")
        missing_kinds = [k for k in REGISTERED_KINDS if k not in altdoc]
        check("ALERT-SCHEMA.md registers all kinds", not missing_kinds,
              str(missing_kinds))

    prd = (DOCS / "PRD.md").read_text(encoding="utf-8")
    check("PRD.md present and defines M0", "M0 契约冻结" in prd and len(prd) > 10000,
          f"{len(prd)} chars")

    failed = [r for r in RESULTS if not r[1]]
    for name, ok, detail in RESULTS:
        line = f"{'PASS' if ok else 'FAIL'}  {name}"
        if detail:
            line += f"  | {detail}"
        print(line)
    suffix = "" if yaml_full else "  [DEGRADED: YAML checks skipped]"
    print(f"\n{len(RESULTS) - len(failed)} passed / {len(failed)} failed{suffix}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
