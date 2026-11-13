# ADR 0004 · M2.3 告警、内置网关与部署运维

- 状态：已实施（2026-11-13）
- 影响范围：`common/alert`（新增）、`components/lan-mdns/src/gateway.rs`（新增）、
  `config.rs` / `state.rs` / `main.rs`、`deploy/`、`scripts/install.sh`、CI
- 关联契约：`contracts/alert-event.json`、`docs/ALERT-SCHEMA.md`、
  `contracts/openapi.yaml`（`/api/alerts`）、PRD 1.4 / 3.4 / 5-G / 5-H / 5-I
- 前置：ADR 0002（协议内核）、ADR 0003（可观测与安全）

## 1. 背景

M2.2 交付了可观测与安全，但 `/api/alerts` 仍是 M0 占位（`enabled:false`），
`gateway_enabled` 指标恒为 0，产物缺少容器 / 服务单元 / 安装脚本。M2.3 需要销项
18 条能力，同时不破坏三条既有约束：

1. **零第三方 crate**（M1 / ADR 0001），保证单文件静态发布与离线构建；
2. **PRD 1.4 红线**：HTTP 转发只对已登记域名做静态 Host 路由，不做通用反代、
   不缓存 / 不解析 / 不改写业务载荷；
3. **M0 契约冻结**：`config.schema.json` / `openapi.yaml` / `alert-event.json`
   不改结构，组件能力放进 `mdns:` 自有命名空间。

## 2. 决策

### D1 · 规范事件与平台适配集中在 `common-alert`

组件只产出 `contracts/alert-event.json` 规范事件；钉钉 / 飞书 / Slack /
企业微信 / 通用 JSON 五类适配全部在 `common/alert` 内完成，组件不得自行拼平台报文
（ALERT-SCHEMA §3）。事件体在构造时经过凭据关键字扫描，命中即整段替换为
`[redacted]`，避免把 `admin_token` / 密码带出内网。

`dedup_key` 与 `cooldown_seconds` 是**内部字段**，不进入规范事件 JSON，用于
「kind + key」冷却与 ALERT-SCHEMA 逐 kind 冷却覆盖（如
`gateway_target_unreachable` 600s、`config_invalid` 60s）。

### D2 · 零依赖下的 TLS：标准库 HTTP + 可选 curl 桥

Rust 标准库没有 TLS。若为此引入 `rustls` / `native-tls`，会破坏「零第三方 crate +
musl 静态单文件」目标，且 CA  bundle 会变成运行时外部依赖。因此：

| URL | 发送方式 | 外部依赖 |
| --- | --- | --- |
| `http://` | `TcpStream` 手写 HTTP/1.1 POST | 无 |
| `https://` | `Command::new("curl")` 子进程（参数数组，不经 shell） | 仅当配置了公网 Webhook |
| 其它 scheme | 直接拒绝 | 无 |

`curl` 只在配置 `https://` Webhook 时才需要；组件本体、mDNS、管理 API、网关都不依赖它。
容器镜像基于 alpine 预装 `curl` + CA + `iproute2`，但**发布物仍是单个二进制**，
拷到裸机上离线运行不受影响。钉钉 / 飞书签名（加签 secret）暂未实现，登记为后续项。

### D3 · 有界编排：队列 200、重试 ≤2、并发 128

`Dispatcher` 单 worker、有界 `VecDeque`，满时丢最旧并计数 `dropped`；冷却表同样设
4096 硬上界并按时间淘汰。发送失败按指数退避最多重试 2 次，`retries` / `failed`
进 `/api/alerts` 与 `alerts_sent_total{kind,result}`。网关每连接一线程，
`MAX_CONNECTIONS=128` 拒绝超额连接，所有 socket 设读写超时，避免长连接挂死。
这满足 F-C06「全链路有界资源」。

### D4 · 网关默认关闭，只路由已登记域名

`mdns.gateway.enabled` 默认 **false**：全局关闭时 `Gateway::start()` 直接返回
`None`，进程内不创建任何网关 socket（不会抢 80 端口）。开启后：

- 路由来源仅两类：① 已发布服务（其 `host` 能在本组件 A 记录里解析到 IP）自动
  生成 `host -> ip:service.port`；② `mdns.gateway.hosts[]` 显式登记，同名覆盖派生值。
- `services[].proxy` 与 `hosts[].proxy` 是每服务开关，关闭即视为未登记。
- `admin_host`（默认 `mdns.local`）单独转发到本机管理端口，不做路径改写。
- 未登记 / 代理关闭 → 404 友好页；目标不可达 → 502 友好页 + 规范事件。所有页面
  对 Host / 目标做 HTML 转义。
