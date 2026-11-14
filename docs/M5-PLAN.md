# M5 落地方案：P2 两组件 + 统一出口 + 生态收敛

> 依据 `docs/PRD.md` §2.2 / §2.3 / §3.7 / §4.8 / §4.9 / §7.5 / §8.2 / §9（D3、D10）。
> 状态：**已按默认决策实施完成**（2026-11-14），全量门禁全绿、PRD §8.2 生态六条通过。入口条件 M4 已完成并提交（`2b5b259`）。本文是 M5 范围依据，逐条销项与实测数字见 `docs/M5-CHECKLIST.md`。

---

## 1. 范围与硬约束

| 项 | 内容 |
| --- | --- |
| 交付物 | `lan-sync`、`lan-ssdp` 两个独立二进制 + Lan Studio Shell 统一出口 + 全生态 DoD |
| 组件总数 | **8 个，不新增第 9 个组件**（PRD §2.2 / §7.5）；Shell 是可选聚合层，不计入组件 |
| 验收 | PRD §8.2 生态级 DoD **6 条逐条通过**，且 §8.1 组件级 DoD 对全部 8 个组件成立 |
| 明确不做 | 组件间事件总线（D10）、统一出口数据总线（D3）、组件运行时互相强依赖、P3/第 9 个组件 |

§8.2 六条的可验证定义（写进 `scripts/test_ecosystem.py`）：

1. 8 个组件各自跑通统一契约 18 端点 + 组件 README / 配置模板 / `--check`；
2. 每个组件**单独**在隔离临时目录启动（其余 7 个不运行），`/healthz`、`/api/config`、离线 WebUI 均可用；
3. 统一契约零偏差：8 个组件的响应信封、8 个错误码、鉴权矩阵、9 个必备指标族、告警规范事件、日志结构、快照 schema 全部一致；
4. Shell 能导航到所有**已运行**组件，且状态灯与真实 `/healthz` 一致；
5. 多平台发布产物附校验和，构建过程可复现（固定工具链 + 确定性构建参数）；
6. `contracts/` 与实现一致，文档无过期描述（版本号 / 里程碑 / 端口 / 组件清单）。

> 现状事实：`lan-mdns` 是 M2 独立实现（指标前缀 `lanstudio_mdns_*`），其余 5 个组件走 `common-component`；两者实现不同但契约一致（M2 的 `test_api_contract.py` 74 项已验证）。零偏差扫描按**契约断言**，不要求实现同源。

---

## 2. 需求拆解

### 2.1 `lan-sync`：多节点配置同步（PRD §4.8）

| # | 能力 | 可实现定义 | 验收 |
| --- | --- | --- | --- |
| S1 | 节点登记 | `node_id / endpoint / token / enabled / note`，HMAC-SHA256 共享密钥校验节点身份 | 未登记节点请求拒绝；token 不进日志 / 指标 / 快照 |
| S2 | 配置快照同步 | 只交换 §3.6 快照 JSON（`schema/component/created_at/reason/config_text/checksum`），先验 checksum 再落地 | 篡改 checksum 的快照被拒；快照不含凭据文件 |
| S3 | 显式冲突策略 | 默认 `reject`（本地已有更新快照时不静默覆盖）；可选 `last_write_wins` / `manual`，**每次决策留痕** | 冲突时产生审计项，策略与理由可查 |
| S4 | 同步审计 | 有界环形：time / direction / node / component / checksum / decision / reason | 条数有界、可查询、不含凭据 |
| S5 | 边界 | **只调用标准契约端点**（`/api/config/snapshot`、`/api/status`、`/healthz`），不调用任何组件专有端点 | 源码扫描 + 运行态断言 |

**不实现**：自动全量覆盖、组件专有配置解析、实时双向增量同步、事件总线。

### 2.2 `lan-ssdp`：SSDP / UPnP 设备发现（PRD §4.9）

| # | 能力 | 可实现定义 | 验收 |
| --- | --- | --- | --- |
| D1 | 发现 | UDP 组播 `239.255.255.250:1900`，发 `M-SEARCH` + 被动收 `NOTIFY`（alive / byebye） | **只发发现报文，绝不 GET/POST 设备控制 URL、不调 SOAP 接口** |
| D2 | 统一数据模型 | 新增 `contracts/service-record.json`，字段 `{name,type,address,port,attributes,ttl,last_seen,source}`；`lan-ssdp` 按它输出，`lan-mdns` 新增只读 `/api/browse/unified` 输出同结构 | 两个组件的记录都能通过该 schema 校验 |
| D3 | TTL 过期清理 | 后台线程按 `max_age` 扫描，超期移除、`ssdp:byebye` 立即移除；容量有界 | 单测时间可控（注入时钟），运行态断言 |
| D4 | 结果快照 | `/api/ssdp/snapshot` 自包含 JSON，内存与落盘有界 | 快照不含控制 URL、不含大文本 |
| D5 | 不主动控制 | 源码无 HTTP 客户端调用控制接口 | 源码扫描 + 本地 fake 设备断言 |

