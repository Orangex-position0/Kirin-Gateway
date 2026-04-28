use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use std::fs;

/// Configuration for the gateway

/// 网关全局配置
///
/// 对应 config.yaml 的顶层结构，包含服务监听、路由规则、上游服务及限流策略
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// 监听地址，格式为 "ip:port"，如 "0.0.0.0:8080"
    pub listen: String,
    /// 工作线程数，可选，未设置时由运行时自动决定
    pub threads: Option<usize>,
}

/// 路由配置默认匹配类型
fn default_match_type() -> String {
    "exact".to_string()
}

/// 路由规则配置
///
/// 支持精确路径匹配和路径前缀匹配两种模式，将请求转发到指定的上游服务
#[derive(Debug, Deserialize)]
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
}

/// 上游服务配置
///
/// 定义一组可用的后端服务节点，网关将在此组内进行负载均衡
#[derive(Debug, Deserialize)]
pub struct UpstreamConfig {
    /// 后端节点列表
    pub nodes: Vec<NodeConfig>,
}

/// 后端节点配置
///
/// 描述单个上游服务实例的地址和负载均衡权重
#[derive(Debug, Deserialize)]
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

/// 从 YAML 文件加载网关配置
///
/// 读取指定路径的配置文件并反序列化为 KirinConfig
pub fn load_config(path: &str) -> Result<KirinConfig, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    parse_config(&content)
}

/// pure function: 从 YAML 字符串解析配置
///
/// - input: YAML 内容字符串
/// - output: 解析后的配置结构
pub fn parse_config(content: &str) -> Result<KirinConfig, Box<dyn std::error::Error>> {
    let config: KirinConfig = serde_yaml::from_str(content)?;
    Ok(config)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 辅助函数：在临时文件中写入 YAML 内容并返回文件路径
    fn write_temp_yaml(content: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    /// 完整配置加载
    #[test]
    fn test_load_full_config() {
        let yaml = r#"
server:
  listen: "0.0.0.0:8080"
  threads: 2

routes:
  - route_id: "test-user"
    path: "/api/users"
    upstream: user-service
    applicant: "test"
    applied_at: "2026-04-20T00:00:00+08:00"
    description: "test route"
  - route_id: "test-default"
    path_prefix: "/api/"
    upstream: default-service
    applicant: "test"
    applied_at: "2026-04-20T00:00:00+08:00"
    description: "test default route"

upstreams:
  user-service:
    nodes:
      - addr: "127.0.0.1:9001"
        weight: 2
  default-service:
    nodes:
      - addr: "127.0.0.1:9002"

rate_limit:
  capacity: 100
  refill_rate: 10

admin:
  listen: "127.0.0.1:9090"
"#;
        let file = write_temp_yaml(yaml);
        let config = load_config(file.path().to_str().unwrap()).unwrap();

        assert_eq!(config.server.listen, "0.0.0.0:8080");
        assert_eq!(config.server.threads, Some(2));
        assert_eq!(config.routes.len(), 2);
        assert_eq!(config.routes[0].path, Some("/api/users".to_string()));
        assert_eq!(config.routes[1].path_prefix, Some("/api/".to_string()));
        assert!(config.upstreams.contains_key("user-service"));
        assert!(config.rate_limit.is_some());
        let rl = config.rate_limit.unwrap();
        assert_eq!(rl.capacity, 100);
        assert_eq!(rl.refill_rate, 10);
        assert!(config.admin.is_some());
        assert_eq!(config.admin.unwrap().listen, "127.0.0.1:9090");
    }

    /// 可选字段缺失时使用默认值
    #[test]
    fn test_load_config_without_optional_fields() {
        let yaml = r#"
server:
  listen: "0.0.0.0:8080"

routes:
  - route_id: "test-route"
    path: "/api/test"
    upstream: svc
    applicant: "test"
    applied_at: "2026-04-20T00:00:00+08:00"
    description: "test route"

upstreams:
  svc:
    nodes:
      - addr: "127.0.0.1:9001"
"#;
        let file = write_temp_yaml(yaml);
        let config = load_config(file.path().to_str().unwrap()).unwrap();

        assert!(config.rate_limit.is_none());
        assert!(config.admin.is_none());
        assert!(config.server.threads.is_none());
    }

    /// 文件不存在返回 Err
    #[test]
    fn test_load_config_file_not_found() {
        let result = load_config("/nonexistent/path/config.yaml");
        assert!(result.is_err());
    }

    /// YAML 格式错误返回 Err
    #[test]
    fn test_load_config_invalid_yaml() {
        let yaml = "this is not: valid: yaml: {{{{";
        let file = write_temp_yaml(yaml);
        let result = load_config(file.path().to_str().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_config_valid_yaml() {
        let yaml = r#"
    server:
      listen: "0.0.0.0:8080"
    routes:
      - route_id: "test"
        path: "/api/test"
        upstream: svc
        applicant: "test"
        applied_at: "2026-04-20T00:00:00+08:00"
        description: "test"
    upstreams:
      svc:
        nodes:
          - addr: "127.0.0.1:9001"
    "#;
        let config = parse_config(yaml).unwrap();
        assert_eq!(config.server.listen, "0.0.0.0:8080");
        assert_eq!(config.routes.len(), 1);
    }

    #[test]
    fn test_parse_config_invalid_yaml() {
        let result = parse_config("this is not: valid: yaml: {{{{");
        assert!(result.is_err());
    }
}
