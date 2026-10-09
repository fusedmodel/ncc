#!/usr/bin/env bash
# HUR profile 与数据快照包端到端冒烟。
#
# 验的是两件事：
#
#   ① **profile 决定必填项**（R12）—— 数据包不许带 entry / 网络权限，技能包不许带 entry，
#      插件包必须说清接哪个宿主。没有这一条，profile 就只是个标签。
#   ② **快照可以出门，活状态不出门** —— kb / mem / ckpt / trace 各导出一份**不可变快照**
#      （说清来源 / 时刻 / 隐私 / 许可），能签名、能发布、能灌回节点；而"导入"默认
#      只出计划，--apply 才真写。
#
# 全程隔离：节点数据、NCC_HOME、HUR_HOME、端口都在临时目录里。
# 用法：bash scripts/profile-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
PORT="${PORT:-18620}"
NODE="http://127.0.0.1:${PORT}"
export NCC_HOME="${TMP}/home"
export HUR_HOME="${TMP}/hur"

REPO="$(cd "${ROOT}/.." && pwd)"      # ncc-ai/
REG_SRC="${REPO}/ncc-connector"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc" >&2; exit 1; }
[[ -d "${REG_SRC}" ]] || { echo "找不到 ncc-connector（${REG_SRC}）" >&2; exit 1; }

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
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
nz() { if [[ "$2" != "0" ]]; then good "$1（退出码 $2）"; else bad "$1：本该失败却退出 0"; fi; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "$1"; }

say "0. 构建内网节点（三样状态住在节点上）"
mkdir -p "${WORK}/bin"
( cd "${REG_SRC}/rust" && cargo build --release -q --bin ncc-registry && cp target/release/ncc-registry "${WORK}/bin/ncc-registry" )
good "ncc-registry 已构建"

say "1. 起节点（:${PORT}，隔离数据目录）"
NCCR_PORT="${PORT}" NCCR_DATA_DIR="${TMP}/node-data" NCCR_JWT_SECRET=smoke-profile \
  NCCR_PUBLIC_URL="${NODE}" "${WORK}/bin/ncc-registry" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }

say "2. 登录 + 造点内容（kb / mem / ckpt / trace 各来一份）"
"${CLI}" --base "${NODE}" register --email seed@ncc.dev --password seed1234 --name Seed >/dev/null
"${CLI}" me >/dev/null && good "已登录"
"${CLI}" kb set hotel-faq --title "酒店 FAQ" --kind faq --content "# 常见问题

- 退房 12:00
- 早餐 7:00-10:00" --tag hotel >/dev/null
"${CLI}" kb set handbook --title "手册" --content "# 手册

本地优先。" >/dev/null
"${CLI}" mem set planner.note "先给结论再给下一步" --kind summary >/dev/null
"${CLI}" mem set style.tone "简短、不客套" --kind preference >/dev/null
printf 'state v1\n' > "${WORK}/s.bin"
"${CLI}" ckpt save --name "初始状态" --file "${WORK}/s.bin" --label handoff >/dev/null
printf '{"type":"tool","name":"kb.search","ms":35,"status":"ok"}\n' > "${WORK}/e1.jsonl"
printf '{"type":"llm","name":"plan","ms":120,"status":"ok"}\n' > "${WORK}/e2.jsonl"
"${CLI}" trace add --file "${WORK}/e1.jsonl" --events >/dev/null
"${CLI}" trace add --file "${WORK}/e2.jsonl" --events >/dev/null
good "内容就位（kb 2 · mem 2 · ckpt 1 · trace 2）"

say "3. profile 规范本身"
OUT="$("${CLI}" hur profile --list)"
contains "列得出规范里的 profile" "kb-seed" "${OUT}"
contains "标出了哪些是不可执行的数据快照" "数据快照（不可执行）" "${OUT}"
for p in agent harness plugin mcp app scaffold skill kb-seed mem-seed ckpt-set trace-set auth; do
  contains "规范里有 $p" "$p" "${OUT}"
