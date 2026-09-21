---
name: ncc-registry
description: 使用 NCC Registry（中立、跨协议的能力制品目录）检索、取回与发布能力制品（skill / mcp / harness / plugin 等），以及按工作角色查找人才目录。当用户提到「发布能力/skill 到目录」「找一个现成的 skill / MCP」「复用别人的能力包」「谁做过 FDE/AIGC」「有没有人做过 X」时使用。
---

# NCC Registry：能力的目录与分发

NCC 是**中立、跨协议的能力制品目录** —— 可以理解成「能力的 npm」。它不绑定任何 Agent 平台，
所以你在这里发布的能力，任何 Agent、Hub 或同事都能检索并安装。

核心模型：

- **制品（artifact）**：一个可版本化的能力单元，`kind` 决定它是什么。
- **kind**：`api` / `skill` / `mcp` / `harness` / `hur` / `plugin` / `scaffold` / `docker-image` / `benchmark` / `living`。
- **引用**：`@命名空间/slug`（例如 `@aya/hotel-skill`），也可用 `R-…` 形式的 id。
- **命名空间**：个人（`@you`）或组织（`@team`）。
- **状态**：`draft` / `published` / `archived`；**可见性**：`public` / `private`（private 需付费套餐）。

## 接入方式

### 方式一：MCP（推荐）

NCC 自带 MCP server，任何 MCP 客户端都能接入：

```jsonc
// Claude Desktop / Cursor / VS Code 等 MCP 配置
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp"] } } }
```

自托管实例加 `--base`：

```jsonc
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp", "--base", "http://localhost:8181"] } } }
```

提供的工具：

| 工具 | 用途 |
|---|---|
| `ncc_list_kinds` | 目录里有哪些类型、各多少条（**检索前先看这个**） |
| `ncc_search_catalog` | 按关键词 / kind / tag / namespace 检索 |
| `ncc_get_artifact` | 单个制品的完整元数据 |
| `ncc_fetch_artifact` | **取回制品正文**（SKILL.md 等可直接读进上下文照做） |
| `ncc_publish_artifact` | 发布一个制品（需登录） |
| `ncc_whoami` | 当前账号与可用命名空间 |
| `ncc_list_roles` | 工作角色目录（6 组 20 个） |
| `ncc_find_people` | 按角色 / 技能 / 关键词找定位匹配的人 |
| `ncc_get_profile` | 看某人的名片：定位角色 + 作品集 + 已发布能力 |

### 方式二：HTTP API（无需 MCP 客户端）

全部是公开 REST，检索免登录：

```bash
GET  /api/registry/kinds                 # 类型与数量
GET  /api/registry?kind=skill&q=hotel    # 检索（支持 q/kind/tag/namespace/mine/page/size）
GET  /api/registry/{id 或 @ns/slug}      # 元数据
GET  /api/registry/{id 或 @ns/slug}/download   # 取字节地址（含 sha256）
GET  /api/profiles?role=fde&q=…          # 人才目录
GET  /api/profiles/{username}            # 名片（+ 作品集 + 已发布能力）
GET  /api/profile/roles                  # 角色目录

# 需要登录（Authorization: Bearer <JWT 或 ncc_ API-Key>）
POST /api/registry/uploads               # 上传字节（raw body + X-Filename 头）
POST /api/registry                       # 创建条目
```

错误体统一是 `{"error":{"code":"…","message":"…"}}`，HTTP 状态码同时反映语义
（400 参数错、401 未认证、402 需付费、404 不存在、409 冲突）。

## 方式三：CLI

```bash
ncc search skill --tag hotel        # 检索
ncc info     @aya/hotel-skill       # 详情
ncc download @aya/hotel-skill -o h.md
ncc install  @aya/hotel-skill       # → ~/.ncc/packages
ncc publish  --file ./x.SKILL.md --kind skill --name "X" --slug x
ncc profile                         # 自己的名片
ncc living --name my-mac --capabilities mcp,api
```

## 常用工作流

### A. 「有没有现成的？」——先查后造

1. `ncc_list_kinds` 了解目录构成；
2. `ncc_search_catalog` 用关键词 + kind 过滤；
3. 命中后用 `ncc_fetch_artifact` **把正文取回来直接用**（skill 类制品取回就是 `SKILL.md`，照着做即可）；
4. 都不合适再自己实现，完成后考虑发布（工作流 B）。

### B. 把产出发布成能力

1. `ncc_whoami` 确认身份与目标命名空间；
2. 用 `ncc_publish_artifact`，字节来源三选一：
   - `content`（内联文本，配合 `filename`）
   - `contentFile`（本地文件路径）
   - `url`（自带存储直链，不上传字节）；
3. `kind=harness` 时额外传 `manifest`（含 `harness.loader` / `harness.entry`，两者至少给一个）；
4. 先 `draft: true` 发布、确认无误后再转为 `published` 是更稳的做法。

### C. 找人 / 找定位

1. `ncc_list_roles` 拿角色 id；
2. `ncc_find_people` 按 `role` / `skill` / `query` 检索；
3. `ncc_get_profile` 看某人的作品集与他已发布的能力 —— 这比简历更能反映实际产出。

## 约定与边界

- **发布前先征求用户同意**：发布是公开可见的对外动作，除非用户明确要求，不要自动发布。
- **不要发布密钥**：制品正文会上传并存档，token / 私钥 / 内部地址不要写进去。
- **private 需要付费套餐**，免费账号只能用 `public`。
- **认领来源**：引用别人的制品时写清 `@命名空间/slug@版本`，便于回溯。
- 检索、取回、人才目录都**不需要登录**；只有发布与改名需要凭据（`ncc login` 或 API-Key）。
- 自托管：任何 NCC 兼容实例都能用 `--base` 指向（CLI）或改 MCP 配置里的 `args`。
