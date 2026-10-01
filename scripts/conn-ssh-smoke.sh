#!/usr/bin/env bash
# SSH 通道（`ncc conn --ssh`）端到端冒烟：**零服务端改动**，目标端只要有 sshd。
#
# 验的是这几条（都是刻意的边界，改坏了要有人喊）：
#   · **复用你本机的 ssh**：参数拼对（-p / -i / BatchMode）、经 `~/.ssh/config` 与 agent 照旧生效
#     —— 我们只当搬运工，不引 SSH 库、不自己管 key
#   · **退出码如实透传**：远端 `exit 5` → CLI 退出码 5（CI 可判）
#   · **文件面**：push 之后**在目标端核 sha256**；pull 回一致；`..` 与绝对路径在客户端就被挡
#   · **账本落在目标机**（`.ncc-ledger.jsonl`，谁/何时/要了什么/结果）——不经云端
#   · **TTL 是软约束**：到点后 CLI 自己拒绝（sshd 不知道我们在计时）
#   · **`close --purge` 只删认得出来的目录**（里面有 `.ncc-channel.json` 才删）
#
# 大多数用例用**替身 ssh**（NCC_SSH_BIN）：确定、不依赖本机有没有 sshd。
# 本机要是开着「远程登录」，最后会再跑一遍**真 ssh** 的端到端（没开就明确跳过，不装成通过）。
#
# 全程隔离：NCC_HOME / 假 HOME（替身 ssh 的"远端"）都在临时目录。
# 用法：bash scripts/conn-ssh-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
export NCC_HOME="${TMP}/ncc"          # 本地登记（含 ~/.ncc/connections.json）
export FAKE_HOME="${TMP}/remote"      # 替身 ssh 眼里的"远端 home"
export STUB_LOG="${TMP}/ssh-argv.log"
mkdir -p "${FAKE_HOME}" "${NCC_HOME}"

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
SKIP=0
cleanup() {
  if [[ -n "${KEEP:-}" ]]; then echo "（KEEP=1）保留 ${TMP}"; else rm -rf "${TMP}"; fi
}
trap cleanup EXIT

say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
skip() { printf '  \033[33m–\033[0m %s\n' "$1"; SKIP=$((SKIP + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 240)）"; fi; }
not_contains() { if [[ "$3" == *"$2"* ]]; then bad "$1：不该出现「$2」"; else good "$1"; fi; }
file_has() { if grep -q -- "$2" "$1" 2>/dev/null; then good "$3"; else bad "$3：$1 里没有「$2」"; fi; }
# 期望**失败**的命令：把输出抓下来再判（⚠️ 不能写成 `cmd | grep -q` ——
# `set -o pipefail` 下「命令自己失败」会盖掉 grep 的成功，断言就会永远为假）。
fails_with() {
  local desc="$1" needle="$2"; shift 2
  local out rc
  if out="$("$@" 2>&1)"; then rc=0; else rc=$?; fi
  if [[ "${rc}" == "0" ]]; then bad "${desc}：本该失败却成功了"; return; fi
  contains "${desc}（退出码 ${rc}）" "${needle}" "${out}"
}

# 替身 ssh（见文件头：够用就好）
mkdir -p "${TMP}/bin"
cat >"${TMP}/bin/ssh" <<'STUB'
#!/bin/sh
# 替身 ssh：解析我们真正会传的参数，然后在假 HOME 里把远端命令跑了。
LOG="${STUB_LOG:?}"; ROOT="${FAKE_HOME:?}"
printf '%s\n' "$*" >>"$LOG"
port=""; ident=""; batch=""; target=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) shift; case "$1" in BatchMode=*) batch="$1" ;; esac; shift ;;
    -p) port="$2"; shift 2 ;;
    -i) ident="$2"; shift 2 ;;
    -*) shift ;;
    *) if [ -z "$target" ]; then target="$1"; shift; else break; fi ;;
  esac
done
[ -z "$port" ] || printf 'port=%s\n' "$port" >>"$LOG"
[ -z "$ident" ] || printf 'ident=%s\n' "$ident" >>"$LOG"
[ -z "$batch" ] || printf 'batch=%s\n' "$batch" >>"$LOG"
host="${target#*@}"
if [ "$host" = "nohost" ]; then
  echo "ssh: Could not resolve hostname nohost: nodename nor servname provided, or not known" >&2
  exit 255
fi
cd "$ROOT" || exit 70
SSH_USER="${target%%@*}" sh -c "$*"
STUB
chmod +x "${TMP}/bin/ssh"
export PATH="${TMP}/bin:${PATH}"
export NCC_SSH_BIN="${TMP}/bin/ssh"

conn() { "$CLI" conn "$@"; }

say "0. 参数解析：本机的 ssh 怎么被调用的"
# --ssh 但没给名字 → 缺省用主机名当通道名；顺手验 -p / -i / BatchMode 都传到了
conn open --ssh deploy@fake-host:2222 --identity "${TMP}/id_key" --batch --name web \
  --note "冒烟：SSH 通道" >"${TMP}/open.out" 2>&1 || { bad "open 失败：$(head -c 300 "${TMP}/open.out")"; }
