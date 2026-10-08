#!/usr/bin/env bash
# `ncc feedback` 端到端冒烟：**两个服务端 + 一个 CLI**，验的就是那条链。
#
#   说一句的地方  = CLI（Agent 就在这儿跑）      → 先落本地队列，再送
#   东西放的地方  = ncc-registry 内网节点        → 就近落下（默认私有）
#   该看到的人    = ncc-platform 云端            → relay 把**公开**的搬上来，到服务提供方
#
# 验这些红线：
#   ① 先落盘再发送：目标不可达 → 命令**非 0** 退出，但那条**留在本地队列**里；
#   ② 私有的永不出机器：relay 只搬 public；
#   ③ 搬运者身份为准：hub 上 author 是搬运的人，原作者只作转述；
#   ④ 换个目标 = 换一批数据（同一份命令，天差地别的可见性）；
#   ⑤ 服务反馈落在声明了 services 的那台上，且**不替你切目标**；
#   ⑥ 内容不可改、处置只有目标拥有者能改。
#
# 全程隔离：数据目录、NCC_HOME、端口都在临时目录里，**不碰你真实的 ~/.ncc**。
# 用法：bash scripts/feedback-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
NODE_PORT="${NODE_PORT:-18570}"
PLAT_PORT="${PLAT_PORT:-18571}"
NODE="http://127.0.0.1:${NODE_PORT}"
PLAT="http://127.0.0.1:${PLAT_PORT}"
export NCC_HOME="${TMP}/home"

REPO="$(cd "${ROOT}/.." && pwd)"
REG_SRC="${REPO}/ncc-registry"
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
[[ -d "${REG_SRC}" ]] || { echo "找不到 ncc-registry（${REG_SRC}）" >&2; exit 1; }
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
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 1500)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
# 跑 ncc，捕获输出与退出码（有些断言要的就是"非 0" + 队列里留着东西）
run_cli() { set +e; OUT="$("$@" 2>&1)"; RC=$?; set -e; }
jget() { python3 -c "
import json,sys
try: d=json.load(sys.stdin)
except Exception: print(''); raise SystemExit
try: print(eval('d'+sys.argv[1]))
except Exception: print('')
" "$1"; }

say "0. 构建两个服务端（节点 + 云端）"
mkdir -p "${WORK}/bin"
( cd "${REG_SRC}/rust" && cargo build --release -q --bin ncc-registry && cp target/release/ncc-registry "${WORK}/bin/ncc-registry" )
( cd "${PLAT_SRC}" && cargo build --release -q --bin ncc-server && cp target/release/ncc-server "${WORK}/bin/ncc-server" )
good "两个服务端都构建好了"

say "1. 起节点与云端"
NCCR_PORT="${NODE_PORT}" NCCR_DATA_DIR="${TMP}/node-data" NCCR_JWT_SECRET=smoke-fb \
  NCCR_NODE_NAME="fb-node" "${WORK}/bin/ncc-registry" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }
NCC_PORT="${PLAT_PORT}" NCC_DATA_DIR="${TMP}/plat-data" NCC_JWT_SECRET=smoke-fb \
  NCC_INVITE_CODE=off "${WORK}/bin/ncc-server" >"${TMP}/plat.log" 2>&1 &
PIDS+=($!)
alive "${PLAT}/api/meta" 40 && good "云端就绪" || { bad "云端起不来（看 ${TMP}/plat.log）"; exit 1; }
check "两端都声明了 feedback 能力" "yes:yes" "$(python3 - "$NODE" "$PLAT" <<'PY'
import json,sys,urllib.request
out=[]
for base in sys.argv[1:3]:
    with urllib.request.urlopen(base+"/api/meta") as r:
        out.append("yes" if "feedback" in json.load(r)["capabilities"] else "no")
print(":".join(out))
PY
)"

