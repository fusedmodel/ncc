# 更新日志（CHANGELOG）

本仓库同时维护两个可分发物，条目里用 **粗体** 标明归属：

- **`ncc`** —— 命令行客户端（`cli/`，Rust，bin: `ncc`）+ npm 包装（`packages/ncc-cli/`，`@fusedmodel/ncc-cli`）
- **`ncc-registry`** —— 内网托管节点（`ncc-registry/`，Go 单二进制）

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。
「怎么用」看 `README.zh-CN.md` 与各组件 README；**本文件只回答「这一版比上一版多了什么」**。

## [未发布]

### 新增 · **`ncc`**：`ncc p2p`（跨局域网节点直连）

- `ncc p2p probe`：**纯本地**打洞条件预检（不需要服务器）——同一本地 UDP socket 向多台 STUN
  请求，判定 NAT **映射行为**；遇到带 `OTHER-ADDRESS` 的服务器（如 `stun.miwifi.com`）就多做
  RFC 5780 的**过滤行为**分类（`CHANGE-REQUEST` `0x06`/`0x02`，位约定照抄 pion 自带工具）。
  结论：`direct | likely_direct | relay_likely | blocked`（实测本机：锥形映射 + 端口相关过滤 → `likely_direct`）。
- `ncc p2p check <节点>`：**真实建连检查** —— 经服务端信令交换双方映射地址，然后双向对打
  STUN Binding 请求（即 ICE connectivity check 的原理），约 1–3 秒、**不传业务字节**。
  预期两端同时跑；单边跑会明确告诉你对端该执行什么命令。
- `ncc p2p {ice,signal,ticket}`：看控制面下发的 ICE 配置 / 信令调试 / 票据（创建·列表·出示·撤销）。
- 新增授权种类 **`p2p`**：`ncc grant set --user @某人 --kind p2p` —— 允许直连我的**私有节点**
  （私有节点无法 `link`，这是既有设计）。语义与其它三类独立：连接 ≠ 授权。
- 实现上**不引新依赖**：STUN 只用到「Binding 请求 + XOR-MAPPED-ADDRESS + OTHER-ADDRESS +
  CHANGE-REQUEST」几样，手搋比拉 webrtc/tokio 生态划算，交叉编译不受影响。
- 实测（macOS，两个 `NCC_HOME` 的独立账号 + 两个 living 节点）：两端同时 `check` → **直连可用 ✓，
  首个往返 15–17 ms**。

### 新增 · **`ncc`** + **`ncc-registry`**：节点侧 P2P（`ncc registry p2p self|check|serve`）

云端 `ncc p2p` 回答的是「**我这台**能不能打到别人」，而内网节点那台机器的出口 NAT 常常完全是另一回事。
所以这一版把判断能力放到**节点自己身上**（`ncc-registry` 提供接口，CLI 提供命令）：

- `ncc registry p2p self`：在**目标节点那台机器**上出 NAT 画像 + 结论 + ICE 配置 + 入口状态。
- `ncc registry p2p check --peer ip:port [--wait N]`：从节点侧与一个已知映射地址**真实对打**（0 字节，不传业务）。
- `ncc registry p2p serve [--on|--off] [--peer ip:port,...]`：开/关**可被打洞入口**（一个 UDP socket，
  只应答 STUN Binding 请求，不接收业务字节），`--peer` 指定**反向打洞**对端。
- 节点侧接口：`GET /api/p2p/self`、`POST /api/p2p/check`、`GET|POST /api/p2p/serve`；随服务启动用
  `NCCR_P2P_SERVE=1`（默认**关**：它会在 UDP 上对外应答）。STUN/TURN 配置：`NCCR_P2P_STUN` / `NCCR_P2P_TURN`。
- 节点 `GET /api/meta` 增加能力 `p2p`（CLI 按能力放行，老服务端不声明也不是错误）。

