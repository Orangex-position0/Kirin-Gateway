# Kirin Gateway 基准测试

基于 [wrk](https://github.com/wg/wrk) 的 HTTP 压测工具，用于测量网关的吞吐量和延迟分布。

## 前提条件

1. **网关正在运行** — 脚本向目标 URL 发送真实请求，网关和上游服务都需要提前启动
2. **路由已配置** — 目标路由（默认 `/api/users`）需要在 `config.yaml` 中配置了对应的上游服务
3. **wrk 已安装** — 脚本启动时会自动检测，未安装会报错退出

### wrk 安装

| 平台 | 命令 |
|---|---|
| Ubuntu/Debian | `sudo apt install wrk` |
| macOS | `brew install wrk` |
| Windows | 使用 WSL 或从源码编译 |

> wrk 不支持 Windows 原生。Windows 用户需要在 WSL 中运行此脚本，同时确保 WSL 能访问到网关监听的地址。

## 使用方法

```bash
# 默认：3 轮测试，目标 http://127.0.0.1:8080/api/users
bash benchmark/benchmark.sh

# 自定义轮次
bash benchmark/benchmark.sh 5

# 自定义轮次和目标 URL
bash benchmark/benchmark.sh 5 http://127.0.0.1:8080/api/users
```

## 测试参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| 线程数 | 4 | wrk 并发线程数 |
| 连接数 | 256 | 总 TCP 连接数 |
| 持续时间 | 30s | 每轮测试时长 |
| 轮次 | 3 | 重复测试次数，用于取平均值 |

## 输出

- **终端输出** — 每轮 wrk 原始结果实时打印
- **文件输出** — 结果以 Markdown 表格追加到 `docs/benchmark.md`，包含：
  - 环境信息（OS、Rust 版本）
  - 每轮 QPS、平均延迟、P50、P90、P99
  - 多轮 QPS 平均值

## 完整流程示例

```bash
# 1. 启动上游服务（示例）
python -m http.server 9000

# 2. 启动网关
cargo run -- config.yaml

# 3. 另开终端，执行压测
bash benchmark/benchmark.sh 3 http://127.0.0.1:8080/api/users

# 4. 查看结果
cat docs/benchmark.md
```
