#!/usr/bin/env sh
# NCC MCP harness 入口：以 stdio 启动 NCC 的 MCP server。
#
# 任何实现了 `mcp/stdio` 的 runtime 都能加载本包（见同目录 harness.json）。
# 依赖：PATH 中有 `ncc`
#   curl -fsSL https://ncc.ai/install.sh | sh     # 或 cargo install --path cli
#
# 连哪个 ncc：由目标（target）决定，三种写法都可以（会透传给 `ncc mcp`）：
#   run.sh --target office                 # 用已保存的内网节点目标
#   run.sh --base http://host:8282         # 按地址（复用/新建目标）
#   run.sh                                 # 用当前默认目标（ncc target list 看）
# 一个 MCP 服务进程绑定一个目标；要两边都用，就配两个 server（各自带不同的 args）。
set -eu

if ! command -v ncc >/dev/null 2>&1; then
  echo "需要先安装 ncc（curl -fsSL https://ncc.ai/install.sh | sh）" >&2
  exit 1
fi

# stdout 是 MCP 协议通道，日志一律走 stderr
exec ncc mcp "$@"
