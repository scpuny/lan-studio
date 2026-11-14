# Lan Studio Shell（统一出口）

> M5 聚合层。**不是第 9 个组件**，不实现任何业务能力，也不是数据总线（PRD §2.3 / D3 / D10）。

Shell 只做一件事：把 8 个组件的入口汇集到一个离线单文件页面里，并通过有界的 `/healthz` 探测点亮状态灯。它不代理任何组件请求、不聚合业务数据、不代管管理员密码。

## 1. 边界（红线）

| 项 | 结论 |
| --- | --- |
| 组件总数 | 仍是 8 个；Shell 不计入组件清单、不出现在 `docs/METRICS.md` 的组件指标表里 |
| 路由 | 仅 3 条：`GET /`、`GET /healthz`、`GET /api/shell/status`；**没有** 18 个统一端点 |
| 鉴权 | 只透传 AdminToken（浏览器内拼到目标组件 URL 的 `?token=`），不输入、不存储、不转发管理员密码 |
| 反向代理 | 无。请求 `/api/records` 等组件专有端点一律 404 |
| 数据 | 静态 manifest + 内存中的布尔状态表；不读组件配置、不读快照、不发事件 |
| 前端 | 单文件内联 CSS/JS/SVG，零外部资源；Calm Console 视觉，8 张组件卡 |

因此 Shell **不能**用 `common_component::run`：那会把 `GET /` 绑到组件六区域控制台并注册 18 个统一端点，而 `App::dispatch` 是「首个匹配路由胜出」，后注册的同路径无法覆盖。Shell 用 `common_api::App` 手工构造 `Context`。

## 2. 运行

```bash
cargo build -p lan-shell
LAN_STUDIO_PORT=8090 ./target/debug/lan-shell
# 离线校验（不监听端口）
./target/debug/lan-shell --check
# {"ok":true,"version":1,"components":8,"probe_enabled":true}
```

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_SHELL_CONFIG` | `shell/components.yaml` | manifest 路径，启动时读取，修改需重启 |
| `LAN_STUDIO_PORT` | `8090` | 监听端口，仅绑定 `127.0.0.1` |
| `LAN_SHELL_TOKEN` | 空 | 非空时 `/api/*` 需 `X-Admin-Token`（或 `?token=`）；`GET /` 与 `/healthz` 不受影响 |

命令行：`--check` / `--version` / `--help`。

## 3. manifest 格式

`shell/components.yaml` 是极简 YAML（逐行解析，不支持嵌套对象数组以外语法）：

```yaml
version: 1
probe_enabled: true
probe_interval_seconds: 15
probe_timeout_ms: 300
components:
  - name: lan-mdns
    label: 名称解析与服务发现
    port: 8080
    icon: dns
    tone: blue
  # ... 共 8 条
```

- `tone` 是白名单：`blue / cyan / purple / teal / indigo / amber / green / orange`。
- 组件数上限 32；`port` 必须非 0；可选字段缺省时 `label=name`、`icon=cube`、`tone=blue`。
- 探测间隔被夹到 `5..3600` 秒、超时夹到 `50..5000` ms；并发上限 4（`std::thread::scope` 分批），单轮探测对所有组件各发一次 `GET /healthz`。
- `probe_enabled: false` 时完全不发网络请求，所有 `online` 返回 `null`。

## 4. 端点

| 方法 | 路径 | 响应 |
| --- | --- | --- |
| GET | `/` | 200 `text/html`，内嵌单文件 8 卡页面 |
| GET | `/healthz` | 200 `{"ok":true}`（纯 JSON，非统一信封，与组件一致） |
| GET | `/api/shell/status` | 统一信封 `{"ok":true,"data":{...},"request_id":"..."}` |

`data` 形状：

```json
{
  "components": [
    {"name":"lan-mdns","label":"名称解析与服务发现","port":8080,
     "icon":"dns","tone":"blue","online":true,"checked_at":"2026-11-14T03:00:00Z"}
  ],
  "total": 8,
  "online": 3,
  "probe_enabled": true,
  "token_hint": false
}
```

`online` 为 `true` / `false` / `null`（未探测）。响应**不含** `password`、`token`、`admin_token`、`shared_secret` 任何字段。

```bash
curl -s http://127.0.0.1:8090/healthz
curl -s http://127.0.0.1:8090/api/shell/status | python3 -m json.tool
curl -s http://127.0.0.1:8090/api/records      # 404：不代理组件接口
```

## 5. 页面

- **8 张卡**：沿用 M2 Calm Console 版式——44px 圆角矩形彩色图标芯片在左（`border-radius:12px` + 淡彩底）、两行文字（组件名 / 中文说明 / 端口）在右并右对齐、卡片左侧 3px 语义色条、右上角状态灯。
- **顶栏**：主题切换（`localStorage` 持久化）、可选 AdminToken 输入框（`type=text`，仅浏览器内使用，**没有密码输入框**）。
- **只读参数区**：横向表单，label 左对齐固定 120px、两列网格，说明跨双列；明确提示 manifest 修改需重启。
- **状态刷新**：每 `probe_interval_seconds` 拉一次 `/api/shell/status` 更新状态灯，不触发整页动画。
- 所有图标为内联 SVG；页面无 `<script src>`、无外链样式、无 `url(http...)`。

点击卡片以新标签打开 `http://<当前主机>:<port>/`，若填写了 Token 则附加 `?token=...`。Token 只在浏览器里从本页传到目标页，**不经过 Shell 服务端**。

## 6. 指标

`lanstudio_shell_components_online`（gauge，本轮探测中可达的组件数）登记在 `docs/METRICS.md` §3.7。Shell 是聚合层，不暴露组件级 `/api/metrics` 端点，也不参与 §8.2 第 3 条的 9 个必备指标族扫描。

## 7. 与生态 DoD 的关系

- §8.2 第 4 条：Shell 能导航到所有已运行组件，状态灯与真实 `/healthz` 一致 —— `scripts/test_ecosystem.py` 逐项断言。
- §8.2 第 5 条：发布矩阵把 `lan-shell` 作为第 9 个**二进制**产物（不是第 9 个组件）打包，附 sha256。
- §8.2 第 6 条：manifest 的 8 个 name / 8080–8087 端口与 README、`contracts/openapi.yaml` 一致。

## 8. 明确不做

反向代理 / SSO / 单点登录、组件间事件总线、业务数据聚合、统一 Arco 前端壳（M3/M4 的原生 JS 六区域控制台继续并存，偏差登记在 `docs/ADR/0007-*`）、动态服务发现（manifest 是静态的）、跨主机探测（当前只探 `127.0.0.1`）。
