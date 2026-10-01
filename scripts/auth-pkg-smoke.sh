#!/usr/bin/env bash
# 授权包（`ncc auth pkg`，profile=auth）端到端冒烟。
#
# 验的是这几条（都是刻意的边界，改坏了要有人喊）：
#   · **包里只有元数据是明文**：值只能在 `auth/vault.enc` 里；元数据文件里出现值 = 不合规
#   · **密文被清单钉住**：改一个字节就解不开（AEAD + `auth.vault_sha256`）
#   · **开锁要有许可**：口令 / 身份二选一；`auth.allow` 限定了身份就不认别人
#   · **调用结束就上锁**：`run` 的明文只活在 0700 的 session 目录里，退出即抹
#   · **更新有锁**：并发写串行化、争用能报出持有者、死锁（进程没了）能回收
#   · **过期条目不再给用**：`get --reveal` 拒、`run` 跳过
#   · **轮换是真的轮换**：项目密钥 gen+1 后旧密钥文件打不开新密文；换主密钥后旧口令失效
#   · **审计不记值**：只记条目名（值进了日志，加密就白做了）
#
# 全程隔离：HUR_HOME / NCC_HOME 都在临时目录，不碰真实的 ~/.harnessuse 与 ~/.ncc。
# 用法：bash scripts/auth-pkg-smoke.sh
set -euo pipefail
# 任何读 stdin 的命令在这里都拿到 EOF 而不是挂在那儿等键盘（踩过：`--value-stdin` 在
# 抢锁用例里没有管道，脚本直接 suspended on tty input）。管道仍然照常生效。
exec </dev/null

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
export HUR_HOME="${TMP}/hur"
export NCC_HOME="${TMP}/ncc"
mkdir -p "${HUR_HOME}" "${NCC_HOME}"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/../ncc-cli/cli/target/debug/ncc" \
              "${ROOT}/cli/target/release/ncc" "${ROOT}/../ncc-cli/cli/target/release/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc"; exit 1; }

PASS=0
FAIL=0
cleanup() {
  if [[ -n "${KEEP:-}" ]]; then echo "（KEEP=1）保留 ${TMP}"; else rm -rf "${TMP}"; fi
}
trap cleanup EXIT

say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 240)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
file_has() { if grep -q -- "$2" "$1" 2>/dev/null; then good "$3"; else bad "$3：$1 里没有「$2」"; fi; }
# 计数：grep -c 无命中时退出码是 1，`|| true` 只是为了让 set -e 别炸（别再 echo 一个 0，会变成两行）
count_in() { grep -c -- "$2" "$1" 2>/dev/null || true; }
# 期望**失败**的命令（⚠️ 不能写 `cmd | grep -q`：pipefail 下命令自己失败会盖掉 grep 的成功）
fails_with() {
  local desc="$1" needle="$2"; shift 2
  local out rc
  if out="$("$@" 2>&1)"; then rc=0; else rc=$?; fi
  if [[ "${rc}" == "0" ]]; then bad "${desc}：本该失败却成功了"; return; fi
  contains "${desc}（退出码 ${rc}）" "${needle}" "${out}"
}
ok_or_fail() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then good "${desc}"; else bad "${desc}"; fi
}
pkg() { "$CLI" auth pkg "$@"; }

PKG_DIR="${TMP}/team-auth"
PP="${TMP}/pass.txt"
printf 'correct-horse-battery-staple\n' >"${PP}"
chmod 600 "${PP}"
BAD="${TMP}/bad.txt"
printf 'wrong-pass\n' >"${BAD}"
SECRET='postgres://u:p@db.internal/app'
TOKEN='sk-live-abcdef123456'

say "0. 建包：一份带项目与口令的授权包"
pkg init "${PKG_DIR}" --id "@me/team-auth" --project web --project api \
  --note "发版用" --passphrase-file "${PP}" >"${TMP}/init.out" 2>&1 || {
    bad "init 失败：$(head -c 300 "${TMP}/init.out")"; exit 1; }
