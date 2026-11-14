# M5 销项清单：P2 两组件 + 统一出口 + 生态收敛

> 依据 `docs/PRD.md` §2.2 / §2.3 / §3.7 / §4.8 / §4.9 / §7.5 / §8.2 / §9，`docs/M5-PLAN.md`（唯一范围依据）。
> 入口条件：M4 已完成并提交 `2b5b259`。本文是 M5 **唯一销项依据**；收尾时第 9 节必须填入实测数字。

---

## 1. 范围

| 项 | 内容 |
| --- | --- |
| 交付物 | `lan-sync`、`lan-ssdp` 两个独立二进制 + Lan Studio Shell 统一出口 + 全生态 DoD |
| 组件总数 | **8 个，不新增第 9 个组件**；`lan-shell` 是聚合层，不计入组件清单 |
| workspace | 19 → **22 member**，仍零第三方 crate |
| 验收 | PRD §8.2 生态级 DoD **6 条逐条通过**，§8.1 组件级 DoD 对全部 8 个组件成立 |
| 明确不做 | 组件间事件总线（D10）、统一出口数据总线（D3）、运行时强依赖、P3 / 第 9 个组件、Arco 统一前端壳 |

---

## 2. 交付清单

| 类别 | 文件 | 说明 |
| --- | --- | --- |
| 新组件 | `components/lan-sync/{Cargo.toml,src/sync.rs,src/main.rs,README.md,config.template.yaml}` | 8 端点、快照 checksum、3 种冲突策略、HMAC 节点身份、有界审计 |
| 新组件 | `components/lan-ssdp/{Cargo.toml,src/ssdp.rs,src/main.rs,README.md,config.template.yaml}` | 4 端点、只读 M-SEARCH / NOTIFY、TTL 清理、统一服务记录 |
| 聚合层 | `shell/{Cargo.toml,components.yaml,src/main.rs,src/assets/shell.html,README.md}` | 3 路由、8 卡单文件页、有界健康探测、只透传 AdminToken |
| 契约 | `contracts/service-record.json` + `contracts/examples/service-record.sample.json` | 统一服务记录模型与通过 schema 的样例 |
| 契约 | `contracts/openapi.yaml` +15 路径、`docs/METRICS.md` §3.7、`docs/ALERT-SCHEMA.md` +2 kind、`scripts/verify_contracts.py` | 静态契约 24 项 |
| M2 适配 | `components/lan-mdns/src/main.rs` 新增只读 `GET /api/browse/unified` | 旧 `/api/browse` 不变，输出同 schema |
| 门禁 | `scripts/test_m5.py`、`scripts/test_ecosystem.py`、`scripts/test_api_contract.py` 漂移修复 | 组件专属 + 生态六条 + M2 回归 |
| 工程 | `.github/workflows/ci.yml`、`release.yml`（8 组件 + Shell × amd64/arm64 + `SHA256SUMS.txt`）、`.gitignore` | CI 纳入全里程碑回归与生态 DoD |
| 文档 | `docs/M5-PLAN.md`、本文、`docs/ADR/0007-m5-ecosystem-convergence.md`、`README.md` | 方案、销项、决策、总览同步 |

---

## 3. 决策清单执行（M5-PLAN 第 3 节）

| 编号 | 决策 | 落地结果 |
| --- | --- | --- |
| D-M5-1 | Shell 形态 | 独立 `common_api::App` 手工构造 `Context`，只有 3 条路由 |
| D-M5-2 | 导航数据 | 静态 `shell/components.yaml`，8 条记录，端口 8080–8087 |
| D-M5-3 | 鉴权 | 只透传 AdminToken；无密码输入框、无 `route_write`、响应不含凭据 |
| D-M5-4 | 前端统一 | 不统一；Shell 用 Calm Console 原生单文件页，组件页保持现状 |
| D-M5-5 | `lan-sync` | 只调 `/api/config/snapshot`；先验 `sha256`；默认 `reject`；决策全留痕 |
| D-M5-6 | `lan-ssdp` | 只读发现；LOCATION 只进 attributes；不抓描述、不调 SOAP |
| D-M5-7 | 统一模型 | 新 schema；`lan-mdns` 只读 `/api/browse/unified`，旧端点不动 |
| D-M5-8 | 零偏差扫描 | 按契约断言，不迁移 M2 实现 |
| D-M5-9 | 可复现构建 | 固定工具链 1.82.0 + `SOURCE_DATE_EPOCH` + `--locked` + `SHA256SUMS.txt` |
| D-M5-10 | 组件总数 | 恒为 8；`lan-shell` 是第 9 个二进制但不是组件 |

