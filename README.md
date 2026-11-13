# Lan Studio

> 内网轻量 ZeroConf 与局域网运维工具箱。
> 8 个 `lan-*` 组件，统一契约、独立二进制、独立 WebUI、可单独部署。

**当前状态：M2.2 可观测 + 安全 + 配置生命周期完成**（2026-11-13）。`lan-mdns` 在真实
mDNS/DNS-SD 引擎之上补齐了管理员密码写保护、配置保存 / 快照 / 历史 / 回滚 / 导入导出 /
热重载、有界结构化日志与磁盘轮转、SSE 实时日志流、TCP 健康目标、名称冲突聚合、
服务地图 TTL 老化，以及 [`docs/METRICS.md`](docs/METRICS.md) 第 3 节全部 13 个专有指标族。
单测 62 项、运行态冒烟 **160 项**全绿。后续批次见
[`docs/M2-CHECKLIST.md`](docs/M2-CHECKLIST.md)。

- 完整规格：[`docs/PRD.md`](docs/PRD.md)
- 指标字典：[`docs/METRICS.md`](docs/METRICS.md)
- 告警登记：[`docs/ALERT-SCHEMA.md`](docs/ALERT-SCHEMA.md)
- 错误码表：[`docs/ERROR-CODES.md`](docs/ERROR-CODES.md)

## 目录

```
lan-studio/
├── Cargo.toml             # workspace，11 个 member
├── docs/                  # 规格与字典
│   ├── PRD.md             # 唯一权威规格书
│   ├── M2-CHECKLIST.md    # 66 条功能逐条销项基线
│   ├── METRICS.md
│   ├── ALERT-SCHEMA.md
│   ├── ERROR-CODES.md
│   └── ADR/               # 架构决策记录（0001 M1 / 0002 M2.1 / 0003 M2.2）
├── contracts/             # 机器可读契约（语言无关）
│   ├── openapi.yaml
│   ├── config.schema.json
│   ├── alert-event.json
│   ├── log-record.json
│   ├── snapshot.json
│   └── examples/          # 通过 schema 的示例
├── scripts/
│   ├── verify_contracts.py # M0 契约校验
│   └── smoke_mdns.py       # M2.1 + M2.2 运行态冒烟（160 项断言）
├── common/                # 公共 crate
│   ├── config/  crypto/  dns/  net/  log/  health/
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

**写操作受管理员密码保护**（F-F02~F-F04）。首次未设置密码时所有写路由返回
`409 setup_required`；先设置密码，再在请求头带 `X-Admin-Password`：

```bash
curl --noproxy '*' -X POST -H 'Content-Type: application/json' \
  -d '{"new_password":"change-me-now"}' http://127.0.0.1:8080/api/auth/password
curl --noproxy '*' -X POST -H 'X-Admin-Password: change-me-now' \
  http://127.0.0.1:8080/api/reload
```

运行时数据目录可全部重定向（冒烟脚本正是靠这一点隔离实例，不污染仓库）：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_STUDIO_AUTH_FILE` | `auth.toml` | 管理员密码摘要，文件权限 `0600` |
| `LAN_STUDIO_LOG_DIR` | `logs` | 轮转日志目录，磁盘硬上界 = `max_bytes × (backup_count + 1)` |
| `LAN_STUDIO_HISTORY_DIR` | `config_history` | 配置快照目录，按 `observability.history_limit` 有界保留 |
| `LAN_STUDIO_PORT` | `server.port`（8080） | 测试 / CI 端口覆盖钩子，生产以 YAML 为准 |

运行态冒烟（自动使用隔离组播组、高位端口与独立临时数据目录，不向真实 LAN 发包）：

```bash
python3 scripts/smoke_mdns.py
```

设计取舍见
[`docs/ADR/0001-m1-skeleton-decisions.md`](docs/ADR/0001-m1-skeleton-decisions.md)、
[`docs/ADR/0002-m2-1-protocol-kernel.md`](docs/ADR/0002-m2-1-protocol-kernel.md) 与
[`docs/ADR/0003-m2-2-observability-security.md`](docs/ADR/0003-m2-2-observability-security.md)。

## 里程碑

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0 | 契约冻结（本目录全部内容） | **完成** |
| M1 | monorepo 骨架、`common-*` 最小集、CI、单文件产物 | **完成** |
| M2 | `lan-mdns` 功能全集 66 条逐条销项 | **进行中（M2.1 + M2.2 完成，43/66）** |
| M3 | 其余 P0：`lan-relay` / `lan-ca` / `lan-scan` | 待启动 |
| M4 | P1：`lan-hosts` / `lan-wol` | 待启动 |
| M5 | P2：`lan-sync` / `lan-ssdp` + 统一出口 + 生态 DoD | 待启动 |

## 说明

- 契约以 `contracts/` 下机器可读文件为准，文字描述冲突时以契约文件为准。
- 仓库：`https://github.com/scpuny/lan-studio.git`，`origin/main`。
- 既有 mDNS 服务已进入维护阶段（仅修缺陷、不接新需求），其功能由 `lan-mdns` 承接。
- 告警、内置 HTTP 网关、容器 / systemd / 安装脚本在 M2.3 销项；Arco Design 正式管理台、
  健康状态灯、在线配置表单与全量 DoD 在 M2.4 销项。
- `LAN_MDNS_GROUP` / `LAN_MDNS_PORT` 仅用于测试隔离；生产组播地址与端口永远取自 YAML。
