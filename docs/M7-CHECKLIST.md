# M7 销项清单：lan-webui 自包含统一控制台（前端 + Rust BFF）

> 依据 `docs/M7-PLAN.md`（范围）、`docs/ADR/0009-m7-lan-webui.md`（决策）与用户拍板记录。
> 本文是 M7 **唯一销项依据**；第 9 节为实测门禁数字。M7 是工程演进项，**不改变 PRD §7.5 的 M0~M5 里程碑状态**，且**不回退 M6** 已销项的任何门禁。

---

## 1. 范围

| 项 | 内容 |
| --- | --- |
| 交付物 | 独立子项目 `lan-webui/`：自带 Rust 后端（BFF）+ Vue3/TS/Arco 前端；后端「引入哪些模块」决定前端渲染哪些模块 |
| 硬约束 | 页面元素由 `lan-webui/modules.yaml` 声明的组件集合决定；**未声明模块不产生 DOM**；清单由 `gen_capabilities.py --aggregate` 合成并经 CI 强制，与后端注册漂移即失败 |
| 不回退 | 18 个统一端点、M2 66/66、M3 66/66、M4 76/76、M5 91/0、M6 152/0、生态 91/0、静态契约 34/0、`npm run verify` RENDER OK |
| 不做 | 第 9 个 `lan-*` 组件、把 8 组件业务逻辑单体编译进本二进制、组件事件总线 / 数据总线、BFF 代管管理员密码、重写 M2 视觉为 bespoke 六区域（改为通用 8 类 widget 渲染） |

## 2. 批次划分

| 批次 | 主题 | 状态 |
| --- | --- | --- |
| **M7.0** | 骨架：`git mv` 前端到 `lan-webui/webui`、Cargo 23 member、`modules.yaml`、`gen_capabilities.py --aggregate`、CI/release 改造 | **done** |
| **M7.1** | Rust 后端：`main.rs` / `capabilities.rs` / `proxy.rs` + `--check` 一致性校验 + 7 单测 | **done** |
| **M7.2** | 前端聚合：`capabilities.ts` + 8 类 widget + 动态三级导航 + `usePanel.ts` + demo 扩展到 `/p/*` + `npm run verify` | **done** |
| **M7.3** | 门禁与文档：`test_m7.py`、CI 纳入、ADR 0009、ADR 0008 收敛注记、M7-CHECKLIST、README 同步 | **done** |
| **M7.4** | 全量回归与提交 | **done** |

## 3. 后端声明与清单合成（唯一真相链）

| 文件 | 作用 |
| --- | --- |
| `lan-webui/modules.yaml` | 后端「引入哪些模块」的唯一声明：`enabled_components`（含 `port`）、`libs`、服务 `port` |
| `scripts/gen_capabilities.py` | 已有：从 8 组件 `.route()` / `.route_write()` 反向派生 8 份 sample + 全局 `allowlist.txt`；新增 `--aggregate`：读 `lan-webui/modules.yaml`，合成 `lan-webui/capabilities.json`（聚合，identity=`lan-webui`）+ `lan-webui/capabilities.allowlist.txt`；`--check` 比对派生结果与提交产物 |
| `lan-webui/capabilities.json` | 合成清单：`component="lan-webui"`、`runtime="aggregated"`、`components` 仅为 `modules.yaml` 声明的子集，每个 module 带 `component` 归属 |
| `lan-webui/capabilities.allowlist.txt` | 合成代理白名单：仅含被声明组件的端点，保证「代理可达」⊆「前端可见」 |

一致性规则（CI 强制，`test_m7.py` + `gen_capabilities.py --check`）：

1. `capabilities.json` 的 `component` 必须为 `lan-webui`（区别 M6 的 8 份 per-component 清单）。
2. 聚合 `components` 集合与 `modules.yaml` 的 `enabled_components` **完全相等**（移除一个组件 → 前端不生成该组件 DOM）。
3. 聚合 `libs` 集合必须包含 `modules.yaml` 声明的所有 libs（决定 `requires_lib` 面板是否出现）。
4. 代理白名单不引用任何未在 `modules.yaml` 声明的组件（有界同源代理不可越权）。
5. `gen_capabilities.py --check` 对 8 份 per-component 清单的端点集合与后端 `.route()` 注册完全一致（M6 规则沿用）。

**「后端控制前端显示哪些模块」的落点**：`visibleModules()` 只遍历清单中 `requires_lib` 满足条件且含面板的模块，代码中**不存在** `if (name === 'lan-wol')` 之类的组件分支；`modules.yaml` 删一条 → 清单少一条 → DOM 少一块，全程不碰前端标记。

## 4. 前端视图（能力清单驱动全集渲染器）

