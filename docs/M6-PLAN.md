# M6 前端聚合方案：能力清单驱动的统一控制台

> 依据：用户指令「前端聚合，但要根据后端引用的模块展示所需的页面元素」。
> 状态：**方案已落档，待拍板**（2026-11-14）。本文是 M6 的范围依据，逐条销项与实测数字另建 `docs/M6-CHECKLIST.md`。
> 边界：不改 M0 冻结契约、不改 PRD §7.5 的 M0~M5 里程碑状态（M5 仍是规格书的最后一个阶段），M6 是**工程演进项**而非规格新增阶段。

---

## 1. 需求拆解

### 1.1 判定

用户要的是**一个统一前端**，且页面元素不是写死的全集，而是由后端实际引用/注册的模块反向决定：后端引用了哪个模块，前端才出现哪个模块的导航、面板、控件；未引用的模块**连 DOM 都不生成**（不是隐藏、不是灰掉、不是留空态占位）。

这推翻了上一轮「不建议合并单一 SPA、只做共享源码 + 注入专有面板」的建议。本方案按「前端聚合」执行。

### 1.2 现状事实核对（2026-11-14）

| 位置 | 现状 | 与目标的差距 |
| --- | --- | --- |
| `common/component/src/lib.rs:35` | `Hooks { name, version, default_config, validate, reload, status, refresh_metrics, routes, start }`，`#[derive(Clone, Copy)]` | 没有可被前端消费的**能力声明**；`routes` 是 `fn(App) -> App`，只有编译后的路由表，无元数据 |
| `common/component/src/lib.rs:877` | `standard_routes()` 注册 18 个统一端点 | 六区域可用，但不知道哪些区域对哪些组件有效 |
| `common/component/src/lib.rs:975` | `let app = (hooks.routes)(standard_routes(App::new()));` | 「后端引用的模块」= `standard_routes` + `Hooks.routes`，这是能力清单的**唯一真实来源** |
| `common/component/src/assets/console.html:155` | `PAGES` 硬编码 6 项，`renderNav()` 由它生成导航；`load(k)` 是 6 条 if | 所有 7 个组件显示完全相同的六区域，与后端能力无关 |
| `common/component/src/assets/console.html` | 全文 `/api/hosts`、`/api/wol`、`/api/sync` 等出现 **0 次** | 40+ 专有端点没有任何交互界面 |
| `components/lan-mdns/webui/` | Vue 3 + TypeScript strict + Arco，产物 `common/webui/src/assets/index.html`（838.30 kB） | 第二套壳，独立实现，未接入统一能力模型 |
| `shell/src/main.rs:319` | `SHELL_HTML.replace("__SHELL_DATA__", &manifest_data_json(&manifest))` | **已有「后端清单 → 前端 DOM」的雏形**，但只到 8 张卡，没有下沉到页面元素 |
| `shell/components.yaml` | 8 个组件的 `name / label / port / icon / tone` | 无 `modules` / `capabilities` 字段 |
| `common/api/src/lib.rs:172` | `App::dispatch` 精确匹配 `route.path == req.path`，无路径参数 / 通配 | Shell 若做同源反向代理需受控扩展 |
| `common/api/src/lib.rs:205` | `handle_conn` 仅对 `/api/` 前缀做鉴权 | 代理路径若不以 `/api/` 开头，必须在 handler 内自行鉴权 |
| 全仓 | 无 `Access-Control-Allow-Origin` | 浏览器跨域直连 8 个端口不可行（除非组件加 CORS 白名单） |

### 1.3 目标定义

**做**：
1. 建立**能力清单**（capabilities）作为后端 → 前端的唯一契约，机器可读、编译期生成、CI 校验一致性。
2. 一份共享前端源码 + 一个通用渲染器：`注册表是全集、实例化按 capabilities`。
3. 统一控制台能在一个应用内切换组件与模块，模块、面板、按钮、表单字段全部由清单驱动。
4. 后端 `libs`（如是否引用 `common-probe`）决定通用元素是否出现，例如只有引用 `common-probe` 的组件才显示「设备电源状态」面板。
5. 离线、零外部资源、单文件产物、深浅主题、图标内联 SVG 的既有约束全部保留。

