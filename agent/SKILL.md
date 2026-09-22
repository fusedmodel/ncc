---
name: ncc-registry
description: 使用 NCC 检索、取回与发布能力制品（skill / mcp / harness / plugin 等），按意图匹配对外服务，读取团队托管配置，以及按工作角色找人。当用户提到「发布能力/skill 到目录」「找一个现成的 skill / MCP」「复用别人的能力包」「帮我订酒店/找能办这件事的服务」「团队的网络/基础设施配置是什么」「谁做过 FDE/AIGC」时使用。
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

## 先确认「在跟谁说话」：目标与能力

ncc 有两个世界，同一个 CLI / 同一套 MCP 工具都可能连到其中任一个：

| 世界 | 目标名 | 提供什么 |
|---|---|---|
| **云端 ncc.ai** | `hub` | 公共目录、服务市场（`ncc_match_services`）、名片与找人、分享页 |
| **内网 registry 节点** | 自定（`local` / `office` …） | 制品、节点、**团队配置**（`ncc_list_configs`）、分享链接 |

每个节点在 `GET /api/meta` 里**声明自己的能力**（`registry` / `services` / `profile` /
`config` / `nodes` / `grants` …），工具按这份清单放行。所以：

- **工具清单是变化的**，以 `tools/list` 为准；调不通时先看错误信息里的「这个目标没有声明 X 能力」
  与它给的切换建议，不要反复重试。
- 一个 MCP 服务进程**绑定一个目标**（启动时决定）。要同时用云端与内网节点，配两个 MCP server：
  `{"args":["mcp"]}` 与 `{"args":["mcp","--target","office"]}`。
- 人类用 `ncc target list / use` 查看与切换；Agent 不要自己改用户的默认目标。

> 引用参数叫 **`ref`**（旧名 `target` 仍兼容）——现在「target」指的是**连接目标**，两者别混。

## 接入方式

### 方式一：MCP（推荐）

NCC 自带 MCP server，任何 MCP 客户端都能接入：

```jsonc
// Claude Desktop / Cursor / VS Code 等 MCP 配置
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp"] } } }
```

自托管实例加 `--target`（目标名，见 `ncc target list`）或 `--base`（地址）：

```jsonc
// 内网 ncc-registry 节点（已存为目标 office）
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp", "--target", "office"] } } }
// 或直接给地址（会按地址复用/新建目标）
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp", "--base", "http://localhost:8282"] } } }
```

提供的工具（20 个，按能力分组；不在当前目标能力清单里的会明确报错）：

| 能力 | 工具 | 用途 |
|---|---|---|
| 目录与制品 | `ncc_list_kinds` | 目录里有哪些类型、各多少条（**检索前先看这个**） |
| | `ncc_search_catalog` | 按关键词 / kind / tag / namespace 检索 |
| | `ncc_get_artifact` | 单个制品的完整元数据（`ref`） |
| | `ncc_fetch_artifact` | **取回制品正文**（SKILL.md 等可直接读进上下文照做） |
| | `ncc_publish_artifact` | 发布一个制品（需登录） |
| 账号 | `ncc_whoami` | 当前账号与可用命名空间 |
| 服务（云端） | `ncc_match_services` | **按意图匹配对外服务**，返回打分理由与接入步骤 |
| | `ncc_list_services` / `ncc_get_service` / `ncc_service_categories` | 浏览 / 看细节 / 取分类目录 |
| 名片与人 | `ncc_list_roles` | 工作角色目录（6 组 20 个） |
| | `ncc_find_people` / `ncc_get_profile` | 按角色 / 技能找人；看某人名片与作品集 |
| 节点与授权 | `ncc_list_nodes` / `ncc_discover_nodes` | 我的节点与连接表 / 发现可连接的节点 |
| | `ncc_region_profile` / `ncc_recommend_nodes` | 区域覆盖 / 按区域要推荐 |
| | `ncc_list_grants` | 我给出与收到的授权 |
| 团队配置（内网节点） | `ncc_list_configs` | 托管配置目录（公开配置无需凭据） |
| | `ncc_get_config` | 取一份配置（**内容默认打码**，`reveal=true` 才出明文） |

### 方式二：HTTP API（无需 MCP 客户端）

全部是公开 REST，检索免登录：

