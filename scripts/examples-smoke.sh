#!/usr/bin/env bash
# 示例包冒烟：`examples/` 下的每个工程都必须是**真的能过**的 HUR 包。
#
# 为什么要有它：示例会烂。今天 `ncc hur verify` 通过，明天改了 R 规则或产物命名，
# 谁都不知道 —— 读者照抄一个已经不合规的示例，比没有示例更糟。所以这里**全程离线**
# 把每个示例自己走一遍：verify（R1~R12）→ pack（确定性产物）→ 断言产物名与容器。
#
# 全程隔离：`NCC_HOME` / `HUR_HOME` 都指向临时目录，**不碰你真实的 ~/.ncc 与 ~/.harnessuse**；
# pack 在**副本**上跑，所以不会往仓库里写 `dist/`。
#
# 用法：bash scripts/examples-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
export NCC_HOME="${TMP}/home"
export HUR_HOME="${TMP}/hur"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/cli/target/release/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc" >&2; exit 1; }

PASS=0
FAIL=0
say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }

# 读清单里的一个字段（**清单是权威**：下面的断言都拿它当基准，不解析文件名猜）
manifest_field() { python3 -c "
import json,sys
p = json.load(open(sys.argv[1], encoding='utf-8'))
print(p.get(sys.argv[2], ''))
" "$1/hur.json" "$2"; }

say "0. 环境"
printf '  CLI      %s\n' "${CLI}"
"${CLI}" --version >/dev/null 2>&1 && good "CLI 可执行" || bad "CLI 跑不起来"

shopt -s nullglob
DIRS=("${ROOT}/examples/"*/)
shopt -u nullglob
if [[ ${#DIRS[@]} -eq 0 ]]; then
  bad "examples/ 下没有任何示例包"
fi

for dir in "${DIRS[@]:-}"; do
  [[ -f "${dir}/hur.json" ]] || continue          # 不是包根（比如 README 之类）
  name="$(basename "${dir}")"
  say "示例 ${name}"

  ID="$(manifest_field "${dir}" id)"
  PROFILE="$(manifest_field "${dir}" profile)"

  # ① 校验：不过就没有下文（示例的价值全在"它是真的"）
  OUT="$("${CLI}" hur verify "${dir}" 2>&1)" && R=0 || R=$?
  if [[ ${R} -eq 0 ]]; then good "verify 通过（R1~R12）"; else bad "verify 没过"; printf '%s\n' "${OUT}" | sed 's/^/      /'; continue; fi

  # ② 示例自己得说清自己是什么（profile 决定必填项，缺了说明包没写全）
  if [[ -n "${PROFILE}" ]]; then good "清单写了 profile=${PROFILE}"; else bad "清单没写 profile（老包能过，但示例该写全）"; fi

  # ③ 每个示例都得有 README（用法与边界）
  [[ -f "${dir}/README.md" ]] && good "带 README.md（用法 + 边界）" || bad "缺 README.md"

  # ④ 产物：在副本上打包，别往仓库里写 dist/
  cp -R "${dir}" "${TMP}/pkg"
  rm -rf "${TMP}/pkg/dist"
  OUT="$("${CLI}" hur pack "${TMP}/pkg" --json 2>&1)" && R=0 || R=$?
  if [[ ${R} -ne 0 ]]; then bad "pack 失败"; printf '%s\n' "${OUT}" | sed 's/^/      /'; rm -rf "${TMP}/pkg"; continue; fi
  ART="$(printf '%s' "${OUT}" | python3 -c "
import json,sys
try: print(json.load(sys.stdin).get('file',''))
except Exception: print('')
")"
  if [[ -n "${ART}" && -f "${ART}" ]]; then good "pack 出了产物（$(basename "${ART}")）"; else bad "pack 没给出可用的产物路径"; fi
  check "产物名带容器后缀（hur 是规范，末尾跟压缩包格式）" "yes" \
    "$([[ "$(basename "${ART}")" == *".hur.gz" ]] && echo yes || echo no)"
  check "产物名里的 id 与清单一致" "yes" \
    "$([[ "$(basename "${ART}")" == "${ID}-"* ]] && echo yes || echo no)"
  if [[ -n "${PROFILE}" ]]; then
    check "产物名里的 profile 段与清单一致" "yes" \
      "$([[ "$(basename "${ART}")" == *".${PROFILE}.hur.gz" ]] && echo yes || echo no)"
  fi
  check "产物外层是 gzip（1f 8b）" "1f8b" "$(python3 -c "print(open('${ART}','rb').read(2).hex())")"
  check "侧车跟着新名字" "yes" "$([[ -f "${ART}.sha256" ]] && echo yes || echo no)"

  rm -rf "${TMP}/pkg"
done

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
