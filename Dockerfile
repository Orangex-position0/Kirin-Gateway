# ============================================
# 阶段 1：构建
# ============================================
FROM rust:1.85-slim AS builder

WORKDIR /app

# 先拷贝依赖文件
COPY Cargo.toml Cargo.lock ./

# 创建空壳项目用于缓存依赖编译
# 必须创建所有 mod 声明对应的空文件，否则 cargo build 会失败
RUN mkdir -p src/config src/control_plane/admin_api src/data_plane/filter src/data_plane/router src/observability && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/config.rs && \
    echo "" > src/config/loader.rs && \
    echo "" > src/config/types.rs && \
    echo "" > src/config/validation.rs && \
    echo "" > src/control_plane.rs && \
    echo "" > src/control_plane/control_plane.rs && \
    echo "" > src/control_plane/admin_api.rs && \
    echo "" > src/control_plane/admin_api/dto.rs && \
    echo "" > src/control_plane/gateway_state.rs && \
    echo "" > src/control_plane/health_check.rs && \
    echo "" > src/data_plane.rs && \
    echo "" > src/data_plane/proxy.rs && \
    echo "" > src/data_plane/upstream.rs && \
    echo "" > src/data_plane/middleware.rs && \
    echo "" > src/data_plane/rate_limit.rs && \
    echo "" > src/data_plane/filter.rs && \
    echo "" > src/data_plane/filter/header.rs && \
    echo "" > src/data_plane/filter/auth.rs && \
    echo "" > src/data_plane/filter/logging.rs && \
    echo "" > src/data_plane/filter/method.rs && \
    echo "" > src/data_plane/filter/rate_limit_filter.rs && \
    echo "" > src/data_plane/filter/whitelist.rs && \
    echo "" > src/data_plane/router.rs && \
    echo "" > src/data_plane/router/router_white_list.rs && \
    echo "" > src/observability.rs && \
    echo "" > src/observability/metrics.rs

# 编译依赖（这一层会被 Docker 缓存）
RUN cargo build --release 2>/dev/null || true

# 拷贝实际源码并重新构建
COPY src/ src/

# 触发重新编译
RUN touch src/main.rs && cargo build --release

# ============================================
# 阶段 2：运行
# ============================================
FROM debian:bookworm-slim

# 安装运行时依赖
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# 拷贝二进制和配置
COPY --from=builder /app/target/release/kirin-gw /usr/local/bin/kirin-gw
COPY config.yaml /etc/kirin/config.yaml

EXPOSE 6188 6189

ENTRYPOINT ["kirin-gw", "/etc/kirin/config.yaml"]
