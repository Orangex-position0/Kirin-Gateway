use super::types::KirinConfig;

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
