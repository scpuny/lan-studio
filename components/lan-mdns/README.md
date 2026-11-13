# lan-mdns（M1 骨架）

> 内网 `.local` 名字解析与 DNS-SD 服务发现组件。
> **当前为空壳**：按 PRD 7.5，mDNS 协议在 M2 实现，本里程碑不含任何组播收发逻辑。

## 构建与运行

```bash
cargo build --release -p lan-mdns
LAN_STUDIO_PORT=8080 LAN_STUDIO_CONFIG=components/lan-mdns/config.template.yaml \
  ./target/release/lan-mdns
```

CLI 模式（不监听端口，仅输出构建信息）：

```bash
./target/release/lan-mdns --cli
```

## 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LAN_STUDIO_CONFIG` | `config.yaml` | 配置文件路径；不存在时使用内置默认值 |
| `LAN_STUDIO_PORT` | `8080` | HTTP 监听端口（M1 暂不放进已冻结的配置 schema） |
| `GIT_COMMIT` | `dev` | 编译期注入，进入 `build_info` 指标 |

配置结构遵循 `contracts/config.schema.json`，绑定地址取自 `server.listen`。

## 已实现端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 内嵌离线 WebUI |
| GET | `/healthz` | 存活探针，恒 `{"ok":true}` |
| GET | `/readyz` | 就绪探针 |
| GET | `/api/status` | 组件状态与运行时长 |
| GET | `/api/metrics` | Prometheus 0.0.4 指标（含 `build_info`） |
| GET | `/api/config` | 当前配置原文 |
| GET | `/api/auth` | 鉴权状态 |

`/api/*` 受 `common-auth` 保护：`admin_token` 为空时仅允许回环访问，非空时校验 `X-Admin-Token` 或 `?token=`。

## 明确不做（M1）

- mDNS / DNS-SD 报文编解码与组播收发
- 服务发布、健康探测、查询审计、服务浏览、冲突检测
- 配置保存 / 快照 / 回滚 / 导入导出、告警、日志轮转
- Arco Design 正式 UI（当前为编译期内嵌的占位页）