**不实现**：UPnP 控制点、端口映射（NAT-PMP/PCP）、设备描述 XML 抓取与渲染、SOAP 调用。

### 2.3 Lan Studio Shell：统一出口（PRD §2.3 / §3.7 / D3、D10）

| # | 能力 | 可实现定义 | 验收 |
| --- | --- | --- | --- |
| X1 | 聚合导航 | 一个内嵌单文件 HTML 页，8 张组件卡（图标 / 名称 / 状态灯 / 打开按钮 / 端口） | 8 张卡齐全，链接指向各组件根路径 |
| X2 | 组件发现 | 静态 `shell/components.yaml` manifest + 可选有界 `/healthz` 探测（并发上限、超时、失败标记 offline） | 不依赖 mDNS 自动发现，不与 `lan-mdns` 重叠 |
| X3 | 鉴权透传 | 只透传 `AdminToken`（`?token=`），**绝不代管 / 存储管理员密码**；密码在组件页输入 | 页面与日志中不出现密码；token 只留在当前浏览器会话 |
| X4 | 统一主题 | 沿用 M2 的 Calm Console 设计令牌（深浅双主题、圆角矩形彩色图标、语义色），零外部资源 | 离线断言：无 CDN / 外链 |
| X5 | 不接管内部路由 | 只做 `<a href>` 跳转，不做反向代理、不聚合组件数据 | 源码扫描无数据总线逻辑 |
| X6 | 独立部署 | 单独二进制 `lan-shell`，不依赖任何组件进程即可启动（组件不在线时显示 offline） | 单独启动 + `/healthz` + 页面可用 |

### 2.4 生态收敛

- `scripts/test_ecosystem.py`：§8.2 六条自动化，含 8 组件逐个隔离启动、零偏差扫描、文档一致性扫描；
- `.github/workflows/release.yml`：从「只发 `lan-mdns`」扩展为 8 组件 × Linux amd64/arm64，逐产物 `sha256` 校验和 + 清单；
- `README.md` / `docs/METRICS.md` / `docs/ALERT-SCHEMA.md` / `contracts/openapi.yaml` / `scripts/verify_contracts.py` 同步到 8 组件口径；
- 新增 ADR `0007-m5-ecosystem-convergence.md` 登记偏差（Arco 统一壳、可复现构建边界等）。

---

## 3. 决策清单（需确认，括号内为建议默认值）

| 编号 | 决策点 | 建议默认 | 影响 | 需谁确认 |
| --- | --- | --- | --- | --- |
| **D-M5-1** | 统一出口形态 | **独立 Rust 二进制 `lan-shell`**（聚合层，不计入 8 组件），复用 `common-api` + 内嵌单文件 HTML，端口 8090 | 纯静态页无法做有界健康探测与鉴权透传 | 产品 / 技术负责人 |
| **D-M5-2** | 组件发现机制 | 静态 manifest + 可选 `/healthz` 探测；**不做 mDNS 自动发现** | 避免与 `lan-mdns` 重叠，符合 D10 无总线 | 网络 |
| **D-M5-3** | 鉴权透传 | 只透传 `AdminToken`（`?token=`），不代管管理员密码 | 凭据不集中，避免单点泄漏 | 安全 |
| **D-M5-4** | 前端是否统一到 Arco 单文件壳 | **Shell 用 Calm Console 视觉，8 个组件页保持现状**（M3 偏差已在 PRD §3.7 登记）；彻底统一另立项 | 若选「统一」，需把 M2 Vue 控制台改造成通用壳并适配 7 个组件，工作量独立于 M5 | 前端 |
| **D-M5-5** | `lan-sync` 冲突策略 | 默认 `reject`（不静默覆盖）+ 审计留痕；`last_write_wins` / `manual` 需显式开启 | 直接决定 S3 验收 | 产品 |
| **D-M5-6** | 节点身份校验 | HMAC-SHA256 共享密钥（复用 `common-crypto` 思路，零第三方 crate） | 不用 X.509、不引入 TLS 依赖 | 安全 |
| **D-M5-7** | SSDP 统一模型对齐方式 | 新增 `contracts/service-record.json`；`lan-ssdp` 按它输出，`lan-mdns` **新增**只读 `/api/browse/unified`（旧 `/api/browse` 保持不变） | 满足「共享统一数据模型」且不破坏 M2 66/66 | 产品 / 前端 |
| **D-M5-8** | 零偏差扫描对 `lan-mdns` 的处理 | 保持 M2 独立实现，按契约断言扫描；**不迁移**到 `common-component` | 迁移会动已验收的 M2，回归风险高 | 技术负责人 |
| **D-M5-9** | 发布矩阵与可复现 | 8 组件 × Linux amd64/arm64 + `sha256` 清单；固定 Rust 工具链版本 + `SOURCE_DATE_EPOCH`，**不承诺跨机器 bit-for-bit** | 见第 10 节风险登记 | 运维 |
| **D-M5-10** | 端口与落盘 | `lan-sync` 8086、`lan-ssdp` 8087、`lan-shell` 8090；业务数据各写自身 JSON lines，路径禁 `..` | 与 8080~8085 不冲突 | 运维 |

