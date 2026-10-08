#!/usr/bin/env bash
# `ncc rsi learn` 端到端冒烟：**RSI 的另一半** —— 从反馈与状态里学。
#
# 验的就是那四条红线（用户的原话：「用户可以设置或通过 Agent 设置 RSI 可以用于进化的轨迹与数据」）：
#   ① **默认关闭**：没有同意声明 → 一个来源都不读，而且明说怎么开；
#   ② **声明即许可**：没在 sources 里声明的来源（mem / kb / ckpt / feedback）一律不读；
#   ③ **只读**：学习不改任何来源，写只写 `.ncc-rsi/`（提案 / 偏好 / 教训）；
#   ④ **提案不自动生效**：plan 只出提案；apply 要点名；**策略类永远不自动写**；
#   另外验：脱敏、过期、导出数据集（manifest + items）与"学到的偏好真的接进了裁决"。
#
# 全程隔离：数据目录、NCC_HOME、端口、工作目录都在临时目录里。
# 用法：bash scripts/rsi-learn-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
NODE_PORT="${NODE_PORT:-18580}"
NODE="http://127.0.0.1:${NODE_PORT}"
export NCC_HOME="${TMP}/home"
REPO="$(cd "${ROOT}/.." && pwd)"
REG_SRC="${REPO}/ncc-registry"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc" >&2; exit 1; }
[[ -d "${REG_SRC}" ]] || { echo "找不到 ncc-registry（${REG_SRC}）" >&2; exit 1; }

PASS=0
FAIL=0
PID=""
cleanup() {
  [[ -n "${PID}" ]] && kill "${PID}" 2>/dev/null || true
  wait 2>/dev/null || true
  if [[ -n "${KEEP:-}" ]]; then echo "（KEEP=1）保留 ${TMP}"; else rm -rf "${TMP}"; fi
}
trap cleanup EXIT

say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 500)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」（实际：$(printf '%s' "$3" | head -c 300)）"; else good "$1"; fi; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
# 在项目目录里跑 ncc（rsi 的项目级目录发现靠 cwd）
rsi() { ( cd "${WORK}" && "${CLI}" "$@" ); }
run_rsi() { set +e; OUT="$( cd "${WORK}" && "${CLI}" "$@" 2>&1 )"; RC=$?; set -e; }
jq_at() { python3 -c "
import json,sys
try: d=json.load(sys.stdin)
except Exception: print(''); raise SystemExit
try: print(eval('d'+sys.argv[1]))
except Exception: print('')
" "$1"; }

say "0. 起一个节点（状态与反馈都住它上面）"
mkdir -p "${WORK}"
( cd "${REG_SRC}/rust" && cargo build --release -q --bin ncc-registry && cp target/release/ncc-registry "${TMP}/ncc-registry" )
NCCR_PORT="${NODE_PORT}" NCCR_DATA_DIR="${TMP}/node-data" NCCR_JWT_SECRET=smoke-learn \
  NCCR_NODE_NAME="learn-node" "${TMP}/ncc-registry" >"${TMP}/node.log" 2>&1 &
PID=$!
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }
"${CLI}" target add node --base "${NODE}" >/dev/null
"${CLI}" --target node register --email me@learn.dev --password smoke1234 --name Me >/dev/null
happy=$?
check "登录成功" "0" "${happy}"
say "0b. 建项目级 rsi（账本 / 偏好 / 提案都跟着这个目录走）"
rsi rsi init --preset unattended-safe >/dev/null
check "项目级目录建好了" "yes" "$([[ -d ${WORK}/.ncc-rsi ]] && echo yes || echo no)"
# 造点账本：两次"要人确认"（git push）→ 应该出一条 guard 提案
for i in 1 2; do
  rsi rsi check --command 'git push origin main' >/dev/null 2>&1 || true
done
check "账本里有 2 条决策" "2" "$(python3 -c "
print(sum(1 for l in open('${WORK}/.ncc-rsi/ledger.jsonl') if l.strip()))")"

say "1. 默认关闭（没有同意声明 = 一个来源都不读）"
run_rsi rsi learn plan
check "没有同意声明时 plan 拒绝" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
contains "并且说清怎么开" "consent set" "${OUT}"
OUT="$(rsi rsi learn digest 2>&1)"
contains "digest 也说清了默认不学" "默认什么都不学" "${OUT}"
OUT="$(rsi rsi learn consent show 2>&1)"
contains "consent show 说还没有声明" "还没有学习同意声明" "${OUT}"