done
OUT="$("${CLI}" hur profile --list --json)"
check "profile 表能出 JSON（给别的工具读）" "12" "$(printf '%s' "${OUT}" | python3 -c "import json,sys;print(len(json.load(sys.stdin)['profiles']))")"
contains "auth 标注成不可执行（它只被 ncc 打开）" "auth" "$(printf '%s' "${OUT}" | python3 -c "import json,sys;ps=json.load(sys.stdin)['profiles'];a=[p for p in ps if p['profile']=='auth'][0];print('auth' if not a['executable'] else 'x')")"
contains "auth 不是 data 类（免得被 hur data import 当成快照）" "auth" "$(printf '%s' "${OUT}" | python3 -c "import json,sys;ps=json.load(sys.stdin)['profiles'];a=[p for p in ps if p['profile']=='auth'][0];print('auth' if not a['data'] else 'x')")"

say "4. 知识库快照包（kb-seed）"
"${CLI}" kb bundle --namespace @seed --as-package "${WORK}/kbseed" --privacy internal --license CC-BY-4.0 >"${TMP}/kbout" 2>&1
contains "导出成功" "✅ 快照包已生成" "$(cat "${TMP}/kbout")"
check "清单里的 profile" "kb-seed" "$(python3 -c "import json;print(json.load(open('${WORK}/kbseed/hur.json'))['profile'])")"
check "data.docs 两份" "2" "$(python3 -c "import json;print(len(json.load(open('${WORK}/kbseed/hur.json'))['data']['docs']))")"
check "快照文件落在 data/ 下" "yes" "$(ls "${WORK}/kbseed/data" | tr '\n' ' ' | grep -q 'hotel-faq.md' && echo yes || echo no)"
check "锁定文件记了这两份的摘要" "yes" \
  "$(python3 -c "import json;f=json.load(open('${WORK}/kbseed/hur.lock'))['files'];print('yes' if 'data/hotel-faq.md' in f else 'no')")"
check "快照声明了来源/时刻/隐私/许可" "internal" "$(jget "['data']['privacy']" < "${WORK}/kbseed/hur.json")"
not_contains "数据包没有代码入口" "src/agent.ts" "$(cat "${WORK}/kbseed/hur.json")"

say "5. 读它：profile 四问 + 体检"
OUT="$("${CLI}" hur profile "${WORK}/kbseed")"
contains "是什么" "profile=kb-seed · 数据快照（不可执行）" "${OUT}"
contains "要什么（快照三件事）" "快照：来源 @seed/*" "${OUT}"
contains "给什么（说了它不执行代码）" "不执行代码" "${OUT}"
contains "怎么接（数据包不接宿主）" "ncc hur data import" "${OUT}"
contains "这个 profile 禁止什么" "permissions.network" "${OUT}"
contains "体检分开报" "✔ 结构 ✔ 自洽 ⚪ 签名（未签名）" "${OUT}"
not_contains "不再拿代码包的要求烦它" "建议在 src/ 下带 schema" "${OUT}"

say "6. 格式认证：R12 真的会拦（profile 是约束，不是标签）"
cp -r "${WORK}/kbseed" "${WORK}/sneaky"
python3 - <<PY
import json
p = "${WORK}/sneaky/hur.json"
d = json.load(open(p))
d["entry"] = "src/agent.ts"                       # 数据包不该有入口
d["permissions"]["network"] = ["api.evil.test"]    # 也不该自己出网
json.dump(d, open(p, "w"), ensure_ascii=False)
PY
OUT="$("${CLI}" hur verify "${WORK}/sneaky" 2>&1)" && R=0 || R=$?
nz "带入口/网络的数据包校验不过" "${R}"
contains "拦的理由是 R12" "[R12" "${OUT}"
contains "说清是数据快照不该可执行" "不允许 entry" "${OUT}"
OUT="$("${CLI}" hur data import --package "${WORK}/sneaky" --apply 2>&1)" && R=0 || R=$?
nz "不合规的包连计划都不给（--apply 也白搭）" "${R}"

