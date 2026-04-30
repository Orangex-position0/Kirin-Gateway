use crate::config::{HealthCheckConfig, NodeConfig, UpstreamConfig};
use crate::control_plane::admin_api::dto::UpstreamDTO;
use futures_util::FutureExt;
use pingora_core::prelude::HttpPeer;
use pingora_load_balancing::discovery::Static;
use pingora_load_balancing::{Backend, Backends, LoadBalancer, selection};
use std::collections::BTreeSet;
use std::sync::Arc;

/// 上游集群：封装负载均衡器与节点地址列表
pub struct UpstreamCluster {
    // 集群名称
    pub name: String,
    // 负载均衡器
    pub lb: LoadBalancerKind,
    // 后端节点地址列表（LoadBalancer 不暴露内部节点，额外保存供 Admin API 查询）
    addrs: Vec<String>,
    // 是否配置了健康检查
    #[allow(dead_code)]
    health_check_enabled: bool,
}

impl UpstreamCluster {
    /// 从配置创建集群实例
    pub fn from_config(name: &str, upstream_cfg: &UpstreamConfig) -> Result<Self, String> {
        let backends = build_backends(&upstream_cfg.nodes);
        let addrs: Vec<String> = upstream_cfg.nodes.iter().map(|n| n.addr.clone()).collect();

        let lb = match upstream_cfg.algorithm.as_str() {
            "round_robin" | "" => LoadBalancerKind::RoundRobin(build_round_robin_lb(
                backends?,
                upstream_cfg.health_check.as_ref(),
            )),
            "consistent_hash" => LoadBalancerKind::Consistent(build_consistent_lb(
                backends?,
                upstream_cfg.health_check.as_ref(),
            )),
            other => {
                return Err(format!(
                    "不支持的负载均衡算法 '{}'，可选: round_robin, consistent_hash",
                    other
                ));
            },
        };

        Ok(Self {
            name: name.to_string(),
            lb,
            addrs,
            health_check_enabled: upstream_cfg.health_check.is_some(),
        })
    }

    /// 负载均衡选择一个健康的上游节点
    pub fn select_peer(&self, key: &[u8]) -> Option<Box<HttpPeer>> {
        let backend = self.lb.select(key, 256)?;
        Some(Box::new(HttpPeer::new(backend, false, String::new())))
    }

    /// 获取集群摘要信息（用于 Admin API）
    pub fn summary(&self) -> UpstreamDTO {
        UpstreamDTO {
            name: self.name.clone(),
            nodes: self.addrs.clone(),
        }
    }

    /// 是否配置了健康检查
    #[allow(dead_code)]
    pub fn has_health_check(&self) -> bool {
        self.health_check_enabled
    }
}

/// 根据配置创建 Backend 集合
fn build_backends(nodes: &[NodeConfig]) -> Result<BTreeSet<Backend>, String> {
    let mut backends = BTreeSet::new();
    for node in nodes {
        let backend = Backend::new_with_weight(&node.addr, node.weight)
            .map_err(|e| format!("节点地址 '{}' 无效: '{}'", node.addr, e))?;
        backends.insert(backend);
    }
    Ok(backends)
}

/// 构建 RoundRobin 负载均衡器
fn build_round_robin_lb(
    backends: BTreeSet<Backend>,
    health_check_config: Option<&HealthCheckConfig>,
) -> Arc<LoadBalancer<selection::RoundRobin>> {
    let discovery = Static::new(backends);
    let backends_obj = Backends::new(discovery);
    let mut lb = LoadBalancer::<selection::RoundRobin>::from_backends(backends_obj);

    // 初始化（必需，否则 selector 为空）
    lb.update()
        .now_or_never()
        .expect("static discovery 不应阻塞")
        .expect("static discovery 不应失败");

    // 配置健康检查（阶段 2 启用）
    if let Some(hc_cfg) = health_check_config {
        crate::control_plane::health_check::setup_health_check(&mut lb, hc_cfg);
    }

    Arc::new(lb)
}

