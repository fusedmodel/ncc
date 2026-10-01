#!/usr/bin/env bash
# `ncc rsi`（运行时安全与自改进）端到端冒烟。
#
# 验的是这几条（都是刻意的边界，改坏了要有人喊）：
#   · **门在动作之前**：`rsi check` 只裁决不执行；`rsi guard` 先判，block 就真的不跑
#   · **退出码就是裁决**：0 放行 / 10 留痕 / 20 拦住 —— 宿主靠它分支，不能含糊
#   · **无人值守只会更严**：`--unattended` 把 warn 升级成 block（**没有反向开关**）
#   · **不跑偏**：目标里 reject 命中的步 → block；看不出关系 → warn
#   · **偏好**：人写进来的「不要」参与裁决并被计数；`prefer`（建议）不拦人
#   · **账本只记事实**：动作 / 裁决 / 理由 / 目标 / 偏好命中 —— 不记密钥、不记文件内容
#   · **接进宿主**：`hook install` 生成的 shim 真的能被宿主调用（喂 JSON → 退出码）
#
# 全程隔离：项目级 .ncc-rsi 在临时目录，HUR_HOME / NCC_HOME 也在临时目录。
# 用法：bash scripts/rsi-smoke.sh
set -euo pipefail
exec </dev/null

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
export HUR_HOME="${TMP}/hur"
export NCC_HOME="${TMP}/ncc"
WORK="${TMP}/work"
mkdir -p "${HUR_HOME}" "${NCC_HOME}" "${WORK}"

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
unset NCC_RSI_DIR NCC_RSI_UNATTENDED || true

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
rsi() { ( cd "${WORK}" && "$CLI" rsi "$@" ); }
# 拿裁决的退出码（0/10/20）；`check` 的退出码本身就是裁决，所以直接看它
verdict_of() { ( cd "${WORK}" && set +e; "$CLI" rsi check "$@" >/dev/null 2>&1; echo $? ); }
jq_of() {
  local expr="$1"; shift
  ( cd "${WORK}" && "$CLI" rsi check "$@" --json 2>/dev/null ) | python3 -c "import json,sys;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "${expr}"
}

say "0. 建项目级 rsi + 策略可按预设切"
rsi init --preset dev-loose >"${TMP}/init.out" 2>&1 || { bad "init 失败：$(head -c 300 "${TMP}/init.out")"; exit 1; }
good "init 成功"
check "项目级目录建出来了" "true" "$([[ -f "${WORK}/.ncc-rsi/policy.json" ]] && echo true || echo false)"
check "策略文件 0600" "600" "$(stat -f '%Lp' "${WORK}/.ncc-rsi/policy.json" 2>/dev/null || stat -c '%a' "${WORK}/.ncc-rsi/policy.json")"
contains "policy show 说是哪个目录" "${WORK}/.ncc-rsi" "$(rsi policy show)"
contains "policy show 认得预设名" "dev-loose" "$(rsi policy show)"
contains "无人值守是开着的" "无人值守 warn→block：开" "$(rsi policy show)"
contains "policy path 给出文件路径" "policy.json" "$(rsi policy path)"
contains "presets 里有三个预设" "strict-prod" "$(rsi policy presets)"
contains "policy check 体检通过" "没问题" "$(rsi policy check)"
contains "从项目子目录里也找得到这份策略" "dev-loose" "$(cd "${WORK}" && mkdir -p a/b && cd a/b && "$CLI" rsi policy show)"

