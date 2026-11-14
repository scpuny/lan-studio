# M7 方案：lan-webui —— 前后端一体的独立统一控制台

> 依据：用户指令「把 `lan-mdns` 下的 webui 拿出来作为独立的前端项目，作为 `lan-webui`，包含所有模块的功能，同样是后端控制前端显示哪些模块的功能。`lan-webui` 包含自己的 rust 目录作为后端，然后它又是前端，后端可以引入其他模块。」
> 状态：**方案已落档，待拍板**（2026-11-14）。本文件只定义方案，**未写实现代码**。
> 定位：M7 与 M6 同为工程演进项，**不改 PRD §7.5 的 M0~M5 规格里程碑**，不回退 M6 已销项的任何门禁。

---

## 1. 需求拆解（对架构的理解）

**「懂」的部分：**

1. `lan-webui` 是 workspace 根目录下的**自包含子项目**（与 `shell/` 平级，不是第 9 个 `lan-*` 组件）：同一个目录里既有前端工程，也有自己的 Rust 后端工程；后端编出**自己的二进制**，前端产物由该二进制 `include_str!` 内嵌，仍是单文件、离线、零外链。
2. 前端从 `components/lan-mdns/webui/` 迁出，沿用 **Vue 3 + TypeScript strict + Arco Design**，但能力从「M2 六区域」扩展为**所有模块的功能全集**：8 个组件的 `core.*` 与全部专有模块都有对应 UI 实现。
3. 页面元素**不由前端硬编码全集决定**：后端引入 / 注册了哪些模块，前端才生成哪些导航、页面、按钮、表单；未引入模块**连 DOM 都不生成**（延续 M6 判定，不是隐藏、不是置灰、不是空态占位）。
4. 「后端可以引入其他模块」= Rust 侧通过 **Cargo 依赖 + 显式模块声明**，把其他组件 / 共享库纳入本二进制的能力集合；声明是能力清单的唯一来源，CI 反向校验「声明 = 后端实际可用」。
5. 与现状的本质区别：M2 前端是 `lan-mdns` 的附属源码、产物编进 `lan-mdns` 二进制；M6 SPA 是 `lan-shell` 里的原生 JS 页面。`lan-webui` 是**独立部署单元**，有自己的端口、`--version` / `--check` / `--help` / `--healthcheck` 与 release 产物。

**一处必须澄清的歧义**：「后端引入其他模块」可以指「编译期 Cargo 依赖 + 运行期代理」（BFF，8 组件仍独立进程），也可以指「把 8 个组件的业务逻辑全部编译进这一个二进制」（单体）。两者工作量与对 PRD §8.2 生态 DoD 的影响完全不同，见 ★D-M7-1。

---

## 2. 现状事实（本轮已核对）

| 位置 | 现状 | 对 M7 的约束 |
| --- | --- | --- |
| `components/lan-mdns/webui/` | Vue3 + TS + Arco，15 个源文件、六区域页面、`demo.ts`；`vite.config.ts` 的 `outDir` 指向 `../../../common/webui/src/assets` | 迁出后 CI 的 `webui` job `working-directory` 必须同步改 |
| `common/webui/src/lib.rs` | `pub const INDEX_HTML = include_str!("assets/index.html")`，带离线断言单测 | 产物必须继续无外链；`lan-mdns` 复用同一产物可做到零行为变化 |
| `components/lan-mdns/src/main.rs:1430` | `view_index` 注入 `window.__LAN_CAPABILITIES__`，Arco 页本身不消费清单 | M7 起前端真正消费清单，M2 二进制行为保持不变 |
| `shell/src/main.rs` | M6 SPA + `route_prefix` 代理 + 8 份清单 `include_str!` + `allowlist.txt`；`PROXY_TIMEOUT_MS=5s`、1 MiB 上界 | 代理实现可直接搬到 `lan-webui/src/proxy.rs` |
| `common/component/src/assets/console-renderer.js` | M6 原生 JS 渲染器，8 类 widget（`kv-grid` / `table` / `form` / `code-editor` / `metric-grid` / `log-list` / `text-view` / `action-list`） | widget 语义可复用，改成 Vue 组件实现 |
| `contracts/examples/capabilities.*.sample.json` × 8 | 五层清单 `component → area → module → panel → endpoint/action`，约 20 个专有模块 id | 合成清单直接复用，不新造 schema |
| `scripts/gen_capabilities.py` | 反向扫描 `.route()` / `.route_write()` 生成 8 份 sample + `allowlist.txt`，CI `--check` | 增加 `--aggregate` 产出 `lan-webui` 合成清单 |
| `shell/components.yaml` | 8 组件静态 manifest，每组件带 `modules:` 列表与端口 | `lan-webui/modules.yaml` 沿用同一形状，但**只列被引入的组件** |
| `.github/workflows/ci.yml` / `release.yml` | `webui` job 硬编码 `components/lan-mdns/webui`；release 为 8 组件 + `lan-shell` × 2 arch | 路径与第 9 个二进制名需同步 |

