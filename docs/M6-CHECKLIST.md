# M6 销项清单：前端聚合（能力清单驱动）

> 依据 `docs/M6-PLAN.md`（范围）、`docs/ADR/0008-m6-frontend-aggregation.md`（决策）与用户拍板记录。
> 本文是 M6 **唯一销项依据**；第 9 节为实测门禁数字。M6 是工程演进项，**不改变 PRD §7.5 的 M0~M5 里程碑状态**。

---

## 1. 范围

| 项 | 内容 |
| --- | --- |
| 交付物 | 一份能力清单契约 + 共享渲染器 + 组件内嵌页 + Shell 统一 SPA + 同源白名单代理 |
| 硬约束 | 页面元素由**后端实际引用的模块**决定；未引用模块不产生 DOM |
| 不回退 | 18 个统一端点、M2 66/66、M2~M5 全量门禁、生态 §8.2 六条 |
| 不做 | 第 9 个组件、组件事件总线 / 数据总线、Shell 服务发现、Shell 代管管理员密码、重写 M2 Arco 页 |

## 2. 批次划分

| 批次 | 主题 | 状态 |
| --- | --- | --- |
| **M6.0** | 能力清单契约：schema、8 份 sample、allowlist、生成器、组件注入 | **done** |
| **M6.1** | 共享渲染器：8 类 widget、导航/面板/动作表单、组件内嵌页 | **done** |
| **M6.2** | Shell 聚合 SPA：`route_prefix`、白名单代理、组件切换器、M2 Arco 挂载 | **done** |
| **M6.3** | 门禁与文档：`test_m6.py`、ADR 0008、README / CI / CHECKLIST 同步 | **done** |
| **M6.4** | 全量门禁与提交 | **done** |

## 3. 能力清单（后端 → 前端唯一契约）

| 文件 | 作用 |
| --- | --- |
| `contracts/capabilities.schema.json` | 五层结构 `component → area → module → panel → endpoint/action/field`，`additionalProperties: false` |
| `contracts/examples/capabilities.<name>.sample.json` × 8 | 每个组件声明的模块、面板、widget、动作与字段 |
| `contracts/capabilities.allowlist.txt` | `name\|path` 行，供 Rust 代理做端点级白名单，无需 JSON 解析器 |
| `scripts/gen_capabilities.py` | 从各组件 `main.rs` 的 `.route()` / `.route_write()` 反向派生上述文件；`--check` 校验漂移 |

一致性规则（`--check` 强制执行）：

1. 清单中每个 `endpoint.path` / `action.path` 必须存在于 `contracts/openapi.yaml`。
2. 非 core 模块的路径集合必须与源码注册的专有端点集合**完全相等**（不多不少）。
3. `write` 标记从 `route_write()` 派生，不由前端手写；`route` 的 POST 端点不带写标记。
4. `shell/components.yaml` 每个组件的 `modules` 集合与对应清单的 module id 集合完全相等。
5. `requires_lib` 指向的库必须出现在 `libs` 中；widget / icon / tone 均在注册表内。
6. 8 份 sample 全部通过 `capabilities.schema.json`（`verify_contracts.py` 34 项检查中的 9 项）。

**`requires_lib` 的语义**：`lan-wol.probe` 带 `requires_lib: "common-probe"`，只有 `libs` 含该库的组件才实例化这个模块。当前 8 个组件里只有 `lan-wol` 引用 `common-probe`，因此「电源状态探测」面板只出现在它的页面上 —— 这是「后端引用的模块决定页面元素」最直接的落点。

## 4. 前端视图

