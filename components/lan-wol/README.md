# lan-wol — 网络唤醒与设备电源状态（M4 · PRD §4.7）

`lan-wol` 是 Lan Studio 的独立二进制组件，负责**登记设备的 Wake-on-LAN 唤醒**
以及**基于只读 TCP connect 的电源状态探测与去抖**。它复用 `common-component`
运行时（统一端点 / 配置闭环 / 指标 / 告警 / 鉴权）与 `common-probe`（`TcpProbe`、
`Debounce`、`DeviceState`），零第三方 crate。

默认监听 `127.0.0.1:8085`；业务数据只写入自身目录，不触碰其他组件的配置文件。

---

## 1. 定位与安全边界

两条硬性安全边界，先读再改配置：

1. **唤醒目标必须来自已登记设备**。`/api/wol/wake` 只按 `device_id` 在登记表中查找，
   请求体里即使夹带 `mac` / `ip` 也**一律忽略**。未登记的 id 返回 `404 not_found`，
   组件绝不接受任意 MAC 构造的 Magic Packet，避免被当成广播反射/放大工具。
   设备只能通过 `POST /api/wol/device` 登记（该端点本身是写操作，受管理员鉴权保护）。
2. **`wol.send_enabled` 默认关闭**（模板为 `false`）。关闭时 `/api/wol/wake` 返回
   `409 invalid_config` 并计入 `wake_rejected_total{reason="disabled"}`，**不静默成功**；
   其余能力（登记、删除、手动探测、状态、历史、指标、配置闭环）在关闭状态下均可完整测试。
   只有显式把 `send_enabled` 置为 `true` 才会真正发出 UDP 包。

其它边界：

- 每台设备 `cooldown_seconds`（默认 30s）内重复唤醒被拒（`429 rate_limited`），抑制广播风暴。
- `broadcast` 允许配成 `127.0.0.1`（测试隔离）或网段定向广播地址；不强制必须是广播地址，
  也不向真实 LAN 发包的测试可以完全离线验证。
- 审计记录（`time / action / device_id / result / detail`）**永不含任何凭据**：`detail`
  只写固定 reason 或 `from->to` 状态标签；告警的 `title/detail` 同样只有设备 id 与固定文案。
- 所有路径配置禁止 `..`（路径穿越会被 `validate` 拒绝）；设备 / 审计 JSON lines 落盘使用
  `.tmp` + `fs::rename` 原子替换，写失败保留旧文件。

---

## 2. 配置字段（`wol.*`）

见 `config.template.yaml`。`server.*` / `observability.*` / `logging.*` / `alerts.*`
沿用公共 schema，由 `common-component` 处理。

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `wol.send_enabled` | bool | `false` | 唤醒总开关，默认关闭，需显式开启 |
| `wol.broadcast` | IPv4 字符串 | `255.255.255.255` | 全局广播/定向广播地址，可被设备级 `broadcast` 覆盖 |
| `wol.port` | u16 | `9` | Magic Packet UDP 目标端口（discard 端口） |
| `wol.devices_path` | 路径 | `lan-wol.devices.json` | 设备登记表 JSON lines，禁 `..` |
| `wol.history_path` | 路径 | `lan-wol.history.json` | 唤醒/状态审计 JSON lines，禁 `..` |
| `wol.max_devices` | usize | `512` | 设备数上限（≤4096），超限登记被拒 |
| `wol.cooldown_seconds` | u64 | `30` | 每设备唤醒冷却窗口 |
| `wol.grace_seconds` | u64 | `90` | 唤醒宽限期，期内不判 `offline` |
| `wol.probe_port_default` | u16 | `22` | 设备未指定 `probe_port` 时的默认探测端口 |
| `wol.probe_timeout_ms` | u64 | `300` | 单次 TCP connect 超时（50..5000） |
| `wol.probe_interval_seconds` | u64 | `15` | 后台低频探测间隔（≥5） |
| `wol.probe_success_threshold` | u32 | `3` | 连续成功几次判定 `online`（1..20） |
| `wol.probe_failure_threshold` | u32 | `3` | 连续失败几次判定 `offline`（1..20） |
| `wol.history_limit` | usize | `200` | 审计环形缓冲条数上限 |

