---
name: html-deck-to-pptx
description: '把 HTML deck（1920×1080 固定画布 + .slide 分页的演示稿）转成【原生可编辑】的 PPTX：卡片、色块、发丝线、图片都是 PowerPoint 形状与图片，每段文字都是真文本框（保留字号/加粗/颜色/行高），并写入演讲者备注。 触发词：导出 PPTX、转成 PPT、做成可编辑的 PPT、HTML 变 PPT、给客户/同事的 PPT、convert deck to pptx、export editable pptx。 不适用：只要 PDF（用 html-deck-to-pdf）、只要截图不可编辑的 PPT（用 html-to-pptx 的截图方式）。'
argument-hint: '<deck.html 路径>'
---

# html-deck-to-pptx — HTML 演示稿 → 原生可编辑 PPTX

把 `.slide` 分页的 HTML deck **重建**成 PowerPoint 原生形状：能改字、能换图、能拖框。
不是截图贴图 —— 截图式转换（每页一张图）在客户/同事要改稿时不可用。

> **本技能随包分发（HUR 包 `html-deck-to-pptx`，profile=skill）**：下面命令里的 `scripts/`
> 指**本技能目录**下的 `scripts/` —— `ncc hur install html-deck-to-pptx` 之后的落点是
> `~/.ncc/packages/html-deck-to-pptx/skills/html-deck-to-pptx/scripts/`；
> 装进宿主时（`.claude/skills/html-deck-to-pptx/` 等）就在同一个目录里。先 `cd` 进去再执行，
> 或把路径换成上面对应的绝对路径。

## 何时用

- 用户说「导出 PPTX / 转成 PPT / 做成可编辑的 PPT / 发给同事（他们只接受 PPT）」
- 需要一份**对方能在 PowerPoint 里直接改**的稿子（融资 BP、客户方案、参赛材料）
- 只要 PDF 请改用另一支技能 `html-deck-to-pdf`（不在本包里）

## 原理（三句话）

1. 用本机 Chrome（CDP，零依赖 Node 脚本）把 deck 渲染出来，**逐个元素量几何、读文字样式**，写成 `_layout.json`
2. 用 python-pptx 把这份几何**原样重建**为 PowerPoint 形状（1 设计 px = 0.5 pt = 6350 EMU，1920×1080 → 13.333×7.5 in）
3. 用校验脚本做「文字完整性 / 换行风险 / 字体 / XML 合规」四件套自检（本机没 Office，这是替代人工目检的手段）

## 操作步骤

### 1. 抽取版面（改了 HTML 就必须重跑这一步）

```bash
node scripts/extract-deck.mjs <deck.html>
# → 写出 <deck 同目录>/<deck 同名>._layout.json，并打印「抽到 N 页 · 盒子 X · 文本 Y · 图片 Z」
# 自检查：Z 应该等于 deck 里的 <img> 数量（为 0 说明图片没被识别，别急着往下走）
```

可选：`--selector .page`（默认 `.slide`）、`--design 2560x1440`、`--no-reset`（不注入「强制显形」样式）、
`--note-selectors`（喂给演讲者备注的页脚元素，默认 `.note, .footline, [data-note]`；
带 `.ft` 页脚的 deck 用 `--note-selectors ".ft, .note, .footline, [data-note]"`）。

### 2. 构建 PPTX

```bash
python3 scripts/deck2pptx.py <deck>._layout.json \
        --title "《材料全名》"          # 会写进每页备注的第一行
# → 默认输出 <同一目录>/<deck 同名>.pptx；--check 只自检不写文件
```

常用参数：`--out 路径.pptx`、`--img-base 图片基准目录`（默认取 deck 同目录）、
`--nowrap-classes a,b`（这些 class 的文本不自动换行）、`--scale-pt 0.95`（整体缩字号救溢出）。

### 3. 校验（**必须做，且要看到全 0**）

```bash
python3 scripts/verify-pptx.py <deck>._layout.json <deck.pptx>
```

合格线（硬指标）：`文字可能缺失` 无 · `字体 latin == ea 且 > 0` · `rPr 顺序异常 0` · `备注 n/n 页`。
软指标：`换行风险` 里**会叠字的那部分**必须为 0；标「下方无元素 → 仅框外排字」的可以放过
（PPT 不裁字，往框外排不影响版面）。

| 现象 | 处理 |
|---|---|
| 文字可能缺失 | 抽取被 `display:none` 吞了（加 `--no-reset` 试试）或 layout 是旧的 → 重跑第 1 步 |
| 换行风险 | ① 会叠字 → 该 class 加进 `--nowrap-classes`（HTML 里本来就不换行的），或 `--scale-pt 0.97`（实测能消掉大部分）；② 仍不行回 HTML 收字/加高；③ 「框外排字」的可以不管 |
| 形状越界（构建时打印） | 元素的 `top+height` 超出画布 → 回 HTML 修版面 |
| `unsupported image format` | 已内建处理：`webp/heic` 走 `sips` 转 PNG、`svg` 用 Chrome 按展示框比例栅格化（结果缓存在 `$TMPDIR/deck2pptx-cache`） |
| 备注为空 | 该 deck 的页脚类名不是默认那几个 → 加 `--note-selectors` 重抽（如 `.ft`） |

### 4. 交付时如实说明

- **可编辑范围**：卡片/线条/色块/图片是形状与图片，文字是真文本框（run 级保留加粗与配色）
- **字体**：latin 指定 `Arial`、中文指定 `PingFang SC`；对方机器（尤其 Windows）没有 PingFang 时会回退，局部会有一两行位移 → 需要绝对保真就统一换成对方有的字库
- **本机没有 PowerPoint / LibreOffice / Keynote** → 只做了结构与度量校验，没渲染目检；PPT 里有 HTML 的动画/交互一律没有
- 每页演讲者备注 = 页眉行（`《材料名》第 i / n 页`）+ 该页页脚 / `.note` 口径

## 已知局限（诚实写在交付说明里）

- 只画 `<img>`；用 `background-image` 承载的照片抽不出字节（要先把照片改成 `<img>`）
- 卡片被 `overflow:hidden` 裁掉的那部分图片：抽取时算成「可见区 + 裁切比例」，PPT 里用 `crop` 还原
  （不这么做的话，图片会按原尺寸画出来、越出画布）
- 渐变只按渐变色带的第一段纯色（本工作区是点睛橙 `#E8590C`）近似
- 飞入/滚动入场动画、`#/N` 路由、localStorage 记忆页 —— PPT 里都没有
- 页面若依赖 JS 运行时生成 DOM，请确保加载完成后 DOM 已稳定（脚本会等 load + fonts + images 就绪，
  并自带「页里路由改 hash 导致执行上下文被销毁」的重试）

## 依赖

Node 22+（内置 `fetch`/`WebSocket`，无需 npm 安装）· 本机 Google Chrome（`CHROME_BIN` 可覆盖）·
`python3` + `python-pptx` + `lxml`（`Pillow` 可选，只用于换行评估，缺了会跳过该项）
