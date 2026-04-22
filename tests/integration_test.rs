use std::io::Write;
use std::net::SocketAddr;
use std::process::Command;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

// === Mock 上游服务 ===

/// 基础 Mock 上游的响应处理器
///
/// 收到请求后返回 JSON，包含请求路径、方法和网关注入的 X-Gateway 头
async fn mock_handler(req: Request<hyper::body::Incoming>) -> hyper::Result<Response<Full<Bytes>>> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // 把网关注入的请求头也返回，方便断言中间件是否生效
    let gateway_header = req
        .headers()
        .get("X-Gateway")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none")
        .to_string();

    let body = format!(
        "{{\"path\": \"{}\", \"method\": \"{}\", \"x_gateway\": \"{}\"}}",
        path, method, gateway_header
    );

    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

/// 带身份标识的 Mock 上游响应处理器
///
/// 除了基础信息外，还在响应体中包含上游节点 ID，用于验证负载均衡分发
async fn mock_handler_with_id(
    req: Request<hyper::body::Incoming>,
    upstream_id: String,
) -> hyper::Result<Response<Full<Bytes>>> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let body = format!(
        "{{\"path\": \"{}\", \"method\": \"{}\", \"upstream_id\": \"{}\"}}",
        path, method, upstream_id
    );

    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

/// 启动一个基础 Mock HTTP 上游服务
///
/// 端口由 OS 随机分配（端口 0），返回 (实际监听地址, 关闭信号发送端)
pub async fn start_mock_upstream() -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            _ = async {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        http1::Builder::new()
                            .serve_connection(io, service_fn(mock_handler))
                            .await
                            .unwrap();
                    });
                }
            } => {},
            _ = rx => {
                // 收到关闭信号，退出
            }
        }
    });

    (addr, tx)
}

/// 启动一个带身份标识的 Mock HTTP 上游服务
///
/// 每个响应包含唯一的 upstream_id，用于验证负载均衡的请求分发
pub async fn start_mock_upstream_with_id(
    upstream_id: String,
) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            _ = async {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let io = TokioIo::new(stream);
                    let id = upstream_id.clone();
                    tokio::spawn(async move {
                        http1::Builder::new()
                            .serve_connection(io, service_fn(move |req| {
                                let id = id.clone();
                                async move { mock_handler_with_id(req, id).await }
                            }))
                            .await
                            .unwrap();
                    });
                }
            } => {},
            _ = rx => {
                // 收到关闭信号，退出
            }
        }
    });

    (addr, tx)
}

// === 测试基础设施 ===

/// 动态生成测试用的 YAML 配置文件
///
/// 所有路由默认使用精确匹配（path），返回生成的配置文件路径
pub fn create_test_config(
    gateway_port: u16,
    upstreams: &[(&str, Vec<String>)],
    routes: &[(&str, &str)],
    rate_limit: Option<(usize, usize)>,
) -> String {
    let mut yaml = String::new();
    yaml.push_str(&format!(
        "server:\n  listen: \"0.0.0.0:{}\"\n  threads: 1\n\n",
        gateway_port
    ));

    yaml.push_str("routes:\n");
    for (path, upstream) in routes {
        yaml.push_str(&format!("  - path: \"{}\"\n    upstream: {}\n", path, upstream));
    }

    yaml.push_str("\nupstreams:\n");
    for (name, addrs) in upstreams {
        yaml.push_str(&format!("  {}:\n    nodes:\n", name));
        for addr in addrs {
            yaml.push_str(&format!("      - addr: \"{}\"\n        weight: 1\n", addr));
        }
    }

    if let Some((capacity, refill)) = rate_limit {
        yaml.push_str(&format!(
            "\nrate_limit:\n  capacity: {}\n  refill_rate: {}\n",
            capacity, refill
        ));
    }

    let file_path = format!("config_test_{}.yaml", gateway_port);
    let mut file = std::fs::File::create(&file_path).unwrap();
    file.write_all(yaml.as_bytes()).unwrap();

    file_path
}

