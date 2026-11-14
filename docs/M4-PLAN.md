# M4 落地方案：`lan-hosts` / `lan-wol`

> 依据 `docs/PRD.md` §4.6 / §4.7 / §7.5 / §8.1、`contracts/` 与 M3 已交付的 `common-component` 运行时。
> 状态：**已实施完成**（2026-11-14，提交 `2b5b259`），全量门禁全绿。本文保留方案与决策依据；逐条销项与实测数字见 `docs/M4-CHECKLIST.md`。文中「归 M5」的 Arco 统一壳按 ADR 0007 D-M5-4 决定不统一、另立项，其余两项（P2 组件、统一出口）已由 M5 交付。

---

## 1. 范围与硬约束

| 项 | 内容 |
| --- | --- |
| 入口条件 | M3 已完成（`709879d`，全量门禁全绿） |
| 交付物 | `components/lan-hosts`、`components/lan-wol` 两个独立二进制 |
| M4 验收（PRD §7.5） | **与 `lan-mdns` 无功能重叠、无强耦合** |
| 明确不做 | 统一出口、P2（`lan-sync` / `lan-ssdp`）、第 9 个组件 |
| 组件级 DoD | PRD §8.1 的 8 项逐条通过 |

**"无重叠 / 无强耦合"的可验证定义**（写进 `test_m4.py` 断言）：

1. 两个新 crate 的 `Cargo.toml` 不依赖任何其他 `components/*` crate；
2. `lan-hosts` 不监听 5353、不加入 `224.0.0.251`、不构造 DNS 报文、不实现 HTTP 反向代理；
3. `lan-hosts` 配置中不出现 `mdns.` 命名空间，运行时不读写 `lan-mdns` 的配置 / 快照 / API；
4. `lan-wol` 不直接调用 `lan-scan` 的 API，关联只允许通过人工导入文件；
5. `/healthz` 恒 `{"ok":true}`，业务探测结果只走组件自有端点。

> 背景风险：`lan-mdns` 已有「内置网关静态 Host 路由」（`mdns.gateway.hosts`，见 README §告警与内置网关）。因此 `lan-hosts` 若做 DNS 应答或 Host 代理必然重叠。本方案把它收敛为**静态映射的单一数据源 + 渲染 / 发布**，不碰解析与转发。

---

## 2. 需求拆解

### 2.1 `lan-hosts`：统一名字映射与分发

| # | 能力 | 可实现定义 | 验收 |
| --- | --- | --- | --- |
| H1 | 映射登记 | `name ↔ ip` 记录，含备注 / 标签 / 启用开关，按 `name` UPSERT | 非法 IP / 非法域名 / 超上限拒绝 |
| H2 | 冲突检测 | 同名不同 IP、同 IP 多名、保留名（`.local`）按策略处理 | 冲突有结构化返回，不静默覆盖 |
| H3 | 批量导入 | 从标准 hosts 文本解析，注释与空行忽略，UPSERT 覆盖 | 导入后渲染结果与输入语义一致 |
| H4 | 渲染 | 输出标准 hosts 文本（`ip<TAB>name<TAB># comment`），可带生成头 | 排序稳定、无重复行、IPv4/IPv6 均可 |
| H5 | 发布 / 分发 | 渲染产物原子写到 `hosts.output_path`；同时提供只读拉取端点 | 原子替换、路径禁 `..`、失败不改旧文件 |
| H6 | 状态与快照 | 映射数 / 上次发布 / 校验结果，配置快照闭环走 `common-component` | 快照不含凭据 |

**不实现**：组播应答、DNS 服务器、HTTP 代理、向其他组件推送配置（M5 统一出口才做联动）。

### 2.2 `lan-wol`：网络唤醒与设备电源状态

