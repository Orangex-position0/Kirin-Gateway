# ============================================
# 1. cargo-chef
# ============================================
FROM rust:1.95-slim AS chef
RUN apt-get update && apt-get install -y --no-install-recommends cmake g++ make && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ============================================
# 2. builder
# ============================================
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release && strip target/release/kirin-gw

# ============================================
# 3. runtime
# ============================================
FROM gcr.io/distroless/cc-debian12
WORKDIR /app
COPY --from=builder /app/target/release/kirin-gw /app/kirin-gw
COPY config.yaml /etc/kirin/config.yaml
USER nonroot:nonroot
EXPOSE 6188 6189 8080 9100 7189
ENTRYPOINT ["/app/kirin-gw", "/etc/kirin/config.yaml"]