- 请求头原样转发，请求体用 `io::copy` 双向流式透传，**不缓存、不解析、不记录**。
- `/go/` 与 `/go/?host=` 辅助入口随本批次提前提供，但按 M2 销项纪律留到 M2.4 销项。

### D5 · 端口隔离钩子不进冻结 schema

`LAN_MDNS_GATEWAY_PORT`（以及 M1 的 `LAN_STUDIO_PORT`、M2.1 的 `LAN_MDNS_*`）
只用于本地 / CI 隔离，避免测试向真实 LAN 发包或抢占 80/5353。生产端口一律取自
YAML。`config.schema.json` 的 `server.port` 在 M2.2 已收回，本批次未改契约结构。

### D6 · 单文件自举：CLI 子命令承担首次启动与探针

新增 `--version` / `--print-config` / `--validate` / `--healthcheck`（F-I02 /
F-E09 / F-E02 / F-I07）。二进制首次启动会**生成一次**内嵌模板到配置路径，已存在则
永不覆盖。容器 HEALTHCHECK 与 systemd 都不需要 curl / shell：`--healthcheck`
用标准库连本机 `/healthz`。探针实现须循环读响应，因为管理服务器分两次写头与体。

部署资产：`deploy/Dockerfile`（多阶段 musl 静态构建 + 最小 alpine 运行时）、
`deploy/docker-compose.yml`（`network_mode: host` 硬约束、单卷 `/data`、
healthcheck）、`deploy/lan-mdns.service`（SIGTERM 优雅退出、SIGHUP 重载、
StateDirectory 持久化）、`scripts/install.sh`（安装 / `--upgrade`，明确不覆盖
`config.yaml` / `auth.toml` / `logs/` / `history/`）。

### D7 · 与系统 mDNS 守护进程共存（F-C03）

`common/net` 的 raw fd 在 bind 前置 `SO_REUSEADDR` + `SO_REUSEPORT`（M2.1 已有）。
M2.3 冒烟以**两个实例共享同一组播地址与端口、且都能应答查询**作为共存证据，
不依赖目标机是否安装 avahi。与真实 `avahi-daemon` / `systemd-resolved` 同机的
长稳压测留到部署观察期；这属于验证环境差异，不是协议实现缺口。

## 3. 触发链路

| kind | 触发点 | level | 冷却 |
| --- | --- | --- | --- |
| `health_transition` | `HealthMonitor::probe_all()` 检测 up↔down（`unknown` 不算翻转） | warning | 300s |
| `name_conflict` | watchdog 每 500ms 扫描 `State::conflicts()`，新冲突发一次 | warning | 300s |
| `config_invalid` | 热重载输入未通过离线校验 | warning | 60s |
| `config_reload_failed` | 热重载失败、保留旧配置 | warning | 60s |
| `gateway_target_unreachable` | 已登记域名转发目标连接失败 | info | 600s |

## 4. 后果与已知限制

- 正面：M2.3 后 `lan-mdns` 具备完整告警 / 网关 / 部署闭环；66 条功能销项 61 条。
- 限制 1：`https://` Webhook 依赖宿主 `curl`；裸机离线运行且配置了公网 Webhook 时
  会记录 `failed`。如需纯零依赖 TLS，须另立 ADR 引入静态 TLS crate 并重评体积门禁。
- 限制 2：网关不终止 TLS、不做健康检查摘除、不支持加权与缓存（PRD 1.4 明确禁止）。
- 限制 3：钉钉 / 飞书加签 secret、Webhook 负载均衡与签名校验未实现。
- 限制 4：正式 Arco 管理台（含健康状态灯、在线配置表单）仍在 M2.4；当前内嵌页
  是离线状态页，`/go/` 导航页是网关自带的最小辅助入口。

## 5. 验收证据

| 门禁 | 结果 |
| --- | --- |
| M0 契约校验（完整模式） | 21 passed / 0 failed |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 78 passed / 0 failed |
| `cargo build --release -p lan-mdns` | 成功，单文件约 1.5 MB（本地 GNU 动态构建） |
| `python3 scripts/smoke_mdns.py` | **221 passed / 0 failed** |
| 部署资产 | `bash -n install.sh`、`docker compose config -q`、`py_compile` 全通过 |
| 真实链路 | 健康翻转 / 目标不可达两类规范事件经本地 Webhook 收到；Host 路由、管理台入口、404/502 友好页、冷却去重、双实例共享组播端口均实测通过 |