say "7. 导入默认只出计划；--apply 才写"
BEFORE="$("${CLI}" kb get hotel-faq | python3 -c "import sys,re;t=sys.stdin.read();m=re.search(r'版本 v(\d+)',t);print(m.group(1) if m else '?')")"
OUT="$("${CLI}" hur data import --package "${WORK}/kbseed")"
contains "默认是计划不是写入" "没有写任何东西" "${OUT}"
contains "说清隐私级别的影响" "privacy=internal ⇒ 全部按 private 落" "${OUT}"
contains "逐份列出落点" "kb hotel-faq" "${OUT}"
AFTER="$("${CLI}" kb get hotel-faq | python3 -c "import sys,re;t=sys.stdin.read();m=re.search(r'版本 v(\d+)',t);print(m.group(1) if m else '?')")"
check "计划阶段确实没改节点上的数据" "${BEFORE}" "${AFTER}"
OUT="$("${CLI}" hur data import --package "${WORK}/kbseed" --require-signature --apply 2>&1)" && R=0 || R=$?
nz "要求签名时，未签名的快照包被拒" "${R}"
OUT="$("${CLI}" hur data import --package "${WORK}/kbseed" --apply)"
contains "加 --apply 才真写" "✅ 已导入 2 份" "${OUT}"
AFTER="$("${CLI}" kb get hotel-faq | python3 -c "import sys,re;t=sys.stdin.read();m=re.search(r'版本 v(\d+)',t);print(m.group(1) if m else '?')")"
check "内容真的写回去了（新版本号）" "$((BEFORE + 1))" "${AFTER}"
contains "内容一字不差" "退房 12:00" "$("${CLI}" kb get hotel-faq)"
check "落进去是 private（隐私级别说了算）" "私有" "$("${CLI}" kb get hotel-faq | grep -o '私有' | head -1)"

say "8. 记忆快照（mem-seed）：默认 private，只有快照能出门"
OUT="$("${CLI}" mem export --as-package "${WORK}/memseed" --license CC0-1.0)"
contains "记忆导出成包" "✅ 快照包已生成" "${OUT}"
check "默认隐私是 private" "private" "$(jget "['data']['privacy']" < "${WORK}/memseed/hur.json")"
check "每条记忆带 subject 与 key" "yes" \
  "$(python3 -c "import json;d=json.load(open('${WORK}/memseed/hur.json'))['data']['docs'];print('yes' if all(x['subject'] and x['key'] for x in d) else 'no')")"
OUT="$("${CLI}" hur data import --package "${WORK}/memseed" --apply)"
contains "灌回记忆" "✅ 已导入 2 份" "${OUT}"

say "9. 检查点集合（ckpt-set）：字节 + 血缘，逐个核摘要"
OUT="$("${CLI}" ckpt export --as-package "${WORK}/ckptseed" --license CC0-1.0 --limit 3)"
contains "检查点导出成包" "✅ 快照包已生成" "${OUT}"
check "带了血缘与粒度" "yes" \
  "$(python3 -c "import json;d=json.load(open('${WORK}/ckptseed/hur.json'))['data']['docs'][0];print('yes' if d.get('label') and 'parent' in d else 'no')")"
OUT="$("${CLI}" hur data import --package "${WORK}/ckptseed" --apply)"
contains "灌回检查点" "✅ 已导入 1 份" "${OUT}"
check "节点上多了一个点" "yes" "$("${CLI}" ckpt ls | grep -c '初始状态' | awk '{print ($1>=2)?"yes":"no"}')"

say "10. 轨迹数据集（trace-set）：默认只带摘要，full+public 直接拦"
OUT="$("${CLI}" trace export --as-package "${WORK}/traceseed" --privacy internal --payload digest --license CC0-1.0 --name "演示轨迹集" --version 1.0.0)"
contains "轨迹导出成包" "✅ 快照包已生成" "${OUT}"
check "声明了载荷级别" "digest" "$(jget "['data']['payload']" < "${WORK}/traceseed/hur.json")"
OUT="$("${CLI}" hur data import --package "${WORK}/traceseed" --apply)"
contains "轨迹进的是本机暂存（不是节点）" "已采集 1 条轨迹" "${OUT}"
contains "并指明了上传是另一个动作" "ncc trace push" "${OUT}"
OUT="$("${CLI}" trace export --as-package "${WORK}/badseed" --privacy public --payload full 2>&1)" && R=0 || R=$?
nz "「完整载荷 + 公开」被拒" "${R}"
contains "拒的理由说清了" "不合规" "${OUT}"

