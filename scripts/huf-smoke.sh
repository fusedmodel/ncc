#!/usr/bin/env bash
# `.huf` 资源包端到端冒烟：**本地工具链** + **真发一份到内网节点**。
#
# 验的是三件事（都是"会不会真出事"的地方）：
# 1. **边界**：资源包不许出现"能跑"的字段（`entry`…），`.hur` 那条线也不认 `.huf` ——
#    一个文件能不能跑，看扩展名与规范号就该知道，不该靠"解开来看看"。
# 2. **字节**：同样的内容打两次必须同一串 sha256（确定性字节是签名与去重的地基）；
#    改一个字节、改侧车、改内容不重新 build，三种都要被挡住。
# 3. **链路**：`ncc publish --kind huf` 到节点后，`kinds` 里看得到、搜得到、下回来字节一致。
#
# 全程隔离：NCC_HOME / HUR_HOME（密钥）/ 节点数据目录 / 端口都在临时目录里，
# **不碰你真实的 ~/.ncc、~/.harnessuse 与节点库**。
# 用法：bash scripts/huf-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
mkdir -p "${WORK}"
PORT="${PORT:-18575}"
NODE="http://127.0.0.1:${PORT}"
export NCC_HOME="${TMP}/home"
export HUR_HOME="${TMP}/hur"

REPO="$(cd "${ROOT}/.." && pwd)"                    # ncc-ai/
NODE_SRC="${REPO}/ncc-connector/rust"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc" >&2; exit 1; }
[[ -d "${NODE_SRC}" ]] || { echo "找不到 ncc-connector（${NODE_SRC}）" >&2; exit 1; }

PASS=0
FAIL=0
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [[ -n "${KEEP:-}" ]]; then echo "（KEEP=1）保留 ${TMP}"; else rm -rf "${TMP}"; fi
}
trap cleanup EXIT

say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 300)）"; fi; }
nz() { if [[ "$2" != "0" ]]; then good "$1（退出码 $2）"; else bad "$1：本该失败却退出 0"; fi; }
sha_of() { python3 -c "import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())" "$1"; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}

PKG="${WORK}/onboarding"
say "1. init：一份资源包工程（没有 src/，那是代码的落点）"
OUT="$("${CLI}" huf init "${PKG}" --id docs/onboarding --name "上手文档" --short "第一次用它看这份" 2>&1)"
contains "生成了工程" "已生成资源包工程" "${OUT}"
contains "下一步里点明了没有 src/" "没有 src/" "${OUT}"
[[ -f "${PKG}/huf.json" ]] && good "写了 huf.json" || bad "没有 huf.json"
[[ -f "${PKG}/docs/README.md" ]] && good "搭了 docs/ 骨架" || bad "没有 docs/README.md"
check "spec 号" "harness-use-files/v1" "$(python3 -c "import json;print(json.load(open('${PKG}/huf.json'))['spec'])")"
check "kind" "files" "$(python3 -c "import json;print(json.load(open('${PKG}/huf.json'))['kind'])")"

mkdir -p "${PKG}/assets" "${PKG}/src"
printf '# 快速上手\n\n三步走。\n' > "${PKG}/docs/quick.md"
printf 'ncc\n' > "${PKG}/assets/note.txt"
printf 'fn main() {}\n' > "${PKG}/src/main.rs"   # 故意放一份"代码"：资源包不该收它

say "2. build + verify（H1~H7 本地）"
OUT="$("${CLI}" huf build "${PKG}" 2>&1)"
contains "锁写好了" "文件已锁定" "${OUT}"
contains "只锁内容目录里的文件（src/ 不算）" "3 个文件" "${OUT}"
OUT="$("${CLI}" huf verify "${PKG}" 2>&1)" && R=0 || R=$?
check "退出码" "0" "${R}"
contains "校验通过" "校验通过" "${OUT}"
OUT="$("${CLI}" huf ls "${PKG}" 2>&1)"
contains "列出了文档" "docs/quick.md" "${OUT}"
if [[ "${OUT}" == *"src/main.rs"* ]]; then bad "资源包把 src/ 也当内容收进去了"; else good "src/ 不在内容清单里"; fi

