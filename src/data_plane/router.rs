#![allow(dead_code)]

use crate::control_plane::admin_api::dto::RouteDTO;
use regex::Regex;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

pub mod router_white_list;

/// 路由匹配结果
pub struct RouteMatch {
    /// 上游集群名称
    pub upstream: String,
    /// 匹配到的路由 ID
    pub route_id: String,
    /// 正则捕获组（仅 regex 类型有值）
    pub captures: Option<Vec<String>>,
}

/// 路由匹配类型
#[derive(Debug, Clone, PartialEq)]
pub enum MatchType {
    // 精确匹配
    Exact,
    // 前缀匹配
    Prefix,
    // 正则匹配
    Regex,
}

/// 路由器错误构建器
#[derive(Debug)]
pub enum RouterErrorBuilder {
    /// 正则表达式编译失败
    InvalidRegex {
        pattern: String,
        source: regex::Error,
    },
    /// route_id 重复
    DuplicateRouteId { route_id: String },
    /// 匹配类型无效
    InvalidMatchType { value: String },
}

impl Display for RouterErrorBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterErrorBuilder::InvalidRegex { pattern, source } => {
                write!(f, "正则表达式 '{}' 编译失败: {}", pattern, source)
            },
            RouterErrorBuilder::DuplicateRouteId { route_id } => {
                write!(f, "route_id '{}' 已存在", route_id)
            },
            RouterErrorBuilder::InvalidMatchType { value } => {
                write!(f, "无效的匹配类型 '{}', 可选: exact, prefix, regex", value)
            },
        }
    }
}

impl std::error::Error for RouterErrorBuilder {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RouterErrorBuilder::InvalidRegex { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 单条路由规则
pub struct RouteRule {
    pub route_id: String,
    pub match_type: MatchType,
    /// 路径模式（exact 为精确路径，regex 为正则表达式）
    pub path: String,
    /// 前缀（仅 prefix 类型使用）
    pub prefix: Option<String>,
    /// 允许的 HTTP 方法，空数组表示放行所有方法
    pub methods: Vec<String>,
    /// 上游集群名称
    pub upstream: String,
}

/// 路由表，只负责路径到集群名称的映射，不持有集群实例
pub struct Router {
    /// 精确匹配
    /// - key: 请求路径
    /// - value: 路由规则
    exact_routes: HashMap<String, RouteRule>,
    /// 正则匹配
    /// - key: 正则表达式
    /// - value: 路由规则
    regex_routes: Vec<(Regex, RouteRule)>,
    /// 前缀匹配：按前缀长度降序排列的有序列表
    /// - key: 路径前缀
    /// - value: 路由规则
    prefix_routes: Vec<(String, RouteRule)>,
}

impl Router {
    pub fn new() -> Self {
        Router {
            exact_routes: HashMap::new(),
            regex_routes: Vec::new(),
            prefix_routes: Vec::new(),
        }
    }

    /// 添加路由规则
    ///
    /// 自动校验正则合法性，route_id 重复时返回错误
    pub fn add_route(&mut self, rule: RouteRule) -> Result<(), RouterErrorBuilder> {
        // route_id 唯一性校验
        let route_id = &rule.route_id;
        let exists = self.exact_routes.values().any(|r| &r.route_id == route_id)
            || self
                .regex_routes
                .iter()
                .any(|(_, r)| &r.route_id == route_id)
            || self
                .prefix_routes
                .iter()
                .any(|(_, r)| &r.route_id == route_id);

        if exists {
            return Err(RouterErrorBuilder::DuplicateRouteId {
                route_id: rule.route_id.clone(),
            });
        }

        match rule.match_type {
            MatchType::Exact => {
                self.exact_routes.insert(rule.path.clone(), rule);
            },
            MatchType::Regex => {
                let compiled =
                    Regex::new(&rule.path).map_err(|e| RouterErrorBuilder::InvalidRegex {
                        pattern: rule.path.clone(),
                        source: e,
                    })?;
                self.regex_routes.push((compiled, rule));
            },
            MatchType::Prefix => {
                let prefix = rule.prefix.as_deref().unwrap_or(&rule.path);
                let prefix_entry = (prefix.to_string(), rule);
                self.prefix_routes.push(prefix_entry);
                self.prefix_routes.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            },
        }

        Ok(())
    }