say "11. 发布 + 按 profile 检索（目录里认得出来）"
# 命名空间的**原始 slug**（不是 `ns list` 里给人看的那个）——发布时按它逐字比对
NS_RAW="$(python3 -c "
import json, urllib.request
cfg = json.load(open('${NCC_HOME}/.ncc/config.json'))
t = cfg['targets'][cfg['current']]
req = urllib.request.Request(t['base_url'] + '/api/namespaces/mine',
                             headers={'Authorization': 'Bearer ' + t['token']})
d = json.load(urllib.request.urlopen(req))
ns = [n for n in d['namespaces'] if n.get('type') == 'personal'] or d['namespaces']
print(ns[0]['slug'])
")"
"${CLI}" hur publish "${WORK}/kbseed" --namespace "${NS_RAW}" --slug kb-seed-demo >"${TMP}/pub" 2>&1 || true
OUT="$(cat "${TMP}/pub")"
contains "数据包也能发布（目录记成 kind=hur）" "已发布" "${OUT}"
ITEM="$(curl -sS "${NODE}/api/registry/@seed/kb-seed-demo")"
check "清单里的 profile 跟着走（别人读得到）" "kb-seed" "$(printf '%s' "${ITEM}" | jget "['item']['manifest']['profile']")"
OUT="$("${CLI}" hur profile @seed/kb-seed-demo)"
contains "远端也能读 profile" "profile=kb-seed" "${OUT}"
contains "远端读的是目录里那份声明" "kb-seed-demo@0.1.0" "${OUT}"
contains "远端能看出这是什么数据" "快照：来源" "${OUT}"
contains "远端说清了隐私级别" "隐私 internal" "${OUT}"
contains "远端给出了取用方式" "ncc hur data import" "${OUT}"
OUT="$("${CLI}" hur match --profile kb-seed)"
contains "按 profile 找得到它" "kb-seed-demo" "${OUT}"
contains "并说明了为什么召回" "profile=kb-seed" "${OUT}"
OUT="$("${CLI}" hur match --profile plugin)"
not_contains "按别的 profile 找不会误召回" "kb-seed-demo" "${OUT}"

say "12. 文件名带上 profile 段（一眼看出这是包还是快照）"
OUT="$("${CLI}" hur pack "${WORK}/kbseed" 2>&1)"
contains "产包成功" "已打包" "${OUT}"
HUR="$(ls "${WORK}/kbseed/dist/"*.hur.gz)"
if [[ "$(basename "${HUR}")" == *.kb-seed.hur.gz ]]; then good "文件名以 .kb-seed.hur.gz 结尾（profile 段 + 容器段）"; else bad "文件名没带 profile / 容器段：$(basename "${HUR}")"; fi
check "产物外层是 gzip（1f 8b）" "1f8b" "$(python3 -c "print(open('${HUR}','rb').read(2).hex())")"
[[ -f "${HUR}.sha256" ]] && good "侧车跟着新名字（后缀都往完整路径后面追加）" || bad "侧车没跟上新名字"
OUT="$("${CLI}" hur verify "${HUR}" 2>&1)"
contains "新名字校验通过" "✔" "${OUT}"
not_contains "名字与清单一致时不吭声" "文件名叫" "${OUT}"
# 改个文件名 ≠ 换身份：清单才是权威，而且**只提醒不拦**
cp "${HUR}" "${WORK}/renamed.app.hur"
OUT="$("${CLI}" hur verify "${WORK}/renamed.app.hur" 2>&1)" && R=0 || R=$?
contains "改名后仍能校验（名字不是门禁）" "✔" "${OUT}"
contains "但提醒名字与清单对不上" "但清单里是 profile=kb-seed" "${OUT}"
check "提醒本身不是失败" "0" "${R}"
cp "${HUR}" "${WORK}/no-token.hur"
OUT="$("${CLI}" hur verify "${WORK}/no-token.hur" 2>&1)"
not_contains "认不出来的名字不当成写错" "文件名叫" "${OUT}"
# 签名 / 登记这两条路以前**自己拼老文件名** —— 现在必须找得到带 profile 段的产物
"${CLI}" hur key gen >/dev/null 2>&1
"${CLI}" hur sign "${WORK}/kbseed" >/dev/null 2>&1
[[ -f "${HUR}.minisig" ]] && good "签名落在带 profile 段的产物旁边（不是找一个不存在的旧名字）" || bad "签名没落在产物旁"
"${CLI}" hur install "${HUR}" --force >/dev/null 2>&1 || true
check "本地 .hur.gz（新名字）能装进本机" "yes" \
  "$(python3 -c "
