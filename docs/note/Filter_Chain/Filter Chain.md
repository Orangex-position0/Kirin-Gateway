# Filter Chain

## 背景：为什么用 Filter Chain 替代 Middleware

旧版中间件机制（`Middleware` trait + `Vec<Box<dyn Middleware>>`）存在以下问题：

1. **无短路能力** — 请求阶段通过 `Result<()>` 传播错误，但错误语义不明确（是网络错误还是拦截？），无法区分"拒绝请求"和"处理失败"
2. **白名单/方法校验/限流逻辑硬编码在 `KirinProxy` 中** — 代理既理解 Pingora 钩子机制，又理解业务逻辑，违反单一职责
3. **无共享上下文** — 中间件之间无法传递中间结果（如 WhiteList 查到的 `route_id`，Method 还要再查一次）

Filter Chain 通过**责任链模式 + 两阶段执行**解决了这些问题。

---

## 架构概览

```
Client Request
    ↓
KirinProxy.upstream_peer()       → 路由匹配 + 选节点 + 初始化 FilterContext
    ↓
KirinProxy.request_filter()      → FilterChain.run_request_filters()
    ↓                                   WhiteListFilter → MethodFilter → RateLimitFilter → HeaderFilter → LoggingFilter
    ↓                                   (遇到 Stop 立即中断)
    ↓
转发请求到上游
    ↓
KirinProxy.response_filter()     → FilterChain.run_response_filters()
    ↓                                   所有 Filter 顺序执行，不短路
    ↓
KirinProxy.logging()             → 最终耗时日志（保留在 Proxy 中，不属于 Filter）
    ↓
返回响应给客户端
```

### 三个核心组件

| 组件 | 职责 | 文件 |
|------|------|------|
| `KirinProxy` | 纯 Pingora 钩子分发器，不包含任何业务逻辑 | `src/data_plane/proxy.rs` |
| `FilterChain` | 编排器，持有有序 Filter 列表，管理两阶段执行 | `src/data_plane/filter/mod.rs` |
| `Filter` trait | 横切关注点的统一抽象，每个 Filter 只关注一件事 | `src/data_plane/filter/` |

---

## 核心类型

### FilterResult — 控制流

```rust
pub enum FilterResult {
    Continue,       // 继续执行下一个 Filter
    Stop(u16),      // 中断 Filter 链，附带 HTTP 错误状态码（由 KirinProxy 统一返回）
}
```

与旧版 `Result<()>` 的区别：`Stop` 携带状态码，语义明确为"拦截"，而非"出错"。

### FilterContext — 共享上下文

```rust
pub struct FilterContext {
    pub path: String,                        // 请求路径
    pub method: String,                      // HTTP 方法
    pub client_ip: String,                   // 客户端 IP
    pub upstream_name: Option<String>,       // 上游集群名称（路由匹配后设置）
    pub route_id: Option<String>,            // 路由 ID（WhiteListFilter 设置，MethodFilter 消费）
    pub start_time: std::time::Instant,      // 请求起始时间
    pub rate_limit_remaining: Option<usize>, // 限流剩余令牌数（RateLimitFilter 设置）
}
```

**设计决策**：所有 Filter 通过读写同一个 `FilterContext` 传递数据，而非各自持有依赖。

**隐式时序耦合**：MethodFilter 依赖 WhiteListFilter 先设置 `route_id`。如果 MethodFilter 排在 WhiteListFilter 之前，`route_id` 为 `None`，会直接放行（见 MethodFilter 源码 L25-28 的防御逻辑）。

### Filter trait

```rust
#[async_trait]
pub trait Filter: Send + Sync {
    fn name(&self) -> &str;

    // 请求阶段 — 支持短路
    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult;

    // 响应阶段 — 所有 Filter 都执行，不短路
    async fn response_filter(
        &self,
        ctx: &mut FilterContext,
        response_header: &mut ResponseHeader,
    );
}
```

**与旧版 Middleware 的对比**：

| | Middleware | Filter |
|--|-----------|--------|
| 请求阶段返回值 | `Result<()>` | `FilterResult`（语义明确） |
| 短路能力 | 依赖 `?` 错误传播 | `Stop(code)` 显式拦截 |
| 共享上下文 | 无 | `FilterContext` |
| 响应阶段签名 | `Result<()>` | `()`（无返回值，不短路） |
| Filter 间协作 | 不支持 | 通过 `FilterContext` 传递数据 |

### FilterChain — 编排器

```rust
#[derive(Clone)]
pub struct FilterChain {
    filters: Vec<Arc<dyn Filter>>,
}
```

两个核心方法：

- `run_request_filters()` — 按 Filter 注册顺序执行，遇到 `Stop` 立即返回
- `run_response_filters()` — 所有 Filter 顺序执行，不短路

`FilterChain` 实现了 `Clone`，因为 `KirinProxy` 在 `request_filter` 和 `response_filter` 中需要 clone 后释放 `RwLockReadGuard`，避免 guard 跨 `.await`。

---

## 内置 Filter

### 执行顺序

```
WhiteListFilter → MethodFilter → RateLimitFilter → HeaderFilter → LoggingFilter
```

**为什么这个顺序？**

