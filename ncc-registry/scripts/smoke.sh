#!/usr/bin/env bash
# ncc-registry 冒烟测试：一个 master + 一个 worker，跑通
#   制品托管（注册 / 上传 / 发布 / 检索 / 下载）
#   Agent 托管节点（注册 + 心跳 + 发现）
#   多节点（worker 注册到 master、目录聚合、能力路由、master 代理 worker 的字节）
#
# 脚本自带启停，不依赖外部已跑的实例；端口用 18282/18283 以免撞上开发实例。
#
# 用法：bash scripts/smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT_M="${PORT_M:-18282}"
PORT_W="${PORT_W:-18283}"
MASTER="http://127.0.0.1:${PORT_M}"
WORKER="http://127.0.0.1:${PORT_W}"
TMP="$(mktemp -d)"
WORK="${TMP}/work"

PASS=0
FAIL=0

cleanup() {
  [[ -n "${PID_M:-}" ]] && kill "${PID_M}" 2>/dev/null || true
  [[ -n "${PID_W:-}" ]] && kill "${PID_W}" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT

say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }

# check <描述> <期望> <实际>
check() {
  if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi
}

# check_code <描述> <期望状态码> <curl 参数…> —— 不匹配时把响应体一并打出来，便于定位
BODY_FILE="$(mktemp)"
check_code() {
  local desc="$1" want="$2"
  shift 2
  local got
  got="$(curl -sS -o "${BODY_FILE}" -w '%{http_code}' "$@")"
  if [[ "${got}" == "${want}" ]]; then
    good "${desc}（${got}）"
  else
    bad "${desc}：期望 ${want}，实际 ${got}  body=$(head -c 200 "${BODY_FILE}")"
  fi
}

# jval <dot.path> —— 从 stdin 的 JSON 里取值（数组用数字段，如 items.0.id）
jval() {
  python3 -c '
import json, sys
d = json.load(sys.stdin)
for p in sys.argv[1].split("."):
    if p == "":
        continue
    d = d[int(p)] if isinstance(d, list) else d[p]
print(d if not isinstance(d, (dict, list)) else json.dumps(d, ensure_ascii=False))
' "$1"
}

# wait_http <url> <超时秒>
wait_http() {
  local url="$1" deadline=$((SECONDS + $2))
  while (( SECONDS < deadline )); do
    if curl -fsS "$url" >/dev/null 2>&1; then return 0; fi
    sleep 0.3
  done
  return 1
}

mkdir -p "${WORK}"

say "0. 构建 + 启动两节点（master:${PORT_M} · worker:${PORT_W}）"
( cd "${ROOT}" && go build -o "${TMP}/ncc-registry" ./cmd/ncc-registry )

NCCR_PORT="${PORT_M}" NCCR_DATA_DIR="${TMP}/m" NCCR_NODE_NAME="smoke-master" \
  NCCR_NODE_REGION="测试-内网" "${TMP}/ncc-registry" >"${TMP}/master.log" 2>&1 &
PID_M=$!

NCCR_ROLE=worker NCCR_PORT="${PORT_W}" NCCR_DATA_DIR="${TMP}/w" NCCR_NODE_NAME="smoke-worker" \
  NCCR_NODE_REGION="测试-内网" NCCR_MASTER_URL="${MASTER}" NCCR_HEARTBEAT=2s \
  "${TMP}/ncc-registry" >"${TMP}/worker.log" 2>&1 &
PID_W=$!

wait_http "${MASTER}/api/health" 15 && good "master 已就绪" || { bad "master 未就绪"; cat "${TMP}/master.log"; exit 1; }
wait_http "${WORKER}/api/health" 15 && good "worker 已就绪" || { bad "worker 未就绪"; cat "${TMP}/worker.log"; exit 1; }

say "1. 集群：worker 是否注册到 master"
for _ in $(seq 1 20); do
  WCOUNT="$(curl -sS "${MASTER}/api/cluster" | jval totals.workers || echo 0)"
  [[ "${WCOUNT}" == "1" ]] && break
  sleep 0.5