say "2. 两个目标、两个账号（同一个人：内网一个身份、云端一个身份）"
"${CLI}" target add node --base "${NODE}" >/dev/null
"${CLI}" --target node register --email me@node.dev --password smoke1234 --name Me >/dev/null
"${CLI}" target add hub --base "${PLAT}" >/dev/null
"${CLI}" --target hub register --email me@hub.dev --password smoke1234 --name Me >/dev/null
# 第二个人（云端）—— 用来验"跨用户"：我说的话她能收到
"${CLI}" target use hub >/dev/null
"${CLI}" --target hub logout >/dev/null 2>&1 || true
"${CLI}" --target hub register --email other@hub.dev --password smoke1234 --name Other >/dev/null
OTHER_TOK="$(python3 -c "import json;print(json.load(open('${NCC_HOME}/.ncc/config.json'))['targets']['hub'].get('token') or '')")"
"${CLI}" target use node >/dev/null
"${CLI}" --target node register --email me@node.dev --password smoke1234 --name Me >/dev/null 2>&1 || "${CLI}" --target node login --email me@node.dev --password smoke1234 >/dev/null
check "当前目标是节点" "node" "$(python3 -c "import json;print(json.load(open('${NCC_HOME}/.ncc/config.json'))['current'])")"

say "3. 说一句：先落盘、再发送、就近落在节点上"
OUT="$("${CLI}" feedback send --about 'artifact:@me/demo-tool' --kind report \
  --body '装完后 run 报 ENOENT' --tag install --trace TR-1 --as-agent claude-code 2>&1)"
contains "送出去了（带 id）" "收下了" "${OUT}"
contains "说了落在哪台目标" "目标：node" "${OUT}"
contains "提醒了默认私有" "只有你与目标拥有者看得到" "${OUT}"
check "本地队列被清空（送成了）" "0" "$("${CLI}" feedback spool --json | jget "['spool'].__len__()")"
OUT="$("${CLI}" feedback ls --about 'artifact:@me/demo-tool' 2>&1)"
contains "看得到自己刚说的那条" "ENOENT" "${OUT}"
contains "带上了 Agent 身份" "via claude-code" "${OUT}"
check "对一个不存在的东西说 → 照记，但说清没对上" "yes" \
  "$("${CLI}" feedback send --about 'artifact:@me/ghost' --kind report --body '找不到' --json | python3 -c "
import json,sys;d=json.load(sys.stdin);print('yes' if d['resolved'] is False and '对不上' in d['note'] else 'no')")"

say "4. 先落盘再发送：目标不可达 → 非 0，但那条留在队列里"
"${CLI}" target add dead --base http://127.0.0.1:9 --use >/dev/null   # 打不通的地址
run_cli "${CLI}" feedback send --about 'artifact:@me/demo-tool' --kind report --body '这条送不出去' --to dead
check "送不出去 → 退出码非 0" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
contains "并且说清'留在队列里'" "本地队列" "${OUT}"
check "队列里真的有（1 条）" "1" "$("${CLI}" feedback spool --json | jget "['spool'].__len__()")"
contains "队列里能看到它属于哪台目标" "dead" "$("${CLI}" feedback spool 2>&1)"
"${CLI}" target use node >/dev/null
run_cli "${CLI}" feedback send --about 'artifact:@me/demo-tool' --kind report --body '离线攒着' --queue-only
check "--queue-only 只是攒着：退出码 0（不是失败）" "0" "${RC}"
check "队列变成 2 条" "2" "$("${CLI}" feedback spool --json | jget "['spool'].__len__()")"

