#![allow(dead_code)]

use jsonwebtoken::DecodingKey;
use serde::{Deserialize, Serialize};
use std::fs;

/// 网关全局配置
///
/// 对应 config.yaml 的顶层结构，包含服务监听、路由规则、上游服务及限流策略
#[derive(Debug, Deserialize, Clone)]
pub struct KirinConfig {
    /// 服务监听配置
    pub server: ServerConfig,
    /// 路由规则列表，定义请求路径到上游服务的映射关系
    pub routes: Vec<RouteConfig>,
    /// 上游服务配置映射，key 为上游服务名称, value 为上游服务配置
    pub upstreams: std::collections::HashMap<String, UpstreamConfig>,
    /// 令牌桶限流配置，可选，未配置则不启用限流
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    /// Admin API 配置，可选，未配置则不启用管理接口
    #[serde(default)]
    pub admin: Option<AdminConfig>,
    /// JWT 认证配置，可选，未配置则不启用 JWT 认证
    #[serde(default)]
    pub auth: Option<AuthConfigRaw>,
}

/// Admin API 配置
#[derive(Debug, Deserialize, Clone)]
pub struct AdminConfig {
    /// Admin API 监听地址，如 "127.0.0.1:9090"
    pub listen: String,
}

/// 服务监听配置
///
/// 定义网关自身的监听地址和工作线程数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 监听地址，格式为 "ip:port"，如 "0.0.0.0:8080"
    pub listen: String,
    /// 工作线程数，可选，未设置时由运行时自动决定
    pub threads: Option<usize>,
    /// TLS 配置，可选，未配置则使用 TLS
    pub tls: Option<TlsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// 证书文件路径
    pub cert_path: String,
    /// 私钥文件路径
    pub key_path: String,
}

/// 路由配置默认匹配类型
fn default_match_type() -> String {
    "exact".to_string()
}

/// 路由规则配置
///
/// 支持精确路径匹配和路径前缀匹配两种模式，将请求转发到指定的上游服务
#[derive(Debug, Deserialize, Clone)]
pub struct RouteConfig {
    /// 接口唯一标识，用于接口注册和管理
    pub route_id: String,
    /// 精确路径（match_type 为 exact 或 regex 时使用）
    pub path: Option<String>,
    /// 前缀路径（match_type 为 prefix 时使用）
    pub path_prefix: Option<String>,
    /// 匹配类型：exact / prefix / regex，默认 exact
    #[serde(default = "default_match_type")]
    pub match_type: String,
    /// 允许的 HTTP 方法列表，为空或未配置则放行所有方法
    #[serde(default)]
    pub methods: Vec<String>,
    /// 上游服务名称
    pub upstream: String,
    /// 申请人
    pub applicant: String,
    /// 申请时间（ISO 8601 格式）
    pub applied_at: String,
    /// 接口场景说明
    pub description: String,
    /// 是否需要 JWT 认证，默认 false
    #[serde(default)]
    pub is_auth: bool,
    /// 灰度发布配置, 可选
    #[serde(default)]
    pub canary: Option<CanaryConfig>,
    /// IP 白名单（CIDR 或精确 IP），仅允许列表中的 IP 访问
    #[serde(default)]
    pub ip_whitelist: Option<Vec<String>>,
    /// IP 黑名单（CIDR 或精确 IP），拒绝列表中的 IP 访问
    #[serde(default)]
    pub ip_blacklist: Option<Vec<String>>,
}

/// 灰度发布配置
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CanaryConfig {
    /// 流量权重百分比 (0-100)
    pub weight: u8,
    /// 条件匹配规则列表
    #[serde(default)]
    pub match_rules: Vec<CanaryMatchRule>,
    /// 会话一致性策略
    #[serde(default)]
    pub stick: Option<StickyStrategy>,
    /// Cookie Sticky Session 配置
    pub sticky_cookie: Option<StickyCookieConfig>,
}

/// 灰度匹配规则类型
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CanaryMatchType {
    Header,
    Cookie,
    Query,
}

