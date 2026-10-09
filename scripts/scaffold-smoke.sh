#!/usr/bin/env bash
# 脚手架规范（`SCAFFOLD.md`）端到端冒烟：**本地** + **真发一份到内网节点**。
#
# 验的是四件事（都是"会不会真出事"的地方）：
# 1. **只有 md**：脚手架是"我知道这类工程该长什么样"，不是一包三个月前的模板 ——
#    一份 md 就是一件制品，`kind=scaffold` 发得出去、搜得到、下得回来。
# 2. **说的与做的一致**：规则 S1~S8 里最值钱的是 S6 ——「验收」段里的命令**真能跑**
#    （`--run` 跑的就是校验时看到的那几条，不是另一份清单）。
# 3. **认栈与适配**：栈从目录里认（Cargo.toml → rust,cargo），前端算力需求
#    （`requires`）与 `ncc profile node fit` **同一套判据**，所以 fit 的结论可信。
# 4. **随包分发**：`HUR.md`（施工说明）与 `SCAFFOLD.md`（本体）都进 `hur.lock`、都进产物 ——
#    包被接手时规范知识还在现场；改完 id / version 不重新生成，R13 会点名。
#
# 全程隔离：NCC_HOME / HUR_HOME / 节点数据目录 / 端口都在临时目录里，
# **不碰你真实的 ~/.ncc、~/.harnessuse 与节点库**。
# 用法：bash scripts/scaffold-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
mkdir -p "${WORK}"
PORT="${PORT:-18595}"
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
nz() { if [[ "${2:-0}" != "0" ]]; then good "$1（退出码 ${2}）"; else bad "$1：本该失败却退出 0"; fi; }
notcontains() { if [[ "$3" != *"$2"* ]]; then good "$1"; else bad "$1：本不该出现「$2」"; fi; }
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print(eval(sys.argv[1],{'d':d}))" "$2" 2>/dev/null <<<"$1"; }
sha_of() { python3 -c "import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())" "$1"; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}

# ── 1. 栈从目录里认出来 ───────────────────────────────────────────────
say "1. init：栈从目录里认（Cargo.toml → rust,cargo），认出来就把前置与验收一起写上"
PROJ="${WORK}/notes-cli"
mkdir -p "${PROJ}/src"
printf '[package]\nname = "notes-cli"\nversion = "0.1.0"\n\n[[bin]]\nname = "notes-cli"\npath = "src/main.rs"\n' > "${PROJ}/Cargo.toml"
printf 'fn main() {}\n' > "${PROJ}/src/main.rs"
OUT="$("${CLI}" scaffold init "${PROJ}" --name notes-cli --desc "带笔记能力的 CLI 骨架" 2>&1)"
contains "生成了脚手架" "已生成" "${OUT}"
contains "认出了栈" "Cargo.toml · src/notes_cli.rs · README.md" "${OUT}"
contains "前置算力按栈自动写了" "tool:cargo" "${OUT}"
contains "验收命令按栈自动写了" "cargo build" "${OUT}"
[[ -f "${PROJ}/SCAFFOLD.md" ]] && good "SCAFFOLD.md 落盘" || bad "没有 SCAFFOLD.md"

# 它必须能过校验（生成器给出的东西自己先要站得住）
OUT="$("${CLI}" scaffold verify "${PROJ}" 2>&1)" && R=0 || R=$?
check "新生成的脚手架自身通过 S1~S8" "0" "${R}"

say "2. verify --run：真跑「验收」里的命令（说的与做的必须是一条）"
OUT="$("${CLI}" scaffold verify "${PROJ}" --run 2>&1)" && R=0 || R=$?
check "验收命令真能跑过" "0" "${R}"
contains "跑了 cargo build" '$ cargo build' "${OUT}"

# 反例：验收命令写错 → 校验必须红（脚手架不许带着红验收发布）
cp "${PROJ}/SCAFFOLD.md" "${WORK}/scaffold.bak"
python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
open(p,'w').write(s.replace("cargo build", "cargo build --this-flag-does-not-exist"))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" --run 2>&1)" && R=0 || R=$?
nz "验收命令跑不过 ⇒ 校验不过" "${R}"
contains "说了为什么" "退出码" "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