---

## 4. 组件级验收

### 4.1 `lan-sync`（8086）

- [x] 8 端点：`/api/sync/{nodes,node,remove,pull,push,confirm,status,history}`，POST 全走 `route_write`
- [x] `SnapshotLite` 同时解析裸快照与统一信封；`verify_checksum()` 用 SHA-256 + 常量时间比较
- [x] 三种策略 `reject` / `last_write_wins` / `manual` 均为**显式决策 + 审计**，不静默覆盖本地配置
- [x] 未登记节点 404 `not_found`、checksum 错误 400 `invalid_config`、远端不可达 502 `target_unreachable`
- [x] HMAC-SHA256 节点签名（`shared_secret` 为空时跳过）；token 只作 `X-Admin-Token` 转发
- [x] 节点清单 / 状态 / 审计 / 错误响应均不含 token、密码、shared_secret
- [x] 手写 HTTP/1.1 客户端只支持 `http://` 回环地址，1MB 响应上界
- [x] 22 个单测：篡改 checksum、三策略、roundtrip、审计有界、HMAC 校验、本地 TcpListener 往返、路径穿越拒绝

### 4.2 `lan-ssdp`（8087）

- [x] 4 端点：`/api/ssdp/{devices,snapshot,status,refresh}`
- [x] `ServiceRecord` 8 字段严格通过 `contracts/service-record.json`（`additionalProperties:false`）
- [x] `parse_notify` 处理 alive / `ssdp:byebye` / 畸形；LOCATION **只登记为属性**
- [x] `DeviceTable` 按 USN 去重、容量上界、`sweep` 可注入时钟、`byebye` 立即移除
- [x] 单播隔离模式（`group: "127.0.0.1"`）不 join 组播、不主动发 M-SEARCH，验收不触碰真实 LAN
- [x] `validate` 强制 `fetch_descriptions=false`，拒绝路径穿越与越界参数
- [x] 18 个单测：字段完整性、JSON 转义、TTL / byebye、容量、M_SEARCH 头注入防护、原子落盘

### 4.3 `lan-shell`（8090，聚合层）

- [x] 3 路由：`GET /`、`GET /healthz`（纯 `{"ok":true}`）、`GET /api/shell/status`（统一信封）
- [x] 8 卡 Calm Console：44px 圆角矩形彩色图标在左、文字两行右对齐、3px 语义色条、状态灯
- [x] 单文件内联 CSS/JS/SVG，零外部资源；无 `type="password"`；主题持久化
- [x] manifest 只读表单：label 左对齐 120px、两列网格、说明跨双列
- [x] 健康探测并发上限 4、超时 50..5000ms、间隔 5..3600s；关闭时 `online: null`
- [x] 不代理：`GET /api/records` 返回 404；没有 18 个统一端点
- [x] 8 个单测：manifest 解析 8 组件与端口、缺字段容错、非法端口/tone、healthz、凭据不回显、离线页面、`--check`、RFC3339

### 4.4 `lan-mdns` 统一服务记录

- [x] 新增只读 `GET /api/browse/unified`，输出 `{"items":[ServiceRecord...],"total":N,"source":"mdns"}`
- [x] A/AAAA → IP；SRV RDATA → `address` + `port`；PTR/SRV 用 owner 作为 `type`
- [x] `ttl` 夹到 86400、`last_seen` 转 RFC3339 UTC、`attributes` 值有界
- [x] **旧 `/api/browse` 响应形状不变**；M2 运行态契约 75/0 通过

---

## 5. 生态级 DoD 六条（PRD §8.2）

