use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{Filter, FilterContext, FilterName, FilterReject, FilterResult};
use async_trait::async_trait;
use jsonwebtoken::{Algorithm, Validation, decode};
use pingora_http::{RequestHeader, ResponseHeader};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use tracing::warn;

/// JWT Claims
#[derive(Debug, Deserialize, Serialize)]
pub struct JwtClaims {
    /// 签发者
    pub iss: String,
    /// 主题（通常是用户 ID）
    pub sub: String,
    /// 过期时间（Unix 时间戳）
    pub exp: usize,
    /// 签发时间（Unix 时间戳）
    #[serde(default)]
    pub iat: Option<usize>,
}

/// JWT 认证 Filter
pub struct AuthFilter;

impl AuthFilter {
    /// 从 Authorization 头中提取 Bearer Token
    fn extract_bearer_token(request_header: &RequestHeader) -> Result<String, &'static str> {
        let auth_header = request_header
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or("missing Authorization Header")?;

        if !auth_header.starts_with("Bearer ") {
            return Err("invalid Authorization Header, require Bearer type");
        }

        let token = auth_header[7..].trim();
        if token.is_empty() {
            return Err("Token is empty");
        }

        Ok(token.to_string())
    }

    /// 将 JWT Claims 序列化为字符串
    fn claim_to_string(value: &serde_json::Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[async_trait]
impl Filter for AuthFilter {
    fn name(&self) -> FilterName {
        FilterName::Auth
    }

    async fn request_filter(
        &self,
        ctx: &mut FilterContext,
        request_header: &mut RequestHeader,
        state: &Arc<RwLock<GatewayState>>,
    ) -> FilterResult {
        let (auth_config, needs_auth) = {
            let state_guard = state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });

            let auth_config = match &state_guard.auth_config {
                None => return FilterResult::Continue,
                Some(ac) => ac.clone(),
            };

            let route_id = match &ctx.route_id {
                None => return FilterResult::Continue,
                Some(id) => id,
            };

            let needs_auth = state_guard
                .registry
                .find_route(route_id)
                .map(|entry| entry.is_auth)
                .unwrap_or(false);

            (auth_config.clone(), needs_auth)
        };

        if !needs_auth {
            return FilterResult::Continue;
        }

        let token = match Self::extract_bearer_token(request_header) {
            Ok(t) => t,
            Err(reason) => {
                warn!(
                    "[AuthFilter] 认证失败，路径: {}，原因: {}",
                    ctx.path, reason
                );
                return FilterResult::Stop(FilterReject::Unauthorized);
            },
        };

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_required_spec_claims(&["exp", "iss"]);
        validation.iss = Some(HashSet::from([auth_config.issuer.clone()]));
        validation.leeway = 30;

        let token_data = match decode::<JwtClaims>(&token, &auth_config.decoding_key, &validation) {
            Ok(data) => data,
            Err(e) => {
                warn!("[AuthFilter] JWT 验证失败，路径: {}，原因: {}", ctx.path, e);
                return FilterResult::Stop(FilterReject::Unauthorized);
            },
        };

        ctx.auth_user_id = Some(token_data.claims.sub.clone());

        let claims_json = serde_json::to_value(&token_data.claims).unwrap_or_default();

        for claim_name in &auth_config.claims_to_forward {
            if let Some(value) = claims_json.get(claim_name) {
                let header_name = format!("X-User-{}", capitalize(claim_name));
                request_header
                    .insert_header(header_name, Self::claim_to_string(value))
                    .unwrap();
            }
        }

        FilterResult::Continue
    }

    async fn response_filter(
        &self,
        _ctx: &mut FilterContext,
        _response_header: &mut ResponseHeader,
    ) {
        // 认证 Filter 在响应阶段无操作
    }
}

/// 辅助函数: 首字母大写
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