**不做**：
- 不新增第 9 个组件；Shell 仍是聚合层。
- 不做组件事件总线 / 数据总线（PRD D10 边界不变）。
- 不让 Shell 代管管理员密码、不把凭据写进日志 / 快照 / 导出。
- 不改 M2 已验收的 66 条功能与旧 `/api/browse`、`/api/status` 等响应形状。

---

## 2. 决策清单（默认决策，标 ★ 的需拍板）

| 编号 | 决策 | 默认方案 | 理由 / 代价 |
| --- | --- | --- | --- |
| ★ **D-M6-1** | 聚合入口形态 | **A：共享壳内嵌每个组件 + Shell 统一 SPA 入口（同源反向代理）** | 真正做到「一个应用」；代价是 Shell 需代理，见 §6.3。备选 B（各组件内嵌、Shell 只导航）改动最小但不算聚合；备选 C（浏览器跨域直连）要给 8 个组件加 CORS |
| ★ **D-M6-2** | M2 Arco 前端处置 | **保留 Arco 产物，作为 `lan-mdns` 模块被统一壳挂载** | 保住 66/66 与前端产物门禁；代价是短期内仍有一块「异质」页面。备选「完全重写」成本最高、风险最大 |
| **D-M6-3** | 能力清单来源 | **编译期常量，由 `Hooks.capabilities` 声明，注入 HTML 占位符** | 不新增统一端点 → 18 端点生态 DoD 断言、openapi、契约门禁全部不用改 |
| **D-M6-4** | 清单格式 | `contracts/capabilities.schema.json` + `contracts/examples/capabilities.*.sample.json` | 与既有 `service-record.json` 同一套机器可读契约风格 |
| **D-M6-5** | 清单粒度 | `component → area → module → panel → action/field` 五层 | 既能生成导航，也能生成表格列与表单字段 |
| **D-M6-6** | 模块 id 命名 | `core.*`（统一契约）/ `<组件前缀>.*`（专有）/ `lib.*`（共享库派生） | 与 openapi 的 tag / 路径前缀一致，便于 CI 比对 |
| **D-M6-7** | 前端源码位置 | 新建 `common/console/`（原生 TS/JS + Vite 单文件产物），替换 `common/component/src/assets/console.html` | 保留零外部资源；M2 的 `components/lan-mdns/webui/` 不动 |
| **D-M6-8** | 鉴权 | 统一请求层统一带 `X-Admin-Password` / `AdminToken`；代理只透传，不缓存、不落盘 | 沿用现有 `common_auth::check` / `check_write` 语义 |
| **D-M6-9** | 一致性门禁 | `scripts/verify_capabilities.py`：清单里的每个 `endpoint` 必须在 `openapi.yaml` 中存在；manifest 的 `modules` 必须与各组件 `Hooks.capabilities` 完全一致 | 从源头杜绝「前端显示但后端没有」的漂移 |
| **D-M6-10** | 组件离线时的行为 | 卡片按 `/healthz` 显示状态灯；离线组件**不隐藏**、但面板显示「服务未启动」且禁用写操作 | 元素是否出现由「后端引用」决定，不由「此刻是否在线」决定，避免布局跳动 |

---

## 3. 数据流

```
Rust 源码（唯一事实源）
  ├─ common/component::standard_routes()      → core.* 六个模块
  ├─ 每个组件 Hooks.routes()                  → <prefix>.* 专有模块
  └─ 每个组件 Hooks.capabilities.libs         → 是否引用 common-probe 等共享库
            │
            │ 编译期：capabilities::to_json() 生成静态 JSON
            ▼
  ① 组件二进制内嵌 HTML：CONSOLE_HTML.replace("__CAPABILITIES__", json)
  ② Shell：components.yaml 的 modules 同样注入 __SHELL_DATA__
            │
            │ CI：verify_capabilities.py 比对 ① ② 与 openapi.yaml
            ▼
  浏览器加载统一控制台
            │
            ├─ 无网络请求即可拿到清单（离线可用）
            ├─ renderCapabilities(caps) → 按 modules 生成 导航 / 区域 / 面板 / 控件
            └─ 数据请求按 panel.endpoint 发出
                    │
                    ├─ 同源直连（组件自己的页面）：/api/...
                    └─ Shell 聚合页：/p/<name>/api/... → 127.0.0.1:<port>/api/...
```

