# examples/ —— 能跑的真包，不是文档片段

这里每个子目录都是一个**完整的 HUR 包工程**：`hur.json` + `hur.lock` + 内容目录。
它们不是"示意代码"，而是**真的能过校验、能打包、能发布**的东西 —— 你照着做时踩的坑，
别人已经踩过了。

```sh
ncc hur verify examples/html-deck-to-pptx      # R1~R12，全程离线
ncc hur profile examples/html-deck-to-pptx     # 是什么 / 要什么 / 给什么 / 怎么接 + 体检
ncc hur pack examples/html-deck-to-pptx        # → dist/<id>-<version>.<profile>.hur.gz
```

## 有哪些

| 示例 | profile | 拿它看什么 |
|---|---|---|
| [`html-deck-to-pptx`](html-deck-to-pptx/) | `skill` | **带脚本的技能包**：`skills/<名字>/SKILL.md` + 同目录 `scripts/`（python + node）；怎么接进 Claude Code / Codex / Cursor；怎么签名、发布、分享 |

## 约定（`scripts/examples-smoke.sh` 会守着）

1. **一个子目录 = 一个包根**（根下有 `hur.json`）—— `ncc hur` 的各种命令都是按这个找根的。
2. **必须 `ncc hur verify` 通过**：`verify` 不过的示例比没有示例更糟 —— 读者会照抄一个错的。
3. **内容必须放在入包目录里**：`src` `skills` `kb` `data` `assets`（`spec.rs::CONTENT_DIRS`）。
   放别处不会进 `.hur`，`verify` 也看不见它。
4. **`dist/` 不入库**（每个示例自带 `.gitignore`）：产物现打现用，免得仓库里躺着
   一堆过期字节，也免得"改了源文件忘了重打包"。
5. **每个示例要有自己的 `README.md`**：说清它演示什么、怎么用、边界在哪（尤其是**诚实边界**：
   本地没跑成的事别写"已验证"）。

## 拿别人的示例当起点

```sh
cp -R examples/html-deck-to-pptx /tmp/my-skill
# 改 hur.json 的 id / name / summary（id 决定产物名与 ~/.ncc/packages/<id>/ 落点）
ncc hur build /tmp/my-skill && ncc hur verify /tmp/my-skill && ncc hur pack /tmp/my-skill
```

## 写一个新示例

```sh
ncc hur init --profile skill --kind repo --name my-thing --dir examples/my-thing
# 摆内容 → 跑通 verify → 写 README → 跑一遍 scripts/examples-smoke.sh
```

`ncc hur init --profile <名>` 会**按 profile 决定必填项**（见 `ncc hur profile --list`），
所以先想清楚它是什么，比先写文件更省事。