/// 单挑灰度匹配规则
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CanaryMatchRule {
    /// 匹配类型：header / cookie
    #[serde(rename = "type")]
    pub match_type: CanaryMatchType,
    /// Header 或 Cookie 的 key
    pub key: String,
    /// 期望的值
    pub value: String,
}

/// 会话一致性策略
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StickyStrategy {
    IpHash,
    Cookie,
}

/// Cookie Sticky Session 配置
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct StickyCookieConfig {
    /// Cookie 名称，默认 "kirin_canary"
    #[serde(default = "default_cookie_name")]
    pub name: String,
    /// Cookie Path 属性，默认 "/"
    #[serde(default = "default_cookie_path")]
    pub path: String,
    /// Cookie 过期时间（秒），默认 3600
    #[serde(default = "default_max_age")]
    pub max_age: u64,
}

fn default_cookie_name() -> String {
    "kirin_canary".to_string()
}
fn default_cookie_path() -> String {
    "/".to_string()
}
fn default_max_age() -> u64 {
    3600
}

/// 上游服务配置
///
/// 定义一组可用的后端服务节点，网关将在此组内进行负载均衡
#[derive(Debug, Deserialize, Clone)]
pub struct UpstreamConfig {
    /// 后端节点列表
    pub nodes: Vec<NodeConfig>,
    /// 负载均衡算法，默认为 round_robin
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    /// 健康检查配置
    #[serde(default)]
    pub health_check: Option<HealthCheckConfig>,
}

fn default_algorithm() -> String {
    "round_robin".to_string()
}

/// 后端节点配置
///
/// 描述单个上游服务实例的地址和负载均衡权重
#[derive(Debug, Deserialize, Clone)]
pub struct NodeConfig {
    /// 节点地址，格式为 "ip:port"
    pub addr: String,
    /// 负载均衡权重，默认值为 1，权重越高分配到的请求越多
    #[serde(default = "default_weight")]
    pub weight: usize,
}

/// weight 字段的默认值函数，serde 反序列化时调用
fn default_weight() -> usize {
    1
}

/// 令牌桶限流配置
///
/// 基于令牌桶算法控制请求速率，防止上游服务过载
#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    /// 令牌桶容量，即允许的最大突发请求数
    pub capacity: usize,
    /// 令牌填充速率，每秒补充的令牌数
    pub refill_rate: usize,
}

/// JWT 认证配置
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// RSA 公钥解码器（启动时从 PEM 文件加载，避免每次请求读文件）
    pub decoding_key: DecodingKey,
    /// 期望的 Token 签发者
    pub issuer: String,
    /// 需要透传给上游服务的 JWT claims
    pub claims_to_forward: Vec<String>,
}

/// auth 配置的反序列化中间结构
#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfigRaw {
    /// 签名算法，当前仅支持 RS256
    pub algorithm: String,
    /// RSA 公钥文件路径（PEM 格式）
    pub public_key_path: String,
    /// 期望的 Token 签发者
    pub issuer: String,
    /// 需要透传给上游的 JWT claims
    #[serde(default)]
    pub claims_to_forward: Vec<String>,
}

impl AuthConfigRaw {
    /// 从文件加载公钥并转为 AutoConfig
    pub fn into_auth_config(self) -> Result<AuthConfig, Box<dyn std::error::Error>> {
        let pem_content = fs::read_to_string(&self.public_key_path)?;
        let decoding_key = DecodingKey::from_rsa_pem(pem_content.as_bytes())?;

        Ok(AuthConfig {
            decoding_key,
            issuer: self.issuer,
            claims_to_forward: self.claims_to_forward,
        })
    }
}

/// Health check 配置
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct HealthCheckConfig {
    // 检查间隔 (秒)
    pub interval_secs: u64,
    // 连接超时阈值 (秒)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    3
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        HealthCheckConfig {
            interval_secs: 5,
            timeout_secs: default_timeout_secs(),
        }
    }
}