三条硬规则：

1. **单向**：后端声明 → 前端渲染；前端不得凭空补模块、不得硬编码兜底面板。
2. **编译期确定**：清单不是运行时探测出来的，组件不在线时元素依然在（只是数据为空），保证「引用的模块」与「显示的元素」一一对应。
3. **零新增统一端点**：清单走 HTML 注入，不占 `/api/*`，18 端点生态门禁不受影响。

---

## 4. 数据结构

### 4.1 `contracts/capabilities.schema.json`（要点）

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Lan Studio Capabilities",
  "type": "object",
  "required": ["schema", "component", "runtime", "libs", "modules"],
  "additionalProperties": false,
  "properties": {
    "schema":   { "type": "integer", "const": 1 },
    "component":{ "type": "string", "pattern": "^lan-[a-z0-9-]+$" },
    "runtime":  { "type": "string", "enum": ["common-component", "standalone"] },
    "libs":     { "type": "array", "items": { "type": "string" }, "uniqueItems": true },
    "modules":  { "type": "array", "minItems": 1, "items": { "$ref": "#/definitions/module" } }
  },
  "definitions": {
    "module": {
      "type": "object",
      "required": ["id", "title", "icon", "area", "panels"],
      "additionalProperties": false,
      "properties": {
        "id":    { "type": "string", "pattern": "^(core|lib)\\.[a-z0-9-]+$|^lan-[a-z0-9-]+\\.[a-z0-9-]+$" },
        "title": { "type": "string", "minLength": 1, "maxLength": 40 },
        "icon":  { "type": "string", "minLength": 1, "maxLength": 40 },
        "tone":  { "type": "string", "enum": ["blue","indigo","green","cyan","purple","amber","teal","red"] },
        "area":  { "type": "string", "enum": ["workspace", "feature"] },
        "requires_lib": { "type": "string" },
        "panels":{ "type": "array", "minItems": 1, "items": { "$ref": "#/definitions/panel" } }
      }
    },
    "panel": {
      "type": "object",
      "required": ["id", "widget", "endpoint"],
      "additionalProperties": false,
      "properties": {
        "id":      { "type": "string" },
        "title":   { "type": "string" },
        "widget":  { "type": "string", "enum": ["kv-grid","table","form","code-editor","metric-grid","log-list","text-view","action-list"] },
        "endpoint":{ "$ref": "#/definitions/endpoint" },
        "columns": { "type": "array", "items": { "type": "object" } },
        "actions": { "type": "array", "items": { "$ref": "#/definitions/action" } }
      }
    },
    "endpoint": {
      "type": "object",
      "required": ["method", "path"],
      "additionalProperties": false,
      "properties": {
        "method": { "type": "string", "enum": ["GET","POST"] },
        "path":   { "type": "string", "pattern": "^/api/" },
        "write":  { "type": "boolean" }
      }
    },
    "action": {
      "type": "object",
      "required": ["id", "label", "method", "path"],
      "additionalProperties": false,
      "properties": {
        "id":     { "type": "string" },
        "label":  { "type": "string" },
        "method": { "type": "string", "enum": ["GET","POST"] },
        "path":   { "type": "string", "pattern": "^/api/" },
        "write":  { "type": "boolean" },
        "fields": { "type": "array", "items": { "type": "object" } }
      }
    }
  }
}
```

### 4.2 `lan-hosts` 完整示例（节选）

```json
{
  "schema": 1,
  "component": "lan-hosts",
  "runtime": "common-component",
  "libs": ["common-component"],
  "modules": [
    { "id": "core.config", "title": "配置管理", "icon": "sliders", "tone": "indigo", "area": "workspace",
      "panels": [{ "id": "config.editor", "widget": "code-editor",
        "endpoint": { "method": "GET", "path": "/api/config" },
        "actions": [
          { "id": "save",   "label": "校验并保存", "method": "POST", "path": "/api/config", "write": true },
          { "id": "reload", "label": "重新载入",   "method": "POST", "path": "/api/reload", "write": true }
        ] }] },
    { "id": "lan-hosts.mappings", "title": "静态映射", "icon": "hosts", "tone": "blue", "area": "feature",
      "panels": [{ "id": "hosts.table", "title": "映射清单", "widget": "table",
        "endpoint": { "method": "GET", "path": "/api/hosts/mappings" },
        "columns": [ {"key":"name","label":"主机名"}, {"key":"ip","label":"地址"}, {"key":"comment","label":"备注"} ],
        "actions": [
          { "id": "upsert", "label": "新增/更新", "method": "POST", "path": "/api/hosts/mapping", "write": true,
            "fields": [ {"key":"name","label":"主机名","type":"text","required":true},
                        {"key":"ip","label":"地址","type":"text","required":true} ] },
          { "id": "remove", "label": "删除", "method": "POST", "path": "/api/hosts/remove", "write": true,
            "fields": [ {"key":"name","label":"主机名","type":"text","required":true} ] },
          { "id": "publish","label":"原子发布","method":"POST","path":"/api/hosts/publish","write":true }
        ] }] },
    { "id": "lan-hosts.render", "title": "渲染与发布", "icon": "terminal", "tone": "teal", "area": "feature",
      "panels": [{ "id": "hosts.render", "widget": "text-view",
        "endpoint": { "method": "GET", "path": "/api/hosts/render" } }] }
  ]
}
```

### 4.3 八组件模块矩阵（由现有 `Hooks.routes` 逐条映射）

| 组件 | runtime | libs | 专有模块（端点） |
| --- | --- | --- | --- |
| `lan-mdns` | `standalone` | — | `records`(/api/records)、`browse`(/api/browse + /api/browse/unified)、`conflicts`(/api/conflicts)、`probe`(/api/probe)、`gateway`(/api/health/targets)、`interfaces`(/api/interfaces)、`queries`(/api/queries) |
| `lan-relay` | `common-component` | — | `stats`(/api/relay/stats)、`interfaces`(/api/relay/interfaces)、`inject`(/api/relay/inject + mark-local) |
| `lan-ca` | `common-component` | — | `root`(/api/ca/root)、`certs`(/api/ca/certificates + issue + renew + revoke + verify)、`crl`(/api/ca/crl)、`audit`(/api/ca/audit + export) |
| `lan-scan` | `common-component` | — | `status`(/api/scan/status)、`results`(/api/scan/results + snapshot)、`start`(/api/scan/start) |
| `lan-hosts` | `common-component` | — | `mappings`(/api/hosts/mappings + mapping + remove + import)、`render`(/api/hosts/render)、`publish`(/api/hosts/publish + status) |
| `lan-wol` | `common-component` | **`common-probe`** | `devices`(/api/wol/devices + device + remove)、`wake`(/api/wol/wake)、`probe`(/api/wol/probe)、`history`(/api/wol/history) |
| `lan-sync` | `common-component` | — | `nodes`(/api/sync/nodes + node + remove)、`transfer`(/api/sync/pull + push + confirm)、`history`(/api/sync/history + status) |
| `lan-ssdp` | `common-component` | — | `devices`(/api/ssdp/devices + snapshot)、`refresh`(/api/ssdp/refresh)、`status`(/api/ssdp/status) |

所有 `common-component` 组件共享 `core.status / core.config / core.observe / core.logs / core.alerts / core.history` 六个模块；`lan-mdns` 作为 `standalone` 通过**同一形状的清单**声明自己具备的 core 模块（不迁移实现）。

**`requires_lib` 的用途**：`lib.probe-state` 模块带 `"requires_lib": "common-probe"`，渲染器只在 `caps.libs` 含 `common-probe` 时实例化它 —— 这是「后端引用的模块决定页面元素」最字面的落点。目前只有 `lan-wol` 会出现电源状态面板。

### 4.4 一致性规则（CI 强制）

1. 清单中每个 `endpoint.path` / `action.path` 必须出现在 `contracts/openapi.yaml`。
2. `write: true` 的端点必须与 `route_write(...)` 注册一致（否则写操作会被 409 `setup_required` 挡住而前端却显示按钮）。
3. `shell/components.yaml` 每项的 `modules` 必须与对应组件 `Hooks.capabilities.modules[].id` 集合**完全相等**。
4. 每个组件必须有 `contracts/examples/capabilities.<name>.sample.json`，通过 schema 校验。
5. 清单里出现但注册表没有的 `widget` / `icon` → 校验失败（防止前端渲染空壳）。

---

## 5. 前端视图

### 5.1 布局

统一控制台仍用 Calm Console 设计令牌（固定实色侧栏、1px 边框、沉稳靛蓝、圆角矩形彩色图标、大图标左 + 两行文字右对齐），但侧栏内容不再写死：

```
┌──────────────┬──────────────────────────────────────────────┐
│ Lan Studio   │ 组件切换器（8 个组件，来自 manifest）          │
│              │──────────────────────────────────────────────│
│ WORKSPACE    │ 当前组件：lan-hosts   [在线]                  │
│ · 状态总览   │──────────────────────────────────────────────│
│ · 配置管理   │ 由 capabilities.modules 生成的面板（表格/表单/  │
│ · 可观测     │ 代码编辑器/指标卡/日志/动作按钮）              │
│ · 日志       │                                              │
│ · 告警       │ 未引用的模块：DOM 中不存在、导航中不存在       │
│ · 版本历史   │                                              │
│──────────────│                                              │
│ COMPONENT    │                                              │
│ · 静态映射   │                                              │
│ · 渲染与发布 │                                              │
└──────────────┴──────────────────────────────────────────────┘
```

切换组件 = 换一份 capabilities（Shell 聚合页内切换无需刷新页面，见 §6.3）；组件自己 serve 的页面只有一份清单，不显示切换器。

### 5.2 渲染器核心（可直接落地的骨架）

```js
// common/console/src/render.js —— 注册表是全集，实例化只认 capabilities
const WIDGETS = {
  'kv-grid':      renderKvGrid,
  'table':        renderTable,
  'form':         renderForm,
  'code-editor':  renderCodeEditor,
  'metric-grid':  renderMetricGrid,
  'log-list':     renderLogList,
  'text-view':    renderTextView,
  'action-list':  renderActionList
};

