# ADR 0006 · API 契约的运行态门禁（F-J03）

- 状态：已实施（2026-11-13）
- 影响范围：`scripts/test_api_contract.py`（新增）、`contracts/openapi.yaml`、
  `docs/ERROR-CODES.md`、`common/config`（段形状校验）、`components/lan-mdns/config.rs`、CI
- 关联契约：`contracts/openapi.yaml`（PRD 3.2）、`docs/ERROR-CODES.md`、PRD 8.1
- 前置：ADR 0001（零第三方 crate）、ADR 0005（管理台与 `vue-tsc` 门禁）

## 1. 背景

`scripts/verify_contracts.py` 已经能证明**文档之间**自洽：OpenAPI 覆盖 22 个端点、
`ErrorCode` 恰好 8 值、`ERROR-CODES.md` / `METRICS.md` / `ALERT-SCHEMA.md` 与枚举一致。
但它一个 HTTP 请求都不发。于是存在一个完全空白的风险区：

> 契约文件全部正确，二进制却返回别的形状，而所有门禁都是绿的。

这个风险区里已经真实存在偏差（见第 4 节）。`F-J03` 要求的是**运行态**契约测试：
对 `openapi.yaml` 全部端点断言状态码 / 统一信封 / 错误码 / 鉴权矩阵。

## 2. 决策

### D1 · 运行态门禁与静态门禁分开，都进 CI

两者失败的含义不同，混在一起会掩盖问题：

| 门禁 | 证明什么 | 失败意味着 |
| --- | --- | --- |
| `verify_contracts.py` | 文档彼此自洽 | 改了枚举没改文档 |
| `test_api_contract.py` | 运行中的二进制符合文档 | 实现漂移或契约写错 |
| `smoke_mdns.py` | 端到端业务行为 | 功能退化 |
| `cargo test` | 单元级不变量 | 逻辑错误 |

CI 里顺序为：静态契约 → 编译 → **运行态契约** → 冒烟。契约测试放在冒烟之前，因为它
更快（约 4 秒）且失败信息更精确。

### D2 · 不引入 PyYAML 依赖，自带窄扫描器 + 双源互校

测试要读 `openapi.yaml` 才知道「文档承诺了什么」。引入 PyYAML 会让脚本从「零依赖、
随时可跑」退化为「先装包」，而 CI 与本机都不该为一条断言付这个代价。

做法是写一个**刻意窄**的扫描器：只认这份文档的固定缩进（2 空格路径键、4 空格方法键），
遇到新的顶层键立即停止跟踪。它顺手还抽出三样测试需要的元信息：

```python
if stripped == "security: []":
    current.exempt = True          # 哪些操作免鉴权
match = STATUS_LINE.match(stripped):
    current.codes.add(int(...))     # 该操作声明了哪些状态码
if "schemas/ApiOk" in stripped:
    current.envelope = True         # 成功响应是否走统一信封
```

**不靠扫描器自证**：装了 PyYAML 时再用它独立解析一遍，两者结果必须相同（第 3 项断言）。
本机无 PyYAML 时该项 SKIP，扫描器为权威——这样既零依赖，又不给「扫描器写错了还全绿」
留口子。

### D3 · 断言「状态码属于该操作声明过的集合」，而不是「200 或 400 都行」

逐操作比对 `op.codes`。若把成功判定放宽成 `status < 400`，`/api/config` 返回 200 却
是错误信封也会过；若放宽成「非 500 即可」，等于没测。收紧到「契约给这个操作声明过」
才既能抓漂移，又不会把合法的 409/404 误判成失败。

对非信封路由（`/api/metrics`、`/api/logs/stream`、`/healthz`）单独一条断言：
**不得返回 `ApiError`**。它们的线格式是文本/SSE，无法套信封，但一旦回归成错误包装
必须被抓到。

### D4 · 错误码闭合性用「可达 + 记录豁免」，不用「必须凑满 8 个」

8 值枚举里只有 5 个能在 `/api/*` 上真实产生。硬凑另外 3 个只能靠注入故障，那是在测
测试框架而不是测服务。于是分三层：

1. **硬失败**：运行时出现过枚举外的 code（实现发明了新错误码）；
2. **硬失败**：5 个可达 code 有任何一个没被产生（错误路径被改坏了）；
3. **打印 NOTE**：剩下 3 个逐条说明为什么这里不产生，以及由谁覆盖：

```python
CODE_NOTES = {
    "internal_error": "只在运行时状态缺失的启动窗口可达，由 common-api 单测覆盖",
    "target_unreachable": "网关专用：ERROR-CODES §2 把它给了 Host 网关的友好 503 页（HTML 而非 JSON），由 smoke 覆盖",
    "rate_limited": "留给会限流的组件（lan-scan / lan-relay）；lan-mdns 没有限流路由，永不产生",
}
```

这条 NOTE 不是装饰：它把「契约是整个生态共享的词汇表，不是每个组件都必须全说一遍」
这个事实写进了代码，避免后续有人为了让门禁好看而去伪造 429。

### D5 · 鉴权矩阵按 ERROR-CODES §3 的表格逐行验，并额外验豁免

六个场景各自一条断言，且用**两个实例**：默认实例（无 Token）验 `setup_required` 与
密码写保护；带 Token 实例验 `auth_required` / `auth_failed`。因为 `auth_required`
只在配置了 Token 后才可能出现——一个实例根本无法覆盖整张表。

