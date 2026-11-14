# 告警事件登记表 · ALERT-SCHEMA

> 契约权威：`contracts/alert-event.json`。本文是 PRD 3.4 的 `kind` 注册表。
> 组件只产出**规范事件**；平台适配由 `common-alert` 统一完成，组件不得自行拼接平台报文。

## 1. 规范事件字段

| 字段 | 必填 | 约束 |
| --- | --- | --- |
| `source` | 是 | `^lan-[a-z0-9-]+$`，等于 `config.component` |
| `level` | 是 | `info` / `warning` / `critical` |
| `kind` | 是 | 见第 2 节登记表，必须已登记 |
| `title` | 是 | 1..200 字符，人类可读，不含凭据 |
| `detail` | 是 | ≤2000 字符，不含凭据 |
| `time` | 是 | RFC 3339 UTC，必须以 `Z` 结尾 |
| `extra` | 否 | 结构化对象，禁止嵌套大文本 |
| `request_id` | 否 | 串联一次请求 |

示例见 `contracts/examples/alert-event.sample.json`。

## 2. `kind` 登记表

| kind | 来源组件 | 默认 level | 触发条件 | `extra` 字段 | 建议冷却 |
| --- | --- | --- | --- | --- | --- |
| `health_transition` | `lan-mdns` | warning | 目标 up→down 或 down→up | `target_alias,from,to` | 300s |
| `name_conflict` | `lan-mdns` | warning | 同名异值应答登记 | `name,source_ip` | 300s |
| `config_invalid` | 全部 | warning | 配置校验失败（含导入 / 保存 / 回滚） | `field,reason` | 60s |
| `config_reload_failed` | 全部 | warning | 热重载失败，回退到旧配置 | `reason` | 60s |
| `gateway_target_unreachable` | `lan-mdns` | info | 已登记域名转发目标不可达 | `host` | 600s |
| `relay_loop_detected` | `lan-relay` | critical | 包指纹窗口命中重复转发 | `interface,fingerprint` | 60s |
| `relay_ttl_exceeded` | `lan-relay` | warning | 转发跳数归零被丢弃 | `interface` | 600s |
| `cert_issued` | `lan-ca` | info | 内网域名证书签发成功 | `subject,serial` | 0s |
| `cert_expiring` | `lan-ca` | warning | 证书剩余有效期低于阈值 | `subject,days_left` | 86400s |
| `cert_revoked` | `lan-ca` | info | 证书被吊销 | `subject,serial` | 0s |
| `scan_anomaly` | `lan-scan` | info | 扫描速率或异常响应超阈值 | `segment,reason` | 600s |
| `sync_conflict` | `lan-sync` | warning | 多节点快照冲突 | `peer,strategy` | 300s |
| `device_wake_failed` | `lan-wol` | warning | 唤醒宽限期结束后仍未上线 | `device` | 300s |
| `wol_wake_sent` | `lan-wol` | info | 已登记设备唤醒包发送成功 | `device,result` | 0s |
| `hosts_publish_failed` | `lan-hosts` | warning | 渲染产物发布失败，旧文件保持完好 | `path,reason` | 300s |
| `hosts_conflict_detected` | `lan-hosts` | warning | 同名异 IP 或 `.local` 保留后缀被拒绝 | `name,reason` | 300s |
| `ssdp_discovery_error` | `lan-ssdp` | info | 发现应答解析失败 | `reason` | 600s |
| `sync_node_unreachable` | `lan-sync` | warning | 已登记节点快照请求超时或不可达 | `node` | 300s |
| `shell_component_offline` | `lan-shell` | info | 健康探测发现组件离线 | `component` | 600s |

## 3. 平台适配（由 `common-alert` 统一实现）

| 平台 | 识别依据 | 转换要点 |
| --- | --- | --- |
| 钉钉 | Webhook 域名含 `oapi.dingtalk.com` | `msgtype=markdown`，title + detail 合并 |
| 飞书 | `open.feishu.cn` / `open.larksuite.com` | `msg_type=interactive` 卡片 |
| Slack | `hooks.slack.com` | `text` 字段 |
| 企业微信 | `qyapi.weixin.qq.com` | `msgtype=markdown` |
| 通用 JSON | 其余 | 原样发送规范事件对象 |

## 4. 编排默认值

| 项 | 默认 | 约束 |
| --- | --- | --- |
| 队列容量 | 200 | 有界，满时丢弃最旧并计数 |
| 失败重试 | 2 次 | 上限 2，指数退避 |
| 发送超时 | 5s | 可配，`exclusiveMinimum: 0` |
| 冷却去重 | 300s | 按 `kind + key` 去重，见第 2 节逐项覆盖 |
| 配置热更新 | 支持 | 改 Webhook 列表无需重启 |

## 5. 新增 `kind` 流程

1. 在本文第 2 节登记（来源 / level / 触发 / extra / 冷却）。
2. 在 `contracts/alert-event.json` 无需改动（`kind` 为模式匹配，非枚举）；但需补 `contracts/examples/` 之外的测试用例。
3. 同步 `docs/METRICS.md` 的 `alerts_sent_total{kind}` 标签枚举说明。
4. 走 PR，禁止组件私自使用未登记 `kind`。

## 6. 验收清单

> M2.3 实现状态（`common-alert` + `lan-mdns` 冒烟）：以下各项均已通过。

- [x] 示例事件通过 `alert-event.json`（M0 契约校验完整模式，21/21）
- [x] 所有已登记 kind 均能在第 2 节查到来源与冷却值
- [x] 5 类平台适配有离线测试（可注入 fake sender）——`common/alert` 9 个单测
- [x] 风暴场景下冷却去重生效、队列不越界——队列 200 / 冷却表 4096 硬上界；运行态重复不可达事件被抑制
- [x] 事件体中不出现令牌 / 密码 / 摘要——构造时凭据关键字扫描 + 冒烟断言