function eligible(caps, mod) {
  // 规则 1：共享库引用决定通用模块是否出现
  if (mod.requires_lib && !caps.libs.includes(mod.requires_lib)) return false;
  // 规则 2：面板端点必须存在（防御式，CI 已保证）
  return Array.isArray(mod.panels) && mod.panels.length > 0;
}

function renderCapabilities(caps, root, api) {
  const mods = caps.modules.filter(m => eligible(caps, m));
  const areas = { workspace: [], feature: [] };
  mods.forEach(m => (areas[m.area] || areas.workspace).push(m));

  // 导航：只生成清单里有的条目；未引用模块连 DOM 都不产生
  root.querySelector('#nav').innerHTML = '';
  ['workspace', 'feature'].forEach(area => {
    if (!areas[area].length) return;               // 空区域整段不渲染
    areas[area].forEach(m => {
      const btn = document.createElement('button');
      btn.className = 'ls-nav-item';
      btn.dataset.module = m.id;
      btn.innerHTML = svg(m.icon) + '<span>' + esc(m.title) + '</span>';
      btn.onclick = () => showModule(m, api);
      root.querySelector('#nav').appendChild(btn);
    });
  });

  // 面板容器：同样按清单生成，不预留全集骨架
  const main = root.querySelector('#moduleRoot');
  main.innerHTML = '';
  mods.forEach(m => {
    const section = document.createElement('section');
    section.dataset.module = m.id;
    section.hidden = true;
    m.panels.forEach(p => {
      const widget = WIDGETS[p.widget];
      if (!widget) return;                         // CI 保证不会走到
      const node = document.createElement('div');
      node.className = 'ls-panel';
      node.innerHTML = '<div class="ls-panel-title">' + svg(m.icon) + esc(p.title || m.title) + '</div>';
      const body = document.createElement('div');
      node.appendChild(body);
      section.appendChild(node);
      widget(body, p, api, m);                    // 内部按 p.endpoint / p.actions 发请求
    });
    main.appendChild(section);
  });
  if (mods.length) showModule(mods[0], api);
}

