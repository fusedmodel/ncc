#!/usr/bin/env bash
# 制品加签端到端冒烟：`ncc sign` / `ncc verify`（**不限 kind** —— Skill / MCP / 任意文件）。
#
# 验的就是这件事：**签的是发布出去的那份字节**，第三方拿公钥用 stock `minisign -V`
# 就能独立核对，全程不需要 NCC。所以这里会真的下一份字节回来、真改一个字节、
# 真拿第二把钥匙去核 —— 不做假动作。
#
# 全程隔离：数据目录、NCC_HOME、HUR_HOME（密钥）、端口都在临时目录里，
# **不碰你真实的 ~/.ncc 与 ~/.harnessuse**。
# 用法：bash scripts/sign-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
PORT="${PORT:-18570}"
PLAT="http://127.0.0.1:${PORT}"
export NCC_HOME="${TMP}/home"
export HUR_HOME="${TMP}/hur"          # 密钥/信任表的落点（隔离，别动真钥匙）

REPO="$(cd "${ROOT}/.." && pwd)"      # ncc-ai/
PLAT_SRC="${REPO}/ncc-platform/server"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc" >&2; exit 1; }
[[ -d "${PLAT_SRC}" ]] || { echo "找不到 ncc-platform/server（${PLAT_SRC}）" >&2; exit 1; }

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
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」（实际：$(printf '%s' "$3" | head -c 300)）"; else good "$1"; fi; }
nz() { if [[ "$2" != "0" ]]; then good "$1（退出码 $2）"; else bad "$1：本该失败却退出 0"; fi; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "$1"; }
sha_of() { python3 -c "import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())" "$1"; }
json_sig() { python3 -c "import json,sys;print(json.dumps(json.load(open(sys.argv[1])),ensure_ascii=False))" "$1"; }

say "0. 构建平台服务端"
mkdir -p "${WORK}/bin"
( cd "${PLAT_SRC}" && cargo build --release -q --bin ncc-server && cp target/release/ncc-server "${WORK}/bin/ncc-server" )
good "ncc-platform 已构建"

say "1. 起平台（:${PORT}，隔离数据目录）"
NCC_PORT="${PORT}" NCC_DATA_DIR="${TMP}/plat-data" NCC_JWT_SECRET=smoke-sign \
  NCC_PUBLIC_BASE="${PLAT}" NCC_INVITE_CODE=off "${WORK}/bin/ncc-server" >"${TMP}/plat.log" 2>&1 &
PIDS+=($!)
alive "${PLAT}/api/meta" 40 && good "平台就绪" || { bad "平台起不来（看 ${TMP}/plat.log）"; exit 1; }

say "2. 登录 + 生成签名密钥（密钥只在 ${HUR_HOME}，不碰真实钥匙串）"
"${CLI}" --base "${PLAT}" register --email signer@ncc.dev --password signer123 --name Signer >/dev/null
"${CLI}" --base "${PLAT}" me >/dev/null && good "已登录"
"${CLI}" hur key gen >/dev/null 2>&1 || true
PUBKEY="${HUR_HOME}/keys/hur.pub"
NS="$(python3 - <<PY
import json, urllib.request
cfg = json.load(open("${NCC_HOME}/.ncc/config.json"))
t = cfg["targets"][cfg["current"]]
req = urllib.request.Request(t["base_url"] + "/api/namespaces/mine",
                             headers={"Authorization": "Bearer " + t["token"]})
d = json.load(urllib.request.urlopen(req))
ns = [n for n in d["namespaces"] if n.get("type") == "personal"] or d["namespaces"]
print(ns[0]["slug"].lstrip("@"))
PY
)"
REF="@${NS}/release-notes"
REF_MCP="@${NS}/notes-mcp"
[[ -n "${NS}" ]] && good "个人命名空间 ${NS}（后面的引用都用它，不靠猜）" || { bad "拿不到命名空间"; exit 1; }
[[ -f "${PUBKEY}" ]] && good "生成了密钥对（$(wc -l < "${PUBKEY}" | tr -d ' ') 行公钥文件）" || { bad "没生成密钥"; exit 1; }

say "3. 签一份 SKILL.md（本地动作，私钥不出设备）"
mkdir -p "${WORK}/art"
cat > "${WORK}/art/SKILL.md" <<'EOF'
---
name: release-notes
description: 把一堆提交整理成发布说明
---
# 发布说明