done
check "master 看到 1 个 worker" "1" "${WCOUNT}"
ROLE_W="$(curl -sS "${WORKER}/api/cluster" | jval role)"
check "worker 自述角色" "worker" "${ROLE_W}"
MASTER_ONLINE="$(curl -sS "${WORKER}/api/cluster" | jval master.online)"
check "worker 认为 master 在线" "True" "${MASTER_ONLINE}"

say "2. 制品托管：在 master 上注册 / 上传 / 发布 / 检索 / 下载"
curl -sS -X POST "${MASTER}/api/auth/register" -H 'Content-Type: application/json' \
  -d '{"email":"alice@corp.com","password":"smoke1234","name":"Alice"}' >"${TMP}/reg-m.json"
TOK_M="$(jval token <"${TMP}/reg-m.json")"
NS_M="$(curl -sS "${MASTER}/api/namespaces/mine" -H "Authorization: Bearer ${TOK_M}" | jval namespaces.0.slug)"
[[ -n "${TOK_M}" ]] && good "master 注册成功（@${NS_M}）" || { bad "master 注册失败"; cat "${TMP}/reg-m.json"; }

printf '# Hotel Skill\n\n冒烟测试用的能力制品。\n' >"${WORK}/hotel.SKILL.md"
UP_M="$(curl -sS -X POST "${MASTER}/api/registry/uploads" \
  -H "Authorization: Bearer ${TOK_M}" -H 'X-Filename: hotel.SKILL.md' \
  --data-binary @"${WORK}/hotel.SKILL.md")"
URL_M="$(printf '%s' "${UP_M}" | jval storageUrl)"
SHA_M="$(printf '%s' "${UP_M}" | jval sha256)"
[[ -n "${URL_M}" ]] && good "master 上传字节成功" || { bad "master 上传失败"; printf '%s\n' "${UP_M}"; }

CREATE_M="$(curl -sS -X POST "${MASTER}/api/registry" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d "{\"kind\":\"skill\",\"name\":\"Hotel Skill\",\"slug\":\"hotel-skill\",
  \"version\":\"1.0.0\",\"summary\":\"冒烟测试\",\"tags\":[\"smoke\",\"hotel\"],
  \"status\":\"published\",\"visibility\":\"public\",
  \"storage\":{\"url\":\"${URL_M}\",\"sha256\":\"${SHA_M}\",\"size\":43}}")"
REF_M="$(printf '%s' "${CREATE_M}" | jval item.ref)"
[[ -n "${REF_M}" ]] && good "master 发布制品成功：${REF_M}" || { bad "master 发布失败"; printf '%s\n' "${CREATE_M}"; }

FOUND="$(curl -sS "${MASTER}/api/registry?q=hotel&kind=skill" | jval total)"
check "master 目录检索命中 1 条" "1" "${FOUND}"
DL_SHA="$(curl -sS "${MASTER}/api/registry/@${NS_M}/hotel-skill/download" | jval sha256)"
check "master 下载元数据 sha256 一致" "${SHA_M}" "${DL_SHA}"

say "3. 托管节点：在 master 上把一个 Agent 节点托管进来 + 发现"
NODE_JSON="$(curl -sS -X POST "${MASTER}/api/nodes/heartbeat" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' \
  -d '{"name":"alice-agent","kind":"agent","region":"测试-内网","capabilities":["mcp","api"],"os":"darwin","arch":"arm64"}')"
NODE_ID="$(printf '%s' "${NODE_JSON}" | jval node.id)"
NODE_KIND="$(printf '%s' "${NODE_JSON}" | jval node.kind)"
NODE_STATUS="$(printf '%s' "${NODE_JSON}" | jval node.status)"
check "托管节点已注册（kind=agent）" "agent" "${NODE_KIND}"
check "托管节点在线" "online" "${NODE_STATUS}"
[[ -n "${NODE_ID}" ]] && good "节点 id：${NODE_ID}" || bad "节点注册失败"