// 统一请求层：同源直连或经 Shell 代理，凭据只在这里注入
function createApi(baseFor) {
  async function call(method, path, write, body) {
    const headers = { 'Content-Type': 'application/json' };
    if (write && currentPassword()) headers['X-Admin-Password'] = currentPassword();
    const r = await fetch(baseFor(path), { method, headers,
      body: body === undefined ? undefined : JSON.stringify(body) });
    const j = await r.json().catch(() => ({ ok: false, error: { message: 'invalid response' } }));
    if (!r.ok || j.ok === false) throw new Error(j.error?.code + ': ' + j.error?.message);
    return j.data;
  }
  return { call };
}
```

`WIDGETS.table` 按 `p.columns` 生成表头、按 `p.actions[].fields` 生成弹窗表单（label 左对齐固定宽度 + 两列网格，长文本跨双列），`write: true` 的按钮在缺密码时仍显示但提交时提示 409 —— 与现有六区域行为一致。

### 5.3 组件切换与离线

- Shell 聚合页：`DATA.components[].capabilities` 随 `__SHELL_DATA__` 注入，切换组件时调 `renderCapabilities()` 重建 DOM，不刷新页面；`baseFor` 把 `/api/...` 映射成 `/p/<name>/api/...`。
- `refresh()` 只更新状态灯，不改变模块可见性（D-M6-10）。
- 组件未启动：面板显示「服务未启动」，`write` 按钮禁用，**导航项保留**。

---

## 6. 后端改动点

### 6.1 `common/component`：能力声明与注入

```rust
// common/component/src/capabilities.rs（新增）
#[derive(Clone, Copy)]
pub struct ModuleSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub tone: &'static str,
    pub area: &'static str,
    pub requires_lib: &'static str,      // "" 表示无依赖
    pub spec: &'static str,              // 该模块 panels/actions 的静态 JSON 片段
}