按影响面分组：**破坏性变更**先写，其次新增，最后修复。
EOF
SKILL="${WORK}/art/SKILL.md"
SKILL_SHA="$(sha_of "${SKILL}")"
OUT="$("${CLI}" sign "${SKILL}" --kind skill --reference "${REF}" --version 0.1.0 2>&1)"
contains "签名成功" "✅ 已签名" "${OUT}"
contains "报告了制品摘要" "${SKILL_SHA}" "${OUT}"
contains "给出了第三方核对命令" "minisign -V -p" "${OUT}"
contains "签名声明写清了 kind/引用/版本" "ncc skill ${REF} 0.1.0 sha256=" "${OUT}"
[[ -f "${SKILL}.minisig" ]] && good "签名落在 <制品>.minisig 旁边" || bad "没有 ${SKILL}.minisig"

say "4. 本地核对：真核一遍，并**真改一个字节**看它能不能挡住"
OUT="$("${CLI}" verify "${SKILL}" 2>&1)" && R=0 || R=$?
contains "本地文件核对通过" "✔ 已验证" "${OUT}"
contains "写出了签发者" "签发者" "${OUT}"
contains "claim 摘要与实测一致" "（与实测一致）" "${OUT}"
check "退出码 0" "0" "${R}"

MUT="${WORK}/art/mutated.md"
sed 's/破坏性变更/破坏性变更（被改过）/' "${SKILL}" > "${MUT}"
cp "${SKILL}.minisig" "${MUT}.minisig"
OUT="$("${CLI}" verify "${MUT}" 2>&1 || true)"
contains "改了一个字节 ⇒ 校验不通过" "✖ 签名校验不通过" "${OUT}"
OUT="$("${CLI}" verify "${MUT}" 2>&1)" && R=0 || R=$?
nz "篡改后的退出码非 0（不用加 --require-signature）" "${R}"

say "5. 发布一份带签名的 Skill（签名进清单，摘要必须绑这次发布的字节）"
SIGJSON="$("${CLI}" sign "${SKILL}" --kind skill --reference "${REF}" --version 0.1.0 --json 2>/dev/null)"
printf '%s' "${SIGJSON}" | python3 -c "
import json,sys
d=json.load(sys.stdin)
json.dump({'signature': d['signature'], 'about': 'skill: 发布说明助手'}, open('${WORK}/manifest.json','w'), ensure_ascii=False)
"
check "清单里的签名摘要 == 制品摘要" "${SKILL_SHA}" \
  "$(jget "['signature']['sha256']" < "${WORK}/manifest.json")"
"${CLI}" publish --file "${SKILL}" --kind skill --name "release-notes" --slug release-notes \
  --version 0.1.0 --summary "把提交整理成发布说明" --manifest "${WORK}/manifest.json" >/dev/null
good "已发布 kind=skill"

say "6. 按引用核对：下载字节 → 核摘要 → 验签（第三方视角）"
OUT="$("${CLI}" verify "${REF}" 2>&1)" && R=0 || R=$?
contains "条目核对通过" "✔ 已验证" "${OUT}"
contains "说明了签名来自条目" "签名来自条目" "${OUT}"
check "退出码 0" "0" "${R}"

ITEM="$(curl -sS "${PLAT}/api/registry/${REF}")"
check "签名归一放在顶层（客户端不用认两种布局）" "${SKILL_SHA}" "$(printf '%s' "${ITEM}" | jget "['item']['signature']['sha256']")"
check "服务端把签名与产物摘要绑在一起" "True" \
  "$(printf '%s' "${ITEM}" | python3 -c "import json,sys;i=json.load(sys.stdin)['item'];print(i['signature']['sha256']==i['storage']['sha256'])")"
contains "签名声明能在条目里读回来" "ncc skill ${REF} 0.1.0" "$(printf '%s' "${ITEM}" | jget "['item']['signature']['sig']")"
contains "条目里带了公钥线索（只是线索，不是信任依据）" "minisign public key" "$(printf '%s' "${ITEM}" | jget "['item']['signature']['pubkey']")"

say "7. 服务端另一道门：签名摘要对不上这次发布的字节 ⇒ 400"
cat > "${WORK}/art/OTHER.md" <<'EOF'
另一份字节
EOF
OTHER_SHA="$(sha_of "${WORK}/art/OTHER.md")"
python3 -c "
import json
d=json.load(open('${WORK}/manifest.json'))
d['signature']['sha256']='${OTHER_SHA}'   # 签名覆盖的是别的文件 —— 这就是造假
json.dump(d, open('${WORK}/manifest-bad.json','w'))
"
OUT="$("${CLI}" publish --file "${SKILL}" --kind skill --name "lied" --slug lied \
  --manifest "${WORK}/manifest-bad.json" 2>&1)" && R=0 || R=$?
nz "摘要对不上的签名发布被拒" "${R}"
contains "拒的理由是 signature_mismatch" "signature_mismatch" "${OUT}"

