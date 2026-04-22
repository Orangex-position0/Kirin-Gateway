use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use log::warn;
use pingora_core::prelude::HttpPeer;
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session};
use serde::Serialize;
use crate::control_plane::control_plane::ControlPlane;
use crate::control_plane::gateway_state::GatewayState;
use crate::control_plane::admin_api::dto::{RouteDTO, UpstreamDTO, RateLimitDTO};

pub mod dto;

/// Admin API 代理服务
pub struct AdminProxy {
    // 共享状态
    pub state: Arc<RwLock<GatewayState>>,
    // 控制面实例
    pub control_plane: Arc<ControlPlane>,
}

/// Admin API 响应统一格式
#[derive(Serialize)]
struct AdminResponse<T: Serialize> {
    // 响应状态，值为 "ok" 或 "error"
    status: String,
    // 响应数据，成功时携带业务数据，失败时为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    // 错误信息，仅失败时携带具体原因描述
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl<T: Serialize> AdminResponse<T> {
    fn ok(data: T) -> Self {
        AdminResponse {
            status: "ok".to_string(),
            data: Some(data),
            message: None,
        }
    }

    fn error(msg: &str) -> AdminResponse<()> {
        AdminResponse {
            status: "error".to_string(),
            data: None,
            message: Some(msg.to_string()),
        }
    }
}

#[async_trait]
impl ProxyHttp for AdminProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut Self::CTX) -> pingora_core::Result<Box<HttpPeer>> {
        // Admin API 在 request_filter 中直接返回响应，不需要代理到上游
        Err(pingora_core::Error::new_str("Admin API should not proxy"))
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        let path = session
            .req_header()
            .uri
            .path();
        let method = session.req_header().method.as_str();

        let (status_code, body): (u16, String) = match (method, path) {
            ("GET", "/admin/routes") => self.handle_routes(),
            ("GET", "/admin/upstreams") => self.handle_upstreams(),
            ("GET", "/admin/rate-limit") => self.handle_rate_limit(),
            ("POST", "/admin/reload") => self.handle_reload(),
            _ => (404, serde_json::to_string(&AdminResponse::<()>::error("接口不存在")).unwrap()),
        };

        let header = ResponseHeader::build(status_code, None)?;
        session.write_response_header(Box::new(header), false).await?;

        let body_bytes = body.into_bytes();
        session.write_response_body(Some(body_bytes.into()), false).await?;

        Ok(true)
    }
}

impl AdminProxy {
    /// 获取所有路由规则
    fn handle_routes(&self) -> (u16, String) {
        let state = match self.state.read() {
            Ok(s) => s,
            Err(e) => {
                warn!("Admin API 获取读锁失败: {}", e);
                return (
                    500,
                    serde_json::to_string(&AdminResponse::<()>::error("内部错误")).unwrap(),
                );
            }
        };

        let routes: Vec<RouteDTO> = state.router.routes_summary();
        let resp = AdminResponse::ok(routes);
        (200, serde_json::to_string(&resp).unwrap())
    }

    /// 获取所有上游集群信息
    fn handle_upstreams(&self) -> (u16, String) {
        let state = match self.state.read() {
            Ok(s) => s,
            Err(e) => {
                warn!("Admin API 获取读锁失败: {}", e);
                return (
                    500,
                    serde_json::to_string(&AdminResponse::<()>::error("内部错误")).unwrap(),
                );
            }
        };

        let upstreams: Vec<UpstreamDTO> = state
            .clusters
            .values()
            .map(|cluster| cluster.summary())
            .collect();

        let resp = AdminResponse::ok(upstreams);
        (200, serde_json::to_string(&resp).unwrap())
    }

    /// 获取当前限流配置
    fn handle_rate_limit(&self) -> (u16, String) {
        let state = match self.state.read() {
            Ok(s) => s,
            Err(e) => {
                warn!("Admin API 获取读锁失败: {}", e);
                return (
                    500,
                    serde_json::to_string(&AdminResponse::<()>::error("内部错误")).unwrap(),
                );
            }
        };

        let dto = state.rate_limit_summary().unwrap_or_else(|| RateLimitDTO {
            enabled: false,
            capacity: None,
            refill_rate: None,
        });

        let resp = AdminResponse::ok(dto);
        (200, serde_json::to_string(&resp).unwrap())
    }

    /// 手动触发配置热重载
    fn handle_reload(&self) -> (u16, String) {
        match self.control_plane.reload_simple() {
            Ok(()) => {
                let resp = AdminResponse::ok("重载成功");
                (200, serde_json::to_string(&resp).unwrap())
            }
            Err(e) => {
                let resp = AdminResponse::<()>::error(&e);
                (400, serde_json::to_string(&resp).unwrap())
            }
        }
    }
}
