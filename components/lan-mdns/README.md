# lan-mdns（M2.2 可观测 + 安全 + 配置生命周期）

> 内网 `.local` 名字解析与 DNS-SD 服务发现组件。
> **M2.2 已完成**：在真实 mDNS 引擎之上补齐管理员密码写保护、配置保存 / 快照 /
> 历史 / 回滚 / 导入导出 / 热重载、有界结构化日志与磁盘轮转、SSE 实时日志流、
> TCP 健康目标、名称冲突聚合、服务地图 TTL 老化，以及 13 个专有指标族。
> 后续批次见 [`docs/M2-CHECKLIST.md`](../../docs/M2-CHECKLIST.md)：M2.3 告警 / 网关 /
> 部署运维，M2.4 Arco 正式管理台与全量销项。

## 构建与运行

```bash
cargo build --release -p lan-mdns
LAN_STUDIO_CONFIG=components/lan-mdns/config.template.yaml \
  ./target/release/lan-mdns
```

CLI 模式（不监听端口，离线校验配置并输出构建信息）：

```bash
./target/release/lan-mdns --cli
```

内建配置模板 `config.template.yaml` 发布 3 条静态记录（A / AAAA / TXT）、1 个
`_http._tcp.local` 服务实例、IPv4 反向 PTR 与服务类型枚举，共编译为 8 条 RR。
`mdns.health_targets` 默认为空，避免开箱即产生 LAN 探测流量。

## 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_STUDIO_CONFIG` | `config.yaml` | 配置文件路径；不存在时使用编译期内嵌模板 |
| `LAN_STUDIO_PORT` | `server.port`（8080） | 测试 / CI 端口覆盖钩子，生产以 YAML 为准 |
| `LAN_STUDIO_AUTH_FILE` | `auth.toml` | 管理员密码摘要，文件权限 `0600` |
| `LAN_STUDIO_LOG_DIR` | `logs` | 轮转日志目录，磁盘硬上界 = `max_bytes × (backup_count + 1)` |
| `LAN_STUDIO_HISTORY_DIR` | `config_history` | 配置快照目录，按 `observability.history_limit` 有界保留 |
| `LAN_MDNS_GROUP` | 配置文件 `mdns.group` | 仅测试隔离用：覆盖组播地址，必须是组播地址 |
| `LAN_MDNS_PORT` | 配置文件 `mdns.port` | 仅测试隔离用：覆盖 UDP 端口 |
| `LAN_MDNS_AGING_TTL_S` | `300` | 观测 / 审计队列的老化窗口 |
| `GIT_COMMIT` | `dev` | 编译期注入，进入 `build_info` 指标 |

两个 `LAN_MDNS_*` 覆盖只在运行时引擎生效，`--cli` 始终报告配置文件自身的值。
CI / 本地冒烟用它们把流量限制在 `239.x` 隔离组 + 高位端口，绝不向真实 LAN 发包；
并为每个实例分配独立临时数据目录，互不共享状态、不写仓库。

配置结构遵循 `contracts/config.schema.json`；公共段由 `common-config` 校验，
组件 `mdns:` 命名空间由本组件的 `config.rs` 解析并编译为 RR。

## 已实现端点

### 基础与协议

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 内嵌离线 WebUI |
| GET | `/healthz` | 存活探针，恒 `{"ok":true}` |
| GET | `/readyz` | 就绪探针（引擎已启动才 ready） |
| GET | `/api/status` | 协议状态、引擎计数、安全 / 历史 / 健康 / 冲突 / 日志状态位 |
| GET | `/api/metrics` | Prometheus 0.0.4（9 必备族 + 13 个 mdns 专有族） |
| GET | `/api/records` | 当前发布的 RR 集合（含 TTL / cache-flush 位） |
| GET | `/api/interfaces` | 本机 IPv4 网卡枚举 |
| POST | `/api/probe` | 主动探针：`{"name","type","unicast"}` |