import json,glob
f=glob.glob('${NCC_HOME}/.ncc/packages/*/_install.json')
print('yes' if f else 'no')" 2>/dev/null || echo no)"
rm -rf "${NCC_HOME}/.ncc/packages"
"${CLI}" hur write "${WORK}/kbseed" --force >/dev/null 2>&1 || true
check "写工程目录会登记（只写源文件，不拷 dist）" "yes" \
  "$(python3 -c "
import json,glob
f=glob.glob('${NCC_HOME}/.ncc/packages/*/_install.json')
print('yes' if f else 'no')" 2>/dev/null || echo no)"
# 产物不在包落点里，就不该给它登记摘要（为不在场的字节签字才是坏的）
check "产物没被拷进去时，登记里不编摘要" "''" \
  "$(python3 -c "
import json,glob
f=glob.glob('${NCC_HOME}/.ncc/packages/*/_install.json')
print(repr(json.load(open(f[0])).get('sha256')) if f else 'no file')" 2>/dev/null || echo 'no file')"

say "13. 不装懂的地方"
OUT="$("${CLI}" hur data import --package "${WORK}/nope" 2>&1)" && R=0 || R=$?
nz "包不存在时非 0" "${R}"
mkdir -p "${WORK}/plain" && printf '不是包\n' > "${WORK}/plain/x.txt"
OUT="$("${CLI}" hur data import --package "${WORK}/plain" 2>&1)" && R=0 || R=$?
nz "不是包时非 0" "${R}"
OUT="$("${CLI}" hur match --profile 不存在 2>&1)" && R=0 || R=$?
nz "profile 写错时非 0" "${R}"
contains "并给出可选值" "不在规范里" "${OUT}"
# 通用模板**生不出**数据类与授权包：以前它照生成，生成物当场过不了自己的 verify
# （`[R12 错误] profile=kb-seed 必须带 data{} 声明`）—— 工具造出一个自己都不认的
# 东西，比直接拒绝更糟。现在拒绝并指明该走哪条路，且**一个文件都不落**。
mkdir -p "${WORK}/refuse"
( cd "${WORK}/refuse" && "${CLI}" hur init --profile kb-seed --name "Refused Seed" >/dev/null 2>&1 ) && R=0 || R=$?
nz "数据类 profile 不让用通用模板生成" "${R}"
REFUSE_SEED="$( cd "${WORK}/refuse" && { "${CLI}" hur init --profile kb-seed --name "Refused Seed" 2>&1 || true; } )"
contains "并指出从节点导出真快照的命令" "--as-package" "${REFUSE_SEED}"
REFUSE_AUTH="$( cd "${WORK}/refuse" && { "${CLI}" hur init --profile auth --name "Refused Auth" 2>&1 || true; } )"
contains "授权包指向 ncc auth pkg init" "ncc auth pkg init" "${REFUSE_AUTH}"
check "拒绝时一个文件都不落（不许先建目录再报错）" "" "$(ls -A "${WORK}/refuse" 2>/dev/null | tr -d '\n')"
# profile 名字写错也不许"安静地给个别的"（以前 --profile claude 会生出一份 harness 包）
( cd "${WORK}/refuse" && "${CLI}" hur init --profile claude --name "Typo" >/dev/null 2>&1 ) && R=0 || R=$?
nz "profile 名写错时非 0" "${R}"
contains "并给出可选值" "不在规范里" "$( cd "${WORK}/refuse" && { "${CLI}" hur init --profile claude --name "Typo" 2>&1 || true; } )"
check "写错时也没落任何文件" "" "$(ls -A "${WORK}/refuse" 2>/dev/null | tr -d '\n')"

