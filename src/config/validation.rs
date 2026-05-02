#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::Formatter;

use super::types::{
    AuthConfigRaw, KirinConfig, RateLimitConfig, RouteConfig, ServerConfig, UpstreamConfig,
};

/// 配置验证 error
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// 校验失败的字段路径
    pub field: String,
    /// 错误描述
    pub message: String,
    /// 关联的路由 ID (路由校验时使用)
    pub route_id: Option<String>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.route_id {
            None => {
                write!(f, "{}: {}", self.field, self.message)
            },
            Some(rid) => {
                write!(f, "[route={}] {}: {}", rid, self.field, self.message)
            },
        }
    }
}

impl std::error::Error for ValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

/// 全量配置校验
///
/// pure function
pub fn validate_config(config: &KirinConfig) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    validate_server(&config.server, &mut errors);
    validate_routes(&config.routes, &config.upstreams, &mut errors);
    validate_upstreams(&config.upstreams, &mut errors);
    if let Some(ref rl) = config.rate_limit {
        validate_rate_limit(rl, &mut errors);
    }
    if let Some(ref auth) = config.auth {
        validate_auth(auth, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// 单路由配置校验 (供灰度发布 Admin API 复用)
pub fn validate_route_config(
    route: &RouteConfig,
    upstreams: &HashMap<String, UpstreamConfig>,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    validate_routes(std::slice::from_ref(route), upstreams, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// server 配置校验
fn validate_server(server: &ServerConfig, errors: &mut Vec<ValidationError>) {
    if let Err(e) = parse_socket_addr(&server.listen) {
        errors.push(ValidationError {
            field: "server.listen".to_string(),
            message: format!("invalid listen address: {}", e),
            route_id: None,
        });
    }

    if let Some(0) = server.threads {
        errors.push(ValidationError {
            field: "server.threads".to_string(),
            message: "worker threads of gateway must be greater than 0".to_string(),
            route_id: None,
        });
    }
}

/// route 配置校验
fn validate_routes(
    routes: &[RouteConfig],
    upstreams: &HashMap<String, UpstreamConfig>,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen_ids = HashSet::new();

    for route in routes {
        // route_id must not be empty
        if route.route_id.trim().is_empty() {
            errors.push(ValidationError {
                field: "route.route_id".to_string(),
                message: "route_id must not be empty".to_string(),
                route_id: None,
            });
            continue;
        }

        // route_id is unique
        if !seen_ids.insert(route.route_id.clone()) {
            errors.push(ValidationError {
                field: "route.route_id".to_string(),
                message: format!("route_id '{}' is duplicated", route.route_id),
                route_id: Some(route.route_id.clone()),
            });
        }

        // match_type must be a valid enum value
        let valid_match_type = ["exact", "prefix", "regex"];
        if !valid_match_type.contains(&route.match_type.as_str()) {
            errors.push(ValidationError {
                field: "route.match_type".to_string(),
                message: format!(
                    "Invalid match_type {}. You can choose: {}",
                    route.match_type,
                    valid_match_type.join(", ")
                ),
                route_id: Some(route.route_id.clone()),
            });
        }

        // path / path_prefix is required based on match_type
        match route.match_type.as_str() {
            "exact" | "regex" => {
                if route.path.as_ref().is_none_or(|p| p.trim().is_empty()) {
                    errors.push(ValidationError {
                        field: "route.path".to_string(),
                        message: format!(
                            "match_type 为 '{}' 时 path 必须存在且非空",
                            route.match_type
                        ),
                        route_id: Some(route.route_id.clone()),
                    });
                }
            },
            "prefix" => {
                if route
                    .path_prefix
                    .as_ref()
                    .is_none_or(|p| p.trim().is_empty())
                {
                    errors.push(ValidationError {
                        field: "route.path_prefix".to_string(),
                        message: "match_type 为 'prefix' 时 path_prefix 必须存在且非空".to_string(),
                        route_id: Some(route.route_id.clone()),
                    });
                }
            },
            _ => {},
        }

        // upstream must reference an existing upstream
        if !upstreams.contains_key(&route.upstream) {
            errors.push(ValidationError {
                field: "route.upstream".to_string(),
                message: format!(
                    "upstream '{}' does not exist. You can choose: {:?}",
                    route.upstream,
                    upstreams.keys().collect::<Vec<_>>()
                ),
                route_id: Some(route.route_id.clone()),
            });
        }
    }
}

/// upstream 配置校验
fn validate_upstreams(
    upstreams: &HashMap<String, UpstreamConfig>,
    errors: &mut Vec<ValidationError>,
) {
    for (name, upstream) in upstreams {
        // nodes 非空
        if upstream.nodes.is_empty() {
            errors.push(ValidationError {
                field: format!("upstream.{}.nodes", name),
                message: "至少需要一个节点".to_string(),
                route_id: None,
            });
        }

        for (i, node) in upstream.nodes.iter().enumerate() {
            // addr 格式
            if let Err(e) = parse_socket_addr(&node.addr) {
                errors.push(ValidationError {
                    field: format!("upstream.{}.nodes[{}].addr", name, i),
                    message: format!("地址格式无效: {}", e),
                    route_id: None,
                });
            }

            // weight > 0
            if node.weight == 0 {
                errors.push(ValidationError {
                    field: format!("upstream.{}.nodes[{}].weight", name, i),
                    message: "权重必须大于 0".to_string(),
                    route_id: None,
                });
            }
        }
    }
}

/// rate_limit 配置校验
fn validate_rate_limit(rl: &RateLimitConfig, errors: &mut Vec<ValidationError>) {
    if rl.capacity == 0 {
        errors.push(ValidationError {
            field: "rate_limit.capacity".to_string(),
            message: "令牌桶容量必须大于 0".to_string(),
            route_id: None,
        });
    }
    if rl.refill_rate == 0 {
        errors.push(ValidationError {
            field: "rate_limit.refill_rate".to_string(),
            message: "令牌补充速率必须大于 0".to_string(),
            route_id: None,
        });
    }
}

/// auth 配置校验
fn validate_auth(auth: &AuthConfigRaw, errors: &mut Vec<ValidationError>) {
    if auth.algorithm != "RS256" {
        errors.push(ValidationError {
            field: "auth.algorithm".to_string(),
            message: format!("签名算法 '{}' 无效，当前仅支持 RS256", auth.algorithm),
            route_id: None,
        });
    }
    if auth.public_key_path.trim().is_empty() {
        errors.push(ValidationError {
            field: "auth.public_key_path".to_string(),
            message: "公钥文件路径不能为空".to_string(),
            route_id: None,
        });
    }
    if auth.issuer.trim().is_empty() {
        errors.push(ValidationError {
            field: "auth.issuer".to_string(),
            message: "Token 签发者不能为空".to_string(),
            route_id: None,
        });
    }
}

/// 解析 ip:port 格式的地址
fn parse_socket_addr(addr: &str) -> Result<(), String> {
    use std::net::SocketAddr;
    addr.parse::<SocketAddr>()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 构建最小合法配置
    fn make_valid_config() -> KirinConfig {
        use super::super::types::{
            KirinConfig, NodeConfig, RouteConfig, ServerConfig, UpstreamConfig,
        };

        KirinConfig {
            server: ServerConfig {
                listen: "0.0.0.0:8080".to_string(),
                threads: Some(2),
                tls: None,
            },
            routes: vec![RouteConfig {
                route_id: "test-route".to_string(),
                path: Some("/api/test".to_string()),
                path_prefix: None,
                match_type: "exact".to_string(),
                methods: vec![],
                upstream: "test-svc".to_string(),
                applicant: "test".to_string(),
                applied_at: "2026-01-01T00:00:00+08:00".to_string(),
                description: "test".to_string(),
                is_auth: false,
            }],
            upstreams: [(
                "test-svc".to_string(),
                UpstreamConfig {
                    nodes: vec![NodeConfig {
                        addr: "127.0.0.1:9001".to_string(),
                        weight: 1,
                    }],
                    algorithm: "round_robin".to_string(),
                    health_check: None,
                },
            )]
            .into_iter()
            .collect(),
            rate_limit: None,
            admin: None,
            auth: None,
        }
    }

    /// server 配置校验test
    #[test]
    fn test_validate_config_ok() {
        let config = make_valid_config();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_validate_server_listen_invalid_format() {
        let mut config = make_valid_config();
        config.server.listen = "not-an-address".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "server.listen"));
    }

    #[test]
    fn test_validate_server_listen_port_out_of_range() {
        let mut config = make_valid_config();
        config.server.listen = "0.0.0.0:99999".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "server.listen"));
    }

    #[test]
    fn test_validate_server_threads_zero() {
        let mut config = make_valid_config();
        config.server.threads = Some(0);
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "server.threads"));
    }

    /// route 配置校验test
    #[test]
    fn test_validate_route_id_empty() {
        let mut config = make_valid_config();
        config.routes[0].route_id = "  ".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "route.route_id"));
    }

    #[test]
    fn test_validate_route_id_duplicate() {
        let mut config = make_valid_config();
        let mut dup_route = config.routes[0].clone();
        dup_route.path = Some("/api/dup".to_string());
        config.routes.push(dup_route);
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.message.contains("duplicated")));
    }

    #[test]
    fn test_validate_route_match_type_invalid() {
        let mut config = make_valid_config();
        config.routes[0].match_type = "glob".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "route.match_type"));
    }

    #[test]
    fn test_validate_route_path_missing_for_exact() {
        let mut config = make_valid_config();
        config.routes[0].path = None;
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "route.path"));
    }

    #[test]
    fn test_validate_route_path_prefix_missing_for_prefix() {
        let mut config = make_valid_config();
        config.routes[0].match_type = "prefix".to_string();
        config.routes[0].path = None;
        config.routes[0].path_prefix = None;
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "route.path_prefix"));
    }

    #[test]
    fn test_validate_route_upstream_not_found() {
        let mut config = make_valid_config();
        config.routes[0].upstream = "nonexistent".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "route.upstream"));
    }

    /// upstream 配置校验test
    #[test]
    fn test_validate_upstream_nodes_empty() {
        let mut config = make_valid_config();
        config.upstreams.get_mut("test-svc").unwrap().nodes.clear();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field.contains("nodes")));
    }

    #[test]
    fn test_validate_upstream_addr_invalid() {
        let mut config = make_valid_config();
        config.upstreams.get_mut("test-svc").unwrap().nodes[0].addr = "bad-addr".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field.contains("addr")));
    }

    #[test]
    fn test_validate_upstream_weight_zero() {
        let mut config = make_valid_config();
        config.upstreams.get_mut("test-svc").unwrap().nodes[0].weight = 0;
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field.contains("weight")));
    }

    /// rate_limit & auth 配置校验test
    #[test]
    fn test_validate_rate_limit_capacity_zero() {
        let mut config = make_valid_config();
        config.rate_limit = Some(RateLimitConfig {
            capacity: 0,
            refill_rate: 10,
        });
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "rate_limit.capacity"));
    }

    #[test]
    fn test_validate_rate_limit_refill_rate_zero() {
        let mut config = make_valid_config();
        config.rate_limit = Some(RateLimitConfig {
            capacity: 100,
            refill_rate: 0,
        });
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "rate_limit.refill_rate"));
    }

    #[test]
    fn test_validate_auth_algorithm_invalid() {
        let mut config = make_valid_config();
        config.auth = Some(AuthConfigRaw {
            algorithm: "HS256".to_string(),
            public_key_path: "/path/to/key.pem".to_string(),
            issuer: "test-issuer".to_string(),
            claims_to_forward: vec![],
        });
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "auth.algorithm"));
    }

    #[test]
    fn test_validate_auth_issuer_empty() {
        let mut config = make_valid_config();
        config.auth = Some(AuthConfigRaw {
            algorithm: "RS256".to_string(),
            public_key_path: "/path/to/key.pem".to_string(),
            issuer: "  ".to_string(),
            claims_to_forward: vec![],
        });
        let errors = validate_config(&config).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "auth.issuer"));
    }

    /// 批量收集错误 + 单路由校验test
    #[test]
    fn test_validate_batch_multiple_errors() {
        let mut config = make_valid_config();
        config.server.listen = "bad".to_string();
        config.server.threads = Some(0);
        config.routes[0].upstream = "missing".to_string();
        let errors = validate_config(&config).unwrap_err();
        assert!(
            errors.len() >= 3,
            "应收集至少 3 个错误，实际: {}",
            errors.len()
        );
    }

    #[test]
    fn test_validate_route_config_single_route() {
        use super::super::types::RouteConfig;

        let route = RouteConfig {
            route_id: "new-route".to_string(),
            path: Some("/api/new".to_string()),
            path_prefix: None,
            match_type: "exact".to_string(),
            methods: vec![],
            upstream: "nonexistent".to_string(),
            applicant: "test".to_string(),
            applied_at: "2026-01-01T00:00:00+08:00".to_string(),
            description: "test".to_string(),
            is_auth: false,
        };
        let upstreams = HashMap::new();
        let errors = validate_route_config(&route, &upstreams).unwrap_err();
        assert!(errors.iter().any(|e| e.field == "route.upstream"));
    }
}
