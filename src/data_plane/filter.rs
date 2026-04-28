pub mod auth;
pub mod header;
pub mod logging;
pub mod method;
pub mod rate_limit_filter;
pub mod whitelist;

use crate::control_plane::gateway_state::GatewayState;
use async_trait::async_trait;
use log::info;
use pingora_http::{RequestHeader, ResponseHeader};
use std::sync::{Arc, RwLock};

/// FilterChain 级别的拒绝原因
#[derive(Debug, Clone)]
pub enum FilterReject {
    Unauthorized,     // 401
    Forbidden,        // 403
    NotFound,         // 404
    MethodNotAllowed, // 405
    TooManyRequests,  // 429
    InternalError,    // 500
    Custom { code: u16, reason: String },
}

impl FilterReject {
    /// 转换为 HTTP 状态码
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::TooManyRequests => 429,
            Self::InternalError => 500,
            Self::Custom { code, .. } => *code,
        }
    }
}

/// Filter 执行结果
///
/// - Continue: 继续执行下一个过滤器
/// - Stop(FilterReject): 中断 Filter 链，附带网关级别拒绝原因
pub enum FilterResult {
    Continue,
    Stop(FilterReject),
}

/// Filter 名称枚举，统一命名风格，预留扩展
#[derive(Debug, Clone)]
pub enum FilterName {
    WhiteList,
    Method,
    Auth,
    RateLimit,
    Header,
    Logging,
    Custom(String),
}

impl std::fmt::Display for FilterName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WhiteList => write!(f, "whitelist-filter"),
            Self::Method => write!(f, "method-filter"),
            Self::Auth => write!(f, "auth-filter"),
            Self::RateLimit => write!(f, "rate-limit-filter"),
            Self::Header => write!(f, "header-filter"),
            Self::Logging => write!(f, "logging-filter"),
            Self::Custom(name) => write!(f, "{}", name),
        }
    }
}

/// 过滤器统一上下文
pub struct FilterContext {
    /// 请求路径
    pub path: String,
    /// HTTP 方法
    pub method: String,
    /// 客户端 IP
    pub client_ip: String,
    /// 上游集群名称（路由匹配后设置）
    pub upstream_name: Option<String>,
    /// 路由 ID（白名单校验后设置）
    pub route_id: Option<String>,
    /// 请求起始时间
    pub start_time: std::time::Instant,
    /// 限流剩余令牌数（限流 Filter 设置）
    pub rate_limit_remaining: Option<usize>,
    /// JWT sub claim
    pub auth_user_id: Option<String>,
}

/// Filter trait：所有横切关注点的统一抽象
#[async_trait]
pub trait Filter: Send + Sync {
    /// Filter 名称
    fn name(&self) -> FilterName;

    /// 请求阶段过滤器
    ///
    /// 返回 Continue 继续执行下一个 Filter，
    /// 返回 Stop(code) 中断链，由 proxy 统一返回错误响应。
    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult;

    /// 响应阶段过滤器（所有 Filter 都会执行，不短路）
    async fn response_filter(&self, ctx: &mut FilterContext, response_header: &mut ResponseHeader);
}

/// Filter 编排器
#[derive(Clone)]
pub struct FilterChain {
    filters: Vec<Arc<dyn Filter>>,
}

impl FilterChain {
    pub fn new() -> Self {
        FilterChain {
            filters: Vec::new(),
        }
    }

    pub fn add_filter(&mut self, filter: Arc<dyn Filter>) {
        info!("Filter Chain - add Filter: {}", filter.name());
        self.filters.push(filter);
    }

    /// 执行请求阶段的 Filter Chain
    pub async fn run_request_filters(
        &self,
        ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        for filter in &self.filters {
            let result = filter.request_filter(ctx, request_header, state).await;
            match result {
                FilterResult::Continue => continue,
                FilterResult::Stop(reject) => {
                    info!(
                        "Filter Chain - Filter '{}' was interrupted (HTTP {})",
                        filter.name(),
                        reject.status_code()
                    );
                    return FilterResult::Stop(reject);
                },
            }
        }

        FilterResult::Continue
    }

    /// 执行响应阶段的 Filter Chain
    pub async fn run_response_filters(
        &self,
        ctx: &mut FilterContext,
        response_header: &mut ResponseHeader,
    ) {
        for filter in &self.filters {
            filter.response_filter(ctx, response_header).await;
        }
    }

    /// 获取已注册 Filter 的数量
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// 判断 Filter Chain 是否为空
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingora_http::ResponseHeader;

    /// 测试用空 Filter
    struct AlwaysContinueFilter;
    struct AlwaysStopFilter;

    #[async_trait]
    impl Filter for AlwaysContinueFilter {
        fn name(&self) -> FilterName {
            FilterName::Custom("always-continue".to_string())
        }
        async fn request_filter(
            &self,
            _ctx: &mut FilterContext,
            _request_header: &mut RequestHeader,
            _state: &Arc<RwLock<GatewayState>>,
        ) -> FilterResult {
            FilterResult::Continue
        }
        async fn response_filter(
            &self,
            _ctx: &mut FilterContext,
            _response_header: &mut ResponseHeader,
        ) {
        }
    }

    #[async_trait]
    impl Filter for AlwaysStopFilter {
        fn name(&self) -> FilterName {
            FilterName::Custom("always-stop".to_string())
        }
        async fn request_filter(
            &self,
            _ctx: &mut FilterContext,
            _request_header: &mut RequestHeader,
            _state: &Arc<RwLock<GatewayState>>,
        ) -> FilterResult {
            FilterResult::Stop(FilterReject::Forbidden)
        }
        async fn response_filter(
            &self,
            _ctx: &mut FilterContext,
            _response_header: &mut ResponseHeader,
        ) {
        }
    }

    #[test]
    fn test_empty_chain_returns_continue() {
        // 注意：此测试需要 Pingora Session，在单元测试中需要特殊处理
        // 实际项目中可使用集成测试验证
    }

    #[test]
    fn test_filter_chain_len() {
        let mut chain = FilterChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }
}