---

## 3. 决策清单

| 编号 | 决策 | 默认值 | 理由 / 影响 |
| --- | --- | --- | --- |
| ★ **D-M7-1** | 「后端引入其他模块」的语义 | **A：编译期声明 + 同源代理（BFF）** | 后端依赖 `common-api` / `common-auth` / 所需 `common-*` 与共享库，`modules.yaml` 声明启用哪些组件，业务数据经 `/p/<name>/api/...` 转发到各组件独立进程。8 组件独立部署、18 端点生态 DoD **不变**，M6 代理实现可复用。备选 B（组件 lib 化后单体编译）需把 8 组件拆 `lib + bin`、重写 release 矩阵，并破坏 PRD §8.2「单独部署」与 M5 销项依据，工作量约 3 倍；备选 C（共享库编译进来、业务仍代理）可作为 A 的子集 |
| ★ **D-M7-2** | `lan-shell` 的处置 | **A：`lan-webui` 后端取代 `lan-shell` 成为统一出口**，端口沿用 8090 | 避免两套聚合 UI 长期漂移；`shell/` 目录保留为只读兼容或移除，`release.yml` 第 9 个二进制换名，二进制总数仍为 9。备选 B（并存，新端口 8091）会让 M6 SPA 与 M7 Arco 页同时存在 |
| ★ **D-M7-3** | M2 前端源码处置 | **A：`git mv components/lan-mdns/webui → lan-webui/webui`**，产物 `outDir` 仍为 `common/webui/src/assets/index.html` | `lan-mdns` 二进制继续内嵌同一份产物，M2 的 66/66 与 75/0 零回退；只有一份 Arco 源码。备选 B（复制一份）会产生两份漂移源码 |
| D-M7-4 | 前端渲染方式 | **A：通用 widget 渲染器** | Vue 侧实现 8 个 widget 组件，消费清单里的 `panel.widget` 元数据；不手写 40+ 个专有页面。`widget: "custom"` + 注册表扩展留给复杂交互（如拖拽树），但需单独登记 |
| D-M7-5 | 能力清单来源 | **A：`lan-webui/modules.yaml` → 生成器合成 → 编译期注入** | 不侵入 `Hooks`；CI 校验「声明组件 ⊆ 8 份清单」「模块 ⊆ 后端实际 `.route()`」「代理白名单 ⊆ 合成清单」，三者任一漂移即失败 |
| D-M7-6 | 鉴权与代理边界 | **A：沿用 D-M6-6 / D-M6-7** | 组件 + 端点双层白名单、目标恒为 `127.0.0.1:<port>`、5s / 1 MiB 有界、不跟随重定向、不缓存凭据；只转发 `X-Admin-Password` / `X-Admin-Token`，DOM 中无 `type="password"`，密码只存页面内存 |
| D-M7-7 | M2 资产与门禁 | **A：全部保留** | Calm Console 设计系统、图标体系、`?demo=1`、指标卡版式、tabs 边框修复一并迁走；`npm run verify` / `render-check` 的 DOM 断言随新页面扩展同步更新 |
| D-M7-8 | 编号与文档 | **A：叫 M7（工程演进项）** | 新增 `docs/M7-PLAN.md` / `M7-CHECKLIST.md` / `ADR/0009`；README 里程碑表加「M7（工程演进）」行，M0~M5 状态不动 |