1. **WhiteList** — 第一道关卡，未注册接口直接 403，fail fast
2. **Method** — 第二道关卡，方法不允许直接 403
3. **RateLimit** — 第三道关卡，只让合法请求消耗令牌配额
4. **Header** — 增强型 Filter，注入网关标识头，不影响请求是否通过
5. **Logging** — 观察型 Filter，记录请求/响应信息，排在最后

> 如果 RateLimit 排在 WhiteList 之前，非法请求会消耗限流配额，导致合法请求被误拒。

### WhiteListFilter

| 阶段 | 行为 |
|------|------|
| 请求 | 查 `RouteRegistry.resolve_path()`，未注册 → `Stop(403)`，已注册 → 设置 `ctx.route_id` |
| 响应 | 无操作 |

### MethodFilter

| 阶段 | 行为 |
|------|------|
| 请求 | 从 `ctx.route_id` 获取路由条目，检查 HTTP 方法是否在允许列表中。空列表放行所有方法 |
| 响应 | 无操作 |

**防御逻辑**：如果 `ctx.route_id` 为 `None`（WhiteListFilter 未执行或被跳过），直接 `Continue` 放行。

### RateLimitFilter

| 阶段 | 行为 |
|------|------|
| 请求 | 从 `GatewayState` 获取 `RateLimiter`，执行令牌桶检查。超限 → `Stop(429)`，通过 → 设置 `ctx.rate_limit_remaining` |
| 响应 | 注入 `X-RateLimit-Remaining` 响应头 |

**唯一同时在两个阶段都有逻辑的 Filter**。请求阶段做检查，响应阶段注入头（此时才拿到上游响应）。

### HeaderFilter

| 阶段 | 行为 |
|------|------|
| 请求 | 插入 `X-Gateway: Kirin Gateway` |
| 响应 | 插入 `X-Powered-By: Kirin Gateway` |

最简单的 Filter，无依赖，纯注入逻辑。

### LoggingFilter

| 阶段 | 行为 |
|------|------|
| 请求 | 记录 `method + path` |
| 响应 | 记录 `status code` |

与 `KirinProxy.logging()` 的区别：后者记录**最终耗时和上游名称**，属于代理职责，不属于 Filter。

---

## KirinProxy 简化

重构前后对比：

| 钩子方法 | 重构前 | 重构后 |
|----------|--------|--------|
| `upstream_peer` | 白名单 + 方法校验 + 路由 + 选节点 | **路由 + 选节点 + 初始化 FilterContext** |
| `request_filter` | 限流逻辑 | **委托 FilterChain，处理 Stop 时统一调用 `respond_error`** |
| `upstream_request_filter` | 中间件请求链 | **空实现（已废弃）** |
| `response_filter` | 中间件响应链 + 限流头 | **委托 FilterChain** |
| `logging` | 耗时日志 | **保留（最终日志，不属于 Filter）** |

关键变化：错误响应统一由 `KirinProxy.request_filter()` 在收到 `Stop(code)` 后调用 `session.respond_error(code)`。Filter 本身不直接操作 Session，只返回状态码。

---

## 与 GatewayState 的集成

`GatewayState` 持有 `FilterChain` 实例，在 `from_config()` 中自动构建默认链：

```rust
pub struct GatewayState {
    pub router: Router,
    pub registry: RouteRegistry,
    pub clusters: HashMap<String, Arc<UpstreamCluster>>,
    pub filter_chain: FilterChain,            // 替代旧版 middlewares
    pub rate_limiter: Option<Arc<RateLimiter>>,
}
```

Filter 通过 `state: &Arc<RwLock<GatewayState>>` 参数读取共享状态（`RouteRegistry`、`RateLimiter` 等）。读锁在获取数据后立即释放，不跨 `.await` 点。

---

## 模块结构

```
src/data_plane/filter/
├── mod.rs               # Filter trait + FilterResult + FilterContext + FilterChain + 子模块声明
├── whitelist.rs         # WhiteListFilter
├── method.rs            # MethodFilter
├── rate_limit_filter.rs # RateLimitFilter
├── header.rs            # HeaderFilter
└── logging.rs           # LoggingFilter
```

> Rust 中 `filter.rs` 和 `filter/` 不能共存。实际采用 `filter/mod.rs` 方案，核心类型和子模块声明在同一个文件中。

---

## 扩展指南

新增 Filter 只需三步：

1. 在 `src/data_plane/filter/` 下新建文件，实现 `Filter` trait
2. 在 `filter/mod.rs` 中添加 `pub mod xxx;`
3. 在 `GatewayState::build_default_filter_chain()` 中注册，注意顺序

示例 — 新增 AuthFilter（放在 Method 之后、RateLimit 之前）：

```rust
pub struct AuthFilter;

#[async_trait]
impl Filter for AuthFilter {
    fn name(&self) -> &str { "auth-filter" }

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        _state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        // 从请求头提取并验证 JWT
        // 验证失败 → FilterResult::Stop(401)
        FilterResult::Continue
    }

    async fn response_filter(
        &self,
        _ctx: &mut FilterContext,
        _response_header: &mut ResponseHeader,
    ) {
        // 无操作
    }
}
```
