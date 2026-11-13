# Lan Studio

> 内网轻量 ZeroConf 与局域网运维工具箱。
> 8 个 `lan-*` 组件，统一契约、独立二进制、独立 WebUI、可单独部署。

**当前状态：M2.1 协议内核完成**（2026-11-13）。`lan-mdns` 已是真实 mDNS/DNS-SD 响应器：
组播收发、查询应答、宣告 / Goodbye、QU 单播、Known-Answer 抑制、外来应答摄取、主动探针、
有界查询审计，62 项运行态断言全绿。后续批次见 [`docs/M2-CHECKLIST.md`](docs/M2-CHECKLIST.md)。

- 完整规格：[`docs/PRD.md`](docs/PRD.md)
- 指标字典：[`docs/METRICS.md`](docs/METRICS.md)
- 告警登记：[`docs/ALERT-SCHEMA.md`](docs/ALERT-SCHEMA.md)
- 错误码表：[`docs/ERROR-CODES.md`](docs/ERROR-CODES.md)

## 目录

```
lan-studio/
├── Cargo.toml             # workspace，10 个 member
├── docs/                  # 规格与字典
│   ├── PRD.md             # 唯一权威规格书
│   ├── M2-CHECKLIST.md    # 66 条功能逐条销项基线
│   ├── METRICS.md
│   ├── ALERT-SCHEMA.md
│   ├── ERROR-CODES.md
│   └── ADR/               # 架构决策记录（0001 M1 / 0002 M2.1）
├── contracts/             # 机器可读契约（语言无关）
│   ├── openapi.yaml
│   ├── config.schema.json
│   ├── alert-event.json
│   ├── log-record.json
│   ├── snapshot.json
│   └── examples/          # 通过 schema 的示例
├── scripts/
│   ├── verify_contracts.py # M0 契约校验
│   └── smoke_mdns.py       # M2.1 运行态冒烟（62 项断言）
├── common/                # 公共 crate
│   ├── config/  dns/  net/  log/  health/
│   └── auth/  metrics/  webui/  api/
├── components/
│   └── lan-mdns/          # 真实引擎 + 内嵌 WebUI + 配置模板
├── shell/                 # 统一出口（M5）
└── .github/workflows/
    ├── ci.yml             # 契约 + fmt/clippy/test + 产物冒烟
    └── release.yml        # musl 静态单文件 + sha256
```

## 契约校验

校验器零强制第三方依赖，任何 Python 3 都能跑：

```bash
python3 scripts/verify_contracts.py
```

| 模式 | 检查数 | 触发条件 | 覆盖范围 |
| --- | --- | --- | --- |
| 完整 | 21 | 已安装 PyYAML | JSON Schema 自解析、示例数据、OpenAPI 必备端点、错误码 / 指标 / 告警 kind 与字典一致 |
| 降级 | 12 + `WARN` | 未安装 PyYAML | 仅 JSON 类检查，跳过 8 项 YAML 相关检查 |

CI 已自动安装 PyYAML，始终跑完整模式；本地离线自检可用降级模式：

```bash
python3 -m pip install pyyaml   # 升级为完整模式
```

降级时输出末尾带 `[DEGRADED: YAML checks skipped]`，退出码仍为 0，表示「已执行到的检查」全部通过——是否完整以该标记为准。

## 构建与运行

零第三方 crate，`cargo build` 不需要访问 crates.io：

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p lan-mdns

LAN_STUDIO_PORT=8080 \
LAN_STUDIO_CONFIG=components/lan-mdns/config.template.yaml \
  ./target/release/lan-mdns

curl --noproxy '*' http://127.0.0.1:8080/healthz
curl --noproxy '*' http://127.0.0.1:8080/api/records
curl --noproxy '*' -X POST -H 'Content-Type: application/json' \
  -d '{"name":"nas.local","type":"A"}' http://127.0.0.1:8080/api/probe
```

`--cli` 模式不监听端口，离线校验配置并输出构建信息。

运行态冒烟（自动使用隔离组播组与高位端口，不向真实 LAN 发包）：

```bash
python3 scripts/smoke_mdns.py
```

设计取舍见 [`docs/ADR/0001-m1-skeleton-decisions.md`](docs/ADR/0001-m1-skeleton-decisions.md)
与 [`docs/ADR/0002-m2-1-protocol-kernel.md`](docs/ADR/0002-m2-1-protocol-kernel.md)。

## 里程碑

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0 | 契约冻结（本目录全部内容） | **完成** |
| M1 | monorepo 骨架、`common-*` 最小集、CI、单文件产物 | **完成** |
| M2 | `lan-mdns` 功能全集 66 条逐条销项 | **进行中（M2.1 完成）** |
| M3 | 其余 P0：`lan-relay` / `lan-ca` / `lan-scan` | 待启动 |
| M4 | P1：`lan-hosts` / `lan-wol` | 待启动 |
| M5 | P2：`lan-sync` / `lan-ssdp` + 统一出口 + 生态 DoD | 待启动 |

## 说明

- 契约以 `contracts/` 下机器可读文件为准，文字描述冲突时以契约文件为准。
- 仓库：`https://github.com/scpuny/lan-studio.git`，`origin/main`。
- 既有 mDNS 服务已进入维护阶段（仅修缺陷、不接新需求），其功能由 `lan-mdns` 承接。
- M2.1 只交付协议内核与配置内核；可观测、配置生命周期、安全、告警、网关、正式 WebUI
  分别在 M2.2 / M2.3 / M2.4 销项。