---

## 4. 目录与部署形态

```
lan-studio/
├── Cargo.toml                  # workspace 22 → 23 member
├── common/webui/src/assets/index.html   # ★ 唯一单文件产物（两个二进制共用）
├── lan-webui/                  # ★ 新增：独立前后端一体项目
│   ├── Cargo.toml              # package lan-webui，依赖 common-* 与可引入的模块 crate
│   ├── modules.yaml            # ★ 后端声明：启用哪些组件 / libs / 端口
│   ├── README.md
│   ├── src/
│   │   ├── main.rs             # 二进制入口：--check / --version / --healthcheck / 监听
│   │   ├── capabilities.rs     # 编译期 include_str! 合成清单 + view_index 注入
│   │   ├── proxy.rs            # 同源白名单代理（复用 M6 实现）
│   │   └── assets/             #（可选）不放产物，避免两份 HTML 分叉
│   └── webui/                  # ★ 前端工程（自 components/lan-mdns/webui git mv）
│       ├── package.json  vite.config.ts  tsconfig.json
│       ├── scripts/render-check.mjs
│       └── src/
│           ├── main.ts  App.vue  api.ts  demo.ts  icons.ts  Icon.vue
│           ├── capabilities.ts   # 清单类型 + 过滤（唯一入口）
│           ├── widgets/          # 8 个通用 Vue widget
│           │   ├── KvGrid.vue  TableView.vue  FormPanel.vue  CodeEditor.vue
│           │   └── MetricGrid.vue  LogList.vue  TextView.vue  ActionList.vue
│           ├── core/             # M2 六区域页面，作为 core.* 模块实现保留
│           │   └── OverviewPage.vue … HistoryPage.vue
│           └── styles/app.css   # Calm Console 设计系统原样迁走
└── shell/                      # 按 D-M7-2 处置
```

**产物路径决策（默认）**：`vite.config.ts` 的 `outDir` 保持 `common/webui/src/assets`；`lan-mdns` 继续 `common-webui::INDEX_HTML`，`lan-webui` 后端用
`include_str!("../../common/webui/src/assets/index.html")` 内嵌**同一份**，只在 `</head>` 前注入自己的合成清单。好处是一份产物、两个二进制，绝不漂移；代价是 `lan-webui` 的 Cargo 需要 `common-webui = { workspace = true }`（纯静态资产 crate，零依赖）。

**端口**：默认 8090（取代 `lan-shell`）；若 D-M7-2 选 B 则 8091。

---

## 5. 数据流

**构建期**

```
8 组件 .route()/.route_write()
        │  scripts/gen_capabilities.py
        ▼
8 份 capabilities.<name>.sample.json + capabilities.allowlist.txt
        │  + lan-webui/modules.yaml（enabled_components / libs）
        │  scripts/gen_capabilities.py --aggregate
        ▼
lan-webui 合成清单 capabilities.json（只含被引入组件的 modules）
        │  CI --check：声明 ⊆ 8 份清单；模块 ⊆ 后端注册；白名单 ⊆ 合成清单
        ▼
npm run verify（typecheck + vite build + render-check）→ 单文件 index.html
        ▼
cargo build：index.html + 合成清单编进 lan-webui 二进制
```

**启动期**

```
lan-webui --check      校验 modules.yaml / 清单 / 白名单 / Cargo 依赖四者一致
GET /                  view_index 把合成清单注入 window.__LAN_CAPABILITIES__
GET /healthz          存活探针（不占统一信封）
GET /api/webui/status  只读状态：已引入组件数、模块数、代理计数上界
```

**运行期（读）**

```
浏览器 → GET / → capabilities.ts 过滤出 visibleModules()
       → App.vue 生成导航 / 页面 / widget（清单外不产生 DOM）
       → widget 调 GET /p/<name>/api/... → 代理双层白名单校验
       → 127.0.0.1:<manifest port> → 组件统一信封 → 渲染
```

**运行期（写）**

```
表单提交 → POST /p/<name>/api/... + X-Admin-Password
         → 代理转发 → 组件自身写保护（409 setup_required / 401 等）
         → 结果回显；代理不缓存凭据、不代管密码
```