| # | 能力 | 可实现定义 | 验收 |
| --- | --- | --- | --- |
| W1 | 设备登记 | `id / name / mac / ip / probe_port / broadcast / enabled / note` | MAC 格式、IP 合法性、数量上限校验 |
| W2 | 唤醒 | Magic Packet `6×0xFF + 16×MAC`，UDP 发往广播地址 | **目标必须来自已登记设备**；未登记返回 `404 not_found`，不接受任意 MAC |
| W3 | 状态探测 | 对登记 `ip:probe_port` 做只读 TCP connect | 连续阈值去抖；唤醒宽限期内不判 offline |
| W4 | 状态机 | `unknown → waking → online / offline / unreachable` | 抖动输入下不误判，有转换审计 |
| W5 | 冷却与限流 | 每设备 `cooldown_seconds` 内重复唤醒被拒并计数 | 不产生广播风暴 |
| W6 | 唤醒审计 | 有界环形记录（谁 / 何时 / 设备 / 结果），不含凭据 | 条数有界、可查询 |
| W7 | 与 `lan-scan` 关联 | 仅支持导入 `lan-scan` 结果 JSON 生成候选设备 | 无直接 API 调用、无共享内存 |

**不实现**：ICMP/ARP 探测（跨平台成本高）、远程关机、电源控制接口、设备控制指令。

---

## 3. 决策清单（需确认，括号内为建议默认值）

| 编号 | 决策点 | 建议默认 | 影响 | 需谁确认 |
| --- | --- | --- | --- | --- |
| **D-M4-1** | `lan-hosts`「分发」的形态 | 渲染文件 + 只读拉取 API + 原子发布到本机文件；**不做 DNS / 代理 / 跨组件推送** | 决定 H4/H5 是否成立，直接影响「无重叠」验收 | 产品 |
| **D-M4-2** | `.local` 名字是否允许登记 | **默认拒绝**（`allow_local_suffix: false`），`.local` 归 mDNS | 是「无功能重叠」最具体的落点 | 产品 / 网络 |
| **D-M4-3** | 是否把映射同步给 `lan-mdns` 网关 | **否**，只允许人工导出后导入 | 若选「是」会引入组件耦合，M4 验收不通过 | 产品 |
| **D-M4-4** | WOL 状态探测手段 | 只读 TCP connect + 成功/失败各 3 次去抖 + 唤醒宽限期；**不做 ICMP/ARP** | 决定 W3/W4 实现与「不误判」验收方式 | 网络 |
| **D-M4-5** | 广播发送默认开关与冷却 | `send_enabled: false`（显式开启）、每设备 30s 冷却、只认登记设备 | 安全边界；关闭状态下可完整测试其余逻辑 | 安全 |
| **D-M4-6** | 是否新建 `common/probe` | **新建，仅 `lan-wol` 依赖**，`lan-scan` 保持不动，M5 再统一收敛 | PRD §4.1 列了 `common-probe` 但仓库尚无；选「抽 M3 重构」会动已验收组件 | 技术负责人 |
| **D-M4-7** | M4 WebUI 技术路线 | **沿用 `common-component` 的原生 JS 六区域控制台**（M3 已在 PRD §3.7 登记偏差），Arco 统一壳归 M5 | 若选 Arco，需先把 M2 的 Vue 控制台抽成共享前端，工作量独立于 M4 | 前端 |
| **D-M4-8** | 端口与数据落盘 | 8084 / 8085；`lan-hosts.mappings.json` / `lan-hosts.out` / `lan-wol.devices.json` / `lan-wol.history.json`，路径禁 `..` | 与 8080~8083 不冲突；数据只写自身文件 | 运维 |
| **D-M4-9** | 与 `lan-scan` 的关联方式 | 仅**文件导入**候选设备，不调用其 API | 满足「可关联但无强耦合」 | 产品 |

> 若全部按建议默认值执行，回复「按默认开工」即可；若要改，指出编号与新取值。

---

## 4. 数据流

### 4.1 `lan-hosts`

