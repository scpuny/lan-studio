# M3 销项清单：其余 P0 组件

> 依据 `docs/PRD.md` §4.3 / §4.4 / §4.5、§7.5 里程碑与 §8.1 组件级 DoD。
> 本表是 M3 的**唯一收尾依据**；`lan-relay` 环路检测 4 项硬性验收是里程碑闸门。

## 1. 批次划分

| 批次 | 主题 | 主要交付 | 状态 |
| --- | --- | --- | --- |
| **M3.0** | 共享契约运行时 | `common/component`：18 个必备端点、配置生命周期、快照、结构化日志 + SSE、指标、鉴权、离线六区域控制台 | **done** |
| **M3.1** | `lan-relay` | 多网卡中继、白名单、EDNS0 OPT 跳数尾标、四道环路防线、转发统计、注入端点 | **done** |
| **M3.2** | `lan-ca` | 单根自签、签发 / 续期 / 吊销 / CRL / 清单 / 验签 / 审计 / 安全导出 | **done** |
| **M3.3** | `lan-scan` | CIDR 展开、只读 TCP 探查、服务识别、速率与并发硬上界、结果快照 | **done** |
| **M3.4** | 契约与门禁 | openapi / METRICS / ALERT-SCHEMA / README 同步、运行态验收脚本、全量 CI 门禁 | **done** |

## 2. 里程碑闸门：`lan-relay` 环路检测（PRD §4.3 / D8）

| # | 硬性验收 | 实现 | 验证 | 状态 |
| --- | --- | --- | --- | --- |
| 1 | 包指纹去重窗口 | SHA-256 前 8 字节指纹；`FingerprintWindow` 5s + 容量双约束 | 单测 `guard1_fingerprint_window_deduplicates_and_expires`；运行态 `/api/relay/inject` 二次注入返回 `duplicate` | done |
| 2 | 出入网卡隔离 | `targets = interfaces - ingress`，空则丢弃 | 单测 `guard2_ingress_is_never_an_egress_target`；运行态断言 `targets` 不含 ingress、单网卡配置下返回 `same_interface` | done |
| 3 | TTL / 跳数约束 | EDNS0 OPT RR 的 TTL 高 8 位携带剩余跳数，逐跳递减，归零丢弃 | 单测 `guard3_ttl_decrements_to_zero_then_drops`；运行态 `ttl=0` 返回 `ttl_exceeded` | done |
| 4 | 自生成包不转发 + 60s 无广播风暴 | 本地指纹窗口 + 每秒转发硬上限 | 单测 `guard4_self_generated_packets_are_never_forwarded`、`sixty_second_loop_storm_produces_no_broadcast_storm`；运行态 `/api/relay/mark-local` + 批量注入 | done |

附加约束：白名单网段 CIDR（`outside_whitelist`）、畸形报文（`malformed`）、速率上限（`rate_limited`）均有单测与运行态断言。

> 报文尾标为合法 EDNS0 OPT 记录，转发时先剥离再重加，ARCOUNT 始终一致；标准 mDNS 客户端忽略该记录即可正常解析。

## 3. `lan-ca` 验收（PRD §4.4）

| 验收项 | 实现 | 状态 |
| --- | --- | --- |
| 单根自签（D8） | 32 字节根密钥，`0600` 权限，指纹作为 issuer | done |
| 内网域名签发 | 名称合法性 + 后缀白名单，HMAC-SHA256 签名，PEM 封装 | done |
| 续期 | 重签 + 延长期限 + 审计 | done |
| 吊销列表 | CRL 含 serial / time / reason | done |
| 证书清单 | `/api/ca/certificates` | done |
| 私钥不落日志 / 不进快照导出 | 单测 `root_key_never_leaks_into_export_or_audit` 断言密钥 hex 不出现在导出 / 状态 / store 文件 | done |
| 签发与吊销有审计记录 | 审计环形缓冲（500 条）+ `/api/ca/audit` | done |

**信任模型边界**：本组件是单根自签的 HMAC 内网凭证，**不是 X.509 公钥证书**，不接入浏览器 TLS 信任链。workspace 保持零第三方 crypto crate；如需标准 X.509，见组件 README 的升级路径。

## 4. `lan-scan` 验收（PRD §4.5）