---

## 6. 数据结构

### 6.1 后端声明 `lan-webui/modules.yaml`

```yaml
version: 1
port: 8090
# 后端引入哪些组件，前端就显示哪些模块的页面元素
enabled_components:
  - name: lan-mdns
    port: 8080
  - name: lan-hosts
    port: 8084
  - name: lan-wol
    port: 8085
libs:
  - common-probe        # 决定 lan-wol.probe 面板是否出现
```

### 6.2 合成清单（注入前端）

```json
{
  "schema": 1,
  "component": "lan-webui",
  "runtime": "aggregated",
  "libs": ["common-probe"],
  "components": [
    {
      "name": "lan-mdns",
      "label": "名称解析与服务发现",
      "port": 8080,
      "tone": "blue",
      "modules": ["/* 原样复用 capabilities.lan-mdns.sample.json 的 modules */"]
    }
  ]
}
```

> 说明：不新造模块 schema，`modules` 数组元素与 M6 的 `module` 完全一致（`id` / `title` / `icon` / `tone` / `area` / `requires_lib` / `panels`），只在合成时给每个模块带上 `component` 归属。

### 6.3 前端类型与唯一过滤入口

```ts
// lan-webui/webui/src/capabilities.ts
export interface Panel {
  id: string; title: string; widget: WidgetKind
  endpoint?: { method: string; path: string }
  actions?: { id: string; label: string; method: string; path: string; write?: boolean }[]
  fields?: { name: string; label: string; type: string; required?: boolean }[]
}
export interface Module {
  id: string; title: string; icon: string; tone: Tone; area: 'workspace' | 'system'
  requires_lib?: string; panels: Panel[]; component: string
}
export interface Capabilities {
  schema: 1; component: 'lan-webui'; libs: string[]; components: ComponentCaps[]
}

export function visibleModules(caps: Capabilities): Module[] {
  return caps.components.flatMap((c) =>
    c.modules
      .filter((m) => !m.requires_lib || caps.libs.includes(m.requires_lib))
      .filter((m) => m.panels.length > 0)
      .map((m) => ({ ...m, component: c.name }))
  )
}
```

`App.vue` 只遍历 `visibleModules()` 生成菜单与 `<component :is="widgetFor(panel.widget)">`；代码中**不出现** `if (name === 'lan-wol')` 之类的组件分支。

### 6.4 Rust 后端路由骨架

```rust
// lan-webui/src/main.rs（示意）
const INDEX_HTML: &str = include_str!("../../common/webui/src/assets/index.html");
const CAPS_JSON: &str = include_str!("../capabilities.json");   // 由生成器产出

fn main() {
    let app = App::new()
        .route("GET", "/", view_index)                 // 注入 window.__LAN_CAPABILITIES__
        .route("GET", "/healthz", view_healthz)
        .route("GET", "/api/webui/status", view_status)
        .route_prefix("GET", "/p/", view_proxy)        // 复用 M6 代理
        .route_prefix("POST", "/p/", view_proxy);
    // --check / --version / --help / --healthcheck / LAN_STUDIO_PORT
}

fn view_index(_: &Request, _: &Context) -> Response {
    let script = format!(
        "<script>window.__LAN_CAPABILITIES__={};</script>",
        CAPS_JSON.replace("</", "<\\/")
    );
    Response::html(200, INDEX_HTML.replacen("</head>", &format!("{script}</head>"), 1))
}
```

---

## 7. 前端视图

**外壳**：顶部产品名 + 组件切换器（只列 `enabled_components`）+ 状态灯 + 主题切换；左侧导航按 `area` 分组，item 全部来自 `visibleModules()`，图标 / tone 来自清单；主区为模块页，由若干 Panel 卡片组成。

**8 个 widget（Arco 实现）**

