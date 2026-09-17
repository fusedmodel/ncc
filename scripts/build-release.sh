#!/usr/bin/env bash
# 构建 ncc CLI 发布二进制到 release/bin（含 checksums.txt）
#   默认：当前平台；--all：尝试交叉编译全部平台（需 rustup 安装对应 target）
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/release/bin"
mkdir -p "$OUT"

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
  local name="ncc-${os}-${arch}${exe}"
  cargo build --manifest-path "$ROOT/cli/Cargo.toml" --release --target "$target" >/dev/null
  cp "$ROOT/cli/target/$target/release/ncc${exe}" "$OUT/$name"
  echo "  ✓ $name"
}

if [ "${1:-}" = "--all" ]; then
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
      echo "  ✗ 跳过 $os/$arch（target 不可用）"
    fi
  done
else
  OS="$(detect_os)"
  ARCH="$(detect_arch)"
  echo "== 构建当前平台：$OS/$ARCH =="
  cargo build --manifest-path "$ROOT/cli/Cargo.toml" --release
  cp "$ROOT/cli/target/release/ncc" "$OUT/ncc-${OS}-${ARCH}"
  echo "  ✓ ncc-${OS}-${ARCH}"
fi

# 校验和
(cd "$OUT" && shasum -a 256 ncc-* > checksums.txt)
echo "checksums.txt:"
cat "$OUT/checksums.txt"
echo "→ 产物位于 $OUT"
