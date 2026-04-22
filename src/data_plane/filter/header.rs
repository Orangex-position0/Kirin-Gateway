use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use pingora_http::{RequestHeader, ResponseHeader};
use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterResult};

/// 网关头注入 Filter
pub struct HeaderFilter;

#[async_trait]
impl Filter for HeaderFilter {
    fn name(&self) -> FilterName {
        FilterName::Header
    }

    async fn request_filter(
        &self,
        _ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        _state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        request_header
            .insert_header("X-Gateway", "Kirin Gateway")
            .unwrap();
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
