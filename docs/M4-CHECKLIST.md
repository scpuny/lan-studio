# M4 销项清单：P1 组件 `lan-hosts` / `lan-wol`

> 依据 `docs/PRD.md` §4.6 / §4.7 / §7.5 / §8.1，落地方案见 `docs/M4-PLAN.md`。
> 本表是 M4 的**唯一收尾依据**；PRD §7.5 的「与 `lan-mdns` 无功能重叠、无强耦合」是里程碑闸门。

## 1. 批次划分

| 批次 | 主题 | 主要交付 | 状态 |
| --- | --- | --- | --- |
| **M4.0** | 契约与骨架 | `common/probe`（只读 TCP 探测 + 去抖状态机）、2 个 workspace member、2 份配置模板、openapi +14 端点、METRICS §3.6、ALERT-SCHEMA +3 kind | **done** |
| **M4.1** | `lan-hosts` | MappingStore、UPSERT / 导入 / 冲突检测、hosts 渲染、原子发布、7 个端点 | **done** |
| **M4.2** | `lan-wol` | DeviceStore、Magic Packet、冷却、只读探测、去抖状态机、唤醒审计、7 个端点 | **done** |
| **M4.3** | 验收脚本 | `scripts/test_m4.py`（统一契约 + 配置闭环 + 组件专属 + 5 条无重叠断言） | **done** |
| **M4.4** | 门禁与文档 | README / CI 同步、`docs/M4-CHECKLIST.md`、全量 CI 门禁 | **done** |

## 2. `lan-hosts` 验收（PRD §4.6）

| 验收项 | 实现 | 验证 | 状态 |
| --- | --- | --- | --- |
| 映射登记 UPSERT | `MappingStore`，按 name 覆盖，JSON lines 原子持久化 | 运行态 upsert / remove / mappings 清单 | done |
| 名称与地址校验 | 小写域名字符集、`IpAddr::from_str`、数量上限 | 非法 IP 与 `.local` 后缀拒绝 | done |
| 冲突检测 | `conflict_policy=reject` 时同名异 IP 拒绝并计数 | 运行态同名异 IP 返回 `invalid_config` | done |
| 批量导入 | 标准 hosts 文本解析（跳过注释 / 空行，别名归入 aliases） | 运行态导入 2 条以上 | done |
| 渲染稳定 | 过滤 disabled → 展开别名 → 排序 → 去重 → 标准 hosts 行 | 连续两次 render 文本完全一致 | done |
| 原子发布 | `.tmp` + `fs::rename`；关闭时拒绝、失败保留旧文件 | 运行态发布 / 禁用 / 目录不存在三组断言 | done |
| 业务数据隔离 | 只写 `hosts.mappings_path` 与 `hosts.output_path`，路径禁 `..` | 运行态检查自身数据文件存在 | done |

## 3. `lan-wol` 验收（PRD §4.7）

| 验收项 | 实现 | 验证 | 状态 |
| --- | --- | --- | --- |
| 设备登记 | `DeviceStore`，MAC 严格 6 组十六进制，JSON lines 原子持久化 | 运行态注册 / 非法 MAC 拒绝 / 删除 | done |
| **目标必须来自已登记设备** | `POST /api/wol/wake` 只按 `device_id` 查表，请求体 MAC/IP 一律忽略 | 未登记 404；删除后再唤醒 404 | done |
| Magic Packet | `6×0xFF + 16×MAC`，严格 102 字节，UDP `set_broadcast` | 回环 UDP socket 收包逐字节断言（不向真实 LAN 发包） | done |
| 广播默认关闭 | `send_enabled=false`，关闭时返回 409 且不静默成功 | 运行态断言，测试通过热重载开启 | done |
| 冷却限流 | 每设备 `cooldown_seconds` 内重复唤醒 429 | 运行态连续两次唤醒 | done |
| 只读探测 | `common_probe::TcpProbe`，仅 connect、无 payload | `common-probe` 单测对端收到 0 字节；运行态回环 TCP 探测 | done |
| 状态不得误判 | `common_probe::Debounce` 连续阈值 + 唤醒宽限期 | 单测交替样本不抖动、宽限期内不判 offline；运行态 3 次探测后 online | done |
| 唤醒审计 | 有界环形 `history_limit`，不含凭据 | 运行态 history total ≤ limit | done |

## 4. 里程碑闸门：无重叠 / 无强耦合（PRD §7.5）

