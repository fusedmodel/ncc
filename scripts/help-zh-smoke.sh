#!/usr/bin/env bash
# 中文帮助冒烟：**帮助与参数报错里不许剩 clap 的英文模板**。
#
# 为什么要这份脚本：clap 4 不做本地化，`Usage:` / `Commands:` / `error: unexpected argument …`
# 这些模板写死在 `clap_builder` 里。我们在 `cli/src/i18n.rs` 里换一次，但那是**对着 clap
# 的模板逐条替换**的 —— clap 升版改了文案、或者有人新加了一条模板，都会**静默退回英文**
# （用户看得到，但没人会在 CI 里发现）。所以这里跑一遍**全命令树**，把英文模板当断言钉住。
#
# 两条纪律：
#   · 查的是 clap 自己的模板，不是"出现英文单词就报" —— 参数名（`--kind`）、
#     `sha256`、`[默认: …]` 里的值本来就该是原文；
#   · 深挖到**三层**子命令（`hur key gen` / `registry config set` 这一层），
#     再深的就是同一套模板，收益归零。
#
# 全程离线、不碰 ~/.ncc（用临时 NCC_HOME）。
# 用法：bash scripts/help-zh-smoke.sh
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
export NCC_HOME="${TMP}/home"

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"
[[ -n "${CLI}" ]] || { echo "找不到 ncc：先在 ncc-cli/cli 里 cargo build，或用 CLI_BIN=/path/to/ncc" >&2; exit 1; }
trap 'rm -rf "${TMP}"' EXIT

PASS=0
FAIL=0
say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
contains() { if [[ "$3" == *"$2"* ]]; then good "$1"; else bad "$1：输出里没有「$2」（实际：$(printf '%s' "$3" | head -c 300)）"; fi; }
not_contains() { if [[ "$3" != *"$2"* ]]; then good "$1"; else bad "$1：不该出现「$2」"; fi; }
nz() { if [[ "${2:-0}" != "0" ]]; then good "$1（退出码 ${2}）"; else bad "$1：本该失败却退出 0"; fi; }

say "1. 帮助与报错里不许剩 clap 的英文模板（全命令树，-h 与 --help 各跑一遍）"
# clap 自己的模板（我们要换掉的那些）——**这条清单对着 clap_builder 的源码**：
#   help_template.rs：Usage: / Commands: / Options: / Arguments: / Possible values:
#   error/format.rs ：error: / tip: / For more information, try / the following required arguments…
#   error/kind.rs   ：几条兜底文案
BOILER='Usage:|Commands:|^Options:|^Arguments:|Possible values:|Print help|Print this message|possible values:|\[default:|\[env:|^error:|^tip:|For more information, try|the following required arguments|unrecognized subcommand|unexpected argument|a value is required for'
OUT="$(HELP_ZH_BOILER="${BOILER}" python3 - "${CLI}" <<'PY'
import os, re, subprocess, sys
cli = sys.argv[1]
bad = re.compile(os.environ["HELP_ZH_BOILER"])
name = re.compile(r'^[a-z][a-z0-9-]*$')

def run(args):
    p = subprocess.run([cli] + args, capture_output=True, text=True, timeout=10, stdin=subprocess.DEVNULL)
    return p.stdout + p.stderr

def subs(prefix):
    m = re.search(r'\n子命令:\n(.*?)(\n用法:|\Z)', run(prefix + ['--help']), re.S)
    if not m:
        return []
    return [x for x in re.findall(r'^  (\S+)', m.group(1), re.M) if name.match(x)]

paths: list[list[str]] = []
def walk(prefix, depth):
    for s in subs(prefix):
        if s == 'help':
            continue
        paths.append(prefix + [s])
        if depth < 3:
            walk(prefix + [s], depth + 1)
walk([], 0)

hits, ran = [], 0
for path in paths:
    for flag in ('-h', '--help'):
        ran += 1
        for line in run(path + [flag]).split('\n'):
            if bad.search(line):
                hits.append(' '.join(path + [flag]) + ' → ' + line.strip()[:120])
print(f"RAN={ran}")
print(f"HITS={len(hits)}")
for h in hits[:10]:
    print(h)
PY
)"
RUN_N="$(printf '%s' "${OUT}" | grep -oE 'RAN=[0-9]+' | cut -d= -f2)"
HIT_N="$(printf '%s' "${OUT}" | grep -oE 'HITS=[0-9]+' | cut -d= -f2)"
if [[ "${HIT_N}" == "0" ]]; then
  good "${RUN_N} 次调用（全命令树的 -h / --help）没有剩英文模板"
else
  bad "${HIT_N} 处还剩英文模板（共 ${RUN_N} 次调用）："
  printf '%s\n' "${OUT}" | tail -n +3 | sed 's/^/      /'
fi

say "2. 常见报错路径逐条看（错误前缀 / 提示 / 用法 / 更多信息）"
case_err() { # case_err <说明> <必须出现> -- <参数…>
  local what="$1" want="$2"; shift 3
  local out rc
  out="$("${CLI}" "$@" 2>&1)"; rc=$?
  nz "${what}：非 0 退出" "${rc}"
  contains "${what}：说了为什么" "${want}" "${out}"
  not_contains "${what}：没有英文 error:" "error:" "${out}"
}

case_err "错子命令" "错误: 没有这个子命令「nope」" -- nope
case_err "错子命令（有相近的）" "提示: 你是不是想用这个子命令" -- huf verfy
case_err "错参数" "错误: 无法识别的参数「--nope」" -- search --nope
case_err "错取值" "「--kind <KIND>」的取值不合法" -- hur init --kind nope
case_err "缺必填" "错误: 缺少必填参数：" -- publish
case_err "少给值" "需要一个值，但没给" -- hur sign --password
case_err "多给值" "多给了一个值" -- hur run --exec=1

say "3. help 子命令与 -h/--help 走同一条路（标题 / 隐式 help / 版本说明都中文）"
OUT="$("${CLI}" help 2>&1)"
contains "顶级 help 有「用法:」" "用法:" "${OUT}"
contains "隐式 help 子命令" "打印帮助（或指定子命令的帮助）" "${OUT}"
contains "选项段标题" "选项:" "${OUT}"
not_contains "没有 Usage:" "Usage:" "${OUT}"
for c in hur huf scaffold registry profile; do
  OUT="$("${CLI}" help "${c}" 2>&1)"
  not_contains "help ${c} 没有 Usage:" "Usage:" "${OUT}"
  not_contains "help ${c} 没有 Commands:" "Commands:" "${OUT}"
done
OUT="$("${CLI}" hur init --help 2>&1)"
contains "长帮助的 help 说明" "打印帮助（\`-h\` 看简版）" "${OUT}"
OUT="$("${CLI}" hur init -h 2>&1)"
contains "短帮助的 help 说明" "打印帮助（\`--help\` 看详细版）" "${OUT}"
contains "默认值标签" "[默认: " "${OUT}"
OUT="$("${CLI}" hur init --help 2>&1)"
contains "可选值标签（长帮助）" "可选值:" "${OUT}"

say "4. 我们自己的中文说明一个字都不许被动（只换 clap 的模板）"
OUT="$("${CLI}" help scaffold 2>&1)"
contains "脚手架说明还在" "一份 md 就是一件制品" "${OUT}"
contains "我们写的边界还在" "服务端只粗筛" "${OUT}"

printf '\n\033[1m结果：%d 通过 / %d 失败\033[0m\n' "${PASS}" "${FAIL}"
[[ "${FAIL}" == "0" ]]