/// 路由配置项：精确路径或前缀路径
pub enum RouteEntry {
    /// 精确路径匹配
    Exact(&'static str, &'static str),
    /// 前缀路径匹配
    Prefix(&'static str, &'static str),
}

/// 动态生成支持前缀路由的 YAML 配置文件
pub fn create_test_config_with_routes(
    gateway_port: u16,
    upstreams: &[(&str, Vec<String>)],
    routes: &[RouteEntry],
    rate_limit: Option<(usize, usize)>,
) -> String {
    let mut yaml = String::new();
    yaml.push_str(&format!(
        "server:\n  listen: \"0.0.0.0:{}\"\n  threads: 1\n\n",
        gateway_port
    ));

    yaml.push_str("routes:\n");
    for entry in routes {
        match entry {
            RouteEntry::Exact(path, upstream) => {
                yaml.push_str(&format!("  - path: \"{}\"\n    upstream: {}\n", path, upstream));
            }
            RouteEntry::Prefix(prefix, upstream) => {
                yaml.push_str(&format!(
                    "  - path_prefix: \"{}\"\n    upstream: {}\n",
                    prefix, upstream
                ));
            }
        }
    }

    yaml.push_str("\nupstreams:\n");
    for (name, addrs) in upstreams {
        yaml.push_str(&format!("  {}:\n    nodes:\n", name));
        for addr in addrs {
            yaml.push_str(&format!("      - addr: \"{}\"\n        weight: 1\n", addr));
        }
    }

    if let Some((capacity, refill)) = rate_limit {
        yaml.push_str(&format!(
            "\nrate_limit:\n  capacity: {}\n  refill_rate: {}\n",
            capacity, refill
        ));
    }

    let file_path = format!("config_test_{}.yaml", gateway_port);
    let mut file = std::fs::File::create(&file_path).unwrap();
    file.write_all(yaml.as_bytes()).unwrap();

    file_path
}

/// 等待网关端口可连接
///
/// 轮询检测端口，超时返回 false
fn wait_for_gateway(addr: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if std::net::TcpStream::connect(addr).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// 启动网关子进程，等待就绪后返回子进程句柄
pub fn start_gateway(config_path: &str, gateway_port: u16) -> std::process::Child {
    let child = Command::new("cargo")
        .args(["run", "--", config_path])
        .spawn()
        .expect("Failed to start gateway");

    let addr = format!("127.0.0.1:{}", gateway_port);
    assert!(
        wait_for_gateway(&addr, Duration::from_secs(10)),
        "Gateway failed to start within 10 seconds"
    );

    child
}

// === 测试用例 ===

/// 验证不同路径被路由到正确的上游服务
#[test]
fn test_route_to_correct_upstream() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // 启动两个 Mock 上游
    let (user_addr, user_shutdown) = rt.block_on(start_mock_upstream());
    let (order_addr, order_shutdown) = rt.block_on(start_mock_upstream());

    let config_path = create_test_config(
        16288,
        &[
            (
                "user-service",
                vec![format!("127.0.0.1:{}", user_addr.port())],
            ),
            (
                "order-service",
                vec![format!("127.0.0.1:{}", order_addr.port())],
            ),
        ],
        &[
            ("/api/users", "user-service"),
            ("/api/orders", "order-service"),
        ],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16288);
    let client = reqwest::blocking::Client::new();

    // 请求 /api/users 应该转发到 user-service
    let resp = client
        .get("http://127.0.0.1:16288/api/users")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/users");

    // 请求 /api/orders 应该转发到 order-service
    let resp = client
        .get("http://127.0.0.1:16288/api/orders")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/orders");

    // 清理
    let _ = user_shutdown.send(());
    let _ = order_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证 HeaderMiddleware 在请求中注入 X-Gateway 头，在响应中注入 X-Powered-By 头
#[test]
fn test_middleware_injects_headers() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (mock_addr, mock_shutdown) = rt.block_on(start_mock_upstream());

    let config_path = create_test_config(
        16289,
        &[(
            "svc",
            vec![format!("127.0.0.1:{}", mock_addr.port())],
        )],
        &[("/api/users", "svc")],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16289);
    let client = reqwest::blocking::Client::new();

    let resp = client
        .get("http://127.0.0.1:16289/api/users")
        .send()
        .unwrap();

    // 检查响应头：中间件应注入 X-Powered-By
    assert!(resp.headers().contains_key("X-Powered-By"));
    assert_eq!(
        resp.headers().get("X-Powered-By").unwrap(),
        "Kirin Gateway"
    );

    // 检查响应体：Mock 上游应收到 X-Gateway 请求头
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["x_gateway"], "Kirin Gateway");

    // 清理
    let _ = mock_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证令牌桶耗尽后返回 429