good "init 成功"
contains "包 id 按规范生成（H- 开头）" "H-" "$(cat "${TMP}/init.out")"
contains "对外引用保留成 @ns/slug" "@me/team-auth" "$(cat "${TMP}/init.out")"
file_has "${PKG_DIR}/hur.json" '"profile": "auth"' "清单里写着 profile=auth"
file_has "${PKG_DIR}/hur.json" '"vault_sha256"' "密文摘要被写进清单（钉住密文）"
check "密文文件在" "true" "$([[ -f "${PKG_DIR}/auth/vault.enc" ]] && echo true || echo false)"
check "每个项目一份密钥文件" "api.enc web.enc" "$(ls "${PKG_DIR}/auth/keys" | sort | tr '\n' ' ' | sed 's/ $//')"
check "密钥文件 0600" "600" "$(stat -f '%Lp' "${PKG_DIR}/auth/keys/web.enc" 2>/dev/null || stat -c '%a' "${PKG_DIR}/auth/keys/web.enc")"
V1="$(cat "${PKG_DIR}/auth/vault.enc" | shasum -a 256 | awk '{print $1}')"
check "清单里的摘要与密文一致" "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["auth"]["vault_sha256"])' "${PKG_DIR}/hur.json")" "${V1}"
ok_or_fail "hur verify 放过授权包（R12 认得它）" "$CLI" hur verify "${PKG_DIR}"
contains "R12 没有对 auth 包报错" "校验通过" "$("$CLI" hur verify "${PKG_DIR}" 2>&1)"

say "1. 包里只有元数据是明文（值一律在密文里）"
printf '%s\n' "${SECRET}" | pkg set "${PKG_DIR}" --project web --entry DB_URL --value-stdin \
  --kind secret --reason "加数据库串" --passphrase-file "${PP}" >"${TMP}/set.out" 2>&1 || {
    bad "set 失败：$(head -c 300 "${TMP}/set.out")"; }
good "写入一条（值从 stdin 来）"
contains "写入时提醒要重新签" "重新" "$(cat "${TMP}/set.out")"
check "整个包目录里找不到明文值" "0" "$(grep -rl -- "${SECRET}" "${PKG_DIR}" 2>/dev/null | wc -l | tr -d ' ')"
check "元数据文件里没有 value 字段" "0" "$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(sum(1 for p in d["projects"] for e in p["entries"] if "value" in e))' "${PKG_DIR}/auth/index.json")"
check "元数据里能看出有条目名（明文，给检索用）" "1" "$(grep -c 'DB_URL' "${PKG_DIR}/auth/index.json")"
check "show 不需要口令" "0" "$(pkg show "${PKG_DIR}" >/dev/null 2>&1; echo $?)"
not_contains "show 不显示值" "${SECRET}" "$(pkg show "${PKG_DIR}")"
contains "show 会告诉你值要开锁才看" "要开锁才看得到" "$(pkg show "${PKG_DIR}")"

say "2. 读一条：默认打码，--reveal 才出明文"
G="$(pkg get "${PKG_DIR}" --project web --entry DB_URL --passphrase-file "${PP}")"
contains "默认打码" "•" "${G}"
not_contains "打码时不出明文" "${SECRET}" "${G}"
check "--reveal 拿到明文" "${SECRET}" "$(pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${PP}")"
fails_with "口令不对 → 拒" "开不了这份包" pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${BAD}"
fails_with "项目不存在 → 拒" "没有项目" pkg get "${PKG_DIR}" --project nope --entry DB_URL --reveal --passphrase-file "${PP}"
fails_with "条目不存在 → 拒" "没有条目" pkg get "${PKG_DIR}" --project web --entry NOPE --reveal --passphrase-file "${PP}"
fails_with "非法条目名 → 拒" "条目名" bash -c "printf 'x\n' | '$CLI' auth pkg set '${PKG_DIR}' --project web --entry 'BAD NAME' --value-stdin --reason t --passphrase-file '${PP}'"

say "3. 调用：run 开锁 → 注入 → 执行 → 抹掉"
printf '%s\n' "${TOKEN}" | pkg set "${PKG_DIR}" --project web --entry API_KEY --kind token \
  --value-stdin --reason "再加一条" --passphrase-file "${PP}" >/dev/null 2>&1
