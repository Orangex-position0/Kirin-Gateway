#![allow(dead_code)]

use crate::config::{CanaryConfig, CanaryMatchType, StickyStrategy};
use crate::control_plane::admin_api::dto::RouteDTO;
use ipnet::IpNet;
use regex::Regex;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// 灰度配置
    pub canary: Option<CanaryConfig>,
    /// IP 白名单（预编译的 CIDR 列表）
    pub ip_whitelist: Option<Vec<IpNet>>,
    /// IP 黑名单（预编译的 CIDR 列表）
    pub ip_blacklist: Option<Vec<IpNet>>,
}

/// 路由表，只负责路径到集群名称的映射，不持有集群实例
pub struct Router {
    /// 精确匹配
    /// - key: 请求路径
    /// - value: 同路径下的路由规则列表（灰度场景下同路径可有多条路由）
    exact_routes: HashMap<String, Vec<RouteRule>>,
    /// 正则匹配
    /// - key: 正则表达式
    /// - value: 路由规则
    regex_routes: Vec<(Regex, RouteRule)>,
    /// 前缀匹配：按前缀长度降序排列的有序列表
    /// - key: 路径前缀
    /// - value: 路由规则
    prefix_routes: Mutex<Vec<(String, RouteRule)>>,
    /// 前缀路由延迟排序标志，被标记的路由会在首次查询时才重排序
    prefix_dirty: AtomicBool,
}

/// 路径匹配候选项（携带足够信息用于灰度分流决策）
struct RouteCandidate {
    upstream: String,
    route_id: String,
    captures: Option<Vec<String>>,
    canary: Option<CanaryConfig>,
}

impl Router {
    pub fn new() -> Self {
        Router {
            exact_routes: HashMap::new(),
            regex_routes: Vec::new(),
            prefix_routes: Mutex::new(Vec::new()),
            prefix_dirty: AtomicBool::new(false),
        }
    }

    /// 添加路由规则
    ///
    /// 自动校验正则合法性，route_id 重复时返回错误
    pub fn add_route(&mut self, rule: RouteRule) -> Result<(), RouterErrorBuilder> {
        // route_id 唯一性校验
        let route_id = &rule.route_id;
        let exists = self
            .exact_routes
            .values()
            .flatten()
            .any(|r| &r.route_id == route_id)
            || self
                .regex_routes
                .iter()
                .any(|(_, r)| &r.route_id == route_id)
            || self
                .prefix_routes
                .lock()
                .unwrap()
                .iter()
                .any(|(_, r)| &r.route_id == route_id);

        if exists {
            return Err(RouterErrorBuilder::DuplicateRouteId {
                route_id: rule.route_id.clone(),
            });
        }

        match rule.match_type {
            MatchType::Exact => {
                self.exact_routes
                    .entry(rule.path.clone())
                    .or_default()
                    .push(rule);
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
                self.prefix_routes
                    .lock()
                    .unwrap()
                    .push((prefix.to_string(), rule));
                self.prefix_dirty.store(true, Ordering::Release);
            },
        }

        Ok(())
    }

    /// 路由匹配（支持灰度分流）
    ///
    /// 匹配优先级：精确 > 正则 > 前缀（路径层面）
    /// 灰度优先级：条件匹配 > Cookie Sticky > IP Hash/权重 > 兜底
    pub fn match_route(&self, path: &str, canary_ctx: &CanaryContext) -> Option<RouteMatch> {
        // 阶段 1：路径匹配，收集所有命中路由
        let matched = self.collect_path_matches(path);
        if matched.is_empty() {
            return None;
        }

        // 单条路由直接返回
        if matched.len() == 1 {
            return matched.into_iter().next().map(|c| RouteMatch {
                upstream: c.upstream,
                route_id: c.route_id,
                captures: c.captures,
            });
        }

        // 阶段 2：灰度分流

        // 2a. 条件匹配优先（Header/Cookie/Query）
        for candidate in &matched {
            if let Some(canary) = &candidate.canary
                && !canary.match_rules.is_empty()
                && matches_conditions(canary, canary_ctx)
            {
                return Some(RouteMatch {
                    upstream: candidate.upstream.clone(),
                    route_id: candidate.route_id.clone(),
                    captures: candidate.captures.clone(),
                });
            }
        }

        // 2b. Cookie Sticky 检查
        if let Some(result) = check_cookie_sticky(&matched, canary_ctx) {
            return result;
        }

        // 2c. 计算分流值：IP Hash 或随机
        let route_value = compute_route_value(&matched, canary_ctx);

        // 2d. 权重分流
        let mut cumulative: u8 = 0;
        for candidate in &matched {
            if let Some(canary) = &candidate.canary {
                cumulative = cumulative.saturating_add(canary.weight);
                if route_value < cumulative {
                    return Some(RouteMatch {
                        upstream: candidate.upstream.clone(),
                        route_id: candidate.route_id.clone(),
                        captures: candidate.captures.clone(),
                    });
                }
            }
        }

        // 2e. 兜底（无 canary 的路由）
        for candidate in matched {
            if candidate.canary.is_none() {
                return Some(RouteMatch {
                    upstream: candidate.upstream,
                    route_id: candidate.route_id,
                    captures: candidate.captures,
                });
            }
        }

        None
    }

