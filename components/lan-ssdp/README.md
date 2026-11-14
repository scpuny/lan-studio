# lan-ssdp — 只读 SSDP / UPnP 设备发现（M5 · PRD §4.9）

`lan-ssdp` 是 Lan Studio 的独立二进制组件，负责**被动/主动发现局域网内的
SSDP（UPnP）设备**，并把结果规范化为 `contracts/service-record.json` 统一服务
记录。它复用 `common-component` 运行时（统一端点 / 配置闭环 / 指标 / 告警 /
鉴权）与 `common-net` 的组播地址工具，零第三方 crate。

默认监听 `127.0.0.1:8087`；SSDP 默认组播 `239.255.255.250:1900`。

---

## 1. 定位与红线：只读发现，绝不调用控制接口

> **READ-ONLY DISCOVERY**
> 本组件**没有 HTTP 客户端、没有 SOAP 调用、不抓设备描述 XML、不调用任何设备
> 控制接口**。`LOCATION` 头只作为一条有界属性登记，绝不发起 HTTP GET/POST，
> 也不解析/渲染描述文档与控制 URL。发出的报文只有 `M-SEARCH * HTTP/1.1` 发现
> 请求；收到的只有 `NOTIFY`（`ssdp:alive` / `ssdp:byebye`）与 M-SEARCH 应答。

其它边界：

- **统一数据模型**：所有输出严格对齐 `contracts/service-record.json`，`source`
  固定 `"ssdp"`，`attributes` 键白名单（`usn` / `server` / `st` / `cache-control` /
  `location`）、值长度有界，禁止把控制 URL / 用户输入作为无界字段。
- **TTL 过期清理**：后台低频线程按 `max_age_seconds` 清理超期设备；收到
  `ssdp:byebye` 立即按 USN 移除；表容量受 `max_devices` 硬上限约束。
- **有界落盘**：结果快照（`results_path`）用 `.tmp` + `fs::rename` 原子替换，
  内容自包含且长度有界，只写组件自身目录。
- **测试隔离**：`ssdp.group` 配成单播地址（如 `127.0.0.1`）时**不加入组播组、
  不向真实 LAN 发任何 M-SEARCH**，只接收发往回环端口的单播 UDP，供本地 fake
  设备注入验收（见第 5 节）。
- 所有路径配置禁止 `..`；`fetch_descriptions` **恒为 false**，配置为 `true`
  会被 `validate` 直接拒绝（红线不可关闭）。
- 后台收发线程与清理线程都**不持有全局状态锁做网络 IO**：`recv_from` /
  `send_to` 在锁外执行，只有更新设备表时才短暂加锁。

---

## 2. 配置字段（`ssdp.*`）

见 `config.template.yaml`。`server.*` / `observability.*` / `logging.*` /
`alerts.*` 沿用公共 schema，由 `common-component` 处理。

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `ssdp.group` | IPv4 字符串 | `239.255.255.250` | SSDP 组播地址；配成单播地址即进入测试隔离模式 |
| `ssdp.port` | u16 | `1900` | SSDP UDP 端口（1..65535） |
| `ssdp.search_interval_seconds` | u64 | `60` | 周期性 M-SEARCH 间隔（5..86400，有上界） |
| `ssdp.max_age_seconds` | u64 | `1800` | 设备 TTL / 过期窗口（10..86400） |
| `ssdp.max_devices` | usize | `512` | 设备表容量上限（1..4096），按 USN 去重 |
| `ssdp.socket_timeout_ms` | u64 | `300` | 单次 UDP 读超时（50..5000），也决定空闲轮询粒度 |
| `ssdp.results_path` | 路径 | `lan-ssdp.devices.json` | 有界结果快照落盘路径，禁 `..` |
| `ssdp.fetch_descriptions` | bool | `false` | 恒为 false；为 true 时 `validate` 拒绝启动 |

`validate` 校验：端口范围、`group` 可解析为合法 IPv4、间隔 ≥5s（≤86400）、
`max_age` 10..86400、`max_devices` 1..4096、`socket_timeout` 50..5000、
`results_path` 不含 `..`、以及 `fetch_descriptions == false`。

---

## 3. 端点（4 个）

所有响应沿用 `common_api::ok/err` 统一信封；错误码只取 8 个既有值
（此处仅可能用到 `invalid_config`）。**POST 用 `App::route_write` 注册**，受
`X-Admin-Password` 写操作鉴权保护。

