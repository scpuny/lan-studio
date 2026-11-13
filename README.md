# Lan Studio

> 内网轻量 ZeroConf 与局域网运维工具箱。
> 8 个 `lan-*` 组件，统一契约、独立二进制、独立 WebUI、可单独部署。

**当前状态：M1 monorepo 骨架完成**（2026-11-13）。`lan-mdns` 为可启动空壳，mDNS 协议尚未实现（按 PRD 7.5 属 M2）。

- 完整规格：[`docs/PRD.md`](docs/PRD.md)
- 指标字典：[`docs/METRICS.md`](docs/METRICS.md)
- 告警登记：[`docs/ALERT-SCHEMA.md`](docs/ALERT-SCHEMA.md)
- 错误码表：[`docs/ERROR-CODES.md`](docs/ERROR-CODES.md)

## 目录

```
lan-studio/
├── Cargo.toml             # workspace，8 个 member
├── docs/                  # 规格与字典
│   ├── PRD.md             # 唯一权威规格书
│   ├── METRICS.md
│   ├── ALERT-SCHEMA.md
│   ├── ERROR-CODES.md
│   └── ADR/               # 架构决策记录（0001 M1 取舍）
├── contracts/             # 机器可读契约（语言无关）
│   ├── openapi.yaml
│   ├── config.schema.json
│   ├── alert-event.json
│   ├── log-record.json
│   ├── snapshot.json
│   └── examples/          # 通过 schema 的示例
├── scripts/
│   └── verify_contracts.py
├── common/                # 公共 crate（config/log/health/auth/metrics/webui/api）
├── components/
│   └── lan-mdns/          # M1 空壳二进制 + 内嵌 WebUI + 配置模板
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

## 构建与运行（M1）

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
curl --noproxy '*' http://127.0.0.1:8080/api/metrics
```

`--cli` 模式不监听端口，仅输出构建信息。设计取舍见 [`docs/ADR/0001-m1-skeleton-decisions.md`](docs/ADR/0001-m1-skeleton-decisions.md)。

## 里程碑

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0 | 契约冻结（本目录全部内容） | **完成** |
| M1 | monorepo 骨架、`common-*` 最小集、CI、单文件产物 | **完成** |
| M2 | `lan-mdns` 功能全集 66 条逐条销项 | 待启动 |
| M3 | 其余 P0：`lan-relay` / `lan-ca` / `lan-scan` | 待启动 |
| M4 | P1：`lan-hosts` / `lan-wol` | 待启动 |
| M5 | P2：`lan-sync` / `lan-ssdp` + 统一出口 + 生态 DoD | 待启动 |

## 说明

- 契约以 `contracts/` 下机器可读文件为准，文字描述冲突时以契约文件为准。
- 未设置 git remote、未推送；仓库地址与可见性见 PRD 第 9 章 D2。
- M1 只实现骨架与统一 API 面；mDNS 协议、可观测、配置快照、告警等 66 条功能属于 M2。
- 既有 mDNS 服务已进入维护阶段（仅修缺陷、不接新需求），其功能由 `lan-mdns` 承接。