REGION_ONLINE="$(curl -sS "${MASTER}/api/nodes/regions" | jval regions.0.online)"
check "区域覆盖里有 1 个在线节点" "1" "${REGION_ONLINE}"

say "4. 多节点：在 worker 上发布制品，看 master 能否聚合与代理字节"
curl -sS -X POST "${WORKER}/api/auth/register" -H 'Content-Type: application/json' \
  -d '{"email":"bob@corp.com","password":"smoke1234","name":"Bob"}' >"${TMP}/reg-w.json"
TOK_W="$(jval token <"${TMP}/reg-w.json")"
NS_W="$(curl -sS "${WORKER}/api/namespaces/mine" -H "Authorization: Bearer ${TOK_W}" | jval namespaces.0.slug)"
[[ -n "${TOK_W}" ]] && good "worker 注册成功（@${NS_W}）" || { bad "worker 注册失败"; cat "${TMP}/reg-w.json"; }

printf '# Edge Skill\n\n只在 worker 上存在的能力制品。\n' >"${WORK}/edge.SKILL.md"
SIZE_W="$(wc -c <"${WORK}/edge.SKILL.md" | tr -d ' ')"
UP_W="$(curl -sS -X POST "${WORKER}/api/registry/uploads" \
  -H "Authorization: Bearer ${TOK_W}" -H 'X-Filename: edge.SKILL.md' \
  --data-binary @"${WORK}/edge.SKILL.md")"
URL_W="$(printf '%s' "${UP_W}" | jval storageUrl)"
SHA_W="$(printf '%s' "${UP_W}" | jval sha256)"
CREATE_W="$(curl -sS -X POST "${WORKER}/api/registry" -H "Authorization: Bearer ${TOK_W}" \
  -H 'Content-Type: application/json' -d "{\"kind\":\"skill\",\"name\":\"Edge Skill\",\"slug\":\"edge-skill\",
  \"summary\":\"只在 worker 上\",\"tags\":[\"smoke\",\"edge\"],
  \"status\":\"published\",\"visibility\":\"public\",
  \"storage\":{\"url\":\"${URL_W}\",\"sha256\":\"${SHA_W}\",\"size\":${SIZE_W}}}")"
REF_W="$(printf '%s' "${CREATE_W}" | jval item.ref)"
[[ -n "${REF_W}" ]] && good "worker 发布制品成功：${REF_W}" || { bad "worker 发布失败"; printf '%s\n' "${CREATE_W}"; }

# master 的聚合目录要能看到 worker 的那条（等 worker 下一次心跳）
VIA=""
for _ in $(seq 1 30); do
  DIR="$(curl -sS "${MASTER}/api/cluster/directory?q=edge")"
  VIA="$(printf '%s' "${DIR}" | python3 -c '
import json,sys
try:
    d = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
for it in d.get("items", []):
    if it.get("via", {}).get("role") == "worker":
        print(it["via"]["nodeName"])
        break
')"
  [[ -n "${VIA}" ]] && break
  sleep 0.5
done
check "master 聚合目录看到 worker 的制品（via=worker）" "smoke-worker" "${VIA}"

ROUTE_NODE="$(curl -sS "${MASTER}/api/nodes/route?ref=@${NS_W}/edge-skill" | jval candidates.0.nodeName)"
check "能力路由指向持有者" "smoke-worker" "${ROUTE_NODE}"

# master 代理 worker 的字节：客户端只认识 master 一个地址
DL_URL="$(curl -sS "${MASTER}/api/registry/@${NS_W}/edge-skill/download" | jval url)"
if [[ "${DL_URL}" == *":${PORT_M}/api/registry/@${NS_W}/edge-skill"*"/bytes" ]]; then
  good "下载地址指向 master 自己的代理端点（${DL_URL}）"
