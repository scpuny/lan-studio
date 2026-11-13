# lan-mdns 控制台（前端）

离线单文件管理台：**Vue 3.5 + TypeScript（strict）+ Arco Design Vue**，由 Vite 打包成
一个自包含 HTML，编译期经 `common-webui` 的 `include_str!` 嵌入二进制。运行时不需要
任何外部静态文件、CDN 或网络（PRD 3.7 / F-I01）。

## 为什么是 TypeScript

管理台要对着 22 个 API 端点手写渲染逻辑，字段错一个就是空白页。`src/types.ts` 把
`contracts/openapi.yaml` 的信封 / 错误码与 `components/lan-mdns/src/main.rs` 各视图
处理器的实际输出逐字段写成 `interface`，`npm run typecheck`（`vue-tsc --noEmit`）
是强制门禁——**仓库内不存在 `.js`/`.jsx` 源文件**（`tsconfig.json` 里 `allowJs: false`）。

严格项全部打开：`strict`、`noUncheckedIndexedAccess`、`noUnusedLocals`、
`noUnusedParameters`、`noImplicitReturns`、`noFallthroughCasesInSwitch`、
`verbatimModuleSyntax`、`isolatedModules`。

## 目录

```
webui/
├── index.html              # 唯一入口，挂载 #app
├── vite.config.ts          # singlefile + 输出到 common/webui/src/assets
├── tsconfig.json           # strict 全开，include 覆盖 src/** 与 vite.config.ts
└── src/
    ├── env.d.ts            # vite/client + Arco 的 GlobalComponents 声明
    ├── main.ts             # createApp + 按需注册 Arco 组件
    ├── App.vue             # 顶栏、6 个一级区域、管理员凭据弹窗
    ├── api.ts              # 信封解包、ApiError、会话凭据
    ├── types.ts            # 唯一的线上契约类型来源
    ├── defaults.ts         # 空态工厂（避免共享可变默认值）
    ├── poll.ts             # 可见性感知轮询 + 时间/字节格式化
    ├── config-text.ts      # YAML 子集解析与序列化（与服务端 parse_flat 对齐）
    ├── styles/app.css      # 玻璃拟态、状态灯、代码块
    └── pages/
        ├── OverviewPage.vue  # 状态灯 / 协议计数 / 健康目标 / 服务导航（F-D04、F-H05）
        ├── ConfigPage.vue    # 表单 + 源码双模式、网卡绑定、安全、告警（F-E04、F-A12）
        ├── ObservePage.vue   # 服务地图 / 查询审计 / 冲突 / 记录 / 指标
        ├── LogsPage.vue      # SSE 实时日志 + 级别与关键字过滤
        ├── AlertsPage.vue    # 队列、发送结果、平台适配、网关状态
        └── HistoryPage.vue   # 快照列表、回滚、导入/导出（F-E06、F-E07、F-E08）
```

## 命令

```bash
npm ci                # 首次
npm run dev           # 本地开发（代理到后端见下）
npm run typecheck     # vue-tsc --noEmit，CI 门禁
npm run build         # typecheck + vite build → ../../../common/webui/src/assets/index.html
```

`npm run build` 会**覆盖**被提交的 `common/webui/src/assets/index.html`。改过任何
`src/` 文件后必须重新构建并一起提交，否则二进制里仍是旧页面（`common-webui` 的单测
会对该产物断言「自包含」）。

本地联调时后端不在 5173，直接 `vite` 起前端会 404；要么开后端并把 `server.proxy`
指向它，要么用后端自带的 `GET /`（构建后的产物）联调。生产路径只有一条。

## 硬约束

- **零外部资源**：不得出现 `<script src=` / `<link rel="stylesheet"` / `url(http...)`。
  `common/webui/src/lib.rs` 与 CI 的 `webui` job 都会拦截。
- **不引入前端路由**：六个区域用 Arco Tabs 承载，刷新不换 URL，避免离线环境下
  history 模式 404。
- **刷新不做入场动画**：轮询更新只改数据，不触发过渡（PRD 3.7 防抖动）。
- **凭据只在会话内**：`sessionStorage`，不写 cookie、不落盘；所有写操作走
  `X-Admin-Password`，由后端统一返回 `409 setup_required` / `403 auth_failed`。
- **表单与源码同一份文档**：`config-text.ts` 的解析/序列化必须与服务端
  `common-config::parse_flat` 保持一致，改一边就要改另一边。