| # | 断言 | 验证方式 | 状态 |
| --- | --- | --- | --- |
| 1 | 两个新 crate 不依赖任何其他组件 crate | `Cargo.toml` 扫描 | done |
| 2 | `lan-hosts` 不监听 5353 / 不加入组播 / 不构造 DNS 报文 | 源码扫描 `224.0.0.251` / `common_dns` / `MDNS_PORT` | done |
| 3 | 配置中不出现 `mdns.` 命名空间，运行时不读写 `lan-mdns` 文件 | `/api/config` raw 断言 | done |
| 4 | `lan-wol` 不调用 `lan-scan` API，仅支持文件导入 | 依赖与源码扫描 | done |
| 5 | 不暴露 `lan-mdns` 业务端点；`/healthz` 恒 `{"ok":true}` | `/api/records` 返回 404；健康检查断言 | done |

> 设计取舍：`lan-mdns` 已有内置网关静态 Host 路由，故 `lan-hosts` 只做静态映射的单一数据源 + 渲染 / 发布，不碰解析与转发。

## 5. 统一契约（PRD §3 / §8.1）逐组件

| 契约 | 实现 | 两组件 |
| --- | --- | --- |
| `/healthz` 恒 ok / `/readyz` | `common-component` | ✅✅ |
| 统一响应包装 + 8 错误码 | `common_api::ok/err` | ✅✅ |
| 鉴权（token / loopback / 写密码 / 409 setup_required） | `App::route_write` | ✅✅ |
| 配置 GET / POST（校验 → 快照 → 原子替换 → 热重载） | `Runtime::save` | ✅✅ |
| 快照导出 / 导入（checksum）/ 历史 / 回滚（ID 白名单） | `Snapshot` / `Runtime` | ✅✅ |
| 结构化日志 + SSE + 有界轮转 | `common-log` | ✅✅ |
| 指标 9 项必备族 + `0.0.4` content type | `common-metrics` | ✅✅ |
| 告警规范事件 + `/api/alerts` | `Runtime::emit_alert` | ✅✅ |
| 离线 WebUI 六区域、无外部资源 | `CONSOLE_HTML` | ✅✅ |
| 组件 README / 配置模板 / `--check` 离线校验 | `run()` CLI | ✅✅ |

## 6. 契约增量

| 文件 | 变更 |
| --- | --- |
| `Cargo.toml` | workspace 16 → 19 member；新增 `common-probe` 路径依赖 |
| `contracts/openapi.yaml` | +14 个组件端点（hosts 7 / wol 7）；component tag 说明补充 M4 |
| `scripts/verify_contracts.py` | `REQUIRED_PATHS` +14、`REGISTERED_KINDS` +4（含补齐 M3 的 `cert_issued`） |
| `docs/METRICS.md` | 新增 §3.6：hosts 5 族 / wol 5 族，标签均为固定枚举 |
| `docs/ALERT-SCHEMA.md` | 新增 `hosts_publish_failed` / `hosts_conflict_detected` / `wol_wake_sent`；复用已登记的 `device_wake_failed` |
| `.github/workflows/ci.yml` | 新增 `cargo build --workspace`、`test_m3.py` 回归、`test_m4.py` |

> 方案偏差：M4-PLAN 中的 `wol_device_unreachable` 未新增，改为复用登记表已有的 `device_wake_failed`，避免 kind 语义重复。

## 7. 门禁记录

| 门禁 | 命令 | 结果 |
| --- | --- | --- |
| 代码格式 | `cargo fmt --all -- --check` | 通过，0 diff |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | 通过，0 warning |
| Rust 单测 | `cargo test --workspace` | 32 个测试目标，150 passed / 0 failed |
| 静态契约 | `python3 scripts/verify_contracts.py` | 21 passed / 0 failed（含 +14 端点 / +4 kind） |
| M3 回归 | `python3 scripts/test_m3.py` | 66/66 passed — M3 RUNTIME OK |
| M4 运行态 | `python3 scripts/test_m4.py` | 76/76 passed — M4 RUNTIME OK |
| 前端单文件 | `cd components/lan-mdns/webui && npm run verify` | typecheck + 838.30 kB 自包含 + RENDER OK |
| 全量构建 | `cargo build --workspace` | 通过，6 个组件二进制产出 |

## 8. 已知边界 / 不做的部分

- **不实现统一出口**（M5）、**不实现 P2**（`lan-sync` / `lan-ssdp`）、不新增第 9 个组件。
- M4 控制台沿用 `common-component` 原生 JS 六区域页（PRD §3.7 偏差已登记）；Arco 统一壳收敛归 M5，M2 前端产物未改动。
- `common/probe` 只供 `lan-wol` 依赖，M3 `lan-scan` 保持原实现不动，避免回归；M5 再统一收敛。
- `lan-hosts` 不做 DNS 应答、不做 HTTP 代理、不向其他组件推送配置。
- `lan-wol` 不做 ICMP / ARP 探测、不做远程关机；广播发送默认关闭，容器 / macOS 下可把 `wol.broadcast` 配成网段定向广播地址。
- 业务数据（映射 / 设备 / 审计）不进配置快照，避免导入配置覆盖运行态数据。
- `scripts/install.sh` 与 `.github/workflows/release.yml` 仍只发布 `lan-mdns`（与 M3 三组件一致）；多组件统一发布归 M5。

