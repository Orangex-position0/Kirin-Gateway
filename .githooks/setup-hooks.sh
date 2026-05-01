#!/usr/bin/env bash
# setup-hooks.sh: 配置 Git hooks 路径
# 将 .githooks/ 目录设置为项目的 Git hooks 目录

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

git config core.hooksPath .githooks

echo "Git hooks 已配置完成！"
echo "  hooks 目录: .githooks/"
echo "  pre-commit:  cargo fmt --check"
echo "  pre-push:    cargo clippy && cargo test"
