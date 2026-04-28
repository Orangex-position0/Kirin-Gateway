use crate::data_plane::router::{MatchType, RouteRule};
use serde::Serialize;

/// DTO used by admin api

/// 限流配置概要 DTO (used by Admin API)
#[derive(Serialize)]
pub struct RateLimitDTO {
    // 是否启用限流
    pub enabled: bool,
    // 令牌桶容量，未启用时为 None
    pub capacity: Option<usize>,
    // 令牌填充速率（个/秒），未启用时为 None
    pub refill_rate: Option<usize>,
}

/// 路由配置概要 DTO (used by Admin API)
#[derive(Serialize)]
pub struct RouteDTO {
    pub route_id: String,
    pub path: String,
    pub match_type: String,
    pub methods: Vec<String>,
    pub upstream: String,
}

/// 接口注册详情 DTO (used by Admin API)
#[derive(Serialize)]
pub struct RouteRegistryDTO {
    pub route_id: String,
    pub path: String,
    pub prefix: Option<String>,
    pub match_type: String,
    pub methods: Vec<String>,
    pub upstream: String,
    pub applicant: String,
    pub applied_at: String,
    pub description: String,
}

impl RouteDTO {
    /// 从 RouteRule 构建 RouteDTO
    pub fn from_rule(rule: &RouteRule, match_type: MatchType) -> Self {
        RouteDTO {
            route_id: rule.route_id.clone(),
            path: rule.path.clone(),
            match_type: match_type_to_str(&match_type),
            methods: rule.methods.clone(),
            upstream: rule.upstream.clone(),
        }
    }
}

impl From<&crate::data_plane::router::router_white_list::RouteEntry> for RouteRegistryDTO {
    fn from(entry: &crate::data_plane::router::router_white_list::RouteEntry) -> Self {
        RouteRegistryDTO {
            route_id: entry.route_id.clone(),
            path: entry.path.clone(),
            prefix: entry.prefix.clone(),
            match_type: match_type_to_str(&entry.match_type),
            methods: entry.methods.clone(),
            upstream: entry.upstream.clone(),
            applicant: entry.applicant.clone(),
            applied_at: entry.applied_at.clone(),
            description: entry.description.clone(),
        }
    }
}

fn match_type_to_str(mt: &MatchType) -> String {
    match mt {
        MatchType::Exact => "exact".to_string(),
        MatchType::Prefix => "prefix".to_string(),
        MatchType::Regex => "regex".to_string(),
    }
}

/// 上游配置概要 DTO (used by Admin API)
#[derive(Serialize)]
pub struct UpstreamDTO {
    pub name: String,
    pub nodes: Vec<String>,
}
