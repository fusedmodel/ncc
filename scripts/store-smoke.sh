#!/usr/bin/env bash
# 「通用记录仓」（`ncc store`）端到端冒烟。
#
# 验的是这条链路：**声明一个集合 = 新增一类内容，服务端一行不用改**。
#
#   ① 声明 → 写 → 读 → 改 → 历史 → 归档，全走 CLI（客户端是手，节点是家）
#   ② 四条边界真的挡住了：动态≠无模式 / 不可变就是不可变 / CRUD≠授权 / 归档≠删除
#   ③ 客户端**先读声明再发请求**：没声明的字段、越界的枚举、写错类型
#      在本地就被拒（省一次往返，错误也说在人近处）
#   ④ 包里能声明它要用哪些集合（`state.stores[]`），`nur profile` 四问要如实显示，
#      快照包不许声明写（R12）
#
# 全程隔离：节点数据、NCC_HOME、端口都在临时目录里。
# 用法：bash scripts/store-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
PORT="${PORT:-18630}"
NODE="http://127.0.0.1:${PORT}"
export NCC_HOME="${TMP}/home"

REPO="$(cd "${ROOT}/.." && pwd)"
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
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 400)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
nz() { if [[ "$2" != "0" ]]; then good "$1（退出码 $2）"; else bad "$1：本该失败却退出 0"; fi; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
out() { printf '%s' "$1" | tail -n 40; }

say "0. 离线那部分（不联网就该能用）"
OUT="$("${CLI}" store kinds 2>&1)"
contains "词表可离线看" "类型白名单" "${OUT}"
contains "三条不变量写清了" "没在 --field 里声明的字段" "${OUT}"
contains "也说清了上限" "256KB" "${OUT}"

say "1. 构建并起节点（:${PORT}，隔离数据目录）"
mkdir -p "${WORK}/bin"
( cd "${REG_SRC}/rust" && cargo build --release -q --bin ncc-registry && cp target/release/ncc-registry "${WORK}/bin/ncc-registry" )
good "ncc-registry 已构建"
NCCR_PORT="${PORT}" NCCR_DATA_DIR="${TMP}/node-data" NCCR_JWT_SECRET=smoke-store \
  NCCR_PUBLIC_URL="${NODE}" "${WORK}/bin/ncc-registry" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }
contains "节点自己声明了 store 能力" '"store"' "$(curl -fsS "${NODE}/api/meta")"

say "2. 登录"
"${CLI}" --base "${NODE}" register --email store@ncc.dev --password store1234 --name Store >/dev/null
"${CLI}" me >/dev/null && good "已登录"
NS="$("${CLI}" me | awk '/👤/ {print $2; exit}')"
[[ -n "${NS}" ]] && good "个人命名空间 @${NS}" || { bad "拿不到命名空间"; exit 1; }

say "3. 声明集合 —— 这就是「新增一类内容」的全部动作"
OUT="$("${CLI}" store declare issue \
  --title "问题单" --summary "用户报的问题与处理进展" \
  --field 'title:string!' --field 'status:enum:open|closed|triaged' \
  --field 'severity:enum:high|mid|low' --field 'labels:string[]' \
  --field 'owner:ref' --field 'body:text?search' \
  --index status,severity,labels 2>&1)"
contains "声明成功" "已声明 集合" "${OUT}"
contains "说清了它是可变集合" "可变" "${OUT}"
contains "给出了下一步怎么用" "ncc store put issue" "${OUT}"
OUT="$("${CLI}" store declare audit --title "审计流水" --append-only \
  --field 'at:string!' --field 'ok:bool' --index ok 2>&1)"
contains "第二类内容也是声明出来的（服务端没动）" "已声明" "${OUT}"
OUT="$("${CLI}" store ls 2>&1)"
contains "列得出集合" "issue" "${OUT}"
contains "审计集合在" "audit" "${OUT}"
contains "不可变的一眼可见" "不可变" "${OUT}"