#[derive(Clone, Copy)]
pub struct Capabilities {
    pub runtime: &'static str,
    pub libs: &'static [&'static str],
    pub modules: &'static [ModuleSpec],
}

pub const EMPTY: Capabilities = Capabilities { runtime: "common-component", libs: &[], modules: &[] };

pub fn to_json(component: &str, caps: &Capabilities) -> String {
    // 手写 JSON 拼接（零第三方 crate），转义走 common_api::json_quote
    // 输出 {"schema":1,"component":...,"runtime":...,"libs":[...],"modules":[...]}
    ...
}
```

`Hooks` 增一个字段（保持 `Copy`，因为都是 `&'static`）：

```rust
pub struct Hooks {
    ...
    pub capabilities: Capabilities,      // 新增
}
```

七个 `common-component` 组件各自在 `const HOOKS: Hooks = Hooks { ... }` 里补声明；`standard_routes` 对应的 `core.*` 六个模块由 `capabilities::core_modules()` 统一返回，组件只需写专有模块，避免八份重复。

`view_index`（`common/component/src/lib.rs:542`）改为：

```rust
fn view_index(_req: &Request, _ctx: &Context) -> Response {
    let rt = RUNTIME.get().expect("runtime installed");
    let json = capabilities::to_json(rt.hooks.name, &rt.hooks.capabilities);
    Response::html(200, CONSOLE_HTML.replace("__CAPABILITIES__", &json))
}
```

> 落地前需确认 `Runtime` 是否持有 `hooks`（`Runtime::new(hooks, ...)`），若未持有则加一个 `OnceLock<Hooks>` 或直接把 `hooks: Hooks` 存进 `Runtime`。