```
配置 / API 写入
  → MappingStore::upsert(校验 name / ip / 后缀 / 上限 / 冲突策略)
  → 持久化 mappings.json（临时文件 + rename 原子替换）
  → 触发渲染：过滤 enabled=false → 按 (name, ip) 排序 → 生成 hosts 文本
  → POST /api/hosts/publish：
       校验 publish_enabled + output_path
       → 写 .tmp → fsync → rename 覆盖（失败保留旧文件 + 发 hosts_publish_failed）
  → GET /api/hosts/render?format=hosts|json 供外部拉取
```

配置保存仍统一走 `common-component::Runtime::save()`：`validate → snapshot → atomic replace → reload`。业务数据与配置分离，**业务数据不进配置快照**（避免快照体积膨胀，也避免导入配置覆盖映射）。

### 4.2 `lan-wol`

```
POST /api/wol/wake {device_id}
  → DeviceStore::get(device_id)  // 未登记 → 404，不解析请求体里的 mac/ip
  → 冷却表检查（device_id → last_wake，30s） // 命中 → 429 rate_limited
  → send_enabled 检查 // 关闭 → 409 invalid_config 并记审计（不静默成功）
  → build_magic_packet(mac) → UdpSocket::bind(0.0.0.0:0)
       set_broadcast(true) → send_to(packet, (broadcast, port))
  → state = waking; grace_until = now + grace_seconds; 写审计
后台探测循环（每 probe_interval_seconds）
  → 对每台 enabled 设备做只读 TCP connect（超时 probe_timeout_ms）
  → 成功/失败各自连续计数，达阈值才翻转状态；grace 期内不判 offline
  → 状态翻转写审计 + 发 wol_device_unreachable（仅 offline）
```

---

## 5. 数据结构

### 5.1 配置（公共段复用 `contracts/config.schema.json`，组件段由 `additionalProperties` 允许）

`components/lan-hosts/config.template.yaml`：

```yaml
version: 1
component: lan-hosts
server:
  listen: 127.0.0.1
  port: 8084
  webui: true
  admin_token: ""
  log_level: info
observability:
  metrics: true
  alerts: true
  history_limit: 30
hosts:
  allow_local_suffix: false      # D-M4-2
  conflict_policy: reject        # reject | upsert
  mappings_path: lan-hosts.mappings.json
  output_path: lan-hosts.out     # D-M4-8
  max_mappings: 2000
  publish_enabled: false         # 显式开启才写文件
  include_header: true
```

`components/lan-wol/config.template.yaml`：

```yaml
version: 1
component: lan-wol
server:
  listen: 127.0.0.1
  port: 8085
  webui: true
  admin_token: ""
  log_level: info
observability:
  metrics: true
  alerts: true
  history_limit: 30
wol:
  send_enabled: false            # D-M4-5
  broadcast: 255.255.255.255
  port: 9
  devices_path: lan-wol.devices.json
  history_path: lan-wol.history.json
  max_devices: 512
  cooldown_seconds: 30
  grace_seconds: 90
  probe_port_default: 22
  probe_timeout_ms: 300
  probe_interval_seconds: 15
  probe_success_threshold: 3
  probe_failure_threshold: 3
  history_limit: 200
```

### 5.2 业务记录（JSON lines，与 M3 `lan-scan` 保持同一形态，便于复用原子写与有界保留）

```json
{"name":"nas.lan","ip":"192.168.5.10","aliases":["storage.lan"],"comment":"NAS","tags":["storage"],"enabled":true,"updated_at":"2026-11-14T01:00:00Z"}
{"id":"dev-01","name":"工作站","mac":"AA:BB:CC:DD:EE:FF","ip":"192.168.5.20","probe_port":22,"broadcast":"255.255.255.255","enabled":true,"note":"三楼"}
```

字段约束：`name` 小写字母/数字/连字符/点，长度 ≤253；`ip` 走 `IpAddr::from_str`；`mac` 严格 6 组十六进制；所有路径禁止 `..`。

### 5.3 API 契约增量（追加到 `contracts/openapi.yaml`）