say "4. 写 / 读 / 改 / 历史"
OUT="$("${CLI}" store put issue ISS-1 --body "登录页点不动，复现 3/3" \
  --field 'title=登录按钮无响应' --field 'status=open' --field 'severity=high' \
  --field 'labels=bug,web' --field "owner=@${NS}" --note "首报" 2>&1)"
contains "建了一条" "已建" "${OUT}"
contains "回显了版本" "rev 1" "${OUT}"
OUT="$("${CLI}" store get issue ISS-1 2>&1)"
contains "取得回正文" "登录页点不动" "${OUT}"
contains "字段也在" "登录按钮无响应" "${OUT}"
OUT="$("${CLI}" store put issue ISS-1 --body "登录页点不动，复现 3/3（已定位）" \
  --field 'title=登录按钮无响应' --field 'status=triaged' --field 'severity=high' \
  --field 'labels=bug,web' --field "owner=@${NS}" --revision 1 --note "定位到事件绑定" 2>&1)"
contains "改一条（带上版本号 = 不是静默覆盖）" "已更新" "${OUT}"
contains "版本 +1" "rev 2" "${OUT}"
OUT="$("${CLI}" store history issue ISS-1 2>&1)"
contains "历史里有上一版" "rev 1" "${OUT}"
contains "历史说明了备注" "首报" "${OUT}"
contains "历史只记元数据" "当前 rev 2" "${OUT}"

say "5. 客户端先读声明，再发请求（错在本地就拦下）"
( "${CLI}" store put issue ISS-9 --field 'nope=1' >"${TMP}/o1" 2>&1 ) && R=0 || R=$?
nz "没声明的字段写不进（本地就拒）" "${R}"
OUT="$(cat "${TMP}/o1")"
contains "并告诉它该往哪放" "--meta" "${OUT}"
( "${CLI}" store put issue ISS-9 --field 'status=urgent' >"${TMP}/o2" 2>&1 ) && R=0 || R=$?
nz "枚举越界被拒" "${R}"
contains "并且列出词表" "open / closed / triaged" "$(cat "${TMP}/o2")"
( "${CLI}" store put issue ISS-9 --field 'severity=3.5' >"${TMP}/o3" 2>&1 ) && R=0 || R=$?
nz "类型不对被拒（不会替你瞎猜）" "${R}"
( "${CLI}" store put issue ISS-9 --body x >"${TMP}/o4" 2>&1 ) && R=0 || R=$?
nz "必填项缺失在本地就说清" "${R}"
contains "并说清是哪个字段" "title" "$(cat "${TMP}/o4")"

say "6. 不可变就是不可变（没有改这条路）"
"${CLI}" store put audit a-1 --body "2026-09-28T10:00 open" --field 'at=2026-09-28T10:00' --field 'ok=true' >/dev/null
( "${CLI}" store put audit a-1 --body "改一改" --field 'at=2026-09-28T10:00' --field 'ok=false' >"${TMP}/o5" 2>&1 ) && R=0 || R=$?
nz "只追加集合改不了" "${R}"
contains "拒的时候说清了为什么" "只追加" "$(cat "${TMP}/o5")"
OUT="$("${CLI}" store put audit a-2 --body "2026-09-28T10:05 done" --field 'at=2026-09-28T10:05' --field 'ok=true' 2>&1)"
contains "新 key 照样能追加" "已建" "${OUT}"