say "1. 决策门：退出码就是裁决（0 放行 / 10 留痕 / 20 拦住）"
check "普通命令 → 放行" "0" "$(verdict_of --command 'ls -la' --text '看一眼')"
check "危险命令 → 拦住（20）" "20" "$(verdict_of --command 'rm -rf /' --text '清理')"
check "敏感路径被拦" "20" "$(verdict_of --command 'cat x' --path "$HOME/.ssh/id_rsa")"
check "要人确认的（npm publish）→ 留痕" "10" "$(verdict_of --command 'npm publish')"
check "风险词（production）→ 留痕" "10" "$(verdict_of --command 'psql production' --text '改线上数据')"
check "--json 里的 exitCode 与真实退出码一致" "20" "$(jq_of "['exitCode']" --command 'rm -rf /')"
contains "--json 给出裁决" "block" "$(jq_of "['verdict']" --command 'rm -rf /')"
contains "理由里写清了命中哪条规则" "denyCommands" "$(jq_of "['reasons']" --command 'rm -rf /')"
check "没命中规则的普通命令就是放行（不再扫成 warn）" "0" "$(verdict_of --command 'make build')"

say "2. 无人值守：只会更严，不会更松"
rsi policy set --preset unattended-safe >/dev/null   # 换成无人值守那套（默认放行 + 踩信号才拦）
# 无人值守 + 策略要求有目标 + 没有目标 = 拦住（"在干什么"没人说得清）
check "无人值守但没立目标 → 拦住" "20" "$(verdict_of --command 'make build' --unattended)"
contains "说清是「策略要求有目标」" "要求有目标" "$(rsi check --command 'make build' --unattended 2>&1 || true)"
rsi goal set --statement "把 v0.3.0 发上线" --accept "deploy,release,发版,打包" \
  --reject "refactor,rewrite,重写,升级依赖" >/dev/null
check "立了目标之后，普通动作在无人值守下放行（门不是把人锁在外边）" "0" "$(verdict_of --command 'ls' --unattended)"
check "要人确认的事在无人值守下被拦（warn→block）" "20" "$(verdict_of --command 'npm publish' --unattended)"
check "环境变量 NCC_RSI_UNATTENDED=1 同样生效" "20" \
  "$(cd "${WORK}" && set +e; NCC_RSI_UNATTENDED=1 "$CLI" rsi check --command 'npm publish' >/dev/null 2>&1; echo $?)"
contains "升级这件事在理由里说明" "无人值守" "$(jq_of "['reasons']" --command 'npm publish' --unattended)"
contains "标了 upgraded=true" "True" "$(jq_of "['upgraded']" --command 'npm publish' --unattended)"
check "危险动作在无人值守下还是拦住" "20" "$(verdict_of --command 'rm -rf /' --unattended)"

say "3. 不跑偏：目标就是判据"
contains "goal status 看得到目标" "把 v0.3.0 发上线" "$(rsi goal status)"
check "在目标上的步（accept 命中）不被目标拖成 warn" "0" "$(verdict_of --command 'make deploy' --text '发版')"
check "跑偏的步（reject 命中）→ 拦住" "20" "$(verdict_of --command "git checkout -b refactor/core" --text '重写一遍')"
contains "拦住时说的是跑偏" "跑偏" "$(jq_of "['reasons']" --command 'refactor the module' --text '重写')"
contains "账本里记了 goalState=drift" "drift" "$(rsi goal status --json)"
check "无关的步：unattended-safe 里 onOffGoal=allow → 放行" "0" "$(verdict_of --command 'make lint')"
contains "但仍会指出「看不出关系」（不装看不见）" "没看出和当前目标" "$(jq_of "['reasons']" --command 'make lint')"
# 换成 onOffGoal=warn 的策略：同一步应该抬到留痕 —— 证明这个旋钮真的在起作用
python3 - "${WORK}/.ncc-rsi/policy.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["goal"]["onOffGoal"] = "warn"
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY
check "onOffGoal=warn 时，无关的步 → 留痕" "10" "$(verdict_of --command 'make lint')"
rsi policy set --preset unattended-safe >/dev/null

say "4. 偏好：人写进来的「不要」参与裁决"
rsi pref add "不要动 production 数据库" --kind avoid --scope db \
  --evidence "2026-09 出过一次事故" >"${TMP}/pref.out" 2>&1
