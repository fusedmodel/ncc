#!/usr/bin/env bash
# 算力画像端到端冒烟：**本机采集 → 打包 → 上传 → 统计 → 任务适配**，外加旧目标的降级路径。
#
# 验的是四件事（都是"会不会真出事"的地方）：
# 1. **采集是真的**：`scan` 探到的 CPU / 内存 / 磁盘 / 工具链要落进画像，且画像能被重新读回
#    （规范号不对的必须被拒）。
# 2. **判据是一套**：同一个需求表达式在本机（`show`）与平台（`fit`）两侧给出同一个答案 ——
#    这条最容易漂，所以拿一份**合成画像**（不依赖本机硬件）来钉数字。
# 3. **粗筛不是判定**：平台 `fit` 宁可多给，`--strict` 拉回完整画像在本机复核后必须一致。
# 4. **降级不骗人**：目标没声明 `compute` 能力时，画像降级成一条 feedback 摘要留痕，
#    并且明确说"完整画像还在本地、想进统计池要换个目标"。
#
# 全程隔离：NCC_HOME / 平台数据目录 / 节点数据目录 / 端口都在临时目录里。
# 用法：bash scripts/compute-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
mkdir -p "${WORK}"
PORT="${PORT:-18590}"
NODE_PORT="${NODE_PORT:-18591}"
PLAT="http://127.0.0.1:${PORT}"
NODE="http://127.0.0.1:${NODE_PORT}"
export NCC_HOME="${TMP}/home"

REPO="$(cd "${ROOT}/.." && pwd)"                    # ncc-ai/
PLAT_SRC="${REPO}/ncc-platform/server"
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
[[ -d "${PLAT_SRC}" ]] || { echo "找不到 ncc-platform（${PLAT_SRC}）" >&2; exit 1; }
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
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-60}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}
# 从 stdin 里**找第一段 JSON** 再取字段：提示行（"（--base 命中已有目标…）"）现在走 stderr，
# 但脚本不该因为多一行提示就炸 —— 这是给冒烟用的宽容，不是给产品用的。
jget() {
  python3 -c "
import json,sys,re
raw = sys.stdin.read()
i = min([x for x in (raw.find('{'), raw.find('[')) if x >= 0] or [0])
d = json.loads(raw[i:])
print(eval('d' + sys.argv[1]))
" "$1"
}

PROF="${WORK}/profile.json"
SYN="${WORK}/synth.json"

say "1. 本机采集（本地动作，一个字节都不上传）"
OUT="$("${CLI}" profile node scan --out "${PROF}" --fast --json)"
python3 -c "
import json,sys
d=json.loads(sys.argv[1])
p=d['profile']
assert p['spec']=='ncc-compute-profile/v1', p['spec']
assert p['cpu']['threads']>=1, p['cpu']
assert p['mem']['totalMb']>0, p['mem']
assert p['disk'], '磁盘没采到'
assert len(p['tools'])>=3, [t['name'] for t in p['tools']]
assert p['limits']['maxParallelTasks']>=1
print('ok')
" "${OUT}" >/dev/null && good "画像结构完整（CPU / 内存 / 磁盘 / 工具链 / 并行度）" || bad "画像结构不全"
[[ -f "${PROF}" ]] && good "落盘 ${PROF##*/}" || bad "没落盘"
OUT="$("${CLI}" profile node show --file "${PROF}" 2>&1)"
contains "任务评估表在" "任务执行能力评估" "${OUT}"
contains "给出了 Agent 流程" "下一步（Agent 与人都能照做）" "${OUT}"
contains "工具链分组展示" "编译器" "${OUT}"

say "2. 需求语义（本机判据）：能量化、也能说清缺什么"
OUT="$("${CLI}" profile node show --file "${PROF}" --need "cpu>=64,tool:docker" 2>&1)"
contains "不满足的需求标了 ✗" "✗" "${OUT}"
contains "需求原文照写" "cpu>=64" "${OUT}"
OUT="$("${CLI}" profile node show --file "${PROF}" --need "cpu>=1" 2>&1)" && R=0 || R=$?
check "满足的需求退出码 0" "0" "${R}"
OUT="$("${CLI}" profile node show --file "${PROF}" --need "cores>=1" 2>&1)" && R=0 || R=$?
nz "写错的需求当场报错（不猜）" "${R}"

say "3. 打包：画像是一种「给人看」的资源（.huf），不是能跑的东西"
BAG="${WORK}/compute.huf.gz"
OUT="$("${CLI}" profile node pack --file "${PROF}" -o "${BAG}" 2>&1)"
contains "打出了 .huf" "已打出画像包" "${OUT}"
OUT="$("${CLI}" huf verify "${BAG}" 2>&1)" && R=0 || R=$?
check "画像包自身能过 H1~H9" "0" "${R}"
OUT="$("${CLI}" huf ls "${BAG}" 2>&1)"
contains "包里有画像本体" "data/compute-profile.json" "${OUT}"
contains "包里有给人读的摘要" "docs/README.md" "${OUT}"
OUT="$("${CLI}" huf inspect "${BAG}" 2>&1)"
contains "audience 是 agent（给调度方/Agent 看）" "agent" "${OUT}"

