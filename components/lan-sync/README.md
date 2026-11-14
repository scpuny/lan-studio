# lan-sync — 多节点配置同步（M5 / PRD §4.8）

`lan-sync` 是 Lan Studio 的第 7 个组件（默认端口 `8086`），负责在多个节点之间
**基于 §3.6 配置快照契约**同步配置。它复用 `common-component` 运行时，因此 18 个
统一端点、`/healthz`、离线 WebUI、鉴权矩阵、9 个必备指标族、告警与日志结构与其它
组件零偏差。

## 1. 定位与三条硬边界

- **绝不静默覆盖**：默认策略 `reject`。每一次 `pull` 都是一次显式、可审计的决策
  （`accepted | rejected | pending | overwritten`），只有策略显式开启
  `last_write_wins` 才会覆盖，只有 `manual` 才进待确认队列、经
  `/api/sync/confirm` 人工确认后才落地到「本地基线」。冲突策略与理由都会被记录。
- **只调标准契约端点**：远端只请求 `GET /api/config/snapshot?reason=sync`（pull）
  与 `POST /api/config/snapshot`（push），配置文件以
  `contracts/snapshot.json` 的 6 字段为准，先验 `checksum == sha256(config_text)`
  再落地。**不调用任何组件专有端点**（`/api/scan/*`、`/api/wol/*` 等一律不碰），
  不解析对端业务配置。
- **凭据不进日志 / 指标 / 快照 / 导出 / 错误响应**：`node.token`、`sync.shared_secret`
  与管理员密码只存在于落盘的节点文件、内存态与转发的 HTTP 请求头里；
  `GET /api/sync/nodes` 用 `public_json()` 去掉 `token` 字段，`/api/sync/status`
  只暴露 `shared_secret_configured` 布尔值，审计行只存 `checksum` 前 16 字符。

附加边界：HTTP 客户端只支持 `http://` **回环地址**（`127.0.0.1` / `localhost` /
`::1`），拒绝 `https://` 与非回环主机；节点与审计均为有界 JSON lines（`.tmp` +
`fs::rename` 原子写），路径禁 `..`。

## 2. 冲突策略与决策

| strategy | 行为 |
| --- | --- |
| `reject`（默认） | 本地已有 `created_at >= 远端` 的基线 → `rejected/conflict`；否则 `accepted/no_conflict`。**不静默覆盖**。 |
| `last_write_wins` | 显式开启后 → `overwritten/explicit_override`，覆盖本地基线。 |
| `manual` | → `pending/awaiting_confirmation`，进待确认队列，等 `/api/sync/confirm`。 |

`checksum` 校验失败永远优先返回 `rejected/bad_checksum`，对应 HTTP `400 invalid_config`。
决策纯函数 `decide(local_created_at, incoming, strategy)` 见 `src/sync.rs`。

## 3. 配置字段（`config.template.yaml`，`sync.*`）

| key | 默认 | 校验 | 说明 |
| --- | --- | --- | --- |
| `sync.nodes_path` | `lan-sync.nodes.json` | 禁 `..` | 节点登记 JSON lines（含 token，仅落盘） |
| `sync.history_path` | `lan-sync.history.json` | 禁 `..` | 有界审计 JSON lines |
| `sync.max_nodes` | `64` | `1..4096` | 节点容量上界 |
| `sync.strategy` | `reject` | 三值之一 | 冲突策略 |
| `sync.request_timeout_ms` | `2000` | `100..30000` | 回环 HTTP 连接 / 读 / 写超时 |
| `sync.shared_secret` | `""` | 空则禁用签名 | HMAC-SHA256 共享密钥，绝不输出 |
| `sync.history_limit` | `200` | `1..10000` | 审计环形容量 |
| `sync.allowed_components` | 8 个标准组件名 | 只能是 8 个标准名 | 数组解析不出来时默认允许 8 个标准组件 |