| 验收项 | 实现 | 状态 |
| --- | --- | --- |
| 网段扫描 | `expand_cidr`，`max_hosts` 硬上界，超大段在建 socket 前拒绝 | done |
| 主机发现 | TCP `connect_timeout`，任一端口开放即存活 | done |
| 端口 / 服务轻量识别 | 端口→服务名 + 只读 banner（≤256B） | done |
| 结果快照 | `/api/scan/snapshot`，内存与磁盘有界 | done |
| 速率与并发可配且有上界 | `RateLimiter` 固定窗口 + worker 池 `concurrency` 钳制 1..64 | done |
| 不发送破坏性载荷 | `TcpProbe` 无 `write`；单测对端断言收到 0 字节 | done |
| 结果不写入其他组件配置 | 只写 `scan.results_path`（原子替换），路径禁止 `..` | done |

## 5. 统一契约（PRD §3 / §8.1）逐组件

| 契约 | `common-component` 实现 | 三组件 |
| --- | --- | --- |
| `/healthz` 恒 `{"ok":true}` / `/readyz` | `view_healthz` / `view_readyz` | ✅✅✅ |
| 统一响应包装 + 8 错误码 | `common_api::ok/err` | ✅✅✅ |
| 鉴权（token / loopback / 写密码 / 409 setup_required） | `common-api` + `common-auth` | ✅✅✅ |
| 配置：GET / POST（校验→快照→原子替换→热重载） | `Runtime::save` | ✅✅✅ |
| 快照导出 / 导入（checksum 校验）/ 历史 / 回滚（ID 白名单） | `Snapshot` / `Runtime` | ✅✅✅ |
| 结构化日志 + SSE + 有界轮转 | `common-log` + `common-api` | ✅✅✅ |
| 指标 9 项必备族 + `0.0.4` content type | `common-metrics` | ✅✅✅ |
| 告警规范事件 + `/api/alerts` | `Runtime::emit_alert` | ✅✅✅ |
| 离线 WebUI 六区域、无外部资源 | `CONSOLE_HTML` | ✅✅✅ |
| 组件 README / 配置模板 / `--check` 离线校验 | `run()` CLI | ✅✅✅ |

## 6. 门禁记录

| 门禁 | 命令 | 结果 |
| --- | --- | --- |
| 代码格式 | `cargo fmt --all -- --check` | 通过，0 diff |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | 通过，0 warning |
| Rust 单元 + 集成测试 | `cargo test --workspace` | 28 个测试目标，119 passed / 0 failed |
| 静态契约 | `python3 scripts/verify_contracts.py` | 21 passed / 0 failed（PyYAML 完整模式） |
| M3 运行态验收 | `python3 scripts/test_m3.py` | 66/66 passed — M3 RUNTIME OK |
| 全量构建 | `cargo build --workspace` | 通过，4 个组件二进制产出 |
| 前端单文件 | `cd components/lan-mdns/webui && npm run verify` | typecheck + 838.30 kB 自包含 + RENDER OK |
| 离线产物 | `common-component` 单测 `offline_console_has_no_external_resources` | 通过 |

## 7. 已知边界 / 不做的部分

- **不实现 P1 / P2**（`lan-hosts` / `lan-wol` / `lan-sync` / `lan-ssdp` 与统一出口归 M4 / M5）。
- M3 控制台是 `common-component` 内的**原生 JS 自包含页**（沿用 Calm Console 设计令牌、六区域、深浅主题、零外部资源）；Arco 版统一壳收敛属于 M5 统一出口范围，已在 PRD §3.7 偏差登记。
- `lan-relay` 多网卡严格选路依赖平台组播能力；macOS 上同端口通常只能绑定一个 socket，传输状态标为 `unavailable`/单 socket，但 API 与四道环路检测完整可用。
- `lan-ca` 根密钥 / store 路径为进程级固定，热重载其余字段生效，改路径需重启。
- `lan-scan` 只做 TCP 连通性探查（无 ICMP、无 UDP 扫描、无漏洞检测），结果不跨组件写入。

## 8. 执行记录

收尾执行于 2026-11-13 23:45，工作区 `/Users/yangbo/WorkBuddy/lanStudio`，工具链 Rust 1.99 / Vite 6.4.6 / PyYAML 6.0.3。

