# ADR 0008 · M6 前端聚合：能力清单驱动 + Shell 同源代理

- 状态：**已接受并落地**（2026-11-14，方案 `docs/M6-PLAN.md`，销项 `docs/M6-CHECKLIST.md`）
- 前置：ADR 0007（M5 生态收敛）、用户拍板「前端聚合，但要根据后端引用的模块展示所需的页面元素」
- 影响：修订 ADR 0007 的 D-M5-3（不代理请求）与 D-M5-4（不统一前端）的**适用范围**，M5 结论本身仍成立

## 背景

M5 结束时存在三套前端：M2 的 Vue 3 + Arco 单文件控制台（838 kB）、其余 7 个组件共用的原生 JS 六区域控制台、Shell 的 8 卡导航页。其中：

- 六区域 `PAGES` 是硬编码的，与后端实际注册的模块无关；
- 40+ 组件专有端点没有交互界面；
- Shell 只做跳转，且服务端没有 CORS，浏览器无法跨域直连 8 个端口。

用户明确要求「前端聚合」，并追加硬约束：**页面元素由后端实际引用的模块决定**，未引用的模块不显示。

## 决策

| 编号 | 决策 | 结论 |
| --- | --- | --- |
| D-M6-1 | 聚合入口形态 | **A：统一 SPA + Shell 同源反向代理**。一个页面内切换 8 个组件，浏览器只与 Shell 同源通信 |
| D-M6-2 | M2 Arco 前端 | **保留并挂载**，不重写、不动 66/66 验收；在聚合页以 iframe 直连组件端口呈现，内部 `/api/*` 仍走自己的源 |
| D-M6-3 | 能力清单来源 | **编译期静态 JSON + CI 反向扫描**，不新增统一端点。清单是后端 `.route()` 注册的镜像，18 端点生态 DoD 不变 |
| D-M6-4 | 前端源码 | **就地升级 `console.html`（原生 JS 单文件、零构建链）**，不新建 Vite 工程；渲染器 `console-renderer.js` 由组件控制台与 Shell 共用同一份源文件 |
| D-M6-5 | 渲染规则 | 注册表是全集、实例化只认清单；`requires_lib` 让「是否引用 `common-probe`」直接决定电源状态面板是否出现；未引用模块不产生 DOM |
| D-M6-6 | 代理边界 | 组件 + 端点双层白名单，目标恒为 `127.0.0.1:<manifest port>`，5s 超时、1 MiB 响应上界、不跟随重定向、不缓存凭据 |
| D-M6-7 | 凭据 | Shell **不代管管理员密码**：DOM 中无密码输入框，写操作凭据由浏览器 `prompt()` 存入页面内存，仅以 `X-Admin-Password` 头透传，由目标组件执行写保护 |
| D-M6-8 | 指标基数 | `common-api` 前缀路由的 `path` 标签统一收敛为 `/p/`，不出现原始 URL，保持 PRD §4 无界标签禁令 |

### 对 ADR 0007 的修订范围

- D-M5-3「不代理请求」：M5 的 Shell 仍不代理；**M6 起新增 `/p/<name>/api/...` 单一前缀**，只做同源、白名单、有界的 API 转发。仍不做服务发现、不代管密码、不聚合业务数据、不成为数据总线。
- D-M5-4「不统一前端」：M5 不统一的事实成立；M6 在**不改 M2 实现**的前提下完成聚合（Arco 页作为 `standalone` 模块挂载），两套壳从「并存」变为「统一出口内的分工」。

## 实现要点

- `scripts/gen_capabilities.py` 从各组件 `main.rs` 的 `.route()` / `.route_write()` 反向派生 8 份 `capabilities.<name>.sample.json` 与 `capabilities.allowlist.txt`；`--check` 会比对 openapi、manifest `modules`、`write` 标记与 widget/icon 注册表，任何漂移即失败。
- `common/component::view_index` 与 `lan-mdns::view_index` 在 serve 时把清单注入 HTML；`lan-shell::view_index` 注入全部 8 份清单 + 共享渲染器。
- `common-api::App::route_prefix` 精确匹配优先、前缀兜底；`dispatch` / `route_label` / `is_write` 同步适配。
- `lan-shell::view_proxy` 显式调用 `common_auth::check`（`/p/` 不触发 `handle_conn` 的 `/api/` 守卫），再按 `allowlist.txt` 的 `name|path` 精确匹配，最后手写 HTTP/1.1 转发。

## 后果

**正面**：一个页面覆盖 8 个组件；页面元素与后端能力一一对应且由 CI 强制；零新增统一端点、零第三方依赖、零外部资源；M2 66/66 与 M2~M5 全部门禁不回退。

**代价 / 风险**：
- Shell 从纯导航层变成有界代理层，攻击面增大 → 用组件 + 端点双层白名单、固定目标 host、超时与体积上界收敛；`/api/*` 裸路径仍一律 404。
- 密码通过浏览器内存 + 请求头透传，未做加密存储 → 依赖同源/HTTPS 部署；文档明确不落盘、不进日志/快照。
- `lan-mdns` 的 Arco 页以 iframe 直连组件端口，属于刻意例外 → 组件不在线时该块显示空白，其余能力面板照常渲染。
- `common-api` 路由匹配新增前缀分支，影响所有二进制 → 单测 + 运行态门禁覆盖「精确优先」「label 有界」。

## 验证

`docs/M6-CHECKLIST.md` 第 9 节记录最终门禁：静态契约 34/0、M6 运行态 152/0、M5 91/0、生态 91/0、Rust 单测 201 passed、9 个二进制。
