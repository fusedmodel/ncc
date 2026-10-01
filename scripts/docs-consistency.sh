#!/usr/bin/env bash
# 文档一致性冒烟：**同一个事实写在几个地方"这件事，机器来对一遍**。
#
# 为什么要有它：这两天连着踩到同一类毛病 ——
#   ① 反馈/RSI 工具进了 `ncc mcp`，但 6 份文档里的工具清单是手抄的（抄漏 = 用户以为没有）；
#   ② profile 从 11 涨到 12，4 份文档还写着 11；
#   ③ 12 行 markdown 表格的单元格里用了没转义的 `|`，表格直接塌成错列。
# 都不是"写错字"，而是**同一份事实多份副本、靠人同步**。这份脚本把它们变成可跑的检查。
#
# 三条原则（与仓库其他冒烟一致）：
#   · **权威只有一处**：工具清单 = `agent/harness.json`（它本身被 `cargo test` 的
#     `harness_manifest_lists_exactly_the_exposed_tools` 钉在 `cli/src/mcp.rs` 的 `tools()` 上）；
#     profile 数量 = `ncc hur profile --list`。文档是副本，副本对不上就是副本错。
#   · **能算就离线算**：拿不到 `ncc` 二进制时，只跳过依赖它的那几条，其余照跑（并明说跳过）。
#   · **不装懂**：报"缺什么"要给具体名字，别只说"不一致"。
#
# 用法：bash scripts/docs-consistency.sh      （在 ncc-cli/ 下跑）
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"      # ncc-cli/
REPO="$(cd "${ROOT}/.." && pwd)"              # ncc-ai/
PLAT="${REPO}/ncc-platform"

PASS=0
FAIL=0
SKIP=0
say()  { printf '\n\033[1m%s\033[0m\n' "$1"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }
skip() { printf '  \033[33m•\033[0m %s\n' "$1"; SKIP=$((SKIP + 1)); }
check() { if [[ "$2" == "$3" ]]; then good "$1（$3）"; else bad "$1：期望 $2，实际 $3"; fi; }

find_cli() {
  if [[ -n "${CLI_BIN:-}" ]]; then printf '%s' "${CLI_BIN}"; return; fi
  for cand in "${ROOT}/cli/target/debug/ncc" "${ROOT}/target/debug/ncc"; do
    [[ -x "${cand}" ]] && { printf '%s' "${cand}"; return; }
  done
  printf ''
}
CLI="$(find_cli)"

say "1. markdown 表格：每一行的列数必须与表头一致"
# 单元格里的未转义 `|` 会把表格撑成错列 —— 渲染出来是乱的，但肉眼看源码很难发现。
TABLE_OUT="$(python3 - "${REPO}" <<'PY'
import os, re, sys
root = sys.argv[1]
skip = ('node_modules', '/target/', '/.git/', '/dist/', '/build/', '/data/uploads/')
def cols(l): return l.replace('\\|', '\x00').count('|')
broken, tables = [], 0
for dp, dn, fn in os.walk(root):
    if any(s in dp + '/' for s in skip):
        continue
    for f in fn:
        if not f.endswith('.md'):
            continue
        p = os.path.join(dp, f)
        lines = open(p, encoding='utf-8', errors='replace').read().split('\n')
        i = 0
        while i < len(lines):
            if lines[i].strip().startswith('|') and i + 1 < len(lines) and re.match(r'^\s*\|[\s:|-]+\|\s*$', lines[i + 1]):
                head, j, tables = cols(lines[i]), i + 2, tables + 1
                while j < len(lines) and lines[j].strip().startswith('|'):
                    if cols(lines[j]) != head:
                        broken.append(f"{os.path.relpath(p, root)}:{j+1}（{head}→{cols(lines[j])} 列）")
                    j += 1
                i = j
            else:
                i += 1
print(f"{tables} {len(broken)}")
print('\n'.join(broken[:12]))
PY
)"
TABLE_COUNT="$(printf '%s' "${TABLE_OUT}" | head -1 | awk '{print $1}')"
BROKEN_COUNT="$(printf '%s' "${TABLE_OUT}" | head -1 | awk '{print $2}')"
if [[ "${BROKEN_COUNT}" == "0" ]]; then
  good "${TABLE_COUNT} 张表格，列数全一致"
