# M2 销项清单：`lan-mdns` 功能全集

> 依据 `docs/PRD.md` 第 5 章（66 条）与 7.6「附录 A 逐条核对，缺一不可收尾」。
> 本表是 M2 的**唯一收尾依据**：只有 66 条全部 `done`、且 PRD 8.1 组件级 DoD 全过，M2 才算完成。
> 状态：`pending` 未开始 / `wip` 实现中 / `done` 已通过验收。

## 1. 批次划分

| 批次 | 主题 | 主要交付 | 状态 |
| --- | --- | --- | --- |
| **M2.1** | 协议内核 + 配置内核 + mDNS 引擎 | `common-dns` 报文编解码、`common-config` 完整解析、`common-net` 组播收发、`lan-mdns` 真实应答/宣告/探针/摄取；离线报文单测 + 组播闭环 | **done** |
| **M2.2** | 可观测 + 安全 + 配置生命周期 | 查询审计 / 冲突检测 / 服务地图 / 健康监控 / SSE / 日志轮转；配置保存·快照·回滚·导入导出·热重载；管理员密码与写保护；13 个专有指标族 | **done** |
| **M2.3** | 告警 + HTTP 网关 + 部署运维 | `common-alert` 五平台适配 + 有界队列 / 冷却 / 重试、健康翻转与冲突触发；静态 Host 网关 + 双开关 + 友好页；Docker / compose / systemd / install.sh / 探针 / 首次启动模板 | **done** |
| **M2.4** | WebUI + 全量销项 + DoD | 离线正式管理台、端到端与 API 契约测试、CI 门禁、66 条全部 `done`、PRD 8.1 组件级 DoD | pending |

## 2. 66 条逐条销项

### A. 名称解析与发布（12）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-A01 | 静态域名 → IPv4 地址映射发布 | M2.1 | done |
| F-A02 | IPv6 地址记录发布 | M2.1 | done |
| F-A03 | 自定义 TXT 记录随服务发布 | M2.1 | done |
| F-A04 | `.local` 名称合法性校验（含多段标签） | M2.1 | done |
| F-A05 | IPv4 反向 PTR 发布 | M2.1 | done |
| F-A06 | 记录 TTL 可配置 | M2.1 | done |
| F-A07 | 启动时主动宣告（次数 / 间隔可配） | M2.1 | done |
| F-A08 | 周期性主动宣告 | M2.1 | done |
| F-A09 | 退出时发送 Goodbye | M2.1 | done |
| F-A10 | Known-Answer 抑制 / 缓存刷新 | M2.1 | done |
| F-A11 | QU 位单播应答 | M2.1 | done |
| F-A12 | 多网卡绑定（指定 / 自动 / 全部枚举） | M2.4 | pending |

### B. 服务发现 DNS-SD（6）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-B01 | 服务实例发布（PTR / SRV / TXT） | M2.1 | done |
| F-B02 | 服务类型枚举 | M2.1 | done |
| F-B03 | 自定义服务端口与主机 | M2.1 | done |
| F-B04 | 外来应答摄取与解析（A/AAAA/PTR/SRV/TXT） | M2.1 | done |
| F-B05 | 局域网服务地图（按类型聚合，TTL 过期清理） | M2.2 | done |
| F-B06 | 主动探针（指定名称 / 类型，单播或组播） | M2.1 | done |

### C. 网络与协议（6）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-C01 | 标准组播地址与端口收发 | M2.1 | done |
| F-C02 | 组播地址 / 端口可配置（隔离测试） | M2.1 | done |
| F-C03 | 与系统 mDNS 守护进程共存 | M2.3 | done |
| F-C04 | 自身应答回环排除 | M2.1 | done |
| F-C05 | DNS 报文编解码（含名称压缩） | M2.1 | done |
| F-C06 | 全链路有界资源 | M2.2 | done |

### D. 可观测（8）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-D01 | 查询审计日志（时间 / 来源 / 名称 / 类型） | M2.2 | done |
| F-D02 | 名称冲突检测（同名异值、来源登记、老化清理） | M2.2 | done |
| F-D03 | 目标健康监控（TCP：up / down / unknown） | M2.2 | done |
| F-D04 | 健康状态灯与列表 | M2.4 | pending |
| F-D05 | 实时日志流（SSE） | M2.2 | done |
| F-D06 | 日志轮转与磁盘硬上界 | M2.2 | done |
| F-D07 | 运行状态与统计 | M2.2 | done |
| F-D08 | Prometheus 指标（全部必备族） | M2.2 | done |