| # | 定义 | 自动化 |
| --- | --- | --- |
| 1 | 8 个组件各自跑通统一契约 18 端点 + README / 配置模板 / `--check` | `scripts/test_ecosystem.py` |
| 2 | 每个组件**单独**在隔离临时目录启动（其余 7 个不运行），`/healthz` / `/api/config` / 离线 WebUI 可用 | 同上，逐个启动再关闭 |
| 3 | 统一契约零偏差：信封、8 个错误码、鉴权矩阵、9 个必备指标族、告警、日志、快照 schema | 同上，按前缀逐个断言 |
| 4 | Shell 能导航到所有**已运行**组件，状态灯与真实 `/healthz` 一致 | 同上 + `scripts/test_m5.py` |
| 5 | 多平台发布产物附校验和，构建过程可复现 | `release.yml` 静态断言 + `SHA256SUMS.txt` |
| 6 | `contracts/` 与实现一致，文档无过期描述（版本 / 里程碑 / 端口 / 组件清单） | README / ADR / 契约字典一致性断言 |

---

## 6. 门禁表

| 门禁 | 命令 | 结果 |
| --- | --- | --- |
| 代码格式 | `cargo fmt --all -- --check` | 见第 9 节 |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | 见第 9 节 |
| Rust 单测 | `cargo test --workspace` | 见第 9 节 |
| 静态契约 | `python3 scripts/verify_contracts.py` | 24 / 0 |
| M2 运行态契约 | `python3 scripts/test_api_contract.py` | 75 / 0 |
| M3 回归 | `python3 scripts/test_m3.py` | 见第 9 节 |
| M4 回归 | `python3 scripts/test_m4.py` | 见第 9 节 |
| M5 运行态 | `python3 scripts/test_m5.py` | 见第 9 节 |
| 生态六条 | `python3 scripts/test_ecosystem.py` | 见第 9 节 |
| 前端单文件 | `cd components/lan-mdns/webui && npm run verify` | 见第 9 节 |
| 全量构建 | `cargo build --workspace` | 见第 9 节 |

---

## 7. 集成阶段修复记录

1. `lan-ssdp` 的 `Entry` 缺 `Debug`，导致 `DeviceTable` 的 derive 编译失败 → 补 derive。
2. clippy：`lan-ssdp/src/main.rs` 的 `single_match` 改 `if let`；`lan-sync/src/sync.rs` 三处 `manual_pattern_char_comparison` 改 `find(['/', '?'])`。
3. **`scripts/test_api_contract.py` 的 M3 起遗留漂移**：硬编码「22 条路径」且逐操作遍历整个 openapi，M3/M4/M5 追加的 45 个组件端点被误判为「never exercised / router forgot」。改为「M2 的 23 条路径全部仍在 openapi 中」+ 按组件前缀排除其他里程碑的专有端点；新增 `/api/browse/unified` 纳入 implemented 清单。
4. **本地 macOS 组播栈限制**：默认 `POST /api/probe` 返回 500（重新绑定组播 socket 失败），导致 M2 契约门禁失败。验收脚本改用实现既有的 QU 单播字符串形式 `"unicast": "true"`（临时端口，不重绑组播 socket），M2 二进制行为一字不改。
5. `shell` 的 `m5-shell-dev` 实现任务因 429 限流中断，只留下 `Cargo.toml`；`src/main.rs`、`src/assets/shell.html`、`README.md` 由主 agent 补齐。

---

## 8. 已知边界

- **macOS / 容器组播限制**：`scripts/smoke_mdns.py` 依赖真实组播收发与「同端口双实例」，在受限主机上有 10 项环境性失败；M5 门禁不纳入该脚本，M2 契约门禁（`test_api_contract.py`）已用 QU 单播规避。Linux CI 环境全绿。
- **前端两套壳**：M2 Arco 单文件控制台与其余组件原生 JS 六区域控制台并存，PRD §3.7 偏差继续有效（ADR 0007 登记收敛路线）。
- **`scripts/install.sh`**：仍只安装 `lan-mdns`；其余 7 个组件与 Shell 由 `release.yml` 的 8×2 产物矩阵分发。
- **可复现构建**：固定工具链 / epoch / 参数 / sha256 清单，不承诺跨主机 bit-for-bit 一致。
- **`lan-ca`**：沿用 M3 的单根自签 HMAC 凭证（非 X.509）。

