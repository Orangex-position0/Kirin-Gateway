# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
# 构建
cargo build

# 运行（默认读取 config.yaml）
cargo run

# 指定配置文件
cargo run -- /path/to/config.yaml

# 运行测试
cargo tests

# 运行单个测试模块
cargo tests --lib router

# Release 构建
cargo build --release
```

## 项目概述

**Kirin Gateway** — 基于 [Pingora](https://github.com/cloudflare/pingora) 框架的 Rust API 网关，实现路由匹配、反向代理、负载均衡、中间件链和令牌桶限流。

Crate 名 `kirin-gateway`，二进制名 `kirin-gw`。Rust edition 2024，依赖 `pingora-core`/`pingora-proxy`/`pingora-load-balancing`/`pingora-http` 0.8.0。

## 架构

请求处理流程：Client → `KirinProxy`（Pingora `ProxyHttp` 实现）→ 路由匹配 → 限流检查 → 中间件请求过滤 → 负载均衡选择上游节点 → 转发请求 → 中间件响应过滤 → 返回客户端。

### 模块职责

- **`main.rs`** — 入口：加载配置、构建路由表/集群注册表/限流器/中间件链、启动 Pingora Server
- **`config.rs`** — YAML 配置加载与反序列化（`KirinConfig`），定义 server/routes/upstreams/rate_limit 结构
- **`router.rs`** — 路由匹配：精确匹配（HashMap O(1)）优先于前缀匹配（按前缀长度降序遍历，最长匹配优先）
- **`proxy.rs`** — `KirinProxy` 实现 `ProxyHttp` trait，编排整个请求生命周期：`upstream_peer`（路由+选节点）→ `request_filter`（限流）→ `upstream_request_filter`（中间件请求链）→ `response_filter`（中间件响应链+限流头）→ `logging`
- **`upstream.rs`** — `UpstreamCluster` 封装 Pingora `LoadBalancer<RoundRobin>`，当前仅支持轮询算法
- **`middleware.rs`** — `Middleware` trait 定义 `request_filter`/`response_filter` 两个异步钩子，内置 `HeaderMiddleware`（注入网关头）和 `LoggingMiddleware`（日志记录）
- **`rate_limit.rs`** — 基于 IP 的令牌桶限流器，`RateLimiter` 持有 `HashMap<String, TokenBucket>`，进程内状态，重启丢失
- **`health_check.rs`** — TCP 健康检查配置（当前未集成到主流程）

### 关键设计

- `RequestContext` 作为每次请求的上下文，保存上游名称、起始时间、限流剩余令牌数
- 中间件按正序执行请求和响应阶段（非洋葱模型）
- 配置文件路径通过命令行参数传入，默认 `config.yaml`
- 连接失败时 `fail_to_connect` 自动标记为可重试