say "7. 过滤：只在声明的字段上，q 只是子串"
check "按声明字段过滤命中" "1" "$("${CLI}" store list issue --where status=triaged --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
check "不误命中" "0" "$("${CLI}" store list issue --where 'status=open' --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
check "数组字段按元素过滤" "1" "$("${CLI}" store list issue --where 'labels=web' --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
check "q 搜到正文（关键词）" "1" "$("${CLI}" store list issue --q 复现 --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
check "审计集合按 bool 过滤" "2" "$("${CLI}" store list audit --where ok=true --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
( "${CLI}" store list issue --where nope=1 >"${TMP}/o6" 2>&1 ) && R=0 || R=$?
nz "没声明的字段不能当过滤条件" "${R}"
contains "并列出能过滤的" "status" "$(cat "${TMP}/o6")"
( "${CLI}" store list issue --where "owner=@${NS}" >"${TMP}/o9" 2>&1 ) && R=0 || R=$?
nz "声明了但没进 index 也不能过滤（不是给你个空结果）" "${R}"
contains "并说清哪些能过滤" "能过滤的" "$(cat "${TMP}/o9")"

say "8. 归档 ≠ 删除"
"${CLI}" store rm issue ISS-1 >/dev/null
check "归档后默认不再列出（用过滤计数，不靠总数）" "0" \
  "$("${CLI}" store list issue --where status=triaged --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
OUT="$("${CLI}" store list issue --archived 2>&1)"
contains "但还在（想看得显式要）" "iss-1" "${OUT}"
check "显式要就看得到" "1" \
  "$("${CLI}" store list issue --archived --where status=triaged --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"
"${CLI}" store rm audit a-2 --hard >/dev/null
check "真删就真没了" "1" "$("${CLI}" store list audit --archived --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"

say "9. 授权是另一道门（CRUD 不等于能读别人的）"
check "匿名读私有记录 → 403（不是给个空）" "403" \
  "$(curl -sS -o /dev/null -w '%{http_code}' "${NODE}/api/store/issue/ISS-1?namespace=@${NS}")"
"${CLI}" --base "${NODE}" register --email other@ncc.dev --password other1234 --name Other >/dev/null
( "${CLI}" store rm issue ISS-1 --namespace "@${NS}" >"${TMP}/o8" 2>&1 ) && R=0 || R=$?
nz "不是成员就写不了（403）" "${R}"
contains "拒的理由是授权" "成员" "$(cat "${TMP}/o8")"
"${CLI}" --base "${NODE}" login --email store@ncc.dev --password store1234 >/dev/null
check "换回身份后还是自己的" "1" \
  "$("${CLI}" store list issue --archived --where status=triaged --json | python3 -c "import json,sys;print(json.load(sys.stdin)['total'])")"

say "10. 包声明它要用哪些集合（state.stores[]）"
"${CLI}" hur init --kind harness --profile harness --name "Issue Triage" --dir "${WORK}/triage" >/dev/null
python3 - <<PY
import json
p = "${WORK}/triage/hur.json"
d = json.load(open(p))
d["state"] = {"stores": [
    {"collection": "issue", "mode": "readwrite",
     "fields": ["title:string!", "status:enum:open|closed|triaged", "severity:enum:high|mid|low"],
     "index": ["status", "severity"], "shape": "mutable", "visibility": "private",
     "reason": "认领问题单并回写处理进展"},
    {"collection": "audit", "mode": "write", "reason": "只上报，从不回读"},
]}
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY
OUT="$("${CLI}" hur profile "${WORK}/triage" 2>&1)"
contains "四问里说清要哪些集合" "集合：" "${OUT}"
contains "读改都标出来" "issue（读改 · mutable · private）" "${OUT}"
contains "只写的也老实标" "audit（只写）" "${OUT}"
contains "给了理由就显示理由" "认领问题单并回写处理进展" "${OUT}"
contains "还说清了它在模型面前是什么样" "模型面：" "${OUT}"
contains "声明了写的那些被标出来（模型能改它们 = 人给了口子）" "**写** issue/audit" "${OUT}"
OUT="$("${CLI}" hur verify "${WORK}/triage" 2>&1)" || true
not_contains "不该冒出集合相关的 R12" "动态 ≠ 无模式" "${OUT}"

