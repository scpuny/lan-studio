# lan-mdns（M2.4 离线管理台）

> 内网 `.local` 名字解析与 DNS-SD 服务发现组件。
> **M2.4 已完成**：管理台由占位页替换为 Vue 3 + TypeScript（strict）+ Arco Design
> 的六区域控制台，打包成单个自包含 HTML 编入二进制——状态总览（含健康状态灯与服务
> 导航）、配置管理（表单 + 源码双模式、网卡绑定）、可观测、日志（SSE）、告警设置、
> 版本历史（回滚 / 导入导出）。66 条功能全集已销项 65 条，仅剩 `F-J03` API 契约
> 运行态测试，见 [`docs/M2-CHECKLIST.md`](../../docs/M2-CHECKLIST.md)。

## 构建与运行

```bash
cargo build --release -p lan-mdns
LAN_STUDIO_CONFIG=components/lan-mdns/config.template.yaml \
  ./target/release/lan-mdns
```

| 子命令 | 作用 |
| --- | --- |
| `--version` / `-V` | 打印版本、commit 与目标平台 |
| `--print-config` | 输出编译期内嵌 YAML 模板（供安装脚本 / 容器初始化） |
| `--validate` | 离线校验配置，**不启动网络、不监听端口** |
| `--cli` | CLI 模式：打印配置摘要后退出，不监听控制台端口 |
| `--healthcheck` | 探本机 `/healthz`，成功 0 / 失败 1，供 Docker / systemd 使用 |
| （无参数） | `webui` 模式，正常启动内嵌控制台与全部服务 |

配置文件不存在时会从内嵌模板**生成一次**；重启与升级永不覆盖用户配置（F-E09）。

内建配置模板发布 3 条静态记录（A / AAAA / TXT）、1 个 `_http._tcp.local` 服务实例、
IPv4 反向 PTR 与服务类型枚举，共编译为 8 条 RR。`mdns.health_targets` 默认为空，
`mdns.gateway.enabled` 默认为 `false`，避免开箱即产生 LAN 探测流量或抢占 80 端口。

## 配置要点

公共段（`server` / `observability` / `logging` / `alerts`）遵循
`contracts/config.schema.json`，组件段在 `mdns:` 命名空间由 `src/config.rs` 解析。

```yaml
alerts:                            # 告警编排（common-alert）
  enabled: true
  webhooks:                        # 也支持内联 ["url"] 写法
    - "https://oapi.dingtalk.com/robot/send?access_token=..."
  cooldown_seconds: 300            # 逐 kind 可被登记表覆盖
  timeout_seconds: 5.0
  max_retries: 2                   # 上限 2

mdns:
  health_targets:                  # TCP 业务健康目标，翻转时发告警
    - alias: nas
      host: 192.168.5.10
      port: 80
  services:
    - instance: Web Server
      type: _http._tcp.local
      host: nas.local
      port: 8080
      proxy: true                  # F-H02 每服务网关开关
  gateway:
    enabled: false                 # F-H02 全局开关；关闭时零监听
    port: 80
    listen: "0.0.0.0"
    admin_host: "mdns.local"       # F-H03 管理台统一域名入口
    timeout_seconds: 6.0
    hosts:                         # 显式路由，同名覆盖服务派生值
      - host: printer.local
        target: "192.168.5.30:80"
        proxy: true
```

平台适配：钉钉 / 飞书 / Slack / 企业微信由 Webhook 域名自动识别，其余按通用 JSON
发送规范事件（`contracts/alert-event.json`）。`http://` 由标准库直接 POST；
`https://` 通过系统 `curl` 桥接。事件体经凭据关键字扫描，不会带出令牌或密码。

网关路由**只**来自「已发布服务 + 自身 A 记录」与 `gateway.hosts[]`；未登记 404、
目标不可达 502，均返回转义后的友好 HTML 页。请求头原样转发，请求体用 `io::copy`
双向流式透传，不缓存 / 不解析 / 不记录（PRD 1.4 红线）。