**⚠️ 这一版最重要的实测结论（写进 PRD §5.4.1）：纯被动应答在「地址/端口相关过滤」的 NAT 上收不到任何包。**
本机过滤行为实测就是 `address_and_port_dependent`：映射存在≠能收包，**过滤孔必须自己先发才开**。
所以入口默认带**反向打洞**（每 300 ms 向 `--peer` 发一个 Binding 请求）—— 两端同时发，孔才互相开。
实测：两端互指对端映射后，`requestsTaken / responsesSeen` 双向持续增长；把一端换成「没有 peer 的入口」，
另一端 `responsesSeen` 立刻停止增长。**结论**：入口要真正可用，`peer` 必须由**信令**下发（每个 socket
映射不同），手写 `--peer` 只用于演示与排障。

### 新增 · Agent 接入包补齐 **P2P**（`ncc_p2p_probe` / `ncc_p2p_check` / `ncc_p2p_node`）

跨网直连的**判断面**现在也接给了 Agent（MCP 工具从 20 → **23 个**）。三个工具算的是**三台不同机器**的条件，
这正是最容易搞混的地方，所以工具描述与 SKILL.md 里都写明了：

- `ncc_p2p_probe` —— **跑 MCP 的这台机器**的打洞条件预检：**纯本地**，不需登录、不需对端
  （UDP 出站 / 公网映射 srflx / NAT 映射行为 / 过滤行为），结论 `direct | likely_direct | relay_likely | blocked`。
  结论是 `relay_likely` / `blocked` 时模型不该承诺直连。
- `ncc_p2p_node` —— **目标内网节点那台机器**的画像 + 「可被打洞入口」状态（内网出口 NAT 常与开发机完全不同）。
  只在内网 `ncc-registry` 目标上可用：打到云端控制面目标时会明确告知「云端只有控制面，切到节点目标或用 probe」。
- `ncc_p2p_check` —— **这条路径到底通不通**：真实对打（0 字节）。`addr` 直接对打（不走信令、不需登录）；
  `peer` 走控制面信令（需登录、双方要在同一分钟内各跑一次）。失败时回的是「原因 + 下一步」，不是一句超时。

**不进 MCP 的动作**（都是「改变谁能进来 / 谁能取什么」，必须用户自己拍）：开/关打洞入口
（`ncc registry p2p serve --on|--off`）、发票据/撤销（`ncc p2p ticket create|revoke`）、
P2P 授权（`ncc grant set --kind p2p`）。工具面只给**读**与**探测**。

同时把「本机预检 / 目标节点画像 / 真实打洞」这三者的区别、以及三条红线（发现 ≠ 授权 ≠ 字节通道；
STUN 可由 NCC 托管但 **TURN 必须客户自托管**；打洞失败**不降级**为中心中转业务字节）写进了
`agent/SKILL.md`（含 HTTP API 与 CLI 命令）与 `agent/harness.json`（工具清单 20 → 23）。

实现上无重复逻辑：`ncc p2p probe` / `ncc p2p check` 的**结构化结果**抽成
`p2p::probe_run` / `p2p::check_flow`（`quiet` 控制是否打印过程信息），CLI 与 MCP 共用同一份，
人读文本也只留一份 `render_profile` / `render_check`（MCP 的 stdout 只能出协议帧，一行日志都不能漏）。

### 修复 · MCP 模式下 `--base` 提示污染 stdout（会让客户端解析失败）

`ncc mcp --base <地址>` 时，「（--base 命中已有目标 …，本次已切到它）」这类**人读提示**被写到了
stdout —— 而 MCP 的 stdio 约定是 **stdout 只能出 JSON-RPC 帧**，一行提示就会让客户端报解析错误。
现在主流程先判定协议模式（`Cmd::Mcp`），所有人类提示统一走 `note()`：正常落 stdout，
协议模式下自动改走 stderr。

### 变更 · Agent 接入包（`agent/`）与 MCP 工具面刷新

- `agent/SKILL.md` / `agent/README.md` / `agent/harness.json` 之前停在「9 个工具、只有 `--base`」；
  现补齐：**目标与能力**（云端 `hub` vs 内网节点、`GET /api/meta` 的 `capabilities` 决定工具面、
  一个 MCP 进程绑定一个目标）、20 个工具的完整清单（按能力分组）、服务匹配 / 团队配置 / 分享链接 /
  节点治理的边界（治理与写入不进 MCP），以及 HTTP API 新增面（`/api/configs*`、`/api/shares*`、`/s/{token}`）。