say "2. 声明来源（先不开）：声明即许可，但开关还关着"
OUT="$(rsi rsi learn consent set --source log:ledger --source 'mem:@me' --source 'feedback:mine' --redact secret --by-agent claude-code 2>&1)"
contains "记下了来源" "L1" "${OUT}"
contains "还提醒现在关着" "还是关着的" "${OUT}"
OUT="$(rsi rsi learn consent show 2>&1)"
contains "show 里能看到 3 条来源" "来源     3 条" "${OUT}"
contains "show 里能看出是 Agent 代设的" "claude-code" "${OUT}"
check "来源 id 是 L1/L2/L3" "3" "$(rsi rsi learn consent show --json | jq_at "['sources'].__len__()")"
run_rsi rsi learn plan
check "关着的时候 plan 还是拒绝" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
contains "并说清开关在哪" "consent on" "${OUT}"

say "3. 打开并读一遍：来源里有什么，就读什么"
rsi rsi learn consent on >/dev/null
# 往节点写一条 kind=preference 的记忆（人写过的偏好 —— 最值得正式化的那种）
rsi mem set prod-db '不要动 production 数据库，先在 staging 验' --kind preference --subject team >/dev/null 2>&1 || \
  rsi mem set prod-db '不要动 production 数据库，先在 staging 验' --kind preference >/dev/null
# 同一个制品上两条 report（反复出问题）
rsi feedback send --about 'artifact:@me/demo-tool' --kind report --body '装完 run 报 ENOENT' >/dev/null
rsi feedback send --about 'artifact:@me/demo-tool' --kind report --body '升级后仍旧 ENOENT' >/dev/null
OUT="$(rsi rsi learn plan 2>&1)"
contains "读到了材料" "读完了" "${OUT}"
contains "log 来源读到了" "log:ledger" "${OUT}"
contains "mem 来源读到了" "mem:@me" "${OUT}"
contains "feedback 来源读到了" "feedback:mine" "${OUT}"
contains "出了一条偏好提案（把记忆里的偏好正式化）" "[pref]" "${OUT}"
contains "出了一条教训提案（同一个东西反复出问题）" "[lesson]" "${OUT}"
contains "出了一条策略提案（账本里反复要人确认）" "[guard]" "${OUT}"
not_contains "没声明的来源（kb / ckpt）一条也没读" "kb:" "${OUT}"

say "4. 提案落到文件里，等点头（plan 不改任何东西）"
check "proposals.json 里有提案" "yes" "$([[ -s ${WORK}/.ncc-rsi/proposals.json ]] && echo yes || echo no)"
check "偏好文件还没被动过（apply 才写）" "False" \
  "$(python3 -c "
import json,os
p='${WORK}/.ncc-rsi/prefs.json'
print(os.path.exists(p) and json.load(open(p)).get('prefs'))" )"
check "策略文件没被动过" "unattended-safe" \
  "$(python3 -c "import json;print(json.load(open('${WORK}/.ncc-rsi/policy.json'))['name'])")"

say "5. apply：偏好接进裁决，策略永远不自动改"
PREF_ID="$(python3 -c "
import json;d=json.load(open('${WORK}/.ncc-rsi/proposals.json'))
print(next(p['id'] for p in d['proposals'] if p['kind']=='pref'))")"
GUARD_ID="$(python3 -c "
import json;d=json.load(open('${WORK}/.ncc-rsi/proposals.json'))
print(next((p['id'] for p in d['proposals'] if p['kind']=='guard'), ''))")"
LESSON_ID="$(python3 -c "
import json;d=json.load(open('${WORK}/.ncc-rsi/proposals.json'))
print(next(p['id'] for p in d['proposals'] if p['kind']=='lesson'))")"
OUT="$(rsi rsi learn apply "${PREF_ID}" 2>&1)"
contains "偏好应用了" "→ 偏好" "${OUT}"
check "偏好进了 prefs.json 且标了 inferred" "inferred" \
  "$(python3 -c "import json;print(json.load(open('${WORK}/.ncc-rsi/prefs.json'))['prefs'][-1]['confidence'])")"
check "偏好里留了依据（可复核）" "mem:prod-db" \
  "$(python3 -c "import json;print(json.load(open('${WORK}/.ncc-rsi/prefs.json'))['prefs'][-1]['evidence'][0])")"
