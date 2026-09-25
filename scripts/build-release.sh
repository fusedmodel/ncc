#!/usr/bin/env bash
# 构建 ncc CLI 发布二进制到 release/bin（含 checksums.txt）
#   默认：当前平台；--all：尝试交叉编译全部平台（需 rustup 安装对应 target）
#   --slim：出「瘦身版」（--no-default-features，不含 wasmtime 沙箱）
#
# 为什么要有 --slim：默认构建带 wasm 沙箱（`ncc hur run --exec` 直接可用），
# 代价是体积 ~12MB（瘦身版 ~5.3MB）与更长的编译时间。瘦身版里 `--exec` 会**如实拒绝**
# 并指路（不假装能跑），其余命令一字不差。产物名带 `-slim` 后缀，与常规产物共存。
#
# 注意：CI（.github/workflows/release.yml）**不用本脚本**（它要的是「要么全出，要么明确失败」，
# 而本脚本 --all 会静默跳过不可用的 target）。要在发布里加瘦身变体，得改 workflow 的矩阵。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# 输出目录可用 NCC_RELEASE_OUT 覆盖（默认 release/bin）。
# 为什么要这个：`release/bin/*` 是**被 git 跟踪的已发布产物**，直接在里面跑会把它们盖掉；
# 验证脚本、或想并存多个变体时，指到临时目录即可。
OUT="${NCC_RELEASE_OUT:-$ROOT/release/bin}"
mkdir -p "$OUT"

ALL=""
# 瘦身版也可用环境变量开（与 NCC_RELEASE_OUT 同一风格，方便 CI 里直接设）
SLIM="${NCC_SLIM:-}"
for arg in "$@"; do
  case "$arg" in
    --all) ALL=1 ;;
    --slim) SLIM=1 ;;
    -h|--help) echo "用法: $0 [--all] [--slim]   （也可用 NCC_SLIM=1 / NCC_RELEASE_OUT=<dir>）"; exit 0 ;;
    *) echo "未知参数：${arg}（支持 --all / --slim）" >&2; exit 2 ;;
  esac
done
FEAT=""
SUFFIX=""
LABEL="含沙箱"
# `NCC_SLIM=0` / `false` 不算开启（否则"非空即真"会让 0 出成瘦身版）
if [ "$SLIM" = "0" ] || [ "$SLIM" = "false" ]; then SLIM=""; fi
if [ -n "$SLIM" ]; then
  FEAT="--no-default-features"
  SUFFIX="-slim"
  LABEL="瘦身版"
fi

detect_os() {
  case "$(uname -s)" in
    Darwin) echo darwin ;;
    Linux)  echo linux ;;
    *) echo unknown ;;
  esac
}
detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo x86_64 ;;
    arm64|aarch64) echo arm64 ;;
    *) echo "$(uname -m)" ;;
  esac
}

build_one() { # build_one <os> <arch> <rust-target> [exe]
  local os="$1" arch="$2" target="$3" exe="${4:-}"
  local name="ncc-${os}-${arch}${SUFFIX}${exe}"
  # FEAT 可能为空，故意不加引号（空串要展开成「没有参数」）
  cargo build --manifest-path "$ROOT/cli/Cargo.toml" --release $FEAT --target "$target" >/dev/null
  cp "$ROOT/cli/target/$target/release/ncc${exe}" "$OUT/$name"
  echo "  ✓ $name"
}

if [ -n "$ALL" ]; then
  echo "== 交叉编译全部平台 =="
  targets=(
    "darwin x86_64 x86_64-apple-darwin"
    "darwin arm64 aarch64-apple-darwin"
    "linux x86_64 x86_64-unknown-linux-gnu"
    "linux arm64 aarch64-unknown-linux-gnu"
    "windows x86_64 x86_64-pc-windows-gnu .exe"
  )
  for t in "${targets[@]}"; do
    read -r os arch target exe <<< "$t"
    if rustup target add "$target" >/dev/null 2>&1; then
      build_one "$os" "$arch" "$target" "$exe" || echo "  ✗ 跳过 $os/$arch"
    else
      echo "  ✗ 跳过 ${os}/${arch}（target 不可用）"
    fi
  done
else
  OS="$(detect_os)"
  ARCH="$(detect_arch)"
  echo "== 构建当前平台：${OS}/${ARCH}（${LABEL}）=="
  # FEAT 可能为空，故意不加引号
  cargo build --manifest-path "$ROOT/cli/Cargo.toml" --release $FEAT
  cp "$ROOT/cli/target/release/ncc" "$OUT/ncc-${OS}-${ARCH}${SUFFIX}"
  echo "  ✓ ncc-${OS}-${ARCH}${SUFFIX}"
fi

# 校验和
(cd "$OUT" && shasum -a 256 ncc-* > checksums.txt)
echo "checksums.txt:"
cat "$OUT/checksums.txt"
echo "→ 产物位于 $OUT"