else
  bad "${TABLE_COUNT} 张表格里有 ${BROKEN_COUNT} 行列数不对："
  printf '%s\n' "${TABLE_OUT}" | tail -n +2 | sed 's/^/      /'
fi

say "2. MCP 工具清单：6 份文档必须与权威清单逐字对齐"
# 权威 = agent/harness.json 的 harness.tools（cargo test 已把它钉在 tools() 上，
# 而 harness.json 列的是**始终存在**的工具；记录仓那几个是目标声明 `store` 才出现的）。
DOC_OUT="$(python3 - "${ROOT}" "${PLAT}" <<'PY'
import json, os, re, sys
root, plat = sys.argv[1], sys.argv[2]
man = json.load(open(os.path.join(root, 'agent/harness.json'), encoding='utf-8'))
authoritative = list(man['harness']['tools'])
docs = {
    'agent/SKILL.md':           os.path.join(root, 'agent/SKILL.md'),
    'agent/README.md':          os.path.join(root, 'agent/README.md'),
    'README.md':                os.path.join(root, 'README.md'),
    'README.zh-CN.md':          os.path.join(root, 'README.zh-CN.md'),
    'web/src/docs/zh/agents.md': os.path.join(plat, 'web/src/docs/zh/agents.md'),
    'web/src/docs/en/agents.md': os.path.join(plat, 'web/src/docs/en/agents.md'),
}
# 免检名单（**故意**不在工具清单里、但文档该提的名字）：
#   · 记录仓三个浏览工具与 ncc_store_* —— 由包声明 `state.stores[]` 动态出现，不进 harness.json
#   · OAuth cookie 名之类的非工具标识
exempt = re.compile(r'^ncc_(list_store_|get_store_|store_)')
tool_re = re.compile(r'\bncc_[a-z0-9_]+\b')
missing_total = 0
for label, path in docs.items():
    if not os.path.exists(path):
        print(f"  ✗ {label}：文件不存在")
        missing_total += 1
        continue
    text = open(path, encoding='utf-8').read()
    found = set(tool_re.findall(text))
    missing = [t for t in authoritative if t not in found]
    extra = sorted(t for t in found if t not in authoritative and not exempt.match(t))
    status = '✓'
    if missing:
        status = '✗'
        missing_total += len(missing)
    print(f"  {status} {label}：{len(authoritative) - len(missing)}/{len(authoritative)} 个工具")
    if missing:
        print(f"      缺：{' '.join(missing)}")
    if extra:
        # 多出来的不一定是错（散文里提到别的命令名），列出来供人判断
        print(f"      文档里还有（不在工具清单，可能是命令名/已删工具）：{' '.join(extra[:8])}")
print(f"MISSING_TOTAL={missing_total}")
PY
)"
printf '%s\n' "${DOC_OUT}"
DOC_MISSING="$(printf '%s' "${DOC_OUT}" | grep -oE 'MISSING_TOTAL=[0-9]+' | cut -d= -f2)"
if [[ "${DOC_MISSING}" == "0" ]]; then
  good "6 份文档的工具清单都与 harness.json 对齐（43 个）"
else
  bad "文档里有 ${DOC_MISSING} 处工具缺失（见上面每行的「缺：…」）"
fi

if [[ -n "${CLI}" ]]; then
  say "3. 权威清单本身：harness.json ↔ 二进制实际暴露的 tools/list"
  REQ='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
  LIVE="$(NCC_HOME="$(mktemp -d)" "${CLI}" mcp <<< "${REQ}" 2>/dev/null | head -1)"
  if [[ -z "${LIVE}" ]]; then
    skip "拿不到 tools/list（ncc mcp 没答）—— 跳过这条"
  else
    # 二进制实际暴露的 = harness.json 那份 + 记录仓浏览三件（目标声明 `store` 才有）。
    # 所以比的是**去掉记录仓那三个之后**的集合，而不是总数 —— 差的正是那三个才对。
    LIVE_JSON="$(mktemp)"
    printf '%s' "${LIVE}" > "${LIVE_JSON}"
    LIVE_OUT="$(python3 - "${ROOT}/agent/harness.json" "${LIVE_JSON}" <<'PY'
