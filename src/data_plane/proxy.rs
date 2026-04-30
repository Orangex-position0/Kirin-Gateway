use std::sync::{Arc, RwLock};

use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{FilterContext, FilterResult};
use async_trait::async_trait;
use log::{info, warn};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::{Error, Result};
use pingora_proxy::{ProxyHttp, Session};

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
        RequestContext { filter_ctx: None }
    }

    /// 执行 FilterChain 请求阶段
    ///
    /// Pingora 调用顺序：request_filter → upstream_peer → upstream_request_filter
    /// 在此阶段初始化 FilterContext 并执行限流、鉴权等 Filter。
    /// 白名单校验在 upstream_peer 中执行（需要先路由匹配）。
    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        // 从 session 中提取请求信息，初始化 FilterContext
        let path = session
            .req_header()
            .uri
            .path_and_query()
            .map(|p| p.path())
            .unwrap_or("/")
            .to_string();

        let method = session.req_header().method.as_str().to_string();

        let client_ip = session
            .client_addr()
            .and_then(|addr| addr.as_inet())
            .map(|addr| addr.ip().to_string())
            .unwrap_or_default();

        ctx.filter_ctx = Some(FilterContext {
            path,
            method,
            client_ip,
            upstream_name: None,
            route_id: None,
            start_time: std::time::Instant::now(),
            rate_limit_remaining: None,
            auth_user_id: None,
        });

        let filter_ctx = ctx.filter_ctx.as_mut().unwrap();

        // Clone FilterChain 后释放读锁，避免 RwLockReadGuard 跨 .await
        let filter_chain = {
            let state = self.state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });
            state.filter_chain().clone()
        };

        let result = filter_chain
            .run_request_filters(filter_ctx, session.req_header_mut(), &self.state)
            .await;

        match result {
            FilterResult::Continue => Ok(false),
            FilterResult::Stop(reject) => {
                let _ = session.respond_error(reject.status_code()).await;
                Ok(true)
            },
        }
    }

    /// 路由匹配 + 白名单校验 + 选择上游节点
    ///
    /// 路由匹配成功后才进行白名单校验（确认路径在注册表中），
    /// 未匹配路由的请求由 Pingora 返回 502。
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

        // 路由匹配 + 白名单校验 + 选节点
        let (cluster, upstream_name) = {
            let state = self.state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });

            let route_match = state
                .router
                .match_route(path)
                .ok_or_else(|| Error::new_str("404 No route matched"))?;

            // 白名单校验：确认路由在注册表中
            let route_id = state
                .registry
                .resolve_path(path)
                .ok_or_else(|| Error::new_str("403 Route not registered in whitelist"))?;

            let cluster = state
                .get_cluster(&route_match.upstream)
                .ok_or_else(|| Error::new_str("上游集群未注册"))?;

            if let Some(fctx) = &mut ctx.filter_ctx {
                fctx.route_id = Some(route_id);
            }

            (cluster, route_match.upstream)
        };

        if let Some(fctx) = &mut ctx.filter_ctx {
            fctx.upstream_name = Some(upstream_name.clone());
        }

        info!("路由匹配成功: {} {} → {}", method, path, upstream_name);

        let key = ctx
            .filter_ctx
            .as_ref()
            .map(|f| f.client_ip.as_bytes())
            .unwrap_or(b"");
        cluster
            .select_peer(key)
            .ok_or_else(|| Error::new_str("无可用上游节点"))
    }

    /// 修改发往上游的请求头
    ///
    /// Pingora 文档明确说明：只有 upstream_request_filter 中修改的请求头
    /// 才会传递给上游服务，request_filter 中的修改不会传递。
    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut pingora_http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // 注入网关标识头，供上游识别请求来源
        upstream_request
            .insert_header("X-Gateway", "Kirin Gateway")
            .unwrap();
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
    async fn logging(&self, _session: &mut Session, _e: Option<&Error>, _ctx: &mut Self::CTX)
    where
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