> 全部按建议默认值执行 → 回复「按默认开工」；要改则指出编号与新取值。

---

## 4. 数据流

### 4.1 `lan-sync`

```
POST /api/sync/pull {node_id, component}
  → NodeStore 查登记节点（未登记 404）→ 校验 enabled / token
  → HTTP GET {endpoint}/api/config/snapshot?reason=sync   // 只调标准契约端点
  → 解析快照：schema==1 / component 匹配 / checksum == sha256(config_text)
  → 冲突判定：本地已有同 component 的较新快照？
        strategy=reject          → 409 invalid_config + 审计(decision=rejected, reason=conflict)
        strategy=last_write_wins → 落地 + 审计(decision=overwritten, checksum)
        strategy=manual          → 进待确认队列 + 审计(decision=pending)
  → POST /api/config/snapshot 导入（先验后写，走组件自身校验链）
POST /api/sync/push {node_id}
  → 导出本地快照 → POST {endpoint}/api/config/snapshot（带 token）
  → 对端返回统一信封 → 审计结果
```

### 4.2 `lan-ssdp`

```
启动 → UdpSocket 绑定 0.0.0.0:1900（可配端口），加入 239.255.255.250 组播
  → 周期性 M-SEARCH（间隔可配，有上界；空闲不空转）
  → 收 NOTIFY / M-SEARCH 应答 → 只解析 LOCATION / ST / USN / CACHE-CONTROL 头
  → 规范化为 ServiceRecord（不抓取 LOCATION 指向的描述 XML）
  → 写入有界 DeviceTable（按 USN 去重、last_seen / ttl 更新）
后台清理线程：now > last_seen + max_age 或 byebye → 移除 + expired_total++
GET /api/ssdp/devices → 按统一模型输出，可通过 service-record.json 校验
```

### 4.3 Shell

```
启动 → 读 shell/components.yaml（8 条静态记录：name/label/port/icon/theme）
  → 可选健康探测：并发上限 4、单请求 300ms 超时、每 15s 一轮（可关闭）
  → GET /api/shell/status 返回 {components:[{...,online,checked_at}], token_hint:false}
  → GET / 返回内嵌单文件 HTML（Calm Console，8 卡 + 主题切换 + manifest 表单）
  → 卡片点击 = <a href="http://host:port/?token=...">，不代理任何请求
```

---

## 5. 数据结构

### 5.1 配置（公共段复用 `contracts/config.schema.json`）

`components/lan-sync/config.template.yaml`：

```yaml
version: 1
component: lan-sync
server: { listen: "127.0.0.1", port: 8086, webui: true, admin_token: "", log_level: info }
observability: { metrics: true, alerts: true, history_limit: 30 }
sync:
  nodes_path: "lan-sync.nodes.json"
  history_path: "lan-sync.history.json"
  max_nodes: 64
  strategy: reject            # reject | last_write_wins | manual
  request_timeout_ms: 2000
  shared_secret: ""           # 空则禁用节点签名；非空时 HMAC-SHA256 校验
  history_limit: 200
  allowed_components: ["lan-mdns", "lan-relay", "lan-ca", "lan-scan",
                       "lan-hosts", "lan-wol", "lan-sync", "lan-ssdp"]
```

