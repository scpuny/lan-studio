# lan-mdns（M2.1 协议内核）

> 内网 `.local` 名字解析与 DNS-SD 服务发现组件。
> **M2.1 已完成真实 mDNS 引擎**：组播收发、查询应答、启动/周期宣告、退出 Goodbye、
> Known-Answer 抑制、QU 单播应答、外来应答摄取、主动探针，以及有界查询审计与观测队列。
> 后续批次见 [`docs/M2-CHECKLIST.md`](../../docs/M2-CHECKLIST.md)：M2.2 可观测/安全/配置生命周期，
> M2.3 告警/网关/部署，M2.4 WebUI 与全量销项。

## 构建与运行

```bash
cargo build --release -p lan-mdns
LAN_STUDIO_PORT=8080 LAN_STUDIO_CONFIG=components/lan-mdns/config.template.yaml \
  ./target/release/lan-mdns
```

CLI 模式（不监听端口，离线校验配置并输出构建信息）：

```bash
./target/release/lan-mdns --cli
```

内建配置模板 `config.template.yaml` 发布 3 条静态记录（A / AAAA / TXT）、1 个
`_http._tcp.local` 服务实例、IPv4 反向 PTR 与服务类型枚举，共编译为 8 条 RR。

## 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_STUDIO_CONFIG` | `config.yaml` | 配置文件路径；不存在时使用编译期内嵌模板 |
| `LAN_STUDIO_PORT` | `8080` | HTTP 监听端口（暂不放进已冻结的配置 schema） |
| `LAN_MDNS_GROUP` | 配置文件 `mdns.group` | 仅测试隔离用：覆盖组播地址，必须是组播地址 |
| `LAN_MDNS_PORT` | 配置文件 `mdns.port` | 仅测试隔离用：覆盖 UDP 端口 |
| `GIT_COMMIT` | `dev` | 编译期注入，进入 `build_info` 指标 |

两个 `LAN_MDNS_*` 覆盖只在运行时引擎生效，`--cli` 始终报告配置文件自身的值。
CI / 本地冒烟用它们把流量限制在 `239.x` 隔离组 + 高位端口，绝不向真实 LAN 发包。

配置结构遵循 `contracts/config.schema.json`；公共段由 `common-config` 校验，
组件 `mdns:` 命名空间由本组件的 `config.rs` 解析并编译为 RR。

## 已实现端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 内嵌离线 WebUI |
| GET | `/healthz` | 存活探针，恒 `{"ok":true}` |
| GET | `/readyz` | 就绪探针（引擎已启动才 ready） |
| GET | `/api/status` | 协议状态、记录数、组播地址/端口、引擎计数 |
| GET | `/api/metrics` | Prometheus 0.0.4（9 必备族 + 7 个 mdns 专有族） |
| GET | `/api/config` | 当前配置原文 |
| GET | `/api/auth` | 鉴权状态 |
| GET | `/api/records` | 当前发布的 RR 集合（含 TTL / cache-flush 位） |
| GET | `/api/interfaces` | 本机 IPv4 网卡枚举 |
| GET | `/api/queries` | 有界查询审计（`?limit=`，1..1000） |
| GET | `/api/browse` | 外来应答摄取（服务地图原始条目） |
| POST | `/api/probe` | 主动探针：`{"name","type","unicast"}` |

`/api/*` 受 `common-auth` 保护：`admin_token` 为空时仅允许回环访问，非空时校验
`X-Admin-Token` 或 `?token=`。`/healthz` 与 `/readyz` 不受鉴权。

响应统一包装 `{"ok":true,"data":...,"request_id":...}`，错误码见
[`docs/ERROR-CODES.md`](../../docs/ERROR-CODES.md)。

## 协议实现位置

| 层 | crate / 文件 | 职责 |
| --- | --- | --- |
| 报文编解码 | `common/dns` | RFC 1035 / 6762 子集：A/AAAA/PTR/SRV/TXT、名称压缩、QU / cache-flush、Known-Answer 抑制、Goodbye |
| 组播传输 | `common/net` | raw fd 预置 `SO_REUSEADDR`/`SO_REUSEPORT` 后 bind、加入组播、网卡枚举 |
| 配置内核 | `common/config` | 列表型 YAML 拍平、类型化 getter、离线校验 |
| 引擎 | `src/engine.rs` | 收包循环、应答、摄取、宣告、探针、有界队列、统计 |
| 记录编译 | `src/config.rs` | `mdns:` 命名空间 → RR 集合 |
| 组装 / HTTP | `src/main.rs` | 生命周期、路由、指标、优雅退出 |

退出时（`SIGINT` / `SIGTERM`）通过信号标志 + 守护线程广播 TTL=0 的 Goodbye，
信号处理器内只做无锁原子写，保证 async-signal-safe。

## 运行态验收

```bash
python3 scripts/smoke_mdns.py
```

62 项断言，覆盖存活/就绪、8 条 RR、9+7 指标族、A/PTR/QU 三种真实组播探针闭环、
查询审计、鉴权（401/403/200）、`--cli` 离线校验与优雅退出。

## 明确不做（M2.1）

- 服务地图 TTL 聚合、冲突检测、TCP 健康监控、SSE 日志流、日志轮转（M2.2）
- 配置保存 / 快照 / 回滚 / 导入导出 / 热重载、管理员密码（M2.2）
- 告警 Webhook、内置 HTTP 网关、Docker / systemd / install.sh（M2.3）
- Arco Design 正式管理台（M2.4，当前为编译期内嵌的占位页）