say "14. 怎么接：MCP 产物必须指向**当前这个 CLI** 的 server"
# 两个 MCP 面不是一回事：独立 `hur` 二进制的面是 8 个"装配 Agent"的工具，
# `ncc hur mcp` 的面是 9 个治理工具。产物里写错命令名，宿主就会**连到另一个面**
# （或者干脆找不到可执行文件）—— 所以这里把「产物写的命令」与「server 自己广告的
# 工具」对起来验，而不是只看着像。
mkdir -p "${WORK}/hook"
( cd "${WORK}/hook" && "${CLI}" hur init --profile harness --name "Hook Probe" >/dev/null 2>&1 )
HOOK="${WORK}/hook/hook-probe"
check "生成了一份可接宿主的包" "yes" "$([[ -f "${HOOK}/hur.json" ]] && echo yes || echo no)"
OUT="$("${CLI}" hur interop "${HOOK}" --targets mcp --out "${WORK}/hook-out" --write 2>&1)"
contains "interop 写出了 .mcp.json" ".mcp.json" "${OUT}"
MCP_JSON="${WORK}/hook-out/.mcp.json"
check "产物里的命令是 ncc（不是老 hur 二进制）" "ncc" \
  "$(jget "['mcpServers']['hur']['command']" < "${MCP_JSON}")"
check "参数是 hur mcp" "hur:mcp" \
  "$(python3 -c "
import json
d=json.load(open('${MCP_JSON}'))
print(':'.join(d['mcpServers']['hur']['args']))")"
check "env 里带上了包 id（宿主知道在替哪个包装东西）" "yes" \
  "$(python3 -c "
import json
d=json.load(open('${MCP_JSON}'))
print('yes' if d['mcpServers']['hur']['env'].get('HUR_MCP_AGENT') else 'no')")"
# 一致性：产物用的命令/参数 = `ncc hur mcp --list-tools` 自己广告的那一份
check "与 ncc hur mcp --list-tools 广告的 server 一致" "ncc hur mcp" \
  "$("${CLI}" hur mcp --list-tools 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(' '.join([d['command']] + list(d['args'])))")"
