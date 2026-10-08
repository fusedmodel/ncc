#!/usr/bin/env bash
# NCC 舱（`ncc app`）端到端冒烟：**三个东西一起跑**，验的就是它们的分工。
#
#   产品本体 = 用户目录里的那个舱（app.json + hur.json + run.sh）
#   引擎     = ncc（app init / doctor / up / status / export）
#   内容     = ncc-registry 节点（kb 画布 / mem 记忆 / ckpt 检查点）
#   互联     = ncc-platform（身份 + 点到点分享：只读快照链接）
#
# 全程隔离：数据目录、NCC_HOME、端口都在临时目录里，**不碰你真实的 ~/.ncc**。
# 用法：bash scripts/app-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
NODE_PORT="${NODE_PORT:-18560}"
PLAT_PORT="${PLAT_PORT:-18561}"
POD_PORT="${POD_PORT:-18562}"
NODE="http://127.0.0.1:${NODE_PORT}"
PLAT="http://127.0.0.1:${PLAT_PORT}"
export NCC_HOME="${TMP}/home"

REPO="$(cd "${ROOT}/.." && pwd)"          # ncc-ai/
REG_SRC="${REPO}/ncc-connector"
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
[[ -d "${REG_SRC}" ]] || { echo "找不到 ncc-connector（${REG_SRC}）" >&2; exit 1; }
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
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 200)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
alive() {  # 等一个 HTTP 面起来
  local url="$1" deadline=$((SECONDS + ${2:-40}))
  while (( SECONDS < deadline )); do curl -fsS "$url" >/dev/null 2>&1 && return 0; sleep 0.3; done
  return 1
}

say "0. 构建两个服务端（节点 + 平台）"
mkdir -p "${WORK}/bin"
( cd "${REG_SRC}/rust" && cargo build --release -q --bin ncc-registry && cp target/release/ncc-registry "${WORK}/bin/ncc-registry" )
( cd "${PLAT_SRC}" && cargo build --release -q --bin ncc-server && cp target/release/ncc-server "${WORK}/bin/ncc-server" )
good "ncc-registry 与 ncc-platform 都已构建"

say "1. 起内网节点（内容这一侧，:${NODE_PORT}）"
NCCR_PORT="${NODE_PORT}" NCCR_DATA_DIR="${TMP}/node-data" NCCR_JWT_SECRET=smoke-app \
  NCCR_PUBLIC_URL="${NODE}" "${WORK}/bin/ncc-registry" >"${TMP}/node.log" 2>&1 &
PIDS+=($!)
alive "${NODE}/api/meta" 40 && good "节点就绪" || { bad "节点起不来（看 ${TMP}/node.log）"; exit 1; }
check "节点声明了 kb / mem / ckpt" "yes" \
  "$(curl -sS "${NODE}/api/meta" | python3 -c "import json,sys;c=json.load(sys.stdin)['capabilities'];print('yes' if all(x in c for x in ('kb','mem','ckpt')) else 'no')")"

say "2. 起云端平台（互联这一侧，:${PLAT_PORT}）"
NCC_PORT="${PLAT_PORT}" NCC_DATA_DIR="${TMP}/plat-data" NCC_JWT_SECRET=smoke-app \
  NCC_PUBLIC_BASE="${PLAT}" NCC_INVITE_CODE=off "${WORK}/bin/ncc-server" >"${TMP}/plat.log" 2>&1 &
PIDS+=($!)
alive "${PLAT}/api/meta" 40 && good "平台就绪" || { bad "平台起不来（看 ${TMP}/plat.log）"; exit 1; }
check "平台声明了 share 能力" "yes" \
  "$(curl -sS "${PLAT}/api/meta" | python3 -c "import json,sys;print('yes' if 'share' in json.load(sys.stdin)['capabilities'] else 'no')")"

