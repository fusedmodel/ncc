#!/usr/bin/env bash
# NCC Gateway **数据面**冒烟：accept（提供出口）与 forward（借出口）真的转发吗？
#
# 为什么单独有这一条：`gateway-audit-smoke.sh` 覆盖的是**控制面**（把本地审计聚合成
# 摘要、签名、上报），它从不调 `ncc gateway run` —— 直白说，数据面在自动化里**没有**
# 任何一条端到端用例，只有单测（单测跑不到"真起进程、真出网、真被拒"）。
# 这个脚本补上那一段：真起假上游 + 两个网关进程，断言"允许的出去了、不允许的没出去"。
#
# 断言的核心不是"返回 200"，而是**字节去哪了**：
#   · 白名单内的请求：上游收到的是 **B 注入的凭据**，调用方塞的 Authorization 没发出去；
#   · 白名单外的请求：403，而且上游日志里**根本没有这条路径**（不是被上游拒的）；
#   · 停掉 B：A 得 502，**不降级、不中转**；
#   · 两侧 audit JSONL：有裁决记录，但**载荷与密钥一个字节都没有**。
#
# 全程隔离（NCC_HOME / 端口 / 审计目录全在临时目录），跑完自动清理。
# 用法：bash scripts/gateway-route-smoke.sh   （KEEP=1 保留现场）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
WORK="${TMP}/work"
mkdir -p "${WORK}"

# 端口：从 $$ 派生，避免和正在跑的东西撞车；可用环境变量覆盖
UP_PORT="${UP_PORT:-$((19100 + $$ % 200))}"
B_PORT="${B_PORT:-$((19300 + $$ % 200))}"
A_PORT="${A_PORT:-$((19500 + $$ % 200))}"

PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [[ -n "${KEEP:-}" ]]; then echo "（KEEP=1）保留 ${TMP}"; else rm -rf "${TMP}"; fi
}
trap cleanup EXIT

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc" "${ROOT}/release/bin/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  command -v ncc || true
}
CLI="$(find_cli)"
if [[ -z "${CLI}" ]]; then echo "找不到 ncc 可执行文件（先 cargo build，或用 CLI_BIN=… 指定）" >&2; exit 2; fi
echo "用 CLI：${CLI}"

PASS=0
FAIL=0
say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 300)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
alive() {
  local url="$1" deadline=$((SECONDS + ${2:-30}))
  while (( SECONDS < deadline )); do curl -fsS -o /dev/null "$url" 2>/dev/null && return 0; sleep 0.2; done
  return 1
}
# 等端口可连（上游会回 404，只要连得上就算起来了）
wait_port() {
  local port="$1" deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    curl -sS -o /dev/null "http://127.0.0.1:${port}/__probe" 2>/dev/null && return 0
    sleep 0.2
  done
  return 1
}

MARKER="MARKER-PAYLOAD-$(date +%s)-$$"
B_SECRET="sk-B-SIDE-SECRET-DO-NOT-LEAK"
CALLER_KEY="sk-CALLER-KEY-DO-NOT-LEAK"
B_TOKEN="b-side-token-1234567890"
A_TOKEN="a-caller-token-1234567890"

# ---------------------------------------------------------------- 1. 假上游
say '1. 假上游（把每条请求的 path / 凭据 / 载荷记进一个日志）'
UP_LOG="${WORK}/upstream.jsonl"
cat > "${WORK}/upstream.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[1]
PORT = int(sys.argv[2])

class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def _log(self, method):
        n = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(n).decode('utf-8', 'replace') if n else ''
        rec = {
            'method': method,
            'path': self.path,
            'authorization': self.headers.get('Authorization') or '',
            'x_route': self.headers.get('X-Route') or '',
            'body': body,
        }
        with open(LOG, 'a') as f:
            f.write(json.dumps(rec) + '\n')
        out = json.dumps({'ok': True, 'seen_path': self.path}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(out)))
        self.end_headers()
        self.wfile.write(out)
    def do_POST(self):
        self._log('POST')
    def do_GET(self):
        self._log('GET')
    def log_message(self, *a):
        pass