say "5. relay：只搬公开的（私有的永不出机器）"
# 先把那两条没送出去的修好：把 dead 目标指回节点（地址**与凭据**都要换成节点的）
python3 - "${NCC_HOME}/.ncc/config.json" <<'PY'
import json,sys
p=sys.argv[1]
d=json.load(open(p))
node=d["targets"]["node"]
dead=d["targets"]["dead"]
dead["base_url"]=node["base_url"]
dead["token"]=node.get("token")
dead["email"]=node.get("email")
json.dump(d,open(p,"w"),ensure_ascii=False,indent=2)
PY
OUT="$("${CLI}" feedback relay --flush 2>&1)"
contains "flush 把队列送出去了" "送出 2 条" "${OUT}"
check "队列清空" "0" "$("${CLI}" feedback spool --json | jget "['spool'].__len__()")"
# 一条公开的（会搬上去），一条私有的（不搬）
"${CLI}" feedback send --about 'artifact:@me/demo-tool' --kind request --body '希望支持 --quiet' --public >/dev/null
OUT="$("${CLI}" feedback relay --all-mine --dry 2>&1)"
contains "--dry 只说要搬什么（列出会搬的那条）" "会把 1 条公开反馈" "${OUT}"
contains "--dry 里能看到公开的那条" "支持 --quiet" "${OUT}"
not_contains "--dry 里**看不到**私有的那条" "ENOENT" "${OUT}"
OUT="$("${CLI}" feedback relay --all-mine 2>&1)"
contains "搬上去 1 条（公开的那条）" "新落 1 条" "${OUT}"
contains "搬运说明写清了 author 是谁" "搬运者" "${OUT}"
OUT="$("${CLI}" feedback relay --all-mine 2>&1)"
contains "再搬一次：算重复（幂等，不报错）" "已经搬过 1 条" "${OUT}"
"${CLI}" target use hub >/dev/null
OUT="$("${CLI}" feedback ls --about 'artifact:@me/demo-tool' 2>&1)"
contains "云端看得到搬上来的那条" "支持 --quiet" "${OUT}"
not_contains "云端**看不到**私有的那条" "ENOENT" "${OUT}"
"${CLI}" "--target" hub feedback get "$("${CLI}" feedback ls --about 'artifact:@me/demo-tool' --json | jget "['feedback'][0]['id']")" --json \
  | python3 -c "
import json,sys
f=json.load(sys.stdin)['feedback']
print('yes' if f.get('relayed') and f.get('originAuthor') and f.get('origin')=='node' else 'no')" > /tmp/fb_relayed.txt
check "云端标了'从哪搬来的 + 原话作者'" "yes" "$(cat /tmp/fb_relayed.txt)"

say "6. 跨用户：换个人看同一台云端"
# Other 是云端另一个人：公开的看得到，私有的看不到
TK_HUB="$(python3 -c "import json;print(json.load(open('${NCC_HOME}/.ncc/config.json'))['targets']['hub']['token'])")"
check "另一个人看得到公开的那条" "1" \
  "$(curl -sS "${PLAT}/api/feedback?aboutKind=artifact&aboutRef=@me/demo-tool" -H "Authorization: Bearer ${TK_HUB}" | jget "['total']")"
check "匿名只看到公开的" "1" \
  "$(curl -sS "${PLAT}/api/feedback?aboutKind=artifact&aboutRef=@me/demo-tool" | jget "['total']")"

say "7. 服务反馈只落在声明 services 的那台（而且不替你切目标）"
"${CLI}" target use node >/dev/null
run_cli "${CLI}" feedback send --about 'service:@me/hotel' --kind report --body '订不到房'
check "节点没声明 services → 非 0" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
contains "报出该敲的命令（含 hub 目标名）" "--target hub" "${OUT}"
contains "并说明不会替你切目标" "不会替你切目标" "${OUT}"
check "当前目标仍然是 node（没被偷偷改掉）" "node" \
  "$(python3 -c "import json;print(json.load(open('${NCC_HOME}/.ncc/config.json'))['current'])")"
check "这条也没有被偷偷塞进队列" "0" "$("${CLI}" feedback spool --json | jget "['spool'].__len__()")"

say "8. 只追加：内容不可改，处置只有目标拥有者能改"
"${CLI}" target use node >/dev/null
FB_ID="$("${CLI}" feedback ls --about 'artifact:@me/demo-tool' --json | jget "['feedback'][0]['id']")"
run_cli "${CLI}" feedback status "${FB_ID}" --set resolved
check "没人能处置（owner 解析不出来时，连作者也不行）→ 非 0" "true" \
  "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
