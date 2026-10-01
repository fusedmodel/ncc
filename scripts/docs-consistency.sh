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

say "5. 工具数量说法：写「N 个工具」的地方必须与权威一致"
# 今天抓到过 6 处还写着 38（首页 / CLI 页 / 站内 services / 竞品 PRD）—— 数字是最容易
# 烂的那类文案：它在 5 个地方各写一遍，改工具面时没人会想起来。这里只认两个数：
# 43（始终存在的工具）与 46（+ 记录仓浏览三件，只有目标声明 store 才有）。
COUNT_OUT="$(python3 - "${PLAT}" <<'PY'
import os, re, sys
plat = sys.argv[1]
CORE = 43
hits = []
pat = re.compile(r'(\d+)\s*(?:个\s*工具|tools\b)')
skip = ('node_modules', '/target/', '/.git/', '/dist/', '/build/', '/data/', '/release/')
roots = ['web/src', 'web/public', 'prd', 'README.md', 'CHANGELOG.md']
for rel in roots:
    base = os.path.join(plat, rel)
    targets = []
    if os.path.isfile(base):
        targets = [('', base)]
    else:
        for dp, dn, fn in os.walk(base):
            if any(s in dp + '/' for s in skip):
                continue
            for f in fn:
                if f.endswith(('.md', '.jsx', '.js', '.html', '.txt')):
                    targets.append((dp, os.path.join(dp, f)))
    for dp, p in targets:
        name = os.path.relpath(p, plat)
        if 'CHANGELOG' in name:      # 更新日志天生会说“以前是 37/38”，不判
            continue
        for i, line in enumerate(open(p, encoding='utf-8', errors='replace').read().split('\n'), 1):
            for m in pat.finditer(line):
                n = int(m.group(1))
                if n not in (CORE, CORE + 3):
                    hits.append(f"{name}:{i} 写着 {n}")
print(f"BAD={len(hits)}")
for h in hits[:12]:
    print('  ' + h)
PY
)"
printf '%s\n' "${COUNT_OUT}" | tail -n +2
BAD_N="$(printf '%s' "${COUNT_OUT}" | grep -oE 'BAD=[0-9]+' | cut -d= -f2)"
if [[ "${BAD_N}" == "0" ]]; then
  good "所有「N 个工具」的说法都是 43 或 46"
else
  bad "有 ${BAD_N} 处工具数量写得不对（应为 43，或含记录仓的 46）：见上"
fi

say "6. JSX 文案里不该有 markdown 强调标记（会原样显示成星号）"
# 前端把文案当纯文本渲染（不是 markdown），所以 `**重点**` 会连星号一起显示出来。
# ⚠️ 站点只能直接读文档：**`.md` 里的 `**` 是对的**（react-markdown 渲染），只有 `.jsx`
# 里的字符串需要自查；注释行（`//` / `{/* … */}`）不算。
JSX_OUT="$(python3 - "${PLAT}/web/src" <<'PY'
import os, re, sys
base = sys.argv[1]
hits = []
for dp, dn, fn in os.walk(base):
    for f in fn:
        if not f.endswith('.jsx'):
            continue
        p = os.path.join(dp, f)
        in_block = False            # 跨行的 {/* … */} 要一直跳到 */}
        for i, line in enumerate(open(p, encoding='utf-8', errors='replace').read().split('\n'), 1):
            s = line.strip()
            if in_block:
                in_block = '*/}' not in s and '*/' not in s
                continue
            if '{/*' in s and '*/}' not in s:
                in_block = True
                continue
            if s.startswith('//') or s.startswith('/*') or s.startswith('*') or s.startswith('{/*'):
                continue
            if '**' in re.sub(r'\{/\*.*?\*/\}', '', line):
                hits.append(f"{os.path.relpath(p, base)}:{i}")
print(f"BAD={len(hits)}")
for h in hits[:12]:
    print('  ' + h)
PY
)"
printf '%s\n' "${JSX_OUT}" | tail -n +2
JSX_BAD="$(printf '%s' "${JSX_OUT}" | grep -oE 'BAD=[0-9]+' | cut -d= -f2)"
if [[ "${JSX_BAD}" == "0" ]]; then
  good "JSX 文案里没有字面星号"
else
  bad "有 ${JSX_BAD} 处 JSX 文案带 markdown 强调标记（会显示成星号）：见上"
fi