/// 构建 ConsistentHash 负载均衡器
fn build_consistent_lb(
    backends: BTreeSet<Backend>,
    health_check_config: Option<&HealthCheckConfig>,
) -> Arc<LoadBalancer<selection::Consistent>> {
    let discovery = Static::new(backends);
    let backends_obj = Backends::new(discovery);
    let mut lb = LoadBalancer::<selection::Consistent>::from_backends(backends_obj);

    lb.update()
        .now_or_never()
        .expect("static discovery 不应阻塞")
        .expect("static discovery 不应失败");

    if let Some(hc_cfg) = health_check_config {
        crate::control_plane::health_check::setup_health_check(&mut lb, hc_cfg);
    }

    Arc::new(lb)
}

/// 负载均衡策略枚举
pub enum LoadBalancerKind {
    RoundRobin(Arc<LoadBalancer<selection::RoundRobin>>),
    Consistent(Arc<LoadBalancer<selection::Consistent>>),
}

impl LoadBalancerKind {
    /// 选择一个健康的后端节点
    pub fn select(&self, key: &[u8], max_iterations: usize) -> Option<Backend> {
        match self {
            LoadBalancerKind::RoundRobin(lb) => lb.select(key, max_iterations),
            LoadBalancerKind::Consistent(lb) => lb.select(key, max_iterations),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NodeConfig, UpstreamConfig};

    /// 加权轮询：权重高的节点被选中的概率更大
    #[test]
    fn test_weighted_round_robin() {
        let upstream_cfg = UpstreamConfig {
            nodes: vec![
                NodeConfig {
                    addr: "127.0.0.1:9001".to_string(),
                    weight: 3,
                },
                NodeConfig {
                    addr: "127.0.0.1:9002".to_string(),
                    weight: 1,
                },
            ],
            algorithm: "round_robin".to_string(),
            health_check: None,
        };

        let cluster = UpstreamCluster::from_config("test", &upstream_cfg).unwrap();

        let mut count_9001 = 0;
        let mut count_9002 = 0;
        for _ in 0..40 {
            let peer = cluster.select_peer(b"").unwrap();
            let addr = peer._address.to_string();
            if addr.contains("9001") {
                count_9001 += 1;
            }
            if addr.contains("9002") {
                count_9002 += 1;
            }
        }

        // 权重 3:1，期望约 30:10，允许一定波动
        assert!(count_9001 > count_9002, "权重 3:1 时 9001 应被选中更多次");
    }

    /// 一致性哈希：相同 key 总是选中相同节点
    #[test]
    fn test_consistent_hash_sticky() {
        let upstream_cfg = UpstreamConfig {
            nodes: vec![
                NodeConfig {
                    addr: "127.0.0.1:9001".to_string(),
                    weight: 1,
                },
                NodeConfig {
                    addr: "127.0.0.1:9002".to_string(),
                    weight: 1,
                },
                NodeConfig {
                    addr: "127.0.0.1:9003".to_string(),
                    weight: 1,
                },
            ],
            algorithm: "consistent_hash".to_string(),
            health_check: None,
        };

        let cluster = UpstreamCluster::from_config("test", &upstream_cfg).unwrap();

        let peer1 = cluster.select_peer(b"192.168.1.1").unwrap();
        let peer2 = cluster.select_peer(b"192.168.1.1").unwrap();
        assert_eq!(peer1._address, peer2._address, "相同 key 应选中相同节点");

        let _peer3 = cluster.select_peer(b"10.0.0.1").unwrap();
        // 不同 key 可能选中不同节点（但不一定，取决于哈希分布）
    }

    /// 不支持的算法返回错误
    #[test]
    fn test_unsupported_algorithm() {
        let upstream_cfg = UpstreamConfig {
            nodes: vec![NodeConfig {
                addr: "127.0.0.1:9001".to_string(),
                weight: 1,
            }],
            algorithm: "least_conn".to_string(),
            health_check: None,
        };

        let result = UpstreamCluster::from_config("test", &upstream_cfg);
        assert!(result.is_err());
    }
}