## 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_STUDIO_CONFIG` | `config.yaml` | 配置文件路径；不存在时生成内嵌模板 |
| `LAN_STUDIO_PORT` | `server.port`（8080） | 测试 / CI 端口覆盖钩子，生产以 YAML 为准 |
| `LAN_STUDIO_AUTH_FILE` | `auth.toml` | 管理员密码摘要，文件权限 `0600` |
| `LAN_STUDIO_LOG_DIR` | `logs` | 轮转日志目录，磁盘硬上界 = `max_bytes × (backup_count + 1)` |
| `LAN_STUDIO_HISTORY_DIR` | `config_history` | 配置快照目录，按 `observability.history_limit` 有界保留 |
| `LAN_MDNS_GROUP` | 配置文件 `mdns.group` | 仅测试隔离用：覆盖组播地址，必须是组播地址 |
| `LAN_MDNS_PORT` | 配置文件 `mdns.port` | 仅测试隔离用：覆盖 UDP 端口 |
| `LAN_MDNS_GATEWAY_PORT` | 配置文件 `mdns.gateway.port` | 仅测试隔离用：覆盖网关监听端口 |
| `LAN_MDNS_AGING_TTL_S` | `300` | 观测 / 审计队列的老化窗口 |
| `GIT_COMMIT` | `dev` | 编译期注入，进入 `build_info` 指标 |

CI / 本地冒烟用隔离钩子把流量限制在 `239.x` 组播组与高位端口，绝不向真实 LAN
发包；每个实例分配独立临时数据目录，互不共享状态、不写仓库。

## 已实现端点

### 基础与协议

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 内嵌离线 WebUI |
| GET | `/healthz` | 存活探针，恒 `{"ok":true}` |
| GET | `/readyz` | 就绪探针（引擎已启动才 ready） |
| GET | `/api/status` | 协议状态、引擎计数、安全 / 历史 / 健康 / 冲突 / 告警 / 网关 / 日志状态位 |
| GET | `/api/metrics` | Prometheus 0.0.4（9 必备族 + 16 个 mdns 专有族） |
| GET | `/api/records` | 当前发布的 RR 集合（含 TTL / cache-flush 位） |
| GET | `/api/interfaces` | 本机 IPv4 网卡枚举 |
| POST | `/api/probe` | 主动探针：`{"name","type","unicast"}` |

