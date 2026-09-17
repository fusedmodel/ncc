#!/usr/bin/env bash
# 启动 ncc.ai 一体化服务：API + 计费 + 前端 SPA（需先 npm run build && npm run server:build）
# 本脚本位于 ncc-cli/scripts；server/web/data 属私有核心，位于同级 ncc-platform/。
set -euo pipefail
SELF="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SELF/.." && pwd)"                      # ncc-cli（release 等在此）
PLATFORM="$(cd "$SELF/../../ncc-platform" && pwd)"  # 私有平台（server/web/data）

# 载入工程配置（$PLATFORM/.env：邀请码 NCC_INVITE_CODE、测试账号 NCC_TEST_*、支付密钥等）。
# 已 export 的环境变量优先，不会被文件覆盖。
if [ -f "${PLATFORM}/.env" ]; then
  while IFS= read -r line || [ -n "${line}" ]; do
    line="${line%$'\r'}"
    case "${line}" in ''|'#'*) continue ;; esac
    case "${line}" in *=*) ;; *) continue ;; esac
    key="${line%%=*}"
    val="${line#*=}"
    key="$(printf '%s' "${key}" | tr -d '[:space:]')"
    val="$(printf '%s' "${val}" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    case "${val}" in
      \"*\") val="${val#\"}"; val="${val%\"}" ;;
      \'*\') val="${val#\'}"; val="${val%\'}" ;;
    esac
    [ -n "${key}" ] || continue
    if [ -z "${!key:-}" ]; then export "${key}=${val}"; fi
  done < "${PLATFORM}/.env"
fi

export NCC_WEB_DIST="${NCC_WEB_DIST:-$PLATFORM/web/dist}"
export NCC_DOWNLOAD_DIR="${NCC_DOWNLOAD_DIR:-$ROOT/release/bin}"
export NCC_PUBLIC_BASE="${NCC_PUBLIC_BASE:-http://localhost:8181}"
export NCC_APP_URL="${NCC_APP_URL:-$NCC_PUBLIC_BASE}"
export NCC_DATA_DIR="${NCC_DATA_DIR:-$PLATFORM/server/data}"
export GIN_MODE="${GIN_MODE:-release}"
# 可选支付：NCC_STRIPE_SECRET_KEY / NCC_STRIPE_PUBLISHABLE_KEY / NCC_STRIPE_WEBHOOK_SECRET / NCC_STRIPE_PRO_PRICE
# 可选开发跳域：NCC_CORS_ORIGINS=http://localhost:5174
# 注册门禁：NCC_INVITE_CODE（逗号分隔多码；off 则开放注册）；测试账号：NCC_TEST_EMAIL / NCC_TEST_PASSWORD

exec "$PLATFORM/server/dist/ncc-server"