    /// 路由匹配
    ///
    /// 匹配优先级：精确 > 正则 > 前缀
    /// 返回匹配到的路由信息
    pub fn match_route(&self, path: &str) -> Option<RouteMatch> {
        // 优先级 1：精确匹配
        if let Some(rule) = self.exact_routes.get(path) {
            return Some(RouteMatch {
                upstream: rule.upstream.clone(),
                route_id: rule.route_id.clone(),
                captures: None,
            });
        }

        // 优先级 2：正则匹配（按声明顺序遍历，先声明优先）
        for (regex, rule) in &self.regex_routes {
            if let Some(captures) = regex.captures(path) {
                let captured_groups: Vec<String> = captures
                    .iter()
                    .skip(1)
                    .filter_map(|c| c.map(|m| m.as_str().to_string()))
                    .collect();
                return Some(RouteMatch {
                    upstream: rule.upstream.clone(),
                    route_id: rule.route_id.clone(),
                    captures: if captured_groups.is_empty() {
                        None
                    } else {
                        Some(captured_groups)
                    },
                });
            }
        }

        // 优先级 3：前缀匹配（最长前缀优先）
        for (prefix, rule) in &self.prefix_routes {
            if path.starts_with(prefix.as_str()) {
                return Some(RouteMatch {
                    upstream: rule.upstream.clone(),
                    route_id: rule.route_id.clone(),
                    captures: None,
                });
            }
        }

        None
    }

    /// 获取所有路由规则的概要（用于 Admin API）
    pub fn routes_summary(&self) -> Vec<RouteDTO> {
        let mut summaries = Vec::new();

        for rule in self.exact_routes.values() {
            summaries.push(RouteDTO::from_rule(rule, MatchType::Exact));
        }
        for (_, rule) in &self.regex_routes {
            summaries.push(RouteDTO::from_rule(rule, MatchType::Regex));
        }
        for (_, rule) in &self.prefix_routes {
            summaries.push(RouteDTO::from_rule(rule, MatchType::Prefix));
        }

        summaries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 精确匹配
    #[test]
    fn test_exact_match() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "user-profile".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users/profile".to_string(),
                prefix: None,
                methods: vec!["GET".to_string()],
                upstream: "user-service".to_string(),
            })
            .unwrap();

        let result = router.match_route("/api/users/profile").unwrap();
        assert_eq!(result.upstream, "user-service");
        assert_eq!(result.route_id, "user-profile");
        assert!(result.captures.is_none());

        // 子路径不匹配
        assert!(router.match_route("/api/users/profile/extra").is_none());
    }