# 最强的一条：**按产物里的命令真起一次**，看它答不答得出工具表、是不是同一个面
ARGS=()
while IFS= read -r a; do ARGS+=("$a"); done < <(python3 -c "
import json
d=json.load(open('${MCP_JSON}'))
print('\n'.join(d['mcpServers']['hur']['args']))")
TOOLS="$("${CLI}" "${ARGS[@]}" <<< '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' 2>/dev/null | head -1)"
check "按产物起的 server 真的答得出工具表（9 个）" "9" \
  "$(printf '%s' "${TOOLS}" | python3 -c "
import json,sys
print(len(json.load(sys.stdin)['result']['tools']))")"
contains "而且是治理面那套（hur_inspect 起头）" "hur_inspect" "${TOOLS}"
not_contains "不是老的装配面（两代工具名不许混）" "hur_install_agent" "${TOOLS}"
# 「下一步」里的参数名必须是真的（曾经写成 --target，实际是 --targets）
OUT="$("${CLI}" hur profile "${HOOK}" 2>&1)"
contains "profile 的下一步给的是 --targets（不是 --target）" "ncc hur interop . --targets" "${OUT}"
contains "且说明它渲染的是宿主能直接用的产物" "渲染成宿主能用的产物" "${OUT}"

# ---------------------------------------------------------------- 15. 创作入口
# 开发者拿到的是一个工程，不是一个仓库：`ncc hur init --kind <k>` 的 8 个 kind
# 都要**生成即通过校验**（生成物自己过不了自己的规矩，比不生成更糟）。
say '15. 创作入口：`ncc hur init --kind` 的八个 kind 都能生成、都能过校验'
DEV="${TMP}/dev"
mkdir -p "${DEV}"
SPEC_JSON="$("${CLI}" hur spec --json)"
check "spec --json 的可创作 kind 是 8 个" "8" \
  "$(printf '%s' "${SPEC_JSON}" | jget "['authorable_kinds'].__len__()")"
check "spec --json 带上了规范里的 profile 表（12 个）" "12" \
  "$(printf '%s' "${SPEC_JSON}" | jget "['profiles'].__len__()")"

for k in agent harness repo skill mcp plugin app scaffold; do
  ( cd "${DEV}" && "${CLI}" hur init --kind "$k" --name "Dev $k" --dir "k-${k}" >/dev/null 2>&1 )
  OUT="$( cd "${DEV}/k-${k}" && "${CLI}" hur verify . 2>&1 )"
  contains "${k}：生成即通过校验" "校验通过" "${OUT}"
  # 只有 `提示`（`[R13 提示] HUR.md 与清单一致`）可以出现 —— 生成器刚写完的施工说明必然一致；
  # 错误与提醒一条都不许有。
  not_contains "${k}：没有错误也没有提醒" "[R" "$(printf '%s' "${OUT}" | grep -v '\[R13 提示\]' || true)"
done

# 清单里的身份：kind 是粗分类，profile 才是权威 —— 两个都要写对
check "skill 包的 kind 记成 skill" "skill" "$(jget "['kind']" < "${DEV}/k-skill/hur.json")"
check "skill 包的 profile 记成 skill" "skill" "$(jget "['profile']" < "${DEV}/k-skill/hur.json")"
check "skill 包没有 entry（技能不是程序）" "" "$(jget "['entry']" < "${DEV}/k-skill/hur.json")"
# 这条是这次改动的核心：以前一份技能文档包里躺着一份 Agent 程序
test -f "${DEV}/k-skill/src/agent.ts" && SKILL_SRC=yes || SKILL_SRC=no
check "skill 包里没有 src/（不再塞 Agent 模板）" "no" "${SKILL_SRC}"
check "mcp 包的入口是 src/server.ts" "src/server.ts" "$(jget "['entry']" < "${DEV}/k-mcp/hur.json")"
check "plugin 包的入口是 src/plugin.ts" "src/plugin.ts" "$(jget "['entry']" < "${DEV}/k-plugin/hur.json")"
check "app 包的入口是 src/app.ts" "src/app.ts" "$(jget "['entry']" < "${DEV}/k-app/hur.json")"
check "plugin 包声明了要接的宿主" "yes" "$(python3 -c "
import json
d = json.load(open('${DEV}/k-plugin/hur.json'))
print('yes' if d['agent']['adapters'] else 'no')")"
# 脚手架模板必须落在**算包内容**的目录里（只有 src/skills/kb/data/assets 会进包）
test -f "${DEV}/k-scaffold/assets/template/README.md" && SC=yes || SC=no
check "scaffold 的模板放进了包内容目录（assets/template）" "yes" "${SC}"
# 编辑器接线：写清单时字段名 / 枚举当场就能看见错
test -f "${DEV}/k-skill/hur.schema.json" && EW=yes || EW=no
check "init 顺手落了 hur.schema.json" "yes" "${EW}"
contains "并把 .vscode/settings.json 指向它" "json.schemas" "$(cat "${DEV}/k-skill/.vscode/settings.json")"
contains "且 schema 本身带 profile 表（给 SDK 与编辑器用）" "x-hur-profiles" \
  "$("${CLI}" hur schema)"
# 生成的 MCP server 得**真跑得起来**（是骨架，不是伪代码）
if command -v node >/dev/null 2>&1; then
  MCP_DEV="$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
    | node --experimental-strip-types "${DEV}/k-mcp/src/server.ts" 2>/dev/null | head -1)"
  contains "生成的 MCP server 真答得出 initialize（node 实跑）" '"protocolVersion"' "${MCP_DEV}"
else
  echo '  （跳过 MCP 实跑：本机没有 node）'
fi
# 「下一步」按 profile 给，不是一句放之四海的 verify → sign → publish
NEXT_MCP="$( cd "${DEV}" && "${CLI}" hur init --kind mcp --name "Dev mcp2" --dir k-mcp2 2>&1 )"
contains "mcp 的下一步先让把 server 跑起来" "先本地跑通" "${NEXT_MCP}"
NEXT_SKILL="$( cd "${DEV}" && "${CLI}" hur init --kind skill --name "Dev skill2" --dir k-skill2 2>&1 )"
contains "skill 的下一步是渲染到宿主" "interop . --targets claude --write" "${NEXT_SKILL}"
NEXT_APP="$( cd "${DEV}" && "${CLI}" hur init --kind app --name "Dev app2" --dir k-app2 2>&1 )"
contains "app 的下一步是先去体检舱" "ncc app doctor" "${NEXT_APP}"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
