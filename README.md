# Lan Studio

> 内网轻量 ZeroConf 与局域网运维工具箱。
> 8 个 `lan-*` 组件，统一契约、独立二进制、独立 WebUI、可单独部署。

**当前状态：M4 完成（M2 `lan-mdns` 66/66 + 其余 P0 三组件 + P1 两组件）**（2026-11-14）。
M2 管理台为 **Vue 3 + TypeScript + Arco Design** 六区域控制台，打包成单个自包含 HTML
编入二进制；四道门禁全绿：Rust 单测 **81 项**、静态契约 **21 项**、运行态契约 **70 项**、
冒烟 **229 项**，逐条销项见 [`docs/M2-CHECKLIST.md`](docs/M2-CHECKLIST.md)。

M3 新增 `lan-relay` / `lan-ca` / `lan-scan` 三个独立二进制，以及共享运行时
`common/component`（统一契约端点 + 离线六区域控制台）。`lan-relay` 的环路检测 4 项
硬性验收（指纹窗口 / 出入网卡隔离 / TTL 归零 / 自生成包）与 60 秒无广播风暴压力测试
全部通过，逐条销项见 [`docs/M3-CHECKLIST.md`](docs/M3-CHECKLIST.md)。

M4 新增 `lan-hosts` / `lan-wol` 两个独立二进制与共享 `common/probe`（只读 TCP 探测 +
去抖状态机）。`lan-hosts` 只做静态名字映射的登记 / 渲染 / 原子发布，不实现组播应答、
DNS 与 HTTP 代理；`lan-wol` 的唤醒目标必须来自已登记设备、广播发送默认关闭、状态探测
连续去抖防误判。PRD §7.5 的「与 `lan-mdns` 无功能重叠、无强耦合」由 5 条自动断言验证，
方案见 [`docs/M4-PLAN.md`](docs/M4-PLAN.md)，逐条销项见
[`docs/M4-CHECKLIST.md`](docs/M4-CHECKLIST.md)。

- 完整规格：[`docs/PRD.md`](docs/PRD.md)
- 指标字典：[`docs/METRICS.md`](docs/METRICS.md)
- 告警登记：[`docs/ALERT-SCHEMA.md`](docs/ALERT-SCHEMA.md)
- 错误码表：[`docs/ERROR-CODES.md`](docs/ERROR-CODES.md)

## 目录

```
lan-studio/
├── Cargo.toml             # workspace，19 个 member
├── docs/                  # 规格与字典
│   ├── PRD.md             # 唯一权威规格书
│   ├── M2-CHECKLIST.md    # 66 条功能逐条销项基线
│   ├── M3-CHECKLIST.md    # 其余 P0 三组件销项基线
│   ├── M4-PLAN.md         # P1 两组件落地方案（决策清单 / 数据结构 / 分批）
│   ├── M4-CHECKLIST.md    # P1 两组件销项基线
│   ├── METRICS.md  ALERT-SCHEMA.md  ERROR-CODES.md
│   └── ADR/               # 0001 M1 / 0002 M2.1 / 0003 M2.2 / 0004 M2.3 / 0005 M2.4 / 0006 契约门禁
├── contracts/             # 机器可读契约（语言无关）
│   ├── openapi.yaml  config.schema.json  alert-event.json
│   ├── log-record.json  snapshot.json
│   └── examples/          # 通过 schema 的示例
├── scripts/
│   ├── verify_contracts.py   # 静态契约校验（文档彼此自洽）
│   ├── test_api_contract.py  # 运行态契约门禁（74 项，比对 openapi 与真实响应）
│   ├── smoke_mdns.py         # 运行态冒烟（229 项断言）
│   ├── test_m3.py           # M3 三组件统一契约 + 环路检测运行态验收
│   ├── test_m4.py           # M4 两组件统一契约 + 无重叠/无耦合运行态验收
│   └── install.sh          # 一键安装 / --upgrade（保留配置与凭据）
├── common/                # 公共 crate
│   ├── config/  crypto/  alert/  dns/  net/  log/  health/
│   ├── auth/  metrics/  webui/  api/
│   ├── component/         # M3 共享运行时：统一契约端点 + 离线控制台
│   └── probe/             # M4 只读 TCP 探测 + 去抖状态机
├── components/
│   ├── lan-mdns/          # 真实引擎 + 网关 + 配置模板
│   │   └── webui/         # Vue 3 + TypeScript + Arco 控制台（构建产物编入二进制）
│   ├── lan-relay/         # 跨网段 mDNS 中继 + 4 项环路检测
│   ├── lan-ca/            # 内网轻量证书签发 / 续期 / 吊销 / 审计
│   ├── lan-scan/          # 只读资产与端口探查（速率 / 并发有硬上界）
│   ├── lan-hosts/         # 静态名字映射登记 / 渲染 / 原子发布（不做 DNS）
│   └── lan-wol/           # 已登记设备网络唤醒 / 去抖状态探测 / 唤醒审计
├── deploy/
│   ├── Dockerfile  docker-compose.yml  lan-mdns.service
├── shell/                 # 统一出口（M5）
└── .github/workflows/
    ├── ci.yml             # 契约 + fmt/clippy/test + 资产 + 产物冒烟
    └── release.yml        # musl 静态单文件 + sha256
```

