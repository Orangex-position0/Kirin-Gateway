use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use log::info;
use pingora_http::{RequestHeader, ResponseHeader};
use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterResult};

/// 请求日志 Filter
pub struct LoggingFilter;

#[async_trait]
impl Filter for LoggingFilter {
    fn name(&self) -> FilterName {
        FilterName::Logging
    }

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        _request_header: &mut RequestHeader,
        _state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        info!("[LoggingFilter] {} {}", ctx.method, ctx.path);
        FilterResult::Continue
    }

    async fn response_filter(
        &self,
        _ctx: &mut FilterContext,
        response_header: &mut ResponseHeader,
    ) {
        info!(
            "[LoggingFilter] Response status: {}",
            response_header.status.as_u16()
        );
    }
}