say "11. R12：想过滤没声明的字段 = 拦（离线就拦，不用等节点）"
cp -r "${WORK}/triage" "${WORK}/sneaky"
python3 - <<PY
import json
p = "${WORK}/sneaky/hur.json"
d = json.load(open(p))
d["state"]["stores"][0]["index"].append("owner")   # owner 没在 fields 里
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY
OUT="$("${CLI}" hur verify "${WORK}/sneaky" 2>&1)" && R=0 || R=$?
nz "校验不过" "${R}"
contains "理由是 R12" "[R12" "${OUT}"
contains "说清是「动态 ≠ 无模式」" "动态 ≠ 无模式" "${OUT}"

say "12. 快照包只读（数据不该能写）"
python3 - <<PY
import json, os
src = "${WORK}/triage/hur.json"
d = json.load(open(src))
d["profile"] = "kb-seed"
d["entry"] = ""
d["permissions"] = {}
d["state"]["stores"][0]["mode"] = "readwrite"
d["data"] = {"source": "@${NS}/handbook", "snapshot_at": "2026-09-28T10:00:00Z",
             "privacy": "internal", "license": "CC-BY-4.0",
             "docs": [{"path": "data/sample.md", "slug": "sample"}]}
os.makedirs("${WORK}/snap/data", exist_ok=True)
open("${WORK}/snap/data/sample.md", "w").write("# 快照\\n")
json.dump(d, open("${WORK}/snap/hur.json", "w"), ensure_ascii=False, indent=2)
PY
OUT="$("${CLI}" hur verify "${WORK}/snap" 2>&1)" && R=0 || R=$?
nz "数据快照带写权限 → 拒" "${R}"
contains "说清快照是只读的" "不该写集合" "${OUT}"

# 为什么下面的 python 都用「带引号的 heredoc」而不是 `python3 -c "…"`：
#   `"$(python3 -c "
#      print({'a','b'})
#    ")"`
# 里的 `{a,b}` 会被 bash 再解析一遍（花括号展开）—— python 实际收到三个参数，
# 断言看起来在跑、其实什么都没检查。同样的坑还有标签里的反引号（会被当命令执行）。
# 这个脚本里一律用 heredoc 传 python 源码。
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "$1"; }
names_of() {  # $1 = tools/list 的响应文件
  python3 - "$1" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print(' '.join(t['name'] for t in d['result']['tools']))
PY
}
mcp_req() {  # $1=包目录(可空) $2=方法 $3=params JSON(可空) $4=落到哪个文件
  local req="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\""
  [[ -n "$3" ]] && req="${req},\"params\":$3"
  req="${req}}"
  if [[ -n "$1" ]]; then
    "${CLI}" mcp --package "$1" <<< "${req}" 2>/dev/null | head -1 > "$4"
  else
    "${CLI}" mcp <<< "${req}" 2>/dev/null | head -1 > "$4"
  fi
}

say "13. 模型面 = 声明面（ncc mcp --package）"
# 造一份声明：issue 读改 · audit 只写 · ghost 声明了但节点上没有
cp -r "${WORK}/triage" "${WORK}/face"
python3 - "${WORK}/face/hur.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["state"]["stores"] = [
    {"collection": "issue", "mode": "readwrite", "reason": "认领问题单并回写进展"},
    {"collection": "audit", "mode": "write", "reason": "只上报，从不回读"},
    {"collection": "ghost", "mode": "read", "reason": "节点上其实没有这个集合"},
]
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY

