# ADR 0003 · M2.2 可观测、安全与配置生命周期取舍

- 状态：已接受
- 日期：2026-11-13
- 影响范围：`common/crypto`、`common/auth`、`common/log`、`common/api`、`common/config`、
  `components/lan-mdns`、`contracts/config.schema.json`、CI

## 背景

M2.1 交付了真实 mDNS 引擎与协议内核，但 `/api/*` 只有只读端点：配置写盘后无法在线
变更，管理接口无写保护，日志只落到 stderr，可观测状态缺少健康 / 冲突 / 版本维度。
M2.2 需要销项 PRD 第 5 章 19 条（D / E / F 域为主），同时不破坏 M0 冻结契约与
ADR 0001 的「零第三方 crate」约束。

## 决策

### 1. 零依赖自实现 SHA-256 / HMAC / PBKDF2，不引入 `ring` / `sha2`

`common-crypto` 用纯标准库实现 SHA-256、HMAC-SHA256、PBKDF2-HMAC-SHA256，
以 FIPS 180-4 / RFC 4231 / RFC 6070 官方向量做单测。理由：

- 密码摘要与快照 checksum 只需要这三个原语；
- musl 静态交叉构建不希望引入 C 工具链依赖（`ring` 需要 `cc`）；
- 算法代码约 200 行，可审计、可离线单测。

代价：这是未经通用侧信道加固的实现。但 PBKDF2 迭代 200_000、常量时间比较、
逐安装随机盐、摘要文件 `0600`，已满足本场景（F-F05 默认仅 loopback 监听）的要求。

### 2. 管理员密码与 Admin Token 分层，写保护统一在 HTTP 层

- `AdminToken` 保护所有 `/api/*`（M1 已有）；
- `AdminPassword` 只保护**写路由**（`POST /api/config`、`/api/config/snapshot`、
  `/api/config/rollback`、`/api/reload`），由 `App::route_write()` 显式登记；
- `common-api` 在 `handle_conn` 里先做 token 校验，再对写路由做 `check_write()`，
  错误码 `409 setup_required` / `403 auth_failed` 遵循 `docs/ERROR-CODES.md`；
- `POST /api/auth/password` 本身**不是**写路由：首次设置必须可达，否则管理员会被
  锁死；修改已有密码则需 `current_password` 或 `X-Admin-Password` 头。

`PasswordGuard` trait 让 HTTP 层不依赖摘要格式；`PasswordManager` 用
`pbkdf2_sha256$iterations$salt$hash\n` 存于 `0600` 文件，tmp + rename 原子替换。
生产迭代数 200_000，测试用 `load_with_iterations(..., 600)` 避免 debug 构建耗时数十秒。

### 3. 快照自包含、checksum 绑定 config_text、ID 白名单防穿越

`ConfigStore` 严格按 `contracts/snapshot.json` 六字段读写，`checksum = sha256:<hex>`
覆盖 `config_text`。导入 / 回滚前必须先通过 `Snapshot::validate()`（形状 + component +
checksum + 离线配置校验）。快照 ID 用 `^config-[0-9TZa-f-]+\.json$` 白名单校验后才
拼路径，`../` 无法逃逸。

保存链路固定为「离线校验 → 写 `before-*` 快照 → 原子替换配置 → 更新内存原文 →
热重载」，任何一步失败都不落盘。历史按 mtime 保留 `history_limit` 个，且刚写入的
快照永不被自己的 `prune()` 删除（粗粒度时钟下防误删）。

### 4. 热重载只换声明式数据，不重建 socket

`apply_config_text()` 做 `Engine::reload()`（原子替换 RR 集合）+
`HealthMonitor::replace_targets()`（立即重新探测）+ `ConfigStore::set_raw()`。
组播地址 / 端口 / 网卡这类 socket 级参数变更仍需重启，在 F-A12 留待 M2.3 评估。
触发源三个：watchdog 轮询磁盘内容变化、`SIGHUP`、`POST /api/reload`；内容比较避免
自己刚原子写完后重复重载。

### 5. 日志：有界内存环 + 磁盘硬上界轮转 + SSE 有界长连接

- `LogBuffer` 是 `VecDeque<(u64, String)>` + `Condvar`，容量固定 512，同时作为
  `EventSource` 供 `/api/logs` 与 SSE 消费；
- `RotatingFile` 按 `max_bytes` 轮转，最多 `backup_count + 1` 个文件，磁盘上界 =
  `max_bytes × (backup_count + 1)`，与旧系统语义一致；
- SSE（`GET /api/logs/stream`）用 `SseSpec { replay, idle_timeout, max_duration,
  max_events }` 强约束：连接最长 30s、最多 200 个 data 帧，空闲发 `: keepalive`，
  客户端不主动断开也会被释放，避免线程泄漏。

### 6. 健康 / 冲突 / 服务地图都是「纯函数 + 有界快照」

`state.rs` 把 `detect_conflicts()` / `aggregate_browse()` 写成对引擎有界队列快照的
纯函数，带 TTL 老化；`HealthMonitor` 独占一个线程做 `TcpStream::connect_timeout`
探测（800ms 超时），`up` / `down` / `unknown` 是业务数据而非 API 错误码。这样每个
`/api/*` 读请求都廉价且一致，不阻塞收包循环。

### 7. `server.port` 收回冻结 schema，`LAN_STUDIO_PORT` 降级为测试钩子

ADR 0001 曾把 HTTP 端口完全交给环境变量。M2.2 在 `contracts/config.schema.json`
新增 `server.port`（1..65535，默认 8080），`server.listen` 默认 `127.0.0.1`。
`port_from_env(configured)` 以配置值为准，环境变量仅在显式设置时覆盖，供 CI /
冒烟隔离实例使用。`main.rs` 额外强制：非 loopback 监听必须配置 `admin_token`（F-F05）。

### 8. 指标族一次补齐到 13 个，`gateway_enabled` 先登记后实现

`declare_mdns_metrics()` 按 `docs/METRICS.md` 第 3 节登记全部 13 个专有族。
`gateway_enabled` 属 M2.3，但指标族必须在 M2.2 存在且恒为 0，保证 PRD 8.1
「第 3 节全部存在」在 M2.4 可一次核对。`path` 标签取路由模板，未知路径收敛为
`/unmatched`，避免无界基数。

## 后果

- 正面：写操作有明确安全边界；配置可在线演进且可一键回滚；全链路资源有硬上界；
  可观测维度一次性补齐，M2.3 只需接告警源与网关开关。
- 负面：零依赖加密实现需自行维护（已用官方向量覆盖）；socket 级配置不能热切换；
  SSE 每个连接占一个线程，靠 `max_duration` / `max_events` 兜底。
- 未决：与系统 mDNS 守护进程同机实测（F-C03，M2.3）；F-A12 的 socket 级热切换；
  告警队列与 Webhook 重试（F-G*，M2.3）。