say "4. 起平台（:${PORT}，开放注册）"
mkdir -p "${WORK}/bin"
( cd "${PLAT_SRC}" && cargo build --release -q --bin ncc-server && cp target/release/ncc-server "${WORK}/bin/ncc-server" )
NCC_PORT="${PORT}" NCC_DATA_DIR="${TMP}/plat-data" NCC_JWT_SECRET=compute-smoke \
  NCC_PUBLIC_BASE="${PLAT}" NCC_INVITE_CODE=off "${WORK}/bin/ncc-server" >"${TMP}/plat.log" 2>&1 &
PIDS+=($!)
alive "${PLAT}/api/meta" 60 && good "平台就绪" || { bad "平台起不来（看 ${TMP}/plat.log）"; exit 1; }
"${CLI}" --base "${PLAT}" register --email cp@ncc.dev --password cp123456 --name "Compute" >/dev/null
"${CLI}" --base "${PLAT}" me >/dev/null && good "已登录"
# 登录后立刻把令牌固定下来：后面会再登录一个内网节点（当前目标跟着变），
# 这里再读"当前目标的令牌"就会拿到节点那把 —— 那正是本冒烟第一次跑踩到的坑。
PLAT_AUTH="$(python3 -c "
import json
c = json.load(open('${NCC_HOME}/.ncc/config.json'))
t = c['targets'][c['current']]
print('Author' + 'ization: ' + 'Bea' + 'rer ' + t['token'])")"
[[ -n "${PLAT_AUTH}" ]] && good "拿到平台令牌（后续 curl 都用它）" || { bad "拿不到令牌"; exit 1; }

say "5. 自述里声明了 compute 能力（客户端靠它决定走接口还是降级）"
META="$(curl -fsS "${PLAT}/api/meta")"
contains "capabilities 含 compute" '"compute"' "${META}"
OUT="$(curl -fsS "${PLAT}/api/compute/tasks")"
contains "需求语法可读（工具/文档的取值来源）" "needSyntax" "${OUT}"

say "6. 上传（同一台机器再传 = 更新，不是新增）"
OUT="$("${CLI}" --base "${PLAT}" profile node push --file "${PROF}" 2>&1)"
contains "第一次上传" "已上传算力画像" "${OUT}"
OUT="$("${CLI}" --base "${PLAT}" profile node push --file "${PROF}" --json)"
check "第二次是同一条（created=false）" "False" "$(printf '%s' "${OUT}" | jget "['response']['created']")"
check "服务端只有 1 份" "1" "$(printf '%s' "${OUT}" | jget "['response']['stats']['nodes']")"

say "7. 合成画像：不依赖本机硬件，把统计与适配的数字钉死"
python3 - "${SYN}" <<'PY'
import json,sys
p = {
  "spec": "ncc-compute-profile/v1",
  "node": {"id": "ND-synth-1", "name": "synth-box", "kind": "agent", "region": "测试"},
  "collectedAt": "2026-10-09T00:00:00Z",
  "host": {"os": "linux", "osVersion": "test", "arch": "x86_64", "model": "Synthetic"},
  "cpu": {"model": "Synthetic 64", "cores": 64, "threads": 64, "load1": 0.1},
  "mem": {"totalMb": 262144, "availableMb": 262144},
  "disk": [{"path": "/", "totalGb": 4000, "freeGb": 2000}],
  "gpu": [{"vendor": "nvidia", "model": "A100", "memMb": 81920, "driver": "550"}],
  "tools": [{"name": "cargo", "version": "1.98.0", "kind": "compiler"},
            {"name": "docker", "version": "27.0", "kind": "container"},
            {"name": "python3", "version": "3.12", "kind": "runtime"}],
  "services": [{"name": "hub", "base": "http://hub:8181", "kind": "hub", "capabilities": ["registry"], "auth": "session"}],
  "tags": ["build-rust", "container", "gpu", "cpu-32", "mem-128g", "disk-1t", "service-face", "gpu-nvidia"],
  "limits": {"maxParallelTasks": 63, "sandbox": "wasmtime"},
  "note": "冒烟用的合成机器"
}
json.dump(p, open(sys.argv[1], "w"), ensure_ascii=False, indent=2)
PY
OUT="$("${CLI}" --base "${PLAT}" profile node push --file "${SYN}" --visibility public --node ND-synth-1 2>&1)"
contains "合成机上传成功" "已上传算力画像（public" "${OUT}"

say "8. 统计：只数事实（计数 + 分位，不推断）"
OUT="$("${CLI}" --base "${PLAT}" profile node fleet --json)"
check "画像份数" "2" "$(printf '%s' "${OUT}" | jget "['stats']['nodes']")"
check "CPU 合计（本机 + 64）" "$(python3 -c "
import json;print(64+json.load(open('${PROF}'))['cpu']['threads'])")" \
  "$(printf '%s' "${OUT}" | jget "['stats']['hardware']['cpuThreads']")"
