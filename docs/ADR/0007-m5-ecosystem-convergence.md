# ADR 0007 · M5 生态收敛与统一出口

- 状态：**已接受并落地**（M5 完成，提交 `9a9c4de`）
- 日期：2026-11-14
- 依据：`docs/PRD.md` §2.2 / §2.3 / §3.7 / §4.8 / §4.9 / §7.5 / §8.2，`docs/M5-PLAN.md`
- 前置：ADR 0006（运行态契约门禁）、M4 提交 `2b5b259`

## 背景

M4 结束时 workspace 有 6 个 `lan-*` 组件，PRD §7.5 的 M5 要求补齐 P2 两组件并达成**生态级 DoD 六条**（§8.2）。最大的架构风险有三个：

1. `lan-mdns` 是 M2 独立实现（不走 `common-component`），若强行迁移会破坏 66/66 与 74 项运行态契约；
2. 「统一出口」容易被实现成反向代理 / 数据总线，违反 D3 / D10「组件之间不强耦合」；
3. 前端存在两套壳（M2 的 Vue 3 + Arco 与 M3/M4 的原生 JS 六区域控制台），M5 若强行统一会变成另一个里程碑。

## 决策

| 编号 | 决策 | 结论 |
| --- | --- | --- |
| D-M5-1 | Shell 形态 | 独立 Rust 二进制 `lan-shell`，**聚合层不是第 9 个组件**；只有 `GET /`、`GET /healthz`、`GET /api/shell/status` 三条路由 |
| D-M5-2 | 导航数据来源 | 静态 `shell/components.yaml`，不做服务发现、不读组件配置 |
| D-M5-3 | 鉴权 | 只透传 AdminToken 到目标页 URL，**不代管管理员密码**、不代理请求 |
| D-M5-4 | 前端统一 | **不统一**。Shell 用 Calm Console 原生单文件页，组件页保持现状；Arco 统一壳另立项 |
| D-M5-5 | `lan-sync` 边界 | 只调标准契约端点（`/api/config/snapshot`），先验 `sha256` checksum，冲突默认 `reject` 且每次决策留痕；凭据不进日志/快照/导出 |
| D-M5-6 | `lan-ssdp` 边界 | 只读 M-SEARCH / NOTIFY，**不抓 LOCATION 描述 XML、不调 SOAP / 控制接口**；运行态验收用回环 fake 设备 |
| D-M5-7 | 统一服务记录 | 新增 `contracts/service-record.json`；`lan-mdns` 只新增只读 `/api/browse/unified`，旧 `/api/browse` 一字不改 |
| D-M5-8 | 零偏差扫描口径 | 按**契约**断言（信封 / 错误码 / 鉴权 / 9 个指标族 / 快照 schema），不要求实现同源，不迁移 M2 |
| D-M5-9 | 可复现构建 | 固定 Rust 工具链 + `SOURCE_DATE_EPOCH` + 统一 cargo 参数 + `SHA256SUMS.txt`；**不承诺跨主机 bit-for-bit** |
| D-M5-10 | 组件总数 | 恒为 8；`lan-shell` 是第 9 个二进制但不是组件 |

## 关键实现约束

### 1. Shell 不能使用 `common_component::run`

`App::dispatch` 是「第一个匹配路由胜出」。`common_component::standard_routes()` 会把 `GET /` 绑到六区域组件控制台并注册 18 个统一端点，后注册的同路径路由无法覆盖。因此 Shell 用 `common_api::App` 手工构造 `Context`：

```rust
let ctx = Arc::new(Context {
    component: "lan-shell".into(),
    metrics: Registry::new("lan-shell"),
    health: Arc::new(Health::new()),
    admin_token: std::env::var("LAN_SHELL_TOKEN").unwrap_or_default(),
    config_raw: manifest_text,
    webui: true,
    events: None,
    password: None, // no route_write routes -> no password guard needed
    ..
});
App::new()
    .route("GET", "/", view_index)
    .route("GET", "/healthz", view_healthz)
    .route("GET", "/api/shell/status", view_status)
    .serve("127.0.0.1:8090", ctx)
```

`GET /api/records` 等组件专有端点由 `App::dispatch` 兜底返回 404，从结构上保证「不代理」。

### 2. 健康探测有界

并发上限 4（`std::thread::scope` 按 4 个一组分批）、单请求 `probe_timeout_ms`（夹到 50..5000ms）、间隔夹到 5..3600s；`probe_enabled=false` 时完全不发网络请求，所有 `online` 为 `null`。探测只访问 `127.0.0.1:<port>/healthz`，不读响应体。

### 3. 统一服务记录

`contracts/service-record.json` 定义 `{name,type,address,port,attributes,ttl,last_seen,source}`，`source` 枚举 `ssdp|mdns`。`lan-mdns` 的映射规则：

- `name`：观察记录的 owner，去尾部点、截断 253；
- `type`：PTR/SRV 用 owner（服务类型），其它用记录类型名；
- `address/port`：A/AAAA 直接取 IP；SRV RDATA `priority weight port target` 拆出两者；其余按主机名登记、port=0；
- `attributes`：`record` / `owner` / `value` 三个键，值有界；
- `ttl`：夹到 86400；`last_seen`：由观察时间戳转 RFC3339 UTC。

只读、**旧 `/api/browse` 响应形状不变**。

### 4. `lan-sync` 冲突语义

`reject` 冲突与 `manual` 待确认都是 **HTTP 200 + `decision` 字段**的显式决策，不是 409；只有未登记 404、checksum 错误 400、远端不可达 502 才是 HTTP 错误。pull/confirm 只做决策与审计，**不静默改写本地配置文件**。

## 已登记的偏差 / 遗留边界

| 项 | 现状 | 后续 |
| --- | --- | --- |
| 前端两套壳 | M2 Arco 单文件控制台 + M3/M4/M5 原生 JS 六区域控制台并存 | 另立项统一，不在 M5 范围；该立项已落档为 `docs/M6-PLAN.md`（能力清单驱动的前端聚合，待拍板） |
| `scripts/install.sh` | 仍只安装 `lan-mdns`（P0 主组件） | 其余 7 个组件与 Shell 由 `release.yml` 的 8×2 产物矩阵分发；install.sh 扩展另立项 |
| 可复现构建 | 固定工具链 / epoch / 参数 + sha256 清单 | 跨主机 bit-for-bit 一致性不作硬承诺 |
| SSDP 平台差异 | macOS / 容器网络下组播绑定可能受限 | API、统一模型、TTL 逻辑完整可用；运行态验收走回环单播 fake 设备 |
| `lan-ca` | 单根自签 HMAC 凭证，非 X.509 | 承接 M3 边界，不接入浏览器 TLS 信任链 |
| M2 契约门禁 | `test_api_contract.py` 原硬编码「22 条路径」 | 已改为「M2 的 23 条路径全部仍在 openapi 中」，并按组件前缀排除其他里程碑的专有路径 |

## 后果

- workspace 19 → **22 member**（新增 `components/lan-sync`、`components/lan-ssdp`、`shell`），仍零第三方 crate。
- 统一契约 18 端点对 8 个组件零偏差；Shell 不参与组件级指标扫描。
- `scripts/test_ecosystem.py` 把 §8.2 六条逐条自动化，`scripts/test_m5.py` 覆盖两个新组件与 Shell 的专有行为；CI 同时纳入 M2/M3/M4 回归。
- `docs/M5-CHECKLIST.md` 是 M5 唯一销项依据。
