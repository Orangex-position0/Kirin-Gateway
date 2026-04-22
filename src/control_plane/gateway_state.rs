use std::collections::HashMap;
use std::fmt::Formatter;
use std::sync::{Arc, RwLock};
use crate::config::{AuthConfig, KirinConfig};
use crate::data_plane::rate_limit::RateLimiter;
use crate::data_plane::upstream::UpstreamCluster;
use crate::data_plane::router::{Router, RouteRule, MatchType, RouteMatch};
use crate::control_plane::admin_api::dto::RateLimitDTO;
use crate::data_plane::filter::auth::AuthFilter;
use crate::data_plane::filter::FilterChain;
use crate::data_plane::router::router_white_list::{RouteEntry, RouteRegistry};

/// 网关运行时共享状态
///
/// 持有控制面运行所需的所有可变状态
pub struct GatewayState {
    // 路由匹配器，根据请求路径匹配对应的上游集群
    pub router: Router,
    // 接口注册表（白名单），所有请求必须先通过白名单校验
    pub registry: RouteRegistry,
    // 上游集群注册表，key 为集群名称，value 为集群实例
    pub clusters: HashMap<String, Arc<UpstreamCluster>>,
    // 过滤器链，按责任链模式对请求/响应进行拦截处理
    pub filter_chain: FilterChain,
    // 令牌桶限流器，未配置时为 None
    pub rate_limiter: Option<Arc<RateLimiter>>,
    // JWT 认证配置
    pub auth_config: Option<AuthConfig>,
}

/// 从配置中构建 GatewayState 时可能出现的错误
#[derive(Debug)]
pub enum StateError {
    /// 路由引用了不存在的上游集群
    UnknownUpstream {
        // 路径
        path: String,
        // 引用的上游集群名称
        upstream: String,
        // 可用上游集群名称
        available: Vec<String>,
    },
    /// 匹配类型无效
    InvalidMatchType { route_id: String, value: String },
    /// 路由构建失败（如正则非法、route_id 重复）
    RouteBuildFailed { route_id: String, reason: String },
    /// 认证配置加载失败
    AuthConfigFailed {
        reason: String,
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::UnknownUpstream { path, upstream, available } => {
                write!(f, "路由 '{}' 引用了不存在的上游 '{}'，可用上游: {:?}", path, upstream, available)
            }
            StateError::InvalidMatchType { route_id, value } => {
                write!(f, "路由 '{}' 的 match_type '{}' 无效，可选: exact, prefix, regex", route_id, value)
            }
            StateError::RouteBuildFailed { route_id, reason } => {
                write!(f, "路由 '{}' 构建失败: {}", route_id, reason)
            }
            StateError::AuthConfigFailed { reason } => {
                write!(f, "认证配置加载失败: {}", reason)
            }
        }
    }
}

impl std::error::Error for StateError {}

