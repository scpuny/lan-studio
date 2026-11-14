# lan-relay（M3）

跨网段 / 跨 VLAN 的 mDNS 中继。与 `lan-mdns` 完全独立部署，只依赖 `common-*` crate。

## 环路检测（PRD §4.3 硬性验收 / D8）

四道独立防线全部在 `src/loop_guard.rs`，是纯逻辑、可确定性单测：

| # | 防线 | 实现 | 默认值 |
| --- | --- | --- | --- |
| 1 | 包指纹去重窗口 | 对去掉中继尾标后的 DNS 报文做 SHA-256 前 8 字节指纹，`FingerprintWindow` 有界（容量 + 时间双约束），命中即 `duplicate` | 5s / 4096 条 |
| 2 | 出入网卡隔离 | 转发目标 = `interfaces - ingress`，为空则 `same_interface` | — |
| 3 | TTL / 跳数 | 报文尾部追加 EDNS0 OPT RR（TYPE 41），TTL 高 8 位携带剩余跳数；每次转发递减，归零丢弃 | 初始 4 跳 |
| 4 | 自生成包不转发 | 本地生成的报文登记 `local` 指纹窗口，反射副本一律 `self_generated` | 5s |

附加：每秒转发硬上限（`max_forward_per_second`）+ 源网段白名单。**60 秒压力测试**（`sixty_second_loop_storm_produces_no_broadcast_storm`）模拟双网卡环路、每秒注入 1000 个报文并立即回灌，断言转发数不破上限、去重窗口不越界、去重吸收量大于转发量，无广播风暴。

> OPT 尾标是合法 DNS 附加记录，标准 mDNS 客户端忽略即可正常解析；同一时刻报文里只有一条（转发时先剥离再重加），不破坏 ARCOUNT。

## 配置

首次启动生成 `lan-relay.yaml`（内嵌模板），此后**绝不覆盖**。可用 `LAN_STUDIO_CONFIG` 指定路径、`LAN_STUDIO_PORT` 覆盖端口（默认 8081）。

关键字段（完整见 `config.template.yaml`）：

```yaml
relay:
  group: "224.0.0.251"
  port: 5353
  interfaces: ["eth0", "eth1"]        # 逻辑网卡标识（也支持直接写 IPv4）
  allowed_networks: ["192.168.0.0/16"]
  fingerprint_window_ms: 5000
  max_hops: 4
  dedup_capacity: 4096
  max_forward_per_second: 2000
  transport: true
```

非回环监听必须设置 `server.admin_token`（F-F05，否则进程拒绝启动）。

## 契约端点

统一端点由 `common-component` 提供，见 PRD §3.2：`/healthz`、`/readyz`、`/api/status`、`/api/config`(GET/POST)、`/api/config/snapshot`、`/api/config/history`、`/api/config/rollback`、`/api/reload`、`/api/logs`、`/api/logs/stream`、`/api/metrics`、`/api/alerts`、`/api/auth*`、`/`（离线 WebUI）。

组件自有端点：

| 方法 | 路径 | 语义 |
| --- | --- | --- |
| GET | `/api/relay/stats` | 环路检测配置 + 全部计数 |
| GET | `/api/relay/interfaces` | 逻辑/物理网卡与传输状态 |
| POST | `/api/relay/inject` | 注入一个报文做环路判定（验收用） |
| POST | `/api/relay/mark-local` | 登记自生成报文 |

`inject` 请求体：`{"payload_hex":"...","ingress":"eth0","source_ip":"192.168.1.5","ttl":3,"self_generated":false}`，返回 `verdict` / `ttl` / `targets` / `frame_hex` / `loop_guard`。`verdict` 取值：`forward` / `duplicate` / `same_interface` / `ttl_exceeded` / `self_generated` / `outside_whitelist` / `rate_limited` / `malformed`。

## 指标

必备族由 `common-metrics` 统一输出（`lanstudio_relay_up` / `uptime_seconds` / `build_info` / `config_reloads_total` / `config_saves_total` / `http_requests_total` / `alerts_sent_total` / `log_disk_*`）。组件族：

`lanstudio_relay_packets_received_total`、`..._packets_forwarded_total`、`..._bytes_forwarded_total`、`..._loop_dropped_total{reason}`、`..._fingerprint_entries`。

告警 kind：`relay_loop_detected`(critical) / `relay_ttl_exceeded`(warning)，见 `docs/ALERT-SCHEMA.md`。

## 运维命令

```bash
cargo build --release -p lan-relay
./target/release/lan-relay --check          # 离线配置校验，不启网络
./target/release/lan-relay --version
LAN_STUDIO_PORT=18081 ./target/release/lan-relay
```

## 已知边界

- 多网卡严格选路依赖平台组播支持：Linux 下 `common-net` 会设置 `SO_REUSEPORT` 并按接口加入组播组；macOS 上同一端口通常只能绑定一个 socket，此时传输状态显示 `unavailable`/单 socket，但 **HTTP API 与四道环路检测仍完整可用**（验收脚本通过 `/api/relay/inject` 验证）。
- `transport: false` 可只跑 API 与环路判定，不占用 5353 端口。
