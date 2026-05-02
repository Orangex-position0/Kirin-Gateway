use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};
use std::sync::LazyLock;

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
}

/// 收集所有指标，返回 Prometheus text format 字符串
pub fn collect() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
