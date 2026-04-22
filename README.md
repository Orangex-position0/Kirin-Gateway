# Kirin Gateway

[English](README.en.md)

基于 [Pingora](https://github.com/cloudflare/pingora) 框架的 Rust API 网关，提供路由匹配、反向代理、负载均衡、Filter Chain 和令牌桶限流等核心能力。

## 特性

- **路由匹配** — 支持精确匹配、前缀匹配（最长前缀优先）、正则匹配，优先级：精确 > 正则 > 前缀
- **Filter Chain** — 可扩展的过滤器链，内置白名单、方法校验、JWT 认证、限流、Header 注入、日志六个过滤器
- **负载均衡** — 基于 Pingora LoadBalancer 的轮询算法
- **令牌桶限流** — 按 IP 维度的进程内令牌桶限流，支持动态更新策略参数
- **JWT 认证** — RS256 签名验证，支持将 Claims 透传给上游服务
- **控制面 / 数据面分离** — 共享状态通过 `Arc<RwLock<GatewayState>>` 管理，控制面负责配置加载与热重载，数据面负责请求转发
- **配置热重载** — 文件监听 + 去抖动，运行时自动重载配置
- **Admin API** — 提供路由、上游集群、限流配置的查询接口及手动重载接口

## 快速开始

### 环境要求

- Rust edition 2024（Rust 1.85+）
- 依赖 Pingora 0.8.0

### 构建

```bash
cargo build --release
```

### 配置

复制配置模板并修改：

```bash
cp config.example.zh.yaml config.yaml
```

配置文件示例：

```yaml
server:
  listen: "0.0.0.0:6188"
  threads: 2

admin:
  listen: "0.0.0.0:6189"

routes:
  - route_id: "user-route"
    path: /api/users
    match_type: exact
    upstream: user-service
    applicant: "developer"
    applied_at: "2026-04-22T00:00:00+08:00"
    description: "用户服务接口"
  - route_id: "order-route"
    path_prefix: /api/orders
    match_type: prefix
    upstream: order-service
    applicant: "developer"
    applied_at: "2026-04-22T00:00:00+08:00"
    description: "订单服务接口"
  - route_id: "default-route"
    path_prefix: /api
    match_type: prefix
    upstream: default-service
    applicant: "developer"
    applied_at: "2026-04-22T00:00:00+08:00"
    description: "默认兜底路由"

upstreams:
  user-service:
    nodes:
      - addr: "127.0.0.1:8081"
        weight: 1
  order-service:
    nodes:
      - addr: "127.0.0.1:8082"
        weight: 1
      - addr: "127.0.0.1:8083"
        weight: 1
  default-service:
    nodes:
      - addr: "127.0.0.1:8090"
        weight: 1

rate_limit:
  capacity: 100
  refill_rate: 10
```

完整的配置参数说明参见 [config.example.zh.yaml](config.example.zh.yaml)。

### 运行

```bash
# 使用默认配置文件 config.yaml
cargo run

# 指定配置文件
cargo run -- /path/to/config.yaml
```

### 测试

```bash
cargo test
```

## 架构

```
Client Request
    │
    ▼
┌─────────────────────────────────────────────────┐
│                  KirinProxy                      │
│                                                  │
│  1. upstream_peer                                │
│     ├── 路由匹配（精确 > 正则 > 前缀）           │
│     └── 负载均衡选择上游节点                      │
│                                                  │
│  2. request_filter (Filter Chain 请求阶段)       │
│     WhiteList → Method → Auth → RateLimit        │
│     → Header → Logging                           │
│                                                  │
│  3. 转发请求到上游节点                            │
│                                                  │
│  4. response_filter (Filter Chain 响应阶段)      │
│     WhiteList → Method → Auth → RateLimit        │
│     → Header → Logging                           │
│                                                  │
│  5. 返回响应给客户端                              │
└─────────────────────────────────────────────────┘
```

### 控制面 / 数据面分离

```
┌──────────────┐     Arc<RwLock<GatewayState>>     ┌──────────────┐
│  Control     │ ◄──────────────────────────────────► │  Data Plane  │
│  Plane       │                                    │              │
│              │  - 配置加载与校验                    │ - 路由匹配   │
│  - YAML 解析 │  - GatewayState 构建                │ - Filter 链  │
│  - 热重载    │  - 文件监听 + 去抖动                │ - 代理转发   │
│  - Admin API │                                    │ - 负载均衡   │
└──────────────┘                                    └──────────────┘
```

### 项目结构

```
src/
├── main.rs                              # 入口：加载配置、启动服务
├── config.rs                            # YAML 配置加载与反序列化
├── data_plane/                          # 数据面
│   ├── proxy.rs                         # KirinProxy（ProxyHttp 实现）
│   ├── router.rs                        # 路由匹配器（精确/前缀/正则）
│   │   └── router_white_list.rs         # 接口注册表（白名单校验）
│   ├── upstream.rs                      # 上游集群（负载均衡封装）
│   ├── rate_limit.rs                    # 令牌桶限流器
│   └── filter/                          # Filter Chain
│       ├── whitelist.rs                 # 白名单 Filter
│       ├── method.rs                    # HTTP 方法校验 Filter
│       ├── auth.rs                      # JWT 认证 Filter
│       ├── rate_limit_filter.rs         # 限流 Filter
│       ├── header.rs                    # Header 注入 Filter
│       └── logging.rs                   # 日志 Filter
└── control_plane/                       # 控制面
    ├── control_plane.rs                 # 配置加载、热重载、文件监听
    ├── gateway_state.rs                 # 网关共享状态（GatewayState）
    ├── admin_api.rs                     # Admin API 代理服务
    │   └── dto.rs                       # Admin API 数据传输对象
    └── health_check.rs                  # TCP 健康检查配置
```

## Admin API

当配置了 `admin.listen` 时，管理接口自动启用。

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/admin/routes` | 查询所有路由规则 |
| GET | `/admin/upstreams` | 查询所有上游集群信息 |
| GET | `/admin/rate-limit` | 查询当前限流配置 |
| POST | `/admin/reload` | 手动触发配置热重载 |

所有接口返回统一的 JSON 格式：

```json
{
  "status": "ok",
  "data": { ... }
}
```

```json
{
  "status": "error",
  "message": "错误原因"
}
```

## Filter Chain

过滤器按顺序执行，请求阶段任一 Filter 返回 `Stop` 将中断链路并直接返回错误响应。

| 顺序 | Filter | 说明 |
|------|--------|------|
| 1 | WhiteList | 校验请求路径是否在接口注册表中 |
| 2 | Method | 校验 HTTP 方法是否被路由规则允许 |
| 3 | Auth | JWT RS256 认证（仅 `is_auth: true` 的路由） |
| 4 | RateLimit | 基于 IP 的令牌桶限流 |
| 5 | Header | 注入 `X-Gateway` / `X-Powered-By` 响应头 |
| 6 | Logging | 请求与响应日志记录 |

## 路由匹配优先级

1. **精确匹配** — HashMap O(1) 查找
2. **正则匹配** — 按声明顺序遍历，先声明优先
3. **前缀匹配** — 按前缀长度降序遍历，最长匹配优先

## License

MIT