good "open 成功（SSH，--batch）"
file_has "${STUB_LOG}" "port=2222" "端口传给了 ssh（-p 2222）"
file_has "${STUB_LOG}" "ident=${TMP}/id_key" "私钥传给了 ssh（-i）"
file_has "${STUB_LOG}" "batch=BatchMode=yes" "CI 模式传了 BatchMode=yes"
contains "输出里说了不经云端 / 目标端不用装 ncc" "目标端也不需要装 ncc" "$(cat "${TMP}/open.out")"
conn open --ssh root@other-host --name other --ttl 600 >/dev/null 2>&1
contains "ls 里能看到 transport=ssh" "deploy@fake-host:2222" "$(conn ls)"
contains "ls 里有第一条通道" "web" "$(conn ls)"
contains "ls 里有第二条通道（另一个目标）" "other" "$(conn ls)"

say "1. 建通道 = 在目标机上建目录 + 写元数据（零服务端改动）"
R_DIR="${FAKE_HOME}/.ncc/conn/web"
check "目标机上有通道目录" "true" "$([[ -d "${R_DIR}" ]] && echo true || echo false)"
check "目录权限 700" "700" "$(stat -f '%Lp' "${R_DIR}" 2>/dev/null || stat -c '%a' "${R_DIR}")"
check "元数据写对了（name=web）" "web" \
  "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["name"])' "${R_DIR}/.ncc-channel.json")"
check "元数据里写着 transport=ssh" "ssh" \
  "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["transport"])' "${R_DIR}/.ncc-channel.json")"
contains "本地登记里没有凭据泄漏（key 为空）" "\"key\": \"\"" "$(cat "${NCC_HOME}/.ncc/connections.json")"

say "2. 通道上执行：退出码跟随远端"
OUT="$(conn exec web "echo ssh-hello && pwd" --reason "冒烟：SSH 上跑一条" 2>&1)"
contains "命令输出回来了" "ssh-hello" "${OUT}"
contains "确实在通道目录里跑（目标端 cwd）" "ncc/conn/web" "${OUT}"
if conn exec web "exit 5" --reason "冒烟：远端非 0" >/dev/null 2>&1; then
  bad "远端退出码非 0 时 CLI 却返回 0（CI 会误判成功）"
else
  good "远端退出码非 0 → CLI 也非 0"
fi
check "exit 5 如实透传成 5" "5" \
  "$(set +e; conn exec web "exit 5" --reason "冒烟" >/dev/null 2>&1; echo $?)"
contains "子目录不存在时说得清楚" "子目录不存在" "$(conn exec web "echo x" --cwd sub --reason "冒烟" 2>&1 || true)"
fails_with "ssh 连不上时不当成远端命令失败" "ssh 本身就没连上" conn open --ssh deploy@nohost --name dead

say "3. 文件面：推 → 目标端核指纹 → 拉回来"
printf 'v1.2.0\n' >"${TMP}/app.txt"
conn push web "${TMP}/app.txt" --reason "冒烟：推版本号" >"${TMP}/push.out" 2>&1
contains "推送成功（带指纹）" "指纹" "$(cat "${TMP}/push.out")"
check "远端文件内容对" "v1.2.0" "$(tr -d '\n' <"${R_DIR}/app.txt")"
LOCAL_SHA="$(shasum -a 256 "${TMP}/app.txt" | awk '{print $1}')"
REMOTE_SHA="$(shasum -a 256 "${R_DIR}/app.txt" | awk '{print $1}')"
check "两端指纹一致（字节即事实）" "${LOCAL_SHA}" "${REMOTE_SHA}"
conn push web "${TMP}/app.txt" --to nested/deep/app.txt --reason "冒烟：嵌套目录" >/dev/null 2>&1
check "嵌套目录会自动建出来" "v1.2.0" "$(tr -d '\n' <"${R_DIR}/nested/deep/app.txt")"
conn pull web app.txt --to "${TMP}/pulled.txt" >/dev/null 2>&1
check "拉回来一致" "v1.2.0" "$(tr -d '\n' <"${TMP}/pulled.txt")"

say "4. 路径门禁：不许跳出通道目录（客户端就挡）"
fails_with "pull ../x → 被拒" "不许跳出通道目录" conn pull web ../x --to "${TMP}/x"
fails_with "pull 绝对路径 → 被拒" "只接受相对路径" conn pull web /etc/passwd --to "${TMP}/x"
fails_with "push --to /abs → 被拒" "只接受相对路径" conn push web "${TMP}/app.txt" --to /tmp/abs.txt --reason "冒烟：绝对路径"
check "真的没写到外面去" "false" "$([[ -e "${TMP}/x" ]] && echo true || echo false)"