`components/lan-ssdp/config.template.yaml`：

```yaml
version: 1
component: lan-ssdp
server: { listen: "127.0.0.1", port: 8087, webui: true, admin_token: "", log_level: info }
observability: { metrics: true, alerts: true, history_limit: 30 }
ssdp:
  group: "239.255.255.250"
  port: 1900
  search_interval_seconds: 60
  max_age_seconds: 1800
  max_devices: 512
  socket_timeout_ms: 300
  results_path: "lan-ssdp.devices.json"
  fetch_descriptions: false   # 恒为 false：不抓 LOCATION 描述、不调控制接口
```

`shell/components.yaml`（Shell 专用，不是组件配置）：

```yaml
version: 1
probe_enabled: true
probe_interval_seconds: 15
probe_timeout_ms: 300
components:
  - { name: lan-mdns,  label: 名称解析与服务发现, port: 8080, icon: dns,     tone: blue }
  - { name: lan-relay, label: 跨网段中继,         port: 8081, icon: relay,   tone: cyan }
  - { name: lan-ca,    label: 内网凭证,           port: 8082, icon: cert,    tone: purple }
  - { name: lan-scan,  label: 资产探查,           port: 8083, icon: scan,    tone: teal }
  - { name: lan-hosts, label: 静态名字映射,       port: 8084, icon: hosts,   tone: indigo }
  - { name: lan-wol,   label: 网络唤醒,           port: 8085, icon: power,   tone: amber }
  - { name: lan-sync,  label: 配置同步,           port: 8086, icon: sync,    tone: green }
  - { name: lan-ssdp,  label: 设备发现,           port: 8087, icon: discover,tone: orange }
```

### 5.2 统一服务记录（新增 `contracts/service-record.json`）

```json
{
  "name": "living-room-speaker",
  "type": "urn:schemas-upnp-org:device:MediaRenderer:1",
  "address": "192.168.5.42",
  "port": 1900,
  "attributes": { "usn": "uuid:abc...", "server": "Linux/3.0 UPnP/1.0" },
  "ttl": 1800,
  "last_seen": "2026-11-14T02:10:00Z",
  "source": "ssdp"
}
```

`source` 固定枚举 `ssdp` / `mdns`；`attributes` 为扁平字符串映射，键白名单、值长度有界；禁止把 LOCATION / 控制 URL / 用户输入作为无界字段。`lan-mdns` 的 `/api/browse/unified` 把 DNS-SD 记录映射成同结构（`source: "mdns"`，`type` 用服务类型）。

### 5.3 API 契约增量（追加到 `contracts/openapi.yaml`）

| 组件 | 方法 | 路径 | 语义 |
| --- | --- | --- | --- |
| sync | GET | `/api/sync/nodes` / `/api/sync/status` / `/api/sync/history` | 节点清单 / 同步统计 / 有界审计 |
| sync | POST | `/api/sync/node` / `/api/sync/remove` / `/api/sync/pull` / `/api/sync/push` | 登记 / 删除 / 拉取 / 推送 |
| sync | POST | `/api/sync/confirm` | `manual` 策略下确认待处理项 |
| ssdp | GET | `/api/ssdp/devices` / `/api/ssdp/snapshot` / `/api/ssdp/status` | 统一模型清单 / 自包含快照 / 统计 |
| ssdp | POST | `/api/ssdp/refresh` | 触发一次有界 M-SEARCH |
| mdns | GET | `/api/browse/unified` | 统一服务记录（旧 `/api/browse` 不变） |
| shell | GET | `/api/shell/status` | 组件在线状态（不代管凭据） |

指标与告警登记（同步进 `docs/METRICS.md` §3.7 与 `docs/ALERT-SCHEMA.md` §2、`scripts/verify_contracts.py`）：

- 指标：`lanstudio_sync_nodes` / `_pull_total` / `_push_total` / `_conflicts_total` / `_audit_entries`；`lanstudio_ssdp_devices` / `_expired_total` / `_discovery_errors_total` / `_probe_total`；`lanstudio_shell_components_online`（标签均为固定枚举）。
- 告警 kind：**复用已登记的** `sync_conflict`、`ssdp_discovery_error`；新增 `sync_node_unreachable`(warning)、`shell_component_offline`(info)。

---

## 6. 前端视图