mcp_req "${WORK}/face" tools/list "" "${TMP}/face.json"
mcp_req "${WORK}/face" initialize '{}' "${TMP}/face-init.json"
FACE_NAMES="$(names_of "${TMP}/face.json")"
contains "声明了读改的集合 → 模型拿到「列」" "ncc_store_list_issue" "${FACE_NAMES}"
contains "也拿到「读一条」" "ncc_store_get_issue" "${FACE_NAMES}"
contains "声明了写 → 模型拿到「写」" "ncc_store_put_issue" "${FACE_NAMES}"
contains "只写的集合 → 只有写口子" "ncc_store_put_audit" "${FACE_NAMES}"
not_contains "只写的集合 → 没有读口子（写进去的不回头读）" "ncc_store_list_audit" "${FACE_NAMES}"
not_contains "收窄之后就没有通用的浏览工具了" "ncc_list_store_collections" "${FACE_NAMES}"
not_contains "节点上没有的集合一个都不给（不广告不存在的工具）" "ncc_store_list_ghost" "${FACE_NAMES}"
contains "说明书里说清了这个面被收窄到哪（模型自己知道边界）" "收窄到包" "$(cat "${TMP}/face-init.json")"
contains "也说清了只写的那一个不给读口子" "audit（write）" "$(cat "${TMP}/face-init.json")"

say "14. 声明成了模型的 schema（词表与必填都从声明来）"
schema_get() {  # $1=工具名 $2=点分路径（如 fields.properties.status.enum）
  python3 - "${TMP}/face.json" "$1" "$2" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
t = [x for x in d['result']['tools'] if x['name'] == sys.argv[2]][0]
cur = t['inputSchema']['properties']
for part in sys.argv[3].split('.'):
    cur = cur[part]
print(json.dumps(cur, ensure_ascii=False))
PY
}
check "必填进了 schema" "true" \
  "$(python3 - "${TMP}/face.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
t = [x for x in d['result']['tools'] if x['name'] == 'ncc_store_put_issue'][0]
print(str('title' in t['inputSchema']['properties']['fields']['required']).lower())
PY
)"
check "枚举词表原样进了 schema（模型看得见合法取值，不会去试错）" '["open", "closed", "triaged"]' \
  "$(schema_get ncc_store_put_issue fields.properties.status.enum)"
check "过滤只广告 index 里的字段" '{"labels": {"description": "按 labels 过滤", "type": "string"}, "severity": {"description": "按 severity 过滤（取值：high / mid / low）", "type": "string"}, "status": {"description": "按 status 过滤（取值：open / closed / triaged）", "type": "string"}}' \
  "$(schema_get ncc_store_list_issue filter.properties)"
not_contains "没在 index 里的字段不广告（广告了就是让模型去撞 400）" "owner" \
  "$(schema_get ncc_store_list_issue filter.properties)"
contains "字段说明里带上了声明" "title:string" "$(schema_get ncc_store_put_issue fields.description)"

say "15. 看不到的也真调不动（不只是不广告）"
mcp_req "${WORK}/face" tools/call '{"name":"ncc_store_list_audit","arguments":{}}' "${TMP}/r1.json"
check "只写集合的读口子被拒（真拦）" "True" "$(jget "['result']['isError']" < "${TMP}/r1.json")"
contains "并说清为什么" "只写" "$(cat "${TMP}/r1.json")"
mcp_req "${WORK}/face" tools/call '{"name":"ncc_store_list_ghost","arguments":{}}' "${TMP}/r2.json"
check "声明了但节点上没有的集合也调不动" "True" "$(jget "['result']['isError']" < "${TMP}/r2.json")"
contains "并说清是「声明了但没落地」，不是「没声明」" "节点上没有" "$(cat "${TMP}/r2.json")"
# 只声明了读的包（triage 是 readwrite，另造一份只读的）
cp -r "${WORK}/triage" "${WORK}/roface"
python3 - "${WORK}/roface/hur.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
for s in d["state"]["stores"]:
    if s["collection"] == "issue":
        s["mode"] = "read"
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY
mcp_req "${WORK}/roface" tools/list "" "${TMP}/ro.json"
RO_NAMES="$(names_of "${TMP}/ro.json")"
contains "只读的包 → 有读工具" "ncc_store_list_issue" "${RO_NAMES}"
not_contains "只读的包 → 工具表里没有写" "ncc_store_put_issue" "${RO_NAMES}"
mcp_req "${WORK}/roface" tools/call '{"name":"ncc_store_put_issue","arguments":{"key":"x","body":"x","fields":{"title":"x","status":"open"}}}' "${TMP}/r3.json"
check "只读的包 → 写被拒" "True" "$(jget "['result']['isError']" < "${TMP}/r3.json")"
contains "拒的理由说清了该怎么改" "readwrite" "$(cat "${TMP}/r3.json")"