contains "报错说清了为什么" "目标拥有者" "${OUT}"
run_cli "${CLI}" feedback status "${FB_ID}" --set bogus
check "乱给状态被本地拦下（不发请求）" "true" "$([[ "${RC}" -ne 0 ]] && echo true || echo false)"
OUT="$("${CLI}" feedback reply "${FB_ID}" --body '自己也回一句' 2>&1)"
contains "回复发得出去" "回复已发" "${OUT}"
contains "并说明回复继承可见性" "继承" "${OUT}"
OUT="$("${CLI}" feedback get "${FB_ID}" 2>&1)"
contains "get 看得到回复" "自己也回一句" "${OUT}"
# 能不能处置看服务端给的那一栏（客户端只负责显示）—— owner 解析不出来时**谁都不能**。
check "get 说清了你不能处置（canResolve=false）" "no" \
  "$("${CLI}" feedback get "${FB_ID}" --json | python3 -c "
import json,sys
f=json.load(sys.stdin)['feedback']
print('no' if not f.get('canResolve') else 'yes')")"

say "9. 聚合与词表"
OUT="$("${CLI}" feedback summary --about 'artifact:@me/demo-tool' 2>&1)"
contains "聚合里有性质分布" "性质：" "${OUT}"
contains "聚合里说清了不是排序分" "不是排序用的分数" "${OUT}"
contains "聚合里有谁在说" "claude-code" "${OUT}"
"${CLI}" target use hub >/dev/null
OUT="$("${CLI}" feedback summary --about 'artifact:@me/demo-tool' 2>&1)"
contains "云端也数得出搬来的那条" "从别处搬来的" "${OUT}"
OUT="$("${CLI}" feedback kinds 2>&1)"
contains "词表离线可读" "三条红线" "${OUT}"
contains "词表还对着一跃目标确认了一份" "的声明：" "${OUT}"
check "本地队列路径在词表里" "yes" "$(printf '%s' "${OUT}" | grep -q 'spool.jsonl' && echo yes || echo no)"

say "10. Agent 面（MCP）：先问后做、说得出话、不替人处置"
# 这一节验的是**Agent 直接用的那层**（`ncc mcp` 的 stdio 面）：工具面里有什么 /
# 没什么，门到底拦不拦，说出去的话落在哪台目标上。
mcp_ask() {  # $1=方法 $2=params JSON（可空） $3=落到哪个文件
  local req
  if [[ -n "${2:-}" ]]; then
    req="$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "$1" "$2")"
  else
    req="$(printf '{"jsonrpc":"2.0","id":1,"method":"%s"}' "$1")"
  fi
  "${CLI}" mcp <<< "${req}" 2>/dev/null | head -1 > "$3"
}
mcp_call() {  # $1=工具名 $2=arguments JSON（可空） $3=落到哪个文件
  local a="${2:-}"
  [[ -n "${a}" ]] || a='{}'
  mcp_ask tools/call "$(printf '{"name":"%s","arguments":%s}' "$1" "${a}")" "$3"
}
mcp_text() {  # $1=响应文件 → 工具返回的文本
  python3 - "$1" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
out = [c.get('text', '') for c in ((d.get('result') or {}).get('content') or [])]
print('\n'.join(out))
PY
}
mcp_names() {  # $1=tools/list 的响应文件
  python3 - "$1" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print(' '.join(t['name'] for t in d['result']['tools']))
PY
}

"${CLI}" target use node >/dev/null
mcp_ask tools/list "" "${TMP}/m-tools.json"
M_NAMES="$(mcp_names "${TMP}/m-tools.json")"
contains "工具面里有「说一句」的正门" "ncc_feedback_send" "${M_NAMES}"
contains "有看反馈与聚合的口子" "ncc_feedback_summary" "${M_NAMES}"
contains "决策门也在工具面里" "ncc_rsi_check" "${M_NAMES}"
not_contains "回复不在工具面里（那是替别人说话）" "ncc_feedback_reply" "${M_NAMES}"
not_contains "改处置状态不在工具面里" "ncc_feedback_status" "${M_NAMES}"
not_contains "中继不在工具面里（把话带出机器得人点头）" "ncc_feedback_relay" "${M_NAMES}"
not_contains "学习提案 apply 不在工具面里（策略永不自动改）" "ncc_rsi_learn_apply" "${M_NAMES}"
mcp_ask initialize '{}' "${TMP}/m-init.json"
M_INIT="$(cat "${TMP}/m-init.json")"
contains "说明书里交代了默认私有" "默认私有" "${M_INIT}"
contains "说明书里交代了门只裁决不执行" "不执行任何东西" "${M_INIT}"