| 位置 | 实现 |
| --- | --- |
| `common/component/src/assets/console-renderer.js` | 唯一渲染器源码；8 类 widget 注册表 + 统一请求层 + 动作表单；`render(caps, opts)` 只实例化清单中的模块 |
| `common/component/src/assets/console.html` | 组件内嵌页：保留成熟的六区域 DOM，`coreStatic: true` 复用；静态 section 中未被清单声明的会被移除；专有模块动态生成 |
| `common/component/src/capabilities.rs` | `include_str!` 编入 8 份清单；`for_component()` / `all_json()` |
| `common/component/src/lib.rs::view_index` | 注入 `__CAPABILITIES__` 与 `__RENDERER_JS__` |
| `components/lan-mdns/src/main.rs::view_index` | 保留 838 kB Arco 产物，仅注入 `window.__LAN_CAPABILITIES__` |
| `shell/src/assets/shell.html` | 统一 SPA：8 组件切换器 + 动态模块导航 + `#moduleRoot` + 状态灯 + 只读 manifest；`coreStatic: false` 全动态渲染 |
| `shell/src/main.rs::view_index` | 注入 8 份清单 + 同一份渲染器 |

运行时规则：

- 组件内嵌页 `baseFor` 为原路径；Shell 聚合页 `baseFor = '/p/' + name + path`。
- 凭据：DOM 中**没有密码输入框**（M5 门禁断言保持通过），写操作凭据由 `prompt()` 存页面内存，仅以 `X-Admin-Password` 头透传；AdminToken 由 `extraHeaders` 透传。
- 组件离线只影响数据区与状态灯，**不改变模块可见性**（元素由编译期清单决定，不由在线状态决定）。
- `lan-mdns`（`runtime: standalone`）额外挂载 Arco 页 iframe 直连组件端口，是刻意例外，见第 8 节。

## 5. 代理边界（ADR 0008 D-M6-6）

| 约束 | 实现 |
| --- | --- |
| 单一前缀 | `GET|POST /p/<name>/api/...`，裸 `/api/*` 路径仍全部 404 |
| 组件白名单 | `name` 必须命中 `shell/components.yaml` |
| 端点白名单 | `name\|path` 必须精确命中 `capabilities.allowlist.txt` |
| 固定目标 | 恒为 `127.0.0.1:<manifest port>`，不接受请求指定 host；拒绝 `..`、非 `api/` 前缀、长度 > 256 的路径 |
| 鉴权 | `/p/` 不触发 `handle_conn` 的 `/api/` 守卫，代理内显式调用 `common_auth::check` |
| 凭据 | 只转发 `X-Admin-Password` / `X-Admin-Token` / `Content-Type`，不缓存、不落盘、不进日志与快照；写保护由目标组件执行 |
| 有界 | 连接/读/写超时 5s（容纳 debug 构建的管理员密码 KDF）、响应 1 MiB 上界、`Connection: close`、不跟随重定向 |
| 指标 | `common-api::App::route_prefix` 让 `path` 标签统一收敛为 `/p/`，无高基数 |

`common-api` 改动保持精确匹配优先：`dispatch` 先遍历精确路由再遍历前缀路由，`route_label` 同样精确优先，前缀路由的 `write` 恒为 false（由代理自行鉴权）。

## 6. 交付清单

| 类别 | 文件 |
| --- | --- |
| 契约 | `contracts/capabilities.schema.json`、`contracts/capabilities.allowlist.txt`、`contracts/examples/capabilities.*.sample.json` × 8 |
| 脚本 | `scripts/gen_capabilities.py`、`scripts/test_m6.py`；`scripts/verify_contracts.py` 纳入新 schema 与 8 份 sample |
| 共享前端 | `common/component/src/assets/console-renderer.js`、`console.html`、`common/component/src/capabilities.rs`、`lib.rs::view_index` |
| Shell | `shell/src/main.rs`（`route_prefix` / `view_proxy` / `manifest_data_json`）、`shell/src/assets/shell.html`、`shell/Cargo.toml`、`shell/components.yaml` |
| 公共 API | `common/api/src/lib.rs`（`Route.prefix` / `route_prefix` / `dispatch` / `route_label`） |
| 门禁 | `scripts/test_m6.py`、`.github/workflows/ci.yml` |
| 文档 | `docs/M6-PLAN.md`、`docs/M6-CHECKLIST.md`、`docs/ADR/0008-m6-frontend-aggregation.md`、`README.md` |