say "16. 模型面真的能用（写进去、读得回）"
mcp_req "${WORK}/face" tools/call '{"name":"ncc_store_put_issue","arguments":{"key":"from-model","body":"模型写的一条","fields":{"title":"模型报的问题","status":"open","severity":"high","labels":["bug"]},"note":"由模型写入"}}' "${TMP}/r4.json"
check "模型写得进去（这个包声明了写）" "False" "$(jget "['result']['isError']" < "${TMP}/r4.json")"
contains "回执说清写到了哪" "from-model" "$(cat "${TMP}/r4.json")"
check "节点侧看得到（1 条）" "1" \
  "$("${CLI}" store list issue --where 'status=open' --json | jget "['total']")"
mcp_req "${WORK}/face" tools/call '{"name":"ncc_store_get_issue","arguments":{"key":"from-model"}}' "${TMP}/r5.json"
contains "模型读得回正文" "模型写的一条" "$(cat "${TMP}/r5.json")"
contains "也看得到是谁写的（可复盘）" "由模型写入" "$("${CLI}" store history issue from-model 2>&1)"

say "17. 没给包 = 只给浏览（写要由持凭据的人显式跑 CLI）"
mcp_req "" tools/list "" "${TMP}/base.json"
BASE_NAMES="$(names_of "${TMP}/base.json")"
contains "有浏览工具" "ncc_list_store_collections" "${BASE_NAMES}"
contains "有读一条" "ncc_get_store_record" "${BASE_NAMES}"
not_contains "但一个写口子都没有" "ncc_store_put" "${BASE_NAMES}"
mcp_req "" tools/call '{"name":"ncc_list_store_collections","arguments":{}}' "${TMP}/r6.json"
check "浏览工具调得动" "False" "$(jget "['result']['isError']" < "${TMP}/r6.json")"
contains "列得出集合" "issue" "$(cat "${TMP}/r6.json")"
mcp_req "" tools/call '{"name":"ncc_list_store_records","arguments":{"collection":"issue","limit":5}}' "${TMP}/r7.json"
contains "也列得出记录" "from-model" "$(cat "${TMP}/r7.json")"

say "18. 包里没声明集合 = 模型看不见任何集合"
EMPTY="${WORK}/noface"
mkdir -p "${EMPTY}"
python3 - "${WORK}/triage/hur.json" "${EMPTY}/hur.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
d["state"] = {}
json.dump(d, open(sys.argv[2], "w"), ensure_ascii=False, indent=2)
PY
mcp_req "${EMPTY}" tools/list "" "${TMP}/noface.json"
EMPTY_NAMES="$(names_of "${TMP}/noface.json")"
contains "包自己还是有工具的（NCC 的通用工具留着）" "ncc_list_kinds" "${EMPTY_NAMES}"
not_contains "但一个集合工具都没有（没声明 = 看不到）" "ncc_store_" "${EMPTY_NAMES}"
mcp_req "${EMPTY}" tools/call '{"name":"ncc_store_list_issue","arguments":{}}' "${TMP}/r8.json"
check "也调不动（面是空的）" "True" "$(jget "['result']['isError']" < "${TMP}/r8.json")"