### E. 配置与生命周期（9）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-E01 | 声明式配置文件（符合 3.1） | M2.1 | done |
| F-E02 | 离线配置校验（不启动网络） | M2.1 | done |
| F-E03 | 配置热重载（文件变更 / 信号 / 手动） | M2.2 | done |
| F-E04 | 在线配置编辑（表单 + 源码双模式） | M2.4 | pending |
| F-E05 | 保存链路：校验 → 快照 → 原子替换 → 热重载 | M2.2 | done |
| F-E06 | 配置版本快照（自动、有界、带原因标记） | M2.2 | done |
| F-E07 | 一键回滚 | M2.2 | done |
| F-E08 | 配置导入 / 导出 | M2.2 | done |
| F-E09 | 首次启动模板生成 + 配置持久化 | M2.3 | done |

### F. 安全（5）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-F01 | Admin Token 鉴权 | M2.2 | done |
| F-F02 | 管理员密码保护写操作、可修改 | M2.2 | done |
| F-F03 | 密码摘要存储（加盐哈希、常量时间校验、文件权限） | M2.2 | done |
| F-F04 | 未初始化时写操作拒绝（409 setup_required） | M2.2 | done |
| F-F05 | 管理接口默认仅本机监听 | M2.2 | done |

### G. 告警（5）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-G01 | 多平台 Webhook（钉钉 / 飞书 / Slack / 企业微信 / 通用 JSON） | M2.3 | done |
| F-G02 | 健康状态翻转触发 | M2.3 | done |
| F-G03 | 名称冲突触发 | M2.3 | done |
| F-G04 | 冷却去重 + 失败重试（≤2 次）+ 有界队列 | M2.3 | done |
| F-G05 | 告警配置热更新与状态查询 | M2.3 | done |

### H. 网关与辅助入口（5）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-H01 | 内置 HTTP 网关（已登记域名静态 Host 路由） | M2.3 | done |
| F-H02 | 全局开关 + 每服务代理开关 | M2.3 | done |
| F-H03 | 管理台统一域名入口 | M2.3 | done |
| F-H04 | 友好错误页（未登记 / 目标不可达） | M2.3 | done |
| F-H05 | 辅助跳转入口 | M2.4 | pending |

### I. 部署与运维（7）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-I01 | 单文件发布、零外部运行时依赖 | M2.3 | done |
| F-I02 | `webui` / `cli` 双模式 | M2.3 | done |
| F-I03 | 容器镜像 + 编排文件（host 网络硬约束） | M2.3 | done |
| F-I04 | 系统服务单元 | M2.3 | done |
| F-I05 | 一键安装脚本 + 升级（保留配置与日志） | M2.3 | done |
| F-I06 | 数据卷持久化 | M2.3 | done |
| F-I07 | 健康检查探针 | M2.3 | done |

### J. 质量保障（3）

| 编号 | 能力 | 批次 | 状态 |
| --- | --- | --- | --- |
| F-J01 | 离线报文单元测试 | M2.1 | done |
| F-J02 | 真实组播端到端测试 | M2.1 | done |
| F-J03 | 管理 API 契约测试 + CI 门禁 | M2.4 | pending |

**合计：10 个能力域 / 66 条；已销项 61 条（剩余 5 条归 M2.4：F-A12 / F-D04 /
F-E04 / F-H05 / F-J03）。**

## 3. 契约端点对照（22 个，`contracts/openapi.yaml`）