## 9. 执行记录

收尾执行于 2026-11-14，工作区 `/Users/yangbo/WorkBuddy/lanStudio`，工具链 Rust 1.99 / Vite 6.4.6 / PyYAML 6.0.3。

### 9.1 门禁逐项结果

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | 通过，0 diff（新 crate 由 `cargo fmt --all` 统一格式化） |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | 通过，0 warning |
| 3 | `cargo test --workspace` | **32 个测试目标，150 passed / 0 failed**（M3 收尾为 119，新增 31 项） |
| 4 | `python3 scripts/verify_contracts.py` | **21 passed / 0 failed**（完整 PyYAML 模式；必备端点清单已覆盖 M4 的 14 个新路径） |
| 5 | `python3 scripts/test_m3.py` | **66/66 passed — M3 RUNTIME OK**（回归无破坏） |
| 6 | `python3 scripts/test_m4.py` | **76/76 passed — M4 RUNTIME OK** |
| 7 | `cd components/lan-mdns/webui && npm run verify` | typecheck 通过；单文件 838.30 kB 自包含；`markers 12/12 / nav 6 / panels 10 / dots 14 / kpi 25 / console 0 / RENDER OK` |
| 8 | `cargo build --workspace` | `Finished dev profile`，6 个组件二进制产出 |

### 9.2 集成阶段修复

| 位置 | 问题 | 修复 |
| --- | --- | --- |
| `common/probe/src/lib.rs` `Debounce::observe` | 唤醒宽限期内累积的失败样本，导致宽限期一结束首次采样就误判 offline | grace 内失败样本清零并直接返回 None；成功样本仍可提前转 Online 并清除 grace。`rustc` 独立单测 4/4 |
| `components/lan-wol/src/wol.rs:744` | 单测 `let reopened = DeviceStore::open(...)` 少 `mut`，`cargo build` 不编译 test target 所以首轮未暴露 | 改为 `let mut reopened` |
| `scripts/test_m4.py` | 与 cargo / M3 脚本并行时首个写请求 8s 超时（偶发） | `urlopen` timeout 8 → 20s，并把验收脚本串行执行 |
| `scripts/test_m4.py` 无重叠断言 | 配置模板注释里出现 "lan-mdns" 说明边界，被字符串断言误判 | 剥离注释行与行内注释后，只对生效 YAML 键检查 `mdns.` / `mdns:` / `lan-mdns` |
| 告警 kind | M4-PLAN 中的 `wol_device_unreachable` 与登记表已有的语义重复 | 复用 `device_wake_failed`，只新增 3 个 kind（见第 6 节） |

以上修复后重跑 9.1 全部 8 项，无回归。

### 9.3 M4 运行态 76 项构成

| 组 | 项数 | 要点 |
| --- | --- | --- |
| 离线校验 | 2 | 两组件 `--check` 不启网络 |
| 统一契约 | 22 | 每组件 11 项：healthz / readyz / status / 9 指标族 / config / logs / alerts / auth / history / 离线六区域 WebUI / 409 写保护 |
| 配置闭环 | 16 | 每组件 8 项：改密 + save → 快照 → 回滚 → 导入 → reload → 非法配置拒绝 |
| `lan-hosts` | 16 | 空态、UPSERT、`.local` 拒绝、冲突、非法 IP、导入、渲染稳定性、清单、禁用发布、3 次热重载、原子发布、失败保留旧文件、数据隔离、删除 |
| `lan-wol` | 14 | 空态、登记、非法 MAC、未登记 404、禁用 409、热重载、102 字节逐字节断言、冷却 429、只读探测、去抖 online、审计有界、数据隔离、删除后再唤醒 404 |
| 无重叠 / 无耦合 | 6 | 依赖扫描、源码无组播/DNS、openapi 覆盖、配置无 mdns 命名空间、两组件均不暴露 `/api/records` |

### 9.4 结论

M4.0 ~ M4.4 全部 done；PRD §7.5 的「与 `lan-mdns` 无功能重叠、无强耦合」由 5 组运行态断言验证通过；统一契约对两个新组件逐项 ✅；M3 回归 66/66 无破坏；全量 CI 门禁全绿。**M4 可销项**，边界与不做部分见第 8 节。

> M4 全部改动已提交为 `2b5b259`（2026-11-14，**未 push**）；M2 离线产物 `common/webui/src/assets/index.html` 本次未改动（前端源码未变，`npm run verify` 产物与已提交版本一致）。