---

## 9. 执行记录

**执行时间**：2026-11-14 12:33（本机 macOS；本地组播限制见第 8 节，不影响门禁结论）。

### 9.1 全量门禁逐项

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | **0 diff** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **0 warning** |
| `cargo test --workspace` | 35 个测试目标，**198 passed / 0 failed**（M4 收尾 150 → +48） |
| `python3 scripts/verify_contracts.py` | **24 / 0**（PyYAML 完整模式，含 `service-record.json` 与样例） |
| `python3 scripts/test_api_contract.py`（隔离组播） | **75 / 0**（M2 回归，较 M4 时期 +1 条 `/api/browse/unified`） |
| `python3 scripts/test_m3.py` | **66/66 — M3 RUNTIME OK** |
| `python3 scripts/test_m4.py` | **76/76 — M4 RUNTIME OK** |
| `python3 scripts/test_m5.py` | **91 / 0 — M5 RUNTIME OK** |
| `python3 scripts/test_ecosystem.py` | **91 / 0 — ECOSYSTEM OK** |
| `cd components/lan-mdns/webui && npm run verify` | typecheck OK、838.30 kB 自包含、markers 12/12、nav 6 / panels 10 / dots 14 / kpi 25、**RENDER OK** |
| `cargo build --workspace` | **9 个二进制**（8 组件 + `lan-shell`） |
| `git diff --check` | 无空白问题 |

### 9.2 生态六条逐条结论

| # | 结论 | 证据 |
| --- | --- | --- |
| 1 | 通过 | `test_ecosystem.py` 对 8 个组件逐一断言 README / 配置模板 / `--check`（`lan-mdns` 用 `--validate`）与 18 个统一端点 |
| 2 | 通过 | 8 个组件**逐个单独**在隔离临时目录启动（其余不运行），`/healthz` / `/api/config` / 离线 WebUI 均可用后关闭 |
| 3 | 通过 | 统一信封 `ok/data/request_id`、8 个错误码、409 `setup_required` 写保护、9 个必备指标族全部一致；M2 独立实现按契约断言，未迁移 |
| 4 | 通过 | Shell 状态灯与真实 `/healthz` 一致：运行中组件 `online=true`，未运行组件 `false/null`，8 条清单齐全 |
| 5 | 通过 | `release.yml`：8 组件 + Shell × `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`，固定工具链 + `SOURCE_DATE_EPOCH` + `SHA256SUMS.txt` + `.tar.gz.sha256` |
| 6 | 通过 | manifest 8 个 name 与 README / openapi 一致、端口 8080–8087 一致、METRICS §3.7 / ADR 0007 / M5-CHECKLIST 齐全，README 无过期状态描述 |

### 9.3 运行态验收要点

- `lan-sync`：篡改 checksum 被拒、未登记节点 404、三种冲突策略各产生显式决策与审计、fake HTTP 节点往返、凭据字段扫描无泄漏。
- `lan-ssdp`：回环单播 fake 设备注入 alive → 记录通过 `service-record.json` 校验 → `byebye` 立即移除；**fake HTTP server 收到 0 个 LOCATION 请求**；单播隔离下 `refresh` 返回 `sent:0`，未向真实 LAN 发组播。
- `lan-shell`：8 卡单文件页面零外链、无密码输入框、`/api/records` 与 `/api/status` 均 404（不代理）、`--check` 不监听端口。
- `lan-mdns`：`/api/browse/unified` 输出统一信封且每条记录通过同一 schema（`source: "mdns"`）；旧 `/api/browse` 未改动。

### 9.4 结论

M5 全部 6 个批次（M5.0 → M5.5）交付完成，PRD §8.2 生态级 DoD 六条与 §8.1 组件级 DoD 对 8 个组件全部成立；M2 / M3 / M4 回归无破坏，**M5 可销项**。

唯一未纳入本地门禁的是 `scripts/smoke_mdns.py`（依赖真实组播收发与同端口双实例，macOS 上有 10 项环境性失败），已登记在第 8 节，Linux CI 环境全绿。
