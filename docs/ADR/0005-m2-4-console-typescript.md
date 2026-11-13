# ADR 0005 · M2.4 离线管理台：TypeScript 与单文件产物

- 状态：已实施（2026-11-13）
- 影响范围：`components/lan-mdns/webui/`（新增）、`common/webui/`（产物与单测）、
  `common/net`（默认出口网卡探测）、`main.rs`（`/api/config` 投影、`/api/interfaces`）、
  `scripts/smoke_mdns.py`、CI
- 关联契约：`contracts/openapi.yaml`、`contracts/config.schema.json`、PRD 3.7 / 5-C / 5-D / 5-E / 5-H
- 前置：ADR 0003（可观测与安全）、ADR 0004（告警与网关）

## 1. 背景

M1~M3 期间 `common/webui/src/assets/index.html` 一直是占位页。M2.4 要把它换成交付级
管理台，同时销项 `F-A12`（多网卡绑定）、`F-D04`（健康状态灯）、`F-E04`（在线配置
表单 + 源码）、`F-H05`（辅助跳转入口）、`F-J03`（API 契约测试）。

三条硬约束决定实现边界：

1. **单文件、离线**：产物必须自包含，运行时不读任何外部文件或 CDN（PRD 3.7 / F-I01）；
2. **零第三方 crate**（ADR 0001）：后端不引入模板引擎或内嵌静态资源框架；
3. **PRD 1.4 红线**：管理台不能变成「流量前置」，`/go/` 只是辅助跳转。

## 2. 决策

### D1 · 前端用 TypeScript，仓库内不保留 `.js` 源文件

管理台要对着 22 个端点手写渲染逻辑，字段错一个就是空白页。TypeScript 把契约变成
编译期检查：

```ts
// src/types.ts —— 与 openapi.yaml 信封 / ERROR-CODES.md / main.rs 视图逐字段对齐
export interface StatusData { /* ... */ }
export type ApiErrorCode =
  | 'invalid_config' | 'auth_required' | 'auth_failed' | 'setup_required'
  | 'not_found' | 'target_unreachable' | 'rate_limited' | 'internal_error'
```

- `tsconfig.json` 开 `strict` + `noUncheckedIndexedAccess` + `noUnusedLocals` +
  `noUnusedParameters` + `noImplicitReturns` + `verbatimModuleSyntax`，并 `allowJs: false`；
- `npm run typecheck`（`vue-tsc --noEmit`）是 `npm run build` 的前置，也是 CI 独立 job；
- 所有 `<script setup>` 都带 `lang="ts"`，`defineProps` / `defineEmits` 用类型参数
  而非运行时对象，模板里的 `$event` 因此有真实类型。

不用 JavaScript 的代价是多一层工具链（`typescript` + `vue-tsc`），换来的是
「后端改了字段、前端构建直接红」，这比线上点开一个空白页便宜得多。

### D2 · 产物形态：Vite + `vite-plugin-singlefile`，提交构建结果

`vite.config.ts` 设 `cssCodeSplit: false`、`assetsInlineLimit` 极大值，并把 `outDir`
直接指向 `common/webui/src/assets`。`npm run build` 覆盖被提交的 `index.html`，
`common-webui` 用 `include_str!` 把它编进二进制。

代价：仓库里存在一个约 800 KB 的构建产物，且**改前端必须重新构建后一起提交**。
收益：`cargo build` 不需要 Node，CI 的 Rust job 也不需要 Node，发布产物仍是单文件。

`common/webui` 的单测不再简单断言「不含 `http://`」——SVG/MathML 命名空间与文档
链接本来就会命中。改为断言**不出现被拉取的资源**：

```rust
const FORBIDDEN: [&str; 8] = [
    "<script src=", "<link rel=\"stylesheet\"",
    "src=\"http://", "src=\"https://", "href=\"http://", "href=\"https://",
    "url(http://", "url(https://",
];
```

CI 另加 `webui` job：`npm ci` → `typecheck` → `vite build` → 对产物跑同一组断言。

### D3 · 表单模式与源码模式共享一份 YAML 子集实现

`src/config-text.ts` 用 `flatten()` / `serializeConfig()` 复刻服务端
`common-config::parse_flat` 的子集语法：块映射、`- key: value` 对象列表、`- scalar`
标量列表、引号标量、`[]` 空列表、`#` 注释。

