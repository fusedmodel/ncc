# html-deck-to-pptx · HUR 技能包

> `harness-use-package/v1` · kind=`repo` · **profile=`skill`** · id=`html-deck-to-pptx` · v0.1.0

把 `.slide` 分页的 HTML deck 转成**原生可编辑 PPTX**：卡片 / 色块 / 图片是 PowerPoint 形状与图片，
每段文字是真文本框（保留字号 / 加粗 / 颜色），并写入演讲者备注。
技能正文：[`skills/html-deck-to-pptx/SKILL.md`](skills/html-deck-to-pptx/SKILL.md)
（三支脚本与它同目录，`skills/html-deck-to-pptx/scripts/`）。

## 这个包是什么

- **一份技能 + 它的脚本**，不是程序：`profile=skill` 只要求 `skills/` 下至少一份技能文件，
  并**禁止 `entry`**（技能不是可执行入口）。`ncc hur run --exec` 不会跑它 ——
  python / node 由宿主 Agent 按 SKILL.md 自己跑，ncc 只负责封装、认证、分发。
- 入包的只有 `src` `skills` `kb` `data` `assets` 这五个目录（`spec.rs::CONTENT_DIRS`）
  —— 所以脚本必须待在 `skills/` 下面，换个地方就不进包。
- 确定性 + 可签名：产物是 `<id>-<version>.<profile>.hur.gz` —— **`.hur` 说“这是 HUR 规范产物”，
  末尾的 `.gz` 说“外面这层是 gzip 容器”**（`file` / 双击 / `gunzip` 都认得出）。同样内容必得同样
  sha256，签名覆盖的是产物字节。

## 装 / 用

```sh
ncc hur install ./dist/html-deck-to-pptx-0.1.0.skill.hur.gz   # 本地产物
ncc hur install @<ns>/html-deck-to-pptx                        # 已发布的条目
# 落点：~/.ncc/packages/html-deck-to-pptx/skills/html-deck-to-pptx/
```

接进宿主（Claude Code / Codex / Cursor 读的都是“一个技能目录里的 SKILL.md”）：

```sh
cp -R ~/.ncc/packages/html-deck-to-pptx/skills/* .claude/skills/
# → .claude/skills/html-deck-to-pptx/{SKILL.md,scripts/}
```

> ⚠️ 别指望 `ncc hur interop --write` 一步到位：① 它给 `skills/<名字>/SKILL.md` 这种**嵌套**
> 布局取宿主技能名时只看**路径最后一段**，`SKILL.md` 会被写成 `.claude/skills/skill/SKILL.md`；
> ② 它只复制 `skills/**` 里的 `.md/.txt`，**不搬 `scripts/`**。整目录 `cp -R` 最省事。

宿主机器上要有：Node 22+ · 本机 Google Chrome（`CHROME_BIN` 可覆盖）·
`python3` + `python-pptx` + `lxml`（`Pillow` 可选，只用于换行评估）。

## 改 / 发 / 分享

```sh
ncc hur build .                  # 刷新 hur.lock（内容改过就要重跑，签名覆盖它）
ncc hur verify .                 # R1~R12，全程离线
ncc hur profile .                # 是什么 / 要什么 / 给什么 / 怎么接 + 分级体检
ncc hur pack .                   # dist/html-deck-to-pptx-0.1.0.skill.hur.gz + .sha256
ncc hur key gen && ncc hur sign . # Minisign（第三方 `minisign -V` 可独立核对）
ncc hur publish . --namespace <ns>  # 发到当前目标；`ncc hub …` = 云端，`ncc registry …` = 内网节点
```

- `--namespace` 传**原始 slug**（节点上叫 `seed`，不是 `@seed`）。
- 分享三条路：`ncc hur export .`（离线发 `.hur.gz` + `.minisig` + `.pub` + `.sha256` + `export.json`）·
  发布后把 `@ns/slug` 给人（对方 `ncc hur install`）· 内网节点上
  `ncc registry share create @ns/html-deck-to-pptx --uses 3 --expires 7`（对方不用登录）。
- **ncc Share（`/{user}/share/:id`）只能分享自包含 HTML，不适用于技能包。**
- 重打包 = 换字节 ⇒ 旧签名一律失效，`ncc hur sign .` 重签一次（这是设计如此，不是报错）。

## 改它就改这里

**本目录就是这个包的本体**（`examples/` 下的每个子目录都是一个完整 HUR 包工程）：
技能正文在 `skills/html-deck-to-pptx/SKILL.md`，三支脚本与它同目录 —— 正文里的 `scripts/…`
说的就是“技能目录里的 scripts/”。改完走一圈：

```sh
ncc hur build .      # 内容改过先刷新 hur.lock（签名覆盖它）
ncc hur verify .     # R1~R12，全程离线
ncc hur pack .       # dist/html-deck-to-pptx-0.1.0.skill.hur.gz + .sha256
```

**拿去当自己的包用**：整目录 `cp -R` 走，改 `hur.json` 里的 `id` / `name` / `summary`
（`id` 决定产物名，也决定 `~/.ncc/packages/<id>/` 落点），再 `ncc hur build .` 一次。

> 这份技能的**最早原稿**是一份“SKILL.md 直接放根目录”的散装技能（不在本仓里）。
> 这里按宿主约定收进了 `skills/<技能名>/`，并做成可校验、可签名、可发布的 HUR 包 ——
> 所以 `skills/` 这一层不是多余的：宿主（Claude Code / Codex / Cursor）要找的就是
> “一个目录，里面一支 `SKILL.md`”。