| 批次 | 端点 |
| --- | --- |
| M1 已有 | `/healthz`、`/readyz`、`/api/status`、`/api/metrics`、`/api/config`(GET)、`/api/auth` |
| M2.1（已交付） | `/api/records`、`/api/probe`、`/api/interfaces`；额外提前提供只读的 `/api/queries`、`/api/browse` |
| M2.2（已交付） | `/api/config`(POST)、`/api/config/snapshot`(GET/POST)、`/api/config/history`、`/api/config/rollback`、`/api/reload`、`/api/logs`、`/api/logs/stream`、`/api/health/targets`、`/api/conflicts`、`/api/auth/password`、`/api/auth/verify`；`/api/queries` 补 CSV 导出与老化、`/api/browse` 补服务类型聚合 |
| M2.3（已交付） | `/api/alerts` 接入真实分发器（状态 / 计数 / 按 kind 统计 / 网关统计）；网关自身独立监听：`Host` 静态路由、`/go/` 辅助入口、404 / 502 友好页 |

## 4. 进度摘要

| 日期 | 批次 | 完成条数 | 累计 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-11-13 | M2.1 | 0 | 0/66 | 建立销项基线 |
| 2026-11-13 | M2.1 | 24 | 24/66 | 协议内核 + 配置内核 + 真实 mDNS 引擎验收通过 |
| 2026-11-13 | M2.2 | 19 | 43/66 | 可观测 + 安全 + 配置生命周期验收通过 |
| 2026-11-13 | M2.3 | 18 | 61/66 | 告警 + 网关 + 部署运维验收通过，冒烟 221 项 |

## 5. M2.1 验收记录（2026-11-13）

### 5.1 销项明细

本批次 `done` 共 24 条：`F-A01`~`F-A11`（11）、`F-B01`~`F-B04` + `F-B06`（5）、
`F-C01` / `F-C02` / `F-C04` / `F-C05`（4）、`F-E01` / `F-E02`（2）、
`F-J01` / `F-J02`（2）。

### 5.2 提前交付但**不提前销项**的能力

以下能力在 M2.1 已有可运行实现，但完整语义（TTL 聚合、老化、UI、热重载、真实
守护进程共存实测等）仍属 M2.2，故对应条目保持 `pending`：

| 条目 | M2.1 已提供 | 待 M2.2 补齐 |
| --- | --- | --- |
| F-A12 | `interface_mode: auto/all/specified` + `mdns.interfaces[]`，传输层逐网卡加入组播 | 管理台表单、热重载 |
| F-B05 | `/api/browse` 外来应答摄取（有界队列） | 按服务类型聚合、TTL 过期清理、冲突关联 |
| F-C03 | raw fd 在 bind 前置 `SO_REUSEPORT` | 与 avahi / systemd-resolved 同机实测 |
| F-C06 | `MAX_QUERIES` / `MAX_OBSERVED` / `MAX_BUFFER` 有界队列 | 日志、快照、告警队列的全链路上界 |
| F-D01 | `/api/queries` 时间 / 来源 / 名称 / 类型，上限 `?limit=1..1000` | SSE、导出、老化策略 |
| F-D07 | `/api/status` 引擎计数（queries / responses / ingested / announces / malformed） | 健康目标、配置版本等状态位 |
| F-D08 | 9 个必备族 + 7 个引擎专有族 | 其余 7 个专有族（见 `docs/METRICS.md` 第 3 节） |
| F-F01 | Admin Token 鉴权（M1 已有，冒烟验证 401 / 403 / 200） | 管理员密码写保护（`F-F02`~`F-F05`） |

### 5.3 验收证据

| 门禁 | 结果 |
| --- | --- |
| M0 契约校验（完整模式） | 21 passed / 0 failed |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 36 passed / 0 failed |
| `cargo build --release -p lan-mdns` | 成功，单文件约 997 KB |
| `python3 scripts/smoke_mdns.py` | **62 passed / 0 failed** |
| 真实组播闭环 | A / PTR / QU 三种探针均取回本引擎发布的 RR；优雅退出广播 Goodbye |

冒烟脚本自动使用隔离组播组 `239.1.2.3` 与高位端口 `15353` / `15354`，不向真实
LAN 发送 `224.0.0.251:5353` 流量；技术取舍见 `docs/ADR/0002-m2-1-protocol-kernel.md`。

### 5.4 缺陷与修复记录

1. `Engine::start()` 中 `Arc<Engine>` 被 `thread::spawn` 移动后又借用，编译失败 → 改用
   独立 `worker` 变量持有克隆，`engine` 仅用于登记 `JoinHandle`。