say "5. 一批：推文件 + 跑脚本（用户场景）"
printf 'echo "batch ok: $(cat payload.txt)"\n' >"${TMP}/batch.sh"
if conn run web --file "${TMP}/app.txt=payload.txt" --script "${TMP}/batch.sh" \
     --reason "冒烟：一批做完" >"${TMP}/run.out" 2>&1; then
  good "conn run 退出码 0"
  contains "脚本读到了推过去的文件" "batch ok: v1.2.0" "$(cat "${TMP}/run.out")"
else
  bad "conn run 失败：$(head -c 300 "${TMP}/run.out")"
fi
contains "脚本是显式 sh 跑的（不靠可执行位）" "sh .ncc-run.sh" "$(cat "${STUB_LOG}")"

say "6. 账本：落在目标机上，不经云端"
L="${R_DIR}/.ncc-ledger.jsonl"
check "目标机上有账本文件" "true" "$([[ -f "${L}" ]] && echo true || echo false)"
file_has "${L}" '"act":"exec"' "账本里有 exec 记录"
file_has "${L}" '"act":"put"' "账本里有 put 记录"
file_has "${L}" '"reason":"冒烟：SSH 上跑一条"' "账本里记着「为什么」（reason）"
file_has "${L}" '"exit":5' "账本里记着退出码"
file_has "${L}" '"by":"' "账本里记着「谁」"
check "账本没落到本地（数据面不过托管云、也不在本地堆）" "false" \
  "$([[ -e "${NCC_HOME}/.ncc/conn" ]] && echo true || echo false)"
ST="$(conn status web)"
contains "status 显示 transport=ssh" "ssh" "${ST}"
contains "status 显示目标" "deploy@fake-host:2222" "${ST}"
contains "status 读得到账本" "冒烟：SSH 上跑一条" "${ST}"

say "7. TTL：SSH 上是软约束，到点 CLI 自己拒绝"
conn open --ssh deploy@fake-host --name ttl --ttl 1 >/dev/null 2>&1
sleep 2
contains "过期后 exec 被拒并说明是软约束" "过期" "$(conn exec ttl "echo x" --reason "冒烟" 2>&1 || true)"
contains "过期后 ls 标成 expired" "expired" "$(conn ls)"
contains "status 里给了重开的办法" "重开一条" "$(conn status ttl)"

say "8. 收线：普通 close 不动远端；--purge 只删认得出来的目录"
conn close web >/dev/null 2>&1
check "普通 close 不删远端目录" "true" "$([[ -f "${R_DIR}/app.txt" ]] && echo true || echo false)"
contains "close 说明了远端什么都没动" "什么都没动" "$(conn close web 2>&1)"
# 先把元数据拿走：这时目录已经「认不出来」了
rm -f "${R_DIR}/.ncc-channel.json"
contains "认不出来的目录 → 拒删" "不肯删" "$(conn close web --purge 2>&1 || true)"
check "拒删之后目录还在" "true" "$([[ -d "${R_DIR}" ]] && echo true || echo false)"
# 放回去再删：应当真的删掉
printf '{"name":"web"}\n' >"${R_DIR}/.ncc-channel.json"
conn close web --purge >/dev/null 2>&1
check "认得出来 → 真删了工作目录" "false" "$([[ -e "${R_DIR}" ]] && echo true || echo false)"
contains "本地登记默认留着" "web" "$(conn ls)"
conn close web --forget >/dev/null 2>&1
not_contains "--forget 才清掉本地登记" "web" "$(conn ls)"

say "9. 真 ssh（本机开了「远程登录」才跑；没开就明确跳过）"
if ssh -o BatchMode=yes -o ConnectTimeout=2 localhost true >/dev/null 2>&1; then
  RH="$(mktemp -d)"
  RDIR="${RH}/ssh-real-$$/.ncc/conn/real"
  if conn open --ssh "localhost" --dir "${RDIR}" --name real --ttl 300 >/dev/null 2>&1; then
    good "真 ssh：建通道成功（${RDIR}）"
    contains "真 ssh：exec 有输出" "real-ok" \
      "$(conn exec real "echo real-ok && pwd" --reason "冒烟：真 ssh" 2>&1)"
    printf 'real\n' >"${TMP}/real.txt"
    conn push real "${TMP}/real.txt" --reason "冒烟：真 ssh 推文件" >/dev/null 2>&1
    check "真 ssh：推上去的内容对" "real" "$(tr -d '\n' <"${RDIR}/real.txt")"
    conn pull real real.txt --to "${TMP}/real-back.txt" >/dev/null 2>&1
    check "真 ssh：拉回来一致" "real" "$(tr -d '\n' <"${TMP}/real-back.txt")"
    conn close real --purge >/dev/null 2>&1
    check "真 ssh：--purge 删掉了远端目录" "false" "$([[ -e "${RDIR}" ]] && echo true || echo false)"
    rm -rf "${RH}"
  else
    bad "真 ssh：本机 localhost 能连上，但 open 失败（看上面的错误）"
  fi
else
  skip "本机没开「远程登录」（sshd）—— 真 ssh 的端到端本次没跑"
fi

say "结果"
echo "  通过 ${PASS} · 失败 ${FAIL} · 跳过 ${SKIP}"
[[ "${FAIL}" == "0" ]] || exit 1
