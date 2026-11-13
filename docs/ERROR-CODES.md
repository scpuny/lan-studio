# 错误码表 · ERROR-CODES

> 契约权威：`contracts/openapi.yaml` → `components.schemas.ErrorCode`。本文是 PRD 3.2 的展开。
> 所有 `/api/*` 错误必须使用统一响应包装，禁止返回裸错误栈或 HTML 错误页。

## 1. 统一响应包装

```json
{
  "ok": false,
  "error": { "code": "auth_failed", "message": "管理员密码错误" },
  "request_id": "01J9Z8K4M2"
}
```

- `code` 必须来自第 2 节枚举；`message` 人类可读，**不得包含凭据、堆栈、内部路径**。
- `request_id` 同时写入结构化日志 `extra`，用于串联排查。
- 成功响应恒为 `{"ok":true,"data":{...},"request_id":"..."}`。
- 客户端可用 `X-Request-Id` 指定追踪号，服务端原样回显为 `request_id`。

### 1.1 信封例外（白名单，逐条登记）

以下端点**不套**统一信封，因为它们承载的不是业务数据而是特定线格式。新增例外必须先改本节并同步 `contracts/openapi.yaml`。

| 端点 | 线格式 | 理由 |
| --- | --- | --- |
| `GET /healthz` | 裸 `{"ok":true}` | 探针要能被任意检测器解析，不引入额外层级 |
| `GET /api/metrics` | `text/plain`（Prometheus 0.0.4） | 抓取端固定解析此格式 |
| `GET /api/logs/stream` | `text/event-stream`（SSE） | 逐帧推送，无法包成单次 JSON |

`GET /api/config/snapshot` **不属于**例外：导出内容作为 `data` 放进信封，`data` 的形状为 `Snapshot`。

## 2. 错误码枚举

| code | HTTP | 含义 | 典型场景 | 客户端处理 |
| --- | --- | --- | --- | --- |
| `invalid_config` | 400 | 配置不合法 | YAML 解析失败、字段越界、快照导入未通过校验 | 展示 `message`，保留用户未提交的输入，不重载配置 |
| `auth_required` | 401 | 缺少凭据 | 未带 `X-Admin-Token` 访问受保护端点 | 弹出 Token 输入，重试一次 |
| `auth_failed` | 403 | 凭据错误 | Token 错误 / 管理员密码错误 / 缺失 | 清空已缓存凭据，提示重新输入 |
| `setup_required` | 409 | 尚未初始化 | 管理员密码未设置时的写操作 | 引导到首次设置密码流程 |
| `not_found` | 404 | 资源不存在 | 未知 API 路径、快照 ID 不存在、未登记域名 | 返回友好页（网关场景）或提示 |
| `target_unreachable` | 503 | 转发目标不可达 | HTTP 网关已登记域名但后端连不上 | 返回友好 503 页，不暴露后端地址与栈 |
| `rate_limited` | 429 | 触发限流 | 探测 / 写入 / 告警发送过频 | 按 `Retry-After` 退避重试 |
| `internal_error` | 500 | 内部错误 | 未预期的异常 | 记录 `request_id`，提示用户用该 ID 查日志 |

## 3. 鉴权与错误码的对应关系

| 场景 | 期望 code |
| --- | --- |
| 空 Token 且非回环地址访问 `/api/*` | `auth_required` |
| Token 错误 | `auth_failed` |
| 写操作未带 `X-Admin-Password` 且密码已设置 | `auth_failed` |
| 写操作未带 `X-Admin-Password` 且密码**未设置** | `setup_required` |
| 改密时 `current_password` 错误 | `auth_failed` |
| 快照 ID 非法（路径穿越尝试） | `invalid_config` |

## 4. 健康检查例外

- `/healthz` 与 `/readyz` 不套用统一包装的业务语义：`/healthz` 恒返回 `{"ok":true}`。
- `/readyz` 未就绪时返回 503 + `ApiError` 包装，`code` 使用 `internal_error`。
- 业务健康目标状态属于**正常业务数据**，走 `/api/health/targets` 并返回成功包装，不得用错误码表达 down 状态。

## 5. 新增错误码流程

1. 同时修改 `contracts/openapi.yaml` 的 `ErrorCode` 枚举与本文第 2 节。
2. 补充 `scripts/verify_contracts.py` 的一致性断言（脚本会自动比对本文与枚举）。
3. 走 PR，禁止实现返回未登记 code。

## 6. 验收清单

- [ ] 8 个 code 全部在 OpenAPI 枚举中登记
- [ ] 每个 code 有明确 HTTP 状态与客户端处理建议
- [ ] 错误响应中无凭据 / 堆栈 / 内部路径
- [ ] 友好页（404 / 503）与管理 API 共用同一设计语言
