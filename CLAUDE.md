# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
cargo build                          # 构建
cargo build --release                # Release 构建
cargo run                            # 运行（默认读取 config.yaml）
cargo run -- /path/to/config.yaml    # 指定配置文件
cargo test                           # 运行全部测试
cargo test --test integration_test   # 运行集成测试
cargo fmt                            # 格式化（edition 2024, max_width=100, Unix 换行）
```

## 项目概述

**Kirin Gateway** — 基于 [Pingora](https://github.com/cloudflare/pingora) 0.8.0 的 Rust API 网关。Crate 名 `kirin-gateway`，二进制名 `kirin-gw`，Rust edition 2024。

## 架构

### 请求处理流程

Client → `KirinProxy`（ProxyHttp trait 实现）→ 路由匹配（精确 > 正则 > 前缀）→ Filter Chain 请求阶段 → 负载均衡选节点 → 转发 → Filter Chain 响应阶段 → 返回客户端。

### 控制面 / 数据面分离

共享状态通过 `Arc<RwLock<GatewayState>>` 管理：

- **控制面** (`src/control_plane/`) — YAML 配置加载与校验、热重载（文件监听 + 去抖动）、Admin API、健康检查
- **数据面** (`src/data_plane/`) — 路由匹配、Filter Chain、代理转发、负载均衡、限流

### 模块职责

- **`config.rs`** — 配置加载入口（loader）+ 数据类型定义（types）+ 校验逻辑（validation）
- **`control_plane/control_plane.rs`** — 配置加载、热重载、文件监听
- **`control_plane/gateway_state.rs`** — `GatewayState` 构建，包含路由表/集群注册表/FilterChain/限流器
- **`control_plane/admin_api.rs`** — Admin API（hyper 实现），路由/上游/限流查询 + 手动重载
- **`data_plane/proxy.rs`** — `KirinProxy` 实现 `ProxyHttp` trait，编排请求生命周期
- **`data_plane/router.rs`** — 路由匹配器（精确/前缀/正则），优先级：精确 > 正则 > 前缀
- **`data_plane/upstream.rs`** — `UpstreamCluster` 封装 Pingora LoadBalancer，轮询算法
- **`data_plane/filter.rs`** — `Filter` trait 定义 + `FilterChain` 编排（非洋葱模型，正序执行）
- **`data_plane/filter/`** — 六个内置 Filter：WhiteList → Method → Auth(JWT RS256) → RateLimit → Header → Logging
- **`data_plane/rate_limit.rs`** — 基于 IP 的令牌桶限流器，进程内状态
- **`observability/metrics.rs`** — Prometheus 指标

### 关键设计

- `GatewayState` 是控制面和数据面的桥梁，包含 `Router`、`clusters`（HashMap）、`FilterChain`、`RateLimiter`
- `RequestContext` 作为每次请求的上下文，保存上游名称、起始时间、限流剩余令牌数
- Filter Chain 请求阶段任一 Filter 返回 `Stop` 将中断链路；非洋葱模型，正序执行请求和响应阶段
- Admin API 使用 hyper 直接实现（非 Pingora ProxyHttp），独立端口
- 健康检查为 TCP 后台服务，注册到 Pingora Server
- TLS Termination 通过 `server.tls` 配置启用
- 配置热重载：文件变更监听 + 500ms 去抖动，运行时替换 `GatewayState`