**Shell 首页**（D-M5-4 默认路线）：沿用 M2 Calm Console 令牌——固定实色侧栏、白底 1px 边框面板、沉稳靛蓝主色、8/12/16 间距；8 张组件卡沿用「**大图标在左（44px 圆角矩形、淡彩底）、两行文字在右并右对齐**」版式，左侧 3px 语义色条 + 右上状态灯。主题切换、健康探测开关在顶栏。点击卡片直接跳到组件自身 WebUI，不带任何代理参数（仅可选 `?token=`）。

**Shell manifest 表单**：横向表单、label 左对齐固定宽度 120px、两列网格（组件名 / 端口、label / 图标色调），备注跨双列；保存走内嵌 JS 的 `/api/shell/manifest` 写端点（受 AdminToken 保护）。

**`lan-sync` 页面**：节点登记两列网格（node_id / endpoint、token / 备注跨双列），同步审计表（time / direction / node / decision / checksum 前 8 位），冲突待确认队列用醒目 amber 卡片 + 确认/拒绝按钮。

**`lan-ssdp` 页面**：设备卡网格（统一模型字段），TTL 倒计时进度条，byebye 移除即时反映；详情只读，不展示控制 URL、不提供「调用」按钮。

所有图标内联 SVG，产物零外部资源（复用 `offline_console_has_no_external_resources` 类断言）。

---

## 7. 关键实现骨架（伪代码，确认后落地）

### 7.1 两个新组件照 M3/M4 的 `Hooks` 形状接入

```rust
const HOOKS: Hooks = Hooks {
    name: "lan-sync", version: env!("CARGO_PKG_VERSION"),
    default_config: include_str!("../config.template.yaml"),
    validate, reload, status, refresh_metrics, routes, start,
};
fn main() { common_component::run(HOOKS); }
```

统一端点由 `standard_routes(App::new())` 提供；组件只注册 `Hooks::routes` 里的专有端点，写端点用 `App::route_write`。

### 7.2 `lan-sync` 冲突决策（核心逻辑）

```rust
fn decide(local: Option<&Snapshot>, incoming: &Snapshot, strategy: Strategy)
    -> (Decision, &'static str)
{
    // 快照先过校验：checksum 必须是 config_text 的 sha256
    if !incoming.verify_checksum() { return (Decision::Rejected, "bad_checksum"); }
    match (local, strategy) {
        (Some(prev), Strategy::Reject) if prev.created_at >= incoming.created_at => {
            (Decision::Rejected, "conflict")
        }
        (_, Strategy::Manual) => (Decision::Pending, "awaiting_confirmation"),
        (_, Strategy::LastWriteWins) => (Decision::Overwritten, "explicit_override"),
        _ => (Decision::Accepted, "no_conflict"),
    }
}
// 每次决策都写有界审计：time / direction / node / component / checksum / decision / reason
// 请求体与审计均不含 token / 密码；shared_secret 只在 HMAC 计算时使用
```

### 7.3 `lan-ssdp` 只读发现与 TTL 清理（核心逻辑）

```rust
const M_SEARCH: &str = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n\
    MAN: \"ssdp:discover\"\r\nMX: 2\r\nST: ssdp:all\r\n\r\n";
// READ-ONLY DISCOVERY: 该模块没有 HTTP GET/POST 客户端，也没有 SOAP 调用。
// LOCATION 头只作为属性登记，绝不发起请求去抓描述 XML。

fn sweep(table: &mut DeviceTable, now: Instant, max_age: Duration) -> usize {
    let before = table.len();
    table.retain(|_, rec| now.duration_since(rec.seen_at) < max_age);
    before - table.len() // expired_total 累加
}
```

### 7.4 生态零偏差扫描（核心逻辑）

```python
# scripts/test_ecosystem.py：对每个组件用同一组契约断言
for name, port in COMPONENTS:                    # 8 个
    base = start_alone(name, port, tmp)          # 其余组件全部不运行
    assert GET /healthz == {"ok": True}
    assert GET /readyz.data.ready is True
    assert unified_envelope(GET /api/status, /api/config, /api/logs, /api/alerts)
    assert nine_metric_families(GET /api/metrics, short_name(name))
    assert write_guard_before_password() == 409 setup_required
    assert config_save_snapshot_rollback_import_reload()
    assert offline_webui_no_external_resources()
    assert component_readme_and_template_exist()
    stop(base)                                   # 单独部署证明
assert shell_navigates_to_all_running_components()
assert docs_and_contracts_are_consistent()       # 版本 / 里程碑 / 端口 / 组件清单
```

---

## 8. 实施分批

