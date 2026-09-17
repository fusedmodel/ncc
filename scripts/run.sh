#!/usr/bin/env bash
# 启动 ncc.ai 一体化服务：API + 计费 + 前端 SPA（需先 npm run build && npm run server:build）
# 本脚本位于 opensource-project/scripts；server/web/data 属私有核心，位于同级 ncc-platform/。
set -euo pipefail
SELF="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SELF/.." && pwd)"                      # opensource-project（release 等在此）
PLATFORM="$(cd "$SELF/../../ncc-platform" && pwd)"  # 私有平台（server/web/data）

export NCC_WEB_DIST="${NCC_WEB_DIST:-$PLATFORM/web/dist}"
export NCC_DOWNLOAD_DIR="${NCC_DOWNLOAD_DIR:-$ROOT/release/bin}"
export NCC_PUBLIC_BASE="${NCC_PUBLIC_BASE:-http://localhost:8181}"
export NCC_APP_URL="${NCC_APP_URL:-$NCC_PUBLIC_BASE}"
export NCC_DATA_DIR="${NCC_DATA_DIR:-$PLATFORM/server/data}"
export GIN_MODE="${GIN_MODE:-release}"
# 可选支付：NCC_STRIPE_SECRET_KEY / NCC_STRIPE_PUBLISHABLE_KEY / NCC_STRIPE_WEBHOOK_SECRET / NCC_STRIPE_PRO_PRICE
# 可选开发跳域：NCC_CORS_ORIGINS=http://localhost:5174

exec "$PLATFORM/server/dist/ncc-server"