| 方法 | 路径 | 写鉴权 | 响应 `data` |
| --- | --- | --- | --- |
| GET | `/api/ssdp/devices` | — | `{"items":[ServiceRecord...],"total":N}`，每条都能通过 `service-record.json` 校验 |
| GET | `/api/ssdp/snapshot` | — | `{"items":[...],"total":N,"max_devices":N,"generated_at":RFC3339,"fetch_descriptions":false}`，自包含、无控制 URL / 大文本 |
| GET | `/api/ssdp/status` | — | `{"devices":N,"expired":N,"errors":N,"probes":N,"max_age_seconds":N,"max_devices":N,"fetch_descriptions":false}` |
| POST | `/api/ssdp/refresh` | 是 | `{"started":true,"sent":N}`，触发一次有界 M-SEARCH；请求体可选 `{"st":"ssdp:all"}`（≤256 字符）指定搜索目标 |

说明：

- `refresh` 在**单播隔离模式下 `sent:0` 且仍返回 200**（不向 LAN 发组播）；
  只有真正的多播模式才会发出 1 个 M-SEARCH，`sent:1`。`st` 会被过滤为可打印
  ASCII（去掉 CR/LF/引号）并截断，无法注入额外的 SSDP 头。
- `errors` 为畸形应答计数（缺首行 / 缺 USN / 缺 NT·ST 等），同时累加
  `discovery_errors_total`；`expired` 为 TTL 过期 + `byebye` 移除总数。

统一端点（`/healthz`、`/readyz`、`/api/status`、`/api/config*`、
`/api/metrics`、`/api/logs`、`/api/alerts`、`/api/auth*` 等）由
`common-component` 提供；`Hooks::status` 的额外字段与 `/api/ssdp/status`
同形，合并进 `/api/status`。

---

## 4. 运维示例（curl）

启动后先完成**首次设置管理员密码**（`POST /api/auth/password` 是写鉴权的
引导入口，本身不是写路由）：

```bash
BASE=http://127.0.0.1:8087

# 首次设置管理员密码（若 server.admin_token 非空，读操作另需 X-Admin-Token）
curl -sS -X POST "$BASE/api/auth/password" \
  -H 'Content-Type: application/json' \
  -d '{"new_password":"change-me-now"}'

# 之后所有写操作带 X-Admin-Password
PW='X-Admin-Password: change-me-now'

curl -sS "$BASE/api/ssdp/devices"
curl -sS "$BASE/api/ssdp/snapshot"
curl -sS "$BASE/api/ssdp/status"

# 触发一次有界 M-SEARCH（不带 st 则默认 ssdp:all）
curl -sS -X POST "$BASE/api/ssdp/refresh" -H "$PW" \
  -H 'Content-Type: application/json' -d '{}'

# 针对特定服务类型刷新
curl -sS -X POST "$BASE/api/ssdp/refresh" -H "$PW" \
  -H 'Content-Type: application/json' \
  -d '{"st":"urn:schemas-upnp-org:device:MediaRenderer:1"}'

curl -sS "$BASE/api/metrics"
```

配置闭环（save → snapshot → rollback → import → reload）同样由运行时提供。

---

## 5. 本地隔离验收（不改真实 LAN 设置）

**不向真实 LAN 发组播**：把配置中的 `ssdp.group` 改为单播 `127.0.0.1`，
并把 `ssdp.port` 改成一个空闲高位端口（如 `41900`，避免与系统 SSDP 服务冲突），
保存后 `/api/reload` 或写到 `config.yaml` 后重启。此时组件：

- 只 `bind 0.0.0.0:41900` 接收单播 UDP，**不调用 `join_multicast_v4`**；
- 后台不发任何 M-SEARCH，`POST /api/ssdp/refresh` 返回 `sent:0`；
- 本地 fake 设备把 NOTIFY 发到 `127.0.0.1:41900` 即可被摄取。

注入一条 `ssdp:alive`：

```bash
python3 - <<'PY'
import socket
notify = (
    "NOTIFY * HTTP/1.1\r\n"
    "HOST: 239.255.255.250:1900\r\n"
    "NT: urn:schemas-upnp-org:device:MediaRenderer:1\r\n"
    "NTS: ssdp:alive\r\n"
    "USN: uuid:fake-speaker-1::urn:schemas-upnp-org:device:MediaRenderer:1\r\n"
    "LOCATION: http://127.0.0.1:41900/desc.xml\r\n"   # 只登记，组件绝不 GET
    "SERVER: Fake/1.0 UPnP/1.0\r\n"
    "CACHE-CONTROL: max-age=1800\r\n\r\n"
)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.sendto(notify.encode(), ("127.0.0.1", 41900))
PY
curl -sS http://127.0.0.1:8087/api/ssdp/devices
```

`byebye` 立即移除（把 `NTS` 改成 `ssdp:byebye`、保留同一 `USN` 再发一次）：

```bash
python3 - <<'PY'
import socket
bye = (
    "NOTIFY * HTTP/1.1\r\n"
    "NTS: ssdp:byebye\r\n"
    "USN: uuid:fake-speaker-1::urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n"
)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.sendto(bye.encode(), ("127.0.0.1", 41900))
PY
curl -sS http://127.0.0.1:8087/api/ssdp/status
```

