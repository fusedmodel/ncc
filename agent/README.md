# NCC for Agents

把 **NCC** 接给任意 Agent，让它能 agentic 地检索、取回与发布能力制品，按意图找对外服务，
读团队托管配置，按角色找人，以及判断跨局域网直连（P2P）条件。

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

自托管 / 内网节点：用 `--target`（目标名，见 `ncc target list`）或 `--base`（地址）：

```jsonc
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp", "--target", "office"] } } }
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp", "--base", "http://localhost:8282"] } } }
```

### 一个进程绑定一个目标；能力决定工具面

ncc 有**两个世界**：云端 `hub`（ncc.ai：目录 / 服务市场 / 名片 / P2P 控制面）与内网 registry 节点
（制品 / 节点 / 团队配置 / 分享 / 节点侧打洞画像）。每个节点在 `GET /api/meta` 里**声明自己的能力**
（`registry` / `services` / `profile` / `config` / `nodes` / `grants` / `p2p` …），工具按这份清单放行——
所以**清单会随目标变化**，以 `tools/list` 为准。要同时用两边，就配两个 MCP server。

接上之后，Agent 最多会看到 23 个工具：

```
# 目录与制品（两边都支持）
ncc_list_kinds       目录里有哪些类型、各多少条（检索前先看）
ncc_search_catalog   按关键词 / kind / tag / namespace 检索
ncc_get_artifact     单个制品的完整元数据（参数 ref）
ncc_fetch_artifact   取回制品正文（SKILL.md 可直接读进上下文照做）
ncc_publish_artifact 发布制品（需登录）
ncc_whoami           当前账号与可用命名空间

# 服务（云端声明）
ncc_match_services     按意图匹配对外服务（分数 / 理由 / 接入步骤）
ncc_list_services      浏览服务目录
ncc_get_service        单条服务的完整接入信息（参数 ref）
ncc_service_categories 业务分类目录

# 名片与人（云端声明）
ncc_list_roles       工作角色目录
ncc_find_people      按角色 / 技能找定位匹配的人
ncc_get_profile      某人的名片：定位角色 + 作品集 + 已发布能力

# 节点与授权
ncc_list_nodes       我的节点与连接表
ncc_discover_nodes   发现可连接的节点
ncc_region_profile   区域覆盖
ncc_recommend_nodes  按区域要推荐
ncc_list_grants      我给出与收到的授权

# 跨网直连（P2P，判断面：只做探测，不搬运业务字节）
ncc_p2p_probe        本机打洞条件预检（纯本地，不需登录/对端）
ncc_p2p_check        真实打洞实测（0 字节）：addr 直接对打 / peer 走信令
ncc_p2p_node         目标内网节点那台机器的画像 + 可被打洞入口状态

# 团队配置（内网节点声明）
ncc_list_configs     托管配置目录（公开配置无需凭据）
ncc_get_config       取一份配置（默认打码，reveal=true 才出明文；参数 ref）
```

> 三个 P2P 工具算的是**不同机器**的条件，别混：`ncc_p2p_probe` = 跑 MCP 的**本机**；
> `ncc_p2p_node` = **目标内网节点那台机器**；`ncc_p2p_check` = **这条路径到底通不通**（实测）。
> 红线：发现 ≠ 授权 ≠ 字节通道；STUN 可由 NCC 托管，**TURN 必须客户自托管**；
> 打洞失败就明确报错，**不降级成 NCC 中转业务字节**。开入口（`ncc registry p2p serve --on`）、
> 发票据、授权都是**人**的动作，不在 MCP 工具里。

> 引用类参数叫 **`ref`**（旧名 `target` 仍兼容）——“target”现在指的是**连接目标**。

**认证**：复用 CLI 的登录态（`~/.ncc/config.json`，按目标存）。先 `ncc login`，或用 API-Key。
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
[`run.sh`](run.sh) 是入口（`exec ncc mcp "$@"`，因此 entry 里可以继续带
`--target office` / `--base …`）。任何实现了 `mcp/stdio` loader 的 runtime 都能加载本包：

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

## 边界（写进契约的事）

- **一个进程一个目标**：MCP 工具只作用于启动时选定的目标；能力不匹配会回可读错误，
  不会静默失败。
- **只读优先**：23 个工具全是只读、探测或「发布自己的制品」。会改变「别人能拿到什么」的动作
  （声明服务、连接节点、授权、分享、配置写入、**节点治理**、**开关 P2P 打洞入口 / 发票据**）
  都不在工具面 —— 由用户用 CLI 执行。
- **引用参数叫 `ref`**（旧名 `target` 仍兼容），别与「连接目标」混。