8 个标准组件名：`lan-mdns` / `lan-relay` / `lan-ca` / `lan-scan` / `lan-hosts` /
`lan-wol` / `lan-sync` / `lan-ssdp`。`shared_secret` 非空时，pull/push 请求附带
`X-Lan-Sync-Sig: sha256:<hex>`（GET 对请求路径、POST 对请求体做 HMAC），节点的
`token` 作为 `X-Admin-Token` 转发给对端做身份校验。

## 4. 端点（8 个，POST 全部 `route_write` 受管理员密码保护）

| 方法 | 路径 | 语义 / 响应 |
| --- | --- | --- |
| GET | `/api/sync/nodes` | `{"items":[{id,endpoint,enabled,note,registered_at}],"total":N}`，**无 token 字段** |
| POST | `/api/sync/node` | body `{"id","endpoint","token"?,"enabled"?,"note"?}` → `{"id":...,"total":N}` |
| POST | `/api/sync/remove` | `{"id"}` → `{"removed":true\|false}` |
| POST | `/api/sync/pull` | `{"node_id","component"}` → `{"decision":...,"reason":...,"node_id":...,"component":...,"checksum":...}`；未登记 `404 not_found`、checksum 错误 `400 invalid_config`、不可达 `502 target_unreachable` |
| POST | `/api/sync/push` | `{"node_id"}` → 导出本地快照 POST 到对端 → `{"pushed":true,"checksum":...,"node_id":...}` |
| POST | `/api/sync/confirm` | `{"id","accept":true\|false}` → 处理 manual 待确认队列 → `{"decision":...,"applied":bool}`，`id` 用 pull 返回的 `checksum` |
| GET | `/api/sync/status` | `{"nodes","strategy","pulls","pushes","conflicts","audit","pending","shared_secret_configured"}`，不输出 secret |
| GET | `/api/sync/history` | `{"items":[...],"total":N,"limit":N}`，审计有界 |

审计行字段：`time / direction(pull|push|confirm) / node / component / checksum(前 16 字符) /
decision / reason`，不含任何凭据。

## 5. curl 示例（含首次设置管理员密码）

写端点受管理员密码保护（未设密码时返回 `409 setup_required`）。首次先设密码：

```bash
# 0) 启动（第一次运行会在当前目录生成 config.template.yaml 的副本，绝不覆盖）
LAN_STUDIO_PORT=8086 ./target/debug/lan-sync --config /tmp/lan-sync.yaml

# 1) 首次设置管理员密码（此端点本身不受写保护）
curl -sS -X POST http://127.0.0.1:8086/api/auth/password \
  -H 'Content-Type: application/json' \
  -d '{"new_password":"Str0ng!Pass"}'

# 之后所有 /api/sync/* 写请求都要带 X-Admin-Password
PW='X-Admin-Password: Str0ng!Pass'

# 2) 登记一个节点（token 仅在请求中出现一次，之后任何响应都不再返回）
curl -sS -X POST http://127.0.0.1:8086/api/sync/node -H "$PW" \
  -H 'Content-Type: application/json' \
  -d '{"id":"peer-a","endpoint":"http://127.0.0.1:18086","token":"peer-token","note":"机架 A"}'

# 3) 查看节点（确认没有 token 字段）
curl -sS http://127.0.0.1:8086/api/sync/nodes

# 4) 拉取对端快照并做冲突决策（只请求标准 /api/config/snapshot）
curl -sS -X POST http://127.0.0.1:8086/api/sync/pull -H "$PW" \
  -H 'Content-Type: application/json' \
  -d '{"node_id":"peer-a","component":"lan-sync"}'

# 5) 推送本地快照到对端标准导入端点
curl -sS -X POST http://127.0.0.1:8086/api/sync/push -H "$PW" \
  -H 'Content-Type: application/json' -d '{"node_id":"peer-a"}'

# 6) manual 策略下确认待处理项（id 取第 4 步返回的 checksum）
curl -sS -X POST http://127.0.0.1:8086/api/sync/confirm -H "$PW" \
  -H 'Content-Type: application/json' \
  -d '{"id":"sha256:...","accept":true}'

# 7) 状态 / 审计 / 指标
curl -sS http://127.0.0.1:8086/api/sync/status
curl -sS http://127.0.0.1:8086/api/sync/history
curl -sS http://127.0.0.1:8086/api/metrics | grep lanstudio_sync
```

