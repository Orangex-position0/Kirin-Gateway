use async_trait::async_trait;
use log::info;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_core::Result;

/// 中间件模块
/// 定义中间件 trait 和内置中间件实现

#[async_trait]
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str;

    // Filter/modify request headers before forwarding to upstream
    async fn request_filter(
        &self,
        _request: &mut RequestHeader,
    ) -> Result<()> {
        Ok(())
    }

    // Filter/modify response headers after receiving from upstream, before returning to client
    async fn response_filter(
        &self,
        _response: &mut ResponseHeader,
    ) -> Result<()> {
        Ok(())
    }
}

pub struct HeaderMiddleware;

#[async_trait]
impl Middleware for HeaderMiddleware {
    fn name(&self) -> &str {
        "header-middleware"
    }

    async fn request_filter(&self, request: &mut RequestHeader) -> Result<()> {
        request.insert_header("X-Gateway", "Kirin Gateway").unwrap();
        Ok(())
    }

    async fn response_filter(&self, response: &mut ResponseHeader) -> Result<()> {
        response.insert_header("X-Powered-By", "Kirin Gateway").unwrap();
        Ok(())
    }
}

pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    fn name(&self) -> &str {
        "logging-middleware"
    }

    async fn request_filter(&self, request: &mut RequestHeader) -> Result<()> {
        let method = request.method.as_str();
        let path = request.uri.path_and_query().map(|pg| pg.as_str()).unwrap_or("/");
        info!("[LoggingMiddleware] {} {}", method, path);
        Ok(())
    }

    async fn response_filter(&self, response: &mut ResponseHeader) -> Result<()> {
        info!("[LoggingMiddleware] Response status: {}", response.status.as_u16());
        Ok(())
    }
}