say "8. 未签名的条目：如实说「未签名」，--require-signature 非 0"
cat > "${WORK}/art/mcp.json" <<'EOF'
{
  "mcpServers": {
    "notes": { "command": "ncc", "args": ["mcp"] }
  }
}
EOF
MCP="${WORK}/art/mcp.json"
"${CLI}" publish --file "${MCP}" --kind mcp --name "notes-mcp" --slug notes-mcp --version 0.1.0 >/dev/null
OUT="$("${CLI}" verify "${REF_MCP}" 2>&1)"
contains "未签名就说未签名" "⚪" "${OUT}"
contains "给了下一步" "ncc sign" "${OUT}"
OUT="$("${CLI}" verify "${REF_MCP}" --require-signature 2>&1)" && R=0 || R=$?
nz "--require-signature 在未签名时非 0" "${R}"

say "9. 事后加签：ncc sign --attach（只上传签名文件与公钥，制品字节不传）"
OUT="$("${CLI}" sign "${MCP}" --kind mcp --reference "${REF_MCP}" --version 0.1.0 --attach "${REF_MCP}" 2>&1)"
contains "加签成功" "✅ 已签名" "${OUT}"
contains "报出了加签的条目" "已加签" "${OUT}"
OUT="$("${CLI}" verify "${REF_MCP}" 2>&1)"
contains "加签后按引用可核对" "✔ 已验证" "${OUT}"
ITEM="$(curl -sS "${PLAT}/api/registry/${REF_MCP}")"
check "加签后条目顶层的签名指向同名文件" "mcp.json.minisig" \
  "$(printf '%s' "${ITEM}" | jget "['item']['signature']['name']")"
check "加签后签名文件本身也有摘要（下载方核得了）" "True" \
  "$(printf '%s' "${ITEM}" | python3 -c "import json,sys;s=json.load(sys.stdin)['item']['signature'];print(bool(s.get('sigSha256')) and bool(s.get('url')))")"

say "10. 加签不是贴标签：签名与条目字节对不上 ⇒ 本机就拒绝上传"
OUT="$("${CLI}" sign "${SKILL}" --kind skill --attach "${REF_MCP}" 2>&1)" && R=0 || R=$?
nz "把 A 的签名贴到 B 上被拒" "${R}"
contains "拒的理由说清了" "加签不是贴标签" "${OUT}"

say "11. 公钥不认识 ≠ 已验证（换一把钥匙，第二台机器的视角）"
export HUR_HOME="${TMP}/hur2"
"${CLI}" hur key gen >/dev/null 2>&1
OUT="$("${CLI}" verify "${REF}" 2>&1)"
contains "别人核不出来时如实说「公钥本机不认识」" "有签名，但公钥本机不认识" "${OUT}"
not_contains "不假称已验证" "✔ 已验证" "${OUT}"
contains "点明了这是自证" "自证" "${OUT}"
OUT="$("${CLI}" verify "${REF}" --require-signature 2>&1)" && R=0 || R=$?
nz "--require-signature 在公钥未知时非 0" "${R}"
# 自己确认过公钥就认得出来 —— 认谁是你的事，别让发布方替你决定
OUT="$("${CLI}" verify "${REF}" --pubkey "${PUBKEY}" 2>&1)"
contains "--pubkey 指定后核对通过" "✔ 已验证" "${OUT}"
contains "写明了用的是显式公钥" "显式 --pub" "${OUT}"
export HUR_HOME="${TMP}/hur"

say "12. 包不在这里签：转交给 ncc hur（两套语义不能混）"
PKG="${WORK}/pkg"
"${CLI}" hur init --dir "${PKG}" --kind agent --name "Sign Demo" >/dev/null 2>&1
OUT="$("${CLI}" sign "${PKG}" 2>&1)"
contains "目录 ⇒ 走 hur 的包签名" "已签名" "${OUT}"
contains "给出的是包的核对方式" "minisign -V -p" "${OUT}"
OUT="$("${CLI}" sign "${PKG}" --kind skill 2>&1)" && R=0 || R=$?
nz "给包加 --kind 被拒（别把包说成 artifact）" "${R}"
OUT="$("${CLI}" sign "${PKG}" --attach "${REF}" 2>&1)" && R=0 || R=$?
nz "包加签被引导到 ncc hur attach" "${R}"
contains "引导文案点名了正确的命令" "ncc hur attach" "${OUT}"

say "13. 找不到东西时不装懂"
OUT="$("${CLI}" sign "${WORK}/art/nope.md" 2>&1)" && R=0 || R=$?
nz "签不存在的文件非 0" "${R}"
contains "说清了原因与下一步" "要签的东西不存在" "${OUT}"
OUT="$("${CLI}" verify "${WORK}/art/nope.md" 2>&1)" && R=0 || R=$?
nz "核对不存在的引用非 0" "${R}"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