| 位置 | 实现 |
| --- | --- |
| `lan-webui/webui/src/capabilities.ts` | 清单类型 + 唯一过滤入口 `visibleModules()` / `hasComponent()`；`getManifest()` 优先取注入 `window.__LAN_CAPABILITIES__`，缺失回退到内嵌的同一份聚合 JSON |
| `lan-webui/webui/src/api.ts` | `componentApi(component)` 返回 `{ get, post, text }`，全部走 `/p/<encodeURIComponent(component)>` 前缀代理 |
| `lan-webui/webui/src/usePanel.ts` | 组合式：取数 / 动作 / 错误 / busy / loaded；`runAction` 按 `action.method` 区分 GET/POST |
| `lan-webui/webui/src/widgets/` | 8 类 widget：`KvGrid` / `Table` / `Form` / `CodeEditor` / `MetricGrid` / `LogList` / `TextView` / `ActionList`，由 `PanelView.vue` 按 `panel.widget` 分派（`kv-grid` 兜底） |
| `lan-webui/webui/src/App.vue` | 三级动态导航：组件下拉 → 模块导航（来自 `visibleModules()`）→ 面板网格；凭据弹窗只存 sessionStorage 经代理透传 |
| `lan-webui/webui/src/demo.ts` | `?demo=1` 拦截扩展到 `/p/*` 与 `/api/*`，覆盖全部模块样例 fixture |
| `lan-webui/webui/src/icons.ts` | 新增 9 个语义 SVG 字形（dns/relay/cert/scan/hosts/power/sync/discover/cube） |

运行时规则：

- 前端是**单一 SPA**，覆盖全部 8 个组件的全部模块；不再用 iframe 外挂 M2 Arco 页（M6 D-M6-2 适用范围被 ADR 0009 修订）。
- 凭据：DOM 中**没有 `type="password"` 输入框**（M5/M6 门禁断言保持通过），写操作凭据只存页面内存，以 `X-Admin-Password` / `X-Admin-Token` 头经代理透传；写保护始终在目标组件执行。
- 离线兜底：render-check / 直接文件打开时，前端内嵌一份提交的聚合清单，SPA 永不空白；经 `lan-webui` 实际服务时注入清单覆盖兜底。
- 产物仍是 `common/webui/src/assets/index.html`（单文件），`lan-mdns` 与 `lan-webui` 两个二进制共用同一份，`lan-mdns` 的 M2 行为零回退。

## 5. 代理边界（ADR 0009 D-M7-6，继承 ADR 0008 D-M6-6/7）

| 约束 | 实现 |
| --- | --- |
| 单一前缀 | `GET|POST /p/<name>/api/...`，裸 `/api/*` 路径全部 404 |
| 组件白名单 | `name` 必须命中 `lan-webui/capabilities.json` 的 `components` |
| 端点白名单 | `name\|path` 必须精确命中 `lan-webui/capabilities.allowlist.txt` |
| 固定目标 | 恒为 `127.0.0.1:<manifest port>`，不接受请求指定 host；拒绝 `..`、非 `api/` 前缀、超长路径 |
| 鉴权 | 代理入口显式调用 `common_auth::check`（loopback / token） |
| 凭据 | 只转发 `X-Admin-Password` / `X-Admin-Token` / `Content-Type`，不缓存、不落盘、不进日志与快照 |
| 有界 | 连接/读/写超时 5s、响应 1 MiB 上界、`Connection: close`、不跟随重定向 |

`lan-webui/src/proxy.rs` 复用 M6 的白名单转发逻辑（TcpStream、5s/1MiB、只透传两头、解析状态与 Content-Type 回写）。

## 6. 交付清单

| 类别 | 文件 |
| --- | --- |
| 契约 / 生成 | `scripts/gen_capabilities.py`（新增 `--aggregate` / 沿用 `--check`）、`lan-webui/modules.yaml`、`lan-webui/capabilities.json`、`lan-webui/capabilities.allowlist.txt` |
| Rust BFF | `lan-webui/Cargo.toml`、`lan-webui/src/main.rs`、`lan-webui/src/capabilities.rs`、`lan-webui/src/proxy.rs` |
| 前端 | `lan-webui/webui/`（由 `components/lan-mdns/webui` git mv）+ 新增 `capabilities.ts` / `usePanel.ts` / `widgets/*.vue`；删除 6 个 mdns 专属页与 `config-text.ts` |
| 共享产物 | `common/webui/src/assets/index.html`（与 `lan-mdns` 共用，单文件） |
| 门禁 | `scripts/test_m7.py`、`.github/workflows/ci.yml`（webui job 路径、`--aggregate` + `git diff`、离线循环、`test_m7.py`、M7 gate）、`.github/workflows/release.yml`（`BINS` 含 `lan-webui`，保留 `lan-shell` 只读兼容，`cp lan-webui/modules.yaml dist/conf/lan-webui.modules.yaml`） |
| 文档 | `docs/M7-PLAN.md`、`docs/M7-CHECKLIST.md`、`docs/ADR/0009-m7-lan-webui.md`、`docs/ADR/0008-m6-frontend-aggregation.md`（收敛注记）、`README.md` |

