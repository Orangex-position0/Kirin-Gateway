use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};
use std::sync::LazyLock;
use tokio::net::TcpListener;

pub static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);

// 请求总数
pub static REQUESTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    IntCounterVec::new(
        Opts::new("kirin_requests_total", "Total number of requests"),
        &["method", "upstream", "status_code"],
    )
    .unwrap()
});

// 请求延迟直方图
pub static REQUEST_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    HistogramVec::new(
        HistogramOpts::new(
            "kirin_request_duration_seconds",
            "Request duration in seconds",
        )
        .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]),
        &["method", "upstream"],
    )
    .unwrap()
});

// 上游错误数
pub static UPSTREAM_ERRORS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    IntCounterVec::new(
        Opts::new("kirin_upstream_errors_total", "Total upstream errors"),
        &["method", "upstream"],
    )
    .unwrap()
});

// Filter 拒绝数
pub static FILTER_REJECTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    IntCounterVec::new(
        Opts::new("kirin_filter_rejects_total", "Total filter rejects"),
        &["filter_name", "status_code"],
    )
    .unwrap()
});

// 灰度请求总数
pub static CANARY_REQUESTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    IntCounterVec::new(
        Opts::new("kirin_canary_requests_total", "Canary route requests total"),
        &["route_id", "method", "status_code"],
    )
    .unwrap()
});

// 灰度请求延迟
pub static CANARY_REQUEST_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    HistogramVec::new(
        HistogramOpts::new(
            "kirin_canary_request_duration_seconds",
            "Canary route request duration",
        )
        .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]),
        &["route_id", "method"],
    )
    .unwrap()
});

/// 初始化: 将所有指标注册到全局 Registry
pub fn init() {
    REGISTRY.register(Box::new(REQUESTS_TOTAL.clone())).unwrap();
    REGISTRY
        .register(Box::new(REQUEST_DURATION.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(UPSTREAM_ERRORS_TOTAL.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(FILTER_REJECTS_TOTAL.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(CANARY_REQUESTS_TOTAL.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(CANARY_REQUEST_DURATION.clone()))
        .unwrap();
}

/// 收集所有指标，返回 Prometheus text format 字符串
pub fn collect() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

/// 处理单个 metrics HTTP 请求，返回 Prometheus text format 指标文本
async fn metrics_handler(
    _req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let body = collect();
    Ok(Response::builder()
        .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

/// 启动独立端口的 Prometheus metrics HTTP 服务
///
/// 监听指定地址，所有请求路径均返回 Prometheus text format 指标。
pub async fn start_metrics_server(addr: String) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => {
            tracing::info!("Prometheus metrics endpoint started on {}", addr);
            l
        },
        Err(e) => {
            tracing::error!("Failed to bind metrics server on {}: {}", addr, e);
            return;
        },
    };

    // 循环接收连接，每个连接独立 tokio 任务处理
    loop {
        let (stream, _) = listener.accept().await.unwrap_or_else(|e| {
            tracing::warn!("Metrics server accept error: {}", e);
            panic!("accept failed: {}", e)
        });
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(io, service_fn(metrics_handler))
                .await;
        });
    }
}
