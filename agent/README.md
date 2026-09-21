# NCC for Agents

把 **NCC Registry** 接给任意 Agent，让它能 agentic 地检索、取回与发布能力制品。

本目录是可直接分发的 Agent 接入包，三种形态对应三类 Agent：

| 文件 | 制品形态 | 适合的 Agent |
|---|---|---|
| `..` → `ncc mcp` | **MCP server** | Claude Desktop / Cursor / VS Code / 任何 MCP 客户端 |
| [`SKILL.md`](SKILL.md) | **`kind=skill`** | 支持 SKILL.md 的 Agent（Claude Skills、harness-use 等） |
| [`harness.json`](harness.json) + [`run.sh`](run.sh) | **`kind=harness`** | 实现了 `mcp/stdio` loader 的 runtime |

三者描述的是**同一个能力**，只是接入协议不同 —— 按你的 Agent 支持哪种挑一种即可。

## 1. MCP（推荐）

```jsonc
// Claude Desktop: ~/Library/Application Support/Claude/claude_desktop_config.json
// Cursor: .cursor/mcp.json      VS Code: .vscode/mcp.json
{
  "mcpServers": {
    "ncc": { "command": "ncc", "args": ["mcp"] }
  }
}
```

自托管实例：

```jsonc
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp", "--base", "http://localhost:8181"] } } }
```

接上之后，Agent 会看到 9 个工具：

```
ncc_list_kinds       目录里有哪些类型、各多少条（检索前先看）
ncc_search_catalog   按关键词 / kind / tag / namespace 检索
ncc_get_artifact     单个制品的完整元数据
ncc_fetch_artifact   取回制品正文（SKILL.md 可直接读进上下文照做）
ncc_publish_artifact 发布制品（需登录）
ncc_whoami           当前账号与可用命名空间
ncc_list_roles       工作角色目录
ncc_find_people      按角色 / 技能找定位匹配的人
ncc_get_profile      某人的名片：定位角色 + 作品集 + 已发布能力
```

**认证**：复用 CLI 的登录态（`~/.ncc/config.json`）。先 `ncc login`，或用 API-Key。
检索与取回是公开只读的，**不登录也能用**。

手动验证协议是否正常：

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | ncc mcp
```

> `ncc mcp` 的 stdout **只输出协议消息**，日志全部走 stderr —— 这是 MCP stdio 的硬要求。

## 2. SKILL.md

把 [`SKILL.md`](SKILL.md) 放进 Agent 的技能目录即可（Claude Skills 约定：带 `name` / `description`
的 YAML frontmatter + Markdown 正文）。它同时写清了 HTTP API，所以**没有 MCP 的 Agent 也能用**。

## 3. Harness

[`harness.json`](harness.json) 是 NCC 的封装契约（`harness.loader` / `harness.entry`），
[`run.sh`](run.sh) 是入口（`exec ncc mcp`）。任何实现了 `mcp/stdio` loader 的 runtime 都能加载本包：

```bash
ncc publish --kind harness --name "NCC for Agents" --slug ncc-for-agents \
            --file ./run.sh --manifest ./harness.json
```

## 发布到 NCC 目录

本包本身可以发布进 NCC，让别的 Agent 也能检索到（先用 `ncc profile` 确认你的命名空间）：

```bash
# Skill 形态
ncc publish --kind skill --name "NCC Registry 使用技能" --slug ncc-registry \
            --file ./SKILL.md --tags ncc,registry,agent

# Harness 形态
ncc publish --kind harness --name "NCC for Agents" --slug ncc-for-agents \
            --file ./run.sh --manifest ./harness.json --tags ncc,mcp
```

或者干脆让 Agent 自己发 —— 这就是本包要达成的效果：装好之后，Agent 可以自己把成果
发布成制品、自己检索别人做过的能力。