## 7. 决策登记

见 `docs/ADR/0009-m7-lan-webui.md`：D-M7-1 = A（编译期声明 + 同源代理 BFF，8 组件仍独立进程）、D-M7-2 = A（`lan-webui` 取代 `lan-shell` 成统一出口 8090，`shell/` 只读兼容）、D-M7-3 = A（`git mv` 前端、产物 outDir 不变）、D-M7-4~8 = 模块可见性单一声明 / 能力清单驱动全集渲染 / 代理边界沿用 / 零外部资源 / 兜底清单。**ADR 0008 的 D-M6-1（聚合入口切 `lan-webui`）、D-M6-2（不再 iframe）适用范围被修订**；D-M6-3~8 沿用。

## 8. 已知边界

- `lan-shell` 退居只读兼容：源码与 M6 门禁脚本保留在仓库、`cargo build --workspace` 仍产出其二进制，但 `release.yml` 不再将其作为统一出口发布（第 9 个发布二进制改为 `lan-webui`）。
- `lan-webui` 是聚合层，**不计入 8 个组件**，没有独立的 18 个统一端点；它只合成清单 + 做有界同源代理。
- `modules.yaml` 是前端可见性唯一开关：漏写端口 / 引用不存在组件会被 `validate()` 在启动前拦截（`--check` 门禁覆盖）。
- 内嵌兜底清单与 `modules.yaml` 同源，CI 用 `--aggregate` + `git diff --exit-code` 保证两者不漂移。
- 前端不再按组件分别定制（M2 六区域 bespoke 页改为通用 widget 渲染），视觉细节统一化，但保留 Calm Console 设计语言。
- `scripts/install.sh` 仍只安装 `lan-mdns`；macOS 上 `smoke_mdns.py` 的环境性失败仍是登记边界，与 M7 无关。

## 9. 执行记录（2026-11-14）

### 9.1 全量门禁逐项

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | **0 diff** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **0 warning** |
| `cargo test --workspace` | **208 passed / 0 failed**（M6 收尾 201 → +7，来自 `lan-webui` 后端能力解析 / 代理 / 注入 7 个单测） |
| `python3 scripts/verify_contracts.py` | **34 / 0**（PyYAML 完整模式，含 capabilities schema + 8 份 sample） |
| 降级模式（无 PyYAML） | **25 / 0 + `[DEGRADED: YAML checks skipped]`** |
| `python3 scripts/gen_capabilities.py --check` | **CAPABILITIES OK: 8 manifests match backend routes** |
| `python3 scripts/test_api_contract.py` | **75 / 0**（M2 回归） |
| `python3 scripts/test_m3.py` | **66/66 — M3 RUNTIME OK** |
| `python3 scripts/test_m4.py` | **76/76 — M4 RUNTIME OK** |
| `python3 scripts/test_m5.py` | **91 / 0 — M5 RUNTIME OK** |
| `python3 scripts/test_m6.py` | **152 / 0 — M6 RUNTIME OK** |
| `python3 scripts/test_ecosystem.py` | **91 / 0 — ECOSYSTEM OK** |
| `python3 scripts/test_m7.py` | **M7 OK: 8 components, 1 libs**（聚合 == modules.yaml、代理白名单有界、--check 通过） |
| `cd lan-webui/webui && npm run verify` | typecheck OK、build OK、markers 含「统一控制台 / 告警」、nav≥6 / panels≥1 / dots≥2、**RENDER OK** |
| `cargo build --workspace` | **10 个二进制**（8 组件 + `lan-shell` 只读兼容 + `lan-webui`） |
| `cargo build --release -p lan-mdns -p lan-webui` | 通过；release 矩阵仍 9 个发布二进制（不含只读兼容 `lan-shell`） |
| `git diff --check` | 无空白问题 |

### 9.2 `test_m7.py` 覆盖要点

- 静态：`modules.yaml` 的 `enabled_components` / `libs` 解析；`capabilities.json` 的 `component=="lan-webui"` 且 `components` 集合与声明**完全相等**；代理白名单不引用未声明组件；`libs` 全部出现在聚合清单。
- 漂移拦截：最后调用 `gen_capabilities.py --check`，保证 8 份 per-component 清单与后端 `.route()` 注册无漂移。
- 移除一条 `enabled_components` 的预期后果：前端 `visibleModules()` 不再生成该组件 DOM —— 这条「后端控制前端」属性由聚合 == 声明断言间接锁定（CI 中 `--aggregate` + `git diff` 双保险）。

### 9.3 结论

M7.0 ~ M7.4 全部交付；「`lan-webui` 作为自包含前后端一体统一控制台、后端声明直接决定前端可见模块」达成且由 CI 强制；M2 / M3 / M4 / M5 / M6 / 生态全部门禁**零回退**。**M7 可销项**。
