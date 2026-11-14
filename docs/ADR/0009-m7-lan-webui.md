# ADR 0009 · M7 统一控制台：lan-webui 自包含子项目（前端 + Rust BFF）

- 状态：**已接受并落地**（2026-11-14，方案 `docs/M7-PLAN.md`，销项 `docs/M7-CHECKLIST.md`）
- 前置：ADR 0008（M6 前端聚合）、用户拍板「把 `lan-mdns` 下的 webui 拿出来作为独立前端项目 `lan-webui`，包含所有模块功能，后端控制前端显示哪些模块；`lan-webui` 包含自己的 rust 目录作为后端，它又是前端」
- 影响：**修订 ADR 0008 的 D-M6-1（聚合入口形态）与 D-M6-2（M2 Arco 前端挂载）的适用范围**。M6 的能力清单契约、生成器与同源代理模式被 M7 完整复用并前移；M6 的 `lan-shell` 降级为只读兼容入口。

## 背景

ADR 0008 用 `lan-shell` 把 8 个组件聚合到一个页面：Arco 控制台以 iframe 直连组件端口，其余能力面板走 `/p/<name>/api/...` 同源代理。这带来两个硬约束下的别扭：

- 两个前端工程并存（`lan-mdns/webui` 的 Arco SPA 与 `common/webui` 的原生 JS 渲染器），`lan-shell` 只是把它们拼起来；
- 用户现在要求一个**真正自包含**的统一控制台：既是前端、又带自己的 Rust 后端（BFF），后端「引入哪些模块」直接决定前端显示哪些模块，且全部模块功能都内置、不靠 iframe 外挂。

用户的原话：「`lan-webui` 包含自己的 rust 目录作为后端，然后它又是前端，懂吗？后端可以引入其他模块。」

## 决策

| 编号 | 决策 | 结论 |
| --- | --- | --- |
| D-M7-1 | 聚合形态 | **A：编译期声明（`modules.yaml`）+ 同源代理（BFF）**。`lan-webui` 是 workspace 第 23 个 member、第 10 个二进制，但不是第 9 个 `lan-*` 组件——它是聚合层。8 个组件仍各自独立进程，不拆 lib、不做单体。 |
| D-M7-2 | 统一出口 | **A：取代并保留只读兼容**。`lan-webui` 在 8090 成为统一控制台；`shell/` 保留只读兼容，不再发布为主入口。 |
| D-M7-3 | 前端物理位置 | **A：`git mv` 前端目录到 `lan-webui/webui`，产物 outDir 不变**（仍是 `common/webui/src/assets`，与 `lan-mdns` 共用同一份单文件产物）。 |
| D-M7-4 | 模块可见性来源 | **单一声明**：`lan-webui/modules.yaml` 的 `enabled_components` 是「后端引入哪些模块」的唯一真相；生成器据此合成 `lan-webui/capabilities.json`（聚合清单）+ `capabilities.allowlist.txt`（代理白名单）。移除一个条目即前端不显示该组件 UI，不碰任何前端标记。 |
| D-M7-5 | 前端渲染 | **能力清单驱动的全集渲染器**：前端读取注入的 `window.__LAN_CAPABILITIES__`，只渲染清单声明的组件 / 模块 / 面板 / 控件；未声明模块**不产生 DOM**（不是隐藏或灰化）。8 类 widget（`kv-grid` / `table` / `form` / `code-editor` / `metric-grid` / `log-list` / `text-view` / `action-list`）按 `panel.widget` 分派。 |
| D-M7-6 | 代理边界 | 继承 ADR 0008 的 D-M6-6 / D-M6-7：`/p/<name>/api/...` 双层白名单、固定 `127.0.0.1:<manifest port>`、5s 超时、1 MiB 上界、不跟随重定向、不缓存凭据；只透传 `X-Admin-Token` / `X-Admin-Password`，写保护始终在目标组件执行；DOM 无密码输入框。 |
| D-M7-7 | 静态资源 | 继承 D-M6-4 的零外部资源原则：`vite-plugin-singlefile` 产出单文件 HTML，`common-webui::INDEX_HTML` 内嵌；`lan-webui` 与 `lan-mdns` 共用同一份产物（D-M7-3）。 |
| D-M7-8 | 兜底清单 | 当页面脱离后端单独打开（render-check / 直接文件打开）时，前端内嵌一份提交的聚合清单作为兜底，保证 SPA 永不空白；经 `lan-webui` 实际服务时注入的清单（与 `modules.yaml` 同源）覆盖兜底。 |