impl GatewayState {
    /// 从配置中构建共享状态 GatewayState
    pub fn from_config(config: &KirinConfig) -> Result<Self, StateError> {
        let available_upstreams: Vec<String> = config.upstreams.keys().cloned().collect();

        // 校验上游引用: 每条路由引用的上游必须存在
        for route_cfg in &config.routes {
            if !config.upstreams.contains_key(&route_cfg.upstream) {
                return Err(StateError::UnknownUpstream {
                    path: route_cfg.path.clone().unwrap_or_default(),
                    upstream: route_cfg.upstream.clone(),
                    available: available_upstreams.clone(),
                });
            }
        }

        // 构建集群注册表
        let mut clusters: HashMap<String, Arc<UpstreamCluster>> = HashMap::new();
        for (name, upstream_cfg) in &config.upstreams {
            let addrs: Vec<&str> = upstream_cfg.nodes.iter().map(|n| n.addr.as_str()).collect();
            let cluster = Arc::new(UpstreamCluster::new(name, addrs));
            clusters.insert(name.clone(), cluster);
        }

        // 1. 构建路由匹配器与接口注册表
        let mut router = Router::new();
        let mut registry = RouteRegistry::new();

        for route_cfg in &config.routes {
            let match_type = match route_cfg.match_type.as_str() {
                "exact" => MatchType::Exact,
                "prefix" => MatchType::Prefix,
                "regex" => MatchType::Regex,
                other => {
                    return Err(StateError::InvalidMatchType {
                        route_id: route_cfg.route_id.clone(),
                        value: other.to_string(),
                    });
                }
            };

            // 根据匹配类型确定路径：Exact/Regex 使用 path，Prefix 使用 path_prefix
            let path = match (&match_type, &route_cfg.path, &route_cfg.path_prefix) {
                (MatchType::Exact | MatchType::Regex, Some(p), _) => p.clone(),
                (MatchType::Prefix, _, Some(p)) => p.clone(),
                _ => {
                    return Err(StateError::RouteBuildFailed {
                        route_id: route_cfg.route_id.clone(),
                        reason: format!(
                            "match_type 为 {:?}，但缺少对应的路径配置（path 或 path_prefix）",
                            match_type
                        ),
                    });
                }
            };

            let rule = RouteRule {
                route_id: route_cfg.route_id.clone(),
                match_type: match_type.clone(),
                path: path.clone(),
                prefix: route_cfg.path_prefix.clone(),
                methods: route_cfg.methods.clone(),
                upstream: route_cfg.upstream.clone(),
            };

            router.add_route(rule).map_err(|e| {
                StateError::RouteBuildFailed {
                    route_id: route_cfg.route_id.clone(),
                    reason: e.to_string(),
                }
            })?;

            registry.register(RouteEntry {
                route_id: route_cfg.route_id.clone(),
                path: path.clone(),
                prefix: route_cfg.path_prefix.clone(),
                match_type,
                methods: route_cfg.methods.clone(),
                upstream: route_cfg.upstream.clone(),
                applicant: route_cfg.applicant.clone(),
                applied_at: route_cfg.applied_at.clone(),
                description: route_cfg.description.clone(),
                is_auth: route_cfg.is_auth,
            });
        }

        // 2. 构建限流器
        let rate_limiter = config
            .rate_limit
            .as_ref()
            .map(|rl| Arc::new(RateLimiter::new(rl.capacity, rl.refill_rate)));

        
        // 3. 构建认证器
        let auth_config = match &config.auth {
            None => None,
            Some(raw) => {
                match raw.clone().into_auth_config() {
                    Ok(ac) => Some(ac),
                    Err(e) => {
                        return Err(StateError::AuthConfigFailed {
                            reason: e.to_string(),
                        })
                    }
                }
            }
        };

        Ok(GatewayState {
            router,
            registry,
            clusters,
            filter_chain: Self::build_default_filter_chain(),
            rate_limiter,
            auth_config,
        })
    }

    /// 构建默认 Filter 链
    ///
    /// 顺序：WhiteList -> Method -> Auth -> RateLimit -> Header -> Logging
    fn build_default_filter_chain() -> FilterChain {
        use crate::data_plane::filter::whitelist::WhiteListFilter;
        use crate::data_plane::filter::method::MethodFilter;
        use crate::data_plane::filter::rate_limit_filter::RateLimitFilter;
        use crate::data_plane::filter::header::HeaderFilter;
        use crate::data_plane::filter::logging::LoggingFilter;

        let mut chain = FilterChain::new();
        chain.add_filter(Arc::new(WhiteListFilter));
        chain.add_filter(Arc::new(MethodFilter));
        chain.add_filter(Arc::new(AuthFilter));
        chain.add_filter(Arc::new(RateLimitFilter));
        chain.add_filter(Arc::new(HeaderFilter));
        chain.add_filter(Arc::new(LoggingFilter));
        chain
    }

    // ---- 读取方法 ----

    /// 路由匹配：根据请求路径查找对应的路由信息
    pub fn match_route(&self, path: &str) -> Option<RouteMatch> {
        self.router.match_route(path)
    }

    /// 获取上游集群
    pub fn get_cluster(&self, name: &str) -> Option<Arc<UpstreamCluster>> {
        self.clusters.get(name).cloned()
    }

    /// 获取 FilterChain 引用
    pub fn filter_chain(&self) -> &FilterChain {
        &self.filter_chain
    }

    /// 获取限流器引用
    pub fn rate_limiter(&self) -> Option<&Arc<RateLimiter>> {
        self.rate_limiter.as_ref()
    }
    
    /// 更新限流策略参数（保留现有令牌桶状态）
    pub fn update_rate_limit_policy(&self, capacity: usize, refill_rate: usize) {
        if let Some(ref limiter) = self.rate_limiter {
            limiter.update_policy(capacity, refill_rate);
        }
    }
    
