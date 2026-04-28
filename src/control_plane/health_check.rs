#![allow(dead_code)]

use pingora_load_balancing::LoadBalancer;
use pingora_load_balancing::health_check;
use pingora_load_balancing::prelude::RoundRobin;
use std::time::Duration;

pub struct HealthCheckConfig {
    // 检查间隔 (秒)
    pub interval_secs: u64,
    // 连接超时阈值 (秒)
    pub timeout_secs: u64,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        HealthCheckConfig {
            interval_secs: 5,
            timeout_secs: 3,
        }
    }
}

// TCP health-check for LoadBalancer
pub fn setup_health_check(lb: &mut LoadBalancer<RoundRobin>, config: &HealthCheckConfig) {
    let mut hc = health_check::TcpHealthCheck::new();
    hc.peer_template.options.connection_timeout = Some(Duration::from_secs(config.timeout_secs));
    lb.set_health_check(hc);
    lb.health_check_frequency = Some(Duration::from_secs(config.interval_secs));
}
