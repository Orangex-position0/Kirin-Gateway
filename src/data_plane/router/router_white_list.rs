#![allow(dead_code)]

use crate::data_plane::router::MatchType;
use regex::Regex;
use std::collections::HashMap;

/// 接口注册条目
pub struct RouteEntry {
    /// 接口唯一标识
    pub route_id: String,
    /// 路径模式
    pub path: String,
    /// 前缀（仅 prefix 类型）
    pub prefix: Option<String>,
    /// 匹配类型
    pub match_type: MatchType,
    /// 允许的 HTTP 方法
    pub methods: Vec<String>,
    /// 上游集群名称
    pub upstream: String,
    /// 申请人
    pub applicant: String,
    /// 申请时间
    pub applied_at: String,
    /// 接口场景说明
    pub description: String,
    /// 是否需要 JWT 认证
    pub is_auth: bool,
}

/// 接口注册表
///
/// 职责: 只负责白名单校验
pub struct RouteRegistry {
    /// 已注册的路由集合
    /// - key: 路由唯一标识 (route_id)
    /// - value: 路由注册条目
    registered_routes: HashMap<String, RouteEntry>,
}

impl RouteRegistry {
    pub fn new() -> Self {
        RouteRegistry {
            registered_routes: HashMap::new(),
        }
    }

    /// 注册接口条目
    pub fn register(&mut self, entry: RouteEntry) {
        self.registered_routes.insert(entry.route_id.clone(), entry);
    }

    /// 注销接口条目
    pub fn unregister(&mut self, route_id: &str) -> bool {
        self.registered_routes.remove(route_id).is_some()
    }

    /// 将请求路径解析为已注册的路由 ID
    ///
    /// 匹配优先级: 精确匹配 > 正则匹配 > 前缀匹配
    pub fn resolve_path(&self, path: &str) -> Option<String> {
        // exact match
        for entry in self.registered_routes.values() {
            if entry.match_type == MatchType::Exact && entry.path == path {
                return Some(entry.route_id.clone());
            }
        }

        // regex match
        for entry in self.registered_routes.values() {
            if entry.match_type == MatchType::Regex
                && let Ok(re) = Regex::new(&entry.path)
                && re.is_match(path)
            {
                return Some(entry.route_id.clone());
            }
        }

        // prefix match
        let mut best_match: Option<&RouteEntry> = None;
        let mut best_len = 0usize;
        for entry in self.registered_routes.values() {
            if entry.match_type == MatchType::Prefix {
                let prefix = entry.prefix.as_deref().unwrap_or(&entry.path);
                if path.starts_with(prefix) && prefix.len() > best_len {
                    best_len = prefix.len();
                    best_match = Some(entry);
                }
            }
        }

        best_match.map(|e| e.route_id.clone())
    }

    /// 获取所有已注册路由
    pub fn list_routes(&self) -> Vec<&RouteEntry> {
        self.registered_routes.values().collect()
    }

    /// 根据路由 ID 查找路由详情
    pub fn find_route(&self, route_id: &str) -> Option<&RouteEntry> {
        self.registered_routes.get(route_id)
    }
}