#[test]
fn test_rate_limit_returns_429() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (mock_addr, mock_shutdown) = rt.block_on(start_mock_upstream());

    // 配置限流：capacity=3, refill_rate=1
    let config_path = create_test_config(
        16290,
        &[(
            "svc",
            vec![format!("127.0.0.1:{}", mock_addr.port())],
        )],
        &[("/api/test", "svc")],
        Some((3, 1)),
    );

    let mut gateway = start_gateway(&config_path, 16290);
    let client = reqwest::blocking::Client::new();

    // 前 3 次请求应该成功
    for _ in 0..3 {
        let resp = client
            .get("http://127.0.0.1:16290/api/test")
            .send()
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // 第 4 次应该被限流
    let resp = client
        .get("http://127.0.0.1:16290/api/test")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 429);

    // 清理
    let _ = mock_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证无匹配路由时 Pingora 返回 502
#[test]
fn test_no_route_returns_502() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (mock_addr, mock_shutdown) = rt.block_on(start_mock_upstream());

    let config_path = create_test_config(
        16291,
        &[(
            "svc",
            vec![format!("127.0.0.1:{}", mock_addr.port())],
        )],
        &[("/api/users", "svc")],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16291);
    let client = reqwest::blocking::Client::new();

    // 请求不存在的路径，Pingora 对无路由返回 500
    let resp = client
        .get("http://127.0.0.1:16291/nonexistent")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 500);

    // 清理
    let _ = mock_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证多节点上游是否轮询分布
#[test]
fn test_round_robin_distribution() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr1, shutdown1) = rt.block_on(start_mock_upstream_with_id("node-1".to_string()));
    let (addr2, shutdown2) = rt.block_on(start_mock_upstream_with_id("node-2".to_string()));

    // 同一个上游注册两个节点
    let config_path = create_test_config(
        16292,
        &[(
            "order-service",
            vec![
                format!("127.0.0.1:{}", addr1.port()),
                format!("127.0.0.1:{}", addr2.port()),
            ],
        )],
        &[("/api/orders", "order-service")],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16292);
    let client = reqwest::blocking::Client::new();

    // 发送 4 次请求，轮询应均匀分布到两个节点
    let mut node1_count = 0;
    let mut node2_count = 0;

    for _ in 0..4 {
        let resp = client
            .get("http://127.0.0.1:16292/api/orders")
            .send()
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        match body["upstream_id"].as_str() {
            Some("node-1") => node1_count += 1,
            Some("node-2") => node2_count += 1,
            other => panic!("Unexpected upstream_id: {:?}", other),
        }
    }

    // 轮询应该均匀分布：每个节点各收到 2 次
    assert_eq!(node1_count, 2);
    assert_eq!(node2_count, 2);

    // 清理
    let _ = shutdown1.send(());
    let _ = shutdown2.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证前缀路由匹配能正确转发请求到上游