contains "偏好记下来了" "记下偏好" "$(cat "${TMP}/pref.out")"
check "偏好命中 → 从留痕抬到拦住（策略 onConflict=warn → 无人值守下 block）" "20" \
  "$(verdict_of --command 'psql -c "update production set x=1"' --unattended)"
contains "理由里带了偏好编号" "P-1" "$(jq_of "['reasons']" --command 'psql production' --unattended)"
# 上面两次 check（拦住那次 + 看理由那次）都真的跑过，所以计数是 2：证据要能被翻出来
contains "偏好命中被计数（证据要留得住）" "命中 2" "$(rsi pref ls)"
rsi pref add "提交信息用中文" --kind prefer >/dev/null
check "prefer（建议）不拦人" "0" "$(verdict_of --command 'ls')"
contains "ls 里两条都在" "不要动 production" "$(rsi pref ls)"
rsi pref add "不要改 vendored 目录" --kind avoid >/dev/null
rsi pref rm P-3 >/dev/null
not_contains "rm 真的删掉了" "vendored" "$(rsi pref ls)"

say "5. 守着跑：block 就真的不跑"
MARK="${TMP}/ran.marker"
set +e
OUT="$(rsi guard --reason "试试强行推分支" -- sh -c "git push --force && touch '${MARK}'" 2>&1)"; RC=$?
set -e
check "被拦时 guard 退出码 20" "20" "${RC}"
contains "说清了没有执行" "没有执行" "${OUT}"
check "**命令真的没跑**（标记文件不存在）" "false" "$([[ -e "${MARK}" ]] && echo true || echo false)"
set +e
OUT2="$(rsi guard --reason "跑个放行的" -- sh -c "touch '${MARK}'" 2>&1)"; RC2=$?
set -e
contains "放行的（ls 白名单）执行了" "执行" "${OUT2}"
check "退出码跟随子进程（touch 是 0）" "0" "${RC2}"
set +e
rsi guard --reason "故意失败" -- sh -c "exit 3" >/dev/null 2>&1; RC3=$?
set -e
check "子进程非 0 → guard 也非 0" "3" "${RC3}"
contains "非 0 退出按事故记账" "事故" "$(rsi report)"
contains "--explain 只看不跑" "只看不跑" "$(rsi guard --explain --reason x -- ls -la)"

say "6. 账本与总账（无人值守过后的那一份）"
contains "report 里有决策计数" "决策" "$(rsi report)"
contains "report 里有拦住次数" "拦住" "$(rsi report)"
contains "report 里有事故" "事故" "$(rsi report)"
contains "report 里有偏好命中" "偏好命中" "$(rsi report)"
contains "report --json 给机器读" "\"incidents\"" "$(rsi report --json)"
contains "suggest 从账本里捞反复出现的理由" "denyCommands" "$(rsi pref suggest)"
check "账本文件在项目级目录里" "true" "$([[ -s "${WORK}/.ncc-rsi/ledger.jsonl" ]] && echo true || echo false)"
not_contains "账本里没有环境变量里的密钥（只记动作与理由）" "SECRET_" "$(cat "${WORK}/.ncc-rsi/ledger.jsonl")"
check "--dry 不写账本" "true" \
  "$(BEFORE=$(wc -c <"${WORK}/.ncc-rsi/ledger.jsonl"); rsi check --command 'ls' --dry >/dev/null 2>&1; AFTER=$(wc -c <"${WORK}/.ncc-rsi/ledger.jsonl"); [[ "${BEFORE}" == "${AFTER}" ]] && echo true || echo false)"

