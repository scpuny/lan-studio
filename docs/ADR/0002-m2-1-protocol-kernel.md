# ADR 0002 · M2.1 协议内核与配置内核取舍

- 状态：已接受
- 日期：2026-11-13
- 影响范围：`common/dns`、`common/net`、`common/config`、`common/metrics`、`components/lan-mdns`、CI

## 背景

M0 冻结统一契约，M1 交付 Cargo workspace 与 `common-*` 最小集，`lan-mdns` 是明确的
空壳。M2 需要按 PRD 第 5 章 66 条功能逐条销项。M2.1 是第一批：协议内核 + 配置内核 +
mDNS 引擎。ADR 0001 的「零第三方 crate」约束继续有效。

## 决策

### 1. 继续零第三方依赖，自实现 DNS/mDNS 报文编解码

不引入 `trust-dns` / `hickory-dns` 等 crate。原因：

- 发布目标是 musl 静态单文件，依赖越少交叉构建越稳；
- 我们只需要 RFC 1035 / 6762 的一个小子集（A/AAAA/PTR/SRV/TXT、压缩指针、QU、
  cache-flush、Known-Answer 抑制、Goodbye），全量 DNS 库反而带来攻击面与体积；
- 编解码必须能离线单测，`common-dns` 提供 `Message::encode/decode` 纯函数。

代价：不支持 OPT / DNSSEC / NSEC 等扩展，未知类型降级为 `RData::Raw(u16, Vec<u8>)`
原样透传，不尝试解析。

### 2. 组播 socket 用 raw fd 在 bind 前设置 `SO_REUSEPORT`

`std::net::UdpSocket::bind` 无法在 bind 之前设置 `SO_REUSEPORT`，而要与系统 mDNS
守护进程（avahi / systemd-resolved）共存于 5353 端口，必须在 bind 前设置。因此
`common/net` 在 Linux 上通过 `extern "C"` 调 `socket` / `setsockopt` / `bind`，再用
`UdpSocket::from_raw_fd` 接管。非 Linux 走标准 `UdpSocket::bind` 降级。

所有组播收发都封装在 `MdnsSocket`，调用方不接触裸 fd。

### 3. 配置：列表型 YAML 拍平为 `prefix[i].field`，不引入 YAML AST

`common/config` 的 `parse_flat()` 用缩进栈把嵌套 map 与 list-of-maps 拍平成点号键，
例如 `mdns.records[0].name`。理由：

- 保持零依赖；
- 组件侧只需 `list_len()` + 类型化 getter，不必理解 YAML 节点类型；
- 离线校验 `validate_text()` 与运行态解析共用同一拍平结果，不会出现「校验通过但
  解析出不同值」的分叉。

已知边界：只支持 `- key: value` 形式的标量列表项与一层嵌套 map；不支持块标量
（`|` / `>`）、锚点与多行流。当前配置 schema 用不到，若 M2.2 需要再扩展并在本 ADR 记录。

### 4. 端口 / 组播地址仍由环境变量提供，但拆成两层

- `LAN_STUDIO_PORT`（HTTP 监听口）保留 ADR 0001 的临时约定，不放进已冻结的 schema；
- 新增 `LAN_MDNS_GROUP` / `LAN_MDNS_PORT` **仅用于测试隔离**，避免 CI 与本地冒烟
  向真实 LAN 发 224.0.0.251:5353 组播包。

这两个覆盖只作用于运行时 `Engine`，`--cli` 仍报告配置文件自身的值。生产配置永远是
YAML 里的 `mdns.group` / `mdns.port`。M2.2 把 HTTP 端口收回 schema 时应同时删除
`LAN_STUDIO_PORT`，并评估是否保留两个测试覆盖变量。

### 5. 引擎用「一个 socket + 两个线程 + 有界队列」，不上 async runtime

`Engine` 持有单个 `MdnsSocket`，receiver 线程负责收包 / 应答 / 摄取，announce 线程负责
启动与周期宣告。共享状态用 `Arc<RwLock<Vec<ResourceRecord>>>` + `AtomicU64` 统计 +
`Mutex<VecDeque<T>>` 有界队列（`push_bounded` 显式淘汰）。

理由：mDNS 是低频 UDP 流量，线程模型足够；避免引入 tokio / async-std 与 musl 交叉
构建的复杂度。所有队列有硬上界（`MAX_QUERIES` / `MAX_OBSERVED` / `MAX_BUFFER`），
满足 PRD「全链路有界资源」。

### 6. 主动探针用独立 socket，QU 模式绑定临时端口

`probe()` 额外 bind 一个 `SO_REUSEPORT` socket 发查询并收集应答，构成真实回环 E2E。
普通（组播）模式绑定 `mdns.port` 以接收组播应答；QU 模式若也绑定同一端口，Linux
会把单播应答在 reuseport 组内做负载分发，可能送回引擎自己的 socket 导致探针收不到。
因此 QU 模式绑定端口 0（临时端口），并把查询显式发往 `group : mdns.port`，让应答
只有一个可能接收者。

### 7. 优雅退出：信号处理器只写原子标志，Goodbye 由守护线程发

Rust 信号处理器内只能调用 async-signal-safe 函数。实现里 handler 仅对 `AtomicBool`
做 `store(Relaxed)`，另起 watchdog 线程轮询标志，调用 `Engine::shutdown()` 广播
TTL=0 的 Goodbye 后 `process::exit(0)`。不依赖 `libc` crate，直接声明
`extern "C" fn signal(i32, usize) -> usize`。

### 8. 指标：Registry 开放 `declare_*` / `set_counter`，组件自行登记专有族

`common/metrics` 保持「只含 9 个每组件必备族」的通用性，不硬编码 `lan-mdns`。
新增 `declare_gauge` / `declare_counter` / `set_counter` 三个公共方法，由 `main.rs`
按 `docs/METRICS.md` 登记 7 个 M2.1 可交付的专有族。

**重要命名修正**：`Registry::new("lan-mdns")` 生成的前缀已是 `lanstudio_mdns_`，
组件传入的短名必须不带 `mdns_`（用 `records` 而非 `mdns_records`），否则会产出
`lanstudio_mdns_mdns_records` 这种双前缀指标。该坑已写入冒烟断言第 27 项。

M2.1 只交付 7 个引擎相关族；`bytes_total`、`health_targets`、`conflicts`、
`config_history_versions`、`auth_password_enabled`、`gateway_enabled` 随对应功能在
M2.2 / M2.3 落地，PRD 8.1 的「第 3 节全部存在」门禁在 M2.4 统一核对。

## 后果

- 正面：真实 mDNS 闭环可离线单测 + 本机 E2E 验证；静态单文件发布路径不变；
  CI 不向真实 LAN 发包。
- 负面：自实现编解码需要自行维护边界（未知类型透传、压缩指针深度上限 32）；
  裸 `extern "C"` 仅 Linux 路径，移植到 macOS/BSD 时需补重用端口逻辑；
  `process::exit` 跳过 Drop，因此 Goodbye 必须在退出前显式发送（已实现）。
- 未决：M2.2 是否把 `LAN_STUDIO_PORT` 收回 schema；配置热重载时 RR 集合如何在
  不中断收包循环的前提下原子替换（当前 `records` 已是 `RwLock`，具备条件）。
