# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-04-29

### Added

- 基于 Pingora 框架的 API 网关项目骨架，包含路由匹配、反向代理、负载均衡、中间件链和令牌桶限流等核心功能
- 数据平面：Filter Chain（Auth / Header / Logging / Method / RateLimit / Whitelist）、Proxy、Rate Limiter、Router、Upstream 模块
- 控制平面：Admin API、Gateway State、Health Check 模块
- 配置管理：YAML 配置加载与反序列化，支持中英文示例配置
- 集成测试覆盖核心功能
- rustfmt.toml 统一代码格式
- 项目文档：README（中英文）、配置参考、Filter Chain 文档

### Changed

- 提取纯函数，重构 data_plane 和 control_plane 模块（admin_api、filter、router、proxy、rate_limit）
- 改进集成测试覆盖率与可靠性

### Fixed

- 解决所有 Clippy 警告，实现零警告构建（含 unused_imports、collapsible_if、for_kv_map 等）
- 修复集成测试中的 zombie_processes 问题
- 修复 config.yaml 以匹配集成测试要求
- 添加 .gitattributes 强制 LF 换行符