### 6.2 `lan-mdns`：standalone 清单

`Hooks` 不适用于 M2（它不走 `common-component::run`）。在 `components/lan-mdns/src/main.rs` 的 `view_index` 里同样注入一份静态 `CAPABILITIES_JSON`（由 `contracts/examples/capabilities.lan-mdns.sample.json` 编译期 `include_str!` 生成），**不改任何既有端点与响应形状**。

### 6.3 Shell：同源反向代理（D-M6-1 = A 时）

`shell/components.yaml` 每项增加 `modules: [...]`（与组件清单 id 集合一致），`manifest_data_json()` 一并输出 `capabilities`。

`common-api::App` 需要一处**受控扩展**（精确匹配优先、前缀兜底，不改既有路由语义）：

```rust
// common/api/src/lib.rs
pub fn route_prefix(mut self, method: &'static str, prefix: &'static str, handler: Handler) -> Self { ... }

pub fn dispatch(&self, req: &Request, ctx: &Context) -> Response {
    for route in &self.routes {
        if route.exact && route.method == req.method && route.path == req.path { return (route.handler)(req, ctx); }
    }
    for route in &self.routes {
        if !route.exact && route.method == req.method && req.path.starts_with(route.path) { return (route.handler)(req, ctx); }
    }
    err(req, "not_found", "unknown endpoint", 404)
}
```

`shell/src/main.rs` 注册：

```rust
let app = App::new()
    .route("GET", "/", view_index)
    .route("GET", "/healthz", view_healthz)
    .route("GET", "/api/shell/status", view_status)
    .route_prefix("GET",  "/p/", view_proxy)
    .route_prefix("POST", "/p/", view_proxy);
```

`view_proxy` 边界（必须写死，防止变成通用网关）：

```rust
fn view_proxy(req: &Request, ctx: &Context) -> Response {
    // 1. 鉴权：/p/ 不触发 handle_conn 的 /api/ 检查，这里显式调用 common_auth::check
    //    写操作再调 common_auth::check_write（AdminToken 透传，密码不缓存）
    // 2. 解析 /p/<name>/<rest>：name 必须命中 manifest，否则 404 not_found
    // 3. rest 必须以 "api/" 开头，且只转发 manifest 里该组件声明过的 endpoint.path（白名单）
    //    未声明路径一律 404，杜绝任意路径转发
    // 4. 目标恒为 127.0.0.1:<manifest port>，不允许从请求体/头指定 host
    // 5. 超时 300ms、响应体 1MiB 上界、不跟随重定向、不透传 Set-Cookie
    // 6. 用 std::net::TcpStream 手写 HTTP/1.1 转发，零第三方 crate（可参考 lan-sync 的客户端）
    ...
}
```

> 这会修订 ADR 0007 D-M5-3「不代理请求」的**范围**：仍不做服务发现、不代管密码、不聚合业务数据，只做「同源、白名单、有界」的 API 转发。需新增 `docs/ADR/0008-m6-frontend-aggregation.md` 登记。

### 6.4 Shell 组件离线时的清单

能力清单来自 `components.yaml` 静态声明，不依赖组件在线；组件离线只影响数据请求与状态灯。CI 用 `verify_capabilities.py` 保证静态声明与 Rust 源码不漂移。

---

## 7. 实施分批