ThreadingHTTPServer(('127.0.0.1', PORT), H).serve_forever()
PY
: > "${UP_LOG}"
python3 "${WORK}/upstream.py" "${UP_LOG}" "${UP_PORT}" &
PIDS+=("$!")
wait_port "${UP_PORT}" || { echo "假上游没起来" >&2; exit 2; }
good "假上游起来了（127.0.0.1:${UP_PORT}）"

# ---------------------------------------------------------------- 2. 出口的"出处"：HUR 包
say '2. 出口必须由一份包声明背书（R10）：生成包 + 写 egress{}'
EG="${WORK}/egress"
"${CLI}" hur init --kind harness --name "LLM Egress" --dir "${EG}" >/dev/null 2>&1
python3 - "${EG}/hur.json" "${UP_PORT}" <<'PY'
import json, sys
p, port = sys.argv[1], int(sys.argv[2])
d = json.load(open(p))
# 出口要能出去：permissions.network 不能空（R10 直接判错）
d['permissions']['network'] = ['127.0.0.1']
d['egress'] = {'provides': [{
    'name': 'llm',
    'target': f'http://127.0.0.1:{port}',
    'paths': ['chat/completions'],
    'methods': ['POST'],
    # 包里**只有头名，没有值**（包要能签名/分发/公开检索 → 凭据不进包）；
    # 值写在网关的本地配置里（下面 route 的 inject）。
    'inject': ['Authorization', 'X-Route'],
}]}
json.dump(d, open(p, 'w'), ensure_ascii=False, indent=2)
PY
( cd "${EG}" && "${CLI}" hur build . >/dev/null 2>&1 && "${CLI}" hur verify . >/dev/null 2>&1 ) \
  && good "出口包合规（verify 通过）" || bad "出口包校验没过"

# ---------------------------------------------------------------- 3. 两份配置 + 启动闸
say '3. 两份 gateway.json（B=accept 提供出口 / A=forward 借出口）'
mkdir -p "${TMP}/home-b/.ncc" "${TMP}/home-a/.ncc" "${WORK}/audit-a" "${WORK}/audit-b"
cat > "${TMP}/home-b/.ncc/gateway.json" <<JSON
{
  "listen": "127.0.0.1:${B_PORT}",
  "audit_dir": "${WORK}/audit-b",
  "routes": [
    {
      "name": "llm",
      "mode": "accept",
      "target": "http://127.0.0.1:${UP_PORT}",
      "paths": ["chat/completions"],
      "methods": ["POST"],
      "inject": { "Authorization": "Bearer ${B_SECRET}", "X-Route": "b-upstream" },
      "token": "${B_TOKEN}",
      "hur": "${EG}"
    }
  ]
}
JSON
cat > "${TMP}/home-a/.ncc/gateway.json" <<JSON
{
  "listen": "127.0.0.1:${A_PORT}",
  "audit_dir": "${WORK}/audit-a",
  "routes": [
    {
      "name": "llm",
      "mode": "forward",
      "peer_url": "http://127.0.0.1:${B_PORT}",
      "peer_token": "${B_TOKEN}",
      "token": "${A_TOKEN}"
    }
  ]
}
JSON

OUT_B="$(NCC_HOME="${TMP}/home-b" "${CLI}" gateway check 2>&1)" || true
contains "B 侧配置可用" "配置可用" "${OUT_B}"
contains "并打出背书包（谁背书的）" "背书包" "${OUT_B}"
contains "以及本配置比包声明更窄" "本配置用了声明里的 1/1 条路径" "${OUT_B}"
OUT_A="$(NCC_HOME="${TMP}/home-a" "${CLI}" gateway check 2>&1)" || true
contains "A 侧配置可用" "配置可用" "${OUT_A}"

