# Kirin Gateway

[中文](README.md)

A Rust API gateway built on [Pingora](https://github.com/cloudflare/pingora), providing routing, reverse proxy, load balancing, filter chain, and token-bucket rate limiting.

## Features

- **Routing** — Exact match, prefix match (longest-prefix-first), and regex match. Priority: exact > regex > prefix
- **Filter Chain** — Extensible filter pipeline with 6 built-in filters: whitelist, method validation, JWT auth, rate limiting, header injection, and logging
- **Load Balancing** — Round-robin via Pingora LoadBalancer
- **Token-Bucket Rate Limiting** — Per-IP in-process rate limiting with dynamic policy updates
- **JWT Authentication** — RS256 signature verification with claim forwarding to upstream services
- **Control Plane / Data Plane Separation** — Shared state via `Arc<RwLock<GatewayState>>`; control plane handles config loading and hot reload, data plane handles request forwarding
- **Hot Reload** — File watcher with debouncing for automatic config reload at runtime
- **Admin API** — Query endpoints for routes, upstream clusters, rate limit config, and manual reload

## Quick Start

### Prerequisites

- Rust edition 2024 (Rust 1.85+)
- Pingora 0.8.0

### Build

```bash
cargo build --release
```

### Configuration

Copy the config template and modify as needed:

```bash
cp config.example.en.yaml config.yaml
```

Example configuration:

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
    description: "User service endpoint"
  - route_id: "order-route"
    path_prefix: /api/orders
    match_type: prefix
    upstream: order-service
    applicant: "developer"
    applied_at: "2026-04-22T00:00:00+08:00"
    description: "Order service endpoint"
  - route_id: "default-route"
    path_prefix: /api
    match_type: prefix
    upstream: default-service
    applicant: "developer"
    applied_at: "2026-04-22T00:00:00+08:00"
    description: "Default fallback route"

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

For full configuration reference, see [config.example.en.yaml](config.example.en.yaml).

### Run

```bash
# Use default config.yaml
cargo run

# Specify a config file
cargo run -- /path/to/config.yaml
```

### Test

```bash
cargo test
```

## Architecture

```
Client Request
    │
    ▼
┌─────────────────────────────────────────────────┐
│                  KirinProxy                      │
│                                                  │
│  1. upstream_peer                                │
│     ├── Route matching (exact > regex > prefix)  │
│     └── Load balance: select upstream node       │
│                                                  │
│  2. request_filter (Filter Chain request phase)  │
│     WhiteList → Method → Auth → RateLimit        │
│     → Header → Logging                           │
│                                                  │
│  3. Forward request to upstream                  │
│                                                  │
│  4. response_filter (Filter Chain response phase)│
│     WhiteList → Method → Auth → RateLimit        │
│     → Header → Logging                           │
│                                                  │
│  5. Return response to client                    │
└─────────────────────────────────────────────────┘
```

### Control Plane / Data Plane Separation

```
┌──────────────┐     Arc<RwLock<GatewayState>>     ┌──────────────┐
│  Control     │ ◄──────────────────────────────────► │  Data Plane  │
│  Plane       │                                    │              │
│              │  - Config loading & validation      │ - Routing    │
│  - YAML parse│  - GatewayState construction        │ - Filter chain│
│  - Hot reload│  - File watcher + debouncing       │ - Proxy      │
│  - Admin API │                                    │ - Load balance│
└──────────────┘                                    └──────────────┘
```

### Project Structure

```
src/
├── main.rs                              # Entry point: load config, start server
├── config.rs                            # YAML config loading & deserialization
├── data_plane/                          # Data plane
│   ├── proxy.rs                         # KirinProxy (ProxyHttp implementation)
│   ├── router.rs                        # Router (exact / prefix / regex)
│   │   └── router_white_list.rs         # Route registry (whitelist validation)
│   ├── upstream.rs                      # Upstream cluster (load balancing)
│   ├── rate_limit.rs                    # Token-bucket rate limiter
│   └── filter/                          # Filter Chain
│       ├── whitelist.rs                 # Whitelist filter
│       ├── method.rs                    # HTTP method validation filter
│       ├── auth.rs                      # JWT authentication filter
│       ├── rate_limit_filter.rs         # Rate limiting filter
│       ├── header.rs                    # Header injection filter
│       └── logging.rs                   # Logging filter
└── control_plane/                       # Control plane
    ├── control_plane.rs                 # Config loading, hot reload, file watcher
    ├── gateway_state.rs                 # Shared gateway state (GatewayState)
    ├── admin_api.rs                     # Admin API proxy service
    │   └── dto.rs                       # Admin API DTOs
    └── health_check.rs                  # TCP health check config
```

## Admin API

Enabled when `admin.listen` is configured.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/routes` | Query all route rules |
| GET | `/admin/upstreams` | Query all upstream cluster info |
| GET | `/admin/rate-limit` | Query current rate limit config |
| POST | `/admin/reload` | Trigger manual config hot reload |

All endpoints return a unified JSON format:

```json
{
  "status": "ok",
  "data": { ... }
}
```

```json
{
  "status": "error",
  "message": "Error reason"
}
```

## Filter Chain

Filters execute in order. If any filter returns `Stop` during the request phase, the chain is short-circuited and an error response is returned immediately.

| Order | Filter | Description |
|-------|--------|-------------|
| 1 | WhiteList | Validates request path against the route registry |
| 2 | Method | Validates HTTP method is allowed by the route rule |
| 3 | Auth | JWT RS256 authentication (only for routes with `is_auth: true`) |
| 4 | RateLimit | Per-IP token-bucket rate limiting |
| 5 | Header | Injects `X-Gateway` / `X-Powered-By` headers |
| 6 | Logging | Request and response logging |

## Route Matching Priority

1. **Exact match** — HashMap O(1) lookup
2. **Regex match** — Traverses in declaration order, first declared wins
3. **Prefix match** — Traverses in descending prefix length, longest match wins

## License

MIT