### 8.1 门禁逐项结果

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo fmt --all && cargo fmt --all -- --check` | 通过，0 diff（原先 6 处格式差异已归一，主要在 `common/component/src/lib.rs`） |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | 通过，0 warning（修复 7 类 lint，见 8.2） |
| 3 | `cargo test --workspace` | 28 个测试目标，**119 passed / 0 failed** |
| 4 | `python3 scripts/verify_contracts.py` | **21 passed / 0 failed**（含 openapi 必备端点、8 个 ErrorCode、9 个指标族、告警 kind 登记表） |
| 5 | `python3 scripts/test_m3.py` | **66/66 passed — M3 RUNTIME OK** |
| 6 | `cargo build --workspace` | `Finished dev profile`；产出 `lan-mdns` / `lan-relay` / `lan-ca` / `lan-scan` |
| 7 | `cd components/lan-mdns/webui && npm run verify` | `vue-tsc --noEmit` 通过；单文件 `common/webui/src/assets/index.html` 838.30 kB、零外链；`markers 12/12`、`nav 6 / panels 10 / dots 14 / kpi 25`、`console 0`、`RENDER OK` |

### 8.2 收尾阶段修复的 clippy / fmt 问题

| 文件 | lint | 修复 |
| --- | --- | --- |
| `components/lan-ca/src/ca.rs:205` | `manual_strip` | `line.starts_with("CERT ")` + `&line[5..]` → `line.strip_prefix("CERT ")` |
| `components/lan-ca/src/ca.rs:209` | `manual_strip` | `AUDIT ` 前缀同样改为 `strip_prefix` |
| `components/lan-ca/src/ca.rs:462` | `manual_div_ceil` | `(len + 2) / 3` → `len.div_ceil(3)` |
| `components/lan-scan/src/scan.rs:184` | `manual_unwrap_or_default` | `match stream.read(...)` → `stream.read(...).unwrap_or_default()` |
| `components/lan-relay/src/loop_guard.rs:10` | `doc_lazy_continuation` | 模块文档列表后补 `//!` 空行，使段落独立 |
| `components/lan-mdns/src/store.rs:225` | `unnecessary_sort_by` | `sort_by(|l, r| r.0.cmp(&l.0))` → `sort_by_key(\|item\| Reverse(item.0))` |
| `components/lan-mdns/src/store.rs:327` | `unnecessary_sort_by` | 同上 |
| `common/component/src/lib.rs` 等 | `cargo fmt` diff | `cargo fmt --all` 自动归一 |

以上均为等价改写，不改行为；改写后重跑 8.1 全部 7 项门禁，无回归。

### 8.3 运行态验收要点（`scripts/test_m3.py`）

- 三组件统一契约：`/healthz`、`/readyz`、配置 GET/POST、`/api/logs`、`/api/alerts`、鉴权写保护、`/api/config/history`、离线六区域 WebUI、409 `setup_required`。
- 配置生命周期：save 走「校验 → 快照 → 原子替换 → 热重载」，快照契约形状、回滚、导入（checksum 校验）、手动 reload、非法配置拒绝全部通过。
- `lan-relay` 四道防线逐项通过：G2 出入网卡隔离 + 正常转发、G1 指纹去重、G3 `ttl=0` 丢弃、G4 自生成包 / mark-local 反射丢弃；附加 CIDR 白名单、单网卡 `same_interface`、60s 环路压力注入无放大。
- `lan-ca`：根指纹不含密钥、签发 / 验签 / 通配符拒绝 / 吊销 / CRL / 审计链 / 根密钥不进导出与快照。
- `lan-scan`：有界只读任务、loopback 端口发现、超大网段拒绝、自包含快照、只写自身结果文件、速率与并发上界暴露。

### 8.4 结论

M3.0 ~ M3.4 全部 done；`lan-relay` 环路检测 4 项硬性验收（含 60 秒环路压力测试无广播风暴）通过；统一契约对三个新组件逐项 ✅；全量 CI 门禁全绿。**M3 可销项**，边界与不做部分见第 7 节。

> 注意：`common/webui/src/assets/index.html` 是编译进 Rust 二进制的离线产物，M2 前端改动后须一并提交；当前所有改动仍在工作区，尚未 commit。