mcp_call ncc_rsi_check '{"command":"rm -rf /","unattended":true}' "${TMP}/m-block.json"
M_BLOCK="$(mcp_text "${TMP}/m-block.json")"
contains "危险命令 + 无人值守 → 拦住" "⛔ 拦住" "${M_BLOCK}"
contains "给的退出码就是拦住的码（调用方据此停手）" "退出码 20" "${M_BLOCK}"
contains "并明说这只是裁决、那一步没被执行" "裁决不是执行" "${M_BLOCK}"
contains "还把升级理由说清了（只会更严）" "只会更严" "${M_BLOCK}"
mcp_call ncc_rsi_check '{"command":"ls -la"}' "${TMP}/m-warn.json"
contains "普通命令 → 不拦（留痕，非 0 退出码）" "退出码 10" "$(mcp_text "${TMP}/m-warn.json")"
mcp_call ncc_rsi_check '{}' "${TMP}/m-argless.json"
contains "既不给 command 也不给 text → 明确报错（不猜）" "至少给 command" \
  "$(mcp_text "${TMP}/m-argless.json")"
M_BEFORE="$("${CLI}" rsi report --json | jget "['checks']")"
mcp_call ncc_rsi_check '{"command":"ls -la","dry":true}' "${TMP}/m-dry.json"
contains "dry=true 照样给裁决" "裁决不是执行" "$(mcp_text "${TMP}/m-dry.json")"
check "dry=true 没往账本里写（只看一眼不留痕）" "${M_BEFORE}" \
  "$("${CLI}" rsi report --json | jget "['checks']")"
mcp_call ncc_rsi_report '{"since":"24h"}' "${TMP}/m-report.json"
M_REPORT="$(mcp_text "${TMP}/m-report.json")"
contains "总账从 MCP 也给得出来" "RSI 总账" "${M_REPORT}"
contains "并且说清了账本不记什么" "不记密钥" "${M_REPORT}"
mcp_call ncc_rsi_learn_digest '{}' "${TMP}/m-learn.json"
M_LEARN="$(mcp_text "${TMP}/m-learn.json")"
contains "学习状态默认是关着的" "关着" "${M_LEARN}"
contains "并说清提案 apply 不在工具面里" "apply 不在工具面里" "${M_LEARN}"

mcp_call ncc_feedback_send \
  '{"about":"artifact:@me/demo-tool","body":"从 MCP 说一句：升级后 run 还是崩","kind":"report","tags":["mcp"],"agent":"mcp-agent"}' \
  "${TMP}/m-send.json"
M_SEND="$(mcp_text "${TMP}/m-send.json")"
contains "从 MCP 说得出去（带 id）" "收下了" "${M_SEND}"
contains "并把「默认私有」写进回执（模型知道边界）" "只有你与目标拥有者看得到" "${M_SEND}"
mcp_call ncc_feedback_list '{"about":"artifact:@me/demo-tool","mine":true}' "${TMP}/m-ls.json"
M_LS="$(mcp_text "${TMP}/m-ls.json")"
contains "从 MCP 看得到刚说的那条" "从 MCP 说一句" "${M_LS}"
contains "也带上了 Agent 身份" "mcp-agent" "${M_LS}"
mcp_call ncc_feedback_summary '{"about":"artifact:@me/demo-tool"}' "${TMP}/m-sum.json"
M_SUM="$(mcp_text "${TMP}/m-sum.json")"
contains "聚合从 MCP 也给得出来" "性质：" "${M_SUM}"
contains "并说清它不是排名分" "不是排名分" "${M_SUM}"
M_CLI="$("${CLI}" feedback ls --about 'artifact:@me/demo-tool' 2>&1)"
contains "MCP 说的那句真的落在节点上（换回 CLI 也看得到）" "从 MCP 说一句" "${M_CLI}"

say "结果"
printf '  通过 %s · 失败 %s\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" -eq 0 ]]