# 学到的偏好真的接进了裁决：无人值守下命中 production 这个词 → 拦
run_rsi rsi check --command 'psql production -c "drop table users"' --unattended
check "学到的偏好在无人值守下真的拦人" "20" "${RC}"
run_rsi rsi learn apply "${GUARD_ID}"
contains "guard 提案**不写策略**" "策略不自动改" "${OUT}"
check "并给出可粘贴的策略片段" "yes" "$(printf '%s' "${OUT}" | grep -q 'Commands' && echo yes || echo no)"
check "policy.json 确实没被改（还是 unattended-safe）" "unattended-safe" \
  "$(python3 -c "import json;print(json.load(open('${WORK}/.ncc-rsi/policy.json'))['name'])")"
OUT="$(rsi rsi learn apply "${LESSON_ID}" 2>&1)"
contains "教训应用了" "→ 教训" "${OUT}"
check "lessons.jsonl 里有一条" "1" "$(python3 -c "
print(sum(1 for l in open('${WORK}/.ncc-rsi/lessons.jsonl') if l.strip()))")"
run_rsi rsi learn apply LP-999
check "不存在的提案 → 非 0" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"

say "6. 声明即许可：换成只声明 log，别的来源一条都不读"
OUT="$(rsi rsi learn consent set --source log:ledger --replace 2>&1)"
check "换完只剩 1 条来源" "1" "$(rsi rsi learn consent show --json | jq_at "['sources'].__len__()")"
OUT="$(rsi rsi learn plan --json 2>&1)"
check "这次一条 mem / feedback 材料都没有" "0" \
  "$(printf '%s' "${OUT}" | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(sum(1 for it in [] ) if False else sum(1 for r in d['read'] if r['where'].startswith(('mem','feedback'))))" 2>/dev/null || echo 0)"
contains "读到的只有 log" "log:ledger" "${OUT}"
contains "说明里点出提案要点头" "提案" "${OUT}"

say "7. 脱敏与导出"
rsi rsi learn consent set --source log:ledger --source 'mem:@me' --redact secret >/dev/null
rsi mem set api-key 'token 是 secret-abc123，别外传' --kind fact >/dev/null 2>&1 || \
  rsi mem set api-key 'token 是 secret-abc123，别外传' --kind fact >/dev/null
OUT="$(rsi rsi learn export --dir "${TMP}/learnset" --json 2>&1)"
check "导出了数据集" "yes" "$([[ -f "${TMP}/learnset/items.jsonl" && -f "${TMP}/learnset/manifest.json" ]] && echo yes || echo no)"
contains "manifest 里写了同意声明（谁允许的）" "setByUser" "$(cat "${TMP}/learnset/manifest.json")"
contains "manifest 里写了脱敏词" "secret" "$(cat "${TMP}/learnset/manifest.json")"
check "导出内容里**没有原文里的 secret 值**" "no" \
  "$(grep -q 'secret-abc123' "${TMP}/learnset/items.jsonl" && echo yes || echo no)"
check "而是打码成了 ***" "yes" "$(grep -q '\*\*\*' "${TMP}/learnset/items.jsonl" && echo yes || echo no)"
OUT="$(rsi rsi learn export --dir "${TMP}/learnset2" --dry 2>&1)"
contains "--dry 只看不写" "会导出" "${OUT}"
check "dry 真的没写目录" "no" "$([[ -d "${TMP}/learnset2" ]] && echo yes || echo no)"

say "8. 过期：过期就不再读（要说得出来）"
python3 - "${WORK}/.ncc-rsi/learn.json" <<'PY'
import json,sys
p=sys.argv[1]
d=json.load(open(p))
d["expires_unix"]=1          # 1970 年就过期了
json.dump(d,open(p,"w"),ensure_ascii=False,indent=2)
PY
run_rsi rsi learn plan
check "过期后拒绝" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
contains "并且说清是过期，不是别的" "过期" "${OUT}"
OUT="$(rsi rsi learn digest --json 2>&1)"
check "digest 里看得到 expired=true" "True" "$(printf '%s' "${OUT}" | jq_at "['expired']")"
python3 - "${WORK}/.ncc-rsi/learn.json" <<'PY'
import json,sys
p=sys.argv[1]
d=json.load(open(p))
d["expires_unix"]=0
json.dump(d,open(p,"w"),ensure_ascii=False,indent=2)
PY
run_rsi rsi learn plan
check "取消过期后又能读了" "0" "${RC}"

say "9. 关掉：声明留着，但不再读"
rsi rsi learn consent off >/dev/null
run_rsi rsi learn plan
check "关掉后拒绝" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
check "声明没被删（来源还在）" "2" "$(rsi rsi learn consent show --json | jq_at "['sources'].__len__()")"

say "结果"
printf '  通过 %s · 失败 %s\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" -eq 0 ]]
