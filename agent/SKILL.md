---
name: ncc-registry
description: 使用 NCC 检索、取回与发布能力制品（skill / mcp / harness / plugin 等），按意图匹配对外服务，读取团队托管配置，按工作角色找人，读 Agent 的知识库 / 记忆 / 检查点与运行轨迹，以及查自己组织的 NCC Gateway 在线状态与合规审计摘要。当用户提到「发布能力/skill 到目录」「找一个现成的 skill / MCP」「复用别人的能力包」「帮我订酒店/找能办这件事的服务」「团队的网络/基础设施配置是什么」「谁做过 FDE/AIGC」「上次那件事的结论是什么 / 这个 Agent 的记忆里有什么」「这个包跑了多少次、成不成」「我们有哪些网关 / 哪台掉线了 / 这周多少请求被拒」时使用。
---

# NCC Registry：能力的目录与分发

NCC 是**中立、跨协议的能力制品目录** —— 可以理解成「能力的 npm」。它不绑定任何 Agent 平台，
所以你在这里发布的能力，任何 Agent、Hub 或同事都能检索并安装。

核心模型：

- **制品（artifact）**：一个可版本化的能力单元，`kind` 决定它是什么。
- **kind**：`api` / `skill` / `mcp` / `harness` / `hur` / `plugin` / `scaffold` / `docker-image` / `benchmark` / `living`。
- **引用**：`@命名空间/slug`（例如 `@aya/hotel-skill`），也可用 `R-…` 形式的 id。
- **命名空间**：个人（`@you`）或组织（`@team`）。组织**免费就能建**（1 个 / 5 名成员，
  创建时要选一档计划：`ncc ns plans` 看目录，`ncc ns create --plan` 指定）；计划里
  **列着但没开放的档选不了** —— 报错会说原因，不会悄悄按免费档给你建。
- **状态**：`draft` / `published` / `archived`；**可见性**：`public` / `private`（private 需付费套餐）。

## 先确认「在跟谁说话」：目标与能力

ncc 有两个世界，同一个 CLI / 同一套 MCP 工具都可能连到其中任一个：

| 世界 | 目标名 | 提供什么 |
|---|---|---|
| **云端 ncc.ai** | `hub` | 公共目录、服务市场（`ncc_match_services`）、名片与找人、分享页、P2P **控制面**（信令 / 票据 / ICE 配置），以及**网关控制面**（`ncc_list_gateways` / `ncc_gateway_audit` / `ncc_gateway_usage`） |
| **内网 registry 节点** | 自定（`local` / `office` …） | 制品、节点、**团队配置**（`ncc_list_configs`）、分享链接、**节点侧打洞画像与入口**（`ncc_p2p_node`）、**运行轨迹**（`ncc_list_traces` / `ncc_trace_stats`）、**三样状态**（`ncc_list_kb` / `ncc_get_kb` / `ncc_list_mem` / `ncc_get_mem` / `ncc_list_ckpt`） |

每个节点在 `GET /api/meta` 里**声明自己的能力**（`registry` / `services` / `profile` /
`config` / `nodes` / `grants` / `p2p` / `gateway` / `trace` / `kb` / `mem` / `ckpt` …），
工具按这份清单放行。所以：

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