    /// 正则匹配
    #[test]
    fn test_regex_match() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "user-by-id".to_string(),
                match_type: MatchType::Regex,
                path: r"^/api/users/(\d+)$".to_string(),
                prefix: None,
                methods: vec!["GET".to_string()],
                upstream: "user-service".to_string(),
            })
            .unwrap();

        // 匹配数字 ID
        let result = router.match_route("/api/users/123").unwrap();
        assert_eq!(result.upstream, "user-service");
        assert_eq!(result.captures, Some(vec!["123".to_string()]));

        // 不匹配非数字
        assert!(router.match_route("/api/users/abc").is_none());
    }

    /// 正则匹配优先级：先声明优先
    #[test]
    fn test_regex_priority_order() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "v2-api".to_string(),
                match_type: MatchType::Regex,
                path: r"^/api/(v2)/.+$".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "v2-service".to_string(),
            })
            .unwrap();
        router
            .add_route(RouteRule {
                route_id: "any-api".to_string(),
                match_type: MatchType::Regex,
                path: r"^/api/.+$".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "default-service".to_string(),
            })
            .unwrap();

        // 两个正则都能匹配，先声明的 v2-api 优先
        let result = router.match_route("/api/v2/users").unwrap();
        assert_eq!(result.route_id, "v2-api");

        // 只有 any-api 匹配
        let result = router.match_route("/api/v1/users").unwrap();
        assert_eq!(result.route_id, "any-api");
    }

    /// 精确 > 正则 > 前缀优先级
    #[test]
    fn test_match_priority() {
        let mut router = Router::new();
        // 精确匹配
        router
            .add_route(RouteRule {
                route_id: "exact-route".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "exact-svc".to_string(),
            })
            .unwrap();
        // 正则匹配（也能匹配 /api/users）
        router
            .add_route(RouteRule {
                route_id: "regex-route".to_string(),
                match_type: MatchType::Regex,
                path: r"^/api/\w+$".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "regex-svc".to_string(),
            })
            .unwrap();
        // 前缀匹配（也能匹配 /api/users）
        router
            .add_route(RouteRule {
                route_id: "prefix-route".to_string(),
                match_type: MatchType::Prefix,
                path: String::new(),
                prefix: Some("/api/".to_string()),
                methods: vec![],
                upstream: "prefix-svc".to_string(),
            })
            .unwrap();

        let result = router.match_route("/api/users").unwrap();
        assert_eq!(result.route_id, "exact-route");

        let result = router.match_route("/api/orders").unwrap();
        assert_eq!(result.route_id, "regex-route");

        let result = router.match_route("/api/orders/123").unwrap();
        assert_eq!(result.route_id, "prefix-route");
    }

    /// 前缀匹配：最长前缀优先
    #[test]
    fn test_prefix_longest_first() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "short-prefix".to_string(),
                match_type: MatchType::Prefix,
                path: String::new(),
                prefix: Some("/api".to_string()),
                methods: vec![],
                upstream: "default".to_string(),
            })
            .unwrap();
        router
            .add_route(RouteRule {
                route_id: "long-prefix".to_string(),
                match_type: MatchType::Prefix,
                path: String::new(),
                prefix: Some("/api/v2".to_string()),
                methods: vec![],
                upstream: "v2".to_string(),
            })
            .unwrap();

        assert_eq!(
            router.match_route("/api/v2/users").unwrap().route_id,
            "long-prefix"
        );
        assert_eq!(
            router.match_route("/api/v1/users").unwrap().route_id,
            "short-prefix"
        );
        assert!(router.match_route("/health").is_none());
    }

    /// 空路由表返回 None
    #[test]
    fn test_empty_router_returns_none() {
        let router = Router::new();
        assert!(router.match_route("/any/path").is_none());
    }

    /// route_id 重复报错
    #[test]
    fn test_duplicate_route_id_error() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "dup".to_string(),
                match_type: MatchType::Exact,
                path: "/a".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc".to_string(),
            })
            .unwrap();

        let result = router.add_route(RouteRule {
            route_id: "dup".to_string(),
            match_type: MatchType::Prefix,
            path: String::new(),
            prefix: Some("/b".to_string()),
            methods: vec![],
            upstream: "svc".to_string(),
        });

        assert!(matches!(
            result,
            Err(RouterErrorBuilder::DuplicateRouteId { .. })
        ));
    }

    /// 正则表达式非法报错
    #[test]
    fn test_invalid_regex_error() {
        let mut router = Router::new();
        let result = router.add_route(RouteRule {
            route_id: "bad-regex".to_string(),
            match_type: MatchType::Regex,
            path: r"^/api/[invalid(".to_string(),
            prefix: None,
            methods: vec![],
            upstream: "svc".to_string(),
        });

        assert!(matches!(
            result,
            Err(RouterErrorBuilder::InvalidRegex { .. })
        ));
    }
}
