#![allow(dead_code)]

use crate::config::{AuthConfig, KirinConfig, RouteConfig, UpstreamConfig};
use crate::control_plane::admin_api::dto::RateLimitDTO;
use crate::data_plane::filter::FilterChain;
use crate::data_plane::filter::auth::AuthFilter;
use crate::data_plane::rate_limit::RateLimiter;
use crate::data_plane::router::router_white_list::{RouteEntry, RouteRegistry};
use crate::data_plane::router::{MatchType, RouteMatch, RouteRule, Router};
use crate::data_plane::upstream::UpstreamCluster;
use log::info;
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::sync::{Arc, RwLock};

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
    // 当前生效的配置快照 (用于增量 diff)
    config_snapshot: KirinConfig,
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
    AuthConfigFailed { reason: String },
    /// 上游集群构建失败
    UpstreamBuildFailed { name: String, reason: String },
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::UnknownUpstream {
                path,
                upstream,
                available,
            } => {
                write!(
                    f,
                    "路由 '{}' 引用了不存在的上游 '{}'，可用上游: {:?}",
                    path, upstream, available
                )
            },
            StateError::InvalidMatchType { route_id, value } => {
                write!(
                    f,
                    "路由 '{}' 的 match_type '{}' 无效，可选: exact, prefix, regex",
                    route_id, value
                )
            },
            StateError::RouteBuildFailed { route_id, reason } => {
                write!(f, "路由 '{}' 构建失败: {}", route_id, reason)
            },
            StateError::AuthConfigFailed { reason } => {
                write!(f, "认证配置加载失败: {}", reason)
            },
            StateError::UpstreamBuildFailed { name, reason } => {
                write!(f, "上游集群 '{}' 构建失败: {}", name, reason)
            },
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
            let _addrs: Vec<&str> = upstream_cfg.nodes.iter().map(|n| n.addr.as_str()).collect();
            let cluster = Arc::new(UpstreamCluster::from_config(name, upstream_cfg).map_err(
                |e| StateError::RouteBuildFailed {
                    route_id: name.clone(),
                    reason: e,
                },
            )?);
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
                },
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
                },
            };

            let rule = RouteRule {
                route_id: route_cfg.route_id.clone(),
                match_type: match_type.clone(),
                path: path.clone(),
                prefix: route_cfg.path_prefix.clone(),
                methods: route_cfg.methods.clone(),
                upstream: route_cfg.upstream.clone(),
            };

            router
                .add_route(rule)
                .map_err(|e| StateError::RouteBuildFailed {
                    route_id: route_cfg.route_id.clone(),
                    reason: e.to_string(),
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
            Some(raw) => match raw.clone().into_auth_config() {
                Ok(ac) => Some(ac),
                Err(e) => {
                    return Err(StateError::AuthConfigFailed {
                        reason: e.to_string(),
                    });
                },
            },
        };

        Ok(GatewayState {
            router,
            registry,
            clusters,
            filter_chain: Self::build_default_filter_chain(),
            rate_limiter,
            auth_config,
            config_snapshot: config.clone(),
        })
    }

    /// 构建默认 Filter 链
    ///
    /// 顺序：Method -> Auth -> RateLimit -> Header -> Logging
    /// 注意：WhiteList 已移至 upstream_peer 中（路由匹配成功后执行），
    /// 因为 request_filter 在 upstream_peer 之前执行，此时还没有路由匹配结果。
    fn build_default_filter_chain() -> FilterChain {
        use crate::data_plane::filter::header::HeaderFilter;
        use crate::data_plane::filter::logging::LoggingFilter;
        use crate::data_plane::filter::method::MethodFilter;
        use crate::data_plane::filter::rate_limit_filter::RateLimitFilter;

        let mut chain = FilterChain::new();
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

    /// 增量更新: 对比新旧配置，只重建变更部分
    pub fn diff_update(&mut self, new_config: &KirinConfig) -> Result<(), StateError> {
        let old_config = self.config_snapshot.clone();

        // 1. Diff Route
        self.diff_routes(&old_config, new_config)?;

        // 2. Diff UpstreamCluster
        self.diff_clusters(&old_config, new_config)?;

        // 3. Diff RateLimiter
        self.diff_rate_limiter(&old_config, new_config);

        // 4. Diff Auth
        self.diff_auth(&old_config, new_config)?;

        // 5. update config_snapshot
        self.config_snapshot = new_config.clone();

        Ok(())
    }

    /// Diff Routes
    fn diff_routes(
        &mut self,
        old_config: &KirinConfig,
        new_config: &KirinConfig,
    ) -> Result<(), StateError> {
        let old_ids: HashSet<&str> = old_config
            .routes
            .iter()
            .map(|r| r.route_id.as_str())
            .collect();
        let new_ids: HashSet<&str> = new_config
            .routes
            .iter()
            .map(|r| r.route_id.as_str())
            .collect();

        for id in old_ids.difference(&new_ids) {
            self.router.remove_route(id);
            self.registry.unregister(id);
            info!("路由已删除: {}", id);
        }

        for route_cfg in &new_config.routes {
            let is_new = !old_ids.contains(route_cfg.route_id.as_str());
            let is_changed = !is_new && {
                let old_route = old_config
                    .routes
                    .iter()
                    .find(|r| r.route_id == route_cfg.route_id)
                    .unwrap();
                routes_differ(old_route, route_cfg)
            };

            if is_new || is_changed {
                if is_changed {
                    self.router.remove_route(&route_cfg.route_id);
                    self.registry.unregister(&route_cfg.route_id);
                }
                self.add_route_from_config(route_cfg)?;
                info!(
                    "路由已{}: {}",
                    if is_new { "新增" } else { "更新" },
                    route_cfg.route_id
                );
            }
        }

        Ok(())
    }

    /// 上游集群 Diff
    fn diff_clusters(
        &mut self,
        old_config: &KirinConfig,
        new_config: &KirinConfig,
    ) -> Result<(), StateError> {
        let old_names: HashSet<&str> = old_config.upstreams.keys().map(|s| s.as_str()).collect();
        let new_names: HashSet<&str> = new_config.upstreams.keys().map(|s| s.as_str()).collect();

        // 删除：旧有新无
        for name in old_names.difference(&new_names) {
            self.clusters.remove(*name);
            info!("上游集群已删除: {}", name);
        }

        // 新增/变更
        for (name, new_upstream) in &new_config.upstreams {
            let is_new = !old_names.contains(name.as_str());
            let is_changed = !is_new && {
                let old_upstream = old_config.upstreams.get(name).unwrap();
                upstreams_differ(old_upstream, new_upstream)
            };

            if is_new || is_changed {
                let cluster = Arc::new(UpstreamCluster::from_config(name, new_upstream).map_err(
                    |e| StateError::UpstreamBuildFailed {
                        name: name.clone(),
                        reason: e,
                    },
                )?);
                self.clusters.insert(name.clone(), cluster);
                info!(
                    "上游集群已{}: {}",
                    if is_new { "新增" } else { "更新" },
                    name
                );
            }
            // 未变更的集群保持不动（保留健康检查状态）
        }

        Ok(())
    }

    /// 限流器 Diff
    fn diff_rate_limiter(&mut self, old_config: &KirinConfig, new_config: &KirinConfig) {
        match (&old_config.rate_limit, &new_config.rate_limit) {
            // 无→有：创建新限流器
            (None, Some(new_rl)) => {
                self.rate_limiter = Some(Arc::new(RateLimiter::new(
                    new_rl.capacity,
                    new_rl.refill_rate,
                )));
                info!("限流器已启用");
            },
            // 有→无：移除限流器
            (Some(_), None) => {
                self.rate_limiter = None;
                info!("限流器已禁用");
            },
            // 参数变更：保留令牌桶状态，仅更新策略
            (Some(old_rl), Some(new_rl))
                if old_rl.capacity != new_rl.capacity
                    || old_rl.refill_rate != new_rl.refill_rate =>
            {
                if let Some(ref rl) = self.rate_limiter {
                    rl.update_policy(new_rl.capacity, new_rl.refill_rate);
                }
                info!("限流策略已更新");
            },
            // 参数未变更
            _ => {},
        }
    }

    /// 认证 Diff
    fn diff_auth(
        &mut self,
        old_config: &KirinConfig,
        new_config: &KirinConfig,
    ) -> Result<(), StateError> {
        let auth_changed = match (&old_config.auth, &new_config.auth) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(old_auth), Some(new_auth)) => {
                old_auth.algorithm != new_auth.algorithm
                    || old_auth.public_key_path != new_auth.public_key_path
                    || old_auth.issuer != new_auth.issuer
                    || old_auth.claims_to_forward != new_auth.claims_to_forward
            },
        };

        if auth_changed {
            self.auth_config = match &new_config.auth {
                None => None,
                Some(raw) => Some(raw.clone().into_auth_config().map_err(|e| {
                    StateError::AuthConfigFailed {
                        reason: e.to_string(),
                    }
                })?),
            };
            info!("认证配置已更新");
        }

        Ok(())
    }

    /// 从配置添加单条路由（供 diff_routes 复用）
    fn add_route_from_config(&mut self, route_cfg: &RouteConfig) -> Result<(), StateError> {
        // 与 from_config 中构建路由的逻辑一致
        let match_type = match route_cfg.match_type.as_str() {
            "exact" => MatchType::Exact,
            "prefix" => MatchType::Prefix,
            "regex" => MatchType::Regex,
            other => {
                return Err(StateError::InvalidMatchType {
                    route_id: route_cfg.route_id.clone(),
                    value: other.to_string(),
                });
            },
        };

        let path = match (&match_type, &route_cfg.path, &route_cfg.path_prefix) {
            (MatchType::Exact | MatchType::Regex, Some(p), _) => p.clone(),
            (MatchType::Prefix, _, Some(p)) => p.clone(),
            _ => {
                return Err(StateError::RouteBuildFailed {
                    route_id: route_cfg.route_id.clone(),
                    reason: format!("match_type 为 {:?}，但缺少对应的路径配置", match_type),
                });
            },
        };

        let rule = RouteRule {
            route_id: route_cfg.route_id.clone(),
            match_type: match_type.clone(),
            path: path.clone(),
            prefix: route_cfg.path_prefix.clone(),
            methods: route_cfg.methods.clone(),
            upstream: route_cfg.upstream.clone(),
        };

        self.router
            .add_route(rule)
            .map_err(|e| StateError::RouteBuildFailed {
                route_id: route_cfg.route_id.clone(),
                reason: e.to_string(),
            })?;

        self.registry.register(RouteEntry {
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

        Ok(())
    }
}

/// 判断两条路由配置是否不同
fn routes_differ(a: &RouteConfig, b: &RouteConfig) -> bool {
    a.path != b.path
        || a.path_prefix != b.path_prefix
        || a.match_type != b.match_type
        || a.methods != b.methods
        || a.upstream != b.upstream
        || a.is_auth != b.is_auth
}

/// 判断两个上游配置是否不同
fn upstreams_differ(a: &UpstreamConfig, b: &UpstreamConfig) -> bool {
    a.algorithm != b.algorithm
        || a.nodes.len() != b.nodes.len()
        || a.nodes
            .iter()
            .zip(b.nodes.iter())
            .any(|(na, nb)| na.addr != nb.addr || na.weight != nb.weight)
        || a.health_check != b.health_check
}

/// 共享状态 handle
pub type SharedState = Arc<RwLock<GatewayState>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        KirinConfig, NodeConfig, RateLimitConfig, RouteConfig, ServerConfig, UpstreamConfig,
    };

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
                        algorithm: "round_robin".to_string(),
                        health_check: None,
                    },
                ),
                (
                    "default-service".to_string(),
                    UpstreamConfig {
                        nodes: vec![NodeConfig {
                            addr: "127.0.0.1:9002".to_string(),
                            weight: 1,
                        }],
                        algorithm: "round_robin".to_string(),
                        health_check: None,
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
            Err(StateError::UnknownUpstream {
                path,
                upstream,
                available,
            }) => {
                assert_eq!(path, "/api/orders");
                assert_eq!(upstream, "nonexistent-service");
                assert!(available.contains(&"user-service".to_string()));
            },
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
        assert_eq!(
            state.match_route("/api/users").unwrap().upstream,
            "user-service"
        );
        assert_eq!(
            state.match_route("/api/orders").unwrap().upstream,
            "default-service"
        );
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
        assert_eq!(
            state.registry.resolve_path("/api/users"),
            Some("test-user-route".to_string())
        );

        // FilterChain 已自动构建（5 个内置 Filter，WhiteList 已移至 upstream_peer）
        assert_eq!(state.filter_chain().len(), 5);
    }

    fn make_base_config() -> KirinConfig {
        KirinConfig {
            server: ServerConfig {
                listen: "0.0.0.0:8080".to_string(),
                threads: Some(1),
            },
            routes: vec![RouteConfig {
                route_id: "route-a".to_string(),
                path: Some("/a".to_string()),
                path_prefix: None,
                match_type: "exact".to_string(),
                methods: vec![],
                upstream: "svc-a".to_string(),
                applicant: "test".to_string(),
                applied_at: "2026-04-20T00:00:00+08:00".to_string(),
                description: "route a".to_string(),
                is_auth: false,
            }],
            upstreams: [(
                "svc-a".to_string(),
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
            rate_limit: Some(RateLimitConfig {
                capacity: 100,
                refill_rate: 10,
            }),
            admin: None,
            auth: None,
        }
    }

    /// 增量新增路由
    #[test]
    fn test_diff_add_route() {
        let old_config = make_base_config();
        let mut state = GatewayState::from_config(&old_config).unwrap();
        assert_eq!(state.registry.list_routes().len(), 1);

        let mut new_config = old_config.clone();
        new_config.routes.push(RouteConfig {
            route_id: "route-b".to_string(),
            path: Some("/b".to_string()),
            path_prefix: None,
            match_type: "exact".to_string(),
            methods: vec![],
            upstream: "svc-a".to_string(),
            applicant: "test".to_string(),
            applied_at: "2026-04-20T00:00:00+08:00".to_string(),
            description: "route b".to_string(),
            is_auth: false,
        });

        state.diff_update(&new_config).unwrap();
        assert_eq!(state.registry.list_routes().len(), 2);
        assert!(state.router.match_route("/b").is_some());
        // 原有路由仍在
        assert!(state.router.match_route("/a").is_some());
    }

    /// 增量删除路由
    #[test]
    fn test_diff_remove_route() {
        let old_config = make_base_config();
        let mut state = GatewayState::from_config(&old_config).unwrap();

        let mut new_config = old_config.clone();
        new_config.routes.clear();

        state.diff_update(&new_config).unwrap();
        assert_eq!(state.registry.list_routes().len(), 0);
        assert!(state.router.match_route("/a").is_none());
    }

    /// 未变更的上游集群保持原实例（健康检查状态保留）
    #[test]
    fn test_diff_unchanged_cluster_preserved() {
        let old_config = make_base_config();
        let mut state = GatewayState::from_config(&old_config).unwrap();

        let old_cluster_ptr = Arc::as_ptr(state.clusters.get("svc-a").unwrap());

        // 相同配置 diff，集群不应重建
        let new_config = old_config.clone();
        state.diff_update(&new_config).unwrap();

        let new_cluster_ptr = Arc::as_ptr(state.clusters.get("svc-a").unwrap());
        assert_eq!(old_cluster_ptr, new_cluster_ptr, "未变更的集群应保持原实例");
    }

    /// 限流参数变更时保留令牌桶状态
    #[test]
    fn test_diff_rate_limit_preserves_buckets() {
        let old_config = make_base_config();
        let mut state = GatewayState::from_config(&old_config).unwrap();

        // 消耗一些令牌
        let (allowed, _) = state.rate_limiter.as_ref().unwrap().check("192.168.1.1");
        assert!(allowed);

        let old_limiter_ptr = Arc::as_ptr(state.rate_limiter.as_ref().unwrap());

        // 变更限流参数
        let mut new_config = old_config.clone();
        new_config.rate_limit = Some(RateLimitConfig {
            capacity: 200,
            refill_rate: 20,
        });

        state.diff_update(&new_config).unwrap();

        let new_limiter_ptr = Arc::as_ptr(state.rate_limiter.as_ref().unwrap());
        assert_eq!(old_limiter_ptr, new_limiter_ptr, "限流器应保持原实例");

        // 验证策略已更新
        let summary = state.rate_limit_summary().unwrap();
        assert_eq!(summary.capacity, Some(200));
    }

    /// Router.remove_route 正确移除精确匹配路由
    #[test]
    fn test_router_remove_exact_route() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "test".to_string(),
                match_type: MatchType::Exact,
                path: "/api/test".to_string(),
                prefix: None,
                methods: vec![],
                upstream: "svc".to_string(),
            })
            .unwrap();

        assert!(router.match_route("/api/test").is_some());
        assert!(router.remove_route("test"));
        assert!(router.match_route("/api/test").is_none());
    }

    /// Router.remove_route 正确移除前缀匹配路由
    #[test]
    fn test_router_remove_prefix_route() {
        let mut router = Router::new();
        router
            .add_route(RouteRule {
                route_id: "test".to_string(),
                match_type: MatchType::Prefix,
                path: String::new(),
                prefix: Some("/api/".to_string()),
                methods: vec![],
                upstream: "svc".to_string(),
            })
            .unwrap();

        assert!(router.match_route("/api/anything").is_some());
        assert!(router.remove_route("test"));
        assert!(router.match_route("/api/anything").is_none());
    }

    /// Router.remove_route 不存在的路由返回 false
    #[test]
    fn test_router_remove_nonexistent() {
        let mut router = Router::new();
        assert!(!router.remove_route("not-exist"));
    }
}
