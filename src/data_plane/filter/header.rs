use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterResult};
use async_trait::async_trait;
use pingora_http::{RequestHeader, ResponseHeader};
use std::sync::{Arc, RwLock};

/// 网关头注入 Filter
///
/// 请求阶段的 X-Gateway 头注入已移至 proxy.rs 的 upstream_request_filter，
/// 因为 Pingora 的 request_filter 中修改的请求头不会传递给上游。
/// 此 Filter 仅负责响应阶段注入 X-Powered-By 头。
pub struct HeaderFilter;

#[async_trait]
impl Filter for HeaderFilter {
    fn name(&self) -> FilterName {
        FilterName::Header
    }

    async fn request_filter(
        &self,
        _ctx: &mut FilterContext,
        _request_header: &mut RequestHeader,
        _state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        // 请求阶段无操作，X-Gateway 头在 upstream_request_filter 中注入
        FilterResult::Continue
    }

    async fn response_filter(
        &self,
        _ctx: &mut FilterContext,
        response_header: &mut ResponseHeader,
    ) {
        response_header
            .insert_header("X-Powered-By", "Kirin Gateway")
            .unwrap();
    }
}