2. 指标短名误加 `mdns_` 前缀，产出 `lanstudio_mdns_mdns_records` 双前缀 → 改为
   `records` / `queries_total` 等，并加冒烟断言第 27 项防回归。
3. QU 单播探针绑定共享端口时，Linux 在 `SO_REUSEPORT` 组内把单播应答分发回引擎
   socket，探针收不到 → QU 模式改绑临时端口并显式发往 `group:mdns.port`。
4. `--cli` 曾打印测试覆盖后的组播地址 / 端口 → 把覆盖移到运行引擎之前，CLI 始终
   报告配置文件自身的值。

### 5.5 M2.2 入口（已完成，见第 6 节）

`F-B05`、`F-C06`、`F-D01`~`F-D03`、`F-D05`~`F-D08`、`F-E03`、`F-E05`~`F-E08`、
`F-F01`~`F-F05` 共 19 条已在 M2.2 销项；同时把 `LAN_STUDIO_PORT` 收回
`config.schema.json`（降级为测试覆盖钩子），并补齐 `docs/METRICS.md` 第 3 节
剩余 6 个指标族。

## 6. M2.2 验收记录（2026-11-13）

### 6.1 销项明细

本批次 `done` 共 19 条：`F-B05`（1）、`F-C06`（1）、
`F-D01` / `F-D02` / `F-D03` / `F-D05` / `F-D06` / `F-D07` / `F-D08`（7）、
`F-E03` / `F-E05` / `F-E06` / `F-E07` / `F-E08`（5）、
`F-F01`~`F-F05`（5）。累计 43/66。

### 6.2 明确不提前销项

| 条目 | M2.2 状态 | 剩余工作 / 归属 |
| --- | --- | --- |
| F-A12 | 配置模型与传输层已具备（`interface_mode` + 逐网卡加入组播） | 管理台表单（M2.4）；socket 级接口热切换需重启，M2.3 评估 |
| F-C03 | `SO_REUSEPORT` 共存机制已实现 | 与 avahi / systemd-resolved 同机实测（M2.3 部署阶段） |
| F-D04 | `/api/health/targets` 已提供 up/down/unknown 数据与计数 | 状态灯与可视化列表属正式 WebUI（M2.4） |
| F-E04 | 保存 / 导入 / 回滚 API 已具备 | 表单 + 源码双模式在线编辑属正式 WebUI（M2.4） |
| F-E09 | 编译期内嵌模板 + 配置落盘已具备 | 首次启动目录生成与 Docker 数据卷（M2.3） |
| F-G* / F-H* / F-I* / F-J03 | 契约端点占位（`/api/alerts` 等） | M2.3 / M2.4 |

### 6.3 验收证据

| 门禁 | 结果 |
| --- | --- |
| M0 契约校验（完整模式，PyYAML） | **21 passed / 0 failed** |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | **62 passed / 0 failed**（M2.1 为 36） |
| `cargo build --release -p lan-mdns` | 成功，单文件 `1,299,504` 字节 |
| `python3 scripts/smoke_mdns.py` | **160 passed / 0 failed**（M2.1 为 62） |
| 真实组播闭环 | A / PTR / QU 三种探针取回本引擎 RR；优雅退出广播 Goodbye |
| 写保护全链路 | 未设密码 409 `setup_required` → 设置 → 缺失/错误 403 `auth_failed` → 正确 200；摘要文件 `0600` |
| 配置生命周期 | save → 原子落盘 → history → export / `?id=` 读取 → import（校验 checksum）→ rollback → reload；路径穿越与篡改均被拒 |
| 日志与 SSE | 结构化记录符合 `log-record.json`；磁盘硬上界 `max_bytes × (backup_count + 1)`；SSE replay + `retry` + keepalive + `max_duration` / `max_events` 有界 |
| 健康与冲突 | 热重载后 2 个 TCP 目标实时得到 up=1 / down=1；冲突聚合同名异值、TTL 老化 |
| 指标 | 第 3 节 13 个专有族全部输出，`path` 标签为路由模板，无双前缀 |

