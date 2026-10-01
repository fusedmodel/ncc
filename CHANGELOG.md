# 更新日志（CHANGELOG）

本仓库维护一个可分发物：**`ncc`** —— 命令行客户端（`cli/`，Rust，bin: `ncc`）
+ npm 包装（`packages/ncc-cli/`，`@fusedmodel/ncc-cli`）。

> **`ncc-registry` 已独立**（2026-09-24）：内网托管节点搬到了
> [`fusedmodel/ncc-registry`](https://github.com/fusedmodel/ncc-registry)，
> 并**从那边的 `v0.1.0` 起走自己的版本线** —— 它的变更历史看那个仓库的 CHANGELOG。
> 这里以 git submodule（`ncc-registry/`）引入；**下面 `[未发布]` 及更早版本里**
> **属于 `ncc-registry` 的条目保留原样**，只作历史记录，不再更新。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。
「怎么用」看 `README.zh-CN.md` 与各组件 README；**本文件只回答「这一版比上一版多了什么」**。

## [未发布]

### 新增 · `ncc conn`：到 Cloud instance 的连接通道（通信基础设施）

`ncc sandbox run` 是「把一条命令送过去跑」；`ncc conn` 是「**在那台机器上开一条会话**」——
推文件、反复跑命令、把产物拉回来，全程留在账本里。用户要的「一系列命令 + 文件推送 + 目标端执行」
就是 `ncc conn run`。

- **`ncc conn open [--on <已登记的云电脑>\|--url <URL> --key <k>] [--name] [--note] [--ttl <秒>]`**：
  建通道；凭据与地址写进 `~/.ncc/connections.json`（**0600**，`ls` 不回显 key）。`--on` 直接复用
  `ncc sandbox init` 登记过的地址与凭据（不重复录一次）。
- **`ncc conn ls`**：状态**现问目标端**（不可达就明说，不猜）。**`status <id\|名>`**：细节 +
  **这条通道上跑过什么**（关掉后仍可看：账本不该随关闭一起消失）。
- **`exec <id> "命令" [--cwd] [--timeout] --reason`**：可反复跑、工作目录跨命令保持；
  **退出码跟随远端**（CI 可直接用）；日志尾巴直接打出来。
- **`push <id> <文件> [--to <相对路径>] [--exec] --reason`** / **`pull <id> <相对路径> [--to <落点>]`**：
  推/拉文件（锁在通道的工作目录里；拉回来会对 `sha256`）。
- **`run <id> --file L[=R] --file … (--script <脚本>\|--sh "命令") --reason`**：一批做完
  —— 推文件 → 推脚本（落 `.ncc-run.sh`，0700）→ 目标端执行。
- **`close <id> [--purge] [--forget]`**：关闭。`--purge` 删远端工作目录；**默认保留本地登记**
  （关了还能 `status` 看账本），要清掉用 `--forget`。
- ⚠️ 踩过的坑（已钉住）：**位置参数字段不能叫 `target`** —— 顶层 `--target` 是 global 的，clap arg id
  撞车，于是 `ncc conn run cli-chan …` 会报「没有名为 cli-chan 的目标」（与 `ncc info` / `ncc mem rm`
  / `ncc verify` 同一个坑），一律叫 `conn`。
- 冒烟：`ncc-registry/scripts/conn-smoke.sh`（57 项，含 CLI 串联与「远端退出码非 0 → CLI 也非 0」）。

### 新增 · `ncc auth`：凭据 + 设备码登录 + 授权管理（对第三方平台的授权颁发方）

服务端那侧是 `ncc-platform` 的 NCC Auth（OIDC 授权服务器 + RFC 8628 设备码，设计见
`prd/ncc-auth.md`）；客户端这一侧只有三件事，且每件都对应一条边界。

- **`ncc auth key new [--purpose identity] [--label …]`**：在本机生成一份身份持有密钥
  （Ed25519，落 `~/.ncc/cred/<purpose>.key`，`0600`）并把**公钥**登记到服务端，拿回 `CR-…`。
  ⚠️ **与发布者签名密钥故意分开**：发布签名在 `~/.harnessuse/keys`，混用会导致
  「换密钥 = 已发布制品验签全废」。私钥永不出本机；服务端永远只见到公钥。
  **登记凭据 ≠ 有权取东西** —— 能拿什么仍然看 scope 与 `ncc grant`。
- **`ncc auth key ls` / `rm <CR-…>`**：列出 / 撤销服务端登记的凭据。
- **`ncc auth login --client <id> [--secret …] [--scope …] [--credential CR-…]`**：
  走设备码（与 `gh auth login` 同一条路）—— 终端打印 `verification_uri` 与 `user_code`，
  用户在浏览器确认，CLI 按 `interval` 轮询换令牌；`authorization_pending` / `slow_down`
  按规范继续等。令牌**按目标单独存**（不覆盖会话）：它受众是某一个平台、只能走数据面，
  账号面（key、授权）仍然用 `ncc login` 的会话。
- **`ncc auth consents [--revoked]` / `revoke <id>` / `status`**：我授出去的应用（含
  scope 与绑定凭据）、**按平台撤销**（撤一个不影响另一个，且立刻失效）。
- 新增依赖 `ring`（Ed25519；本来就在依赖树里，是显式用它而不是多引一套密码学库）。
- **`--auth` 全局开关**：以对外令牌跑这条命令（数据面）。令牌若绑了凭据，CLI 自动附上
  持有证明 `NCC-Proof`（规范串 `METHOD\nPATH\nb64url(sha256(token))`，与服务端逐字节一致）；
  账号面仍然会 403 `auth_token_path`（这是设计）。
- **MCP 只读两件**：`ncc_list_credentials` / `ncc_list_consents`（走 `auth` 能力位）；
  登录 / 绑凭据 / 撤销都是「改变别人能拿到什么」的动作，仍然只在 CLI。
- 验收：`ncc-platform/scripts/auth-smoke.sh`（协议与红线 70/70）与
  `auth-cli-smoke.sh`（客户端四条链，含 cnf 与 --auth）。

### 新增 · 索引与匹配：`ncc index` / `ncc list` / `ncc match`

把「我有什么 / 我要什么」登记进一个**自由频道**，别人用一句需求就能检索到（服务端那侧是
`ncc-platform` 的 `IndexEntry`，见那边的 CHANGELOG 与 `prd/ncc-index-match.md`）。

- **`ncc index publish <频道> [--service @我/slug | --item @我/slug | --need "一句话"]`**：
  先写平台（权威、跨网可查），再**尽力推已接入的内网节点**（`--node` 指定、`--no-push` 跳过），
  并逐个报告结果：`✓ 节点 local 已接收` / `✗ 节点 local：<原因>`。
  **节点推失败不回滚平台，但绝不假装成功**；成功的节点回报平台，`ncc index show` 里看得到「已推节点」。
- **`ncc index list [--mine|--channel|--side|--kind|--q]` · `show <引用>` · `rm <引用>` · `push <引用>`**：
  `show` 会打印「怎么接过去」（端点或引用原件）、已推节点；`rm` 只撤回索引，**不动原件**。
- **`ncc list users` · `ncc list needs` · `ncc list channels`**：索引里有什么 ——
  按人聚合的「已索引的人」、别人放出来的需求、以及有哪些检索空间。
- **`ncc match "…" [--channel|--region|--kind|--want supply|need|--limit|--from <目标>]`**：
  匹配源默认是当前目标，也可以用 `--from <目标名>` 指向内网节点（或由环境变量
  `NCC_INDEX_SOURCE` 指定 —— 这就是「系统设置」那一档）。`--want need` 是**找活儿**。
  输出带命中理由与下一步；**排序含内部信誉权重，但评分不对外显示**，CLI 也不会打印任何分数。
- **MCP 只读两件**：`ncc_match_index`（一句需求找人）与 `ncc_list_index_channels`。
  登记 / 撤回是改「别人能搜到我什么」，仍然只在 CLI。
- 验收：`ncc-platform/scripts/index-match-smoke.sh` **81/81**（含第 9 节 15 条 CLI 断言）。

### 变更 · 评分不再对外显示：`ncc profile rate/ratings` 不再打印别人的均分

服务端改了口径：**星级只作为匹配的内部权重**，不进任何公开响应（见 `ncc-platform` 的 CHANGELOG）。

- `ncc profile rate` 只回显「我给了几星」，不再打印对方的均分/人数（服务端已经不返回）。
- `ncc profile ratings <别人>` 只列**评语**；只有看自己收到的评价时才打印均分与分布。
  打分的人仍能看到自己打的那一分（要能改它）。

### 新增 · 组织计划：`ncc ns plans` / `ncc ns create --plan`

组织在服务端改成**免费也能建（1 个 / 5 名成员）**、创建时**选一档计划**（`ncc-platform`
那侧的事），CLI 这一版把这件事接到命令面：

- **`ncc ns plans`** 列组织计划目录：可选档标 `✓`、未开放档标 `·` 并带出**原因**；
  登录时连「已拥有 n / m 个组织」一起显示（目录本身公开，那两个数只有你自己有）。
  目录**原样来自服务端**（`GET /api/namespaces/plans`）—— CLI 不另抄一份计划表，
  否则两边必然慢慢长歪。
- **`ncc ns create --slug <标识> --name <名称> [--plan free]`** 创建组织时带上计划
  （缺省 `free`）—— 创建是**这件事唯一发生的地方**，建完再补计划等于没选。
- **`ncc ns list`** 的组织行多一列**计划**（个人空间打 `-`：它固定 free，打出来只是噪声）。
- 边界跟服务端一致，CLI 不自己判：认不出的计划 id → `400 unknown_plan`（**列出可选值**）；
  目录里列着但没开放的档 → `400 plan_not_available`（**附原因**）—— **绝不静默降级成
  free**：让人以为拿到了 Pro 额度是最糟的失败方式。超额度是 `402 org_quota_exceeded`。
- 验收：`ncc-platform/scripts/org-plan-smoke.sh` **61/61**（其中第 9 节 8 条是 CLI 面：
  列目录 / 标未开放 / 显示已用额度 / 建组织回显计划 / 建 pro 档被拒且回显服务端原因）。

## [0.3.0] — 2026-09-29

### 新增 · 名片社交的 **CLI 面与 Agent 面**：关注 / 评价

服务端与 Web 早就上线了（在 `ncc-platform` 那侧），这次补的是**客户端两面** ——
在此之前 `ncc profile` 只有名片与作品集，MCP 也完全看不到关注关系。

- **CLI**：`ncc profile follow / unfollow / followers / following` 与
  `rate / unrate / ratings`。`follow --note` 是**只给关注方自己看**的备注；
  `rate --score 1~5` 一人一条，**重复提交是覆盖，不是加一票** —— 刷分从结构上就不可能。
- `ncc profile show` 现在带出**粉丝数 / 关注数 / 评分**，并按 `viewer.canFollow / canRate`
  提示下一步能做什么。
- ⚠️ **两个 `note` 的语义故意不同**，别当成一回事：关注的备注**私密**、空串 = **不改动**；
  评价的评语**公开**、空串 = **清空**。所以 `rate` 不带 `--note` 会清掉已有评语，
  CLI 在这种情况下会明确警告（`follow` 则不会）。
- `--score` 越界**在本地就拦**（退出码 1，消息里说清 `1~5`），不发请求去等服务端 400。
- **Agent**：MCP `ncc_find_people` 加 `following` 参数 —— 只看我关注的人。
  该参数需要登录，未登录会明确回 `[unauthorized] following=1 需要登录`，而不是给一个空列表
  让人以为「我关注的人都不在目录里」。
- 写动作**仍然只在 CLI**：关注 / 打分要动服务端上的身份，不做成 Agent 的自主动作。
- `ncc key scopes` 补上 `social:write`（`Risky`，且**不被任何作用域蕴含** ——
  要让它关注 / 打分必须显式给，`profile:write` 不算）。

验收：平台冒烟 `scripts/profile-social-smoke.sh` 第 11 节的 10 条 CLI 断言全绿。
（该脚本没找到 CLI 时会**静默跳过**这一节，所以别只看总数：不带 CLI 是 63 通过，
带上 CLI 才是 72 —— 差的 9 条就是这一节。）

### 新增 · `examples/`：能跑的真包示例

[`examples/html-deck-to-pptx/`](examples/html-deck-to-pptx/) —— 一个 `profile=skill` 的完整 HUR
工程（`skills/<名字>/SKILL.md` + 同目录 python / node 脚本），连同「怎么接进 Claude Code / Codex /
Cursor、怎么签名发布分享」一起写在它自己的 README 里。它不是文档片段：真的过 `ncc hur verify`、
真的 `pack` 得出产物、真的发布得出去。

新增 [`scripts/examples-smoke.sh`](scripts/examples-smoke.sh)（**全程离线**，pack 在副本上跑，不往仓库写
`dist/`）：把 `examples/*/` 逐个走一遍 `verify` → `pack`，并断言产物名带容器后缀、`id` 与 `profile`
段跟**清单**一致、外层是 gzip（`1f 8b`）、侧车跟着新名字。理由很简单：**示例会烂** —— 今天合规、
明天改了 R 规则或产物命名谁都不知道，读者照抄一个错的比没有示例更糟。

### 变更 · 产物文件名末尾加**容器后缀**：`….hur.gz`

口径（用户 2026-09-29）：**hur 本身只是一种规范，文件本身用通用压缩包格式结尾** ——
于是 `file`、双击、`gunzip`、编辑器、IM 预览都认得出它，不用先知道 hur 是什么。

```text
dist/html-deck-to-pptx-0.1.0.skill.hur.gz
                            └──┘ └┬─┘
                        这是 HUR 产物 └ 外面这层是 gzip 容器
```

- 后缀由**容器**决定（`spec::ARTIFACT_CONTAINER_EXT`）：以后换容器（比如不再压缩）只改一个常量。
- 侧车跟着往后加：`x.hur.gz.minisig` / `x.hur.gz.sha256`。
- **老名字全认**：`….profile.hur`（上一代）与 `….hur`（上上代）照旧能找、能校验、能装、能签
  —— 候选名 `spec::artifact_candidates` 三条，`spec::is_archive_path` 同时认 `.hur` 与 `.hur.gz`。
- 「这是产物还是工程目录」现在**只有一处实现**（`spec::is_archive_path`）：`ncc hur verify` /
  `ncc hur sign` / `ncc sign`（转交）/ MCP 装包 / `interop` 同步判定全走它 ——
  以前各地自己写 `extension() == "hur"`，加一个后缀就要改一圈。

### 变更 · `.hur` 现在是**压过的**（容器改为 gzip 包住 zip）

在此之前 `.hur` 里的条目一律以 `Stored` 写入 —— **不压缩**。包一多、数据包一大，
分享与上传就白白多传几倍字节（实测一个技能包：61958 → 24244 字节）。

新容器：

```text
.hur = gzip( zip( hur.json · hur.lock · 内容文件… ) )
         ↑ 压缩在这里          ↑ 结构 / 防穿越解包在这里
```

- 外层 gzip：`file x.hur.gz` 报 gzip，`gunzip -c x.hur.gz > x.zip` 出来的**仍是标准 zip**，
  任何 zip 工具都能看包内清单（不牺牲"能一眼看进去"这点）。
- 内层 zip 仍是 `Stored`：不二次压缩（压两遍只会更慢更大），只保留条目名 / 路径顺序 /
  防穿越解包。
- **确定性照旧**：条目排序 + 时间戳固定 + gzip 头部写死（`mtime=0` / `OS=255` / 不带文件名）
  × 级别固定 ⇒ 同样内容仍得同样 sha256。⚠️ 一点新代价：确定性现在也依赖 **flate2 的版本**
  （同一份 `Cargo.lock` 内稳定）；包里 `hur.lock` 的逐文件摘要是内容级真值，不受影响。
- **老包继续能装**：`unpack` 按魔数辨认（`1f 8b` = gzip，`PK` = 裸 zip）。格式升级不该让
  已经发出去的字节变成废纸。
- 签名语义不变，但**换容器 = 换字节**：格式升级前签过的**本地工程目录**重打包后要
  `ncc hur sign` 重签一次（已发布/已下载的 `.hur` 不受影响 —— 核的仍是那份字节）。

### 修复 · `.hur` 产物的签名核对：`ncc hur verify x.hur.gz --require-signature` 一律失败

拿 `.hur` 产物核签名时，R9 会跑去**解包出来的临时目录**里找 `.minisig`（那里永远不会有），
于是"有签名、公钥也认识、签名本身有效"的包照样报「找不到签名文件」并以退出码 1 结束 ——
`ncc hur export` 印给收件人的那两行命令**照抄就是错的**。

现在按**手里这份字节**核（`.minisig` 就躺在产物旁边），收件人视角实测通过：不认识公钥时
如实说「有签名，但公钥不在本机受信列表」，认了之后 ✔。

## [0.2.1] — 2026-09-28

### 修复 · `ncc download` / `ncc install` **完全不可用**

```
$ ncc download @you/hotel-skill -o out.md
✗ 没有名为 @you/hotel-skill 的目标
```

条目引用被当成了**服务目标名**，于是这两个命令**没有任何办法**能跑通 ——
换条目 ID、加 `--target`、加 `--base` 全一样。

根因是 clap 的 arg id 撞车：顶层 `--target` 是 `global = true`，位置参数与它
**arg id 相同**时，clap 会把位置参数的值塞进全局项。`ncc info` 早就因此把字段名
改成了 `reference`（源码里还留着注释），`download` / `install` 当时漏了。
这次一并改成 `reference`，并补上同样的警示注释 —— 同一个坑在 `info` /
`verify` / `mem rm` / `download` / `install` 上已经出现过五次。

### 修复 · `ncc hur run . --exec` 被策略拒绝时不说理由

原来只在**不带** `--exec` 时才渲染执行计划，带 `--exec` 时只留下一句
「当前策略不允许执行该包（理由见上）」—— 而上面空无一物。
拒绝本身是对的（比如 `hur init` 脚手架包自己声明了 `exec.enabled=false`，
只走声明式装配），但用户看不到**为什么**被拒。现在被拒时不论有没有 `--exec`
都先把计划与拒绝理由打出来。

## [0.2.0] — 2026-09-28

### 变更 · `ncc store list --q` 从子串改为**关键词匹配 + 加权排序**

- **几个词都要出现**（空格 / 中英文逗号 / 顿号分开），命中位置决定排序：
  key 3 / 标签与 `?search` 字段 2 / 正文 1 —— 与 kb 的关键词加权同一口径。
- **明说的取舍**：排序在最多 500 条候选上做（超出按更新时间截断），不是索引检索、
  更不是向量检索 —— `ncc store kinds` 与接口文案里都这么写。
- **`ncc store ls` 成为「这台节点上有什么内容」**：列出实际有的集合（内置的带标记），
  并列出节点支持的几类内置内容与哪些还没用过（取用即声明）。
- **`ncc store export` 跳过内置集合**（它们的声明在节点代码里，不是配置），并在
  stdout 给 JSON 时把人看的说明改到 stderr（之前粘在 JSON 后面，调用方一解析就炸）。
- 验证：`bash scripts/store-smoke.sh`（133 通过 / 0 失败）。

### 新增 · 声明即配置：`ncc store export` / `declare --file|--dir|--check`

集合声明是**配置**，不是一次性敲出来的命令行参数 —— 它得能进 git、能被评审、
能先看差异再决定改不改。

- **`ncc store export --dir stores/`**：一件一个 `<集合>.json` + 一份说清用法的 README；
  也可以 `--file` 打成一份或直接打到 stdout。
- **`ncc store declare --dir stores/`**：逐件 apply（幂等：一样的再来一次就是「不变」）。
- **`--check`**：只看会改什么，**真的不改**；有差异退出码 1 —— 可以直接当 CI 门禁。
  差异说得具体：只说哪几项变了（`fields` / `index` / `visibility` …）。
- **一个形状两种用法**：`CollectionSpec` 是节点与包共用的同一个形状 ——
  节点声明「提供什么」，包的 `state.stores[]` 声明「需要什么」（多一个 `mode`）。
  所以 `--file` 也直接吃 **`hur.json`**：「把这个包需要的集合落地」
  就是 `ncc store declare --file hur.json`，不用先手工抄一遍。
- **`declare` 是完整的一份说法，不是补丁**：文件里没写的项就是没有。
- **`mode` 只属于包**：从 `hur.json` apply 时会提醒一句，
  免得有人以为文件里写了 `readwrite` 就等于这台节点授权了写。
- **离线先拦能拦的**：`index` 里的字段没在 `fields` 声明过 → 本地就拒（与包的 R12 同一条口径）。
- 节点侧 `Collection` 多一个 `reason`（为什么提供这一类内容），导出时跟声明一起走。
- 验证：`bash scripts/store-smoke.sh`（123 通过 / 0 失败）。

### 新增 · 模型面 = 声明面：`ncc mcp --package <包>`

一个 HUR 包（插件 / harness）需要的是**恰好它能用的那部分**：多了是风险（模型看得到
不该看的），少了是幻觉（模型以为自己能调）。于是 `ncc mcp` 支持把面收窄到包的声明：

- **`ncc mcp --package ./my-pkg`**：工具表 = 这个包 `state.stores[]` 声明过的集合。
  `read`（默认）→ `ncc_store_list_<集合>` / `ncc_store_get_<集合>`；`readwrite` → 多一个
  `ncc_store_put_<集合>`；`write`（只写）→ **只给写工具**（写进去的不回头读）；
  **没声明 → 连名字都看不到**。
- **看不到的也真调不动**：`tools/list` 不广告只是第一步 —— 边界判在**执行处**，
  不然一个跑偏的客户端（或手工构造的请求）照样能发 `tools/call`。
- **声明成了模型的 schema**：字段类型、枚举词表、必填全从**节点解析好的声明**来
  （不在客户端重写一套字段语法）；**过滤只广告 `index` 里声明过的字段** ——
  广告一个服务端会 400 的条件，等于让模型去撞墙。
- **`fields` / `filter` 是嵌套对象**：集合完全可以声明一个叫 `body` / `archived` / `q`
  的字段，平铺就分不清是哪个了。
- **没给 `--package`**：只给浏览用的只读工具（与 kb / mem 同一档：写状态由持凭据的人
  显式跑 CLI 决定）。记录仓多一条 —— **包可以用声明打开写口子**，因为每次写都自带审计
  （谁、何时、改成哪个摘要、备注）。
- **声明了但节点上没有**的集合：不广告、不放行，并说清是「没声明」还是「声明了没落地」。
- **R12**：任何写模式（`readwrite` / `write`）不给 `reason` 都会提醒 ——
  声明了写 = 「模型能改这些内容」，该说清为什么要给它这个口子。
- **`nur profile`** 把结果算给作者看：
  `模型面：ncc mcp --package . → 读 issue · **写** issue/audit`。
- 验证：`bash scripts/store-smoke.sh`（99 通过 / 0 失败）。

### 新增 · `ncc store`：通用记录仓（**声明一个集合 = 新增一类内容**）

在做完知识库 / 记忆 / 检查点 / 轨迹之后，下一个需求是「问题单」「运行日志」「复盘」……
每来一个都照着前四个抄一遍（建表、写路由、写授权、写分页、写归档）。抄到第四遍能看清：
**它们的不同只在"声明"，机械部分是同一套**。于是把声明（集合）与数据（记录）分开。

- **`ncc store declare <集合>`**：字段（`title:string!` / `status:enum:open|closed` /
  `labels:string[]` / `body:text?search` / `owner:ref`）、能过滤的字段（`--index`）、
  能否修改（`--immutable` / `--append-only`）、可见性、单条上限、默认存活期 —— 全在一处声明。
- **`ncc store put`**：客户端**先读声明再发请求** —— 按声明在本地把值类型化
  （int / bool / string[] / enum 词表），没声明的字段、越界的枚举、写错的类型、缺的必填项
  **在本地就拒**（省一次往返，错误说在人近处）；动词按集合形态选（可变走 PUT，
  只追加/不可变走 POST）。
- **`ncc store list --where 字段=值`**：线上是 `?f.字段=值` —— 加前缀是为了不与保留参数撞车
  （一个集合完全可以声明一个叫 `status` 的字段）。
- **`ncc store kinds`** 离线可读：词表 / 上限 / 三条不变量。
- **包里的声明**：`state.stores[]`（`collection` / `mode` / `fields` / `index` / `shape` /
  `visibility` / `reason`），`nur profile` 四问如实显示「集合：issue（读改 · mutable · private）」，
  **R12 离线就拦**：index 里的字段没在 fields 里声明、集合名不合法、重复声明、
  数据快照包声明写（快照发出去是只读的）、只写却不说 reason（警告）。
  R12 **刻意不重新实现字段声明语法** —— 那套语法的唯一实现在节点侧，
  在这里再写一遍就成了"两套说法"；报错要发生在真正声明的地方。
- 三条不变量落在两端：**动态 ≠ 无模式**、**不可变就是不可变**、**CRUD ≠ 授权**。
- 验证：`bash scripts/store-smoke.sh`（61 通过 / 0 失败）。

### 新增 · HUR 包规范：**一个封装 + 多组 profile**（`ncc hur profile` / `match` / 数据快照包）

一句话：过去"这份包是什么"由一个只有三个值的 `kind`（`agent|harness|repo`）兼职，现在它有了正经的名字 ——
**profile**。共 11 个：`agent` `harness` `plugin` `mcp` `app` `scaffold` `skill`
`kb-seed` `mem-seed` `ckpt-set` `trace-set`。封装（确定性字节 + 清单 + 锁 + 签名）**不变**，
profile 只管"要什么、能不能跑、怎么接、按什么匹配"。设计见 `ncc-platform/prd/ncc-hur-spec.md`。

- **`ncc hur profile <本地包 | @命名空间/slug>`**：读「是什么 / 要什么 / 给什么 / **怎么接**」四问
  （`inspect` 只答了三分之一），外加**分级体检**：`✔ 结构 ✔ 自洽 ⚪ 签名` —— 三条轴分开报，
  不合成一个"通过"。`--list` 列规范里的全部 profile（`--json` 给别的工具读）。
- **`ncc hur match --profile kb-seed [--host cursor] [--capability mcp]`**：按 profile / 宿主 /
  能力在目录里找能用的包（**只读**）。如实说局限：目录不索引 profile，所以是"按 kind 拉一批 + 按清单位过滤"，
  清单里没写 profile 的条目标成"按 kind 推导"，**不混进"匹配上了"**。
- **R12：profile 决定必填项**（这才叫规范）。数据类 profile **禁止** `entry` 与
  `permissions.network`（一份知识库快照不该能跑代码、也不该自己出网），`skill` 禁止 `entry`，
  `plugin` 必须说清接哪个宿主；数据包还必须带 `data{}`（来源 / 时刻 / 隐私级别），
  且 `payload=full` 不许 `privacy=public`。**老包（没写 profile）不受影响**（有单测守着）。
- **目录里认得出来**：`ncc hur publish` 现在把 `manifest.profile`（含 `profile_declared`：
  作者写的还是按 kind 推导的）与快照声明带进条目，服务端零改动。字段名跟着包规范（snake_case），
  不另造一套 camelCase。
- **映射进规范**：`profile → 目录 kind` 由 `profile::registry_kinds` 唯一确定（`--kind` 仍可覆盖）。
  一处行为变化：`kind=repo`（脚手架）过去被记成 `hur`，现在如实记成 `scaffold`。
- **产物文件名也带 profile 段**：`<id>-<version>.<profile>.hur`（如 `…-0.1.0.kb-seed.hur`）——
  一个 `dist/` 里躺着几十个 `.hur` 时，一眼看得出哪个是能跑的包、哪个是一份数据。
  `.hur` 仍是最后的扩展名（侧车与解包机制不受影响）；**名字只是线索、清单才是权威**：
  改名只多一条提醒、认不出来的名字不吭声；找产物时新名字在前、老名字兜底。
  顺手收拢了三处**自己拼老文件名**的地方（签名/登记/提示词）—— 改命名后它们会静默找不到产物。
- **数据快照包（新）**：`kb / mem / ckpt / trace` 各自能导出成**不可变快照包**
  （`ncc kb bundle --as-package` · `ncc mem export --as-package` · `ncc ckpt export --as-package` ·
  `ncc trace export --as-package`），带来源 / 快照时刻 / 隐私级别 / 许可 / 载荷级别，
  可签名、可发布、可 `ncc hur data import --package <目录> --apply` 灌回去。
  - **默认只出计划**（与 `nur run` 同一条纪律）：这一步会改节点上的数据，不该悄悄发生。
  - **先验后导**：不合规或签名核对不过（`--require-signature`）就不落一个字节。
  - **隐私级别说了算**：`privacy != public` ⇒ 导入的每一份都按 `private` 落（并打印说明）；
    `full` + `public` 在生成阶段就拒绝。`mem-seed` 默认 `private`；`trace-set` 默认 `payload=full`
    （**宣称得比实际更严是危险的那个方向**）。
  - 轨迹包的落点是**本机暂存区**，上传是另一个动作（`ncc trace push`）—— 一个包不替使用者决定要不要上传。
- **红线**：**活状态不出门，只有快照能**。kb / mem / ckpt 是会被反复写、持续变大、默认私有的状态，
  它们住在节点上；能打包分发的是它们的不可变快照。（`mem-seed` 尤其：能分发的那一刻，它就不再是记忆了。）
- **端到端**：`scripts/profile-smoke.sh` **73 项全绿**（把 `entry` + 网络权限塞回数据包看 R12 拦不拦、
  计划阶段确认节点版本号没变、`--apply` 后内容一字不差、`full+public` 被拒、四种快照包往返）。

### 新增 · 制品加签：`ncc sign` / `ncc verify`（**不限 kind** —— Skill / MCP / 任意文件）

过去只有 `kind=hur` 能加签（签的是**规范打包字节**）。现在一份 `SKILL.md`、一份
`mcp.json`、任意一份字节也能被署名 —— 而且签的就是**你发布出去的那一份**，
第三方拿公钥 `minisign -V -p ncc.pub -m SKILL.md` 就能独立核对：**不需要 NCC，也不需要 `ncc-cli`**。

- **`ncc sign <文件>`**：本地签（私钥不出设备），写 `<制品>.minisig`；
  `--kind/--reference/--version` 只进**签名声明**（`ncc <kind> <引用> <版本> sha256=<hex>`，
  被签名保护）。`--json` 直接吐出可放进发布清单的 `signature{}` 块。
- **`ncc sign --attach <引用>`**：给**已发布**条目事后加签。唯一联网动作，而且
  **只上传签名文件与公钥**，制品字节一个字节都不传（与 `publish` 的离线铁律一致）。
  本机先挡一次「签名摘要 == 条目产物摘要」——**加签不是贴标签**，贴到别的字节上就是造假。
- **`ncc verify <文件 | @命名空间/slug>`**：本地文件全程离线；条目引用则
  下载字节 → **先核摘要**（拿回来的必须是登记的那一份）→ 再验签。`--require-signature` 可进 CI。
- **两处诚实边界写进判定里**：① 有签名但公钥本机不认识 ⇒ 只能说"有签名"，
  **不能说"已验证"**；条目里随签名一起带的公钥会做一次**自洽性**检查，但输出明确写"属**自证**"；
  ② 签名**对不上**（被改过 / 用错钥匙）**不需要任何开关就非 0 退出** —— 那不是"没签名"。
- **包不在这里签**：目录 / `.hur` 产物转交 `ncc hur sign`（签规范打包字节，
  连包身份、版本、`hur.lock` 一起核对），避免了同一件事长出两套实现。
- **`hur-core`**：`Claim` 增加 `kind` 字段并识别 `ncc <kind> <引用> <版本> sha256=` 语法
  （`hur <id> <version> …` 照旧，向后兼容），新增 `comment_for_artifact`。
- **端到端**：`scripts/sign-smoke.sh` **51 项全绿**（真改一个字节、真换一把钥匙、
  服务端摘要对不上被 400 拒绝、未签名时 `--require-signature` 非 0、加签只传签名文件）。

### 新增 · `ncc app`：NCC 舱（可自部署的个人 Agent 助理）

把「用户自己部署一个人助理（Muse 类：记得住你、有工作台、能安全地给别人看一部分）」变成一条命令。
分层沿用整个项目的铁律：**产品本体是用户的（`app.json`）、引擎是 ncc、应用逻辑是 HUR 包（`hur.json`）、
内容住节点、互联走平台**。

- **命令面**：`app init`（生成 app.json / hur.json / run.sh / README.md / SKILL.md；`hur.json` 必须过
  `hur_core` 与 `ncc hur verify` **同一份判定**）、`app doctor`（逐项自检，每项都给"下一步跑什么"）、
  `app up`（起本机 loopback 控制台）、`app status`、`app export`（可交付目录 + sha256 清单）。
- **舱记着两个目标**：内容走**当前目标**（ncc-registry 节点），分享走 `share.target`（默认 `hub` 云端）。
  "内容住节点、互联走平台"落到配置上就是两个目标名 —— 缺哪个 `doctor` 就说哪个。
- **本机控制台**（`cli/src/app_templates/console.html`）：四个面板（画布 / 记忆 / 检查点 / 给别人看）
  + 「生成快照链接」。面板只读；唯一的写动作是把当前内容打成**只读快照**上传成分享链接（人点的），
  对方**不用账号**就能看，带 key 的链接要把 key 一起发（只回显一次）。
- **顺手抽了一层**：网关里手写的 HTTP/1.1 服务端抽成 `cli/src/httpsrv.rs`，网关与控制台共用一份实现
  （协议细节只写一遍）。
- **一处安全加固**：控制台会渲染用户自己的内容，所以不给页面开 `unsafe-inline` —— 内联脚本用
  **一次性 nonce** 放行；编码时用显式锚点注入快照，**锚点找不到就报错**，不产出"打得开但空"的假快照。
- **一处目标管理修复**：按 `--base` 新建目标时，如果算出来的名字撞上已有目标，过去会**直接覆盖**
  （连带抹掉那台机器的登录态）；现在名字冲突一律换名（`local-2`），**绝不覆盖**，并补了单测。
- **端到端**：`scripts/app-smoke.sh`（引擎 + ncc-registry + ncc-platform 一起跑，隔离数据与 `NCC_HOME`）
  **51 项全绿**。设计见 `ncc-platform/prd/ncc-personal-agent.md`。

### 新增 · Agent 面跟上最新能力：网关控制面只读三件套（MCP 工具 30 → 33）

`ncc mcp` 的工具面此前落后于 CLI 已经落地的能力（轨迹与三样状态写在代码里、文档里没写；
网关控制面完全没进 Agent 面）。这次一起补齐 —— **Agent 能读，但改不了**：

- **新增三个只读工具**（能力 `gateway`，由云端 ncc.ai 声明）：
  `ncc_list_gateways`（有哪些网关 / 在线吗 / 用了多少）、`ncc_gateway_audit`
  （某台网关留存的**窗口摘要**：计数、**主机名**、状态桶、延迟分位）、`ncc_gateway_usage`（用量汇总）。
  参数用 `gateway`（`GW-…` 或名字；名字不唯一时列出候选，**宁可报错也不瞎挑一台**）。
- **工具说明里写死三条口径**（模型会照着说）：在线状态是控制面按心跳超时**推导**的、
  控制面**只有摘要**（路径与载荷不出网关本机）、用量是网关**自报**的（签名只证明来源与完整）。
- **文档口径纠正**：`agent/`（README / SKILL.md / harness.json）与站内文档原来都写"最多 20/23 个工具"，
  `harness.json` 的工具清单也漏了 7 个已有工具 —— 现在 `harness.json` 的清单**由 `mcp.rs` 生成**，
  契约和实现不会再各说各话。
- `SKILL.md` 新增三段工作流：G 运行轨迹（`payload=digest` 不能当训练材料、`score` 只对打过分的轨迹求平均）、
  H 三样状态（关键词加权检索、记忆没有公开档、写状态留在 CLI）、I 网关与合规审计（摘要 ≠ 完整审计；
  "谁调了哪个 URL"要去网关本机跑 `ncc gateway audit`）。

### 新增 · `ncc gateway` 接控制面：`bind` / `heartbeat` / `report` / `usage` / `audit --remote` / `unbind`

S2a 的网关只管本机转发与本地审计；现在它能**注册到控制面**（= ncc.ai）并把本地审计
**聚合成窗口摘要**签名上报 —— **路径、载荷、凭据都不出本机**，控制面只看到计数、字节数、
**主机名**、状态桶、延迟分位。

- **`ncc gateway bind [--namespace @org] [--name …]`**：注册网关 → 令牌（`ncc_gw_…`）
  写进 `~/.ncc/gateway.json`（0600，**只回一次**）。换绑会清空上报账本（本地审计文件不动）。
- **`ncc gateway heartbeat [--status online|draining]`**：缺省语义是 `online` ——
  不这样的话一次 `draining` 会把网关永远钉在 draining 上（`offline` 只能由控制面推导，不能自报）。
- **`ncc gateway report [--since] [--window-minutes N] [--dry-run] [--drop-rejected]`**：
  本地账本 `~/.ncc/gateway-report.json` 记**水位 + 待传队列**，**先落盘再发送** ——
  控制面不可达 / 令牌被吊销 / 机器重启，审计都不丢，恢复后自动补传；被拒的摘要**留在队列里**
  并报出原因（`--drop-rejected` 才丢）。失败**不静默**：报原因 + 待传条数 + 两条出路。
- **`ncc gateway usage`** / **`ncc gateway audit --remote [--csv] [--since]`**：
  用量汇总（输出里写明「自报计数」）/ 看与**合规导出**控制面留存的摘要。
- **`ncc gateway unbind [--purge-state]`**：注销 + 清本地绑定；控制面不可达也能解绑
  （否则这台机器既报不上去也清不掉）。
- **`ncc gateway run`** 在绑定时起后台线程，按 `heartbeat_sec` / `report_sec` 周期做事。
- 摘要的规范字节沿用 `ncc trace` 的约定（长度前缀 `key=len:value`，**不是 JSON**）：
  键顺序/浮点表示在 Rust 与 Go 里不一样，用 JSON 会算出两个 digest。两边各有一份固定向量测试。

### 新增 · 托管状态：`ncc kb` / `ncc mem` / `ncc ckpt`（知识库 / 记忆 / 检查点）

Agent 的三样**状态**住到节点上：知识库（语料）、记忆（键值 + TTL + 来源）、检查点（不可变快照 + 血缘）。
它们**不是制品**（包是能力：内容寻址 + 有签名；状态是数据：会被改写、会长大、默认私有），
所以包只在 `hur.json` 的 `state{}`（规则 R11）里**声明**要哪些，字节住节点。

- **命令组 `ncc kb`**：`ls` / `get`（`--revision N` 取历史版）/ `set`（存在即新版本）/ `search`（关键词加权，
  服务端打分）/ `history` / `archive` / `restore` / `rm` / `bundle` / `kinds`，以及
  **`ncc kb pull --package <目录>`** —— 读包的 `state.kb` 声明，按 `checksum` **增量**拉到 `~/.ncc/kb`
  （写 `index.json` 记校验和）；包没声明时**明确拒绝**，不替它猜。落地前逐篇核对正文的 `sha256` 与声明一致。
- **命令组 `ncc mem`**：`set`（同键即更新，`--ttl-days` / `--kind` / `--source` / `--confidence` / `--pin`）/
  `get`（Agent 读记忆的主路径）/ `ls`（默认不含过期）/ `rm`（id 或 key 都认）/ `gc` / `kinds`。
- **命令组 `ncc ckpt`**：`save`（算摘要 → 建元数据 → 传字节；`--parent-last` 自动接血缘、`--meta k=v`）/
  `ls` / `show` / `pull`（**落盘前核对摘要**）/ `lineage` / `prune`（每个制品留最新 N 个）/
  `rm` / `kinds`。`ncc ckpt pull` 拿字节优先走**短时签名地址**（对方不用带凭据）。
- **MCP 新增 5 个只读工具**（共 27 个）：`ncc_list_kb` / `ncc_get_kb` / `ncc_list_mem` / `ncc_get_mem` /
  `ncc_list_ckpt`。**写不在工具里** —— 让一次工具调用悄悄改掉 Agent 的记忆或知识，事后没人能复盘。
- **位置参数命名踩坑（已记）**：`mem rm` / `ckpt show|pull|lineage|rm` 的位置参数**不能**叫 `target`
  （顶层 `--target` 是 global 的，clap 会把值塞进全局目标，报「没有名为 X 的目标」），一律叫 `reference`。
- **KB 缓存目录**：与轨迹的本地暂存同一套（`~/.ncc/kb`，`NCC_HOME` 可改根），不家目录乱放。

### 新增 · `ncc trace`：运行轨迹（能力评估 + 后训练数据集）

把「真跑过什么」变成可用的数据：**同一份轨迹**既回答「这个版本好不好」（成功率 / 耗时 /
 token / 花费 / 人工结论），也能导成**后训练数据集**（JSONL + 奖励 / 得分 / 切分）。

- **命令组 `ncc trace`**：`add`（采集）/ `ls` / `show` / `push`（上传）/ `stats`（聚合）/ `export`（数据集）/ 
  `label`（评测标注）/ `kinds` / `rm` / `status`。
  除 `push` 与带 `--remote` 的以外**全部本地可用**（离线也能采集与看结论）。
- **三种采集输入**：原生 `ncc-trace/v1` 文档；`ncc hur run` 的执行留痕（自动收敛成 `kind=hur-run`）；
  任意 Agent 的事件流（每行 `{"type","name","ms","in","out"}`）。
- **`ncc hur run --exec` 跑完自动采集**：留痕 → 一条轨迹写进本地暂存（**不联网**），
  `ncc trace push` 才上传。留痕里没有提示词与输出，所以转出来的轨迹如实标成 `payload=digest`。
- **内容默认不上传**：`--payload digest|preview|full`（默认 digest），`--redact strict|basic|off`
  （密钥 / 邮箱 / IPv4 / 长随机串）。采集时就脱敏，并把**做过什么**写进 `redaction.rules`。
  摘要**不含原文**，所以脱敏不影响幂等去重（有测试钉住这个性质）。
- **摘要跨语言一致**：`trace_digest_core` 与 ncc-registry 的 Go 实现逐字节相同
  （长度前缀拼接，不用 JSON 序列化），两边测试用**同一个 sha256 向量**。
  因此轨迹里没有浮点：金额用微美元、得分用千分位整数。
- **本地暂存** `~/.ncc/traces/`（`spool/*.json` + `pushed.json` + `labels.json`，权限 0600）；
  `push` 幂等（重复上传算 `duplicates`），`--prune` 才删本地副本。
- **MCP 新增两个只读工具** `ncc_list_traces` / `ncc_trace_stats`（共 22 个）；
  **写入口（push / label）故意不暴露给 Agent**。
- 15 个单测：跨语言摘要向量、脱敏规则、预览按字符边界截断、事件包装、留痕收敛、校验、路径穿越防护。

实测（真起 registry + 真跑 wasm 包）：事件流采集 → 邮箱与 `sk-` 密钥被替换成 `<email>`/`<api_key>`；
push → 服务端可见；重推一次 → `duplicates 1`；另一个账号 → 看不到（默认私有）；
`ncc hur run --exec` → 自动多出一条 `kind=hur-run` 轨迹；`export --remote` → JSONL + 数据集摘要。

### 新增 · HUR 清单的 `egress` 出口声明（R10）+ 网关路由**绑到包**（S2b①）

把「我愿意提供一个出口」从**一份手写 JSON** 变成**一份可签名、可分发的包声明**。

- **清单新字段** `egress.provides[]`：每条 = `name` / `target` / `paths` / `methods` / `inject`。
  `inject` 里**只有请求头名，没有凭据** —— 包要能被签名、分发、公开检索，密钥就不能进包。
- **新规则 R10**（`ncc hur verify` 会一并跑）：路由名 `[a-z0-9._-]` 且不重复；`target` 必须 `https://`
  （本机 loopback 允许 `http` 用于联调）；`paths` / `methods` **不能为空**（空 = 放行一切，不允许）；
  要出网就得在 `permissions.network` 里出现（不在则 warn，与 R5 权限面一致）。
  R10 抽成 `hur_core::spec::validate_egress` —— **`verify` 与网关用同一份判定**。
- **网关侧**：`accept` 路由新增 `"hur"`（包目录，或直接指 `hur.json`）**必填**：
  路由必须由包里**同名**的 `egress` 声明背书，且本地配置**只能更窄** ——
  `target` 精确相等，`paths` / `methods` / `inject` 逐项子集；任何一处更宽都在启动时**逐条报出来并拒绝启动**。
- 规则不再各写一份：网关的 target 解析 / 路径归一化 / 路由名与头名合法性 / loopback 判定
  **全部改用 hur-core 的同一批函数**（否则「配置只能比声明更窄」无从判定）。
- `ncc gateway check` 会打出背书的包 id/版本、**声明范围**，以及「本配置用了声明里的 N/M 条路径」。
- 新增单测：`hur-core` 8 个（R10 通过/缺失/各种不合法、`egress_covers` 只窄不宽、路径归一化、
  危险 target 形状），`ncc` 4 个（缺 `hur` / 比声明宽 / 找不到同名声明 / 声明自己不合法 → 拒；更窄 → 过）。

实测（真起进程）：accept 不写 `hur` → 拒；多一条 `paths: models` → 拒并指名该路径；
**只改包声明、本地配置不动** → 立刻变成拒启动（这就是 S2b 的要害）；合规时白名单内 200
（上游看到 `inject` 里 B 的凭据，调用方塞的 `X-Api-Key` 根本没发出去），白名单外 403 且不发出。

> 仍只是「绑到声明」，**不是「绑到签名」**：网关没强制要求包已签名（与 S2a 的已知边界一致）。
> 要做就该做成一条：`accept` 路由可要求「包签名可核对且已 trust」，否则拒启动（归入 S3/S4）。

### 新增 · `ncc gateway`：固定路由的白名单代理（S2a）

网关的第一块真代码：一个**人配置过的转发 + 强制执行点**（`ncc-platform/prd/ncc-gateway-prd.md` §16）。
**纯本地** —— 不向 NCC 上报任何东西，数据面只在 A↔B 之间。

- `ncc gateway init | check | run | status | audit`；配置 `~/.ncc/gateway.json`。
- 同一进程两个方向：`accept`（我这台机器提供出口）/ `forward`（借对端出口）。
- **调用方永远不能指定目标地址** —— 只能给「路由名 + 路由内的一条路径」；上游地址与凭据全在 B 的配置里
  （`inject`），调用方自带的 `Authorization` **绝不透传**（只透传 `content-type` / `accept`）。
- 默认拒绝：路径不在白名单 → 403 **且请求不发出**；方法不允许 → 405；令牌不对 → 401；超配额 → 429；
  路径含 `..` / `%` / 反斜杠 / 空段 → 403；body 超上限 → 413。
- **不跟随重定向**（跟随 = 3xx 能把请求带去别的域，白名单形同虚设）。
- 两侧 JSONL 审计，**只记元数据**（无载荷、无凭据）。
- 不安全的配置**拒绝启动**：accept 无令牌 / 空白名单 / 空 methods；非 loopback 监听未显式承担风险；
  非 loopback 的明文 `http://` 出站。
- 不引新依赖：HTTP 服务端用 `std::net` 手写（约 200 行，只支持定长 body），出站用已有的 `ureq`。
- 13 个单测（时间格式化、路径穿越拒绝、白名单精确匹配、target 解析、令牌常量时间比较、每条启动闸、配额）。

实测（假上游 + A 网关 + B 网关）：允许路径 → 200，上游看到的是 **B 的**凭据与调用方载荷，
调用方自己的凭据从未到达；白名单外的路径 → 403 且上游日志证明**没发出**；令牌错 → 401；
`%2e%2e` 与空段 → 403；一分钟内第 3 次 → 429；两侧审计文件里 grep 不到载荷与凭据；
停掉 B 后 A 回 502 **不降级**，B 回来后自动恢复。

本次不含（S2b/S3/S4）：P2P 那一跳、审计摘要上报控制面、计量计费 —— **路由绑到包已在上一节补上**。

### 新增 · **`ncc hur run --exec`**：本机执行（`ncc` 成为 harness-use 的执行引擎）

用户口径：「harnessuse 的 hur sandbox 功能也需要在 ncc 中实施，**ncc-cli 是一个事实的 harness-use 的执行引擎**」。

- **`hur-sandbox` 从 harnessuse 整包搬入本仓**（`cli/crates/hur-sandbox`，wasmtime 36）：
  逻辑**一行未改**，连它那 12 个安全保证测试（死循环 / 内存炸弹 / 越权工具 / 越权外呼 / ABI 缺失…）一起带过来，在 ncc 的 CI 里跑。保留 **MIT**。
- **执行接线** `cli/src/hurrun.rs`：宿主能力桥（`repo_search` 只在包目录内检索 · `task.create` 写 `~/.harnessuse/tasks/` · `kb.search` 如实回答）+ 限额映射 + 留痕。
  与搬之前的关键区别：**不再需要 shell 出 `ncc hur run --json` 拿计划** —— 计划在同一进程里由 `hur-core::policy` 算，"策略怎么判"与"按策略怎么跑"是同一份代码。
- **引擎注册**：启动时 `policy::register_engines(hur_sandbox::ENGINES)`（CLI 与 `ncc mcp` 看到同一份事实）；于是 `ncc hur run` 的计划里 `engine=wasm` 且 `engine_ready=true`，`ncc hur sandbox` 如实显示"本二进制托管 wasm"。
- **默认开启、可关**：`sandbox` feature 默认开（release 二进制 ~12MB）；`--no-default-features` 得到不含 wasmtime 的瘦身版（~5.3MB），**不注册任何引擎**，`--exec` 如实拒绝并指路。
- **出网开关**：`hur.http_get` 先过包的 `permissions.network` 白名单与次数上限（越权拦下整次运行），再过生效策略的 `exec.local_only`（默认档为真 = 数据不出设备，一个包外字节都不发）。
- **留痕修正**：trace 文件名改为 `{时间戳}-{包 id}-{序号:02}.json`，序号**总是**带上 —— 修掉两个真 bug：
  ① 同秒连跑两次会互相覆盖（"跑过必留痕"落空）；② 旧命名 `…-01.json` 的字典序排在 `….json` 前面，`retain` 裁剪反而删掉**最新**那条（`retain=1` 会留下最旧的）。`ncc hur task inspect <文件名>` 改为按**真实文件名**匹配，不再靠猜。
- 实测（隔离实例）：正常包 ✅ 跑通并留痕；死循环 ✅ `[fuel] 指令预算耗尽`（退出码 1，留痕照写）；未声明域名外呼 ✅ 拦下且**没真的发出请求**；同秒两次 ✅ 两条留痕都能 `task ls` 看到。
- **`--exec --json` 只输出一份 JSON**：原先会先打印「计划」JSON、再打印「执行结果」JSON，两份连在一起
  没有任何机器能解析 —— 而 `--exec --json` 恰恰是桌面端/脚本委派要用的形式。现在执行结果里自带完整的 `plan`
  （含 `checks` / `reasons`，即「为什么允许跑、按什么限额」）。
- **发布脚本可出瘦身变体**：`scripts/build-release.sh --slim`（或 `NCC_SLIM=1`）→ `--no-default-features`，
  产物名带 `-slim` 后缀，与常规产物共存。输出目录可用 `NCC_RELEASE_OUT` 覆盖
  （默认的 `release/bin/*` 是被 git 跟踪的已发布产物，验证脚本时别误盖）。CI（`release.yml`）不用这个脚本
  （它要「要么全出、要么明确失败」，且是各平台原生构建），要在发布里加瘦身变体得改 workflow 矩阵。
- **跨平台构建已核实**：CI 是各平台原生构建（macos / ubuntu / windows 各自 runner），唯一的同 OS
  交叉案例 `x86_64-apple-darwin` 已在本地实跑通过（`Mach-O 64-bit executable x86_64`）。
  release 体积：含沙箱 12MB（arm64）/ 15MB（x86_64），瘦身 5.3MB。
- **修掉另一个仓库外的坑**：`scripts/build-release.sh` 里 `$os/$arch（target 不可用）` 之类的写法在 macOS 自带
  bash 3.2 下会把中文吞进变量名 → `set -u` 报 `unbound variable`（`--all` 分支原本就中招：本该打印「跳过」却直接崩）。
  全仓脚本已扫描并改为 `${VAR}`（扫描剩余数为 0）。

### 新增 · **`ncc hur attach`**：把本机签名附到**已发布**条目上

补上「事后加签」这条路：发布时没签、或换了签名，现在不必重发新版本。

- `ncc hur attach [path] [--ref @命名空间/slug] [--sign]`：
  - 按**包 id**（工程的稳定身份，不是 slug —— slug 是发布时自定的）在自己名下找条目；
    引用用 `--ref` 而非位置参数：两个位置参数都有默认值时 clap 按顺序填充，
    `ncc hur attach .` 里的 `.` 会被当成引用（实测踩到）。
  - `--sign`：本机还没签名时先签一次。
  - **本地工程改过就拒**：附着前核对「本机产物摘要 == 签名覆盖的摘要」，不一致要求重签
    —— 旧签名属于旧的字节，贴到新产物上不是「加签」。
  - **只上传签名文件与公钥**，包内容一个字节都不传（与 `publish` 的离线铁律一致；
    故 `sign`/`verify`/`pack` 仍然从不联网）。
- 服务端配套：平台 `PUT /api/registry/:id/signature`（已发布）；内网节点同路径，
  额外支持 `@ns/slug` 两段引用，且**副本只能到源头节点加签**。
- `ncc hur trust` 在条目没签名时的提示改为指向 `ncc hur attach --ref …`。
- 参数与 `ncc hur sign` 对齐：`--key`（签名私钥）/ `--password` / `--pub-key`（核对用公钥）。
  用**非本机**密钥签时尤其需要 `--pub-key` —— 否则 `check_dir` 找不到配对公钥，结论是「有签名但不可核对」而拒绝。


### 新增 · **`ncc`**：`ncc hur`（hur 制品工具链内嵌进客户端）

> ⚠️ 本节写于 **sandbox 搬入之前**：其中「只出计划 / 不内置沙箱运行时 / wasmtime 不进 ncc /
> 托管仍在客户端」的表述已被本文件**更上方**同日条目
> 「**`ncc hur run --exec`：本机执行（`ncc` 成为 harness-use 的执行引擎）**」取代
> （用户二次改判：ncc-cli 就是执行引擎）。下面保留原文，作为当时的决策记录。

- **`hur-core` 从 harnessuse 迁入本仓**：`cli/crates/hur-core`（制品规范 `harness-use-package/v1`：R1~R9 校验 / 确定性打包 / 安全策略与执行计划 / Minisign 签名 / 互操作导出 / MCP）。`cli/` 现在是 Cargo 工作区根（`members = ["crates/hur-core"]`），**`cli/target` 与 `scripts/build-release.sh`、`.github/workflows/release.yml` 的路径一行未改**。crate 保留 **MIT**（本仓整体 Apache-2.0，此 crate 例外）。纯 Rust、无 C 依赖；**不含 wasmtime**。
- **`ncc hur` 子命令**（薄壳，能力全在 hur-core）：
  - 本地/离线：`init`（生成合规工程 + `hur.lock`）· `build` · `pack` · `sign` · `key gen|show|pub|trust|trusted|untrust` · `verify`（R1~R9，含签名）· `inspect` · `ls`
  - `run`：只出**可审执行计划**；`--exec` 如实拒绝并指路（`ncc` 不内置沙箱运行时 —— wasmtime 不进这个二进制，执行属客户端）
  - `publish`：本地 `verify` → `pack`（确定性字节）→ 策略要求或 `--sign` 时签名 → 走 **ncc 的 registry 条目模型**（`kind=hur` + `manifest.hur` 带包元数据与签名指纹）。**离线铁律**：只有这一步联网，且只上传"已经打完包的字节"。
  - **沙箱治理（同日追加）**：`sandbox`（治理视图：认识的引擎 / 本二进制托管了什么（现在=无）/ **各引擎托管归属表** / 生效策略与限额 / 已登记环境 / 留痕条数）· `dep`（依赖对账：声明 ↔ `hur.lock` ↔ 实际字节，六状态 `ok/unlocked/missing/drift/unresolved/stale`，有缺失或漂移退出码 1）· `env ls|show|use|add|rm`（沙箱环境登记与证明，非法登记照拒）· `task ls|inspect`（执行留痕 `audit.trace_dir` ＋ 投递任务 `~/.harnessuse/tasks/`；`inspect latest|序号|文件名` 显示当时的限额快照/宿主调用/用量/错误）。**沙箱拆两面**：治理归 NCC（这些命令，不执行代码），托管仍在客户端（`ncc` 不链接 wasmtime）；将来节点托管只需改那张托管归属表。
  - 新模块 **`hur-core::dep`**；`pack::local_dep_hash()` 抽出来给锁与对账**共用同一算法**（否则 drift 判断就是猜）。
- 三条边界写进文档与实现：离线铁律 · 身份一次到底但不复制账号（用 ncc 登录态，签名私钥仍在本机 `~/.harnessuse/keys`）· wasmtime 不进 ncc。设计见 `ncc-platform/prd/ncc-hur.md`。
- 实测（本地隔离实例）：`init → verify → key gen → sign → verify(✔ 签名有效) → pack → run --exec(拒绝) → publish(--sign) → search --kind hur → info`，条目 manifest 里能看到 `signature.keynum`。

### 新增 · **`ncc hur`**：导出 / 信任条目 / 策略 / MCP / 安装 / 互操作（P-23）

把 `ncc hur` 补成**完整**的 hur 工具链，目标只有一个：**客户端不必再自带一份 `hur-core`**。

- **`ncc hur export`**：导出可分发产物 —— `.hur` + `.minisig` + `.sha256` + **公钥** + `export.json`
  （说明书：清单 / 入口 / 权限面 / 安全策略 / 签名指纹 / 当时生效策略 / R1~R9 结论）。
  离线；先自己验一遍，不过就拒绝导出（`--allow-issues` 可明确覆盖）。
- **`ncc hur trust <条目>`**：信任一个**已发布条目**的签名公钥，顺序是**先核对再信任** ——
  下载条目里的产物与 `.minisig`、比 sha256、验签名（要求 `SigOutcome::Verified` **且**待信任公钥的
  keynum 与签名一致），通过才写进 `~/.harnessuse/trusted-keys.json`。
  实测拒绝：换错公钥 / 公钥文本非法 / 条目未签名 / **产物字节被换过**。`--force` 可跳过，
  但会在信任表里留下「未核对」的证据。
- **`ncc hur policy show|check|path|presets|set`**：`set` 只动显式给了的字段，写完**重新解析**
  并逐项对照「你要的 → 实际生效的」，被上层更严的层挡回去的标 ⚠。
- **`ncc hur mcp`**：把 hur **治理面**做成 MCP 工具面（stdio，9 个**只读**工具）。
  `mcp.rs` 抽出 `serve_face`，与 `ncc mcp` 共用同一套 JSON-RPC 收发 —— 协议细节只写一遍。
  `--list-tools` 只把工具表（含 `server/command/args/instructions/tools`）打成 JSON 就退出，
  给客户端（如 harness-use GUI）生成 `.mcp.json` 提示用 —— 免得工具名在客户端再拄一份。
- **`ncc hur install|uninstall|list`**：落点与桌面端**同一个** `~/.harnessuse/packages/<id>/`
  （`HUR_HOME` 可覆盖）并登记桌面 `config.json`。远端安装要求 sha256 相符；**条目带签名时
  签名也必须相符**（本机认不认识那把钥匙只提醒，不拦）。
- **`ncc hur interop`**：渲染成 Claude / Cursor / Cline / Codex / MCP 能直接用的产物（默认只预览）。
- **`publish` 上传 `.minisig` + 公钥**：以前条目只留 keynum（自述的指纹，不是证据）。现在签名
  字节复用同一个 `/api/registry/uploads` 存起来（**服务端零改动**），公钥进 `manifest.hur.signature`。
- 顺带修掉一个会吓人的语义（`hur-core::policy::resolve`）：工程/机器层写的策略 `id` 与包内请求的
  具名策略**同名**时，原先算作「宿主已选定 → 请求不叠加」，等于「只写了一行 id 就把请求的整套规则
  悄悄顶掉」；现在同名即同一策略，照常叠加。

### 变更 · **包落点归位到 `~/.ncc/packages`**（P-24）

- **落点**：`install` / `ncc hur install|write|list|uninstall` 的包目录从 `~/.harnessuse/packages` 改为
  **`~/.ncc/packages`**（`NCC_PACKAGES_DIR` 直接覆盖；`NCC_HOME` 当 HOME 用，与 `ncc` 其它路径一致）。
  与顶层 `ncc install` 共用同一个实现（`hur-core::cfg::packages_dir`），两个入口装出来的包必须互相可见。
  `~/.harnessuse` 保留的是**本机工具状态**：签名密钥 `keys/`、`trusted-keys.json`、`environments.json`、
  `security.json`（机器层策略）、`tasks/`、桌面端 `config.json`（含 `packages[]` 索引）。
- 新增 **`ncc hur write [path]`**：把工程目录写进包落点并登记（桌面端「写入本机」用的就是这一步，不产包）；
  **`ncc hur pack`** 在目录已是本机包（有 `_install.json`）时**顺手刷新登记里的产物 sha256**。
- 新增 **`ncc hur publish --update`**：slug 已存在时改为更新（只改版本 / 产物地址 / 状态；
  **清单里的签名与权限面不会变**，命令会明确提醒 —— 改过签名请重发）。
- 新增 **`ncc login --api-key <key>`**：直接拿 API-Key 当登录态（机器人 / 桌面端绑定用，不必知道密码）；
  `--email/--password` 因此变成可选。
- 新增 **`ncc hur mcp --list-tools`**：只把工具表打成 JSON 就退出（客户端据此生成 `.mcp.json` 提示，
  免得工具名在客户端再抄一份）。

### 修复 · **`ncc`**：`ncc info <id | @org/slug>` 一直不可用

- 顶层 `--target` 是 `global = true`，而 `Cmd::Info` 的位置参数**字段名也叫 `target`** → clap 的 arg id 撞车，位置参数的值被当成"目标名"，任何 `ncc info R-…` / `ncc info @ns/slug` 都报「没有名为 … 的目标」。
  位置参数改名 `reference`（对命令行用户不可见）后恢复正常。


### 变更 · **`ncc-registry`** 独立成库，本仓库改用 submodule 引入

- 内网托管节点搬到 [`fusedmodel/ncc-registry`](https://github.com/fusedmodel/ncc-registry)，
  module 路径 `github.com/fusedmodel/ncc/ncc-registry` → `github.com/fusedmodel/ncc-registry`。
  原来所有代码都在 `internal/` 下，模块外 import 不到任何一个包 —— 拆出来时把
  `config` / `model` / `storage` / `store` / `httpapi` 提到顶层成为**公开包**，
  `p2p` / `secretbox` 留在 `internal/`。
- 那个仓库同时发布了二进制（`cmd/ncc-registry` —— 顺带修好了一个一直存在的缺口：
  本仓库的 `README` / `ncc-registry/deploy/Dockerfile` / `scripts/smoke.sh` 一直在
  `go build ./cmd/ncc-registry`，但这个入口**从来没有被提交过**）与库本身，
  **从那边的 `v0.1.0` 起走自己的版本线**，变更历史见那个仓库的 CHANGELOG。
- 本仓库路径 `ncc-registry/` 现在是 **git submodule**：直接 `git clone` 该目录为空，
  需要 `--recurse-submodules` 或 `git submodule update --init`。CLI（`cli/`）与
  npm 包不受影响 —— `ncc registry …` 走的是 HTTP 契约，不依赖源码在同一仓库里。

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

[未发布]: https://github.com/fusedmodel/ncc/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/fusedmodel/ncc/releases/tag/v0.3.0
[0.1.3]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.3
[0.1.2]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.2
[0.1.1]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.1
[0.1.0]: https://github.com/fusedmodel/ncc/releases/tag/v0.1.0
