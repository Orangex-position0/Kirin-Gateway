use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use log::{info, warn};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::{Error, Result};
use pingora_proxy::{ProxyHttp, Session};
use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{FilterContext, FilterResult};

/// 网关代理服务（纯数据面）
///
/// 不直接持有任何可变状态，所有运行时数据通过共享状态层读取。
/// 控制面通过写锁更新配置，数据面通过读锁获取最新配置。
pub struct KirinProxy {
    pub state: Arc<RwLock<GatewayState>>,
}

impl KirinProxy {
    pub fn new(state: Arc<RwLock<GatewayState>>) -> Self {
        KirinProxy { state }
    }
}

/// 每次请求的上下文
pub struct RequestContext {
    // 过滤链上下文
    pub filter_ctx: Option<FilterContext>,
}

#[async_trait]
impl ProxyHttp for KirinProxy {
    type CTX = RequestContext;

    /// 创建本次请求的上下文实例
    fn new_ctx(&self) -> Self::CTX {
        RequestContext {
            filter_ctx: None,
        }
    }

    /// 路由匹配 + 选择上游节点
    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let path = session
            .req_header()
            .uri
            .path_and_query()
            .map(|p| p.path())
            .unwrap_or("/");

        let method = session.req_header().method.as_str();

        // 初始化 FilterContex
        let client_ip = session
            .client_addr()
            .and_then(|addr| addr.as_inet())
            .map(|addr| addr.ip().to_string())
            .unwrap_or_default();

        ctx.filter_ctx = Some(FilterContext {
            path: path.to_string(),
            method: method.to_string(),
            client_ip,
            upstream_name: None,
            route_id: None,
            start_time: std::time::Instant::now(),
            rate_limit_remaining: None,
            auth_user_id: None,
        });

        // 路由匹配 + 选节点
        let (cluster, upstream_name) = {
            let state = self.state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });

            let route_match = state
                .router
                .match_route(path)
                .ok_or_else(|| Error::new_str("404 No route matched"))?;

            let cluster = state
                .get_cluster(&route_match.upstream)
                .ok_or_else(|| Error::new_str("上游集群未注册"))?;

            (cluster, route_match.upstream)
        };

        if let Some(fctx) = &mut ctx.filter_ctx {
            fctx.upstream_name = Some(upstream_name.clone());
        }

        info!(
            "路由匹配成功: {} {} → {}",
            method, path, upstream_name
        );

        cluster
            .select_peer()
            .ok_or_else(|| Error::new_str("无可用上游节点"))
    }

    /// 执行 FilterChain 请求阶段
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<bool> {
        let filter_ctx = match &mut ctx.filter_ctx {
            Some(fctx) => fctx,
            None => return Ok(false),
        };

        // Clone FilterChain 后释放读锁，避免 RwLockReadGuard 跨 .await
        let filter_chain = {
            let state = self.state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });
            state.filter_chain().clone()
        };

        let result = filter_chain
            .run_request_filters(
                filter_ctx,
                session.req_header_mut(),
                &self.state,
            )
            .await;

        match result {
            FilterResult::Continue => Ok(false),
            FilterResult::Stop(reject) => {
                let _ = session.respond_error(reject.status_code()).await;
                Ok(true)
            }
        }
    }

    /// 已废弃（Header 注入已移入 FilterChain）
    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        _upstream_request: &mut pingora_http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        Ok(())
    }

    /// 执行 FilterChain 响应阶段
    async fn response_filter(
        &self,
        _session: &mut Session,
        _upstream_response: &mut pingora_http::ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        let filter_ctx = match &mut _ctx.filter_ctx {
            Some(fctx) => fctx,
            None => return Ok(()),
        };

        // Clone FilterChain 后释放读锁
        let filter_chain = {
            let state = self.state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });
            state.filter_chain().clone()
        };

        filter_chain
            .run_response_filters(filter_ctx, _upstream_response)
            .await;

        Ok(())
    }

    /// 最终日志：记录请求处理耗时与目标上游
    async fn logging(
        &self,
        _session: &mut Session,
        _e: Option<&Error>,
        _ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        if let Some(ref fctx) = _ctx.filter_ctx {
            let elapsed = fctx.start_time.elapsed();
            info!(
                "请求处理完成，上游: {}，耗时: {:?}",
                fctx.upstream_name.as_deref().unwrap_or("unknown"),
                elapsed
            );
        }
    }

    /// 连接失败时标记为可重试
    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        _ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        let mut err = e;
        err.set_retry(true);
        err
    }
}