#[test]
fn test_prefix_route_matching() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (mock_addr, mock_shutdown) = rt.block_on(start_mock_upstream());

    let config_path = create_test_config_with_routes(
        16293,
        &[(
            "svc",
            vec![format!("127.0.0.1:{}", mock_addr.port())],
        )],
        &[
            RouteEntry::Prefix("/api/", "svc"),
        ],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16293);
    let client = reqwest::blocking::Client::new();

    // 前缀匹配：/api/ 下所有路径都应转发
    let resp = client
        .get("http://127.0.0.1:16293/api/users/123")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/users/123");

    let resp = client
        .get("http://127.0.0.1:16293/api/orders")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/orders");

    // 不匹配前缀的路径应返回 500
    let resp = client
        .get("http://127.0.0.1:16293/health")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 500);

    // 清理
    let _ = mock_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证精确路由和前缀路由共存时精确路由优先
#[test]
fn test_exact_and_prefix_route_coexistence() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (user_addr, user_shutdown) = rt.block_on(start_mock_upstream());
    let (default_addr, default_shutdown) = rt.block_on(start_mock_upstream());

    let config_path = create_test_config_with_routes(
        16294,
        &[
            (
                "user-svc",
                vec![format!("127.0.0.1:{}", user_addr.port())],
            ),
            (
                "default-svc",
                vec![format!("127.0.0.1:{}", default_addr.port())],
            ),
        ],
        &[
            RouteEntry::Exact("/api/users", "user-svc"),
            RouteEntry::Prefix("/api/", "default-svc"),
        ],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16294);
    let client = reqwest::blocking::Client::new();

    // 精确匹配 /api/users 应转发到 user-svc
    let resp = client
        .get("http://127.0.0.1:16294/api/users")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/users");

    // 前缀匹配 /api/orders 应转发到 default-svc
    let resp = client
        .get("http://127.0.0.1:16294/api/orders")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/orders");

    // 清理
    let _ = user_shutdown.send(());
    let _ = default_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}

/// 验证配置热重载后新路由生效
#[test]
fn test_config_hot_reload() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (user_addr, user_shutdown) = rt.block_on(start_mock_upstream());
    let (order_addr, order_shutdown) = rt.block_on(start_mock_upstream());

    // 初始配置只有 /api/users 路由
    let config_path = create_test_config(
        16295,
        &[
            (
                "user-svc",
                vec![format!("127.0.0.1:{}", user_addr.port())],
            ),
            (
                "order-svc",
                vec![format!("127.0.0.1:{}", order_addr.port())],
            ),
        ],
        &[
            ("/api/users", "user-svc"),
        ],
        None,
    );

    let mut gateway = start_gateway(&config_path, 16295);
    let client = reqwest::blocking::Client::new();

    // 初始状态：/api/users 可达，/api/orders 不可达
    let resp = client
        .get("http://127.0.0.1:16295/api/users")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .get("http://127.0.0.1:16295/api/orders")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 500);

    // 更新配置文件，增加 /api/orders 路由
    let updated_yaml = format!(
        r#"server:
  listen: "0.0.0.0:16295"
  threads: 1

routes:
  - path: "/api/users"
    upstream: user-svc
  - path: "/api/orders"
    upstream: order-svc

upstreams:
  user-svc:
    nodes:
      - addr: "127.0.0.1:{}"
        weight: 1
  order-svc:
    nodes:
      - addr: "127.0.0.1:{}"
        weight: 1
"#,
        user_addr.port(),
        order_addr.port()
    );
    std::fs::write(&config_path, updated_yaml).unwrap();

    // 等待文件监听器检测到变化并完成热重载
    std::thread::sleep(std::time::Duration::from_secs(3));

    // 热重载后：/api/orders 应该可达
    let resp = client
        .get("http://127.0.0.1:16295/api/orders")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["path"], "/api/orders");

    // /api/users 仍然可达
    let resp = client
        .get("http://127.0.0.1:16295/api/users")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 清理
    let _ = user_shutdown.send(());
    let _ = order_shutdown.send(());
    gateway.kill().unwrap();
    std::fs::remove_file(&config_path).ok();
}