- `harness.json`：`tools` 补到 20；`inputs` 里旧的 `target`（本意是「制品引用」）改名为 **`ref`**，
  另加 `connection`（连接目标名）与 `intent`（服务匹配意图），避免与新的「目标」概念撞名。
- **MCP 工具参数 `target` → `ref`**（`ncc_get_artifact` / `ncc_fetch_artifact` / `ncc_get_service` /
  `ncc_get_config`）：schema 里只广告 `ref`，但**旧名仍兼容**（老 prompt / 老配置不会突然失效）。
- `agent/run.sh` 注释写清三种连接写法（`--target` / `--base` / 默认目标），entry 里可直接带参。

### 新增 · **目标（target）**：云端 ncc.ai 与内网节点分得清

- 配置从「一个 base_url + 一个 token」升级为**多目标**：`{ current, targets: { hub: {…}, office: {…} } }`。
  每个目标各自一份地址与凭据（会话 token / admin key+secret）—— 在本地节点 `login`
  **不会再把你从云端挤下线**。旧版扁平配置（v1）读到即自动迁移成 `hub` 目标；
  保存时**继续镜像写回** v1 字段，旧版 CLI 仍可用。
- 新命令组 `ncc target list | use | add | rm | show`；全局 `--target <名字>`（本次命令用哪个目标）。
- `--base <URL>` 不再静默改写当前目标：按地址复用已有目标，否则**新建一个目标**并切过去（会提示）。
- `ncc hub <命令>` = 本次命令跑在云端（`hub` 前缀，实现上是主命令树的目标选择器，不重写一套子树）；
  `ncc hub` 单独出现 = 看云端目标状态。
- **能力（capability）而不是产品分类**：每个节点在 `GET /api/meta` 声明自己支持什么
  （`registry` / `config` / `share` / `nodes` / `grants` / `services` / `profile` / …），
  CLI 按这份清单放行。localhost 没声明 `services` 时 `ncc services match` 会直接告诉你
  「这个能力在云端，切过去：ncc target use hub」，而不是丢一个 404。
  **本地节点将来声明 `services` / `profile` 时，同一个命令在那台节点上直接可用。**
  老服务端不声明能力时按「未知 → 不限制」处理，不会把功能锁死。
- `ncc registry …` 只在内网节点目标上跑（在云端目标上会明确提示该切到哪个）。
- MCP 跟着目标走：`initialize` 的 instructions 带上「当前目标 + 它声明的能力」，
  工具按能力门禁（如 `ncc_match_services` 打到内网节点上会回可读的错误，不是 404）。
- 目标名自动提示：`--base http://127.0.0.1:8282` → `local`，内网 IP → 主机名前缀，
  ncc.ai → `cloud`；`ncc target add` 会先做一次可达性探测。

### 新增 · **ncc-registry** 节点治理（admin）：用户 / 节点 / 服务 + 审计

- **管理员身份两条路，等价**：本节点**第一个注册的用户**自动成为管理员（`User.IsAdmin`）；
  同时自动签发一份**机器凭据** `AK-…` + secret（secret 只在注册响应里返回一次）。
  另提供 `POST /api/admin/keys/rotate` 轮换（新 secret 生效、旧的立即失效）。
- **两套门禁**：治理面（`/api/admin/*`）只认 `requireAdmin`，不并进普通作用域体系 ——
  承认对话身份（管理员账号会话）或 `X-NCC-Admin-Key` + `X-NCC-Admin-Secret`。
- **用户**：列表（含被禁用的，带各自节点/制品数）、禁用 / 启用（`adminNote` 会随登录错误回给本人，
  **旧令牌立即失效**：`authMiddleware` 每次都回查库）、重置密码（不给则由服务端生成并只返回一次）。
  两条硬规则：不能禁用自己；不能禁用最后一个可用管理员。
- **节点**：管理面看到全部托管节点（含私有与离线，带 `ownerEmail`）；`DELETE /api/admin/nodes/:id`
  摘除任意节点并清理指向它的连接记录。
- **服务**：`GET /api/admin/services` 一次返回两类来源 —— 节点侧 `kind=service`（正在跑的）与
  制品侧 `kind=api`（声明/交付的接口）；`DELETE /api/admin/services/:ref` 中 `ND-…` → 摘除节点、
  `@ns/slug` → **归档**制品（只改 `status=archived`，字节与历史保留）。