## 契约校验

校验器零强制第三方依赖，任何 Python 3 都能跑：

```bash
python3 scripts/verify_contracts.py
```

| 模式 | 检查数 | 触发条件 | 覆盖范围 |
| --- | --- | --- | --- |
| 完整 | 21 | 已安装 PyYAML | JSON Schema 自解析、示例数据、OpenAPI 必备端点、错误码 / 指标 / 告警 kind 与字典一致 |
| 降级 | 12 + `WARN` | 未安装 PyYAML | 仅 JSON 类检查，跳过 8 项 YAML 相关检查 |

CI 已自动安装 PyYAML，始终跑完整模式；本地离线自检可用降级模式：

```bash
python3 -m pip install pyyaml   # 升级为完整模式
```

降级时输出末尾带 `[DEGRADED: YAML checks skipped]`，退出码仍为 0，表示「已执行到的检查」全部通过——是否完整以该标记为准。

## 构建与运行

零第三方 crate，`cargo build` 不需要访问 crates.io：

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p lan-mdns

LAN_STUDIO_PORT=8080 \
LAN_STUDIO_CONFIG=components/lan-mdns/config.template.yaml \
  ./target/release/lan-mdns

curl --noproxy '*' http://127.0.0.1:8080/healthz
curl --noproxy '*' http://127.0.0.1:8080/api/records
curl --noproxy '*' -X POST -H 'Content-Type: application/json' \
  -d '{"name":"nas.local","type":"A"}' http://127.0.0.1:8080/api/probe
```

单文件自举与运维子命令（均无需外部工具）：

```bash
./target/release/lan-mdns --version        # 版本 / 架构
./target/release/lan-mdns --print-config   # 输出内嵌 YAML 模板
./target/release/lan-mdns --validate       # 离线校验配置，不启动网络
./target/release/lan-mdns --cli            # CLI 模式，不监听端口
./target/release/lan-mdns --healthcheck    # 存活探针，供 Docker / systemd 调用
```

**首次启动**会在配置路径生成一次内嵌模板；已存在的配置永不覆盖（F-E09）。

### 管理台（前端）

管理台源码在 `components/lan-mdns/webui/`，Vue 3 + **TypeScript（strict）** + Arco
Design，由 Vite 打成单个 HTML 后再编入二进制。**仓库内没有 `.js` 源文件**。

```bash
cd components/lan-mdns/webui
npm ci
npm run typecheck   # vue-tsc --noEmit，类型门禁
npm run build       # typecheck + build → common/webui/src/assets/index.html
```

改动前端后必须重新构建并一起提交，否则二进制里仍是旧页面；CI 的 `webui` job 会跑
`typecheck` 并对产物断言「无外部资源」。详见
[`components/lan-mdns/webui/README.md`](components/lan-mdns/webui/README.md)。

**写操作受管理员密码保护**（F-F02~F-F04）。首次未设置密码时所有写路由返回
`409 setup_required`；先设置密码，再在请求头带 `X-Admin-Password`：

```bash
curl --noproxy '*' -X POST -H 'Content-Type: application/json' \
  -d '{"new_password":"change-me-now"}' http://127.0.0.1:8080/api/auth/password
curl --noproxy '*' -X POST -H 'X-Admin-Password: change-me-now' \
  http://127.0.0.1:8080/api/reload
```

运行时数据目录可全部重定向（冒烟脚本正是靠这一点隔离实例，不污染仓库）：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_STUDIO_AUTH_FILE` | `auth.toml` | 管理员密码摘要，文件权限 `0600` |
| `LAN_STUDIO_LOG_DIR` | `logs` | 轮转日志目录，磁盘硬上界 = `max_bytes × (backup_count + 1)` |
| `LAN_STUDIO_HISTORY_DIR` | `config_history` | 配置快照目录，按 `observability.history_limit` 有界保留 |
| `LAN_STUDIO_PORT` | `server.port`（8080） | 测试 / CI 端口覆盖钩子，生产以 YAML 为准 |
| `LAN_MDNS_GATEWAY_PORT` | `mdns.gateway.port`（80） | 仅测试隔离用：覆盖网关监听端口 |

## 告警与内置网关

告警在 `alerts:` 段配置，规范事件由 `common-alert` 统一适配五类平台：

```yaml
alerts:
  enabled: true
  webhooks:
    - "https://oapi.dingtalk.com/robot/send?access_token=..."
  cooldown_seconds: 300
  timeout_seconds: 5.0
  max_retries: 2
```

`http://` Webhook 由标准库直接发送；`https://` 公网 Webhook 通过系统 `curl` 桥接
（仅此时需要 curl，组件本体仍是零依赖单文件）。`/api/alerts` 可查队列、发送 / 失败
计数与按 `kind` 统计。