| 组件 | 方法 | 路径 | 语义 |
| --- | --- | --- | --- |
| hosts | GET | `/api/hosts/mappings` | 映射清单（含冲突计数） |
| hosts | POST | `/api/hosts/mapping` | UPSERT 单条 |
| hosts | POST | `/api/hosts/remove` | 按 name 删除 |
| hosts | POST | `/api/hosts/import` | 从 hosts 文本批量导入 |
| hosts | GET | `/api/hosts/render` | `?format=hosts\|json` 只读渲染 |
| hosts | POST | `/api/hosts/publish` | 渲染 → 原子发布 |
| hosts | GET | `/api/hosts/status` | 映射数 / 上次发布 / 校验结果 |
| wol | GET | `/api/wol/devices` | 设备清单 + 当前状态 |
| wol | POST | `/api/wol/device` | UPSERT 设备 |
| wol | POST | `/api/wol/remove` | 删除设备 |
| wol | POST | `/api/wol/wake` | 按 `device_id` 唤醒（写操作，走 `route_write`） |
| wol | POST | `/api/wol/probe` | 手动触发一次探测 |
| wol | GET | `/api/wol/status` | 状态分布 / 冷却 / 发送统计 |
| wol | GET | `/api/wol/history` | 有界唤醒与状态转换审计 |

所有响应沿用 `common_api::ok/err` 统一信封与 8 个既有错误码；写端点用 `App::route_write` 以复用 `409 setup_required` / `X-Admin-Password` 鉴权。

### 5.4 指标与告警登记

`docs/METRICS.md` 新增 §3.6（标签均为固定枚举，不含 IP / 域名 / MAC）：

| 组件 | 指标 | 类型 |
| --- | --- | --- |
| hosts | `lanstudio_hosts_mappings` / `lanstudio_hosts_conflicts_total` | gauge / counter |
| hosts | `lanstudio_hosts_published_total` / `lanstudio_hosts_publish_failed_total` / `lanstudio_hosts_render_bytes` | counter / counter / gauge |
| wol | `lanstudio_wol_devices` / `lanstudio_wol_device_state{state}` | gauge |
| wol | `lanstudio_wol_wake_sent_total` / `lanstudio_wol_wake_rejected_total{reason}` / `lanstudio_wol_probe_total` | counter |

`docs/ALERT-SCHEMA.md` 第 2 节新增 4 个 kind：`hosts_publish_failed`(warning)、`hosts_conflict_detected`(warning)、`wol_wake_sent`(info)、`wol_device_unreachable`(warning)，按第 5 节流程登记来源 / level / extra / 冷却。

---

## 6. 前端视图（D-M4-7 默认路线）

复用 `common/component/src/assets/console.html` 六区域骨架（状态总览 / 配置管理 / 可观测 / 日志 / 告警 / 版本历史），只增加组件专属子标签，与 M3 三组件保持同一交互语言：

- **`lan-hosts` — 映射管理**：左侧列表（name / IP / 状态灯），右侧**横向表单**；label 左对齐固定宽度 120px，字段两列网格（`name` / `ip`、`aliases` / `tags`），`comment` 跨双列；底部「导入 hosts 文本」文本域 + 「渲染预览」代码块 + 「发布」按钮（发布前显示 diff 摘要）。
- **`lan-hosts` — 发布状态**：最近发布时间 / 目标路径 / 字节数 / 失败原因，指标卡沿用 M2 的「大图标在左、两行文字右对齐、圆角矩形彩色图标」版式。
- **`lan-wol` — 设备登记**：两列网格（名称 / MAC、IP / 探测端口），广播地址与备注跨双列；设备卡片显示状态灯与「唤醒」按钮（未开启发送时按钮禁用并给出原因）。
- **`lan-wol` — 唤醒审计**：时间 / 设备 / 结果 / 原因四列表格，最多展示 `history_limit` 条。