    /// 按 route_id 查找路由规则信息（用于 IP 过滤等场景）
    pub fn find_route_info_by_id(&self, route_id: &str) -> Option<RouteRuleInfo> {
        for rules in self.exact_routes.values() {
            for rule in rules {
                if rule.route_id == route_id {
                    return Some(RouteRuleInfo {
                        ip_whitelist: rule.ip_whitelist.clone(),
                        ip_blacklist: rule.ip_blacklist.clone(),
                    });
                }
            }
        }
        for (_, rule) in &self.regex_routes {
            if rule.route_id == route_id {
                return Some(RouteRuleInfo {
                    ip_whitelist: rule.ip_whitelist.clone(),
                    ip_blacklist: rule.ip_blacklist.clone(),
                });
            }
        }
        for (_, rule) in self.prefix_routes.lock().unwrap().iter() {
            if rule.route_id == route_id {
                return Some(RouteRuleInfo {
                    ip_whitelist: rule.ip_whitelist.clone(),
                    ip_blacklist: rule.ip_blacklist.clone(),
                });
            }
        }
        None
    }

    /// 收集所有路径匹配的路由候选
    fn collect_path_matches(&self, path: &str) -> Vec<RouteCandidate> {
        let mut results = Vec::new();

        // 精确匹配
        if let Some(rules) = self.exact_routes.get(path) {
            for rule in rules {
                results.push(RouteCandidate {
                    upstream: rule.upstream.clone(),
                    route_id: rule.route_id.clone(),
                    captures: None,
                    canary: rule.canary.clone(),
                });
            }
        }

        // 正则匹配
        for (regex, rule) in &self.regex_routes {
            if let Some(captures) = regex.captures(path) {
                let captured_groups: Vec<String> = captures
                    .iter()
                    .skip(1)
                    .filter_map(|c| c.map(|m| m.as_str().to_string()))
                    .collect();
                results.push(RouteCandidate {
                    upstream: rule.upstream.clone(),
                    route_id: rule.route_id.clone(),
                    captures: if captured_groups.is_empty() {
                        None
                    } else {
                        Some(captured_groups)
                    },
                    canary: rule.canary.clone(),
                });
            }
        }

        // 前缀匹配
        {
            let mut routes = self.prefix_routes.lock().unwrap();
            if self.prefix_dirty.load(Ordering::Acquire) {
                routes.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
                self.prefix_dirty.store(false, Ordering::Release);
            }
            for (prefix, rule) in routes.iter() {
                if path.starts_with(prefix) {
                    results.push(RouteCandidate {
                        upstream: rule.upstream.clone(),
                        route_id: rule.route_id.clone(),
                        captures: None,
                        canary: rule.canary.clone(),
                    });
                }
            }
        }

        results
    }

    /// 获取所有路由规则的概要（用于 Admin API）
    pub fn routes_summary(&self) -> Vec<RouteDTO> {
        let mut summaries = Vec::new();

        for rules in self.exact_routes.values() {
            for rule in rules {
                summaries.push(RouteDTO::from_rule(rule, MatchType::Exact));
            }
        }
        for (_, rule) in &self.regex_routes {
            summaries.push(RouteDTO::from_rule(rule, MatchType::Regex));
        }
        for (_, rule) in self.prefix_routes.lock().unwrap().iter() {
            summaries.push(RouteDTO::from_rule(rule, MatchType::Prefix));
        }

        summaries
    }

    /// 按 route_id 移除路由规则
    pub fn remove_route(&mut self, route_id: &str) -> bool {
        // 在精确匹配表中查找
        for rules in self.exact_routes.values_mut() {
            let before = rules.len();
            rules.retain(|r| r.route_id != route_id);
            if rules.len() < before {
                return true;
            }
        }

        // 在正则匹配表中查找
        let before = self.regex_routes.len();
        self.regex_routes.retain(|(_, r)| r.route_id != route_id);
        if self.regex_routes.len() < before {
            return true;
        }

        // 在前缀匹配表中查找
        {
            let mut routes = self.prefix_routes.lock().unwrap();
            let before = routes.len();
            routes.retain(|(_, r)| r.route_id != route_id);
            if routes.len() < before {
                return true;
            }
        }
        false
    }
}