提供的工具（38 个，按能力分组；不在当前目标能力清单里的会明确报错）：

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
| 索引与匹配 | `ncc_match_index` | **按一句需求在索引里找人 / 找需求**（`want=need` 反过来）；排序含内部信誉权重，**不给分数** |
| | `ncc_list_index_channels` | 索引里的频道（检索空间）列表与各多少条 |
| 名片与人 | `ncc_list_roles` | 工作角色目录（6 组 20 个） |
| | `ncc_find_people` / `ncc_get_profile` | 按角色 / 技能找人；看某人名片与作品集 |
| 节点与授权 | `ncc_list_nodes` / `ncc_discover_nodes` | 我的节点与连接表 / 发现可连接的节点 |
| | `ncc_region_profile` / `ncc_recommend_nodes` | 区域覆盖 / 按区域要推荐 |
| | `ncc_list_grants` | 我给出与收到的授权 |
| 对外授权（NCC Auth，目标声明 `auth` 才有） | `ncc_list_credentials` | 我登记过的**持有证明凭据**（CR-…）：指纹、用途、来源机器。凭据回答「是不是这份在说话」，不是权限 |
| | `ncc_list_consents` | 我授给第三方平台的应用与实际同意的 scope。**登录 / 绑凭据 / 撤销不在工具里**（会改变别人能拿到什么） |
| 跨网直连（P2P） | `ncc_p2p_probe` | **本机**打洞条件预检（纯本地：UDP 出站 / 公网映射 / NAT 映射与过滤行为） |
| | `ncc_p2p_check` | 真实打洞实测（0 字节）：`addr` 直接对打 / `peer` 走控制面信令 |
| | `ncc_p2p_node` | **目标节点那台机器**的 NAT 画像 + 可被打洞入口状态 |
| 团队配置（内网节点） | `ncc_list_configs` | 托管配置目录（公开配置无需凭据） |
| | `ncc_get_config` | 取一份配置（**内容默认打码**，`reveal=true` 才出明文） |
| 运行轨迹（内网节点） | `ncc_list_traces` | 这个包 / 这个 Agent 跑过什么（`payload=digest` = 只有哈希与结构） |
| | `ncc_trace_stats` | 轨迹的**聚合结论**：成功率 / 耗时 / token / 花费 / 按版本分组 / 标注覆盖率 |
| 三样状态（内网节点） | `ncc_list_kb` / `ncc_get_kb` | 知识库：查语料（关键词加权）/ 取正文（带 checksum，可核对） |
| | `ncc_list_mem` / `ncc_get_mem` | 记忆：按键读（Agent 读自己记忆的主路径）/ 带 TTL 与来源 |
| | `ncc_list_ckpt` | 检查点：不可变快照与血缘（**不给字节通道**，取字节要 `ncc ckpt pull`） |
| 网关控制面（云端） | `ncc_list_gateways` | 我/我们名下的网关：在线状态（**推导**）、最近心跳、用量 |
| | `ncc_gateway_audit` | 某台网关留存的**审计摘要**（计数 / 主机名 / 状态桶 / 延迟分位） |
| | `ncc_gateway_usage` | 某台网关的用量汇总（**自报计数**，口径写在返回里） |

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

# P2P（跨网直连）：云端是**控制面**，内网节点是**节点侧画像 + 打洞入口**
GET  /api/p2p/ice                                 # 控制面下发的 STUN/TURN 配置（TURN 必须客户自托管）
GET  /api/p2p/peer?ref=@ns/slug                   # 对端节点是谁、我够不够得着（够不着会给原因）
GET  /api/p2p/self                                # 【内网节点】那台机器的 NAT 画像 + 入口状态
POST /api/p2p/check {peer, waitSec}               # 【内网节点】从节点侧与一个映射地址对打（0 字节）
GET  /api/p2p/serve · POST /api/p2p/serve {on, peer}   # 【内网节点】看/开「可被打洞入口」（只应答 STUN）
GET  /api/p2p/tickets?mine=1                      # 我发出的 P2P 票据（连接 ≠ 授权）

# 需要登录（Authorization: Bearer <JWT 或 ncc_ API-Key>）
POST /api/registry/uploads               # 上传字节（raw body + X-Filename 头）
POST /api/registry                       # 创建条目
POST /api/shares                         # 建分享链接（ref / uses / expiresInDays）

# 内网 registry 节点专用：运行轨迹 / 三样状态（读）
GET  /api/traces?ref=&kind=&status=&tag=&since=&size=   # 轨迹列表（默认看不到别人的）
GET  /api/traces/stats?ref=&kind=&since=                # 聚合结论
GET  /api/traces/{id}                                   # 一条轨迹
GET  /api/kb?namespace=&kind=&tag=&q=&size=             # 知识库目录（关键词加权）
GET  /api/kb/{@ns/slug 或 KD-…}?revision=N              # 知识库正文（带 checksum）
GET  /api/mem?namespace=&subject=&prefix=&kind=         # 记忆列表（过期默认不算）
GET  /api/mem/{key}?subject=&namespace=                 # 一条记忆
GET  /api/ckpt?ref=&label=&q=&size=                     # 检查点列表

