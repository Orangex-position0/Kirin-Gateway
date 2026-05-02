#!/usr/bin/env bash
set -euo pipefail

# ============================================
# 生成自签名 TLS 测试证书
# 用法：./scripts/gen_cert.sh
# 输出：certs/server.crt + certs/server.key
# ============================================

CERT_DIR="certs"
mkdir -p "$CERT_DIR"

echo "生成自签名 TLS 证书..."

openssl req -x509 -newkey rsa:2048 \
    -keyout "${CERT_DIR}/server.key" \
    -out "${CERT_DIR}/server.crt" \
    -days 365 \
    -nodes \
    -subj "/C=CN/ST=Shanghai/L=Shanghai/O=Kirin Gateway/CN=localhost" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"

echo "证书已生成："
echo "  证书: ${CERT_DIR}/server.crt"
echo "  私钥: ${CERT_DIR}/server.key"
echo ""
echo "请确保 ${CERT_DIR}/ 目录已在 .gitignore 中"