另外两条容易漏的：

- `/healthz`、`/readyz` 在路径上不以 `/api/` 开头，因此**完全不过鉴权中间件**，
  配了 Token 也必须 200；
- 密码设置后，**读路由仍然可读**（只有写路由挂密码），这条防止把写保护误做成全局。

### D6 · 泄漏扫描针对「真实秘密值」，不针对 `token=` 这种字面量

最初用 `password=` / `token=` 之类子串扫，`GET /api/config` 立刻误报：模板注释里写着
`access_token=...`。正确的断言对象是：

- 本次运行**铸造的真实凭据**（`contract-pass-1`、`contract-token` 等）不得回显；
- 栈帧标记（`panicked at`、`RUST_BACKTRACE`）不得出现；
- 内部路径（仓库根、临时工作目录）不得出现。

成功与错误响应都要扫，因为泄漏更可能发生在错误信息里。

### D7 · 会写配置的验证脚本一律复制到临时目录

`Instance` 把配置写到 `mkdtemp()` 下的副本，并把 `LAN_STUDIO_CONFIG` /
`LAN_STUDIO_AUTH_FILE` / `LAN_STUDIO_LOG_DIR` / `LAN_STUDIO_HISTORY_DIR` 全部指向该
目录，端口与组播坐标也隔离。

这不是洁癖——开发中正因为手动实验时把 `LAN_STUDIO_CONFIG` 直接指向仓库里的
`config.template.yaml`，一次「保存」就把模板写坏了；而模板是 `include_str!` 进二进制
的，于是 `--print-config` 的输出跟着一起被污染，冒烟里 4 条模板相关断言立刻变红。
测试自己写脏自己的输入文件，是最难查的一类失败。

## 3. 断言清单（74 项；无 PyYAML 时 73）

| 小节 | 项数 | 内容 |
| --- | --- | --- |
| 1 契约操作清单 | 3~4 | 24 个操作 / 22 个路径；扫描器与 PyYAML 互校；豁免端点恰为两个探针 |
| 2 端点运行态一致性 | 13 | 起隔离实例，`/healthz` 体形状、Prometheus 文本与 9 个必备指标族、SSE 握手与帧、快照 Schema-1、保存/历史/回滚/导入/重载闭环 |
| 3 逐操作状态码 | 24 | 每个操作的成功响应必须落在契约给**该操作**声明的码集合内，信封有无与契约一致 |
| 4 错误码与鉴权 | 20 | 未知路径 404、三类非法配置 400、密码错误/缺失 403、路径穿越 400、未知快照 404、四个写路由的 409、读路由不受密码约束、`X-Request-Id` 回显、泄漏扫描、日志无凭据与磁盘上界（PRD 8.1 #6） |
| 5 Token 矩阵 | 7 | 探针豁免、缺 Token 401、错 Token 403、正确 Token 200、`token_required` 反映配置、`/` 返回内联控制台 |
| 5b 错误码闭合性 | 3 | 5 个可达 code 全部产生、无枚举外 code、枚举恰为 8 值 |
| 6 文档与实现漂移 | 3 | 文档承诺的操作不得 404、不得暴露未文档化路由、路由表不得漏掉契约操作 |

其中第 6 小节是双向的：只查「文档有、实现无」会漏掉「实现有、文档无」的隐性攻击面；
只查反向则会漏掉「文档承诺了但根本没实现」。

## 4. 它第一次运行就抓到的三个真实偏差

1. **`GET /api/config/snapshot` 响应形状与文档不符**。实现是 `ok(req, &snap.to_json())`
   ——统一信封、`data` 为快照；`openapi.yaml` 却声明裸 `Snapshot`。按
   `ERROR-CODES.md` §1「成功响应恒为信封」判定**文档错**，已修契约并补 `description`
   说明「GET 走信封、POST 收裸文档」这个不对称。
2. **信封例外从未登记**。`/api/metrics`（Prometheus 文本）与 `/api/logs/stream`（SSE）
   同为 `/api/*` 却不套信封，与「恒为信封」的措辞直接冲突。已在 `ERROR-CODES.md`
   新增 §1.1 白名单逐条记录理由，并明确快照导出**不属于**例外。
3. **内联段写法被静默吞掉**。`server: {port: 8080}` 经 `parse_flat` 塌缩成叶子键
   `server`，于是所有 `server.*` 回落到默认值、整段配置丢失，而保存返回 `saved:true`。
   已让 `validate_common` 与 `MdnsConfig::from_flat` 拒绝「段键带非空标量值」的形状
   （`{}` / `[]` 空映射仍合法），并各配一条单元测试。

## 5. 后果

- CI 新增一步（约 4 秒），失败信息直接给出「哪个操作、什么码、契约声明了什么」；
- 契约改动必须同时过静态门禁与运行态门禁，改文档绕过实现不再可行；
- 配置校验更严：内联段写法从「静默丢字段」变成明确的 `invalid_config`；
- 后续 7 个组件可直接复用同一脚本：把 `implemented` 路由表换成该组件的即可，断言
  逻辑与契约解析完全通用——这正是 PRD 8.2「统一契约在 8 个组件间零偏差（自动化契约
  扫描）」需要的基础设施。