| 批次 | 交付 | 完成判据 |
| --- | --- | --- |
| **M5.0** 契约与骨架 | `Cargo.toml` +2 member + `lan-shell`、3 份配置/manifest、`contracts/service-record.json`、openapi +12 端点、METRICS §3.7、ALERT-SCHEMA +2 kind、verify_contracts 同步 | `cargo build --workspace` 通过；静态契约 21/0 |
| **M5.1** `lan-sync` | NodeStore、快照校验、冲突策略、HMAC 节点身份、有界审计、7 个端点、单测 | 单测覆盖篡改 checksum / 三种策略 / 未登记节点 / 审计有界 |
| **M5.2** `lan-ssdp` | 组播收发、统一模型、TTL 清理、有界快照、4 个端点、单测 | 单测覆盖 TTL / byebye / 容量 / 不抓控制 URL；`lan-mdns` 新增 `/api/browse/unified` 且不破坏 66/66 |
| **M5.3** 统一出口 | `lan-shell` 二进制、manifest、有界健康探测、Calm Console 单文件页、鉴权透传 | 8 卡齐全、离线零外链、单独启动可用、不代管密码 |
| **M5.4** 生态收敛 | `scripts/test_ecosystem.py`（§8.2 六条）、`scripts/test_m5.py`（组件专属）、CI 纳入全部门禁、`release.yml` 8×2 矩阵 + sha256、README/ADR 0007 同步 | 生态门禁全绿；M2/M3/M4 回归无破坏 |
| **M5.5** 全量门禁与销项 | `docs/M5-CHECKLIST.md`、全量门禁、提交 | 见第 9 节 |

---

## 9. 验收与门禁

`scripts/test_m5.py` 复用 `test_m3.py` / `test_m4.py` 的临时实例夹具：

- 两组件 `--check`、18 个统一端点、配置闭环、离线 WebUI、409 写保护；
- `lan-sync`：登记 → 篡改 checksum 拒绝 → 未登记节点 404 → 三种冲突策略逐项 → 审计有界 → token 不进日志/快照；
- `lan-ssdp`：本地 fake UDP 设备回 `NOTIFY` → 统一模型通过 schema → TTL 过期清理 → byebye 立即移除 → 容量上界 → 源码无控制接口调用 → `lan-mdns` `/api/browse/unified` 同 schema；
- Shell：8 卡 + 状态灯与 `/healthz` 一致 + 健康探测有界 + 无密码字段 + 不代理。

`scripts/test_ecosystem.py`：§8.2 六条逐条自动化（见 7.4）。

全量门禁：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/verify_contracts.py
python3 scripts/test_api_contract.py     # M2 回归
python3 scripts/test_m3.py               # M3 回归
python3 scripts/test_m4.py               # M4 回归
python3 scripts/test_m5.py
python3 scripts/test_ecosystem.py
cd components/lan-mdns/webui && npm run verify
cargo build --workspace
```

---

## 10. 边界与风险

- **不新增第 9 个组件**；`lan-shell` 是聚合层，不实现任何业务能力、不作为数据总线（D3 / D10）。
- **不改 M2 `lan-mdns` 的既有行为**：只新增只读 `/api/browse/unified`，旧 `/api/browse` 与 66/66 验收保持不变；零偏差扫描按契约断言，不迁移实现（D-M5-8）。
- **Arco 统一壳未收敛**（D-M5-4 默认）：M3/M4 的原生 JS 六区域控制台与 M2 的 Arco 控制台并存，PRD §3.7 偏差继续有效，在 ADR 0007 登记收敛路线。
- **可复现构建边界**（D-M5-9）：固定 Rust 工具链 + `SOURCE_DATE_EPOCH` + 统一构建参数，生成 `sha256` 清单；不同主机/工具链的 bit-for-bit 一致不作硬承诺，登记为已知边界。
- **SSDP 平台差异**：macOS / 容器网络下组播绑定可能受限（与 `lan-relay` 同类问题），API、统一模型与 TTL 逻辑仍完整可用；运行态验收用回环 fake 设备，**不向真实 LAN 发组播**。
- **凭据边界**：Shell 只透传 Token、不存密码；`lan-sync` 的 `shared_secret` / `token` 不进日志、指标、快照、导出与错误响应。
- workspace 保持**零第三方 crate**；新增二进制、`shell/components.yaml` 与所有内嵌页面均零外部资源。
- `scripts/install.sh` 若仍只发布 `lan-mdns`，需在 M5.4 明确登记为遗留边界或一并扩展（决策项，默认随 release 矩阵一起扩展）。