主题令牌、圆角、间距沿用 M2 的 Calm Console；所有图标内联 SVG，产物零外部资源（`offline_console_has_no_external_resources` 类断言扩展到两个新组件）。

---

## 7. 关键实现骨架（伪代码，确认后落地）

### 7.1 组件注册：照 M3 `Hooks` 形状，业务代码不碰统一端点

```rust
const HOOKS: Hooks = Hooks {
    name: "lan-hosts",
    version: env!("CARGO_PKG_VERSION"),
    default_config: include_str!("../config.template.yaml"),
    validate,          // 校验 hosts.* 参数 + 路径 + 上限
    reload,            // 重读 Flat → 更新 Arc<RwLock<HostsConfig>>
    status,            // 返回 {"mappings":N,"last_publish":"...","conflicts":N}
    refresh_metrics,   // 登记 §5.4 指标
    routes,            // 只注册 /api/hosts/* 共 7 个端点
    start,             // 载入 mappings.json，启动发布/探测循环
};

fn routes(app: App) -> App {
    app.route("GET", "/api/hosts/mappings", view_mappings)
        .route_write("POST", "/api/hosts/mapping", view_upsert)
        .route_write("POST", "/api/hosts/publish", view_publish)
        // ...
}

fn main() { common_component::run(HOOKS); }
```

### 7.2 hosts 渲染与发布（核心逻辑）

```rust
fn render(store: &MappingStore, header: bool) -> String {
    let mut rows: Vec<_> = store.iter()
        .filter(|m| m.enabled)
        .flat_map(|m| std::iter::once((&m.name, &m.ip))
            .chain(m.aliases.iter().map(|a| (a, &m.ip))))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));
    rows.dedup();
    let mut out = String::new();
    if header { out.push_str("# generated by lan-hosts — do not edit\n"); }
    for (name, ip) in rows {
        out.push_str(&format!("{}\t{}\n", ip, name));   // 标准 hosts 行
    }
    out
}

fn publish(cfg: &HostsConfig, text: &str) -> Result<(), String> {
    if !cfg.publish_enabled { return Err("publish is disabled".into()); }
    if cfg.output_path.contains("..") { return Err("invalid output path".into()); }
    let tmp = format!("{}.tmp", cfg.output_path);
    fs::write(&tmp, text)?;                 // 写临时文件
    fs::rename(&tmp, &cfg.output_path)?;     // 原子替换；失败时旧文件完好
    Ok(())
}
```

### 7.3 Magic Packet 与冷却（核心逻辑）

```rust
fn magic_packet(mac: &[u8; 6]) -> [u8; 102] {
    let mut buf = [0u8; 102];
    buf[..6].fill(0xFF);
    for i in 0..16 { buf[6 + i * 6..12 + i * 6].copy_from_slice(mac); }
    buf
}

fn wake(device: &Device, cfg: &WolConfig, cooldown: &mut HashMap<String, Instant>)
    -> Result<(), ErrorCode>
{
    // 只接受登记表里的设备；请求体里的 mac/ip 一律忽略
    if !device.enabled { return Err(ErrorCode::InvalidConfig); }
    if cooldown.get(&device.id).is_some_and(|t| t.elapsed() < Duration::from_secs(cfg.cooldown_seconds)) {
        return Err(ErrorCode::RateLimited);
    }
    if !cfg.send_enabled { return Err(ErrorCode::InvalidConfig); }   // 不静默成功
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.send_to(&magic_packet(&device.mac), (device.broadcast, cfg.port))?;
    cooldown.insert(device.id.clone(), Instant::now());
    Ok(())
}
```

### 7.4 状态机去抖（避免误判）

```rust
fn observe(dev: &mut DeviceState, ok: bool, cfg: &WolConfig, now: Instant) {
    if now < dev.grace_until { return; }              // 唤醒宽限期内不判 offline
    if ok { dev.streak_ok += 1; dev.streak_fail = 0; }
    else  { dev.streak_fail += 1; dev.streak_ok = 0; }
    let next = if dev.streak_ok >= cfg.success_threshold { State::Online }
               else if dev.streak_fail >= cfg.failure_threshold { State::Offline }
               else { return };                        // 未达阈值：保持上次状态
    if next != dev.state { dev.state = next; audit_state_change(dev, next); }
}
```