say "3. H3：资源包里不许有「能跑」的字段"
python3 - "${PKG}/huf.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d["entry"]="src/main.rs"; json.dump(d,open(p,'w'),ensure_ascii=False,indent=2)
PY
OUT="$("${CLI}" huf verify "${PKG}" 2>&1)" && R=0 || R=$?
nz "有 entry ⇒ 校验不过" "${R}"
contains "H3 点名了那个字段" "H3" "${OUT}"
contains "指出了能跑的是 .hur" "ncc hur" "${OUT}"
OUT="$("${CLI}" huf pack "${PKG}" 2>&1)" && R=0 || R=$?
nz "有 entry ⇒ 拒绝产包（别把坏字节发出去）" "${R}"
python3 - "${PKG}/huf.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d.pop("entry",None); json.dump(d,open(p,'w'),ensure_ascii=False,indent=2)
PY

say "4. pack：确定性字节 + 侧车"
OUT="$("${CLI}" huf pack "${PKG}" 2>&1)"
contains "产物是 .huf.gz（通用压缩包结尾）" ".huf.gz" "${OUT}"
ART="${PKG}/dist/docs_onboarding-0.1.0.huf.gz"
[[ -f "${ART}" ]] && good "产物落盘 ${ART##*/}" || bad "没有 ${ART}"
[[ -f "${ART}.sha256" ]] && good "有 sha256 侧车" || bad "没有侧车"
SHA1="$(sha_of "${ART}")"
"${CLI}" huf pack "${PKG}" >/dev/null 2>&1
check "同样内容打两次，字节一致" "${SHA1}" "$(sha_of "${ART}")"

say "5. 产物校验：H9 侧车 + 改名成 .hur 会被那条线拒掉"
OUT="$("${CLI}" huf verify "${ART}" 2>&1)" && R=0 || R=$?
check "产物校验退出码" "0" "${R}"
contains "认出这是产物" "（产物）" "${OUT}"
cp "${ART}" "${WORK}/renamed.hur.gz"
OUT="$("${CLI}" hur verify "${WORK}/renamed.hur.gz" 2>&1)" && R=0 || R=$?
nz "把 .huf 交给 ncc hur ⇒ 不认（清单名不同）" "${R}"
printf 'tamper\n' > "${WORK}/bad.sha256" && cp "${ART}.sha256" "${WORK}/sidecar.bak" && cp "${WORK}/bad.sha256" "${ART}.sha256"
OUT="$("${CLI}" huf verify "${ART}" 2>&1)" && R=0 || R=$?
nz "侧车被改 ⇒ 校验不过" "${R}"
contains "H9 点名了侧车" "H9" "${OUT}"
cp "${WORK}/sidecar.bak" "${ART}.sha256"

say "6. H7：内容改了不重新 build 要被挡住"
printf '\n补一句。\n' >> "${PKG}/docs/quick.md"
OUT="$("${CLI}" huf verify "${PKG}" 2>&1)" && R=0 || R=$?
nz "内容漂移 ⇒ 校验不过" "${R}"
contains "H7 点名了锁" "H7" "${OUT}"
"${CLI}" huf build "${PKG}" >/dev/null 2>&1
"${CLI}" huf pack "${PKG}" >/dev/null 2>&1
ART_SHA="$(sha_of "${ART}")"

