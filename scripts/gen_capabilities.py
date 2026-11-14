#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Derive the frontend capability manifests from the backend route registrations.

The backend is the single source of truth: every `.route(...)` / `.route_write(...)`
in components/*/src/main.rs must be represented by exactly one panel endpoint or
action in contracts/examples/capabilities.<name>.sample.json.  The frontend then
renders only the declared modules, so "which page elements exist" is decided by
"which modules the backend references", never by hardcoded frontend markup.

Usage:
    python3 scripts/gen_capabilities.py            # regenerate all 8 samples
    python3 scripts/gen_capabilities.py --check   # CI: fail on any drift
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXAMPLES = ROOT / "contracts" / "examples"
COMPONENTS = ROOT / "components"
OPENAPI = ROOT / "contracts" / "openapi.yaml"
SCHEMA = ROOT / "contracts" / "capabilities.schema.json"
ALLOWLIST = ROOT / "contracts" / "capabilities.allowlist.txt"
MANIFEST = ROOT / "shell" / "components.yaml"

COMPONENT_NAMES = [
    "lan-mdns", "lan-relay", "lan-ca", "lan-scan",
    "lan-hosts", "lan-wol", "lan-sync", "lan-ssdp",
]

# Endpoints supplied by common-component::standard_routes() (18 operations).
CORE_PATHS = {
    "/api/status", "/api/metrics", "/api/config", "/api/config/snapshot",
    "/api/config/history", "/api/config/rollback", "/api/reload",
    "/api/logs", "/api/logs/stream", "/api/alerts", "/api/auth",
    "/api/auth/password", "/api/auth/verify",
}

WIDGETS = {
    "kv-grid", "table", "form", "code-editor",
    "metric-grid", "log-list", "text-view", "action-list",
}
TONES = {"blue", "indigo", "green", "cyan", "purple", "amber", "teal", "red"}
AREAS = {"workspace", "feature"}
ICONS = {
    "grid", "sliders", "activity", "terminal", "bell", "history", "dns",
    "relay", "cert", "scan", "hosts", "power", "sync", "discover", "cube",
}

ROUTE_RE = re.compile(
    r'\.route(_write)?\(\s*"(GET|POST|PUT|DELETE)"\s*,\s*"(/api/[^"]+)"'
)


def field(key, label, kind="text", required=True):
    return {"key": key, "label": label, "type": kind, "required": required}


def col(key, label):
    return {"key": key, "label": label}


def act(aid, label, method, path, write=False, fields=None):
    out = {"id": aid, "label": label, "method": method, "path": path}
    if write:
        out["write"] = True
    if fields:
        out["fields"] = fields
    return out


def pan(pid, title, widget, method, path, columns=None, actions=None):
    out = {
        "id": pid,
        "title": title,
        "widget": widget,
        "endpoint": {"method": method, "path": path},
    }
    if columns:
        out["columns"] = columns
    if actions:
        out["actions"] = actions
    return out


def mod(mid, title, icon, tone, area, panels, requires_lib=None):
    out = {
        "id": mid, "title": title, "icon": icon,
        "tone": tone, "area": area, "panels": panels,
    }
    if requires_lib:
        out["requires_lib"] = requires_lib
    return out


def core_modules():
    """The six workspace areas present on every component."""
    return [
        mod("core.status", "状态总览", "grid", "blue", "workspace", [
            pan("status.kv", "运行状态", "kv-grid", "GET", "/api/status"),
        ]),
        mod("core.config", "配置管理", "sliders", "indigo", "workspace", [
            pan("config.editor", "配置编辑器", "code-editor", "GET", "/api/config", actions=[
                act("save", "校验并保存", "POST", "/api/config", True),
                act("reload", "重新载入", "POST", "/api/reload", True),
                act("export", "导出快照", "GET", "/api/config/snapshot"),
                act("import", "导入快照", "POST", "/api/config/snapshot", True),
            ]),
        ]),
        mod("core.observe", "可观测", "activity", "cyan", "workspace", [
            pan("observe.metrics", "Prometheus 指标", "text-view", "GET", "/api/metrics"),
            pan("observe.logs", "最近日志", "log-list", "GET", "/api/logs"),
        ]),
        mod("core.logs", "日志", "terminal", "teal", "workspace", [
            pan("logs.list", "运行日志", "log-list", "GET", "/api/logs"),
        ]),
        mod("core.alerts", "告警", "bell", "amber", "workspace", [
            pan("alerts.kv", "告警通道", "kv-grid", "GET", "/api/alerts"),
        ]),
        mod("core.history", "版本历史", "history", "purple", "workspace", [
            pan("history.list", "快照与回滚", "table", "GET", "/api/config/history", actions=[
                act("rollback", "回滚", "POST", "/api/config/rollback", True),
            ]),
        ]),
    ]


def proprietary_modules(name):
    """Page modules for the endpoints a component registered itself."""
    p = name.replace("lan-", "")
    pid = lambda suffix: "lan-" + p + "." + suffix  # noqa: E731
    if name == "lan-mdns":
        return [
            mod(pid("records"), "资源记录", "dns", "blue", "feature", [
                pan("records.table", "已发布记录", "table", "GET", "/api/records",
                    columns=[col("name", "名称"), col("type", "类型"), col("value", "值")]),
                pan("records.interfaces", "网卡", "kv-grid", "GET", "/api/interfaces"),
                pan("records.queries", "查询审计", "log-list", "GET", "/api/queries"),
                pan("records.probe", "主动探测", "action-list", "GET", "/api/browse", actions=[
                    act("probe", "发起探测", "POST", "/api/probe", True,
                        [field("name", "名称"), field("type", "类型", required=False)]),
                ]),
            ]),
            mod(pid("browse"), "服务浏览", "discover", "cyan", "feature", [
                pan("browse.table", "统一服务记录", "table", "GET", "/api/browse/unified",
                    columns=[col("name", "名称"), col("type", "类型"), col("address", "地址"), col("source", "来源")]),
                pan("browse.conflicts", "冲突检测", "kv-grid", "GET", "/api/conflicts"),
                pan("browse.targets", "健康目标", "kv-grid", "GET", "/api/health/targets"),
            ]),
        ]
    if name == "lan-relay":
        return [
            mod(pid("stats"), "中继统计", "relay", "cyan", "feature", [
                pan("relay.stats", "转发与环路计数", "kv-grid", "GET", "/api/relay/stats"),
                pan("relay.interfaces", "出入网卡", "table", "GET", "/api/relay/interfaces"),
            ]),
            mod(pid("inject"), "环路注入", "activity", "amber", "feature", [
                pan("relay.inject", "注入测试帧", "form", "GET", "/api/relay/stats", actions=[
                    act("inject", "注入", "POST", "/api/relay/inject", True),
                    act("mark-local", "标记本地包", "POST", "/api/relay/mark-local", True),
                ]),
            ]),
        ]
    if name == "lan-ca":
        return [
            mod(pid("root"), "根凭证", "cert", "purple", "feature", [
                pan("ca.root", "根指纹", "kv-grid", "GET", "/api/ca/root"),
                pan("ca.export", "安全导出", "text-view", "GET", "/api/ca/export"),
            ]),
            mod(pid("certs"), "凭证管理", "cert", "indigo", "feature", [
                pan("ca.certs", "已签发凭证", "table", "GET", "/api/ca/certificates",
                    columns=[col("subject", "主体"), col("serial", "序号"), col("status", "状态")], actions=[
                        act("issue", "签发", "POST", "/api/ca/issue", True, [field("subject", "主体")]),
                        act("renew", "续期", "POST", "/api/ca/renew", True, [field("serial", "序号")]),
                        act("revoke", "吊销", "POST", "/api/ca/revoke", True, [field("serial", "序号")]),
                        act("verify", "验签", "POST", "/api/ca/verify", True, [field("token", "凭证", "textarea")]),
                    ]),
                pan("ca.crl", "吊销列表", "table", "GET", "/api/ca/crl"),
            ]),
            mod(pid("audit"), "签发审计", "history", "teal", "feature", [
                pan("ca.audit", "审计链", "log-list", "GET", "/api/ca/audit"),
            ]),
        ]
    if name == "lan-scan":
        return [
            mod(pid("status"), "扫描状态", "scan", "teal", "feature", [
                pan("scan.status", "速率与并发上界", "kv-grid", "GET", "/api/scan/status",
                    actions=[act("start", "启动扫描", "POST", "/api/scan/start", True)]),
            ]),
            mod(pid("results"), "探查结果", "scan", "blue", "feature", [
                pan("scan.results", "主机与端口", "table", "GET", "/api/scan/results",
                    columns=[col("host", "主机"), col("ports", "开放端口")]),
                pan("scan.snapshot", "结果快照", "text-view", "GET", "/api/scan/snapshot"),
            ]),
        ]
    if name == "lan-hosts":
        return [
            mod(pid("mappings"), "静态映射", "hosts", "indigo", "feature", [
                pan("hosts.mappings", "映射清单", "table", "GET", "/api/hosts/mappings",
                    columns=[col("name", "主机名"), col("ip", "地址"), col("comment", "备注")], actions=[
                        act("upsert", "新增/更新", "POST", "/api/hosts/mapping", True,
                            [field("name", "主机名"), field("ip", "地址")]),
                        act("remove", "删除", "POST", "/api/hosts/remove", True, [field("name", "主机名")]),
                        act("import", "导入", "POST", "/api/hosts/import", True,
                            [field("text", "映射文本", "textarea")]),
                    ]),
                pan("hosts.status", "发布状态", "kv-grid", "GET", "/api/hosts/status"),
            ]),
            mod(pid("render"), "渲染与发布", "terminal", "teal", "feature", [
                pan("hosts.render", "hosts 预览", "text-view", "GET", "/api/hosts/render",
                    actions=[act("publish", "原子发布", "POST", "/api/hosts/publish", True)]),
            ]),
        ]
    if name == "lan-wol":
        return [
            mod(pid("devices"), "唤醒设备", "power", "amber", "feature", [
                pan("wol.devices", "已登记设备", "table", "GET", "/api/wol/devices",
                    columns=[col("name", "名称"), col("mac", "MAC"), col("state", "状态")], actions=[
                        act("upsert", "登记设备", "POST", "/api/wol/device", True,
                            [field("device_id", "设备 ID"), field("mac", "MAC")]),
                        act("remove", "移除", "POST", "/api/wol/remove", True, [field("device_id", "设备 ID")]),
                        act("wake", "唤醒", "POST", "/api/wol/wake", True, [field("device_id", "设备 ID")]),
                    ]),
                pan("wol.status", "组件状态", "kv-grid", "GET", "/api/wol/status"),
            ]),
            mod(pid("probe"), "电源状态探测", "activity", "green", "feature", [
                pan("wol.probe", "只读 TCP 探测", "kv-grid", "GET", "/api/wol/status", actions=[
                    act("probe", "重新探测", "POST", "/api/wol/probe", True),
                ]),
                pan("wol.history", "唤醒审计", "log-list", "GET", "/api/wol/history"),
            ], requires_lib="common-probe"),
        ]
    if name == "lan-sync":
        return [
            mod(pid("nodes"), "同步节点", "sync", "green", "feature", [
                pan("sync.nodes", "已登记节点", "table", "GET", "/api/sync/nodes",
                    columns=[col("name", "节点"), col("url", "地址"), col("status", "状态")], actions=[
                        act("upsert", "登记节点", "POST", "/api/sync/node", True,
                            [field("name", "节点"), field("url", "地址")]),
                        act("remove", "移除", "POST", "/api/sync/remove", True, [field("name", "节点")]),
                    ]),
                pan("sync.status", "同步状态", "kv-grid", "GET", "/api/sync/status"),
            ]),
            mod(pid("transfer"), "配置传输", "sync", "blue", "feature", [
                pan("sync.history", "决策历史", "log-list", "GET", "/api/sync/history", actions=[
                    act("pull", "拉取", "POST", "/api/sync/pull", True, [field("name", "节点")]),
                    act("push", "推送", "POST", "/api/sync/push", True, [field("name", "节点")]),
                    act("confirm", "确认冲突", "POST", "/api/sync/confirm", True, [field("id", "决策 ID")]),
                ]),
            ]),
        ]
    if name == "lan-ssdp":
        return [
            mod(pid("devices"), "发现设备", "discover", "amber", "feature", [
                pan("ssdp.devices", "统一服务记录", "table", "GET", "/api/ssdp/devices",
                    columns=[col("name", "名称"), col("address", "地址"), col("ttl", "TTL")], actions=[
                        act("refresh", "刷新发现", "POST", "/api/ssdp/refresh", True),
                    ]),
                pan("ssdp.status", "监听状态", "kv-grid", "GET", "/api/ssdp/status"),
            ]),
            mod(pid("snapshot"), "发现快照", "cube", "teal", "feature", [
                pan("ssdp.snapshot", "结果快照", "text-view", "GET", "/api/ssdp/snapshot"),
            ]),
        ]
    raise KeyError(name)


def build(name):
    runtime = "standalone" if name == "lan-mdns" else "common-component"
    libs = [] if name == "lan-mdns" else ["common-component"]
    if name == "lan-wol":
        libs.append("common-probe")
    return {
        "schema": 1,
        "component": name,
        "runtime": runtime,
        "libs": libs,
        "modules": core_modules() + proprietary_modules(name),
    }


def normalize_writes(doc, name):
    """The write flag belongs to the backend: derive it from route_write()."""
    src = source_routes(name)
    for m in doc["modules"]:
        if m["id"].startswith("core."):
            continue
        for panel in m["panels"]:
            for a in panel.get("actions", []):
                if a["method"] != "POST":
                    continue
                a.pop("write", None)
                if src.get(a["path"], False):
                    a["write"] = True
    return doc


def source_routes(name):
    """(path, is_write) pairs actually registered by the component binary."""
    text = (COMPONENTS / name / "src" / "main.rs").read_text(encoding="utf-8")
    found = {}
    for is_write, method, path in ROUTE_RE.findall(text):
        if path in CORE_PATHS:
            continue  # supplied by standard_routes(), not a proprietary module
        found[path] = bool(is_write)
    return found


def declared_paths(doc):
    """All paths in non-core modules, plus their write flags."""
    paths = {}
    for m in doc["modules"]:
        if m["id"].startswith("core."):
            continue
        for panel in m["panels"]:
            ep = panel["endpoint"]
            paths.setdefault(ep["path"], False)
            for a in panel.get("actions", []):
                paths[a["path"]] = paths.get(a["path"], False) or bool(a.get("write"))
    return paths


def validate_schema_shape(doc):
    """Keep the samples honest without third-party jsonschema."""
    errs = []
    for m in doc["modules"]:
        if m["tone"] not in TONES:
            errs.append(f"{m['id']}: tone {m['tone']!r} not in palette")
        if m["area"] not in AREAS:
            errs.append(f"{m['id']}: area {m['area']!r} invalid")
        if m["icon"] not in ICONS:
            errs.append(f"{m['id']}: icon {m['icon']!r} not in inline SVG registry")
        if m.get("requires_lib") and m["requires_lib"] not in doc["libs"]:
            errs.append(f"{m['id']}: requires_lib {m['requires_lib']!r} not declared in libs")
        for panel in m["panels"]:
            if panel["widget"] not in WIDGETS:
                errs.append(f"{panel['id']}: widget {panel['widget']!r} not in renderer registry")
            for a in panel.get("actions", []):
                if a.get("write") and not a["path"].startswith("/api/"):
                    errs.append(f"{a['id']}: write action path invalid")
    return errs


def verify(doc, name, openapi_text):
    errs = validate_schema_shape(doc)
    src = source_routes(name)
    declared = declared_paths(doc)
    missing = sorted(set(src) - set(declared))
    extra = sorted(set(declared) - set(src))
    if missing:
        errs.append(f"{name}: backend registers endpoints with no UI: {missing}")
    if extra:
        errs.append(f"{name}: UI declares endpoints the backend does not have: {extra}")
    for path, is_write in src.items():
        if is_write and not declared.get(path):
            errs.append(f"{name}: {path} is a protected write but has no write action")
        if not is_write and declared.get(path):
            errs.append(f"{name}: {path} is read-only but is marked as a write action")
    for path in declared:
        if path not in openapi_text:
            errs.append(f"{name}: {path} missing from contracts/openapi.yaml")
    return errs


def check_manifest(docs):
    if not MANIFEST.exists():
        return ["shell/components.yaml missing"]
    text = MANIFEST.read_text(encoding="utf-8")
    errs = []
    for name, doc in docs.items():
        expected = [m["id"] for m in doc["modules"]]
        # modules are declared as a YAML list under each component entry
        block = re.search(
            r"- name:\s*" + re.escape(name) + r"\b(.*?)(?=\n  - name:|\Z)",
            text, re.S,
        )
        if not block:
            errs.append(f"manifest: component {name} missing")
            continue
        got = re.findall(r"^\s+- (lan-[a-z0-9-]+\.[a-z0-9-]+|core\.[a-z0-9-]+)\s*$",
                         block.group(1), re.M)
        if sorted(got) != sorted(expected):
            errs.append(f"manifest: {name} module ids drift (expected {len(expected)}, got {len(got)})")
    return errs


def main() -> int:
    check_only = "--check" in sys.argv
    if not SCHEMA.exists():
        print("FAIL contracts/capabilities.schema.json missing")
        return 1
    json.loads(SCHEMA.read_text(encoding="utf-8"))
    openapi_text = OPENAPI.read_text(encoding="utf-8")
    docs = {name: normalize_writes(build(name), name) for name in COMPONENT_NAMES}
    errors = []
    for name, doc in docs.items():
        errors.extend(verify(doc, name, openapi_text))
    errors.extend(check_manifest(docs))

    # Per-component allowlist for the Shell same-origin proxy: one `name|path`
    # per line, so the Rust side needs no JSON parser at all.
    lines = []
    for name, doc in docs.items():
        paths = set()
        for m in doc["modules"]:
            for panel in m["panels"]:
                paths.add(panel["endpoint"]["path"])
                for a in panel.get("actions", []):
                    paths.add(a["path"])
        lines.extend(f"{name}|{path}" for path in sorted(paths))
    allow_text = "\n".join(lines) + "\n"
    current_allow = ALLOWLIST.read_text(encoding="utf-8") if ALLOWLIST.exists() else None
    if current_allow != allow_text:
        if check_only:
            errors.append("capabilities.allowlist.txt is out of date (run gen_capabilities.py)")
        else:
            ALLOWLIST.write_text(allow_text, encoding="utf-8")
            print("wrote contracts/capabilities.allowlist.txt")

    if errors:
        for e in errors:
            print("FAIL " + e)
        return 1

    changed = []
    for name, doc in docs.items():
        target = EXAMPLES / f"capabilities.{name}.sample.json"
        rendered = json.dumps(doc, ensure_ascii=False, indent=2) + "\n"
        current = target.read_text(encoding="utf-8") if target.exists() else None
        if current != rendered:
            changed.append(target)
            if not check_only:
                target.write_text(rendered, encoding="utf-8")
    if check_only:
        if changed:
            for path in changed:
                print(f"FAIL drift: {path.relative_to(ROOT)}")
            return 1
        print(f"CAPABILITIES OK: {len(docs)} manifests match backend routes")
        return 0
    for path in changed:
        print("wrote " + str(path.relative_to(ROOT)))
    print(f"CAPABILITIES GENERATED: {len(docs)} manifests ({len(changed)} changed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