---

## 8. 实施分批

| 批次 | 交付 | 完成判据 |
| --- | --- | --- |
| **M4.0** 契约与骨架 | `Cargo.toml` +2 member、`common/probe`（D-M4-6）、两份 `config.template.yaml`、openapi / METRICS §3.6 / ALERT-SCHEMA 4 个 kind、组件 README 骨架 | `cargo build --workspace` 通过；`verify_contracts.py` 21→同数全绿（新端点被纳入必备清单校验） |
| **M4.1** `lan-hosts` | MappingStore、UPSERT / 导入 / 渲染 / 原子发布 / 冲突检测 / 7 个端点 | 单测覆盖非法输入、冲突策略、原子替换失败保留旧文件 |
| **M4.2** `lan-wol` | DeviceStore、magic packet、冷却、TCP 只读探测、去抖状态机、审计 / 7 个端点 | 单测覆盖未登记拒绝、冷却、抖动不误判、宽限期 |
| **M4.3** 验收脚本与清单 | `scripts/test_m4.py`（零第三方依赖）、`docs/M4-CHECKLIST.md` | 运行态全绿，含第 1 节 5 条「无重叠 / 无耦合」断言 |
| **M4.4** 全量门禁与提交 | README 里程碑 / 目录 / 端口说明、`.github/workflows/ci.yml` 纳入新二进制与 `test_m4.py`、全量门禁 | 见第 9 节 |

### 9. 验收与门禁

`scripts/test_m4.py` 复用 `scripts/test_m3.py` 的临时实例夹具，用例组：

1. 两组件 `--check` 离线校验、`/healthz`、`/readyz`、18 个统一端点信封与鉴权矩阵；
2. 配置 save → snapshot → rollback → import → reload 闭环（两组件各一套）；
3. `lan-hosts`：导入 → UPSERT → 冲突拒绝 → 渲染稳定性 → 发布原子性 → 失败不改旧文件 → `.local` 拒绝 → 路径穿越拒绝；
4. `lan-wol`：登记 → 未登记唤醒 404 → 冷却 429 → 关闭发送时 409 → magic packet 102 字节断言（用本地 UDP 回环 socket 捕获，**不向真实 LAN 发广播**）→ 抖动状态机 → 宽限期 → 审计有界；
5. 无重叠 / 无耦合：依赖清单扫描、端口与组播断言、配置命名空间断言、跨组件文件写入断言。

全量门禁（与 M3 收尾一致）：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/verify_contracts.py
python3 scripts/test_m3.py          # 回归：M3 不得被破坏
python3 scripts/test_m4.py
cd components/lan-mdns/webui && npm run verify
cargo build --workspace
```

---

## 10. 边界与风险

- **不改 M3 已验收组件行为**（D-M4-6 选「新建 `common/probe`」即为此）；若改选「抽 `lan-scan` 复用」，必须重跑 `test_m3.py` 66 项并在 ADR 记录。
- **不动 M2 前端产物**（D-M4-7 默认路线下 `common/webui/src/assets/index.html` 不重建）。
- workspace 保持**零第三方 crate**；Magic Packet、hosts 解析、状态机全部标准库实现。
- 广播发送在 macOS / 容器网络下可能受限于定向广播权限，需在组件 README 标注「`broadcast` 可配为网段定向广播地址」。
- 业务数据（映射 / 设备 / 审计）**不进配置快照**，避免导入配置时覆盖运行态数据；备份由各自 JSON lines 文件承担。
- 统一出口、P2 组件、Arco 统一壳收敛全部留给 M5，M4 不提前实现（前两项已由 M5 交付；Arco 统一壳按 ADR 0007 D-M5-4 另立项）。
