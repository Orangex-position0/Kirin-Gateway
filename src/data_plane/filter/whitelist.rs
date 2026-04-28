use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterReject, FilterResult};
use async_trait::async_trait;
use log::warn;
use pingora_http::{RequestHeader, ResponseHeader};
use std::sync::{Arc, RwLock};

/// 白名单校验 Filter
///
/// 检查请求路径是否在接口注册表中注册过
pub struct WhiteListFilter;

#[async_trait]
impl Filter for WhiteListFilter {
    fn name(&self) -> FilterName {
        FilterName::WhiteList
    }

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        _request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        // 提取路由匹配结果后立即释放读锁，避免 RwLockReadGuard 跨 .await
        let route_id = {
            let state_guard = state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });
            state_guard.registry.resolve_path(&ctx.path)
        };

        match route_id {
            None => {
                warn!("接口未注册，拒绝访问: {} {}", ctx.method, ctx.path);
                FilterResult::Stop(FilterReject::Forbidden)
            },
            Some(id) => {
                ctx.route_id = Some(id);
                FilterResult::Continue
            },
        }
    }

    async fn response_filter(
        &self,
        _ctx: &mut FilterContext,
        _response_header: &mut ResponseHeader,
    ) {
        // 白名单 Filter 在响应阶段无操作
    }
}
