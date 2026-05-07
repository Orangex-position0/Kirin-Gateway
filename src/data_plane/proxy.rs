use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::config::{CanaryConfig, StickyCookieConfig, StickyStrategy};
use crate::control_plane::gateway_state::GatewayState;
use crate::data_plane::filter::{FilterContext, FilterResult};
use crate::data_plane::router::CanaryContext;
use async_trait::async_trait;
use ipnet::IpNet;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::{Error, Result};
use pingora_proxy::{ProxyHttp, Session};
use tracing::{info, warn};
// Prometheus exposition format 标准协议版本
//const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

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

/// 灰度路由决策结果
pub struct CanaryDecision {
    pub route_id: String,
    pub sticky_cookie: Option<StickyCookieConfig>,
    pub is_new_assignment: bool,
}

/// 每次请求的上下文
pub struct RequestContext {
    pub filter_ctx: Option<FilterContext>,
    pub canary_decision: Option<CanaryDecision>,
}

#[async_trait]
impl ProxyHttp for KirinProxy {
    type CTX = RequestContext;

    /// 创建本次请求的上下文实例
    fn new_ctx(&self) -> Self::CTX {
        RequestContext {
            filter_ctx: None,
            canary_decision: None,
        }
    }

    /// 路由匹配 + 白名单校验 + IP 过滤 + 选择上游节点
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

