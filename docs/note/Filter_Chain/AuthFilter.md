# AuthFilter 笔记

## 核心记忆点

- RS256 非对称，网关只拿公钥，私钥在认证服务
- 路由级别 `is_auth: bool` 控制，默认 false
- 只校验 exp + iss，不校验 aud/iat
- 所有失败统一 401，不暴露原因

## Filter Chain 排序逻辑

```
WhiteList → Method → AuthFilter → RateLimit → Header → Logging
```

记住一个原则：**fail fast + 保护下游资源**

- Method 在 Auth 前面：避免对方法不合法的请求做昂贵的 RSA 验证
- Auth 在 RateLimit 前面：避免无效 Token 耗尽限流配额

## Claims 透传数据流

JWT 验证通过后，claims 通过两条路径传递，用途完全不同：

```
JWT 验证通过
    ├→ RequestHeader          ← 写入 X-User-Sub、X-User-Roles 等请求头 → 传给上游服务
    └→ FilterContext          ← 写入 auth_user_id: Option<String>      ← 网关内部消费（如 LoggingFilter）
```

- **RequestHeader**：上游服务直接从请求头读取用户信息，无需重复解析 JWT
- **FilterContext**：仅网关内部 Filter 间共享，不会到达上游
- **GatewayState 不参与此数据流**：它只持有验证配置（公钥、issuer），不持有任何请求级别的数据

## 编码踩坑记录

### jsonwebtoken v10 API

| 踩坑点 | 错误写法 | 正确写法 |
|--------|---------|---------|
| `validation.iss` 类型 | `Some(issuer.clone())` | `Some(HashSet::from([issuer.clone()]))` |
| `insert_header` 参数 | `insert_header(&header_name, ...)` | `insert_header(header_name, ...)` |
| `JwtClaims` 序列化 | 只 derive `Deserialize` | 需要同时 derive `Serialize` |

### AuthConfigRaw 需要 Clone

`into_auth_config(self)` 消费所有权，但 `config.auth` 是 `Option<AuthConfigRaw>` 的引用。需要先 `.clone()` 再调用，所以 `AuthConfigRaw` 必须 derive `Clone`。

### RwLockReadGuard 不能跨 .await

和所有 Filter 一样，先在块内提取数据、释放锁，再使用数据：

```rust
let (auth_config, needs_auth) = {
    let guard = state.read().unwrap_or_else(|e| {
        warn!("Gateway state lock poisoned, recovering");
        e.into_inner()
    });
    // 提取数据...
}; // guard 在此释放
```

### 防御逻辑：route_id 为 None 时放行

`ctx.route_id` 由 WhiteListFilter 设置。如果为 `None`（WhiteList 未执行或被跳过），AuthFilter 直接 Continue——不会因配置错误导致所有请求被拒。与 MethodFilter 的处理方式一致。

## 与 RateLimitFilter 的对称性

两者共享"未配置则跳过"模式：

```
auth_config: None     → Continue      |  rate_limiter: None     → Continue
is_auth: false        → Continue      |  （无路由级别跳过）
Stop(401)             |                |  Stop(429)
响应阶段无操作        |                |  响应阶段注入 X-RateLimit-Remaining
```