say "7. 签名（H8）：签的是产物那份字节，密钥不出本机"
"${CLI}" hur key gen >/dev/null 2>&1 || true
OUT="$("${CLI}" huf sign "${ART}" 2>&1)"
contains "签名成功" "已签名" "${OUT}"
contains "给出第三方核对方式" "minisign -V -p" "${OUT}"
[[ -f "${ART}.minisig" ]] && good "签名落在 <产物>.minisig" || bad "没有 ${ART}.minisig"
OUT="$("${CLI}" huf verify "${ART}" --require-signature 2>&1)" && R=0 || R=$?
check "要求签名时通过" "0" "${R}"
contains "报的是 H8" "H8" "${OUT}"
cp "${ART}" "${WORK}/tampered.huf.gz" && cp "${ART}.minisig" "${WORK}/tampered.huf.gz.minisig"
printf 'junk' >> "${WORK}/tampered.huf.gz"
OUT="$("${CLI}" huf verify "${WORK}/tampered.huf.gz" 2>&1)" && R=0 || R=$?
nz "产物被改一个字节 ⇒ H8 挡住" "${R}"
contains "说出了原因" "签名校验失败" "${OUT}"

say "8. unpack：解出来还能再校验（防穿越与清单都留住了）"
DEST="${WORK}/unpacked"
OUT="$("${CLI}" huf unpack "${ART}" -d "${DEST}" 2>&1)"
contains "解开了" "已解开" "${OUT}"
[[ -f "${DEST}/huf.json" && -f "${DEST}/docs/quick.md" ]] && good "文件都在" || bad "解出来的内容不全"
OUT="$("${CLI}" huf verify "${DEST}" 2>&1)" && R=0 || R=$?
check "解出来的目录是合法资源包" "0" "${R}"

say "9. 起节点（:${PORT}，隔离数据目录）"
mkdir -p "${WORK}/bin"
( cd "${NODE_SRC}" && cargo build -q 2>/dev/null )
NODE_BIN="$(ls -t "${NODE_SRC}"/target/debug/ncc-registry 2>/dev/null | head -1)"
[[ -n "${NODE_BIN}" ]] || { bad "节点没构建出来"; exit 1; }
NCCR_DATA_DIR="${TMP}/node-data" NCCR_PORT="${PORT}" NCCR_JWT_TTL=1h \
  "${NODE_BIN}" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }

say "10. 目录里认 huf 这个 kind"
OUT="$(curl -fsS "${NODE}/api/registry/kinds")"
contains "kinds 里有 huf" '"huf"' "${OUT}"
contains "带中文标签" "HUF" "${OUT}"

say "11. 发布 + 检索 + 下回来（字节必须一致）"
"${CLI}" --base "${NODE}" register --email huffer@ncc.dev --password huffer123 --name Huffer >/dev/null
"${CLI}" --base "${NODE}" me >/dev/null && good "已登录节点"
OUT="$("${CLI}" --base "${NODE}" publish --kind huf --name "上手文档" --slug onboarding \
  --version 0.1.0 --summary "第一次用它看这份" --file "${ART}" 2>&1)"
contains "发布成功" "已发布" "${OUT}"
OUT="$("${CLI}" --base "${NODE}" search --kind huf 2>&1)"
contains "搜得到 kind=huf" "onboarding" "${OUT}"
DL="${WORK}/downloaded.huf.gz"
OUT="$("${CLI}" --base "${NODE}" download "@huffer/onboarding" -o "${DL}" 2>&1)"
contains "下载成功" "已保存" "${OUT}"
check "下回来的字节与本地一致" "${ART_SHA}" "$(sha_of "${DL}")"
OUT="$("${CLI}" huf verify "${DL}" 2>&1)" && R=0 || R=$?
check "下回来的产物仍能过校验" "0" "${R}"

say "12. 规范总览（工具与文档的取值都从这里来）"
OUT="$("${CLI}" huf spec --json)"
python3 -c "
import json,sys
d=json.loads(sys.argv[1])
assert d['spec']=='harness-use-files/v1', d['spec']
assert d['kind']=='files', d['kind']
assert 'docs' in d['contentDirs'] and 'src' not in d['contentDirs'], d['contentDirs']
assert d['registryKind']=='huf'
assert d['runtime'] is False
print('ok')
" "${OUT}" >/dev/null && good "spec --json 的取值自洽（无 src、runtime=false、kind=huf）" || bad "spec --json 取值不对"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