        let (cluster, upstream_name) = {
            let state = self.state.read().unwrap_or_else(|e| {
                warn!("Gateway state lock poisoned, recovering");
                e.into_inner()
            });

            let canary_ctx = build_canary_context(session);

            let route_match = state
                .match_route(path, &canary_ctx)
                .ok_or_else(|| Error::new_str("404 No route matched"))?;

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

            // IP 黑白名单校验
            if let Some(route_info) = state.find_route_info_by_id(&route_match.route_id) {
                let client_ip = canary_ctx.client_ip.as_deref().unwrap_or("");
                if !is_ip_allowed(
                    client_ip,
                    &route_info.ip_whitelist,
                    &route_info.ip_blacklist,
                ) {
                    return Err(Error::new_str("403 IP not allowed"));
                }
            }

            // 设置灰度决策（用于 response_filter 注入 Set-Cookie 和 logging 记录指标）
            if state.find_route_info_by_id(&route_match.route_id).is_some() {
                // 查找完整 canary 配置
                let canary_config = find_canary_config(&state, &route_match.route_id);
                if let Some(canary) = canary_config {
                    let sticky_cookie = if matches!(canary.stick, Some(StickyStrategy::Cookie)) {
                        canary.sticky_cookie.clone().or_else(|| {
                            Some(StickyCookieConfig {
                                name: "kirin_canary".to_string(),
                                path: "/".to_string(),
                                max_age: 3600,
                            })
                        })
                    } else {
                        None
                    };
                    let is_new =
                        !is_cookie_sticky_match(&canary, &canary_ctx, &route_match.route_id);
                    ctx.canary_decision = Some(CanaryDecision {
                        route_id: route_match.route_id.clone(),
                        sticky_cookie,
                        is_new_assignment: is_new,
                    });
                }
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

        if path == "/metrics" {
            let body = crate::observability::metrics::collect();
            let resp = pingora_http::ResponseHeader::build(200, None)?;
            // resp.insert_header("Content-Type", PROMETHEUS_CONTENT_TYPE)?;
            session.write_response_header(Box::new(resp), false).await?;
            session.write_response_body(Some(body.into()), true).await?;
            return Ok(true);
        }

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

    /// 执行 FilterChain 响应阶段 + Cookie Sticky 注入
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

        // Cookie Sticky：新分配时注入 Set-Cookie
        if let Some(decision) = &_ctx.canary_decision
            && decision.is_new_assignment
            && let Some(cookie_cfg) = &decision.sticky_cookie
        {
            let value = format!(
                "{}={}; Path={}; Max-Age={}",
                cookie_cfg.name, decision.route_id, cookie_cfg.path, cookie_cfg.max_age
            );
            let _ = _upstream_response.insert_header("Set-Cookie", &value);
        }

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

    /// 最终日志：记录请求处理耗时、目标上游及灰度指标
    async fn logging(&self, _session: &mut Session, _e: Option<&Error>, _ctx: &mut Self::CTX)
    where
        Self::CTX: Send + Sync,
    {
        if let Some(ref fctx) = _ctx.filter_ctx {
            let elapsed_ms = fctx.start_time.elapsed().as_millis() as u64;
            let upstream = fctx.upstream_name.as_deref().unwrap_or("unknown");

            crate::observability::metrics::REQUESTS_TOTAL
                .with_label_values(&[&fctx.method, upstream, "200"])
                .inc();

            crate::observability::metrics::REQUEST_DURATION
                .with_label_values(&[&fctx.method, upstream])
                .observe(elapsed_ms as f64);

            if _e.is_some() {
                crate::observability::metrics::UPSTREAM_ERRORS_TOTAL
                    .with_label_values(&[&fctx.method, upstream])
                    .inc();
            }

            // 灰度指标
            if let Some(decision) = &_ctx.canary_decision {
                let status = if _e.is_some() { "500" } else { "200" };
                crate::observability::metrics::CANARY_REQUESTS_TOTAL
                    .with_label_values(&[&decision.route_id, &fctx.method, status])
                    .inc();
                crate::observability::metrics::CANARY_REQUEST_DURATION
                    .with_label_values(&[&decision.route_id, &fctx.method])
                    .observe(elapsed_ms as f64);
            }

            info!(
                method = %fctx.method,
                path = %fctx.path,
                upstream = fctx.upstream_name.as_deref().unwrap_or("unknown"),
                elapsed_ms = elapsed_ms,
                client_ip = %fctx.client_ip,
                "请求处理完成"
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

/// 从 Session 中提取 CanaryContext
fn build_canary_context(session: &Session) -> CanaryContext {
    let mut headers = HashMap::new();
    let mut cookies = HashMap::new();
    let mut query_params = HashMap::new();

    let req_header = session.req_header();
    let header_map = &req_header.headers;
    for (name, value) in header_map.iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.to_string(), v.to_string());
        }
    }

    // 解析 Cookie header
    if let Some(cookie_str) = header_map.get("cookie")
        && let Ok(v) = cookie_str.to_str()
    {
        for pair in v.split(";") {
            let pair = pair.trim();
            if let Some((key, value)) = pair.split_once("=") {
                cookies.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }

    // 解析 Query String
    if let Some(pq) = req_header.uri.path_and_query()
        && let Some(query) = pq.query()
    {
        for pair in query.split("&") {
            if let Some((k, v)) = pair.split_once("=") {
                query_params.insert(k.to_string(), v.to_string());
            }
        }
    }

    let client_ip = session
        .client_addr()
        .and_then(|addr| addr.as_inet())
        .map(|addr| addr.ip().to_string());

    CanaryContext {
        headers,
        cookies,
        query_params,
        client_ip,
    }
}

/// 检查客户端 IP 是否被允许
fn is_ip_allowed(
    client_ip: &str,
    whitelist: &Option<Vec<IpNet>>,
    blacklist: &Option<Vec<IpNet>>,
) -> bool {
    use std::net::IpAddr;

    let ip: IpAddr = match client_ip.parse() {
        Ok(ip) => ip,
        Err(_) => return true,
    };

    if let Some(bl) = blacklist
        && bl.iter().any(|net| net.contains(&ip))
    {
        return false;
    }

    if let Some(wl) = whitelist {
        return wl.iter().any(|net| net.contains(&ip));
    }

    true
}

/// 判断是否通过 Cookie Sticky 命中
fn is_cookie_sticky_match(
    canary: &CanaryConfig,
    ctx: &CanaryContext,
    matched_route_id: &str,
) -> bool {
    if !matches!(canary.stick, Some(StickyStrategy::Cookie)) {
        return false;
    }
    let cookie_name = canary
        .sticky_cookie
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "kirin_canary".to_string());
    ctx.cookies
        .get(&cookie_name)
        .map(|v| v == matched_route_id)
        .unwrap_or(false)
}

/// 通过 GatewayState 查找某路由的 CanaryConfig
fn find_canary_config(state: &GatewayState, route_id: &str) -> Option<CanaryConfig> {
    state.find_canary_config_by_route_id(route_id)
}
