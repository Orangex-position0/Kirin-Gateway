use crate::control_plane::admin_api::dto::UpstreamDTO;
use crate::control_plane::control_plane::ControlPlane;
use crate::control_plane::gateway_state::GatewayState;
use async_trait::async_trait;
use pingora_core::prelude::HttpPeer;
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session};
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tracing::warn;

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

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Box<HttpPeer>> {
        // Admin API 在 request_filter 中直接返回响应，不需要代理到上游
        Err(pingora_core::Error::new_str("Admin API should not proxy"))
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        let path = session.req_header().uri.path();
        let method = session.req_header().method.as_str();

        let (status_code, body): (u16, String) = match (method, path) {
            ("GET", "/admin/routes") => self.handle_routes(),
            ("GET", "/admin/upstreams") => self.handle_upstreams(),
            ("GET", "/admin/rate-limit") => self.handle_rate_limit(),
            ("POST", "/admin/reload") => self.handle_reload(),
            _ => (
                404,
                serde_json::to_string(&AdminResponse::<()>::error("接口不存在")).unwrap(),
            ),
        };

        let header = ResponseHeader::build(status_code, None)?;
        session
            .write_response_header(Box::new(header), false)
            .await?;

        let body_bytes = body.into_bytes();
        session
            .write_response_body(Some(body_bytes.into()), false)
            .await?;

        Ok(true)
    }
}

impl AdminProxy {
    /// imperative Shell: 获取读锁，并调用纯函数处理数据
    fn with_state<F>(&self, handler: F) -> (u16, String)
    where
        F: FnOnce(&GatewayState) -> (u16, String),
    {
        match self.state.read() {
            Ok(state) => handler(&state),
            Err(e) => {
                warn!("Admin API 获取读锁失败: {}", e);
                build_error_response(500, "内部错误")
            },
        }
    }

    /// 获取所有路由规则
    fn handle_routes(&self) -> (u16, String) {
        self.with_state(|state| {
            let routes = state.router.routes_summary();
            build_ok_response(routes)
        })
    }

    /// 获取所有上游集群信息
    fn handle_upstreams(&self) -> (u16, String) {
        self.with_state(|state| {
            let upstreams: Vec<UpstreamDTO> = state
                .clusters
                .values()
                .map(|cluster| cluster.summary())
                .collect();

            build_ok_response(upstreams)
        })
    }

    /// 获取当前限流配置
    fn handle_rate_limit(&self) -> (u16, String) {
        self.with_state(|state| {
            let dto = state.rate_limit_summary().unwrap_or_default();
            build_ok_response(dto)
        })
    }

    /// 手动触发配置热重载
    fn handle_reload(&self) -> (u16, String) {
        match self.control_plane.reload_simple() {
            Ok(()) => build_ok_response("重载成功"),
            Err(e) => build_error_response(400, &e),
        }
    }
}

/// pure function: 构建成功响应
///
/// - input: 可序列化的数据
/// - output:  (HTTP 状态码, JSON 字符串)
fn build_ok_response<T: Serialize>(data: T) -> (u16, String) {
    let resp = AdminResponse::ok(data);
    (200, serde_json::to_string(&resp).unwrap())
}

/// pure function: 构建错误响应
///
/// - input: HTTP 状态码、错误消息
/// - output: (HTTP 状态码, JSON 字符串)
fn build_error_response(status: u16, message: &str) -> (u16, String) {
    let resp = AdminResponse::<()>::error(message);
    (status, serde_json::to_string(&resp).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_ok_response() {
        let (status, body) = build_ok_response(vec!["route1", "route2"]);
        assert_eq!(status, 200);
        assert!(body.contains("\"status\":\"ok\""));
        assert!(body.contains("route1"));
    }

    #[test]
    fn test_build_error_response() {
        let (status, body) = build_error_response(500, "内部错误");
        assert_eq!(status, 500);
        assert!(body.contains("\"status\":\"error\""));
        assert!(body.contains("内部错误"));
    }
}