要点：fake 设备发出的 `LOCATION` 只会出现在 `attributes.location` 里；组件进程
不会对该 URL 发起任何请求。把 `max_age_seconds` 调小（如 `10`）可快速验证 TTL
过期清理；连续注入超过 `max_devices` 个不同 USN 可验证容量上界。

仓库单测全部为纯离线逻辑（注入时钟、temp 目录唯一子目录并清理），
**不绑定真实组播、不发任何网络包**。

---

## 6. 统一数据模型（`contracts/service-record.json`）

`lan-ssdp` 与 `lan-mdns` 的 `/api/browse/unified` 共享同一模型，`source` 区分
来源。每条记录**恰好** 8 个属性（`additionalProperties: false`）：

| 字段 | 类型 / 约束 | lan-ssdp 取值 |
| --- | --- | --- |
| `name` | string 1..253 | 由 `USN` 派生并截断 |
| `type` | string 1..253 | `NT` / `ST`（搜索目标 / 设备类型） |
| `address` | string ≤64 | 应答源 IPv4 地址 |
| `port` | integer 0..65535 | 配置的 SSDP 端口（默认 1900） |
| `attributes` | object（扁平 string→string） | 键白名单 `usn` / `server` / `st` / `cache-control` / `location`，每个值截断到 256 字符 |
| `ttl` | integer 0..86400 | `max_age_seconds`，上限 86400 |
| `last_seen` | RFC3339，`Z` 结尾 | 收到报文时刻（无 chrono，手写 civil calendar） |
| `source` | 枚举 `ssdp` / `mdns` | 固定 `"ssdp"` |

`attributes` 用 `json_quote` 逐个转义，恶意 USN / SERVER / LOCATION 里夹带的
引号、反斜杠、控制字符都不会破坏 JSON 信封；`LOCATION` 与控制 URL 永不作为
无界字段或可执行内容输出。示例：

```json
{
  "name": "uuid:fake-speaker-1::urn:schemas-upnp-org:device:MediaRenderer:1",
  "type": "urn:schemas-upnp-org:device:MediaRenderer:1",
  "address": "127.0.0.1",
  "port": 41900,
  "attributes": {
    "usn": "uuid:fake-speaker-1::urn:schemas-upnp-org:device:MediaRenderer:1",
    "st": "urn:schemas-upnp-org:device:MediaRenderer:1",
    "server": "Fake/1.0 UPnP/1.0",
    "cache-control": "max-age=1800",
    "location": "http://127.0.0.1:41900/desc.xml"
  },
  "ttl": 1800,
  "last_seen": "2026-11-14T02:10:00Z",
  "source": "ssdp"
}
```

---

## 7. 指标（`docs/METRICS.md` §3.7）

短名由 `Registry::new("lan-ssdp")` 自动加 `lanstudio_ssdp_` 前缀；标签均为
固定枚举，不含 IP / USN / 设备名。

| 指标 | 类型 | 说明 |
| --- | --- | --- |
| `lanstudio_ssdp_devices` | gauge | 当前发现设备数（有界 `max_devices`） |
| `lanstudio_ssdp_expired_total` | counter | TTL 过期 + `byebye` 移除总数 |
| `lanstudio_ssdp_discovery_errors_total` | counter | 畸形应答解析失败数 |
| `lanstudio_ssdp_probe_total` | counter | 实际发出的 M-SEARCH 次数（含 `refresh`） |

`Hooks::refresh_metrics` 在每次 `/api/metrics` 展示前被调用，用 `set_counter`
发布绝对快照。

---

## 8. 告警 kind（`docs/ALERT-SCHEMA.md` §2）

| kind | level | 触发 |
| --- | --- | --- |
| `ssdp_discovery_error` | info | 畸形应答被忽略（首次及每 32 次节流告警）；或 UDP socket 绑定/加入组播失败 |

告警 `title` / `detail` 只含固定文案，不含报文内容、IP、USN 或凭据；平台适配
由 `common-alert` 统一完成。

---

## 9. 不做的部分

- **不做 UPnP 控制点**：不调 SOAP、不抓设备描述 XML、不渲染控制界面、不提供
  任何「调用」按钮。
- **不做端口映射**（NAT-PMP / PCP / UPnP IGD）、不做设备动作调用。
- **不做自动全量配置同步 / 组件事件总线**（M5 其它组件范畴），与其它组件无
  运行时强耦合。
- 单个 UDP 报文按 2048 字节上界读取，超长报文截断后按畸形处理，不做分片重组。
- macOS / 容器网络下组播绑定可能受限（与 `lan-relay` 同类问题）：此时
  API、统一模型与 TTL 逻辑仍完整可用，运行态验收走第 5 节回环 fake 设备，
  **不向真实 LAN 发组播**。
