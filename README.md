# Lan Studio

> 内网轻量 ZeroConf 与局域网运维工具箱。
> 8 个 `lan-*` 组件，统一契约、独立二进制、独立 WebUI、可单独部署。

**当前状态：M0 契约冻结完成**（2026-11-13）。尚未编写任何组件实现代码。

- 完整规格：[`docs/PRD.md`](docs/PRD.md)
- 指标字典：[`docs/METRICS.md`](docs/METRICS.md)
- 告警登记：[`docs/ALERT-SCHEMA.md`](docs/ALERT-SCHEMA.md)
- 错误码表：[`docs/ERROR-CODES.md`](docs/ERROR-CODES.md)

## 目录

```
lan-studio/
├── docs/                  # 规格与字典
│   ├── PRD.md             # 唯一权威规格书
│   ├── METRICS.md
│   ├── ALERT-SCHEMA.md
│   ├── ERROR-CODES.md
│   └── ADR/               # 架构决策记录
├── contracts/             # 机器可读契约（语言无关）
│   ├── openapi.yaml
│   ├── config.schema.json
│   ├── alert-event.json
│   ├── log-record.json
│   ├── snapshot.json
│   └── examples/          # 通过 schema 的示例
├── scripts/
│   └── verify_contracts.py
├── common/                # 公共 crate（M1）
├── components/            # 8 个组件（M1 起）
├── shell/                 # 统一出口（M5）
└── .github/workflows/     # CI / release（M1）
```

## 契约校验

零第三方依赖，`yaml` 可选（用于解析 `.yaml` 契约）：

```bash
python3 scripts/verify_contracts.py
```

校验内容：JSON Schema 自解析、示例数据通过 schema、OpenAPI 必备端点完整、错误码 / 指标 / 告警 kind 与字典一致。退出码 0 表示 M0 契约可交付。

## 里程碑

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0 | 契约冻结（本目录全部内容） | **完成** |
| M1 | monorepo 骨架、`common-*` 最小集、CI、单文件产物 | 待启动 |
| M2 | `lan-mdns` 功能全集 66 条逐条销项 | 待启动 |
| M3 | 其余 P0：`lan-relay` / `lan-ca` / `lan-scan` | 待启动 |
| M4 | P1：`lan-hosts` / `lan-wol` | 待启动 |
| M5 | P2：`lan-sync` / `lan-ssdp` + 统一出口 + 生态 DoD | 待启动 |

## 说明

- 契约以 `contracts/` 下机器可读文件为准，文字描述冲突时以契约文件为准。
- 未设置 git remote、未推送；仓库地址与可见性见 PRD 第 9 章 D2。
- 未创建 `Cargo.toml`、未写 Rust 代码——按 PRD，M1 才进入实现阶段。
- 既有 mDNS 服务已进入维护阶段（仅修缺陷、不接新需求），其功能由 `lan-mdns` 承接。