say "3. 两个身份：节点上的舱主 + 平台上的同一个人（两个目标各管一侧）"
"${CLI}" target add node --base "${NODE}" >/dev/null
"${CLI}" --target node register --email pod@ncc.dev --password poddev123 --name Pod >/dev/null
"${CLI}" target add cloud --base "${PLAT}" >/dev/null
"${CLI}" --target cloud register --email pod@ncc.ai --password poddev123 --name Pod >/dev/null
"${CLI}" target use node >/dev/null
check "节点侧已登录" "yes" "$(python3 -c "
import json;d=json.load(open('${NCC_HOME}/.ncc/config.json'));t=d['targets']['node'];print('yes' if t.get('token') else 'no')")"
check "平台侧已登录" "yes" "$(python3 -c "
import json;d=json.load(open('${NCC_HOME}/.ncc/config.json'));t=d['targets']['cloud'];print('yes' if t.get('token') else 'no')")"
check "当前目标是节点（内容这一侧）" "node" \
  "$(python3 -c "import json;print(json.load(open('${NCC_HOME}/.ncc/config.json'))['current'])")"

say "4. ncc app init：一个目录变成一个舱（app.json + hur.json + run.sh + README + SKILL）"
POD="${WORK}/pod"
OUT="$("${CLI}" app init --dir "${POD}" --namespace @pod --share-target cloud 2>&1)"
contains "init 提示下一步" "ncc app doctor" "${OUT}"
for f in app.json hur.json run.sh README.md SKILL.md; do
  [[ -f "${POD}/${f}" ]] && good "生成了 ${f}" || bad "少了 ${f}"
done
[[ -x "${POD}/run.sh" ]] && good "run.sh 可执行（harness 入口）" || bad "run.sh 不可执行"
check "app.json 的 spec 正确" "ncc-app/v1" \
  "$(python3 -c "import json;print(json.load(open('${POD}/app.json'))['spec'])")"
check "hurl.json 声明了状态" "yes" \
  "$(python3 -c "import json;d=json.load(open('${POD}/hur.json'));print('yes' if d.get('state') and d.get('entry')=='run.sh' else 'no')")"

say "5. ncc app doctor：逐项自检（内容还没写，但接口必须是通的）"
OUT="$("${CLI}" app doctor --dir "${POD}" 2>&1)"
contains "自检认可部署描述" "✓ 部署描述 app.json" "${OUT}"
contains "自检认可包声明（走 hur-core）" "✓ 应用逻辑 hur.json" "${OUT}"
contains "自检认可三样内容可读" "✓ 画布 / 笔记（kb）" "${OUT}"
contains "自检认可分享那一侧" "✓ 分享" "${OUT}"
not_contains "自检没有 ✗ 项" "✗" "${OUT}"
check "退出码 0" "0" "$?"

say "6. 往节点上写内容：画布 / 记忆 / 检查点"
printf '# 画布：把助理搬回自己的机器\n\n- 引擎 ncc\n- 内容住节点\n' > "${WORK}/canvas.md"
"${CLI}" kb set canvas-1 --title "画布：自部署助理的边界" --summary "内容住节点、互联走平台" \
  --file "${WORK}/canvas.md" >/dev/null
"${CLI}" mem set planner.decision "把 Agent 舱做成可自部署的，引擎用 ncc" --kind summary >/dev/null
printf 'pod state\n' > "${WORK}/state.bin"
"${CLI}" ckpt save --name "舱初始状态" --file "${WORK}/state.bin" --label handoff >/dev/null
good "三样内容都写进去了"

say "7. ncc app up：起舱（本机控制台 :${POD_PORT}）"
"${CLI}" app up --dir "${POD}" --port "${POD_PORT}" >"${TMP}/pod.log" 2>&1 &
PIDS+=($!)
alive "http://127.0.0.1:${POD_PORT}/healthz" 30 && good "控制台起来了" || { bad "起不来（看 ${TMP}/pod.log）"; }
check "健康检查" "ok" "$(curl -sS "http://127.0.0.1:${POD_PORT}/healthz")"