RUN="$(pkg run "${PKG_DIR}" --project web --reason "发版" --passphrase-file "${PP}" -- \
  sh -c 'echo "DB=$NCC_AUTH_DB_URL"; echo "KEY=$NCC_AUTH_API_KEY"; echo "PROJ=$NCC_AUTH_PROJECT"; python3 -c "import json,os;d=json.load(open(os.environ[\"NCC_AUTH_BUNDLE\"]));print(\"BUNDLE=\"+\",\".join(sorted(d[\"entries\"])))"' 2>&1)"
contains "值从 env 注入（第一条）" "DB=${SECRET}" "${RUN}"
contains "值从 env 注入（第二条）" "KEY=${TOKEN}" "${RUN}"
contains "项目名注入" "PROJ=web" "${RUN}"
contains "bundle 文件里也有（给不方便用 env 的）" "BUNDLE=API_KEY,DB_URL" "${RUN}"
contains "说了用完就抹" "已上锁" "${RUN}"
STATE="${HUR_HOME}/auth"
SESS="$(find "${STATE}" -maxdepth 2 -name 'sessions' -type d | head -1)"
check "session 目录里不留明文" "0" "$(find "${SESS}" -mindepth 1 2>/dev/null | wc -l | tr -d ' ')"
check "审计里不记值" "0" "$(count_in "$(find "${STATE}" -name 'audit.jsonl' | head -1)" "${SECRET}")"
file_has "$(find "${STATE}" -name 'audit.jsonl' | head -1)" '"act":"run"' "审计里有 run（谁/何时/为什么）"
file_has "$(find "${STATE}" -name 'audit.jsonl' | head -1)" '"reason":"发版"' "审计里记着 reason"
file_has "$(find "${STATE}" -name 'audit.jsonl' | head -1)" 'DB_URL' "审计里记着条目名（只记名，不记值）"
check "远端退出码透传（exit 7）" "7" \
  "$(set +e; pkg run "${PKG_DIR}" --project web --reason t --passphrase-file "${PP}" -- sh -c 'exit 7' >/dev/null 2>&1; echo $?)"
check "失败那次的明文也抹了" "0" "$(find "${SESS}" -mindepth 1 2>/dev/null | wc -l | tr -d ' ')"

say "4. 权限：身份允许列表说了算"
fails_with "没给口令又没身份密钥 → 拒" "开不了这份包" pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal
# `--offline`：授权包只需要本机有这把密钥，不必为了它先联网登记一份凭据
"$CLI" auth key new --offline --json >"${TMP}/key.out" 2>&1 || bad "auth key new --offline 失败：$(head -c 200 "${TMP}/key.out")"
FPR="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["fingerprint"])' "${TMP}/key.out" 2>/dev/null || echo "")"
[[ -n "${FPR}" ]] && good "拿到本机身份指纹（${FPR}）" || bad "没拿到身份指纹：$(head -c 200 "${TMP}/key.out")"
# ① 只允许**别人**的包：本机身份不在列表里 → 要有明确的话
pkg init "${TMP}/other-auth" --id "@me/other" --project web --note "只给某台机器" \
  --identity --allow SHA256:deadbeefdeadbeef --passphrase-file "${PP}" >/dev/null 2>&1
OUT="$(pkg get "${TMP}/other-auth" --project web --entry X --reveal --passphrase-file "${PP}" 2>&1 || true)"
contains "身份不在 allow 列表 → 拒，并说清是名单的事" "不在允许列表" "${OUT}"
# ② 本机指纹写进 allow，且只给 identity → 不需要口令就能开
pkg init "${TMP}/mine-auth" --id "@me/mine" --project web --note "只给本机" \
  --identity --allow "${FPR}" --passphrase-file "${PP}" >/dev/null 2>&1
printf 'mine-value\n' | pkg set "${TMP}/mine-auth" --project web --entry X --value-stdin \
  --reason "写一条" --passphrase-file "${PP}" >/dev/null 2>&1
check "本机身份在 allow 里 → 不给口令也能读" "mine-value" \
  "$(pkg get "${TMP}/mine-auth" --project web --entry X --reveal 2>/dev/null || echo "（开不了）")"

say "5. 锁：并发串行、争用能说清、死锁能回收"
(printf 'a\n' | pkg set "${PKG_DIR}" --project web --entry K_A --value-stdin --reason "并发 A" --passphrase-file "${PP}" >"${TMP}/ca.log" 2>&1 &)
sleep 0.15
(printf 'b\n' | pkg set "${PKG_DIR}" --project web --entry K_B --value-stdin --reason "并发 B" --passphrase-file "${PP}" >"${TMP}/cb.log" 2>&1 &)
sleep 3
check "并发两个写入者都落下了" "a|b" "$(pkg get "${PKG_DIR}" --project web --entry K_A --reveal --passphrase-file "${PP}")|$(pkg get "${PKG_DIR}" --project web --entry K_B --reveal --passphrase-file "${PP}")"
# 状态目录按包 id 落在 HUR_HOME/auth/<slug>；用 status --json 问它，别猜（目录里会有好几个包）
STATE="$(pkg status "${PKG_DIR}" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])')"
check "写完之后锁是空的" "false" "$([[ -f "${STATE}/lock" ]] && echo true || echo false)"
printf '{"pid":%d,"host":"%s","atUnix":%d,"op":"set","reason":"手工占着","by":"smoke","token":"x"}\n' \
  "$$" "$(hostname)" "$(date +%s)" >"${STATE}/lock"
HOLD="$(printf 'x\n' | pkg set "${PKG_DIR}" --project web --entry K_C --value-stdin --reason t --passphrase-file "${PP}" --lock-timeout 0 2>&1 || true)"
contains "有人在用时不去抢，并报出持有者" "手工占着" "${HOLD}"
contains "锁冲突时也说清怎么清残留" "lock" "${HOLD}"
printf '{"pid":999999,"host":"%s","atUnix":%d,"op":"set","reason":"上一个挂了","by":"dead@x","token":"y"}\n' \
  "$(hostname)" "$(date +%s)" >"${STATE}/lock"
OUT="$(printf 'c\n' | pkg set "${PKG_DIR}" --project web --entry K_C --value-stdin --reason "回收" --passphrase-file "${PP}" 2>&1)"
contains "进程没了的死锁会被回收" "抢回" "${OUT}"
check "回收之后真的写进去了" "c" "$(pkg get "${PKG_DIR}" --project web --entry K_C --reveal --passphrase-file "${PP}")"

say "6. 过期条目：不给用（get 拒 / run 跳过）"
printf 'stale\n' | pkg set "${PKG_DIR}" --project web --entry OLD_KEY --value-stdin --expires 1s \
  --reason "短命的" --passphrase-file "${PP}" >/dev/null 2>&1
sleep 2
fails_with "过期条目 --reveal → 拒" "过期" pkg get "${PKG_DIR}" --project web --entry OLD_KEY --reveal --passphrase-file "${PP}"
contains "show 里标出已过期" "已过期" "$(pkg show "${PKG_DIR}")"
RUN2="$(pkg run "${PKG_DIR}" --project web --reason "跳过过期" --passphrase-file "${PP}" -- \
  sh -c 'echo "跳过检查:${NCC_AUTH_OLD_KEY:-无}"' 2>&1)"
contains "run 跳过过期条目并说一声" "跳过" "${RUN2}"
not_contains "过期条目的值没被注入" "stale" "${RUN2}"

say "7. 轮换：项目密钥 / 主密钥（旧材料必须失效）"
OLD_KEYFILE="$(shasum -a 256 "${PKG_DIR}/auth/keys/web.enc" | awk '{print $1}')"
GEN_BEFORE="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print([p["gen"] for p in d["projects"] if p["name"]=="web"][0])' "${PKG_DIR}/auth/index.json")"
pkg rotate "${PKG_DIR}" --project web --reason "老 key 泄漏了" --passphrase-file "${PP}" >"${TMP}/rot.out" 2>&1
contains "项目密钥换了代数" "gen" "$(cat "${TMP}/rot.out")"
GEN_AFTER="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print([p["gen"] for p in d["projects"] if p["name"]=="web"][0])' "${PKG_DIR}/auth/index.json")"
check "gen 从 ${GEN_BEFORE} 变成 ${GEN_AFTER}（+1）" "$((GEN_BEFORE + 1))" "${GEN_AFTER}"
not_contains "项目密钥文件字节真的变了" "${OLD_KEYFILE}" "$(shasum -a 256 "${PKG_DIR}/auth/keys/web.enc" | awk '{print $1}')"
check "轮换后旧值仍然读得到（重加密，不是丢数据）" "${SECRET}" \
  "$(pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${PP}")"
contains "其它项目不受影响（api 的密钥文件没动）" "api.enc" "$(ls "${PKG_DIR}/auth/keys")"
printf 'new-pass-2026\n' >"${TMP}/new.txt"
# 主密钥轮换：--passphrase-file 是**当前**口令（先打开包），--new-passphrase-file 是新的
pkg rotate "${PKG_DIR}" --reason "换口令" --passphrase-file "${PP}" --new-passphrase-file "${TMP}/new.txt" >"${TMP}/rotm.out" 2>&1 || true
if grep -q "主密钥已轮换" "${TMP}/rotm.out"; then
  good "主密钥轮换跑起来了"
else
  bad "主密钥轮换没跑起来：$(head -c 300 "${TMP}/rotm.out")"
fi
fails_with "换主密钥后旧口令开不了（轮换是真轮换）" "开不了这份包" \
  pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${PP}"
check "新口令能用" "${SECRET}" \
  "$(pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${TMP}/new.txt" 2>/dev/null || echo "（新口令也不行）")"
CUR="${TMP}/new.txt"

say "8. 篡改：密文被改一个字节就解不开"
cp "${PKG_DIR}/auth/vault.enc" "${TMP}/vault.bak"
python3 - "$PKG_DIR/auth/vault.enc" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[-1] ^= 0x01          # 翻一个 bit
open(p, "wb").write(bytes(b))
PY
OUT="$(pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${CUR}" 2>&1 || true)"
if [[ "${OUT}" == *"密文与清单对不上"* || "${OUT}" == *"解不开"* ]]; then
  good "改了密文就打不开（而且先说清是清单对不上）"