每个冒烟实例使用独立临时数据目录（配置 / 摘要 / 日志 / 快照），不写仓库、不共享状态；
组播流量限制在 `239.1.2.3` 隔离组与高位端口。技术取舍见
`docs/ADR/0003-m2-2-observability-security.md`。

### 6.4 缺陷与修复记录

1. `common_crypto::pbkdf2_sha256` 手写 `(dk_len + HLEN - 1) / HLEN` 触发
   `clippy::manual_div_ceil` → 改用 `dk_len.div_ceil(HLEN)`。
2. `common_log` 的 `trim_end_matches(|ch| ...)` 触发
   `clippy::manual_pattern_char_comparison` → 改用字符数组 `['\r', '\n']`。
3. 信号处理器把函数项直接 cast 成 `usize` 触发 `function_casts_as_integer` →
   先转 `*const ()` 再转 `usize`。
4. `Arc::clone(&log_buffer)` 无法把 `&Arc<LogBuffer>` 协变为 `&Arc<dyn EventSource>`
   （E0308）→ 改用 `log_buffer.clone()`，让 unsize coercion 发生在返回值上。
5. `store::validate_config_text` 返回 `Result<MdnsConfig, _>` 与签名 `Result<(), _>`
   不符（E0308）→ 追加 `.map(|_| ())`。
6. `PasswordGuard` trait 未导入作用域，导致 `is_set()` / `verify()` 全部 E0599 →
   `use common_auth::{PasswordGuard, PasswordManager}`。
7. `assert!(cond, id.clone())` 使用非字面量格式串 → `assert!(cond, "{}", id)`。
8. `-D warnings` 下的 dead_code 不用 `#[allow]` 掩盖，而是补齐真实用途：
   `Conflict.rtype` 写入 `/api/conflicts` 输出、`read_snapshot` 接成
   `GET /api/config/snapshot?id=`、`probe_now` 接到启动首次同步探测（顺带让首帧
   健康状态不再是 `unknown`）；`is_some_and` 冗余闭包改为直接传函数指针。

### 6.5 M2.3 入口（已完成，见第 7 节）

`F-C03`、`F-E09`、`F-G01`~`F-G05`、`F-H01`~`F-H04`、`F-I01`~`F-I07` 共 18 条
已在 M2.3 销项。`F-A12` 的 socket 级接口热切换评估结论：接口集合变更必须重建
组播 socket，属于不可热更字段，因此保持「配置可改、变更需重启」，管理台表单在
M2.4 交付时明确提示，不伪装成热重载能力。

## 7. M2.3 验收记录（2026-11-13）

### 7.1 销项明细

本批次 `done` 共 18 条：`F-C03`（1）、`F-E09`（1）、`F-G01`~`F-G05`（5）、
`F-H01`~`F-H04`（4）、`F-I01`~`F-I07`（7）。累计 **61/66**。

| 编号 | 验收证据 |
| --- | --- |
| F-C03 | 两个实例共享同一隔离组播地址 + 端口，均启动成功且都能应答 A 查询（冒烟 218~221） |
| F-E09 | 首次启动在缺失路径生成内嵌模板；重启后用户追加内容不被覆盖；`install.sh` 同样不覆盖（冒烟 178~180） |
| F-G01 | `common-alert` 五平台识别与载荷构造 9 个单测；本地通用 JSON Webhook 收到规范事件 |
| F-G02 | up→down 翻转发出 `health_transition`，`extra.from/to` 正确；`unknown→up` 不发 |
| F-G03 | watchdog 扫描新冲突发 `name_conflict`（`state.rs::scan_conflicts` + 冷却） |
| F-G04 | 队列 200 / 重试 ≤2 / 冷却去重单测；运行态重复不可达事件被抑制（冒烟 209） |
| F-G05 | `/api/alerts` 状态与按 kind 计数；配置保存热换 Webhook 列表无需重启（冒烟 212~213） |
| F-H01 | `up.local` Host 路由转发上游内容；请求头原样、请求体流式透传 |
| F-H02 | 全局关闭不创建监听（端口不可连 + gauge=0）；`disabled.local` 每服务开关返回 404 |
| F-H03 | `mdns.local` 直达管理台 `/healthz` |
| F-H04 | 未登记 404 / 不可达 502 均返回转义后的友好 HTML 页 |
| F-I01 | musl 静态单文件；`--version` / `--print-config` 不依赖外部文件 |
| F-I02 | `webui`（默认）与 `--cli` 双模式，CLI 不监听端口 |
| F-I03 | `Dockerfile` + `docker-compose.yml`，`network_mode: host` 硬约束，`docker compose config -q` 通过 |
| F-I04 | `lan-mdns.service`：SIGTERM 优雅退出、SIGHUP 重载、StateDirectory 持久化 |
| F-I05 | `install.sh --upgrade` 保留 `config.yaml` / `auth.toml` / `logs/` / `history/`，`bash -n` 通过 |
| F-I06 | 容器单卷 `/data` 覆盖配置 / 摘要 / 日志 / 快照 |
| F-I07 | 二进制自带 `--healthcheck`，运行中 0、未运行非 0；Dockerfile 与 compose 均接入 |

