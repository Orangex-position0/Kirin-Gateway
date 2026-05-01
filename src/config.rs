#![allow(unused_imports)]

pub mod loader;
pub mod types;
pub mod validation;

// 统一 re-export，保持外部 API 不变
pub use loader::*;
pub use types::*;
pub use validation::ValidationError;
pub use validation::validate_config;

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
