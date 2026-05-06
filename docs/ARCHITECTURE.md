# Kirin Gateway 架构文档

## 目录

- [整体架构](#整体架构)
- [启动流程](#启动流程)
- [控制面 / 数据面分离](#控制面--数据面分离)
- [请求处理流程](#请求处理流程)
- [路由匹配](#路由匹配)
- [Filter Chain](#filter-chain)
- [负载均衡](#负载均衡)
- [令牌桶限流](#令牌桶限流)
- [配置热重载](#配置热重载)
- [Admin API](#admin-api)
- [可观测性](#可观测性)
- [项目结构](#项目结构)

---

## 整体架构

Kirin Gateway 采用 **控制面 / 数据面分离** 架构，共享状态通过 `Arc<RwLock<GatewayState>>` 桥接两个面：

```mermaid
graph TB
    subgraph Client
        C[客户端请求]
    end

    subgraph DataPlane["数据面 (Data Plane)"]
        KP[KirinProxy<br/>ProxyHttp 实现]
        RM[路由匹配器 Router]
        FC[Filter Chain]
        LB[负载均衡器]
        RL[令牌桶限流器]
    end

    subgraph ControlPlane["控制面 (Control Plane)"]
        CP[ControlPlane]
        GS[GatewayState]
        AA[Admin API]
        FW[文件监听器]
        HC[健康检查]
    end

    subgraph Observability["可观测性"]
        MT[Prometheus Metrics]
        LG[Tracing 日志]
    end

    subgraph Upstream["上游服务"]
        U1[Node 1]
        U2[Node 2]
        U3[Node N]
    end

    C --> KP
    KP -->|读锁| GS
    KP --> RM
    KP --> FC
    KP --> LB
    FC --> RL

    GS -.->|Arc RwLock| KP

    CP -->|写锁| GS
    FW -->|文件变更| CP
    AA -->|读锁/触发重载| GS
    AA --> CP
    HC --> LB

    KP --> MT
    KP --> LG

    LB --> U1
    LB --> U2
    LB --> U3
```

---

## 启动流程

```mermaid
sequenceDiagram
    participant Main as main.rs
    participant Config as config.rs
    participant GS as GatewayState
    participant CP as ControlPlane
    participant FW as FileWatcher
    participant KP as KirinProxy
    participant Pingora as Pingora Server
    participant Admin as AdminProxy

    Main->>Config: load_config("config.yaml")
    Config-->>Main: KirinConfig

    Main->>GS: GatewayState::from_config(config)
    Note over GS: 构建路由表 + 接口注册表<br/>+ 集群注册表 + FilterChain + 限流器
    GS-->>Main: GatewayState

    Main->>Main: Arc::new(RwLock::new(state))

    Main->>CP: ControlPlane::new(state, config_path)
    Main->>FW: start_file_watcher(500ms)
    Note over FW: 后台线程监听配置变更

    Main->>KP: KirinProxy::new(state)

    Main->>Pingora: Server::new() + bootstrap()
    Main->>Pingora: add_tcp / add_tls(listen)
    Main->>Pingora: add_service(kirin_proxy)

    alt admin 配置存在
        Main->>Admin: AdminProxy::new(state, control_plane)
        Main->>Pingora: add_service(admin_proxy)
    end

    Main->>Pingora: run_forever()
```

---

## 控制面 / 数据面分离

两个面通过 `Arc<RwLock<GatewayState>>` 共享运行时状态，遵循 **控制面写、数据面读** 的原则：

```mermaid
graph LR
    subgraph 控制面
        YAML[YAML 配置文件]
        CP[ControlPlane]
        FW[FileWatcher]
        AA[Admin API]
    end

    subgraph 共享状态
        GS["GatewayState<br/><br/>• Router 路由表<br/>• RouteRegistry 接口注册表<br/>• HashMap&lt;String, Arc&lt;UpstreamCluster&gt;&gt;<br/>• FilterChain<br/>• Option&lt;RateLimiter&gt;<br/>• Option&lt;AuthConfig&gt;<br/>• KirinConfig 快照"]
    end

    subgraph 数据面
        KP[KirinProxy]
    end

    YAML -->|加载| CP
    FW -->|文件变更| CP
    AA -->|手动重载| CP
    CP -->|write lock| GS
    KP -->|read lock| GS
```

| 面 | 职责 | 锁类型 |
|---|---|---|
| **控制面** | 配置加载、校验、热重载、Admin API | 写锁 (write) |
| **数据面** | 路由匹配、Filter Chain、代理转发、负载均衡 | 读锁 (read) |

---

## 请求处理流程

一个完整的请求经过 Pingora `ProxyHttp` trait 的多个生命周期钩子：

```mermaid
sequenceDiagram
    participant C as 客户端
    participant KP as KirinProxy
    participant RF as request_filter
    participant UP as upstream_peer
    participant URF as upstream_request_filter
    participant US as 上游服务
    participant RSF as response_filter
    participant LOG as logging

    C->>KP: HTTP 请求

    Note over KP: Pingora 调用顺序
    KP->>RF: ① request_filter
    Note over RF: 初始化 FilterContext<br/>（path, method, client_ip）<br/>执行 Filter Chain 请求阶段<br/>Method → Auth → RateLimit → Header → Logging

    alt Filter 返回 Stop
        RF-->>C: 错误响应 (401/403/405/429/500)
    end

    KP->>UP: ② upstream_peer
    Note over UP: 路由匹配（精确 > 正则 > 前缀）<br/>白名单校验 (RouteRegistry)<br/>负载均衡选择上游节点

    alt 路由未匹配
        UP-->>C: 502 No route matched
    else 白名单校验失败
        UP-->>C: 403 Route not registered
    else 无可用节点
        UP-->>C: 502 无可用上游节点
    end

    KP->>URF: ③ upstream_request_filter
    Note over URF: 注入 X-Gateway 请求头

    KP->>US: ④ 转发请求到上游
    US-->>KP: 上游响应

    KP->>RSF: ⑤ response_filter
    Note over RSF: 执行 Filter Chain 响应阶段<br/>（所有 Filter 都执行，不短路）

    KP->>LOG: ⑥ logging
    Note over LOG: 记录请求耗时<br/>更新 Prometheus 指标

    KP-->>C: HTTP 响应
```

### 各阶段详情

| 阶段 | Pingora 钩子 | 主要职责 |
|------|-------------|---------|
| ① | `request_filter` | 初始化 `FilterContext`；执行 Filter Chain 请求阶段（Method → Auth → RateLimit → Header → Logging）；处理 `/metrics` 端点 |
| ② | `upstream_peer` | 路由匹配 + 白名单校验 + 负载均衡选节点；返回 `HttpPeer` |
| ③ | `upstream_request_filter` | 注入 `X-Gateway: Kirin Gateway` 请求头给上游 |
| ④ | Pingora 内部 | 将请求转发到选定的上游节点 |
| ⑤ | `response_filter` | 执行 Filter Chain 响应阶段（注入响应头、日志等） |
| ⑥ | `logging` | 记录请求耗时、更新 Prometheus 计数器/直方图 |

---

## 路由匹配

`Router` 维护三张路由表，按优先级依次匹配：

```mermaid
flowchart TD
    REQ[请求路径] --> EXACT{精确匹配<br/>HashMap O 1}
    EXACT -->|命中| HIT[返回 RouteMatch]
    EXACT -->|未命中| REGEX{正则匹配<br/>按声明顺序遍历}

    REGEX -->|命中| HIT
    REGEX -->|未命中| PREFIX{前缀匹配<br/>按长度降序遍历<br/>最长匹配优先}

    PREFIX -->|命中| HIT
    PREFIX -->|未命中| NONE[返回 None<br/>502 No route matched]

    style EXACT fill:#4CAF50,color:#fff
    style REGEX fill:#FF9800,color:#fff
    style PREFIX fill:#2196F3,color:#fff
    style HIT fill:#8BC34A,color:#fff
    style NONE fill:#F44336,color:#fff
```

| 匹配类型 | 数据结构 | 复杂度 | 说明 |
|---------|---------|--------|------|
| 精确匹配 (Exact) | `HashMap<String, RouteRule>` | O(1) | 路径完全一致 |
| 正则匹配 (Regex) | `Vec<(Regex, RouteRule)>` | O(n) | 按声明顺序，先声明优先 |
| 前缀匹配 (Prefix) | `Vec<(String, RouteRule)>` | O(n) | 按前缀长度降序，最长优先 |

白名单校验在 `upstream_peer` 阶段由 `RouteRegistry.resolve_path()` 执行，与路由匹配使用相同的优先级策略。

---

## Filter Chain

Filter Chain 采用 **正序执行** 模式（非洋葱模型）：

- **请求阶段**：任一 Filter 返回 `Stop` 即中断链路，后续 Filter 不执行
- **响应阶段**：所有 Filter 都执行，不短路

```mermaid
flowchart LR
    subgraph 请求阶段["请求阶段 (request_filter)"]
        direction LR
        REQ[请求] --> M[Method<br/>HTTP 方法校验]
        M -->|Continue| A[Auth<br/>JWT RS256 认证]
        A -->|Continue| RL[RateLimit<br/>令牌桶限流]
        RL -->|Continue| H1[Header<br/>注入网关头]
        H1 -->|Continue| L1[Logging<br/>请求日志]
        L1 --> PASS[放行]

        M -->|Stop 405| E1[返回错误]
        A -->|Stop 401| E2[返回错误]
        RL -->|Stop 429| E3[返回错误]
    end

    subgraph 响应阶段["响应阶段 (response_filter)"]
        direction LR
        RESP[响应] --> M2[Method]
        M2 --> A2[Auth]
        A2 --> RL2[RateLimit<br/>注入 X-RateLimit-* 头]
        RL2 --> H2[Header<br/>X-Powered-By 等]
        H2 --> L2[Logging<br/>响应日志]
        L2 --> OUT[返回客户端]
    end

    style PASS fill:#4CAF50,color:#fff
    style E1 fill:#F44336,color:#fff
    style E2 fill:#F44336,color:#fff
    style E3 fill:#F44336,color:#fff
```

### Filter 接口

```rust
#[async_trait]
pub trait Filter: Send + Sync {
    fn name(&self) -> FilterName;

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult;  // Continue | Stop(FilterReject)

    async fn response_filter(
        &self,
        ctx: &mut FilterContext,
        response_header: &mut ResponseHeader,
    );
}
```

### FilterContext 字段

| 字段 | 类型 | 来源 |
|------|------|------|
| `path` | `String` | `request_filter` 阶段从 session 提取 |
| `method` | `String` | `request_filter` 阶段从 session 提取 |
| `client_ip` | `String` | `request_filter` 阶段从 session 提取 |
| `upstream_name` | `Option<String>` | `upstream_peer` 阶段设置 |
| `route_id` | `Option<String>` | `upstream_peer` 阶段设置（白名单校验后） |
| `start_time` | `Instant` | `request_filter` 阶段初始化 |
| `rate_limit_remaining` | `Option<usize>` | RateLimit Filter 设置 |
| `auth_user_id` | `Option<String>` | Auth Filter 设置 |

---

## 负载均衡

`UpstreamCluster` 封装了 Pingora `LoadBalancer`，通过 `LoadBalancerKind` 枚举支持多种算法：

```mermaid
classDiagram
    class UpstreamCluster {
        +name: String
        +lb: LoadBalancerKind
        +addrs: Vec~String~
        +health_check_enabled: bool
        +from_config(name, upstream_cfg) Result
        +select_peer(key) Option~HttpPeer~
        +summary() UpstreamDTO
    }

    class LoadBalancerKind {
        +RoundRobin(Arc~LoadBalancer~)
        +Consistent(Arc~LoadBalancer~)
        +select(key, max_iterations) Option~Backend~
    }

    UpstreamCluster --> LoadBalancerKind

    note for LoadBalancerKind "round_robin: 轮询（支持加权）\nconsistent_hash: 一致性哈希"
```

| 算法 | 配置值 | 特点 |
|------|-------|------|
| 加权轮询 | `round_robin` | 按 weight 比例分配，支持加权 |
| 一致性哈希 | `consistent_hash` | 相同 key 总是路由到相同节点 |

节点选择时，`select_peer(key)` 使用客户端 IP 的字节作为 key。

---

## 令牌桶限流

基于 IP 维度的进程内令牌桶限流：

```mermaid
flowchart TD
    REQ[请求到达] --> RL{RateLimiter}

    RL -->|获取 IP| BUCKET{查找 IP 对应的 TokenBucket}

    BUCKET -->|不存在| CREATE[创建新桶<br/>初始令牌 = capacity]
    CREATE --> REFILL
    BUCKET -->|已存在| REFILL[补充令牌<br/>tokens += elapsed * refill_rate<br/>不超过 capacity]

    REFILL --> CHECK{current_tokens > 0?}
    CHECK -->|是| ACQUIRE[消耗 1 个令牌<br/>返回 (true, remaining)]
    CHECK -->|否| REJECT[返回 (false, 0)<br/>HTTP 429]

    ACQUIRE --> PASS[放行请求]
    REJECT --> STOP[Filter Chain 中断]

    style PASS fill:#4CAF50,color:#fff
    style STOP fill:#F44336,color:#fff
```

```mermaid
classDiagram
    class RateLimiter {
        +enabled: bool
        +capacity: AtomicUsize
        +refill_rate: AtomicUsize
        +buckets: Mutex~HashMap~String, TokenBucket~~
        +check(ip) (bool, usize)
        +update_policy(capacity, refill_rate)
        +summary() RateLimitDTO
    }

    class TokenBucket {
        +capacity: usize
        +current_tokens: usize
        +refill_rate: usize
        +last_refill: Instant
        +try_acquire() bool
        +remaining() usize
        -refill()
    }

    RateLimiter *-- TokenBucket : 每个 IP 一个桶
```

关键设计：
- `capacity` 和 `refill_rate` 使用 `AtomicUsize`，支持运行时动态更新
- 热重载时保留现有令牌桶实例和状态，仅更新策略参数

---

## 配置热重载

支持两种热重载策略：

```mermaid
flowchart TD
    CHANGE[配置文件变更] --> FW[FileWatcher 检测到事件]
    FW --> DEBOUNCE[去抖动 500ms]
    DEBOUNCE -->|无新事件| RELOAD

    subgraph RELOAD["热重载策略"]
        direction TB
        DIFF[增量热重载 reload_diff<br/>文件监听自动触发]
        SIMPLE[全量热重载 reload_simple<br/>Admin API 手动触发]
    end

    DIFF --> PARSE1[解析新配置]
    PARSE1 --> VALIDATE1[全量配置校验]
    VALIDATE1 --> DIFFOP[增量 Diff 操作]
    DIFFOP --> DIFF_ROUTE[Diff 路由<br/>新增 / 更新 / 删除]
    DIFFOP --> DIFF_CLUSTER[Diff 上游集群<br/>保留未变更实例]
    DIFFOP --> DIFF_RL[Diff 限流器<br/>保留令牌桶状态]
    DIFFOP --> DIFF_AUTH[Diff 认证配置]

    SIMPLE --> PARSE2[解析新配置]
    PARSE2 --> VALIDATE2[配置校验]
    VALIDATE2 --> REPLACE[整体替换 GatewayState]

    style DIFF fill:#4CAF50,color:#fff
    style SIMPLE fill:#FF9800,color:#fff
```

### 增量 Diff 策略

```mermaid
flowchart LR
    subgraph 路由 Diff
        R_OLD[旧路由 ID 集合] --> R_DIFF{集合差集}
        R_DIFF -->|旧有新无| R_DEL[删除路由 + 注销注册表]
        R_DIFF -->|新有旧无| R_ADD[新增路由]
        R_DIFF -->|都有但内容变更| R_UPD[删除旧 + 添加新]
    end

    subgraph 上游集群 Diff
        C_OLD[旧集群名称集合] --> C_DIFF{集合差集}
        C_DIFF -->|旧有新无| C_DEL[删除集群]
        C_DIFF -->|新有旧无| C_ADD[新增集群]
        C_DIFF -->|未变更| C_KEEP[保持原实例<br/>保留健康检查状态]
    end

    subgraph 限流器 Diff
        RL_OLD[旧限流配置] --> RL_DIFF{对比参数}
        RL_DIFF -->|无→有| RL_CREATE[创建新限流器]
        RL_DIFF -->|有→无| RL_REMOVE[移除限流器]
        RL_DIFF -->|参数变更| RL_UPDATE[更新策略<br/>保留桶状态]
    end
```

---

## Admin API

Admin API 是独立的 Pingora HTTP 服务，运行在独立端口，直接使用 `request_filter` 返回响应（不代理到上游）：

```mermaid
sequenceDiagram
    participant Client as 管理客户端
    participant Admin as AdminProxy
    participant GS as GatewayState
    participant CP as ControlPlane

    Client->>Admin: GET /admin/routes
    Admin->>GS: read lock
    GS-->>Admin: Router.routes_summary()
    Admin-->>Client: 200 {"status":"ok","data":[...]}

    Client->>Admin: GET /admin/upstreams
    Admin->>GS: read lock
    GS-->>Admin: clusters.summary()
    Admin-->>Client: 200 {"status":"ok","data":[...]}

    Client->>Admin: GET /admin/rate-limit
    Admin->>GS: read lock
    GS-->>Admin: rate_limiter.summary()
    Admin-->>Client: 200 {"status":"ok","data":{...}}

    Client->>Admin: POST /admin/reload
    Admin->>CP: reload_simple()
    CP->>GS: write lock → 替换 GatewayState
    Admin-->>Client: 200 {"status":"ok","data":"重载成功"}
```

| 方法 | 路径 | 锁类型 | 说明 |
|------|------|--------|------|
| GET | `/admin/routes` | 读锁 | 查询所有路由规则 |
| GET | `/admin/upstreams` | 读锁 | 查询所有上游集群 |
| GET | `/admin/rate-limit` | 读锁 | 查询限流配置 |
| POST | `/admin/reload` | 写锁 | 全量热重载 |

---

## 可观测性

```mermaid
flowchart TB
    subgraph Metrics["Prometheus 指标 (GET /metrics)"]
        RT[kirin_requests_total<br/>Counter {method, upstream, status_code}]
        RD[kirin_request_duration_seconds<br/>Histogram {method, upstream}]
        UE[kirin_upstream_errors_total<br/>Counter {method, upstream}]
        FR[kirin_filter_rejects_total<br/>Counter {filter_name, status_code}]
    end

    subgraph Logging["Tracing 日志 (JSON)"]
        REQ_LOG[请求日志<br/>method, path, upstream, elapsed_ms, client_ip]
        RELOAD_LOG[热重载日志<br/>路由/集群/限流器变更记录]
        FILTER_LOG[Filter 日志<br/>拒绝原因, Filter 名称]
    end

    RT --> PROM[Prometheus 采集]
    RD --> PROM
    UE --> PROM
    FR --> PROM

    REQ_LOG --> STDOUT[stdout JSON 输出]
    RELOAD_LOG --> STDOUT
    FILTER_LOG --> STDOUT
```

| 指标 | 类型 | 标签 | 说明 |
|------|------|------|------|
| `kirin_requests_total` | Counter | method, upstream, status_code | 请求总数 |
| `kirin_request_duration_seconds` | Histogram | method, upstream | 请求延迟分布 |
| `kirin_upstream_errors_total` | Counter | method, upstream | 上游错误总数 |
| `kirin_filter_rejects_total` | Counter | filter_name, status_code | Filter 拒绝总数 |

---

## 项目结构

```
src/
├── main.rs                              # 入口：加载配置、启动 Pingora Server
├── config/
│   ├── loader.rs                        # 配置文件加载（read + parse）
│   ├── types.rs                         # KirinConfig 等配置类型定义
│   └── validation.rs                    # 配置校验逻辑
├── control_plane/
│   ├── control_plane.rs                 # 控制面：配置加载、热重载、文件监听
│   ├── gateway_state.rs                 # 共享状态 GatewayState + 增量 Diff
│   ├── admin_api.rs                     # Admin API 代理服务
│   │   └── dto.rs                       # Admin API 数据传输对象
│   └── health_check.rs                  # TCP 健康检查
├── data_plane/
│   ├── proxy.rs                         # KirinProxy（ProxyHttp trait 实现）
│   ├── router.rs                        # 路由匹配器（精确/前缀/正则）
│   │   └── router_white_list.rs         # 接口注册表（白名单校验）
│   ├── upstream.rs                      # 上游集群（负载均衡封装）
│   ├── rate_limit.rs                    # 令牌桶限流器
│   ├── filter.rs                        # Filter trait + FilterChain 编排
│   └── filter/
│       ├── auth.rs                      # JWT RS256 认证 Filter
│       ├── header.rs                    # Header 注入 Filter
│       ├── logging.rs                   # 日志 Filter
│       ├── method.rs                    # HTTP 方法校验 Filter
│       ├── rate_limit_filter.rs         # 限流 Filter
│       └── whitelist.rs                 # 白名单 Filter
└── observability/
    └── metrics.rs                       # Prometheus 指标定义与收集
```