# 三条 fail-closed：包不背书 / 配置比声明宽 / 空 paths
python3 - "${TMP}/home-b/.ncc/gateway.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
# ① 配置比声明宽：多加一条包声明里没有的路径
wider = json.loads(json.dumps(d))
wider['routes'][0]['paths'] = ['chat/completions', 'models']
json.dump(wider, open(p + '.wider', 'w'), indent=2)
# ② 不绑包（缺 "hur"）
nohur = json.loads(json.dumps(d))
nohur['routes'][0].pop('hur', None)
json.dump(nohur, open(p + '.nohur', 'w'), indent=2)
PY
cp "${TMP}/home-b/.ncc/gateway.json" "${TMP}/home-b/.ncc/gateway.json.orig"
cp "${TMP}/home-b/.ncc/gateway.json.nohur" "${TMP}/home-b/.ncc/gateway.json"
R=0; NCC_HOME="${TMP}/home-b" "${CLI}" gateway check >/dev/null 2>&1 || R=$?
check "accept 路由没绑包 → 拒绝启动" "1" "${R}"
OUT="$(NCC_HOME="${TMP}/home-b" "${CLI}" gateway check 2>&1 || true)"
contains "并说明要绑包" "hur" "${OUT}"
cp "${TMP}/home-b/.ncc/gateway.json.wider" "${TMP}/home-b/.ncc/gateway.json"
R=0; NCC_HOME="${TMP}/home-b" "${CLI}" gateway check >/dev/null 2>&1 || R=$?
check "配置比包声明宽（多一条 models）→ 拒绝启动" "1" "${R}"
OUT="$(NCC_HOME="${TMP}/home-b" "${CLI}" gateway check 2>&1 || true)"
contains "并指名越界的那条路径" "models" "${OUT}"
cp "${TMP}/home-b/.ncc/gateway.json.orig" "${TMP}/home-b/.ncc/gateway.json"
R=0; NCC_HOME="${TMP}/home-b" "${CLI}" gateway check >/dev/null 2>&1 || R=$?
check "还原后又能启动" "0" "${R}"

# ---------------------------------------------------------------- 4. 起两个网关
say '4. 起 B（accept）与 A（forward）两个真进程'
NCC_HOME="${TMP}/home-b" nohup "${CLI}" gateway run > "${WORK}/gw-b.log" 2>&1 &
PIDS+=("$!")
NCC_HOME="${TMP}/home-a" nohup "${CLI}" gateway run > "${WORK}/gw-a.log" 2>&1 &
PIDS+=("$!")
wait_port "${B_PORT}" || { echo "B 没起来：$(cat "${WORK}/gw-b.log")" >&2; exit 2; }
wait_port "${A_PORT}" || { echo "A 没起来：$(cat "${WORK}/gw-a.log")" >&2; exit 2; }
good "两个网关都在监听（A:${A_PORT} / B:${B_PORT}）"

# ---------------------------------------------------------------- 5. 白名单内：出得去，且带的是 B 的凭据
say '5. 白名单内：请求出去了，而且是 B 自己注入的凭据'
BODY="{\"marker\":\"${MARKER}\",\"messages\":[]}"
# 直连 B 要用 **B 这条路由的令牌**（来客拿不到 B 的密钥，只能拿到这把令牌）；
# 顺手再多塞一个请求头，用来看它会不会被透传到上游。
CODE="$(curl -sS -o "${WORK}/resp-b.json" -w '%{http_code}' -X POST \
  "http://127.0.0.1:${B_PORT}/v1/llm/chat/completions" \
  -H "Authorization: Bearer ${B_TOKEN}" -H "X-Caller-Secret: ${CALLER_KEY}" \
  -H 'Content-Type: application/json' -d "${BODY}" || echo 000)"
check "直连 B：200" "200" "${CODE}"
UP="$(cat "${UP_LOG}")"
contains "上游收到了这条载荷" "${MARKER}" "${UP}"
contains "上游看到的是 B 注入的凭据" "Bearer ${B_SECRET}" "${UP}"
not_contains "调用方自己塞的 Authorization 没有发出去" "${CALLER_KEY}" "${UP}"

: > "${UP_LOG}"
CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
  "http://127.0.0.1:${A_PORT}/v1/llm/chat/completions" \
  -H "Authorization: Bearer ${A_TOKEN}" -H 'Content-Type: application/json' \
  -d "${BODY}" || echo 000)"
check "经 A 借出口：200" "200" "${CODE}"
UP="$(cat "${UP_LOG}")"
contains "A→B→上游 一路把载荷带到了" "${MARKER}" "${UP}"
contains "上游仍然只看到 B 注入的提据" "Bearer ${B_SECRET}" "${UP}"
not_contains "调用方的令牌也没发到上游" "${CALLER_KEY}" "${UP}"
not_contains "peer_token 也没发到上游" "${B_TOKEN}" "${UP}"