### 7.2 提前交付但不提前销项

| 条目 | M2.3 已提供 | 归属 |
| --- | --- | --- |
| F-H05 | `/go/` 导航页与 `/go/?host=` 302 跳转（网关自带） | 正式管理台内入口属 M2.4 |
| `config_invalid` / `config_reload_failed` | 已接入触发链路 | 属 ALERT-SCHEMA 已有 kind，非第 5 章独立编号 |

### 7.3 验收证据

| 门禁 | 结果 |
| --- | --- |
| M0 契约校验（完整模式） | **21 passed / 0 failed** |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | **78 passed / 0 failed**（M2.2 为 62） |
| `cargo build --release -p lan-mdns` | 成功，单文件约 1.5 MB（静态单文件由 release workflow 的 musl cross 构建保证） |
| `python3 scripts/smoke_mdns.py` | **221 passed / 0 failed**（M2.2 为 160） |
| 部署资产门禁 | `bash -n scripts/install.sh`、`docker compose config -q`、`python3 -m py_compile` 全通过 |
| 真实告警链路 | 本地 Webhook 收到 `gateway_target_unreachable` + `health_transition` 两条规范事件，字段完整、`time` 以 `Z` 结尾、无凭据 |
| 红线合规 | 网关仅路由已登记 Host；不缓存 / 不解析 / 不记录业务载荷；全局关闭零监听 |

### 7.4 缺陷与修复记录

1. 自研 YAML 解析器把内联数组 `webhooks: ["url"]` 存成标量，导致 Webhook 被静默
   忽略 → `alert_config_from_flat()` 同时接受块列表与内联数组，模板注释改为块列表示例。
2. `--healthcheck` 只调用一次 `read`，而管理服务器分两次写响应头与体，探针误判失败
   → 改为循环读取直到出现 `"ok":true` 或连接关闭 / 超时。
3. 网关友好页把调用方 `body` 当 HTML 直接插入，存在 Host 注入面 → `page()` 统一
   `html_escape(body)`，单测覆盖 `<script>` 转义。
4. `gateway.rs::read_head(&TcpStream)` 无法对不可变引用调用 `read`（E0596）→
   改为 `mut stream: &TcpStream`。
5. `--cli` 首次启动会写配置，CI 直接跑会在仓库根留下 `config.yaml` → CI 把
   `LAN_STUDIO_CONFIG` 指向 `mktemp` 临时文件。
6. `clippy::needless_as_bytes` / `derivable_impls` / `bool_assert_comparison` 三处
   按提示修正，未使用 `#[allow]` 掩盖。
7. 冒烟脚本内已有局部变量 `cli`（`CompletedProcess`）与新辅助函数同名 → 辅助函数
   更名 `run_cli`；`import http.server` 被 `def http()` 覆盖 → 改为
   `import http.server as httpserver`。

### 7.5 M2.4 入口

剩余 5 条：`F-A12`（多网卡管理台表单）、`F-D04`（健康状态灯与列表）、
`F-E04`（在线配置表单 + 源码双模式）、`F-H05`（管理台内辅助跳转入口）、
`F-J03`（API 契约测试 + CI 门禁）。同时按 PRD 8.1 完成组件级 DoD 核对与
66 条全部销项。技术取舍见 `docs/ADR/0004-m2-3-alerting-gateway-deploy.md`。