- **审计**：新增 `audit_logs` 表 + `GET /api/admin/audit`；禁用 / 启用 / 重置密码 / 摘除 / 归档 /
  分享创建与撤销 / 凭据轮换全部记 actor（`user` 或 `admin_key`）、目标、理由、IP。
- `/api/meta` 新增 `counts.admins` / `counts.services` / `counts.serviceNodes` / `counts.shares`
  与 `auth.adminKey`；控制台新增「节点管理（管理员）」区块（填 admin key/secret 即可在网页里做上述动作）。

### 新增 · **ncc-registry** 分享链接（对方不用登录）

- 把一条制品变成**临时下载地址**：`GET /s/<token>` 说明页（不计数）、
  `GET /s/<token>/raw` 直链（**只有它计数**）、`?meta=1` 只看元数据（不计数）。
- token 32 位随机串、库里只存 sha256、只在创建时返回一次；可限次（`uses`）、可过期、可撤销，
  失效统一回 `410 share_expired`。
- **分享 ≠ 授权**：不改制品可见性，也不进 Grant 体系；**创建分享不是提权** ——
  只有本来就能读这条制品的人能分享它（否则 403）。
- 接口：`POST /api/shares` · `GET /api/shares[?mine=1\|all=1]` · `GET /api/shares/info/:token` ·
  `DELETE /api/shares/:id`（`all=1` 与撤任意分享需管理员；已实测 admin key 头在分享路由生效）。

### 新增 · **`ncc`** 命令

- `ncc registry admin login \| logout \| status \| overview \| users \| disable \| enable \| passwd \|
  nodes \| rm-node \| services \| rm-service \| audit \| keys \| rotate`；凭据优先取 `--key/--secret`，
  其次 `NCC_ADMIN_KEY`/`NCC_ADMIN_SECRET`，再取本机配置（`~/.ncc/config.json`，现按 `0600` 权限写入）；
  没配机器凭据时自动退化为「用登录会话的管理员身份」。
- `ncc registry share create \| list \| rm \| info`；`rm`/`info` 都接受完整链接（自动抠出 token）。
- `ncc register` 现在是**第一个账号时会打印一次 admin key/secret** ——
  不打印就等于让用户永久失去它。

### 新增 · **ncc-registry** 配置托管（团队的网络 / 基础设施配置）

- 配置成为**一等资源**（不再塞进制品）：内容进库、**默认 `private`**、就地修改且每次写入留一版历史、
  **不参与跨节点 fan-out**（权威数据只维护在「被指向的那个节点」上）。
- 类型 10 种（`network` / `gateway` / `infra` / `agent` / `ci` / `security` / `storage` / `observability` / `model` / `other`）；
  格式 `json|yaml|toml|env|ini|text|shell`；环境 `any|dev|staging|prod`；单条上限 128 KB。
- `secret=true` 的配置**静态加密**（AES-256-GCM，密钥由本节点 `jwt-secret` 派生，密文前缀 `enc:v1:`）——
  只备份数据库是安全的；反过来说换机器 / 丢数据目录就解不开（设计意图）。校验和按**明文**算。
- 权限三条判定：读公开 = 任何人；读非公开 = 作用域 `config:read` **且**（命名空间成员 **或** `config` 授权）；
  写 / 回滚 / 删除 = 作用域 `config:write` **且** 成员。`secret` 强制私有；非 `--reveal` 一律打码，只回 `sha256` 与大小。
- 接口：`/api/configs/kinds`、`/api/configs/bundle`、`/api/configs[/:id[/:slug]]`、`/:id/revisions`、`/:id/rollback`。
- 控制台新增「配置托管」区块与计数；`/api/meta` 增加 `configs` / `publicConfigs`。

### 新增 · **`ncc`** 命令

- `ncc registry config list | get | set | history | rollback | bundle | kinds | rm`：
  `set` 是 upsert（内容来自 `--file` / `--value` / stdin）；`get --reveal --out` 明文落盘；
  `rollback --to N` 以**新版本**写回、不改写历史；`bundle --ns @team --env prod` 成组拉取
  （`--env prod` 同时命中 `prod` 与 `any`，**默认跳过 `secret`**，要用就 `--secrets --reveal`）。
