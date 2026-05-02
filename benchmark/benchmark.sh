#!/usr/bin/env bash
set -euo pipefail

# ============================================
# Kirin Gateway 基准压测脚本
# 依赖：wrk (https://github.com/wg/wrk)
# 用法：./scripts/benchmark.sh [轮次] [目标URL]
# ============================================

ITERATIONS=${1:-3}
TARGET_URL=${2:-"http://127.0.0.1:8080/api/users"}
THREADS=4
CONNECTIONS=256
DURATION="30s"
OUTPUT_FILE="docs/benchmark.md"

# 检查 wrk 是否安装
if ! command -v wrk &> /dev/null; then
    echo "错误：wrk 未安装"
    echo "安装方式："
    echo "  Ubuntu/Debian: sudo apt install wrk"
    echo "  macOS: brew install wrk"
    echo "  Windows: 可使用 WSL 或下载预编译二进制"
    exit 1
fi

echo "========================================"
echo "Kirin Gateway 基准测试"
echo "目标: ${TARGET_URL}"
echo "轮次: ${ITERATIONS}"
echo "线程: ${THREADS}, 连接: ${CONNECTIONS}, 时长: ${DURATION}"
echo "========================================"

# 收集环境信息
OS_INFO=$(uname -srm 2>/dev/null || echo "unknown")
RUST_VERSION=$(rustc --version 2>/dev/null || echo "unknown")
DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# 用于存储每轮结果
declare -a QPS_VALUES
declare -a LATENCY_VALUES
declare -a P50_VALUES
declare -a P90_VALUES
declare -a P99_VALUES

for i in $(seq 1 "$ITERATIONS"); do
    echo ""
    echo "--- 轮次 ${i}/${ITERATIONS} ---"

    RESULT=$(wrk -t"${THREADS}" -c"${CONNECTIONS}" -d"${DURATION}" \
        --latency "${TARGET_URL}" 2>&1)

    echo "$RESULT"

    # 提取 QPS（Requests/sec 行）
    QPS=$(echo "$RESULT" | grep "Requests/sec" | awk '{print $2}')
    # 提取平均延迟（Latency 行的第一个数值）
    AVG_LATENCY=$(echo "$RESULT" | grep -A1 "Latency" | tail -1 | awk '{print $2}')
    # 提取 P50
    P50=$(echo "$RESULT" | grep "50%" | awk '{print $2}')
    # 提取 P90
    P90=$(echo "$RESULT" | grep "90%" | awk '{print $2}')
    # 提取 P99
    P99=$(echo "$RESULT" | grep "99%" | awk '{print $2}')

    QPS_VALUES+=("$QPS")
    LATENCY_VALUES+=("$AVG_LATENCY")
    P50_VALUES+=("$P50")
    P90_VALUES+=("$P90")
    P99_VALUES+=("$P99")
done

# 写入 docs/benchmark.md（追加模式）
{
    echo ""
    echo "## Benchmark - ${DATE}"
    echo ""
    echo "**环境：** ${OS_INFO} / ${RUST_VERSION}"
    echo "**日期：** ${DATE}"
    echo "**命令：** wrk -t${THREADS} -c${CONNECTIONS} -d${DURATION} ${TARGET_URL}"
    echo ""
    echo "| 轮次 | QPS | 平均延迟 | P50 | P90 | P99 |"
    echo "|---|---|---|---|---|---|"

    for i in "${!QPS_VALUES[@]}"; do
        echo "| $((i + 1)) | ${QPS_VALUES[$i]} | ${LATENCY_VALUES[$i]} | ${P50_VALUES[$i]} | ${P90_VALUES[$i]} | ${P99_VALUES[$i]} |"
    done

    # 计算平均值（简单 shell 算术，只处理整数部分）
    AVG_QPS=$(echo "${QPS_VALUES[@]}" | awk '{s=0; for(i=1;i<=NF;i++) s+=$i; printf "%.0f", s/NF}')
    echo "| **平均** | **${AVG_QPS}** | - | - | - | - |"
    echo ""
} >> "$OUTPUT_FILE"

echo ""
echo "结果已追加到 ${OUTPUT_FILE}"