`validate` 会校验端口范围、超时 50..5000、阈值 1..20、间隔 ≥5s、`max_devices` ≤4096、
冷却/宽限上界、所有路径禁 `..`、`broadcast` 可解析为 IPv4。后台探测循环按
`probe_interval_seconds` 对 `enabled` 设备做只读 TCP connect，**先克隆设备列表再释放锁**，
绝不持锁做网络 IO。

---

## 3. 端点（7 个）

所有响应沿用 `common_api::ok/err` 统一信封；错误码只取 8 个既有值
（`not_found` / `invalid_config` / `rate_limited` / `internal_error` 等）。
**所有 POST 均用 `App::route_write` 注册**，受 `X-Admin-Password` 写操作鉴权保护。

| 方法 | 路径 | 写鉴权 | 说明 |
| --- | --- | --- | --- |
| GET | `/api/wol/devices` | — | `{"items":[{...设备字段...,"state":"unknown\|waking\|online\|offline"}],"total":N}` |
| POST | `/api/wol/device` | 是 | 登记/UPSERT 设备：`{"id","name","mac","ip","probe_port"?,"broadcast"?,"enabled"?,"note"?}` → `{"id":...,"total":N}`；校验 MAC/IP/上限 |
| POST | `/api/wol/remove` | 是 | `{"id"}` → `{"removed":true\|false}` |
| POST | `/api/wol/wake` | 是 | `{"device_id":"..."}` 按登记表唤醒；成功 `{"sent":true,"bytes":102,"device_id":...,"state":"waking"}` |
| POST | `/api/wol/probe` | 是 | `{"device_id":"..."}` 一次只读 TCP 探测并喂给去抖 → `{"reachable":bool,"state":...,"transition":...\|null}` |
| GET | `/api/wol/status` | — | `{"devices":N,"states":{...},"sent":N,"rejected":N,"probes":N,"send_enabled":bool,"cooldown_seconds":N}` |
| GET | `/api/wol/history` | — | `{"items":[...],"total":N,"limit":N}` 有界审计 |

`wake` 返回码：未登记 `404 not_found`；`send_enabled=false` 或设备 `enabled=false`
`409 invalid_config`；冷却命中 `429 rate_limited`；UDP 发送失败 `500 internal_error`。
成功时置 `waking`、写审计、发 `wol_wake_sent` 告警。

状态机直接复用 `common_probe::Debounce`：连续阈值去抖、唤醒宽限期内不判 `offline`，
状态翻转写审计；宽限期后由 `waking` 翻到 `offline` 时发 `device_wake_failed` 告警。

---

## 4. 运维示例（curl）

启动后先完成**首次设置管理员密码**（`POST /api/auth/password` 本身不是写路由，
是写操作鉴权的引导入口）：

```bash
# 首次设置管理员密码（若 server.admin_token 非空，读操作另需 X-Admin-Token）
curl -sS -X POST http://127.0.0.1:8085/api/auth/password \
  -H 'Content-Type: application/json' \
  -d '{"new_password":"change-me-now"}'

# 之后所有写操作带 X-Admin-Password
PW='X-Admin-Password: change-me-now'

# 登记一台设备（probe_port 省略则用 wol.probe_port_default）
curl -sS -X POST http://127.0.0.1:8085/api/wol/device -H "$PW" \
  -H 'Content-Type: application/json' \
  -d '{"id":"dev-01","name":"nas","mac":"AA:BB:CC:DD:EE:FF","ip":"192.168.5.20","probe_port":22,"note":"机柜"}'

curl -sS http://127.0.0.1:8085/api/wol/devices
curl -sS http://127.0.0.1:8085/api/wol/status
curl -sS http://127.0.0.1:8085/api/wol/history

# 手动触发一次只读探测
curl -sS -X POST http://127.0.0.1:8085/api/wol/probe -H "$PW" \
  -H 'Content-Type: application/json' -d '{"device_id":"dev-01"}'

# 唤醒（默认 send_enabled=false 会返回 409；确认开启且目标可信后再调用）
curl -sS -X POST http://127.0.0.1:8085/api/wol/wake -H "$PW" \
  -H 'Content-Type: application/json' -d '{"device_id":"dev-01"}'

# 删除
curl -sS -X POST http://127.0.0.1:8085/api/wol/remove -H "$PW" \
  -H 'Content-Type: application/json' -d '{"id":"dev-01"}'
```