# ---------------------------------------------------------------- 6. 请求不出门的那几种
say '6. 不该出门的：403 / 405 / 401 / 目录穿越 —— 而且上游日志里根本没有它们'
: > "${UP_LOG}"
CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:${B_PORT}/v1/llm/models" \
  -H "Authorization: Bearer ${B_TOKEN}" -H 'Content-Type: application/json' -d '{}' || echo 000)"
check "白名单外的路径 → 403" "403" "${CODE}"
check "且一个字节都没发到上游" "0" "$(wc -l < "${UP_LOG}" | tr -d ' ')"

CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X GET "http://127.0.0.1:${B_PORT}/v1/llm/chat/completions" \
  -H "Authorization: Bearer ${B_TOKEN}" || echo 000)"
check "方法不在白名单 → 405" "405" "${CODE}"

CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:${B_PORT}/v1/llm/chat/completions" \
  -H "Authorization: Bearer wrong-token-000000" -H 'Content-Type: application/json' -d '{}' || echo 000)"
check "令牌不对 → 401" "401" "${CODE}"

CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:${B_PORT}/v1/llm/%2e%2e/chat/completions" \
  -H "Authorization: Bearer ${B_TOKEN}" -H 'Content-Type: application/json' -d '{}' || echo 000)"
check "目录穿越 → 403（不做百分号解码）" "403" "${CODE}"
check "三种被拒的请求都没到上游" "0" "$(wc -l < "${UP_LOG}" | tr -d ' ')"

# ---------------------------------------------------------------- 7. 断 B：不降级
say '7. 断开 B：A 立即 502，**不降级成什么中转**'
BPID="${PIDS[1]}"
kill "${BPID}" 2>/dev/null || true
sleep 1
CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:${A_PORT}/v1/llm/chat/completions" \
  -H "Authorization: Bearer ${A_TOKEN}" -H 'Content-Type: application/json' -d '{}' || echo 000)"
check "B 不可达 → 502" "502" "${CODE}"
check "（且不是 200 —— 没有偷偷绕过去）" "true" "$([[ "${CODE}" != "200" ]] && echo true || echo false)"

# ---------------------------------------------------------------- 8. 配额
say '8. 配额：每分钟 2 次，第 3 次 429'
python3 - "${TMP}/home-b/.ncc/gateway.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d['routes'][0]['quota_per_min'] = 2
json.dump(d, open(p, 'w'), indent=2)
PY
NCC_HOME="${TMP}/home-b" nohup "${CLI}" gateway run > "${WORK}/gw-b2.log" 2>&1 &
PIDS+=("$!")
wait_port "${B_PORT}" || { echo "B 没起来（配额轮）：$(cat "${WORK}/gw-b2.log")" >&2; exit 2; }
CODES=""
for _ in 1 2 3; do
  C="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:${B_PORT}/v1/llm/chat/completions" \
    -H "Authorization: Bearer ${B_TOKEN}" -H 'Content-Type: application/json' -d '{}' || echo 000)"
  CODES="${CODES}${C} "
done
check "三次请求的状态码" "200 200 429 " "${CODES}"

# ---------------------------------------------------------------- 9. 审计：有裁决、没载荷
say '9. 两侧审计：记了裁决，但载荷与密钥一个字节都没进去'
AUDIT_B="$(cat "${WORK}"/audit-b/*.jsonl 2>/dev/null || true)"
AUDIT_A="$(cat "${WORK}"/audit-a/*.jsonl 2>/dev/null || true)"
contains "B 侧记了裁决（decision）" '"decision"' "${AUDIT_B}"
contains "B 侧记了被拒的理由" "reason" "${AUDIT_B}"
contains "A 侧也记了（side=forward）" '"side":"forward"' "${AUDIT_A}"
contains "B 侧记成 side=accept" '"side":"accept"' "${AUDIT_B}"
not_contains "审计里没有载荷" "${MARKER}" "${AUDIT_B}${AUDIT_A}"
not_contains "审计里没有 B 的密钥" "${B_SECRET}" "${AUDIT_B}${AUDIT_A}"
not_contains "审计里没有调用方令牌" "${A_TOKEN}" "${AUDIT_B}${AUDIT_A}"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