say "19. 声明可以被配置（进 git / 评审 / apply）"
gitless="${WORK}/stores"
"${CLI}" store export --dir "${gitless}" >/dev/null
check "导出了一件一个文件" "yes" "$( [[ -f "${gitless}/issue.json" && -f "${gitless}/audit.json" ]] && echo yes || echo no)"
check "也给了怎么用的一句话（README）" "yes" "$( [[ -f "${gitless}/README.md" ]] && echo yes || echo no)"
check "导的是声明本身（能再 apply）" "issue" \
  "$(python3 - "${gitless}/issue.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["collection"])
PY
)"
check "字段拼回了声明语法（类型/必填/词表都在）" "yes" \
  "$(python3 - "${gitless}/issue.json" <<'PY'
import json, sys
fs = json.load(open(sys.argv[1]))["fields"]
want = {"title:string!", "status:enum:open|closed|triaged", "severity:enum:high|mid|low",
        "labels:string[]", "body:text?search", "owner:ref"}
print("yes" if want <= set(fs) else "no:" + ",".join(sorted(fs)))
PY
)"
# 保真不是"看起来一样"：导出来的那份要能**真的 apply 回去**（不仅 --check 无差异）。
# 这里真 apply 一次 —— 早期 enum 少写了 `enum:` 前缀，--check 两边都走同一个函数所以
# "无差异"，真 apply 才被节点拒（导出的声明其实不可再应用）。
( "${CLI}" store declare --dir "${gitless}" >"${TMP}/rt" 2>&1 ) && R=0 || R=$?
check "导出的声明真的能再 apply（保真）" "0" "${R}"
contains "而且是「不变」而不是重建" "不变" "$(cat "${TMP}/rt")"
# 保真：导出来的再 apply 一遍，应当「什么都不变」
OUT="$("${CLI}" store declare --dir "${gitless}" --check 2>&1)" && R=0 || R=$?
check "导出→再 apply：全部不变（保真）" "0" "${R}"
contains "并且如实报「不变」" "不变" "${OUT}"
contains "汇总说清了什么都没改" "什么都没改" "${OUT}"
contains "还说明了导的不是记录本身" "不是记录本身" "$("${CLI}" store export --dir "${gitless}" 2>&1)"

say "20. --check 是门禁：先看会改什么，再决定改不改"
python3 - "${gitless}/issue.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["fields"].append("priority:enum:p0|p1|p2")   # 加一个字段
d["index"].append("priority")                  # 并且让它能过滤
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY
OUT="$("${CLI}" store declare --dir "${gitless}" --check 2>&1)" && R=0 || R=$?
check "有差异时 --check 退出码 1（能当 CI 门禁）" "1" "${R}"
contains "说清是哪一项变了" "fields" "${OUT}"
contains "也点了 index" "index" "${OUT}"
count_fields() {
  "${CLI}" store ls --json | python3 -c '
import json, sys
cs = json.load(sys.stdin)["collections"]
print(len([c for c in cs if c["kind"] == "issue"][0]["fields"]))'
}
check "而且真的没改（节点上还是 6 个字段）" "6" "$(count_fields)"
# 这一处要**看退出码**：`OUT="$(...)"` 在 set -e 下会把失败悄悄变成"脚本直接结束"，
# 于是真正的报错永远不出现（本文件早期就在这上面踩过一次）。
( "${CLI}" store declare --dir "${gitless}" >"${TMP}/apply1" 2>&1 ) && R=0 || R=$?
check "apply 成功" "0" "${R}"
contains "apply 之后才真的加上" "已更新 集合" "$(cat "${TMP}/apply1")"
check "节点上有 7 个字段了" "7" "$(count_fields)"
check "再 apply 一次就「不变」了（幂等）" "0" \
  "$("${CLI}" store declare --dir "${gitless}" --check >/dev/null 2>&1; echo $?)"