### 可观测与告警

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/queries` | 有界查询审计（`?limit=1..1000`、`?export=csv`） |
| GET | `/api/browse` | 外来应答 + 按服务类型聚合的地图（TTL 老化） |
| GET | `/api/conflicts` | 名称冲突（同名异值、来源登记） |
| GET | `/api/health/targets` | 业务健康目标 `up/down/unknown` 明细与计数 |
| GET | `/api/logs` | 有界结构化日志（`?limit=`）+ 磁盘上界 |
| GET | `/api/logs/stream` | SSE 实时日志流（replay + keepalive，长连接有界） |
| GET | `/api/alerts` | 分发器状态：开关 / Webhook 数 / 队列 / sent / failed / dropped / suppressed / retries / 按 kind 计数 / 网关统计 |

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

### 网关（独立监听，仅 `gateway.enabled=true` 时存在）

| Host / 路径 | 行为 |
| --- | --- |
| `Host: <admin_host>` | 转发到本机管理台端口 |
| `Host: <已登记且 proxy=true>` | 静态转发到 `target` |
| `Host: <未登记 / proxy=false>` | 404 友好页 |
| 目标连接失败 | 502 友好页 + `gateway_target_unreachable` 告警 |
| `/go/`、`/go/?host=` | 导航页 / 302 跳转（辅助入口，管理台「服务导航」为正式入口） |

## 部署

```bash
docker compose -f deploy/docker-compose.yml up -d --build   # host 网络硬约束
sudo ./scripts/install.sh                                   # systemd 安装
sudo ./scripts/install.sh --upgrade                         # 升级，保留用户数据
```

`network_mode: host` 是 mDNS 组播的硬约束，不能用 `ports:` 替代。所有可变状态集中在
一个 `/data` 卷（配置 / 密码摘要 / 日志 / 快照）。容器 HEALTHCHECK 与 systemd 都不
需要 curl：`lan-mdns --healthcheck` 自带探针。

## 实现位置

| 层 | crate / 文件 | 职责 |
| --- | --- | --- |
| 报文编解码 | `common/dns` | RFC 1035 / 6762 子集：A/AAAA/PTR/SRV/TXT、名称压缩、QU / cache-flush、Known-Answer 抑制、Goodbye |
| 组播传输 | `common/net` | raw fd 预置 `SO_REUSEADDR`/`SO_REUSEPORT` 后 bind、加入组播、网卡枚举 |
| 配置内核 | `common/config` | 列表型 YAML 拍平、类型化 getter、离线校验、`server.port` |
| 加密原语 | `common/crypto` | 零依赖 SHA-256 / HMAC-SHA256 / PBKDF2-HMAC-SHA256、常量时间比较 |
| 告警 | `common/alert` | 规范事件、5 平台适配、有界队列 / 冷却 / 重试 / 热更新、HTTP 与 curl sender |
| 鉴权 | `common/auth` | Admin Token、管理员密码摘要、`check_write()` |
| 日志 | `common/log` | 结构化记录、磁盘轮转、有界 `LogBuffer`、`EventSource` |
| HTTP | `common/api` | 路由 / 统一信封 / SSE 流式响应 / 写保护 |
| 引擎 | `src/engine.rs` | 收包循环、应答、摄取、宣告、探针、有界队列、统计、热重载 RR |
| 记录编译 | `src/config.rs` | `mdns:` 命名空间 → RR 集合、健康目标与网关路由解析 |
| 网关 | `src/gateway.rs` | 静态 Host 路由、双开关、友好页、`/go/`、有界并发、流式透传 |
| 运行态 | `src/state.rs` | 健康监控（含翻转事件）、冲突检测、服务地图聚合 |
| 快照 | `src/store.rs` | 配置保存 / 快照 / 历史 / 回滚 / 原子替换 / ID 白名单 |
| 组装 | `src/main.rs` | 生命周期、CLI 子命令、路由登记、指标、告警触发、信号处理、优雅退出 |

退出时（`SIGINT` / `SIGTERM`）通过信号标志 + 守护线程广播 TTL=0 的 Goodbye，
信号处理器内只做无锁原子写，保证 async-signal-safe。

## 运行态验收

```bash
python3 scripts/smoke_mdns.py
```

229 项断言，覆盖：存活 / 就绪、8 条 RR、9+16 指标族、A / PTR / QU 三种真实组播探针
闭环、409 → 403 → 200 写保护全链路、密码摘要 `0600`、配置保存 / 导出 / 导入 /
历史 / 回滚 / 热重载、路径穿越与 checksum 篡改拒绝、结构化日志与磁盘上界、
SSE replay + keepalive、健康目标 up/down、优雅退出与 Admin Token 回归；
M2.3 增量覆盖告警链路（健康翻转 / 目标不可达 / 冷却 / 热更新）、网关 Host
路由与友好页 / 双开关 / `/go/`、CLI 子命令与首次启动模板、部署资产静态断言、
以及两实例共享组播端口的共存验证；M2.4 增量覆盖管理台产物（`#app` 挂载点、内联
样式、无外部 script/link、六个区域标题）与 `/api/interfaces` 的绑定模式 /
`restart_required`。

## 明确不做（M2.4 之后）

- API 契约运行态测试与 CI 门禁（`F-J03`，M2.4 收尾项）
- 网卡集合的 socket 级热切换（变更需重启进程，已在管理台显式标注）
- 通用反向代理 / 负载均衡 / TLS 终止 / 请求体缓存（PRD 1.4 红线，永久不做）
- 钉钉 / 飞书加签 secret 与 Webhook 签名校验（后续 ADR 评估）
