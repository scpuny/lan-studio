# 指标字典 · METRICS

> 契约权威：`contracts/openapi.yaml`（`/api/metrics` 端点）。本文是 PRD 3.3 的可执行展开。
> 文本格式：Prometheus `0.0.4`。任何指标变更必须先改本文并同步 `contracts/`，再改实现。

## 1. 命名规范

```
lanstudio_<c>_<metric>[_<unit>]
```

- 全小写、下划线分隔。`<c>` = 组件名去掉 `lan-` 前缀，例如 `lan-mdns` → `mdns`、`lan-relay` → `relay`。
- Counter 必须以 `_total` 结尾；字节用 `_bytes`，秒用 `_seconds`。
- 禁止出现组件名前缀以外的历史命名；兼容别名见第 5 节。

## 2. 每个组件必备指标族

| 指标 | 类型 | 标签 | 说明 |
| --- | --- | --- | --- |
| `lanstudio_<c>_up` | gauge | — | 主循环运行中，1 / 0 |
| `lanstudio_<c>_uptime_seconds` | gauge | — | 运行时长 |
| `lanstudio_<c>_build_info` | gauge | `version,commit,target` | 构建信息，恒为 1 |
| `lanstudio_<c>_config_reloads_total` | counter | — | 热重载次数 |
| `lanstudio_<c>_config_saves_total` | counter | — | 配置保存次数 |
| `lanstudio_<c>_http_requests_total` | counter | `method,path,code` | API 请求数 |
| `lanstudio_<c>_alerts_sent_total` | counter | `kind,result` | 告警发送结果，`result=ok\|failed` |
| `lanstudio_<c>_log_disk_bytes` | gauge | — | 日志磁盘占用 |
| `lanstudio_<c>_log_disk_limit_bytes` | gauge | — | 日志磁盘上界 |

## 3. `lan-mdns` 专有指标

> 对应 PRD 第 5 章功能全集；M2 逐条销项时须同时核对指标覆盖。

| 指标 | 类型 | 标签 | 对应功能 |
| --- | --- | --- | --- |
| `lanstudio_mdns_records` | gauge | `type` | 当前发布记录数（A/AAAA/PTR/SRV/TXT） |
| `lanstudio_mdns_queries_total` | counter | — | 收到查询总数 |
| `lanstudio_mdns_responses_total` | counter | — | 发出应答总数 |
| `lanstudio_mdns_bytes_total` | counter | `direction` | 收发字节，`direction=in\|out` |
| `lanstudio_mdns_announces_total` | counter | — | 主动宣告次数 |
| `lanstudio_mdns_probe_total` | counter | `result` | 主动探针结果 |
| `lanstudio_mdns_health_targets` | gauge | `state` | 健康目标数，`state=up\|down\|unknown` |
| `lanstudio_mdns_query_audit_entries` | gauge | — | 审计环形缓冲当前条数 |
| `lanstudio_mdns_browse_instances` | gauge | — | 服务地图实例数 |
| `lanstudio_mdns_conflicts` | gauge | — | 冲突登记数 |
| `lanstudio_mdns_config_history_versions` | gauge | — | 快照版本数 |
| `lanstudio_mdns_auth_password_enabled` | gauge | — | 管理员密码是否已设置 |
| `lanstudio_mdns_gateway_enabled` | gauge | — | HTTP 网关全局开关（M2.3 接真实值） |
| `lanstudio_mdns_alerts_queued` | gauge | — | 等待投递的规范事件数（队列上界 200） |
| `lanstudio_mdns_gateway_routes` | gauge | — | 已登记静态 Host 路由数 |
| `lanstudio_mdns_gateway_requests_total` | counter | — | 网关请求总数（含转发 / 友好页 / 拒绝） |

`alerts_sent_total{kind,result}` 属第 2 节必备族，M2.3 起按 `docs/ALERT-SCHEMA.md`
已登记的 `kind` 输出真实样本（`result=ok|failed`）；`kind` 标签值只取登记表内的
固定枚举，禁止把主机名 / IP / URL 作为标签。

**可选单目标指标**：`lanstudio_mdns_health_target_up{target_alias}` 仅在目标配置了**别名**时输出，别名白名单上限 64；未配置别名时不得输出，目标明细走 `/api/health/targets`。

**实现状态（M2.3 收尾）**：上表 16 个专有族已全部实现并输出。M2.1 交付 7 个
（`records` / `queries_total` / `responses_total` / `announces_total` / `probe_total` /
`query_audit_entries` / `browse_instances`）；M2.2 补齐 6 个
（`bytes_total` / `health_targets` / `conflicts` / `config_history_versions` /
`auth_password_enabled` / `gateway_enabled`）；M2.3 把 `gateway_enabled` 接上真实
全局开关，并新增 3 个族（`alerts_queued` / `gateway_routes` /
`gateway_requests_total`）以及 `alerts_sent_total{kind,result}` 的真实样本。
冒烟脚本对 16 个族的存在性、标签取值、双前缀与开关 0/1 均有断言。

## 4. 标签规范（强制）

1. `path` 必须是**路由模板**（如 `/api/config/snapshot`），禁止使用原始 URL 或带查询参数的路径。
2. 禁止把 IP、域名、用户输入、请求 ID、快照 ID 作为标签值。
3. 枚举型标签必须使用本文登记的固定值；新增枚举值先改本文。
4. 标签值需转义 `\`、`"`、换行；单组件标签基数上界 2000，超限应降级为聚合指标。
5. `build_info` 的 `target` 使用 Rust 目标三元组，不允许任意字符串。

## 5. 兼容策略

- 历史前缀若需平滑过渡，可在**同一端点**额外输出别名指标，过渡期不少于一个大版本。
- 别名必须在本文件登记「引入版本 / 弃用版本 / 替代指标」三列，未登记不得输出。
- 过渡期结束后删除别名，并在此处记录删除版本。

## 6. 验收清单

- [ ] 组件启动后 `/api/metrics` 返回 200 且 `Content-Type: text/plain; version=0.0.4`
- [ ] 第 2 节 9 项全部存在且类型正确
- [ ] `lan-mdns` 第 3 节全部存在（M2 门禁）
- [ ] 无未登记标签值、无无界基数标签
- [ ] `log_disk_bytes <= log_disk_limit_bytes`
