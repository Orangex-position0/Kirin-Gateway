use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use log::warn;
use pingora_http::{RequestHeader, ResponseHeader};
use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterReject, FilterResult};

/// HTTP 方法校验 Filter
///
/// 检查请求的 HTTP 方法是否在路由规则允许的方法列表中
pub struct MethodFilter;

#[async_trait]
impl Filter for MethodFilter {
    fn name(&self) -> FilterName {
        FilterName::Method
    }

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        _request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        let route_id = match &ctx.route_id {
            None => return FilterResult::Continue,
            Some(id) => id.clone(),
        };

        // 提取允许的方法列表后立即释放读锁
        let methods: Vec<String> = {
            let state_guard = state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });
            match state_guard.registry.find_route(&route_id) {
                None => return FilterResult::Continue,
                Some(e) => e.methods.clone(),
            }
        };

        if methods.is_empty() {
            return FilterResult::Continue;
        }

        if methods.iter().any(|m| m == &ctx.method) {
            FilterResult::Continue
        } else {
            warn!(
                "HTTP 方法不允许: {} {} (允许的方法: {:?})",
                ctx.method, ctx.path, methods
            );
            FilterResult::Stop(FilterReject::Forbidden)
        }
    }

    async fn response_filter(
        &self,
        _ctx: &mut FilterContext,
        _response_header: &mut ResponseHeader,
    ) {
        // 方法校验 Filter 在响应阶段无操作
    }
}