else
  bad "下载地址没有落在 master 上：${DL_URL}"
fi
PROXIED_SHA="$(curl -sS "${DL_URL}" | shasum -a 256 | awk '{print $1}')"
check "master 代理回来的字节与原文件 sha256 一致" "${SHA_W}" "${PROXIED_SHA}"

say "5. 集群总览"
TOT="$(curl -sS "${MASTER}/api/cluster")"
check "集群节点数（master + 1 worker）" "2" "$(printf '%s' "${TOT}" | jval totals.nodes)"
check "集群在线 worker 数" "1" "$(printf '%s' "${TOT}" | jval totals.workersOnline)"

say "6. 接入票据：key/secret 与内网短链"
TK="$(curl -sS -X POST "${MASTER}/api/access/tickets" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d '{"label":"冒烟-给 carol 的 Agent","uses":1,"expiresInDays":1}')"
TKEY="$(printf '%s' "${TK}" | jval ticket.key)"
TSECRET="$(printf '%s' "${TK}" | jval secret)"
TLINK="$(printf '%s' "${TK}" | jval link)"
[[ -n "${TKEY}" && -n "${TSECRET}" ]] && good "签发票据成功：${TKEY}" || { bad "签发票据失败"; printf '%s\n' "${TK}"; }
case "${TLINK}" in
  *"/j/${TKEY}#${TSECRET}") good "接入短链形态正确（secret 在 fragment）" ;;
  *) bad "接入短链形态不对：${TLINK}" ;;
esac
check_code "短链落地页可访问" 200 "${MASTER}/j/${TKEY}"
check "票据概要（公开，不含 secret）" "${TKEY}" "$(curl -sS "${MASTER}/api/access/tickets/${TKEY}" | jval ticket.key)"

REDEEM="$(curl -sS -X POST "${MASTER}/api/access/redeem" -H 'Content-Type: application/json' \
  -d "{\"key\":\"${TKEY}\",\"secret\":\"${TSECRET}\",\"node\":{\"name\":\"carol-agent\",\"kind\":\"agent\",\"region\":\"测试-内网\",\"capabilities\":[\"mcp\"]}}")"
NTOK="$(printf '%s' "${REDEEM}" | jval token)"
NODE_ID_T="$(printf '%s' "${REDEEM}" | jval node.id)"
NODE_NS_T="$(printf '%s' "${REDEEM}" | jval node.namespace.slug)"
check "节点令牌已签发" "True" "$(printf '%s' "${REDEEM}" | python3 -c 'import json,sys;print(bool(json.load(sys.stdin).get("token")))')"
check "票据归属命名空间（= 签发者）" "${NS_M}" "${NODE_NS_T}"
[[ -n "${NODE_ID_T}" ]] && good "兑换即入网：节点 ${NODE_ID_T}" || bad "兑换未返回节点"

HEART="$(curl -sS -X POST "${MASTER}/api/nodes/heartbeat" -H "Authorization: Bearer ${NTOK}" \
  -H 'Content-Type: application/json' -d '{"name":"carol-agent","slug":"carol-agent"}')"
check "节点令牌可用于心跳续租" "carol-agent" "$(printf '%s' "${HEART}" | jval node.slug)"
PUB_CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${MASTER}/api/registry" -H "Authorization: Bearer ${NTOK}" \
  -H 'Content-Type: application/json' -d '{"kind":"skill","name":"Nope","slug":"nope","status":"published","storage":{"url":"http://x/y.md"}}')"
check "节点令牌不能发布（最小权限）" "403" "${PUB_CODE}"
check_code "secret 错误时拒绝兑换" 401 -X POST "${MASTER}/api/access/redeem" \
  -H 'Content-Type: application/json' -d "{\"key\":\"${TKEY}\",\"secret\":\"deadbeef\"}"
check_code "票据次数用尽后失效（uses=1）" 403 -X POST "${MASTER}/api/access/redeem" \
  -H 'Content-Type: application/json' -d "{\"key\":\"${TKEY}\",\"secret\":\"${TSECRET}\"}"