```bash
GET  /api/meta                            # 节点自述：product / kind / capabilities（先看这个）
GET  /api/registry/kinds                 # 类型与数量
GET  /api/registry?kind=skill&q=hotel    # 检索（支持 q/kind/tag/namespace/mine/page/size）
GET  /api/registry/{id 或 @ns/slug}      # 元数据
GET  /api/registry/{id 或 @ns/slug}/download   # 取字节地址（含 sha256）
GET  /api/profiles?role=fde&q=…          # 人才目录
GET  /api/profiles/{username}            # 名片（+ 作品集 + 已发布能力）
GET  /api/profile/roles                  # 角色目录

# 云端（ncc.ai）专用：服务市场
GET  /api/services/catalog                 # 业务分类 / 接入方式 / 授权方式目录
GET  /api/services/match?intent=订酒店      # 按意图匹配（含 reasons / howToUse.steps）
GET  /api/services/{@提供方/标识}            # 单条服务接入信息

# 内网 registry 节点专用：配置托管 / 分享链接
GET  /api/configs?namespace=&kind=&env=&mine=1     # 配置目录（公开配置匿名可读）
GET  /api/configs/{@ns/slug}?reveal=1             # 取一份（默认打码，只回 sha256/大小）
GET  /api/configs/bundle?namespace=&env=prod      # 成组拉取（env 命中 prod 与 any）
GET  /api/shares?mine=1                           # 我发的分享链接
GET  /s/{token}/raw?meta=1                        # 分享链接：先看元数据（不计数）
GET  /s/{token}/raw                               # 分享链接：取字节（**计数**，不用登录）

# 需要登录（Authorization: Bearer <JWT 或 ncc_ API-Key>）
POST /api/registry/uploads               # 上传字节（raw body + X-Filename 头）
POST /api/registry                       # 创建条目
POST /api/shares                         # 建分享链接（ref / uses / expiresInDays）
```

错误体统一是 `{"error":{"code":"…","message":"…"}}`，HTTP 状态码同时反映语义
（400 参数错、401 未认证、402 需付费、403 无权限、404 不存在、409 冲突、410 分享失效）。

## 方式三：CLI

```bash
# 先看现在连着谁（云端 ncc.ai / 内网节点各是一个目标）+ 各自声明了什么能力
ncc target list

ncc search skill --tag hotel        # 检索
ncc info     @aya/hotel-skill       # 详情
ncc download @aya/hotel-skill -o h.md
ncc install  @aya/hotel-skill       # → ~/.ncc/packages
ncc publish  --file ./x.SKILL.md --kind skill --name "X" --slug x
ncc profile                         # 自己的名片
ncc living --name my-mac --capabilities mcp,api

# 云端命令：ncc hub <命令>（本次跑云端，不改默认目标）
ncc hub services match "帮我订杭州的酒店"

# 内网节点：团队配置 / 分享 / 治理（都在 ncc registry 下）
ncc registry config list --mine
ncc registry config get @team/network --reveal --out ./network.yaml
ncc registry share create @team/report --label "给合作方" --uses 1 --expires 7
ncc registry admin overview         # 管理员才有的节点治理（用户/节点/服务 + 审计）
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

### D. 「这件事该找谁办」——服务匹配（云端）

1. 用户说一句人话（「帮我订杭州的酒店」「门店要巡检」）；
2. `ncc_match_services` 传 `intent`（可加 `region` / `category`）——服务端打分排序，返回
   `reasons`（为什么召回）与 `howToUse.steps`（怎么接过去）；
3. 需要授权时它会告诉你 `requiresGrant` 与申请办法 —— **不要自己去要授权**，让用户处理；
4. 拿到端点/规范后按步骤调用（`ncc_get_service` 看完整条款与 SLA）。

### E. 团队配置：先看有什么，再取该取的那份

1. `ncc_list_configs`（可加 `namespace` / `kind` / `env`）——公开配置无需凭据；
2. `ncc_get_config` 只取元数据（默认打码）；用户确实要明文时才 `reveal:true`；
3. `secret:true` 的配置在服务端是**静态加密**的，取明文前先确认必要性；
4. **写配置（set / rollback / bundle 落盘）不在 MCP 工具里** —— 让用户跑
   `ncc registry config set …`（写操作改的是团队的真实基础设施，要用户自己拍）。

## 约定与边界

- **发布前先征求用户同意**：发布是公开可见的对外动作，除非用户明确要求，不要自动发布。
- **不要发布密钥**：制品正文会上传并存档，token / 私钥 / 内部地址不要写进去。
- **分享 ≠ 授权**：`ncc registry share` 给的是**临时下载链接**（可限次/限时/撤销，拿到字节即结束）；
  要给人长期权限得用 `ncc grant`。分享只能由「本来就能读那条制品」的人创建。
- **节点治理（admin）不进 MCP**：禁用账号、重置密码、摘除节点、归档服务条目只走 CLI，
  而且需要管理员身份 —— 这是**对人的动作**，必须由用户自己执行。
- **private 需要付费套餐**，免费账号只能用 `public`。
- **认领来源**：引用别人的制品时写清 `@命名空间/slug@版本`，便于回溯。
- 检索、取回、人才目录、服务匹配、公开配置都**不需要登录**；发布、看自己的配置、分享需要凭据
  （`ncc login` 或 API-Key）。
- 目标与能力：先 `ncc target list` 或看 `GET /api/meta` 的 `capabilities`；命令/工具不匹配时
  按错误提示切目标（`ncc --target <名字>` / `ncc hub …`），别反复重试同一个目标。