/// 判断当前请求是否符合灰度路由规则
fn matches_conditions(canary: &CanaryConfig, ctx: &CanaryContext) -> bool {
    canary.match_rules.iter().any(|rule| match rule.match_type {
        CanaryMatchType::Header => ctx
            .headers
            .get(&rule.key)
            .map(|v| v == &rule.value)
            .unwrap_or(false),
        CanaryMatchType::Cookie => ctx
            .cookies
            .get(&rule.key)
            .map(|v| v == &rule.value)
            .unwrap_or(false),
        CanaryMatchType::Query => ctx
            .query_params
            .get(&rule.key)
            .map(|v| v == &rule.value)
            .unwrap_or(false),
    })
}

/// Cookie Sticky 检查
///
/// 遍历配置了 sticky: cookie 的候选路由，
/// 查找请求 Cookie 中是否有匹配的 route_id。
fn check_cookie_sticky(
    candidates: &[RouteCandidate],
    ctx: &CanaryContext,
) -> Option<Option<RouteMatch>> {
    let cookie_name = candidates.iter().find_map(|c| {
        c.canary.as_ref().and_then(|canary| {
            if matches!(canary.stick, Some(StickyStrategy::Cookie)) {
                Some(
                    canary
                        .sticky_cookie
                        .as_ref()
                        .map(|c| c.name.clone())
                        .unwrap_or_else(|| "kirin_canary".to_string()),
                )
            } else {
                None
            }
        })
    })?;

    let cookie_value = ctx.cookies.get(&cookie_name)?;

    for candidate in candidates {
        if candidate.route_id == *cookie_value {
            return Some(Some(RouteMatch {
                upstream: candidate.upstream.clone(),
                route_id: candidate.route_id.clone(),
                captures: candidate.captures.clone(),
            }));
        }
    }

    None
}

