use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use log::warn;
use pingora_http::{RequestHeader, ResponseHeader};
use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterReject, FilterResult};

/// 限流 Filter
///
/// 请求阶段：基于客户端 IP 执行令牌桶限流检查
/// 响应阶段：注入 X-RateLimit-Remaining 响应头
pub struct RateLimitFilter;

#[async_trait]
impl Filter for RateLimitFilter {
    fn name(&self) -> FilterName {
        FilterName::RateLimit
    }

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        _request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        // 提取限流器引用后释放读锁
        let rate_limiter = {
            let state_guard = state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });
            state_guard.rate_limiter().cloned()
        };

        // 限流器未配置时直接通过
        let rate_limiter = match rate_limiter {
            Some(rl) => rl,
            None => return FilterResult::Continue,
        };

        let (allowed, remaining) = rate_limiter.check(&ctx.client_ip);
        ctx.rate_limit_remaining = Some(remaining);

        if allowed {
            FilterResult::Continue
        } else {
            warn!("请求被限流，IP: {}", ctx.client_ip);
            FilterResult::Stop(FilterReject::TooManyRequests)
        }
    }

    async fn response_filter(
        &self,
        ctx: &mut FilterContext,
        response_header: &mut ResponseHeader,
    ) {
        // 注入限流剩余令牌响应头
        if let Some(remaining) = ctx.rate_limit_remaining {
            response_header
                .insert_header("X-RateLimit-Remaining", remaining.to_string())
                .unwrap();
        }
    }
}