    /// 获取限流配置摘要（用于 Admin API）
    pub fn rate_limit_summary(&self) -> Option<RateLimitDTO> {
        self.rate_limiter.as_ref().map(|limiter| limiter.summary())
    }

}

/// 共享状态 handle
pub type SharedState = Arc<RwLock<GatewayState>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KirinConfig, ServerConfig, RouteConfig, UpstreamConfig, NodeConfig};

    /// 构建测试用最小合法配置
    fn make_config() -> KirinConfig {
        KirinConfig {
            server: ServerConfig {
                listen: "0.0.0.0:8080".to_string(),
                threads: Some(1),
            },
            routes: vec![
                RouteConfig {
                    route_id: "test-user-route".to_string(),
                    path: Some("/api/users".to_string()),
                    path_prefix: None,
                    match_type: "exact".to_string(),
                    methods: vec![],
                    upstream: "user-service".to_string(),
                    applicant: "test".to_string(),
                    applied_at: "2026-04-20T00:00:00+08:00".to_string(),
                    description: "test route".to_string(),
                    is_auth: false,
                },
                RouteConfig {
                    route_id: "test-default-route".to_string(),
                    path: None,
                    path_prefix: Some("/api/".to_string()),
                    match_type: "prefix".to_string(),
                    methods: vec![],
                    upstream: "default-service".to_string(),
                    applicant: "test".to_string(),
                    applied_at: "2026-04-20T00:00:00+08:00".to_string(),
                    description: "test default route".to_string(),
                    is_auth: false,
                },
            ],
            upstreams: [
                (
                    "user-service".to_string(),
                    UpstreamConfig {
                        nodes: vec![NodeConfig {
                            addr: "127.0.0.1:9001".to_string(),
                            weight: 1,
                        }],
                    },
                ),
                (
                    "default-service".to_string(),
                    UpstreamConfig {
                        nodes: vec![NodeConfig {
                            addr: "127.0.0.1:9002".to_string(),
                            weight: 1,
                        }],
                    },
                ),
            ]
            .into_iter()
            .collect(),
            rate_limit: None,
            admin: None,
            auth: None,
        }
    }

    /// 路由引用不存在上游时返回 StateError::UnknownUpstream
    #[test]
    fn test_from_config_unknown_upstream() {
        let mut config = make_config();
        config.routes.push(RouteConfig {
            route_id: "test-bad-upstream".to_string(),
            path: Some("/api/orders".to_string()),
            path_prefix: None,
            match_type: "exact".to_string(),
            methods: vec![],
            upstream: "nonexistent-service".to_string(),
            applicant: "test".to_string(),
            applied_at: "2026-04-20T00:00:00+08:00".to_string(),
            description: "test bad upstream".to_string(),
            is_auth: false,
        });

        let result = GatewayState::from_config(&config);
        match result {
            Err(StateError::UnknownUpstream { path, upstream, available }) => {
                assert_eq!(path, "/api/orders");
                assert_eq!(upstream, "nonexistent-service");
                assert!(available.contains(&"user-service".to_string()));
            }
            Ok(_) => panic!("期望返回 StateError::UnknownUpstream，但构建成功"),
            Err(other) => panic!("期望 StateError::UnknownUpstream，但得到: {:?}", other),
        }
    }

    /// 所有路由引用合法上游时正确构建 GatewayState
    #[test]
    fn test_from_config_success() {
        let config = make_config();
        let state = GatewayState::from_config(&config).unwrap();

        // 路由表正确
        assert_eq!(state.match_route("/api/users").unwrap().upstream, "user-service");
        assert_eq!(state.match_route("/api/orders").unwrap().upstream, "default-service");
        assert!(state.match_route("/health").is_none());

        // 集群注册表正确
        assert!(state.get_cluster("user-service").is_some());
        assert!(state.get_cluster("default-service").is_some());
        assert!(state.get_cluster("nonexistent").is_none());

        // 限流器未配置
        assert!(state.rate_limiter.is_none());

        // 接口注册表正确
        assert_eq!(state.registry.list_routes().len(), 2);
        assert!(state.registry.find_route("test-user-route").is_some());
        assert_eq!(state.registry.resolve_path("/api/users"), Some("test-user-route".to_string()));

        // FilterChain 已自动构建（6 个内置 Filter）
        assert_eq!(state.filter_chain().len(), 6);
    }
}