# 云端 ncc.ai 专用：网关控制面（只读；需要凭据 + gateways:read）
GET  /api/gateways/summary                              # 我名下的网关 + 各自用量 + 合计
GET  /api/gateways?namespace=@team                      # 按命名空间过滤
GET  /api/gateways/{GW-…}                               # 单台（含推导出的在线状态）
GET  /api/gateways/{GW-…}/audit?since=&limit=&format=csv   # 留存摘要（csv = 合规导出）
GET  /api/gateways/{GW-…}/usage?since=                  # 用量汇总（自报计数，口径在 basis 字段）
GET  /api/meta                                          # 云端也声明 `gateway` 能力
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

# P2P（跨网直连的判断面；这些是**人**执行的动作，Agent 只用读工具 + check）
ncc p2p probe                       # 本机条件预检（纯本地）
ncc p2p check @team/nas             # 两端同时跑：真实建连检查
ncc p2p check --addr 1.2.3.4:5678   # 不走信令，直接对打（对端 mapped）
ncc registry p2p self               # 内网节点那台机器的画像
ncc registry p2p serve --on --peer <对端 mapped>   # 在节点上开可被打洞入口（只应答 STUN）
ncc p2p ticket create --peer @team/nas --ref @team/db-backup --expires-in 300

# 轨迹（节点）：采集在本地，上传才出机器
ncc trace add --file ./run.jsonl        # 采集（ncc-trace/v1 / HUR 运行轨迹 / 事件流）
ncc trace push --dataset eval           # 上传到节点，成为可评估的数据集
ncc trace ls --ref @aya/agent --limit 20

# 三样状态（节点）：`pull` 会按包的 state{} 声明增量拉取
ncc kb set @team/handbook --title … --file ./handbook.md
ncc kb pull --package ./my-agent        # 读包的声明，按 checksum 增量拉到 ~/.ncc/kb
ncc mem get planner.last_plan           # Agent 读记忆的主路径
ncc ckpt save --name step-3 --file ./state.bin --parent-last

# 网关控制面（云端）：注册/心跳/上报都是**网关那台机器上**的人的动作
ncc gateway init --accept llm           # 本机网关配置（白名单代理，S2a）
ncc gateway bind --namespace @team      # 注册到控制面（令牌只回一次，写进 gateway.json 0600）
ncc gateway run                         # 常驻：周期心跳 + 摘要上报（先落盘再发送）
ncc gateway audit --remote --csv        # 看/导出控制面留存的摘要
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
2. `ncc_find_people` 按 `role` / `skill` / `query` 检索；加 `following: true` 则**只看我关注的人**
   （需要已登录 —— 它问的是「我」关注了谁；结果里也只有公开名片）；
3. `ncc_get_profile` 看某人的作品集与他已发布的能力 —— 这比简历更能反映实际产出。

### D. 「这件事该找谁办」——服务匹配（云端）

1. 用户说一句人话（「帮我订杭州的酒店」「门店要巡检」）；
2. `ncc_match_services` 传 `intent`（可加 `region` / `category`）——服务端打分排序，返回
   `reasons`（为什么召回）与 `howToUse.steps`（怎么接过去）；
3. 需要授权时它会告诉你 `requiresGrant` 与申请办法 —— **不要自己去要授权**，让用户处理；
4. 拿到端点/规范后按步骤调用（`ncc_get_service` 看完整条款与 SLA）。

### D2. 「谁在提供这个 / 谁有这个需求」——索引匹配

服务目录是**声明**（提供什么、怎么接）；**索引**是**登记**（谁能被搜到）。两者互补：
服务解决「怎么接过去」，索引解决「一句话能不能搜到人」。

1. 用户说一句需求（「国庆要两间杭州的大床房」）→ `ncc_match_index` 传 `intent`，
   可加 `channel`（检索空间，如 `booking`）与 `region`；结果是**人 + 他登记的那个东西**
   （`owner` / `title` / `channel` / `howToUse`）；