say "7. 授权：连接 ≠ 授权（私有制品 / 私有节点）"
curlv() { curl -sS "$@"; }
curl -sS -X POST "${MASTER}/api/auth/register" -H 'Content-Type: application/json' \
  -d '{"email":"carol@corp.com","password":"smoke1234","name":"Carol"}' >"${TMP}/reg-c.json"
TOK_C="$(jval token <"${TMP}/reg-c.json")"
printf '# Private Skill\n\n只给被授权的人。\n' >"${WORK}/priv.SKILL.md"
UP_P="$(curlv -X POST "${MASTER}/api/registry/uploads" -H "Authorization: Bearer ${TOK_M}" \
  -H 'X-Filename: priv.SKILL.md' --data-binary @"${WORK}/priv.SKILL.md")"
CREATE_P="$(curlv -X POST "${MASTER}/api/registry" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d "{\"kind\":\"skill\",\"name\":\"Private Skill\",\"slug\":\"priv-skill\",
  \"status\":\"published\",\"visibility\":\"private\",\"tags\":[\"priv\"],
  \"storage\":{\"url\":\"$(printf '%s' "${UP_P}" | jval storageUrl)\",\"sha256\":\"$(printf '%s' "${UP_P}" | jval sha256)\"}}")"
PREF="$(printf '%s' "${CREATE_P}" | jval item.ref)"
[[ -n "${PREF}" ]] && good "alice 发布私有制品：${PREF}" || bad "私有发布失败"

check_code "未授权时看不到私有制品" 404 "${MASTER}/api/registry/@${NS_M}/priv-skill" -H "Authorization: Bearer ${TOK_C}"
GRANT="$(curlv -X POST "${MASTER}/api/grants" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d '{"ref":"@carol","kind":"artifact","note":"冒烟测试"}')"
GID="$(printf '%s' "${GRANT}" | jval grant.id)"
check_code "授权后可见" 200 "${MASTER}/api/registry/@${NS_M}/priv-skill" -H "Authorization: Bearer ${TOK_C}"
# 私有条目的字节地址是短时签名地址：客户端拿到它就能取，不再需要带凭据。
SIGNED_URL="$(curl -sS "${MASTER}/api/registry/@${NS_M}/priv-skill/download" -H "Authorization: Bearer ${TOK_C}" | jval url)"
case "${SIGNED_URL}" in
  *"exp="*"sig="*) good "私有条目给的是签名地址" ;;
  *) bad "私有条目没有给签名地址：${SIGNED_URL}" ;;
esac
check_code "凭签名地址（无凭据）可取字节" 200 "${SIGNED_URL}"
check_code "伪造签名的地址被拒" 404 "${MASTER}/api/registry/@${NS_M}/priv-skill/bytes?exp=4102444800&sig=deadbeef"
check_code "公开条目仍是稳定地址（无签名）" 200 "${MASTER}/api/registry/@${NS_M}/hotel-skill/bytes"
check "授权列表（给出的）含该条" "${GID}" "$(curl -sS "${MASTER}/api/grants?direction=outgoing" -H "Authorization: Bearer ${TOK_M}" | jval grants.0.id)"
check "授权列表（收到的）含该条" "${GID}" "$(curl -sS "${MASTER}/api/grants?direction=incoming" -H "Authorization: Bearer ${TOK_C}" | jval grants.0.id)"

