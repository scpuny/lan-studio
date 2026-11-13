# ADR 0001 · M1 骨架的技术取舍

- 状态：已接受
- 日期：2026-11-13
- 影响范围：M1，M2 需复核

## 背景

PRD 7.5 要求 M1 交付 workspace、`common-*` 最小集、CI / release 流水线与**可启动的单文件产物**，验收为 `/healthz`、`build_info` 指标、离线 WebUI 可打开，且明确「不写组件业务代码」。PRD 2.4 的技术基线是 Rust + YAML + Arco + 单文件发布；D1 默认「最小依赖，优先无重型运行时」。

## 决策

### 1. 零第三方 crate

M1 全部 crate 只用 Rust 标准库：HTTP 服务手写 HTTP/1.1（`TcpListener` + 每连接一线程）、Prometheus 文本手写、JSON 手写转义、内嵌 WebUI 用 `include_str!`。

理由：
- 编译完全离线可复现，`cargo build` 不访问 crates.io；
- 单文件产物的可信度最高，无供应链与动态库顾虑；
- M1 验收只涉及骨架，不需要复杂框架能力。

代价：手写实现覆盖面有限，M2 起按需要引入成熟 crate（见下）。

### 2. 配置解析采用最小 YAML 子集

`common-config` 的 `parse_flat()` 只解析顶层标量 + 一级 section（`server.listen` 这类扁平键），能读通 `contracts/examples/config.sample.yaml` 与组件模板。

理由：M0 已冻结 `config.schema.json`，M1 空壳无需完整 YAML；避免此时就锁定某个序列化库。

**M2 动作**：按 `config.schema.json` 引入 `serde` + `serde_yaml`（或完善自研解析器），补齐数组、嵌套、类型校验与 `additionalProperties` 语义；删除 `parse_flat()` 或降级为回退路径。

### 3. 监听端口用环境变量，不进配置 schema

`LAN_STUDIO_PORT`（默认 8080）提供 HTTP 端口，绑定地址仍取 `server.listen`。

理由：M0 契约的 `server` 段未定义 `port`，为不破坏已冻结契约，M1 不擅自增改字段。

**M2 动作**：走契约变更流程，在 `config.schema.json` 的 `server` 段补 `port`（1..65535），同步 `config.sample.yaml` 与 `contracts/openapi.yaml` 的 servers 变量，然后移除环境变量依赖。

### 4. WebUI 为占位页，不是 Arco

M1 的 `common-webui` 内嵌一个零外部资源、深浅色自适应的静态页面。

理由：PRD 3.7 要求 Arco 构建产物编译期嵌入；但 M1 明确不做组件业务，引入前端构建链会提前锁定工具链且无业务内容可承载。

**M2/M5 动作**：接入 Arco 壳，构建产物仍以 `include_str!`/资源嵌入方式进二进制，保持「离线可用、零外部静态文件」不变。

### 5. MSRV 1.75，edition 2021

取保守值以覆盖较旧发行版；本机验证工具链为 1.94.1。D1（Rust MSRV 与异步运行时选型）仍待确认，本 ADR 不引入任何异步运行时。

### 6. 依赖方向

`common-api` 依赖 `common-log` / `common-auth` / `common-health` / `common-metrics`；组件依赖全部 `common-*`；`common-*` 之间不反向依赖、不依赖任何组件。由 CI 的 clippy 与人工评审共同保障。

## 后果

- 正面：M1 可离线构建、可真实启动验收、CI 简单、产物小。
- 负面：M2 需要替换配置解析与 WebUI，存在一次受控返工；已在 PRD 7.7 R4（契约未冻结就开工导致返工）中登记，本 ADR 把返工范围明确限定在上述两点。