### 对 ADR 0008 的修订范围

- D-M6-1「聚合入口形态」：M6 的聚合入口是 `lan-shell`；**M7 起聚合入口是 `lan-webui`**，`lan-shell` 降级为只读兼容。能力清单驱动 + 同源代理的核心模式不变。
- D-M6-2「M2 Arco 前端挂载」：M6 在聚合页以 iframe 直连组件端口挂载 Arco 页；**M7 不再用 iframe**——Arco 前端被改造为能力清单驱动的全集渲染器，经 BFF 代理拉取所有组件数据，单一 SPA 覆盖全部 8 个组件的全部模块。
- D-M6-3 / D-M6-4 / D-M6-5 / D-M6-8 的结论在 M7 中全部沿用，不重复论证。

## 实现要点

- `lan-webui/` 同目录含 `webui/`（Vue 3 + TS + Arco，单文件产物）与 `src/`（Rust BFF）。BFF 不用 `common_component::run`，手工构造 `Context`，只注册 4 条路由：`GET /`、`GET /healthz`、`GET /api/webui/status`、`GET|POST /p/`（前缀代理）。
- `lan-webui/modules.yaml` 是模块声明；`scripts/gen_capabilities.py --aggregate` 从它合成 `lan-webui/capabilities.json`（聚合，identity 为 `lan-webui`）+ `capabilities.allowlist.txt`，`--check` 比对生成结果与提交产物，漂移即失败。
- 前端 `capabilities.ts` 读取注入清单（缺失时回退到内嵌的同一份聚合 JSON）；`App.vue` 动态构建「组件 → 模块 → 面板」三级导航；`PanelView.vue` 按 `widget` 分发到 8 类 widget；`usePanel.ts` 统一负责取数 + 动作执行。
- `lan-webui/src/proxy.rs` 复用 M6 的白名单转发逻辑（TcpStream、5s/1MiB、只透传两个头、解析状态与 Content-Type 回写）；`common_auth::check` 在代理入口校验 loopback / token。

## 后果

**正面**：一个真正自包含的统一控制台（前端 + 后端同源），后端「引入哪些模块」直接决定前端 UI；单一 SPA 覆盖全部 8 组件的全部模块；能力清单由 CI 强制、零新增统一端点、零第三方依赖、零外部资源；M2~M6 全部门禁不回退。

**代价 / 风险**：
- `lan-shell` 退居只读兼容，历史部署若仍以其为主入口需迁移到 `lan-webui`。
- 前端不再按组件分别定制（M2 的六区域 bespoke 页面改为通用 widget 渲染），视觉细节略统一化，但保留 Calm Console 设计语言。
- `modules.yaml` 是前端可见性的唯一开关 → 配置错误（漏写端口 / 引用不存在的组件）会被 `validate()` 在启动前拦截。
- 内嵌兜底清单与 `modules.yaml` 同源，CI 用 `--aggregate` + `git diff` 保证两者不漂移。

## 验证

`docs/M7-CHECKLIST.md` 第 N 节记录最终门禁：静态契约 34/0、M7 `test_m7.py` 通过、`cargo test -p lan-webui` 7 passed、`npm run verify` RENDER OK、10 个二进制、`gen_capabilities.py --check` 8 manifests match、release.yml 含 `lan-webui` + `lan-shell` 兼容。