# 私有节点：不给 node 授权时不可见、不可连接
curl -sS -X POST "${MASTER}/api/nodes/heartbeat" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d '{"name":"alice-private-svc","kind":"service","region":"测试-内网","visibility":"private"}' >/dev/null
PRIV_FOUND="$(curl -sS "${MASTER}/api/nodes/discover?q=alice-private" -H "Authorization: Bearer ${TOK_C}" | jval total)"
check "未授权看不到私有节点" "0" "${PRIV_FOUND}"
curl -sS -X POST "${MASTER}/api/grants" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d '{"ref":"@carol","kind":"node"}' >/dev/null
PRIV_FOUND2="$(curl -sS "${MASTER}/api/nodes/discover?q=alice-private" -H "Authorization: Bearer ${TOK_C}" | jval total)"
check "node 授权后私有节点可见" "1" "${PRIV_FOUND2}"
curl -sS -X DELETE "${MASTER}/api/grants/${GID}" -H "Authorization: Bearer ${TOK_M}" >/dev/null
check_code "撤销授权立即生效（制品）" 404 "${MASTER}/api/registry/@${NS_M}/priv-skill" -H "Authorization: Bearer ${TOK_C}"

say "8. 集群写：发布分发（replicate）与下架回收（revoke）"
printf '# Fanout Skill\n\n分发到 worker 的副本。\n' >"${WORK}/fanout.SKILL.md"
UP_F="$(curlv -X POST "${MASTER}/api/registry/uploads" -H "Authorization: Bearer ${TOK_M}" \
  -H 'X-Filename: fanout.SKILL.md' --data-binary @"${WORK}/fanout.SKILL.md")"
CREATE_F="$(curlv -X POST "${MASTER}/api/registry" -H "Authorization: Bearer ${TOK_M}" \
  -H 'Content-Type: application/json' -d "{\"kind\":\"skill\",\"name\":\"Fanout Skill\",\"slug\":\"fanout-skill\",
  \"status\":\"published\",\"tags\":[\"fanout\"],\"replicate\":\"all\",
  \"storage\":{\"url\":\"$(printf '%s' "${UP_F}" | jval storageUrl)\",\"sha256\":\"$(printf '%s' "${UP_F}" | jval sha256)\"}}")"
FREF="$(printf '%s' "${CREATE_F}" | jval item.ref)"
check "发布即分发：worker 收到副本" "True" "$(printf '%s' "${CREATE_F}" | jval replicated.0.ok)"
check "副本字节大小一致" "$(wc -c <"${WORK}/fanout.SKILL.md" | tr -d ' ')" "$(printf '%s' "${CREATE_F}" | jval replicated.0.size)"

W_ITEM="$(curl -sS "${WORKER}/api/registry/@${NS_M}/fanout-skill")"
check "worker 本地可见副本" "replica" "$(printf '%s' "${W_ITEM}" | jval item.origin)"
check "worker 副本标注来源引用" "${FREF}" "$(printf '%s' "${W_ITEM}" | jval item.replicaOf)"
W_SHA="$(curl -sS "${WORKER}/api/registry/@${NS_M}/fanout-skill/bytes" | shasum -a 256 | awk '{print $1}')"
check "worker 副本字节 sha256 与本地一致" "$(printf '%s' "${UP_F}" | jval sha256)" "${W_SHA}"
check_code "副本不可在 worker 上改" 403 -X PATCH \
  "${WORKER}/api/registry/@${NS_M}/fanout-skill" -H "Authorization: Bearer ${TOK_W}" \
  -H 'Content-Type: application/json' -d '{"summary":"hack"}'

DEL="$(curlv -X DELETE "${MASTER}/api/registry/@${NS_M}/fanout-skill" -H "Authorization: Bearer ${TOK_M}")"
check "下架已广播到 worker" "True" "$(printf '%s' "${DEL}" | jval revoked.0.ok)"
check "worker 上的副本已回收" "True" "$(printf '%s' "${DEL}" | jval revoked.0.removed)"
check_code "回收后 worker 查不到该制品" 404 "${WORKER}/api/registry/@${NS_M}/fanout-skill"

printf '\n\033[1m结果：%d 项通过，%d 项失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" -eq 0 ]] || { echo "---- master.log ----"; tail -20 "${TMP}/master.log"; echo "---- worker.log ----"; tail -20 "${TMP}/worker.log"; exit 1; }