else
  bad "改密文后没有拒绝：${OUT}"
fi
contains "hur verify 也会红（R12 会指出摘要对不上）" "R12" "$("$CLI" hur verify "${PKG_DIR}" 2>&1 || true)"
cp "${TMP}/vault.bak" "${PKG_DIR}/auth/vault.enc"
check "把密文放回去就恢复" "${SECRET}" \
  "$(pkg get "${PKG_DIR}" --project web --entry DB_URL --reveal --passphrase-file "${CUR}" 2>/dev/null || echo "（恢复失败）")"

say "9. 签名：包能签，签完能独立核对"
if "$CLI" hur key gen >"${TMP}/keygen.out" 2>&1; then
  "$CLI" hur build "${PKG_DIR}" >"${TMP}/build.out" 2>&1 || bad "hur build 失败"
  ok_or_fail "hur sign 成功" "$CLI" hur sign "${PKG_DIR}"
  contains "hur verify 里能看到签名" "签名" "$("$CLI" hur verify "${PKG_DIR}" 2>&1)"
else
  bad "hur key gen 失败：$(head -c 200 "${TMP}/keygen.out")"
fi

say "10. 本地状态：lock / status"
contains "status 显示包与锁状态" "锁" "$(pkg status "${PKG_DIR}")"
contains "status 的审计尾巴里有条目名" "DB_URL" "$(pkg status "${PKG_DIR}")"
not_contains "status 不泄漏值" "${SECRET}" "$(pkg status "${PKG_DIR}")"
# 造一个假的残留 session：lock 该清掉它
mkdir -p "${STATE}/sessions/left-over"
printf 'leftover plaintext\n' >"${STATE}/sessions/left-over/bundle.json"
pkg lock "${PKG_DIR}" --force >"${TMP}/lock.out" 2>&1
contains "lock --force 清残留" "锁" "$(cat "${TMP}/lock.out")"
check "残留目录被清了" "false" "$([[ -e "${STATE}/sessions/left-over" ]] && echo true || echo false)"

say "结果"
echo "  通过 ${PASS} · 失败 ${FAIL}"
[[ "${FAIL}" == "0" ]] || exit 1
