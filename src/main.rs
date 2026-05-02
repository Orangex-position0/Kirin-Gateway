mod config;
mod control_plane;
mod data_plane;
mod observability;

use crate::control_plane::control_plane::ControlPlane;
use crate::control_plane::gateway_state::GatewayState;
use crate::control_plane::health_check;
use data_plane::proxy::KirinProxy;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .init();

    // 初始化 Prometheus 指标注册
    crate::observability::metrics::init();

    // 加载配置文件
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.yaml".to_string());

    let config = config::load_config(&config_path).unwrap_or_else(|e| {
        eprintln!("Failed to load config '{}': {}", config_path, e);
        std::process::exit(1);
    });

    info!("Config loaded from: {}", config_path);

    // 从配置构建 GatewayState（包含路由表、集群注册表、限流器、FilterChain）
    let initial_state = GatewayState::from_config(&config).unwrap_or_else(|e| {
        eprintln!("配置校验失败: {}", e);
        std::process::exit(1);
    });

    let shared_state = Arc::new(RwLock::new(initial_state));

    let control_plane = Arc::new(ControlPlane::new(shared_state.clone(), config_path));

    control_plane.start_file_watcher(Duration::from_millis(500));

    let proxy = KirinProxy::new(shared_state.clone());

    // 启动 Pingora Server
    let mut server = pingora_core::server::Server::new(None).unwrap();
    server.bootstrap();

    let mut lb_service = pingora_proxy::http_proxy_service(&server.configuration, proxy);
    lb_service.add_tcp(&config.server.listen);
    server.add_service(lb_service);

    info!("Kirin Gateway Data Plane started {}", config.server.listen);

    // 注册 Admin API Service
    if let Some(ref admin_cfg) = config.admin {
        let admin_proxy = control_plane::admin_api::AdminProxy {
            state: shared_state.clone(),
            control_plane: control_plane.clone(),
        };
        let mut admin_service =
            pingora_proxy::http_proxy_service(&server.configuration, admin_proxy);
        admin_service.add_tcp(&admin_cfg.listen);
        server.add_service(admin_service);

        info!("Admin API started {}", admin_cfg.listen);
    }

    // 注册健康检查后台服务
    {
        let state = shared_state.read().unwrap();
        for cluster in state.clusters.values() {
            if let Some(bg) = health_check::create_background_service(&cluster.name, &cluster.lb) {
                server.add_boxed_service(bg);
            }
        }
    }

    server.run_forever();
}