2. 用户是**供给方**、想找活儿 → `want: "need"`，看谁在提需求；
3. `ncc_list_index_channels` 先看有哪些检索空间 —— **不知道频道名时先用它**；
4. **登记（写操作）不在 MCP 工具里**：让用户跑 `ncc index publish <频道> …`
   （要写明频道与来源，属于对外动作，由用户自己拍）；
5. **不要向用户报分数**：结果里没有 score，只有一个**面向需求的可读理由**；
   `reasons` 里也不会出现信誉权重。
6. **索引 ≠ 授权**：能搜到只代表找得到。私有制品要 `grant`、非公开服务要 `service` 授权 ——
   拿不到细节时把 `requiresGrant` 如实告诉用户，**不要自己去要授权**。

### E. 团队配置：先看有什么，再取该取的那份

1. `ncc_list_configs`（可加 `namespace` / `kind` / `env`）——公开配置无需凭据；
2. `ncc_get_config` 只取元数据（默认打码）；用户确实要明文时才 `reveal:true`；
3. `secret:true` 的配置在服务端是**静态加密**的，取明文前先确认必要性；
4. **写配置（set / rollback / bundle 落盘）不在 MCP 工具里** —— 让用户跑
   `ncc registry config set …`（写操作改的是团队的真实基础设施，要用户自己拍）。

### F. 「这两个节点能不能直连 / 跨网传数据」——先判条件，别承诺

三个工具对应三种不同的机器，别混：

| 问的是谁的条件 | 用哪个工具 | 判什么 |
|---|---|---|
| **跑 MCP 的这台机器** | `ncc_p2p_probe` | 纯本地预检（不需要登录/对端）：UDP 出站、公网映射(srflx)、NAT 映射行为、过滤行为 → `direct` / `likely_direct` / `relay_likely` / `blocked` |
| **目标内网节点那台机器** | `ncc_p2p_node` | 那台机器的画像 + 它的**可被打洞入口**开没开（内网出口 NAT 常与开发机完全不同） |
| **这条路径到底通不通** | `ncc_p2p_check` | 真实对打（0 字节）：`ok:true` + RTT = 通了；`ok:false` 会给出原因与下一步 |

怎么用：

1. 先用 `ncc_p2p_probe` 判「**永远打不通**」的情况（UDP 被封 / 对称 NAT / CGNAT）——这一步不需要登录，
   也不用等对端，能立刻把「别指望直连」说清楚；
2. 要跨到某个内网节点，`ncc_p2p_node` 拿它的 `mapped`（对端应发往的地址）与入口状态；
3. 再用 `ncc_p2p_check` 实测：`addr` 直接对打（不需要登录），或 `peer` 走控制面信令（双方都要登录且
   **在同一分钟内各跑一次**）。

⚠️ 三个必须说给用户听的事实：

- **打洞必须双方同时发起**。本机 NAT 过滤多为「地址/端口相关」，对端即使开着入口，单纯被动应答
  **一个包也收不到**（实测：入口 `已应答 0 次`）。所以 `peer` 这个「反向打洞对端」必须由**信令**给，
  不是配置项。
- **失败不要降级**：打洞失败 + 没有客户自托管 TURN 时，**明确报「不可达」**。
  红线：发现 ≠ 授权 ≠ 字节通道；STUN 可由 NCC 托管，**TURN 必须客户自托管**；
  **NCC 永不中转业务字节**（中心搬运（master/worker `replicate`）是另一条路，不是这里的兜底）。
- **入口开关不是 Agent 能拍的事**：`ncc registry p2p serve --on` 会在 UDP 上对外开放一个入口，
  必须由用户在节点上显式执行（工具里只有**只读**的状态查看）。

### G. 「这个包 / 这个 Agent 到底行不行」——运行轨迹（内网节点）

1. `ncc_trace_stats`（可加 `ref` 只看某个制品版本）拿**聚合结论**：成功/失败/取消各多少、
   耗时 p50/p90、token 与花费、按制品版本分组、标注覆盖率与结论分布；
2. 要下钻就 `ncc_list_traces` 过滤（`ref` / `kind` / `status` / `tag` / `since`）；
3. ⚠️ 两条别搞错：
   - `payload=digest` 表示这条轨迹**只有哈希与结构**（没有提示词与输出原文）——那是默认档，
     适合评估与统计，**不能当训练材料**；要原文得由采集方重新采。
   - `score` 只对**打过分的**轨迹求平均：没标注就别下结论，如实说「没有标注，无法评价」；
