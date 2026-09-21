#!/usr/bin/env sh
# NCC MCP harness 入口：以 stdio 启动 NCC 的 MCP server。
#
# 任何实现了 `mcp/stdio` 的 runtime 都能加载本包（见同目录 harness.json）。
# 依赖：PATH 中有 `ncc`
#   curl -fsSL https://ncc.ai/install.sh | sh     # 或 cargo install --path cli
#
# 自托管实例：把基址透传进来，例如 harness entry 写成 `run.sh --base http://host:8181`
set -eu

if ! command -v ncc >/dev/null 2>&1; then
  echo "需要先安装 ncc（curl -fsSL https://ncc.ai/install.sh | sh）" >&2
  exit 1
fi

# stdout 是 MCP 协议通道，日志一律走 stderr
exec ncc mcp "$@"
