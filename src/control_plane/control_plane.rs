#![allow(dead_code)]

use crate::config;
use crate::config::parse_config;
use crate::control_plane::gateway_state::GatewayState;
use log::{error, info};
use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::Duration;

/// 控制面模块
/// 负载配置加载、校验、状态构建和配置热重载
pub struct ControlPlane {
    /// 共享可变状态
    state: Arc<RwLock<GatewayState>>,
    config_path: String,
}

impl ControlPlane {
    pub fn new(state: Arc<RwLock<GatewayState>>, config_path: String) -> Self {
        ControlPlane { state, config_path }
    }

    /// 加载配置文件并应用到共享状态
    pub fn load_and_apply(&self) -> Result<(), String> {
        info!("开始加载配置文件: {}", self.config_path);

        let new_config = config::load_config(&self.config_path)
            .map_err(|e| format!("加载配置文件失败: ‘{}’： {}", self.config_path, e))?;

        let new_state = GatewayState::from_config(&new_config)
            .map_err(|e| format!("配置文件校验失败: {}", e))?;

        let old_rate_limiter = {
            let state = self
                .state
                .read()
                .map_err(|e| format!("获取读锁失败: {}", e))?;
            state.rate_limiter().cloned()
        };

        let mut state = self
            .state
            .write()
            .map_err(|e| format!("获取写锁失败: {}", e))?;

        if let (Some(_new_rl), Some(_old_rl)) =
            (new_state.rate_limiter(), old_rate_limiter.as_ref())
        {
            let _summary = _new_rl.summary();
            let mut state = self
                .state
                .write()
                .map_err(|e| format!("获取写锁失败: {}", e))?;
            let mut rebuilt = GatewayState::from_config(&new_config)
                .map_err(|e| format!("重建状态失败: {}", e))?;
            if let Some(ref mut _rl) = rebuilt.rate_limiter {}
            *state = rebuilt;
        } else {
            *state = new_state;
        }

        info!("配置热重载完成: {}", self.config_path);
        Ok(())
    }

    /// 热重载 (不保留限流桶状态)
    pub fn reload_simple(&self) -> Result<(), String> {
        info!("开始热重载配置: {}", self.config_path);

        let content =
            std::fs::read_to_string(&self.config_path).map_err(|e| format!("加载失败: {}", e))?;

        let new_config = parse_config(&content).map_err(|e| format!("配置文件解析失败: {}", e))?;
        let new_state = GatewayState::from_config(&new_config)
            .map_err(|e| format!("配置文件校验失败: {}", e))?;

        let mut state = self.state.write().map_err(|e| format!("写锁失败: {}", e))?;
        *state = new_state;

        info!("配置热重载完成");
        Ok(())
    }

    /// 获取配置文件路径
    pub fn config_path(&self) -> &str {
        &self.config_path
    }

    /// 启动配置文件监听 (后台线程)
    pub fn start_file_watcher(&self, debounce_interval: Duration) {
        let config_path = self.config_path.clone();
        let state = self.state.clone();

        thread::spawn(move || {
            let (tx, rx) = mpsc::channel();

            let config_dir = PathBuf::from(&config_path)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));

            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    error!("创建文件监听器失败: {}", e);
                    return;
                },
            };

            if let Err(e) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
                error!("启动文件监听失败: {}", e);
                return;
            }

            info!("开始监听配置文件变化: {}", config_path);

            // 事件循环：去抖动处理
            while rx.recv().is_ok() {
                // 去抖动：等待一段时间，如果没有新事件才执行重载
                loop {
                    thread::sleep(debounce_interval);
                    match rx.try_recv() {
                        Ok(_) => continue,                       // 还有新事件，继续等
                        Err(mpsc::TryRecvError::Empty) => break, // 没有新事件了
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }

                // 执行热重载
                let cp = ControlPlane::new(state.clone(), config_path.clone());
                match cp.reload_simple() {
                    Ok(()) => info!("热重载成功"),
                    Err(e) => error!("热重载失败，保持旧配置: {}", e),
                }
            }
        });
    }
}