4. 采集与上传（`ncc trace add` / `ncc trace push`）**不在工具里**：把轨迹推给哪个节点是用户的事。

### H. 三样状态：知识库 / 记忆 / 检查点（内网节点）

Agent 自己的**状态**住节点上（不是制品：制品是能力，状态是数据）。三样都**只有读工具**：

| 你想干什么 | 工具 | 注意 |
|---|---|---|
| 找语料 | `ncc_list_kb`（`namespace` / `kind` / `tag` / `query`） | 检索是**关键词加权**（标题 3 / 摘要 2 / 正文 1），换个说法不一定命中 |
| 读正文 | `ncc_get_kb`（`ref` = `@ns/slug` 或 `KD-…`，可 `revision`） | 正文带 `checksum`，要核对就自己算一遍 |
| 读记忆 | `ncc_get_mem`（`key` + 可选 `subject`，默认 `self`） | **Agent 读自己记忆的主路径**；带 TTL 的过期即视为不存在 |
| 列记忆 | `ncc_list_mem`（`subject` / `prefix` / `kind` / `source`） | 记忆**默认私有**，没有公开档 |
| 看检查点 | `ncc_list_ckpt`（`ref` / `label` / `query`） | 不可变（没有"改"这个动作）+ 血缘（`parent`） |

几条语义（回答用户时要说清）：

- **包只声明，字节住节点**：`hur.json` 的 `state{}`（规则 R11）声明这个 Agent 要哪些知识库/记忆/
  检查点，`ncc kb pull --package <目录>` 按声明**增量**拉下来（按 `checksum` 比对）；
- **写不进工具**：`ncc kb set` / `ncc mem set` / `ncc ckpt save` 都要用户自己跑 —— 让一次工具调用
  悄悄改写 Agent 的记忆或知识，出问题没人能复盘是谁改的；
- 检查点**不给字节通道**：工具只回元数据与摘要，取字节走 `ncc ckpt pull`（客户端会核对摘要）。

### I. 「我们有哪些出口、被用了多少、被拒了多少」——网关与合规审计（云端）

客户自装一台 `ncc gateway`（固定路由的白名单代理）之后，它把**逐请求审计留在那台机器上**，
只把**窗口摘要**上报控制面。Agent 能读的就是控制面这一侧：

1. `ncc_list_gateways` —— 有哪些网关、命名空间、**在线状态**、版本、最近心跳、摘要窗口数、用量；
   回答"我们有几台 / 哪台掉线了 / 最近多少请求被拒"；
2. `ncc_gateway_audit`（`gateway` + 可选 `since` / `limit`）—— 某台网关留存的**窗口摘要**：
   请求/放行/拒绝、出站入站字节、延迟 p50/p95/max、**主机名聚合**、状态码桶、拒绝理由、路由与方向；
3. `ncc_gateway_usage`（`gateway`）—— 用量汇总（窗口数、活跃天数、首末窗口）。

三条必须说给用户听的边界（**别把摘要说成"完整审计"**）：

- **控制面只有摘要**：路径、请求载荷、凭据一律不在 —— 路径里常带订单号这类业务标识，
  那是**故意不上报**的（NFR-1：NCC 不碰业务数据）。所以**不要**用这些工具回答
  "谁调了哪个 URL"；那要去**那台网关本机**跑 `ncc gateway audit`（本地 JSONL 才有路径）；
- **在线状态是控制面按心跳超时推导的**（默认 90s），不是网关自报：`status` 是推导结果，
  `statusReported` 才是网关自己说的（`online` / `draining`）；
- **用量是网关自报的计数**：签名（`HMAC-SHA256(key = sha256(网关令牌))`）只证明「是持有那枚令牌的
  进程报的、内容没被改过」，**不证明内容为真** —— 被入侵的网关可以少报。要"可核对"得先有网关侧的
  证明（今天没有）；
