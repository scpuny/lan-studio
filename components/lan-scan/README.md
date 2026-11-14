# lan-scan（M3）

局域网资产与端口**只读**探查（PRD §4.5）。不做漏洞扫描，不发送破坏性载荷，结果只写本组件自己的文件。

## 能力

| 能力 | 实现 |
| --- | --- |
| 网段扫描 | CIDR 展开（`expand_cidr`），硬上界 `max_hosts`；`/8` 这类超大段在建立任何 socket 之前被拒绝 |
| 主机发现 | 对配置端口做 TCP `connect_timeout`，任一端口开放即记为存活 |
| 端口 / 服务识别 | 端口 → 服务名映射 + 只读 banner（最多 `banner_bytes` 图形字符） |
| 结果快照 | `GET /api/scan/snapshot`，内存与磁盘均有界（`max_results`） |

## 硬约束与验收

- **速率与并发可配且有上界**：`RateLimiter` 固定窗口按 `rate_per_second` 放行，超额探测计入 `throttled`；`concurrency` 钳制在 1..64。单测 `rate_limiter_enforces_a_hard_ceiling` 与 `scan_results_and_probes_are_bounded` 断言探测次数不破上限。
- **不发送破坏性载荷**：`TcpProbe` 只有 `connect` 与 `read`，实现中没有任何 `write`。单测 `tcp_probe_never_writes_payload` 在对端断言收到 0 字节。
- **结果不写入其他组件配置**：`ScanStore::persist()` 只写 `scan.results_path`（原子替换），路径禁止 `..`；单测断言快照不含其他组件配置引用。
- 离线校验：`lan-scan --check` 不建立任何网络连接。

## 端点

统一契约端点由 `common-component` 提供（见 PRD §3.2）。组件自有：

| 方法 | 路径 | 语义 |
| --- | --- | --- |
| POST | `/api/scan/start` | `{"segment":"127.0.0.1/32","ports":[8080]}`，异步启动扫描 |
| GET | `/api/scan/status` | 运行状态、上次扫描统计、速率/并发配置 |
| GET | `/api/scan/results` | 保留的结果行（含 hits） |
| GET | `/api/scan/snapshot` | 自包含结果快照 |

扫描进行中重复 `start` 返回 `429 rate_limited`。

## 配置与命令

首次启动生成 `lan-scan.yaml`（默认端口 8083），此后不覆盖。关键字段见 `config.template.yaml`。

```bash
cargo build --release -p lan-scan
./target/release/lan-scan --check
LAN_STUDIO_PORT=18083 ./target/release/lan-scan
curl -s -X POST http://127.0.0.1:8083/api/scan/start \
  -H 'Content-Type: application/json' \
  -d '{"segment":"127.0.0.1/32","ports":[8083]}'
```

## 指标与告警

组件族：`lanstudio_scan_hosts_total`、`..._probes_total`、`..._throttled_total`、`..._open_ports`、`..._results`、`..._rate_per_second`。告警 kind：`scan_anomaly`(info，速率上限被触发)。
