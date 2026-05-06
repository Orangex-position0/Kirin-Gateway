# Kirin Gateway 配置参考

配置文件为 YAML 格式，通过命令行参数指定路径（默认 `config.yaml`）。

```bash
kirin-gw /path/to/config.yaml
```

---

## 顶层结构

| 字段           | 类型               | 必填 | 说明                 |
|--------------|------------------|----|--------------------|
| `server`     | ServerConfig     | 是  | 服务监听配置             |
| `routes`     | RouteConfig[]    | 是  | 路由规则列表             |
| `upstreams`  | UpstreamConfig{} | 是  | 上游服务配置（key 为服务名称）  |
| `rate_limit` | RateLimitConfig  | 否  | 令牌桶限流配置，删除整节则不启用   |
| `admin`      | AdminConfig      | 否  | 管理 API 配置，删除整节则不启用 |
| `auth`       | AuthConfigRaw    | 否  | JWT 认证配置，删除整节则不启用  |

---

## server — 服务监听

| 字段        | 类型     | 必填 | 默认值     | 说明                  |
|-----------|--------|----|---------|---------------------|
| `listen`  | string | 是  | —       | 监听地址，格式 `"ip:port"` |
| `threads` | usize  | 否  | CPU 核心数 | 工作线程数               |
| `tls`     | TlsConfig | 否 | — | TLS 终止配置，不配置则 HTTP 明文模式 |

### TlsConfig

| 字段          | 类型     | 必填 | 说明                |
|-------------|--------|----|-------------------|
| `cert_path` | string | 是  | TLS 证书文件路径（PEM 格式） |
| `key_path`  | string | 是  | TLS 私钥文件路径（PEM 格式） |

```yaml
server:
  listen: "0.0.0.0:6188"
  threads: 2
  tls:
    cert_path: "certs/server.crt"
    key_path: "certs/server.key"
```

---

## routes — 路由规则

路由匹配优先级：**精确匹配 > 正则匹配（按声明顺序）> 前缀匹配（最长前缀优先）**。

| 字段            | 类型       | 必填 | 默认值          | 说明                                         |
|---------------|----------|----|--------------|--------------------------------------------|
| `route_id`    | string   | 是  | —            | 接口唯一标识，用于注册和管理                             |
| `path`        | string   | 条件 | —            | 精确路径（`match_type` 为 `exact` 或 `regex` 时使用） |
| `path_prefix` | string   | 条件 | —            | 前缀路径（`match_type` 为 `prefix` 时使用）          |
| `match_type`  | string   | 否  | `"exact"`    | 匹配类型：`exact` / `prefix` / `regex`          |
| `methods`     | string[] | 否  | `[]`（放行所有方法） | 允许的 HTTP 方法列表                              |
| `upstream`    | string   | 是  | —            | 转发目标的上游服务名称                                |
| `applicant`   | string   | 是  | —            | 申请人                                        |
| `applied_at`  | string   | 是  | —            | 申请时间（ISO 8601 格式）                          |
| `description` | string   | 是  | —            | 接口场景说明                                     |
| `is_auth`     | bool     | 否  | `false`      | 是否需要 JWT 认证                                |

> `path` 和 `path_prefix` 二选一，取决于 `match_type`。

```yaml
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

  - route_id: "admin-route"
    path: "/api/admin/.*"
    match_type: regex
    methods: ["GET", "POST"]
    upstream: admin-service
    is_auth: true
    applicant: "developer"
    applied_at: "2026-04-22T00:00:00+08:00"
    description: "管理后台接口"
```

### match_type 说明

| 值        | 行为                         | 示例                                    |
|----------|----------------------------|---------------------------------------|
| `exact`  | 精确匹配路径                     | `path: /api/users` 只匹配该路径             |
| `prefix` | 前缀匹配，多个前缀规则按长度降序匹配（最长匹配优先） | `path_prefix: /api` 匹配所有 `/api` 开头的路径 |
| `regex`  | 正则表达式匹配                    | `path: "/api/v\\d+/.*"` 匹配版本化 API     |

---

## upstreams — 上游服务

| 字段      | 类型           | 必填 | 默认值 | 说明     |
|---------|--------------|----|------|--------|
| `nodes` | NodeConfig[] | 是  | —    | 后端节点列表 |
| `algorithm` | string   | 否  | `"round_robin"` | 负载均衡算法，可选 `round_robin` / `consistent_hash` |
| `health_check` | HealthCheckConfig | 否 | — | 健康检查配置 |

### NodeConfig

| 字段       | 类型     | 必填 | 默认值 | 说明                  |
|----------|--------|----|-----|---------------------|
| `addr`   | string | 是  | —   | 节点地址，格式 `"ip:port"` |
| `weight` | usize  | 否  | `1` | 负载均衡权重，权重越高分配的请求越多  |

```yaml
upstreams:
  user-service:
    algorithm: round_robin
    nodes:
      - addr: "127.0.0.1:8081"
        weight: 2
      - addr: "127.0.0.1:8082"
        weight: 1
    health_check:
      interval_secs: 5
      timeout_secs: 3
```

### HealthCheckConfig

| 字段             | 类型   | 必填 | 默认值 | 说明       |
|----------------|------|----|-----|----------|
| `interval_secs` | u64  | 是  | —   | 检查间隔（秒） |
| `timeout_secs`  | u64  | 否  | `3` | 连接超时（秒） |

---

## rate_limit — 限流配置

基于令牌桶算法，按客户端 IP 独立限流。

| 字段            | 类型    | 必填 | 说明             |
|---------------|-------|----|----------------|
| `capacity`    | usize | 是  | 令牌桶容量（最大突发请求数） |
| `refill_rate` | usize | 是  | 每秒补充的令牌数（平均速率） |

```yaml
rate_limit:
  capacity: 100
  refill_rate: 10
```

---

## admin — 管理 API

| 字段       | 类型     | 必填 | 说明          |
|----------|--------|----|-------------|
| `listen` | string | 是  | 管理 API 监听地址 |

```yaml
admin:
  listen: "127.0.0.1:9090"
```

---

## auth — JWT 认证

启用后，`is_auth: true` 的路由将要求请求携带有效的 JWT Token。

| 字段                  | 类型       | 必填 | 默认值  | 说明                                             |
|---------------------|----------|----|------|------------------------------------------------|
| `algorithm`         | string   | 是  | —    | 签名算法，当前仅支持 `RS256`                             |
| `public_key_path`   | string   | 是  | —    | RSA 公钥文件路径（PEM 格式）                             |
| `issuer`            | string   | 是  | —    | 期望的 Token 签发者（iss 字段）                          |
| `claims_to_forward` | string[] | 否  | `[]` | 需要透传给上游的 JWT claims，以 `X-User-{ClaimName}` 头传递 |

```yaml
auth:
  algorithm: "RS256"
  public_key_path: "/etc/kirin/public.pem"
  issuer: "auth-service"
  claims_to_forward: ["sub", "exp"]
```