## 6. 用 fake HTTP server 做本地验收

不需要真实对端组件，只需一个监听回环高端口、实现 `GET /api/config/snapshot` 的
最小 HTTP 服务（返回统一信封 `{"ok":true,"data":{§3.6 快照}}`，`Content-Length` +
`Connection: close`）。Python 示例（终端 A）：

```python
import hashlib, json
from http.server import BaseHTTPRequestHandler, HTTPServer

CONFIG = "version: 1\ncomponent: lan-sync\n"
SUM = "sha256:" + hashlib.sha256(CONFIG.encode()).hexdigest()
SNAP = {
    "schema": 1, "component": "lan-sync",
    "created_at": "2026-11-14T00:00:00Z", "reason": "sync",
    "config_text": CONFIG, "checksum": SUM,
}

class H(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"ok": True, "data": SNAP, "request_id": "fake"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass

HTTPServer(("127.0.0.1", 18086), H).serve_forever()
```

终端 B：按第 5 节登记 `endpoint: http://127.0.0.1:18086` 后调用
`/api/sync/pull`，应得到 `decision=accepted`（无本地基线）或按策略返回
`rejected|pending|overwritten`。把 `SNAP["config_text"]` 改一个字节但保留旧
`checksum`，pull 必须返回 `400 invalid_config`（篡改校验失败）。停掉 fake server
再 pull，必须返回 `502 target_unreachable` 并产生 `sync_node_unreachable` 告警。

单测里的 `http_get` / `http_post` 用本地 `TcpListener` 做往返，不访问真实网络；
所有文件用例使用 `std::env::temp_dir()` 下唯一子目录并清理，不污染仓库目录。

## 7. 指标（`docs/METRICS.md` §3.7，前缀 `lanstudio_sync_`）

| 指标 | 类型 | 标签 |
| --- | --- | --- |
| `lanstudio_sync_nodes` | gauge | — |
| `lanstudio_sync_pull_total` / `lanstudio_sync_push_total` | counter | — |
| `lanstudio_sync_conflicts_total` | counter | `strategy`（`reject` / `last_write_wins` / `manual` 固定枚举） |
| `lanstudio_sync_audit_entries` | gauge | — |

标签为固定枚举，不含 IP / 域名 / token / 节点名。第 2 节 9 个必备指标族由运行时自动输出。

## 8. 告警 kind

| kind | level | 触发 |
| --- | --- | --- |
| `sync_conflict` | warning | `reject` 冲突 / `manual` 待确认 / 篡改 checksum 等被拒绝或挂起的决策 |
| `sync_node_unreachable` | warning | pull / push 远端连接失败、超时或地址非法 |

详情字段绝不携带 token 或 shared_secret。

## 9. 明确不做的部分

- 自动全量覆盖本地配置文件（「绝不静默覆盖」）；`confirm` 只更新本地基线与队列，
  不改写组件配置文件，不代管管理员密码。
- 组件专有配置解析 / 实时双向增量同步 / 事件总线（D10）。
- X.509 / TLS / 非回环直连；节点身份用轻量 HMAC-SHA256 共享密钥 + 转发的 token。
- 抓取对端 `/api/config/snapshot` 之外的任何端点（S5 边界，源码可扫）。
- 单测访问真实网络；所有文件读写限于临时目录。

## 10. 单测覆盖（`src/sync.rs` 14 项 + `src/main.rs` 8 项）

checksum 正确 / 篡改、三种冲突策略各一（含无本地基线 accept）、Node JSON
roundtrip + token 不外泄、审计有界且不含凭据、HMAC 签名篡改失败、
`http_get`/`http_post` 对本地 `TcpListener` 往返、SnapshotLite 解析裸快照与标准信封、
回环 endpoint 校验、NodeStore 容量、json helper、strategy 封闭枚举、
模板校验、非法 strategy / 路径穿越 / 越界字段 / 未知组件拒绝、数组解析、Hooks 身份。