- 注册网关、改名、**吊销**、让网关开始上报（`ncc gateway bind|unbind|report`）都是**那台机器上的人**的
  动作，不在工具里；吊销之后网关心跳/上报会立刻 401，但**本地审计一条不丢**（进待传队列）。

## 约定与边界

- **发布前先征求用户同意**：发布是公开可见的对外动作，除非用户明确要求，不要自动发布。
- **不要发布密钥**：制品正文会上传并存档，token / 私钥 / 内部地址不要写进去。
- **分享 ≠ 授权**：`ncc registry share` 给的是**临时下载链接**（可限次/限时/撤销，拿到字节即结束）；
  要给人长期权限得用 `ncc grant`。分享只能由「本来就能读那条制品」的人创建。
- **节点治理（admin）不进 MCP**：禁用账号、重置密码、摘除节点、归档服务条目只走 CLI，
  而且需要管理员身份 —— 这是**对人的动作**，必须由用户自己执行。
- **加签不进 MCP**：`ncc sign` 要动**用户设备的私钥**（`~/.harnessuse/keys`），
  而工具面是只读的 —— 给制品签名必须由用户自己执行：`ncc sign <文件> --attach @you/slug`。
  你可以替用户读签名：条目 JSON 的顶层 `signature`（`keynum` / `sha256` / `signer`）说明
  「谁签了哪份字节」；但**不要**把 `signature` 存在说成"可信"——
  公钥是否认识、自带的 `pubkey` 只是发布方声明（自证），确认得跑 `ncc verify --pubkey`。
- **P2P 的写动作也不进 MCP**：开/关可被打洞入口（`ncc registry p2p serve --on|--off`）、
  发/撤票据（`ncc p2p ticket create|revoke`）、授权（`ncc grant set --kind p2p`）都属于
  「改变谁能进来 / 谁能取什么」的动作，必须由用户显式执行；工具里只有读（画像、入口状态）与探测（打洞实测）。
- **摘要 ≠ 完整审计**：控制面的网关审计**只有摘要**（计数 / 主机名 / 状态桶 / 延迟分位），
  路径与载荷留在网关本机。用户问"谁调了哪个 URL / 传了什么"时，如实说"控制面看不到，
  要去那台网关本机 `ncc gateway audit`"，不要用摘要硬答。
- **自报计数不等于事实**：网关的在线状态是控制面**推导**的，用量是网关**自报**的，
  签名只证明来源与完整。给合规结论时要带上这句限定。
- **private 需要付费套餐**，免费账号只能用 `public`。
- **profile 与数据快照不在工具面里**：`ncc hur profile`（读一份包是什么 / 要什么 / 给什么 / 怎么接）、
  `ncc hur match`、以及四种数据快照包（`kb bundle --as-package` / `mem export` / `ckpt export` /
  `trace export` + `ncc hur data import`）都是 **CLI 动作** —— 导出要挑数据范围、导入会改节点上的数据，
  这两件事必须由用户自己拍板。你能做的是：
  - 从条目清单里读 `profile`（`manifest.profile`，以及 `profile_declared` 区分"作者写的"与"按 kind 推的"），
    从而知道"这份包能不能跑、接哪个宿主、是不是一份数据快照"；
  - 读 `manifest.data`（`source` / `snapshot_at` / `privacy` / `license` / `payload` / `docs_count`）
    判断"这份数据能不能用在我这里"；
  - 要完整四问与逐份明细，让用户跑 `ncc hur profile <包或引用>`；要取用数据快照，
    让用户跑 `ncc hur data import --package <目录> --apply`（**默认只出计划**，这一步会改数据）。
  - ⚠️ 别把"有 profile/有签名"说成"可信/安全"：profile 只说明**它是什么**，两者都不是安全背书。
- **认领来源**：引用别人的制品时写清 `@命名空间/slug@版本`，便于回溯。
- 检索、取回、人才目录、服务匹配、公开配置都**不需要登录**；发布、看自己的配置、分享需要凭据
  （`ncc login` 或 API-Key）。
- 目标与能力：先 `ncc target list` 或看 `GET /api/meta` 的 `capabilities`；命令/工具不匹配时
  按错误提示切目标（`ncc --target <名字>` / `ncc hub …`），别反复重试同一个目标。