import json, sys
man = set(json.load(open(sys.argv[1], encoding='utf-8'))['harness']['tools'])
names = {t['name'] for t in json.load(open(sys.argv[2], encoding='utf-8'))['result']['tools']}
store = {n for n in names if 'store' in n}
core = names - store
print(f"{len(core)}/{len(man)}")
print('EXTRA=' + ' '.join(sorted(core - man)))
print('MISSING=' + ' '.join(sorted(man - core)))
print('STORE=' + ' '.join(sorted(store)))
PY
)"
    rm -f "${LIVE_JSON}"
    CORE_MAN="$(printf '%s' "${LIVE_OUT}" | head -1)"
    check "二进制暴露的**非记录仓**工具 = harness.json 的条数" "$(printf '%s' "${CORE_MAN}" | cut -d/ -f2)" "$(printf '%s' "${CORE_MAN}" | cut -d/ -f1)"
    STORE_LINE="$(printf '%s' "${LIVE_OUT}" | grep '^STORE=' | cut -d= -f2-)"
    if [[ "$(printf '%s' "${STORE_LINE}" | wc -w | tr -d ' ')" == "3" ]]; then
      good "记录仓浏览三件也在（目标声明 store 才出现，故意不进 harness.json）：${STORE_LINE}"
    else
      bad "记录仓浏览工具应该是 3 个，实际：${STORE_LINE:-（无）}"
    fi
    EXTRA_LINE="$(printf '%s' "${LIVE_OUT}" | grep '^EXTRA=' | cut -d= -f2-)"
    MISSING_LINE="$(printf '%s' "${LIVE_OUT}" | grep '^MISSING=' | cut -d= -f2-)"
    check "没有多余工具" "" "${EXTRA_LINE}"
    check "没有漏掉工具" "" "${MISSING_LINE}"
  fi

  say "4. profile 数量：文档里写的数 = 规范里的数"
  PROF_LINE="$("${CLI}" hur profile --list 2>/dev/null | head -1)"
  PROF_N="$(printf '%s' "${PROF_LINE}" | grep -oE '[0-9]+' | head -1)"
  if [[ -z "${PROF_N}" ]]; then
    skip "拿不到 profile 清单 —— 跳过这条"
  else
    MISM="$(python3 - "${REPO}" "${PROF_N}" <<'PY'
import os, re, sys
root, want = sys.argv[1], sys.argv[2]
skip = ('node_modules', '/target/', '/.git/', '/dist/', '/build/', '/data/uploads/')
pat = re.compile(r'(\d+)\s*(?:个\s*profile|profiles?\b)', re.I)
hits = []
for dp, dn, fn in os.walk(root):
    if any(s in dp + '/' for s in skip):
        continue
    for f in fn:
        if not f.endswith('.md'):
            continue
        p = os.path.join(dp, f)
        for i, l in enumerate(open(p, encoding='utf-8', errors='replace').read().split('\n'), 1):
            for m in pat.finditer(l):
                if m.group(1) != want:
                    hits.append(f"{os.path.relpath(p, root)}:{i} 写着 {m.group(1)}")
print(len(hits))
for h in hits[:12]:
    print(h)
PY
)"
    MISM_N="$(printf '%s' "${MISM}" | head -1)"
    if [[ "${MISM_N}" == "0" ]]; then
      good "文档里的 profile 数量都对（${PROF_N}）"
    else
      bad "有 ${MISM_N} 处写的不是 ${PROF_N}："
      printf '%s\n' "${MISM}" | tail -n +2 | sed 's/^/      /'
    fi
  fi
else
  say "3-4. 依赖 ncc 二进制的两条"
  skip "没找到 ncc（先 cargo build，或 CLI_BIN=/path/to/ncc）—— 跳过"
fi

say "结果"
printf '  通过 %s · 失败 %s · 跳过 %s\n' "${PASS}" "${FAIL}" "${SKIP}"
[[ "${FAIL}" -eq 0 ]]