## 7. 决策登记

见 `docs/ADR/0008-m6-frontend-aggregation.md`：D-M6-1 = A、D-M6-2 = 保留并挂载、D-M6-3 = 编译期 JSON + CI 反向扫描、D-M6-4 = 就地升级 `console.html`（不新建 Vite 工程）、D-M6-5 ~ D-M6-8 = 渲染规则 / 代理边界 / 凭据 / 指标基数。

## 8. 已知边界

- `lan-mdns` 的 Arco 页经 iframe 直连组件端口（跨源但父页面不读取内容），组件离线时该块显示空白；其余 7 个组件与 Shell 全部能力驱动。
- Shell 现在是有界代理层，攻击面大于 M5 的纯导航层；已用双层白名单、固定目标与超时/体积上界收敛，但部署仍应限制在可信内网。
- 管理员密码经浏览器内存 + 请求头透传，未加密存储；依赖同源 / HTTPS 部署。
- `scripts/smoke_mdns.py` 在 macOS 的 10 项环境性失败仍是 M5 登记的已知边界，与 M6 无关。
- `scripts/install.sh` 仍只安装 `lan-mdns`；Arco 产物未重建（M6 未改动 M2 前端源码）。

## 9. 执行记录（2026-11-14）

### 9.1 全量门禁逐项

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | **0 diff** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **0 warning** |
| `cargo test --workspace` | **201 passed / 0 failed**（M5 收尾 198 → +3，来自 `common-component::capabilities` 单测） |
| `python3 scripts/verify_contracts.py` | **34 / 0**（PyYAML 完整模式，含 capabilities schema + 8 份 sample） |
| 降级模式（无 PyYAML） | **25 / 0 + `[DEGRADED: YAML checks skipped]`** |
| `python3 scripts/gen_capabilities.py --check` | **CAPABILITIES OK: 8 manifests match backend routes** |
| `python3 scripts/test_api_contract.py` | **75 / 0**（M2 回归） |
| `python3 scripts/test_m3.py` | **66/66 — M3 RUNTIME OK** |
| `python3 scripts/test_m4.py` | **76/76 — M4 RUNTIME OK** |
| `python3 scripts/test_m5.py` | **91 / 0 — M5 RUNTIME OK**（含「无密码输入框」「裸 `/api/*` 404」） |
| `python3 scripts/test_m6.py` | **152 / 0 — M6 RUNTIME OK** |
| `python3 scripts/test_ecosystem.py` | **91 / 0 — ECOSYSTEM OK** |
| `cd components/lan-mdns/webui && npm run verify` | typecheck OK、markers 12/12、nav 6 / panels 10 / dots 14 / kpi 25、**RENDER OK** |
| `cargo build --workspace` | **9 个二进制**（8 组件 + `lan-shell`） |
| `git diff --check` | 无空白问题 |

### 9.2 `test_m6.py` 覆盖要点

- 静态：生成器与后端路由一致、8 份清单形状正确、**每个组件都不含其它组件的模块 id**、`requires_lib` 只在 `lan-wol` 生效、所有 widget 在注册表内。
- 组件内嵌页：注入 renderer 与自身清单、无占位符、无外链、**不含 `lan-wol.devices`**。
- Shell SPA：8 份清单全部下发、组件切换器与 `#moduleRoot` 存在、无密码输入框、8 个语义 tone、零外链。
- 代理：白名单内 200 且返回统一信封、未声明端点 / 未知组件 / 非 api 前缀 / 裸 `/api/*` 全部 404；**写操作不带密码被拒、带密码经代理透传后成功写入并可读回**。

### 9.3 结论

M6.0 ~ M6.4 全部交付；「前端聚合 + 页面元素由后端引用模块决定」达成且由 CI 强制；M2 / M3 / M4 / M5 / 生态六条门禁**零回退**。**M6 可销项**。