say "8. 控制台的面板：读到的必须是**真内容**（不是空壳）"
PANELS="$(curl -sS "http://127.0.0.1:${POD_PORT}/api/status"; curl -sS "http://127.0.0.1:${POD_PORT}/api/canvas"; \
          curl -sS "http://127.0.0.1:${POD_PORT}/api/memory"; curl -sS "http://127.0.0.1:${POD_PORT}/api/checkpoints"; \
          curl -sS "http://127.0.0.1:${POD_PORT}/api/shares")"
contains "状态里报告了内容目标" "ncc-registry" "${PANELS}"
contains "画布面板看到刚写的那篇" "画布：自部署助理的边界" "${PANELS}"
contains "记忆面板看到刚写的那条" "把 Agent 舱做成可自部署的" "${PANELS}"
contains "检查点面板看到刚打的那个" "舱初始状态" "${PANELS}"
contains "分享面板指向云端目标" '"target": "cloud"' "$(curl -sS "http://127.0.0.1:${POD_PORT}/api/shares" | python3 -m json.tool)"
check "分享面板能做快照" "True" \
  "$(curl -sS "http://127.0.0.1:${POD_PORT}/api/shares" | python3 -c "import json,sys;print(json.load(sys.stdin).get('snapshot'))")"

say "9. 生成只读快照链接（舱里唯一的写动作，人点的）"
SNAP="$(curl -sS -X POST "http://127.0.0.1:${POD_PORT}/api/share" -H 'content-type: application/json' -d '{"key":"canvas-smoke"}')"
SNAP_URL="$(printf '%s' "${SNAP}" | python3 -c "import json,sys;print(json.load(sys.stdin).get('url',''))")"
[[ "${SNAP_URL}" == http* ]] && good "拿到分享链接：${SNAP_URL}" || bad "没拿到链接：${SNAP}"
BODY="$(curl -sS "${SNAP_URL}?key=canvas-smoke")"
contains "匿名打开能在快照里看到画布" "画布：自部署助理的边界" "${BODY}"
contains "匿名打开能在快照里看到记忆" "planner.decision" "${BODY}"
contains "快照页自己声明是只读快照" "只读快照" "${BODY}"
WRONG="$(curl -sS "${SNAP_URL}?key=wrong-key")"
not_contains "错 key 看不到内容" "planner.decision" "${WRONG}"
contains "错 key 得到解锁页" "访问" "${WRONG}"

say "10. ncc app export：别人拿到就能部署一份自己的"
EXPORT="${WORK}/pod-export"
OUT="$("${CLI}" app export --dir "${POD}" --out "${EXPORT}" 2>&1)"
contains "导出提示别人怎么部署" "ncc app up" "${OUT}"
for f in app.json hur.json run.sh README.md SKILL.md console.html export.json checksums.txt; do
  [[ -f "${EXPORT}/${f}" ]] && good "交付物含 ${f}" || bad "交付物少了 ${f}"
done
check "checksums.txt 能核对（shasum -c）" "0" \
  "$(cd "${EXPORT}" && shasum -a 256 -c checksums.txt >/dev/null 2>&1; echo $?)"
check "export.json 记的 sha256 与文件一致" "yes" "$(python3 - "${EXPORT}" <<'PY'
import hashlib, json, pathlib, sys
d = pathlib.Path(sys.argv[1])
m = json.loads((d / "export.json").read_text())
ok = True
for f in m["files"]:
    h = hashlib.sha256((d / f["name"]).read_bytes()).hexdigest()
    ok = ok and h == f["sha256"]
print("yes" if ok else "no")
PY
)"

say "11. 隔离性自检"
[[ "${NCC_HOME}" == "${TMP}/home" ]] && good "NCC_HOME=${NCC_HOME}（临时目录）" || bad "NCC_HOME 可能污染真实环境"
if grep -q "${TMP}" "${HOME}/.ncc/config.json" 2>/dev/null; then bad "真实 ~/.ncc 被写进了本次测试的目标"; else good "真实 ~/.ncc 没被碰"; fi

printf '\n结果：%s 通过 · %s 失败\n（临时目录 %s；日志 %s/{node,plat,pod}.log）\n' "${PASS}" "${FAIL}" "${TMP}" "${TMP}"
[[ "${FAIL}" == "0" ]]