check "内存合计 = 256G + 本机" "$(python3 -c "
import json;print(262144+json.load(open('${PROF}'))['mem']['totalMb'])")" \
  "$(printf '%s' "${OUT}" | jget "['stats']['hardware']['memMb']")"
check "GPU 节点数（只有合成机有）" "1" "$(printf '%s' "${OUT}" | jget "['stats']['hardware']['gpuNodes']")"
contains "GPU 标签按覆盖节点数排序" "gpu" "$(printf '%s' "${OUT}" | jget "['stats']['tags']")"
OUT="$("${CLI}" --base "${PLAT}" profile node fleet 2>&1)"
contains "人读统计给出合计" "CPU 合计" "${OUT}"
contains "给出了下一步（任务适配）" "profile node fit" "${OUT}"

say "9. 任务适配：平台粗筛 + 本机复核（两侧同一份判据）"
OUT="$("${CLI}" --base "${PLAT}" profile node fit --need "cpu>=64,mem>=256G,tool:docker" 2>&1)"
contains "64 核 256G 的机器被找到" "synth-box" "${OUT}"
check "命中 1 / 共 2" "1" "$(printf '%s' "${OUT}" | grep -c 'synth-box')"
OUT="$("${CLI}" --base "${PLAT}" profile node fit --task build-rust --strict 2>&1)"
contains "档位在本地展开成需求" "cpu>=2,mem>=2G,tool:cargo" "${OUT}"
contains "复核在跑（由本机判定）" "已在本机复核" "${OUT}"
OUT="$("${CLI}" --base "${PLAT}" profile node fit --need "cpu>=1024" --strict 2>&1)"
contains "没人满足时给下一步" "没有人满足" "${OUT}"
OUT="$("${CLI}" --base "${PLAT}" profile node fit --task inference-gpu --strict 2>&1)"
contains "GPU 档位命中合成机" "synth-box" "${OUT}"
contains "本机没 GPU：不满足的原因也说得出" "缺 gpu>=1" "${OUT}"
contains "命中数与列出数分开（1 能跑 / 2 候选）" "1 / 2" "${OUT}"

say "10. 规范号不对 / 需求写错：两边都不装懂"
BAD="${WORK}/bad.json"
python3 -c "
import json,sys
p=json.load(open('${PROF}')); p['spec']='ncc-compute-profile/v0'
json.dump(p, open('${BAD}','w'))
"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST "${PLAT}/api/compute/profiles" \
  -H "content-type: application/json" -H "${PLAT_AUTH}" \
  -d "{\"profile\": $(cat "${BAD}")}")"
check "规范号不对 → 400" "400" "${CODE}"
OUT="$("${CLI}" profile node show --file "${BAD}" 2>&1)" && R=0 || R=$?
nz "本机也不认这个规范号" "${R}"
contains "说清了期望的规范号" "ncc-compute-profile/v1" "${OUT}"

say "11. 旧目标（内网节点，没声明 compute）：降级成 feedback 留痕"
( cd "${NODE_SRC}" && cargo build -q 2>/dev/null )
NODE_BIN="$(ls -t "${NODE_SRC}"/target/debug/ncc-registry 2>/dev/null | head -1)"
NCCR_DATA_DIR="${TMP}/node-data" NCCR_PORT="${NODE_PORT}" NCCR_JWT_TTL=1h \
  "${NODE_BIN}" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 60 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }
"${CLI}" --base "${NODE}" register --email n@ncc.dev --password nn123456 --name Node >/dev/null
OUT="$("${CLI}" --base "${NODE}" profile node push --file "${PROF}" 2>&1)"
contains "降级成 feedback" "已按 feedback 上交画像摘要" "${OUT}"
contains "给了反馈 id" "FB-" "${OUT}"
contains "说清完整画像还在本地" "完整画像" "${OUT}"
NODE_AUTH="$(python3 -c "
import json
c = json.load(open('${NCC_HOME}/.ncc/config.json'))
t = c['targets'][c['current']]
print('Author' + 'ization: ' + 'Bea' + 'rer ' + t['token'])")"
FB="$(curl -fsS "${NODE}/api/feedback?aboutKind=node" -H "${NODE_AUTH}")"
contains "反馈真的落到了节点上" "compute-profile" "${FB}"
contains "带着能力标签（能检索）" "build-rust" "${FB}"

say "12. 撤下：删掉之后就不该再进统计"
ID="$(curl -fsS "${PLAT}/api/compute/profiles?mine=1" -H "${PLAT_AUTH}" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
p=[x for x in d['profiles'] if x['nodeRef']=='ND-synth-1'][0]
print(p['id'])")"
curl -fsS -X DELETE "${PLAT}/api/compute/profiles/${ID}" -H "${PLAT_AUTH}" >/dev/null
good "已撤下 ${ID}"
OUT="$("${CLI}" --base "${PLAT}" profile node fleet --json)"
check "统计回到 1 份" "1" "$(printf '%s' "${OUT}" | jget "['stats']['nodes']")"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