say "21. 离线先拦能拦的（别等人跑一趟才知道）"
cat > "${gitless}/bad.json" <<'JSON'
{"collection": "badcase", "fields": ["title:string"], "index": ["status"]}
JSON
OUT="$("${CLI}" store declare --file "${gitless}/bad.json" 2>&1)" && R=0 || R=$?
nz "想过滤没声明的字段 → 本地就拒（与包的 R12 同一条口径）" "${R}"
contains "说清是「动态 ≠ 无模式」" "动态 ≠ 无模式" "${OUT}"
rm -f "${gitless}/bad.json"
cat > "${gitless}/mode.json" <<'JSON'
{"state": {"stores": [{"collection": "note", "mode": "readwrite", "reason": "包需要它",
  "fields": ["title:string!"], "index": ["title"]}]}}
JSON
( "${CLI}" store declare --file "${gitless}/mode.json" >"${TMP}/modeout" 2>&1 ) && R=0 || R=$?
check "包里的 state.stores 能直接落地" "0" "${R}"
contains "并且说清 mode 是包的需求、节点这边不看" "节点这边不看它" "$(cat "${TMP}/modeout")"
check "落地的集合在节点上" "yes" \
  "$("${CLI}" store ls --json | python3 -c '
import json, sys
print("yes" if any(c["kind"] == "note" for c in json.load(sys.stdin)["collections"]) else "no")')"
check "包里的 reason 也落下来了（导出时能带回去）" "包需要它" \
  "$("${CLI}" store export --json 2>/dev/null | python3 -c '
import json, sys
ss = json.load(sys.stdin)
print([s["reason"] for s in ss if s["collection"] == "note"][0])')"

say "22. 内容清单：ncc store ls 回答「这台节点上有什么内容」"
OUT="$("${CLI}" store ls 2>&1)"
contains "列得出内置的那几类（哪怕还没用过）" "内置（名字保留，节点自己用）：kb / mem / ckpt / trace" "${OUT}"
contains "并说清还没用过的是哪些" "还没用过" "${OUT}"
# 内置名字是保留的：谁都能 declare 同名集合的话，`ls` 就开始说谎（同名两种内容）
OUT2="$("${CLI}" store declare kb --field 'x:string' 2>&1)" && R=0 || R=$?
nz "内置名字不能 declare" "${R}"
contains "拒的时候说清为什么" "内置集合名" "${OUT2}"
# 用过一次之后它出现在清单里（取用即声明）
"${CLI}" mem set probe.key "看一眼清单" --kind fact >/dev/null
LSJSON="$("${CLI}" store ls --json)"
check "用过之后 mem 出现在清单里（取用即声明）" "yes" \
  "$(python3 -c '
import json, sys
cs = json.load(sys.stdin)["collections"]
print("yes" if any(c["kind"] == "mem" for c in cs) else "no")' <<<"${LSJSON}")"
check "并且标出来它是内置的" "yes" \
  "$(python3 -c '
import json, sys
cs = json.load(sys.stdin)["collections"]
print("yes" if any(c["kind"] == "mem" and c.get("builtin") for c in cs) else "no")' <<<"${LSJSON}")"
check "内置带了字段声明（清单本身就是一份说明）" "yes" \
  "$(python3 -c '
import json, sys
cs = json.load(sys.stdin)["collections"]
m = [c for c in cs if c["kind"] == "mem"][0]
print("yes" if any(f["name"] == "subject" for f in m["fields"]) else "no")' <<<"${LSJSON}")"
contains "导出时跳过内置（它们的声明在节点代码里，不是配置）" "跳过" \
  "$("${CLI}" store export 2>&1 | tail -2 | tr '\n' ' ')"
check "导出的文件里确实没有内置" "no" \
  "$("${CLI}" store export --json 2>/dev/null | python3 -c '
import json, sys
ss = json.load(sys.stdin)
print("no" if not any(s["collection"] in ("kb","mem","ckpt","trace") for s in ss) else "yes")')"
# 五类内容共用同一份口径（节点侧断言五处逐字相同，这边断言读得到）
contains "节点说得出五类共用的口径" "归档 ≠ 删除" "$(curl -fsS "${NODE}/api/store/kinds")"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]] || exit 1