- `ncc services [list | match | show | catalog | add | rm]` —— NCC Service：服务提供方把多条业务
  打包声明，其他 Agent 按匹配策略找到并接入；`match` 是 Agent 主入口
  （`ncc services match "帮我订杭州的酒店"`）。不带子命令 = 浏览公开服务目录。
- `ncc grant --kind` 支持 `service` 与 `config`：三个 kind 按对端能力取并集
  （平台 `artifact|service|share`，registry `artifact|node|config`）；CLI 自动识别对端
  （先试 `/api/services/catalog`，再试 `/api/configs/kinds`）。
- MCP 工具增至 **20** 个：新增只读的 `ncc_list_configs` / `ncc_get_config`。
- 文档：`README.zh-CN.md` / `README.md` 补 `ncc registry config` 与 `ncc services` 命令表，
  `ncc-registry/README.md` 新增「配置托管」章节（含概念边界表、权限表与 bundle 口径）。

### 新增 · **ncc-registry** 工程本体（内网托管节点）

- 新工程 `ncc-registry/`：单二进制内网托管节点（gin + GORM + 纯 Go SQLite），默认 `:8282`，
  环境变量前缀 `NCCR_`；涵盖**制品托管 + 节点发现与互联 + 内嵌 Web 控制台**（`//go:embed`）。
- master / worker 多节点：制品可 **分发（replicate）/ 回收（revoke）**，`NCCR_REPLICATE_TARGETS`。
- 存储目录可配置：数据根 / 制品字节 / 库文件各自独立（`NCCR_DATA_DIR` / `NCCR_BLOB_DIR` / `NCCR_DB_PATH`）。
- `ncc registry …` 命令组：用**一条内网短链**接入（`ncc registry add '<短链>' --join`）、登录、节点上报
  （`--daemon` 常驻心跳）、票据签发与兑换、集群与分发管理；配套 `ncc nodes list | kinds | discover | link | label | unlink | region | recommend`。
- 授权新增 kind `node`、`config`；作用域新增 `config:read` / `config:write`
  （默认 API-Key 含两者，**接入票据不含** —— 票据必须显式签发）。

### 说明

- 以上内容尚未打 tag；`cli/Cargo.toml` 与 `packages/ncc-cli/package.json` 仍为 `0.1.3`。
- 计划：这两批内容发布时统一升到 `0.2.0`（新增功能，向后兼容），并为 `ncc-registry` 起独立版本线。
- 验证：`scripts/smoke.sh`（单 master + worker）**153 项通过 / 0 失败**；
  另有一套针对治理面与分享的接口级用例（108 项）与 CLI 实跑（含匿名 `curl` 取分享字节、
  轮换后旧凭据 401、被禁用账号旧令牌立即失效）。

## [0.1.3] — 2026-09-22

### 修复

- **`ncc`** `ncc upgrade` 的版本比较不能用字符串序 —— `0.9 → 0.10` 会被误判为「已是最新」。

## [0.1.2] — 2026-09-22

### 新增

- **`ncc`** `ncc upgrade`：自更新（`--check` 只检查、不下载）。

### 修复

- **`ncc`** npm 包装（`@fusedmodel/ncc-cli`）缓存失效，导致升级后仍跑旧二进制。

## [0.1.1] — 2026-09-22

### 新增

- **`ncc`** 个人名片（profiles）、MCP server（stdio，供任意 Agent 接入）、**节点模型**
  （连接 ≠ 授权：`ncc nodes …` + `ncc grant`）。

### 修复

- **`ncc`** Windows 下二进制必须存成 `ncc.exe` —— 此前每个 Windows 用户首次运行必失败。

### 内部

- `cli/Cargo.toml` 版本补到 `0.1.1`，与 npm 包对齐，否则发布被拦。

## [0.1.0] — 2026-09-21

首个可发布版本：**`ncc`** 单二进制客户端（注册 / 登录 / 发布 / 检索 / 下载 / 安装 / API-Key / Terminal…）、
npm 包装 `@fusedmodel/ncc-cli`、发布工作流与 `publishConfig`。

[未发布]: https://github.com/fusedmodel/ncc/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.3
[0.1.2]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.2
[0.1.1]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.1
[0.1.0]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.0