配置闭环（save → snapshot → rollback → import → reload）与 `/api/status`、
`/api/metrics`、`/api/logs`、`/api/alerts` 等统一端点由 `common-component` 提供。

---

## 5. 如何安全地验证唤醒（不向真实 LAN 发广播）

Magic Packet 是 `6×0xFF + 16×MAC` 的严格 102 字节。测试可用本地 UDP 回环 socket
捕获，**不触碰 LAN**：

1. 把 `wol.broadcast` 改为 `127.0.0.1`，把 `wol.port` 改为一个高位端口（如 `45999`），
   置 `wol.send_enabled: true`，保存并 `/api/reload`（或直接写到 `config.yaml` 后启动）。
2. 起一个回环 UDP 监听：

   ```bash
   python3 - <<'PY'
   import socket
   s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
   s.bind(("127.0.0.1", 45999))
   data, _ = s.recvfrom(2048)
   print("bytes=", len(data), "prefix=", data[:6].hex(), "tail=", data[-6:].hex())
   assert len(data) == 102 and data[:6] == b"\xff"*6
   PY
   ```

3. 调用 `/api/wol/wake`（目标设备需先登记，MAC 任意合法值即可），监听端应收到 102 字节、
   前 6 字节 `ffffffffffff`、后 6 字节等于该设备 MAC。
4. 仓库单测 `send_magic_targets_local_loopback_without_lan_broadcast` 即按此思路用
   `127.0.0.1` 回环 `UdpSocket` 验证，测试进程不会向真实网络发广播。

生产环境在 macOS / 容器网络下可能受定向广播权限限制，按网络实际把 `broadcast` 配成
网段定向广播地址（如 `192.168.5.255`）。

---

## 6. 指标（`docs/METRICS.md` §3.6）

短名由 `Registry::new("lan-wol")` 自动加 `lanstudio_wol_` 前缀；标签均为固定枚举，
不含 IP / MAC / 设备名。

| 指标 | 类型 | 标签 | 说明 |
| --- | --- | --- | --- |
| `lanstudio_wol_devices` | gauge | — | 已登记设备数 |
| `lanstudio_wol_device_state` | gauge | `state` | `unknown` / `waking` / `online` / `offline` |
| `lanstudio_wol_wake_sent_total` | counter | — | 实际发出的 Magic Packet 数 |
| `lanstudio_wol_wake_rejected_total` | counter | `reason` | `unregistered` / `cooldown` / `disabled` / `invalid` |
| `lanstudio_wol_probe_total` | counter | — | 只读 TCP 探测次数 |

`send_enabled=false` 时每次唤醒拒绝累加 `wake_rejected_total{reason="disabled"}`。

---

## 7. 告警 kind（`docs/ALERT-SCHEMA.md` §2）

| kind | level | 触发 |
| --- | --- | --- |
| `wol_wake_sent` | info | 已登记设备唤醒包发送成功 |
| `device_wake_failed` | warning | 唤醒宽限期结束后设备仍未上线（`waking → offline`） |

告警事件体不含凭据；平台适配由 `common-alert` 统一完成。

---

## 8. 与 `lan-scan` 的关系

**仅文件导入关联，无运行时耦合**：`lan-wol` 不调用 `lan-scan` 的 API、不共享内存、
不读写其配置/快照。若要从扫描结果生成候选设备，由运维人工导出 `lan-scan` 结果 JSON，
整理成 `id/name/mac/ip` 记录后通过 `POST /api/wol/device` 逐条登记（或整理成
`devices_path` 的 JSON lines）。两个组件的数据文件彼此独立。

---

## 9. 不做的部分

- 不做 ICMP / ARP 探测（跨平台成本高），状态探测只走只读 TCP connect。
- 不做远程关机、电源控制接口、设备控制指令。
- 不做跨组件配置推送 / 统一出口（M5 范畴）、不做 DNS 应答 / HTTP 代理。
- 业务数据（设备 / 审计）不进配置快照，避免导入配置时覆盖运行态数据。
- 不实现 `lan-mdns` 相关能力，保持 M4「无功能重叠、无强耦合」验收。