| 批次 | 主题 | 交付 | 验收 |
| --- | --- | --- | --- |
| **M6.0** | 契约与骨架 | `contracts/capabilities.schema.json` + 8 份 sample、`Hooks.capabilities`、`capabilities::to_json`、8 个组件清单声明、`shell/components.yaml` 增 `modules`、`scripts/verify_capabilities.py` | schema 校验通过；每个 endpoint 在 openapi 中存在；manifest 与各组件声明一致 |
| **M6.1** | 共享前端源码 | `common/console/`（渲染器 + 8 类 widget + 统一请求层 + Calm Console 令牌）、单文件产物 | `npm run verify`：typecheck、零外部资源、markers 断言 |
| **M6.2** | 组件内嵌页 | `common/component` 与 `lan-mdns` 的 `view_index` 注入清单，产物替换 `console.html` | 8 个组件 `--check` 通过；各自页面只显示自己声明的模块 |
| **M6.3** | Shell 聚合入口 | `common-api::route_prefix`、`view_proxy` 白名单转发、组件切换器、状态灯 | 聚合页能在一个页面内切换 8 个组件；未声明路径 404；超时/体积上界生效 |
| **M6.4** | M2 Arco 挂载（D-M6-2 默认） | 统一壳里 `lan-mdns` 模块挂载现有 Arco 页（iframe 同源嵌入或构建期合并，二选一待细化） | M2 66/66 与前端产物门禁不回退 |
| **M6.5** | 门禁与销项 | CI 加入 `verify_capabilities.py` + `test_m6.py`、ADR 0008、README 与目录同步、`docs/M6-CHECKLIST.md` | fmt/clippy/test 全绿、既有 M2~M5 门禁不回退、生态 91/0 保持 |

---

## 8. 门禁与验收

**不回退**（必须全部保持）：

```
cargo fmt --all -- --check              0 diff
cargo clippy --workspace --all-targets -- -D warnings   0 warning
cargo test --workspace                 198 passed 起，只增不减
verify_contracts.py                    24 / 0（新 schema 加入后 +2）
test_api_contract.py                   75 / 0
test_m3.py / test_m4.py               66/66、76/76
test_m5.py / test_ecosystem.py        91 / 0、91 / 0（18 统一端点不变）
npm run verify（M2 webui）            RENDER OK
cargo build --workspace                9 binaries
```

**新增**：

| 门禁 | 判定 |
| --- | --- |
| `scripts/verify_capabilities.py` | 8 份 sample 通过 schema；所有 endpoint 在 openapi 中；`write` 标记与 `route_write` 一致；manifest `modules` 与源码一致；widget/icon 均在注册表 |
| `scripts/test_m6.py` | 逐组件断言「清单里没有的模块，DOM 里也不存在」，含负向断言（例如 `lan-hosts` 页面不得出现 `/api/wol/` 相关控件） |
| 代理安全 | 白名单外的路径 404；目标 host 不可由请求指定；超时与体积上界生效；`/p/` 路径同样受 AdminToken 保护 |
| 前端产物 | 单文件、零外链、深浅主题、内联 SVG 图标 |

---

## 9. 风险与边界

- **ADR 0007 D-M5-3 冲突**：代理只在 D-M6-1 = A 时引入，需 ADR 0008 明确「同源、白名单、有界、不代管凭据」，并在 README 更新该边界描述。
- **M2 异质前端**：838 kB Arco 产物与原生控制台短期并存；D-M6-2 选「完全重写」会动 66/66 验收，需单独立项评估。
- **`common-api` 改动面**：`route_prefix` 影响所有二进制的路由匹配，必须保证精确匹配优先、`is_write` / `route_label` 同步处理前缀路由，否则指标 `path` 标签会出现高基数（PRD §4 禁止无界标签）→ 前缀路由统一用 `/p/` 作为 label。
- **零外部资源**：新增前端仍走 Vite `vite-plugin-singlefile`，不引 CDN、不引图标字体。
- **不在本轮范围**：组件事件总线、跨组件数据聚合、Shell 服务发现、`install.sh` 多组件安装（保持 ADR 0007 遗留边界）。

---

## 10. 待拍板（确认后开工）

| # | 问题 | 选项 |
| --- | --- | --- |
| ★1 | 聚合入口形态 | **A 统一 SPA + Shell 同源代理（推荐）** / B 各组件内嵌同一份壳、Shell 只做导航 / C 浏览器跨域直连（8 组件加 CORS） |
| ★2 | M2 Arco 前端 | **保留并挂载（推荐）** / 完全重写为共享壳 / 本轮不动、留到 M6.4 之后 |
| ★3 | 清单粒度 | **五层 component→area→module→panel→action/field（推荐）** / 只到 module 层（前端手写面板） |

未拍板前不落地代码；本文件与 README 的指引更新是唯一改动。