### 可观测

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/queries` | 有界查询审计（`?limit=1..1000`、`?export=csv`） |
| GET | `/api/browse` | 外来应答 + 按服务类型聚合的地图（TTL 老化） |
| GET | `/api/conflicts` | 名称冲突（同名异值、来源登记） |
| GET | `/api/health/targets` | 业务健康目标 `up/down/unknown` 明细与计数 |
| GET | `/api/logs` | 有界结构化日志（`?limit=`）+ 磁盘上界 |
| GET | `/api/logs/stream` | SSE 实时日志流（replay + keepalive，长连接有界） |
| GET | `/api/alerts` | 告警状态占位（M2.3 接入真实分发器） |

### 配置生命周期（写操作需 `X-Admin-Password`）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/config` | 当前配置原文 + 结构化摘要 |
| POST | `/api/config` | 保存：`{"text":"<yaml>"}`，校验 → 快照 → 原子替换 → 热重载 |
| GET | `/api/config/snapshot` | 导出当前配置快照；`?id=` 读取指定历史快照 |
| POST | `/api/config/snapshot` | 导入快照（校验 checksum + 离线配置） |
| GET | `/api/config/history` | 有界版本列表（id / 时间 / 原因 / checksum / 大小） |
| POST | `/api/config/rollback` | 一键回滚：`{"id":"config-...json"}` |
| POST | `/api/reload` | 手动热重载磁盘配置 |

### 安全

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/auth` | 鉴权状态（token / password / write_protection） |
| POST | `/api/auth/password` | 首次设置或修改管理员密码 |
| POST | `/api/auth/verify` | 校验管理员密码 |

`/api/*` 受 `common-auth` 保护：`admin_token` 为空时仅允许回环访问，非空时校验
`X-Admin-Token` 或 `?token=`。写路由额外要求 `X-Admin-Password`：未初始化返回
`409 setup_required`，缺失或错误返回 `403 auth_failed`。`/healthz`、`/readyz` 不受鉴权。

响应统一包装 `{"ok":true,"data":...,"request_id":...}`，错误码见
[`docs/ERROR-CODES.md`](../../docs/ERROR-CODES.md)。

## 实现位置

| 层 | crate / 文件 | 职责 |
| --- | --- | --- |
| 报文编解码 | `common/dns` | RFC 1035 / 6762 子集：A/AAAA/PTR/SRV/TXT、名称压缩、QU / cache-flush、Known-Answer 抑制、Goodbye |
| 组播传输 | `common/net` | raw fd 预置 `SO_REUSEADDR`/`SO_REUSEPORT` 后 bind、加入组播、网卡枚举 |
| 配置内核 | `common/config` | 列表型 YAML 拍平、类型化 getter、离线校验、`server.port` |
| 加密原语 | `common/crypto` | 零依赖 SHA-256 / HMAC-SHA256 / PBKDF2-HMAC-SHA256、常量时间比较 |
| 鉴权 | `common/auth` | Admin Token、管理员密码摘要、`check_write()` |
| 日志 | `common/log` | 结构化记录、磁盘轮转、有界 `LogBuffer`、`EventSource` |
| HTTP | `common/api` | 路由 / 统一信封 / SSE 流式响应 / 写保护 |
| 引擎 | `src/engine.rs` | 收包循环、应答、摄取、宣告、探针、有界队列、统计、热重载 RR |
| 记录编译 | `src/config.rs` | `mdns:` 命名空间 → RR 集合、健康目标解析 |
| 运行态 | `src/state.rs` | 健康监控、冲突检测、服务地图聚合 |
| 快照 | `src/store.rs` | 配置保存 / 快照 / 历史 / 回滚 / 原子替换 / ID 白名单 |
| 组装 | `src/main.rs` | 生命周期、路由登记、指标、信号处理、优雅退出 |

退出时（`SIGINT` / `SIGTERM`）通过信号标志 + 守护线程广播 TTL=0 的 Goodbye，
信号处理器内只做无锁原子写，保证 async-signal-safe。

## 运行态验收

```bash
python3 scripts/smoke_mdns.py
```

160 项断言，覆盖：存活 / 就绪、8 条 RR、9+13 指标族、A / PTR / QU 三种真实组播探针
闭环、409 → 403 → 200 写保护全链路、密码摘要 `0600`、配置保存 / 导出 / 导入 /
历史 / 回滚 / 热重载、路径穿越与 checksum 篡改拒绝、结构化日志与磁盘上界、
SSE replay + keepalive、健康目标 up/down、优雅退出与 Admin Token 回归。

## 明确不做（M2.2 之后）

- 告警 Webhook、健康翻转 / 冲突触发、冷却重试（M2.3）
- 内置 HTTP 网关、全局与每服务代理开关、友好页（M2.3）
- Docker / compose / systemd / install.sh、首次启动模板生成（M2.3）
- 与 avahi / systemd-resolved 同机共存实测（M2.3）
- Arco Design 正式管理台、健康状态灯、在线配置表单（M2.4，当前为编译期内嵌占位页）