say "3. S1~S8：每条规则都要挡得住它的那类坏样例（每次改一处、验一次、再还原）"
python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
open(p,'w').write(s.replace("ncc-scaffold/v1", "whatever/v9"))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" 2>&1)" && R=0 || R=$?
nz "S1：规范号不对 ⇒ 不过" "${R}"
contains "点名 S1" "S1" "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
open(p,'w').write(s.replace("## 验收", "## 说明"))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" 2>&1)" && R=0 || R=$?
nz "S6：没有「验收」段 ⇒ 不过" "${R}"
contains "点名 S6" "S6" "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
open(p,'w').write(s.replace("## 目标结构", "## 目录"))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" 2>&1)" && R=0 || R=$?
nz "S5：没有「目标结构」段 ⇒ 不过" "${R}"
contains "点名 S5" "S5" "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
open(p,'w').write(s.replace("cargo build", 'export AWS_KEY="AKIAIOSFODNN7EXAMPLE"'))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" 2>&1)" && R=0 || R=$?
nz "S7：正文里出现密钥 ⇒ 不过" "${R}"
contains "点名 S7" "S7" "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
open(p,'w').write(s.replace("requires: [tool:cargo]", "requires: [把 cargo 装好]"))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" 2>&1)" && R=0 || R=$?
nz "S8：requires 不是合法需求表达式 ⇒ 不过" "${R}"
contains "点名 S8" "S8" "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

say '4. 围栏里的 # 注释 不许把「验收」段截断（实测踩过的坑）'
python3 - "${PROJ}/SCAFFOLD.md" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
# 注释在前、真命令在后：截断的话就会读成"没有可执行命令"
open(p,'w').write(s.replace("cargo build", "# 先把工作区编一遍\n# 注释也是命令行的一部分\ncargo build"))
PY
OUT="$("${CLI}" scaffold verify "${PROJ}" --run 2>&1)" && R=0 || R=$?
check "带注释的验收块照样跑得起来" "0" "${R}"
contains "两条注释后的真命令被执行" '$ cargo build' "${OUT}"
cp "${WORK}/scaffold.bak" "${PROJ}/SCAFFOLD.md"

say "5. fit：requires 折成需求，与本机算力画像同源判定"
OUT="$("${CLI}" scaffold fit "${PROJ}" --scan 2>&1)" && R=0 || R=$?
check "本机能跑这份脚手架" "0" "${R}"
contains "逐条给了判据" "tool:cargo" "${OUT}"
OUT="$("${CLI}" scaffold fit "${PROJ}" --need "cpu>=9999" --json)"
check "json 里 ok=false" "False" "$(jget "${OUT}" "d['ok']")"
contains "缺什么也说清了" "需要 cpu >= 9999" "${OUT}"
OUT="$("${CLI}" scaffold fit "${PROJ}" --need "cpu>=9999" --check 2>&1)" && R=0 || R=$?
nz "--check：不满足 ⇒ 退出码 1" "${R}"
OUT="$("${CLI}" scaffold fit "${PROJ}" --need "cpu>=9999" 2>&1)" && R=0 || R=$?
check "不加 --check 时只报告、不当作失败" "0" "${R}"

say "6. spec：取值有一处权威（工具与文档都从这里来）"
OUT="$("${CLI}" scaffold spec --json)"
check "规范号" "ncc-scaffold/v1" "$(jget "${OUT}" "d['spec']")"
check "制品名" "SCAFFOLD.md" "$(jget "${OUT}" "d['file']")"
check "规则条数" "8" "$(jget "${OUT}" "len(d['rules'])")"
check "字段里有 requires" "True" "$(jget "${OUT}" "'requires' in [f['field'] for f in d['fields']]")"
check "模板里带验收段" "True" "$(jget "${OUT}" "'## 验收' in d['template']")"

say "7. 包线：SCAFFOLD.md 是本体（进 lock、进产物），HUR.md 是施工说明"
PKG="${WORK}/scaffold-pkg"
OUT="$("${CLI}" hur init --kind scaffold --name "Notes 脚手架" --summary "带笔记能力的 CLI 骨架" --dir "${PKG}" 2>&1)"
contains "生成了包工程" "已生成" "${OUT}"
contains "顺带写了施工说明" "HUR.md" "${OUT}"
[[ -f "${PKG}/HUR.md" ]] && good "HUR.md 在包目录里" || bad "没有 HUR.md"
[[ -f "${PKG}/SCAFFOLD.md" ]] && good "SCAFFOLD.md 在包目录里" || bad "没有 SCAFFOLD.md"
# 认不出栈的目录（包里没有 Cargo.toml）→ 如实写 generic，并把验收留成 TODO（S6 会点名）
contains "认不出栈就如实说、并点名待补" "S6" "${OUT}"
# `requires: []` 是合法的：fit 要如实回答"没前置要求"，不能拿空表达式报错
OUT="$("${CLI}" scaffold fit "${PKG}" 2>&1)" && R=0 || R=$?
check "没声明 requires 也答得出来" "0" "${R}"
contains "如实说它没声明前置" "没声明 requires" "${OUT}"