/// 计算分流值：IP Hash 或随机
fn compute_route_value(candidates: &[RouteCandidate], ctx: &CanaryContext) -> u8 {
    let use_ip_hash = candidates.iter().any(|c| {
        c.canary
            .as_ref()
            .map(|canary| matches!(canary.stick, Some(StickyStrategy::IpHash)))
            .unwrap_or(false)
    });

    if use_ip_hash && let Some(ref ip) = ctx.client_ip {
        let mut hasher = DefaultHasher::new();
        ip.hash(&mut hasher);
        return (hasher.finish() % 100) as u8;
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    ((nanos >> 7) % 100) as u8
}

/// 路由规则摘要（用于 IP 过滤等场景）
pub struct RouteRuleInfo {
    pub ip_whitelist: Option<Vec<IpNet>>,
    pub ip_blacklist: Option<Vec<IpNet>>,
}

/// 灰度分流上下文，携带请求的 header、cookie、query 参数和客户端 IP 信息
pub struct CanaryContext {
    /// 请求头键值对
    pub headers: HashMap<String, String>,
    /// Cookie 键值对（从 Cookie 请求头解析）
    pub cookies: HashMap<String, String>,
    /// URL query 参数键值对
    pub query_params: HashMap<String, String>,
    /// 客户端 IP 地址（用于 IP Hash 确定性分流）
    pub client_ip: Option<String>,
}

impl CanaryContext {
    /// 空上下文（无灰度信息）
    pub fn empty() -> Self {
        CanaryContext {
            headers: HashMap::new(),
            cookies: HashMap::new(),
            query_params: HashMap::new(),
            client_ip: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CanaryConfig, CanaryMatchRule, CanaryMatchType, StickyCookieConfig};

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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let result = router
            .match_route("/api/users/profile", &CanaryContext::empty())
            .unwrap();
        assert_eq!(result.upstream, "user-service");
        assert_eq!(result.route_id, "user-profile");
        assert!(result.captures.is_none());

        assert!(
            router
                .match_route("/api/users/profile/extra", &CanaryContext::empty())
                .is_none()
        );
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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let result = router
            .match_route("/api/users/123", &CanaryContext::empty())
            .unwrap();
        assert_eq!(result.upstream, "user-service");
        assert_eq!(result.captures, Some(vec!["123".to_string()]));

        assert!(
            router
                .match_route("/api/users/abc", &CanaryContext::empty())
                .is_none()
        );
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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let result = router
            .match_route("/api/v2/users", &CanaryContext::empty())
            .unwrap();
        assert_eq!(result.route_id, "v2-api");

        let result = router
            .match_route("/api/v1/users", &CanaryContext::empty())
            .unwrap();
        assert_eq!(result.route_id, "any-api");
    }

    /// 精确 > 正则 > 前缀优先级
    #[test]
    fn test_match_priority() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "exact-route".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "exact-svc".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();
        router
            .add_route(RouteRule {
                route_id: "regex-route".to_string(),
                match_type: MatchType::Regex,
                path: r"^/api/\w+$".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "regex-svc".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();
        router
            .add_route(RouteRule {
                route_id: "prefix-route".to_string(),
                match_type: MatchType::Prefix,
                path: String::new(),
                prefix: Some("/api/".to_string()),
                methods: vec![],
                upstream: "prefix-svc".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let result = router
            .match_route("/api/users", &CanaryContext::empty())
            .unwrap();
        assert_eq!(result.route_id, "exact-route");

        let result = router
            .match_route("/api/orders", &CanaryContext::empty())
            .unwrap();
        assert_eq!(result.route_id, "regex-route");

        let result = router
            .match_route("/api/orders/123", &CanaryContext::empty())
            .unwrap();
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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        assert_eq!(
            router
                .match_route("/api/v2/users", &CanaryContext::empty())
                .unwrap()
                .route_id,
            "long-prefix"
        );
        assert_eq!(
            router
                .match_route("/api/v1/users", &CanaryContext::empty())
                .unwrap()
                .route_id,
            "short-prefix"
        );
        assert!(
            router
                .match_route("/health", &CanaryContext::empty())
                .is_none()
        );
    }

    /// 空路由表返回 None
    #[test]
    fn test_empty_router_returns_none() {
        let router = Router::new();
        assert!(
            router
                .match_route("/any/path", &CanaryContext::empty())
                .is_none()
        );
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
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let result = router.add_route(RouteRule {
            route_id: "dup".to_string(),
            match_type: MatchType::Prefix,
            path: String::new(),
            prefix: Some("/b".to_string()),
            methods: vec![],
            upstream: "svc".to_string(),
            canary: None,
            ip_whitelist: None,
            ip_blacklist: None,
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
            canary: None,
            ip_whitelist: None,
            ip_blacklist: None,
        });

        assert!(matches!(
            result,
            Err(RouterErrorBuilder::InvalidRegex { .. })
        ));
    }

    /// 灰度条件匹配：带 X-Canary header 走灰度路由
    #[test]
    fn test_canary_header_match() {
        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: Some(CanaryConfig {
                    weight: 100,
                    match_rules: vec![],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 0,
                    match_rules: vec![CanaryMatchRule {
                        match_type: CanaryMatchType::Header,
                        key: "X-Canary".to_string(),
                        value: "true".to_string(),
                    }],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext {
            headers: HashMap::from([("X-Canary".to_string(), "true".to_string())]),
            cookies: HashMap::new(),
            query_params: HashMap::new(),
            client_ip: None,
        };
        let result = router.match_route("/api/users", &ctx).unwrap();
        assert_eq!(result.route_id, "canary");

        let ctx = CanaryContext::empty();
        let result = router.match_route("/api/users", &ctx).unwrap();
        assert_eq!(result.route_id, "stable");
    }

    /// 灰度条件匹配：cookie 匹配
    #[test]
    fn test_canary_cookie_match() {
        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 0,
                    match_rules: vec![CanaryMatchRule {
                        match_type: CanaryMatchType::Cookie,
                        key: "env".to_string(),
                        value: "canary".to_string(),
                    }],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext {
            headers: HashMap::new(),
            cookies: HashMap::from([("env".to_string(), "canary".to_string())]),
            query_params: HashMap::new(),
            client_ip: None,
        };
        let result = router.match_route("/api/users", &ctx).unwrap();
        assert_eq!(result.route_id, "canary");

        let ctx = CanaryContext::empty();
        let result = router.match_route("/api/users", &ctx).unwrap();
        assert_eq!(result.route_id, "stable");
    }

    /// 灰度权重分流：80:20 比例统计
    #[test]
    fn test_canary_weight_split() {
        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: Some(CanaryConfig {
                    weight: 80,
                    match_rules: vec![],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 20,
                    match_rules: vec![],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext::empty();
        let mut stable_count = 0;
        let mut canary_count = 0;
        for i in 0..1000 {
            let result = router.match_route("/api/users", &ctx).unwrap();
            if result.route_id == "stable" {
                stable_count += 1;
            } else {
                canary_count += 1;
            }
            if i % 10 == 0 {
                std::thread::yield_now();
            }
        }

        assert!(stable_count > 400, "stable_count: {}", stable_count);
        assert!(canary_count > 50, "canary_count: {}", canary_count);
    }

    /// 灰度兜底：无 canary 配置的路由作为兜底
    #[test]
    fn test_canary_fallback() {
        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 0,
                    match_rules: vec![],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext::empty();
        let result = router.match_route("/api/users", &ctx).unwrap();
        assert_eq!(result.route_id, "stable");
    }

    /// 单条路由（无灰度）保持原有行为
    #[test]
    fn test_single_route_no_canary() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "single".to_string(),
                match_type: MatchType::Exact,
                path: "/api/test".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext::empty();
        let result = router.match_route("/api/test", &ctx).unwrap();
        assert_eq!(result.route_id, "single");
        assert_eq!(result.upstream, "svc");
    }

    /// IP Hash 确定性分流：同一 IP 多次请求始终命中同一版本
    #[test]
    fn test_ip_hash_deterministic() {
        use crate::config::StickyStrategy;

        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: Some(CanaryConfig {
                    weight: 80,
                    match_rules: vec![],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/users".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 20,
                    match_rules: vec![],
                    stick: Some(StickyStrategy::IpHash),
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext {
            headers: HashMap::new(),
            cookies: HashMap::new(),
            query_params: HashMap::new(),
            client_ip: Some("192.168.1.100".to_string()),
        };

        let mut results = std::collections::HashSet::new();
        for _ in 0..10 {
            let result = router.match_route("/api/users", &ctx).unwrap();
            results.insert(result.route_id.clone());
        }
        assert_eq!(results.len(), 1, "同一 IP 应始终命中同一版本");
    }

    /// Query Parameter 灰度匹配
    #[test]
    fn test_query_param_match() {
        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/products".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: None,
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/products".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 0,
                    match_rules: vec![CanaryMatchRule {
                        match_type: CanaryMatchType::Query,
                        key: "version".to_string(),
                        value: "canary".to_string(),
                    }],
                    stick: None,
                    sticky_cookie: None,
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        let ctx = CanaryContext {
            headers: HashMap::new(),
            cookies: HashMap::new(),
            query_params: HashMap::from([("version".to_string(), "canary".to_string())]),
            client_ip: None,
        };
        let result = router.match_route("/api/products", &ctx).unwrap();
        assert_eq!(result.route_id, "canary");

        let ctx = CanaryContext::empty();
        let result = router.match_route("/api/products", &ctx).unwrap();
        assert_eq!(result.route_id, "stable");
    }

    /// Cookie Sticky Session：按 Cookie 中的 route_id 匹配
    #[test]
    fn test_cookie_sticky_match() {
        use crate::config::StickyStrategy;

        let mut router = Router::new();

        router
            .add_route(RouteRule {
                route_id: "stable".to_string(),
                match_type: MatchType::Exact,
                path: "/api/orders".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v1".to_string(),
                canary: Some(CanaryConfig {
                    weight: 90,
                    match_rules: vec![],
                    stick: Some(StickyStrategy::Cookie),
                    sticky_cookie: Some(StickyCookieConfig {
                        name: "kirin_canary".to_string(),
                        path: "/".to_string(),
                        max_age: 3600,
                    }),
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        router
            .add_route(RouteRule {
                route_id: "canary".to_string(),
                match_type: MatchType::Exact,
                path: "/api/orders".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc-v2".to_string(),
                canary: Some(CanaryConfig {
                    weight: 10,
                    match_rules: vec![],
                    stick: Some(StickyStrategy::Cookie),
                    sticky_cookie: Some(StickyCookieConfig {
                        name: "kirin_canary".to_string(),
                        path: "/".to_string(),
                        max_age: 3600,
                    }),
                }),
                ip_whitelist: None,
                ip_blacklist: None,
            })
            .unwrap();

        // 带 sticky cookie 直接命中对应路由
        let ctx = CanaryContext {
            headers: HashMap::new(),
            cookies: HashMap::from([("kirin_canary".to_string(), "canary".to_string())]),
            query_params: HashMap::new(),
            client_ip: None,
        };
        let result = router.match_route("/api/orders", &ctx).unwrap();
        assert_eq!(result.route_id, "canary");

        // 不带 cookie 走权重分流
        let ctx = CanaryContext::empty();
        let result = router.match_route("/api/orders", &ctx).unwrap();
        assert!(result.route_id == "stable" || result.route_id == "canary");
    }
}
