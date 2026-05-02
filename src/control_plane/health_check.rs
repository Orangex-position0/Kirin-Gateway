#![allow(dead_code)]

use crate::config::HealthCheckConfig;
use crate::data_plane::upstream::LoadBalancerKind;
use pingora_core::services::ServiceWithDependents;
use pingora_core::services::background::GenBackgroundService;
use pingora_load_balancing::LoadBalancer;
use pingora_load_balancing::health_check::TcpHealthCheck;
use pingora_load_balancing::selection::{BackendIter, BackendSelection};
use std::time::Duration;
use tracing::info;

/// 为 LoadBalancer 配置 TCP 健康检查
///
/// - S: 不同的选择算法
pub fn setup_health_check<S: BackendSelection<Iter: BackendIter> + 'static>(
    lb: &mut LoadBalancer<S>,
    config: &HealthCheckConfig,
) {
    let mut hc = TcpHealthCheck::new();
    hc.peer_template.options.connection_timeout = Some(Duration::from_secs(config.timeout_secs));
    lb.set_health_check(hc);
    lb.health_check_frequency = Some(Duration::from_secs(config.interval_secs));
    info!(
        "健康检查配置完成: interval={}s, timeout={}s",
        config.interval_secs, config.timeout_secs
    );
}

///从 LoadBalancerKind 构建后端健康检查服务
pub fn create_background_service(
    name: &str,
    lb: &LoadBalancerKind,
) -> Option<Box<dyn ServiceWithDependents>> {
    match lb {
        LoadBalancerKind::RoundRobin(lb) => {
            let bg = GenBackgroundService::new(format!("HC {}", name), lb.clone());
            Some(Box::new(bg))
        },
        LoadBalancerKind::Consistent(lb) => {
            let bg = GenBackgroundService::new(format!("HC {}", name), lb.clone());
            Some(Box::new(bg))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HealthCheckConfig;
    use std::sync::Arc;

    /// 验证 setup_health_check 配置正确
    #[test]
    fn test_setup_health_check_round_robin() {
        use pingora_load_balancing::LoadBalancer;
        use pingora_load_balancing::selection::RoundRobin;

        let mut lb: LoadBalancer<RoundRobin> =
            LoadBalancer::try_from_iter(["127.0.0.1:80"]).unwrap();

        let config = HealthCheckConfig {
            interval_secs: 5,
            timeout_secs: 3,
        };

        setup_health_check(&mut lb, &config);
        assert!(lb.health_check_frequency.is_some());
        assert_eq!(lb.health_check_frequency.unwrap().as_secs(), 5);
    }

    /// 验证 create_background_service 对 RoundRobin 类型正常工作
    #[test]
    fn test_create_background_service_round_robin() {
        use crate::data_plane::upstream::LoadBalancerKind;
        use pingora_load_balancing::LoadBalancer;
        use pingora_load_balancing::selection::RoundRobin;

        let lb = Arc::new(LoadBalancer::<RoundRobin>::try_from_iter(["127.0.0.1:80"]).unwrap());
        let kind = LoadBalancerKind::RoundRobin(lb);

        let bg = create_background_service("test-service", &kind);
        assert!(bg.is_some());
    }

    /// 验证 create_background_service 对 Consistent 类型正常工作
    #[test]
    fn test_create_background_service_consistent() {
        use crate::data_plane::upstream::LoadBalancerKind;
        use pingora_load_balancing::LoadBalancer;
        use pingora_load_balancing::selection::Consistent;

        let lb = Arc::new(LoadBalancer::<Consistent>::try_from_iter(["127.0.0.1:80"]).unwrap());
        let kind = LoadBalancerKind::Consistent(lb);

        let bg = create_background_service("test-service", &kind);
        assert!(bg.is_some());
    }
}
