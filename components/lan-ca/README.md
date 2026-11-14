# lan-ca（M3）

内网轻量证书签发与管理，单根自签信任模型（PRD §4.4 / D8）。与 `lan-mdns` 完全独立部署。

## 信任模型与边界（重要）

- **单根自签**：首次启动生成 32 字节根密钥，落 `lan-ca.root.key`（Unix 权限 `0600`），后续永不覆盖。
- 每张证书是一条规范化记录（`serial / subject / sans / not_before / not_after / issuer / signature`），其 TBS 串由根密钥做 **HMAC-SHA256** 认证；`issuer` 存根指纹而非密钥。
- 这是**内网轻量凭证**，**不是 X.509 公钥证书**，不接入浏览器/操作系统 TLS 信任链。整个 workspace 保持零第三方 crypto crate；若将来需要标准 X.509，可引入 `rcgen`/`ring` 并在 ADR 记录，本组件的业务流程（签发/续期/吊销/审计/导出）无需改动。

## 能力

| 能力 | 端点 | 说明 |
| --- | --- | --- |
| 根证书信息 | `GET /api/ca/root` | 只返回指纹与算法，**不含密钥** |
| 签发 | `POST /api/ca/issue` | `{"subject":"nas.lan","sans":["nas.lan","192.168.1.5"],"days":90}`，返回记录 + PEM |
| 续期 | `POST /api/ca/renew` | `{"serial":"...","days":365}`，重签并延长期限 |
| 吊销 | `POST /api/ca/revoke` | `{"serial":"...","reason":"key-compromise"}` |
| 吊销列表 | `GET /api/ca/crl` | 含签发主体、时间、原因 |
| 证书清单 | `GET /api/ca/certificates` | 全量记录 |
| 验签 | `POST /api/ca/verify` | `{"serial":"..."}` → `valid` |
| 审计 | `GET /api/ca/audit` | 签发/续期/吊销三条链路均留痕，有界 500 条 |
| 安全导出 | `GET /api/ca/export` | 证书 + 审计快照，**不含根密钥** |

## 验收

- 私钥不明文落日志、不进快照导出、不进 store 文件 —— 单测 `root_key_never_leaks_into_export_or_audit` 直接断言根密钥 hex 不出现在导出/状态/落盘内容里。
- 签发与吊销有审计记录 —— 单测 `issue_verify_tamper_renew_revoke_lifecycle` 验证 3 条审计 + CRL + 签名篡改被拒。
- 域名合法性 + 内网后缀白名单（默认 `.lan` / `.internal` / `.local`），拒绝通配符与越界名称。
- 配置保存 → 快照 → 回滚 → 导入导出闭环由 `common-component` 统一提供。

## 配置与命令

首次启动生成 `lan-ca.yaml`（默认端口 8082），此后不覆盖。关键字段见 `config.template.yaml`：`ca.default_days` / `max_days` / `allowed_suffixes` / `max_certificates` / `root_key_path` / `store_path`。**根密钥与 store 路径为进程级固定**，修改后需重启（热重载会生效其余字段）。

```bash
cargo build --release -p lan-ca
./target/release/lan-ca --check
LAN_STUDIO_PORT=18082 ./target/release/lan-ca
```

## 指标与告警

组件族：`lanstudio_ca_certificates{status}`、`..._issued_total`、`..._revoked_total`、`..._audit_entries`。告警 kind：`cert_expiring`(warning，`not_after` 进入 7 天窗口)、`cert_revoked`(info)、`cert_issued`(info)。
