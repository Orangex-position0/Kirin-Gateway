use crate::control_plane::admin_api::dto::UpstreamDTO;
use pingora_core::prelude::HttpPeer;
use pingora_load_balancing::{LoadBalancer, selection::RoundRobin};
use std::sync::Arc;

/// 上游集群：封装负载均衡器与节点地址列表
pub struct UpstreamCluster {
    // 集群名称
    pub name: String,
    // Pingora 轮询负载均衡器
    pub load_balancer: Arc<LoadBalancer<RoundRobin>>,
    // 后端节点地址列表（LoadBalancer 不暴露内部节点，额外保存供 Admin API 查询）
    addrs: Vec<String>,
}

impl UpstreamCluster {
    /// 创建集群实例
    pub fn new(name: &str, addrs: Vec<&str>) -> Self {
        let owned_addrs: Vec<String> = addrs.iter().map(|s| s.to_string()).collect();
        let lb = LoadBalancer::try_from_iter(addrs).unwrap();
        Self {
            name: name.to_string(),
            load_balancer: Arc::new(lb),
            addrs: owned_addrs,
        }
    }

    /// 负载均衡选择一个上游节点
    pub fn select_peer(&self) -> Option<Box<HttpPeer>> {
        let upstream = self.load_balancer.select(b"", 256)?;
        Some(Box::new(HttpPeer::new(upstream, false, String::new())))
    }

    /// 获取集群摘要信息（用于 Admin API）
    pub fn summary(&self) -> UpstreamDTO {
        UpstreamDTO {
            name: self.name.clone(),
            nodes: self.addrs.clone(),
        }
    }
}