网关默认**关闭**，不会抢占 80 端口；开启后只对已登记域名做静态 Host 路由：

```yaml
mdns:
  gateway:
    enabled: true
    port: 80
    admin_host: "mdns.local"
    hosts:
      - host: printer.local
        target: "192.168.5.30:80"
        proxy: true
```

已发布服务（其 `host` 有对应 A 记录）自动登记为路由，`services[].proxy` 与
`hosts[].proxy` 是每服务开关。未登记返回 404 友好页，目标不可达返回 502 友好页并
发出 `gateway_target_unreachable` 告警；`/go/` 提供最小导航入口。

## 部署

```bash
# Docker Compose（host 网络是 mDNS 的硬约束）
docker compose -f deploy/docker-compose.yml up -d --build
docker compose -f deploy/docker-compose.yml logs -f

# 裸机 / systemd
sudo ./scripts/install.sh                 # 首次安装
sudo ./scripts/install.sh --upgrade       # 升级：保留配置 / 凭据 / 日志 / 快照
sudo ./scripts/install.sh --from ./dist   # 使用本地发布目录
```

运行态冒烟（自动使用隔离组播组、高位端口与独立临时数据目录，不向真实 LAN 发包）：

```bash
python3 scripts/smoke_mdns.py
```

运行态契约门禁（起隔离实例，逐个端点比对 `openapi.yaml` 声明的状态码、统一信封、
错误码与鉴权矩阵；与静态校验互补）：

```bash
python3 scripts/test_api_contract.py
```

设计取舍见
[`docs/ADR/0001-m1-skeleton-decisions.md`](docs/ADR/0001-m1-skeleton-decisions.md)、
[`docs/ADR/0002-m2-1-protocol-kernel.md`](docs/ADR/0002-m2-1-protocol-kernel.md)、
[`docs/ADR/0003-m2-2-observability-security.md`](docs/ADR/0003-m2-2-observability-security.md)、
[`docs/ADR/0004-m2-3-alerting-gateway-deploy.md`](docs/ADR/0004-m2-3-alerting-gateway-deploy.md) 与
[`docs/ADR/0005-m2-4-console-typescript.md`](docs/ADR/0005-m2-4-console-typescript.md) 与
[`docs/ADR/0006-api-contract-runtime-gate.md`](docs/ADR/0006-api-contract-runtime-gate.md)。

## 里程碑

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0 | 契约冻结（本目录全部内容） | **完成** |
| M1 | monorepo 骨架、`common-*` 最小集、CI、单文件产物 | **完成** |
| M2 | `lan-mdns` 功能全集 66 条逐条销项 | **完成（66/66）** |
| M3 | 其余 P0：`lan-relay` / `lan-ca` / `lan-scan` | **完成（环路检测 4/4）** |
| M4 | P1：`lan-hosts` / `lan-wol` | **完成（无重叠 5/5）** |
| M5 | P2：`lan-sync` / `lan-ssdp` + 统一出口 + 生态 DoD | 待启动 |

## 说明

- 契约以 `contracts/` 下机器可读文件为准，文字描述冲突时以契约文件为准。
- 仓库：`https://github.com/scpuny/lan-studio.git`，`origin/main`。
- 既有 mDNS 服务已进入维护阶段（仅修缺陷、不接新需求），其功能由 `lan-mdns` 承接。
- M2 已完成：66/66 销项，PRD 8.1 的 8 项组件级 DoD 逐条核对通过
  （见 [`docs/M2-CHECKLIST.md`](docs/M2-CHECKLIST.md) 第 8 节）。
- M3 已完成：三组件共用 `common-component` 契约运行时；`lan-relay` 环路检测 4 项
  硬性验收与 60 秒压力测试通过（见 [`docs/M3-CHECKLIST.md`](docs/M3-CHECKLIST.md)）。
  三组件默认端口 8081 / 8082 / 8083，与 `lan-mdns` 的 8080 不冲突。
- M3 的 `lan-ca` 采用**单根自签 HMAC 凭证**（非 X.509 公钥证书、不接入浏览器 TLS
  信任链），以保持 workspace 零第三方 crypto crate；边界详见组件 README。
- M4 两组件默认端口 8084 / 8085，沿用 `common-component` 的统一契约与离线六区域控制台；
  业务数据各写自身 JSON lines 文件，不进配置快照。
- M4 的 `lan-hosts` 拒绝 `.local` 映射（归 `lan-mdns`），`lan-wol` 的 `send_enabled`
  默认关闭，需显式开启并向已登记设备发送唤醒包；两者互不依赖、也不依赖 `lan-mdns` /
  `lan-scan`。
- `LAN_MDNS_GROUP` / `LAN_MDNS_PORT` / `LAN_MDNS_GATEWAY_PORT` 仅用于测试隔离；
  生产组播地址、mDNS 端口与网关端口永远取自 YAML。