| widget | Arco 组件 | 数据源 | 备注 |
| --- | --- | --- | --- |
| `kv-grid` | `a-descriptions` | `GET endpoint` 返回对象 | 状态总览、运行参数 |
| `table` | `a-table` | 数组 | 记录 / 设备 / 证书 / 映射列表 |
| `form` | `a-form` + 动态字段 | `actions[].write` | 字段底色与边框沿用 M2 修复 |
| `code-editor` | `a-textarea`（等宽） | `GET /api/config` | `.ls-code` 底色与边框 |
| `metric-grid` | 指标卡 | `GET /api/metrics` | 大图标在左、两行文字右对齐、圆角矩形 chip |
| `log-list` | `a-list` | `GET /api/logs` | 有界滚动 |
| `text-view` | `a-typography` | 文本响应 | 渲染结果 / 快照导出 |
| `action-list` | `a-button` 组 | `actions[]` | 写操作带密码头 |

**视觉**：Calm Console 设计系统、`tone-*` 语义色、`Icon.vue` 内联 SVG、告警指标卡撑满、`.arco-tabs-type-card-gutter .arco-tabs-content { border: none }` 全部原样迁走。

**演示模式**：`?demo=1` 的 `fetch` / `EventSource` 拦截扩展到 `/p/*` 与 `/api/*`，覆盖全部模块样例 fixture；生产默认 no-op。

**鉴权 UI**：密码经 `window.prompt()` 只存页面内存，DOM 中无 `type="password"` 输入框（沿用 D-M6-7）。

---

## 8. 实施分批（拍板后执行）

| 批次 | 内容 |
| --- | --- |
| **M7.0 契约与骨架** | `modules.yaml` schema、`gen_capabilities.py --aggregate`、合成清单 + 白名单派生、`git mv` 前端目录、`Cargo.toml` 22→23 member、CI `webui` job 路径、`release.yml` 第 9 个二进制 |
| **M7.1 Rust 后端** | `main.rs`（4 条路由 + 子命令）、`capabilities.rs`（编译期注入）、`proxy.rs`（复用 M6 代理）、`modules.yaml` 解析与 `--check` 一致性校验 |
| **M7.2 前端** | `capabilities.ts` + 8 个 widget Vue 组件、`App.vue` 动态导航、M2 六区域作为 `core.*` 保留、`demo.ts` 扩展、`render-check.mjs` 增加负向 DOM 断言 |
| **M7.3 门禁与文档** | `scripts/test_m7.py`（清单漂移 / 未引入模块零 DOM / 代理白名单 / 写密码透传 / 离线产物）、CI 纳入、`docs/M7-CHECKLIST.md`、`docs/ADR/0009-m7-lan-webui.md`、README 与 ADR 0008 适用范围修订 |
| **M7.4 全量回归** | fmt / clippy / test、静态契约 34/0、M2 75/0、M3 66/66、M4 76/76、M5 91/0、M6 152/0、生态 91/0、`npm run verify`、9 个二进制、提交并 push |

---

## 9. 风险与边界

- **D-M7-1 若选 B（单体编译）**：PRD §8.2 生态 DoD 第 2 条「单独部署」与 M5 销项依据会失效，需要重写 `test_ecosystem.py` 与 release 矩阵，成本远高于 A。
- **widget 表达力**：40+ 专有端点里若有复杂交互（拖拽树、三栏弹窗）超出 8 类 widget，走 `widget: "custom"` + 显式注册表，并在 M7-CHECKLIST 登记，不允许偷偷写死页面。
- **M2 零回退**：`lan-mdns` 二进制行为、`66/66` 销项、`75/0` 运行态契约、`229` 项冒烟必须全部保持；产物路径不变是这条的保证。
- **ADR 0008 修订**：D-M6-1（统一 SPA 落在 `lan-shell`）、D-M6-2（Arco iframe 直连组件端口）在 M7 后不再适用，需在 ADR 0009 明确收敛范围；M2 Arco 页不再以 iframe 挂载，而是成为 `lan-webui` 本身。
- **不改**：M0 冻结契约、PRD §7.5 的 M0~M5 里程碑状态、8 组件的 18 个统一端点与独立部署模型。

---

## 10. 决策登记

| 编号 | 用户选择 | 登记时间 |
| --- | --- | --- |
| D-M7-1 | 待拍板 | — |
| D-M7-2 | 待拍板 | — |
| D-M7-3 | 待拍板 | — |
| D-M7-4 ~ D-M7-8 | 默认采纳（如无异议） | — |