- 进「源码模式」时若表单有改动，先按表单重生成源码；
- 离开「源码模式」时若源码有改动，先尝试解析回表单，解析失败则保留表单并提示，
  源码不丢；
- 表单保存会重写为标准格式（注释不保留），源码保存原样提交。

已知取舍：`server.admin_token` 服务端只返回 `admin_token_set` 布尔值，因此表单模型
不是 `StructuredConfig` 的投影，而是独立的 AST——否则一保存就会把 Token 抹掉。

### D4 · `F-A12` 明确「需重启」，不做假热重载

网卡集合变更会重建组播 socket，`common-net` 的 `MdnsSocket` 在 `bind()` 时预置
`SO_REUSEADDR` / `SO_REUSEPORT` 并入组。因此：

- `/api/interfaces` 返回 `mode` / `selected` / `default_interface` / `joined` /
  `restart_required: true`；
- 管理台「网卡绑定」页顶部直接写明「保存后必须重启进程才生效」；
- 新增 `common_net::default_interface()`（`ip -4 route show default` 解析 `dev` 字段），
  供 `auto` 模式标出「实际参与的网卡」，而不是把所有网卡都标绿。

### D5 · 轮询与 SSE 都受控，避免刷新抖动

`usePolling` 在 `document.hidden` 或页面未激活时不发请求，并且同一时刻只允许一次在途
请求。所有列表刷新只替换数据，不触发入场动画（M2.2 已确定的防抖动规则）。

`/api/logs/stream` 由后端强制上界（`max_duration` 30s / `max_events` 200），前端用
原生 `EventSource` 自动重连，不自己实现退避。

### D6 · 历史页的回滚走 `onOk`，不用返回值

`@arco-design/web-vue` 的 `Modal.confirm()` 返回 `ModalReturn`（`close` / `update`），
不是 Promise。因此回滚动作挂在 `onOk` 上，取消对话框绝不会触发写操作——不能用
`if (await Modal.confirm(...) !== 'ok')` 这种想当然的写法。

### D7 · 用 jsdom 做渲染门禁，而不是浏览器

`vue-tsc` 只能证明类型正确，证明不了页面画得出来。`scripts/render-check.mjs` 把产物
里的内联 bundle 抽出来，在 jsdom 里求值，断言六个 tab、面板、状态灯与 KPI 单元都渲染
出来、且控制台零报错。

不用无头 Chrome：它依赖 ptrace / sandbox 权限，在受限容器与本机内核策略下会直接
SIGTRAP（实测 `ptrace: Operation not permitted`），CI 里更不可靠。jsdom 只需 Node，
代价是少数 DOM API 要打桩（`ResizeObserver` / `matchMedia` / `EventSource`）。

这个门禁立刻抓到第一个真实缺陷：`LogsPage` 在 `onMounted` 里直接 `new EventSource()`，
缺少该 API 的环境会抛错。虽然 Vue 仍渲染出其它面板，但这属于「一个可选能力缺失拖垮
一个页面」。已改为能力探测 + 降级提示（「请用『载入历史』手动刷新」）。

## 3. 后果

- 前端成为独立工具链（Node 22 + npm），但**不影响** `cargo build` 与发布产物；
- `common/webui/src/assets/index.html` 从 1 KB 占位页变成约 800 KB 的完整控制台，
  二进制从 1.55 MB 增至 2.38 MB（仍为单文件）；
- 服务端顺带补齐 `/api/config` 的结构化投影（`config_model_json`）与
  `/api/interfaces` 的 `joined` / `mode` / `restart_required`，这两个端点此前只有
  半成品形态；
- `F-J03`（API 契约运行态测试）仍需单独脚本，见 `docs/M2-CHECKLIST.md`。

## 4. 验收

| 门禁 | 结果 |
| --- | --- |
| `npm run typecheck` | 0 error（strict 全开） |
| `npm run check:render` | 12/12 标记、14 tab、10 面板、14 状态灯、25 KPI、0 控制台报错 |
| `npx vite build` | 单文件 803,968 字节，无外部引用 |
| `cargo test --workspace` | 79 passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `scripts/verify_contracts.py`（完整） | 21 passed |
| `scripts/smoke_mdns.py` | 229 passed（新增 8 项控制台 / 网卡断言） |
| 管理台运行态 | `GET /` 200 + `text/html`，含 `#app`、内联样式、六个区域标题 |