OUT="$("${CLI}" hur spec-kit "${PKG}" 2>&1)"
contains "施工说明讲清了改动时要守住什么" "改这份包时要守住的事" "${OUT}"
contains "施工说明里带着命令面" "ncc hur verify" "${OUT}"
contains "规则表在里面" "R13" "${OUT}"
OUT="$("${CLI}" hur spec-kit "${PKG}" --write 2>&1)"
contains "写进包目录" "已写入" "${OUT}"
OUT="$("${CLI}" hur verify "${PKG}" 2>&1)" && R=0 || R=$?
check "含 HUR.md 的包照样过 R1~R13" "0" "${R}"
contains "R13 核对了一致性" "R13" "${OUT}"

# R13 的漂移：改 version 不重新生成 → 提醒；重新生成 → 恢复
python3 - "${PKG}/hur.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d["version"]="0.2.0"; json.dump(d,open(p,'w'),ensure_ascii=False,indent=2)
PY
"${CLI}" hur build "${PKG}" >/dev/null 2>&1
OUT="$("${CLI}" hur verify "${PKG}" 2>&1)"
contains "版本变了没重新生成 ⇒ R13 点名" "R13" "${OUT}"
contains "并且说清怎么重新生成" "spec-kit" "${OUT}"
"${CLI}" hur spec-kit "${PKG}" --write >/dev/null 2>&1
"${CLI}" hur build "${PKG}" >/dev/null 2>&1
OUT="$("${CLI}" hur verify "${PKG}" 2>&1)" && R=0 || R=$?
check "重新生成后恢复" "0" "${R}"

"${CLI}" hur pack "${PKG}" >/dev/null 2>&1
ART2="$(ls "${PKG}"/dist/*.hur.gz | head -1)"
[[ -n "${ART2}" ]] && good "打出了产物 ${ART2##*/}" || bad "没有产物"
OUT="$("${CLI}" hur ls "${ART2}" 2>&1)"
contains "产物里有施工说明（随包分发）" "HUR.md" "${OUT}"
contains "产物里有脚手架本体" "SCAFFOLD.md" "${OUT}"
OUT="$("${CLI}" hur verify "${ART2}" 2>&1)" && R=0 || R=$?
check "产物校验通过" "0" "${R}"

say "8. 起节点（:${PORT}，隔离数据目录）"
( cd "${NODE_SRC}" && cargo build -q 2>/dev/null )
NODE_BIN="$(ls -t "${NODE_SRC}"/target/debug/ncc-registry 2>/dev/null | head -1)"
[[ -n "${NODE_BIN}" ]] || { bad "节点没构建出来"; exit 1; }
NCCR_DATA_DIR="${TMP}/node-data" NCCR_PORT="${PORT}" NCCR_JWT_TTL=1h \
  "${NODE_BIN}" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }
OUT="$(curl -fsS "${NODE}/api/registry/kinds")"
contains "目录里认 scaffold 这个 kind" '"scaffold"' "${OUT}"

say "9. 发布 + 检索 + 下回来（脚手架就是一份 md：字节必须一致）"
"${CLI}" --base "${NODE}" register --email scaffer@ncc.dev --password scaffer123 --name Scaffer >/dev/null
MD_SHA="$(sha_of "${PROJ}/SCAFFOLD.md")"
OUT="$("${CLI}" --base "${NODE}" publish --kind scaffold --name "notes-cli 脚手架" --slug notes-cli \
  --version 0.1.0 --summary "带笔记能力的 CLI 骨架" --file "${PROJ}/SCAFFOLD.md" 2>&1)"
contains "发布成功" "已发布" "${OUT}"
OUT="$("${CLI}" --base "${NODE}" search --kind scaffold 2>&1)"
contains "搜得到 kind=scaffold" "notes-cli" "${OUT}"
DL="${WORK}/downloaded-SCAFFOLD.md"
OUT="$("${CLI}" --base "${NODE}" download "@scaffer/notes-cli" -o "${DL}" 2>&1)"
contains "下载成功" "已保存" "${OUT}"
check "下回来的字节与本地一致" "${MD_SHA}" "$(sha_of "${DL}")"
OUT="$("${CLI}" scaffold verify "${DL}" 2>&1)" && R=0 || R=$?
check "下回来的脚手架还能过 S1~S8" "0" "${R}"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