say "7. 缩写展开：NCC = Neural Cloud Computers（旧展开不许残留）"
# 2026-10-01 用户口径改过一次（原为 Neural · Capability · Catalog）—— 展开词出现在 7 个文件里，
# 靠人记必漏。这里两头都查：该有的地方得有新展开，旧展开一个字都不许留。
EXP_OUT="$(python3 - "${REPO}" "${PLAT}" <<'PY'
import os, re, sys
repo, plat = sys.argv[1], sys.argv[2]
NEW = 'Neural Cloud Computers'
OLD = re.compile(r'Neural\s*[·・]\s*Capability\s*[·・]\s*Catalog')
# 该带新展开的落点（PRD §10 的清单）
must = [
    os.path.join(repo, 'README.md'),
    os.path.join(plat, 'README.md'),
    os.path.join(plat, 'prd/README.md'),
    os.path.join(plat, 'prd/ncc-agent-infra.md'),
    os.path.join(plat, 'web/index.html'),
    os.path.join(plat, 'web/public/llms.txt'),
    os.path.join(plat, 'web/src/pages/Landing.jsx'),
    os.path.join(plat, 'web/src/docs/zh/registry.md'),
    os.path.join(plat, 'web/src/docs/en/registry.md'),
]
bad = []
for p in must:
    if not os.path.exists(p):
        bad.append(f"{os.path.relpath(p, repo)} 不存在")
        continue
    t = open(p, encoding='utf-8', errors='replace').read()
    if NEW not in t:
        bad.append(f"{os.path.relpath(p, repo)} 没有新展开")
# 旧展开（**完整三连**才算；PRD 决策记录里单独提 `Capability · Catalog` 是允许的）：全仓不许再出现
skip = ('node_modules', '/target/', '/.git/', '/dist/', '/build/', '/data/', '/release/')
for dp, dn, fn in os.walk(repo):
    if any(s in dp + '/' for s in skip):
        continue
    for f in fn:
        if not f.endswith(('.md', '.jsx', '.js', '.html', '.txt', '.json')):
            continue
        p = os.path.join(dp, f)
        for i, line in enumerate(open(p, encoding='utf-8', errors='replace').read().split('\n'), 1):
            if OLD.search(line):
                bad.append(f"{os.path.relpath(p, repo)}:{i} 还有旧展开：{line.strip()[:60]}")
print(f"BAD={len(bad)}")
for b in bad[:12]:
    print('  ' + b)
PY
)"
printf '%s\n' "${EXP_OUT}" | tail -n +2
EXP_BAD="$(printf '%s' "${EXP_OUT}" | grep -oE 'BAD=[0-9]+' | cut -d= -f2)"
if [[ "${EXP_BAD}" == "0" ]]; then
  good "9 个落点都写 Neural Cloud Computers，旧展开已清干净"
else
  bad "有 ${EXP_BAD} 处缩写展开不一致：见上"
fi

say '8. 宿主清单：官网展示的 Agent 与 `ncc hur interop` 的目标必须对得上'
# 清单是「可验证的集成目标」，不能让官网自己长出一份（多了 = 吹牛，少了 = 用户不知道自己能接）。
HOST_OUT="$(python3 - "${ROOT}" "${PLAT}" <<'PY'
import os, re, sys
root, plat = sys.argv[1], sys.argv[2]
targets = None
for p in (os.path.join(root, 'cli/crates/hur-core/src/interop.rs'),):
    m = re.search(r'TARGETS:\s*\[&str;\s*\d+\]\s*=\s*\[([^\]]+)\]', open(p, encoding='utf-8').read())
    if m:
        targets = sorted(t.strip().strip('"') for t in m.group(1).split(','))
marks = os.path.join(plat, 'web/src/components/HostMarks.jsx')
keys = sorted(re.findall(r"k:\s*'([a-z]+)'", open(marks, encoding='utf-8').read()))
bad = []
if targets is None:
    bad.append('读不到 interop::TARGETS')
else:
    missing = [t for t in targets if t not in keys]
    if missing:
        bad.append('官网缺这些目标：' + ' '.join(missing))
print(f"BAD={len(bad)}")
print(f"TARGETS={' '.join(targets or [])}")
print(f"HOSTS={' '.join(keys)}")
for b in bad:
    print('  ' + b)
PY
)"
printf '%s\n' "${HOST_OUT}" | tail -n +2
HOST_BAD="$(printf '%s' "${HOST_OUT}" | grep -oE 'BAD=[0-9]+' | cut -d= -f2)"
if [[ "${HOST_BAD}" == "0" ]]; then
  good "interop 的五个目标都在官网宿主清单里"
else
  bad "官网宿主清单与 interop 目标对不上：见上"
fi

say "结果"
printf '  通过 %s · 失败 %s · 跳过 %s\n' "${PASS}" "${FAIL}" "${SKIP}"
[[ "${FAIL}" -eq 0 ]]