say "7. 接进宿主：hook install 生成的 shim 真的能用"
rsi hook install --host claude >"${TMP}/hook.out" 2>&1
SHIM="${WORK}/.ncc-rsi/hooks/pretooluse.sh"
check "shim 落盘" "true" "$([[ -f "${SHIM}" ]] && echo true || echo false)"
check "shim 可执行（0700）" "700" "$(stat -f '%Lp' "${SHIM}" 2>/dev/null || stat -c '%a' "${SHIM}")"
contains "按宿主给了挂法（Claude 的 PreToolUse）" "PreToolUse" "$(cat "${TMP}/hook.out")"
contains "提醒了无人值守怎么开" "NCC_RSI_UNATTENDED" "$(cat "${TMP}/hook.out")"
set +e
rsi hook install --host nosuch >/dev/null 2>&1; HR=$?
set -e
check "不认识的宿主 → 非 0" "1" "${HR}"
# 真的把工具调用 JSON 喂给 shim（宿主就是这么干的）
set +e
( cd "${WORK}" && NCC_BIN="${CLI}" "${SHIM}" <<<"{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"rm -rf /\"}}" >/dev/null 2>&1 ); H1=$?
( cd "${WORK}" && NCC_BIN="${CLI}" "${SHIM}" <<<"{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"ls -la\"}}" >/dev/null 2>&1 ); H2=$?
set -e
check "危险的工具调用：shim 按宿主约定拦（退出码 2）" "2" "${H1}"
check "放行的工具调用：shim 返回 0" "0" "${H2}"
set +e
( cd "${WORK}" && NCC_BIN="${CLI}" NCC_RSI_UNATTENDED=1 "${SHIM}" \
  <<<"{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"npm publish\"}}" >/dev/null 2>&1 ); H3=$?
set -e
check "无人值守下，要人确认的调用也被 shim 拦住" "2" "${H3}"

say "8. 健壮性：读不懂就非 0（别把配置坏了当放行）"
set +e
OUT="$(echo 'not json' | ( cd "${WORK}" && "$CLI" rsi check ) 2>&1)"; RC=$?
set -e
check "坏请求 → 非 0（且不是 0/10/20）" "1" "${RC}"
contains "坏请求说清是 JSON 的问题" "JSON" "${OUT}"
printf '{"version":9}\n' >"${WORK}/.ncc-rsi/policy.json"
set +e
OUT="$(rsi policy check 2>&1)"; RC=$?
set -e
check "不认识的策略版本 → 非 0" "1" "${RC}"
contains "并说清版本对不上" "version" "${OUT}"
rsi policy set --preset strict-prod >/dev/null
check "strict-prod：默认不放行" "20" "$(verdict_of --command 'make anything')"
check "strict-prod：白名单里、而且在目标上的读操作放行" "0" \
  "$(verdict_of --command 'kubectl get pods' --text '准备 deploy 前先看一眼')"
# strict-prod 的 onOffGoal=block：白名单里但和目标无关的动作也不放（生产上只做该做的事）
check "strict-prod：白名单里但跟目标无关 → 也拦" "20" "$(verdict_of --command 'kubectl get pods')"
contains "policy show 认得新策略" "strict-prod" "$(rsi policy show)"
printf '{"version":1,"name":"x","default":"allow","unattended":{},"rules":{"allowCommands":["ls"],"denyCommands":["ls"]},"goal":{},"prefs":{}}\n' >"${TMP}/bad.json"
set +e
OUT="$(rsi policy set --file "${TMP}/bad.json" 2>&1)"; RC=$?
set -e
contains "自相矛盾的策略能被写进去" "已写入" "${OUT}"
set +e
OUT="$(rsi policy check 2>&1)"; RC=$?
set -e
check "但体检会把它挑出来（非 0）" "1" "${RC}"
contains "说清是同一命令两边都写了" "allowCommands" "${OUT}"
rm -rf "${WORK}/.ncc-rsi"
# （说清的原文是「这里**没有**策略文件」——中间夹着 markdown 加粗，别按「没有策略」查）
contains "没有策略文件时按保守默认（不假装安全）" "策略文件" "$(rsi policy show)"
check "没有策略时陌生动作是 warn 而不是 allow" "10" "$(verdict_of --command 'make anything')"

say "结果"
echo "  通过 ${PASS} · 失败 ${FAIL}"
[[ "${FAIL}" == "0" ]] || exit 1
