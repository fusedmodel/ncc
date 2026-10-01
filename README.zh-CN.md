# NCC Registry

[English](README.md) · [注册中心](https://ncc.ai) · [问题反馈](https://github.com/fusedmodel/ncc/issues) · [更新日志](CHANGELOG.md)

![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)
![Status: alpha](https://img.shields.io/badge/status-alpha-orange)
![Rust](https://img.shields.io/badge/rust-1.98%2B-orange)

**NCC Registry** 的官方命令行客户端。NCC Registry 是一个中立、跨协议的**能力制品（capability artifact）**注册中心，收录 API、Skill（`SKILL.md`）、MCP Server、Harness（含 HUR）、Plugin、Scaffold、Docker 镜像、Benchmark 与活体节点。

`ncc` 是一个单一的 Rust 二进制：不需要 Node / Python 运行时，也不依赖系统 OpenSSL。它覆盖制品的完整生命周期 —— 注册、发布、检索、安装、下载，另含面向 CI 的 API-Key、设备状态上报与 NCC Terminal 能力命令台。

```bash
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --slug hotel-skill
ncc search skill --tag hotel
ncc install @you/hotel-skill
```

## 为什么需要它

- **一次发布，处处可解析。** 每个制品都有稳定引用 `@命名空间/slug`，任何 Agent、Hub、CI 任务或同事都能解析，不绑定任何厂商或 Agent 框架。
- **开放格式，不是私有黑盒。** 制品就是普通文件（例如 `SKILL.md`），另附 manifest 契约与 `sha256` 摘要 —— 可读、可 diff、可镜像。
- **机器友好。** CLI 全部经由公开 HTTP API（`/api/…`），脚本、CI 流水线及其它客户端可直接对接注册中心，无需 shell 调用 `ncc`。
- **可自托管。** 用 `--base` 指向任意注册中心实例。

## 当前状态

> **Alpha（`0.1.0`）—— 尚未对外分发。** 注册中心处于邀请制闭测，命令与数据结构仍可能调整。
>
> - **目前唯一端到端可用的安装方式是源码构建。** `@fusedmodel/ncc-cli` 未发布到 npm，GitHub 也没有 Release，安装脚本与 npm 启动器都还没有可下载的来源。
> - **没有公开的托管注册中心。** `ncc.ai` 尚未上线，且客户端内置默认地址指向**本地**实例（`http://localhost:8181`）。现阶段请始终显式传 `--base`。
> - **面向人的 CLI 输出目前是中文。** 面向程序的接口（退出码、stderr 错误、`ncc info` 的 JSON）稳定且与语言无关；英文输出层在计划中。

## 安装

### 从源码构建（当前推荐）

```bash
git clone https://github.com/fusedmodel/ncc.git
cd ncc/cli
cargo install --path .        # → ~/.cargo/bin/ncc
```

或只构建不安装：

```bash
cargo build --release         # → cli/target/release/ncc
```

需要 Rust 工具链（edition 2021，已在 rustc 1.98 上验证）。TLS 由 `rustls` 提供，无需安装 OpenSSL 开发包。

### 安装脚本（等有可达的注册中心后可用）

注册中心实例会通过 `/install.sh` 提供自己的安装脚本：

```bash
curl -fsSL https://<your-registry>/install.sh | sh   # → ~/.ncc/bin/ncc
```

脚本会按操作系统 / 架构挑选二进制，并遵循 `NCC_RELEASE_BASE` 决定下载来源。该路径已实现，但**目前还无法对着任何公网域名使用** —— 见[当前状态](#当前状态)。

### npm 包装（源码就绪，尚未发布）

包装代码位于 [`packages/ncc-cli`](packages/ncc-cli)，源码完整且已入库，但包还没发布到 npm：

```bash
# 发布后可用：
npm install -g @fusedmodel/ncc-cli     # 或：npx @fusedmodel/ncc-cli --help

# 想自己从检出目录发布：
cd packages/ncc-cli && npm publish --access public
```

包装只是一个薄启动器：定位二进制后转发参数、stdio 与信号。查找顺序：

1. `NCC_BIN`（显式路径）
2. 随包分发的 `vendor/ncc-<os>-<arch>`
3. `~/.ncc/bin/ncc`
4. `cli/target/release/ncc` —— 仓库内构建，供开发使用
5. 都没有时，下载发布二进制到 `~/.ncc/bin/ncc`

`postinstall` 会尽力执行同样的下载，且失败不会阻塞安装。

> npm 上无作用域的 `ncc` 属于一个无关的包，因此包装发布在 `@fusedmodel` 作用域下：`@fusedmodel/ncc-cli`。

> **全局**安装时，能不能敲出 `ncc` 取决于 **npm 全局 bin 目录**在不在 PATH 上 —— 二进制装到 `~/.ncc/bin`，软链在 npm 的 bin 目录，这是两个地方。npm 前缀是 `/usr/local/lib/npm`（常见默认值）时该目录**不在** PATH 上，结果就是 `command not found`。先用 `npx @fusedmodel/ncc-cli --version`（不依赖 PATH）确认装好了，再 `export PATH="$PATH:$(npm prefix -g)/bin"`。

### 校验预编译二进制

预编译二进制及其 SHA-256 已入库，位于 [`release/bin`](release/bin)：

```bash
cd release/bin && shasum -a 256 -c checksums.txt      # macOS
cd release/bin && sha256sum -c checksums.txt          # Linux
```

## 快速开始

由于尚无公开注册中心，以下命令请对着本地或自托管实例执行：

```bash
# 0) 指向某个实例：已存在同名目标就复用，否则新建一个目标并切过去
#    （不会再默默改掉你原来的目标；现在什么样的目标都有：ncc target list）
ncc --base http://localhost:8181 me

# 1) 注册账号（会自动创建个人命名空间）
#    闭测期需要邀请码。
#    如果这台上第一个注册的账号，它还会自动成为节点/云端管理员，
#    并打印一次性 admin key/secret（见「节点管理」）。
ncc register --email you@example.com --password 'a-strong-password' \
             --name You --invite NCC-2026-INVITE
ncc me

# 2) 从本地 SKILL.md 发布一个 Skill
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" \
            --slug hotel-skill --tags hotel,travel --summary "Booking helper"

# 3) 检索与消费
ncc search skill --tag hotel
ncc info     @you/hotel-skill
ncc download @you/hotel-skill -o hotel.md
ncc install  @you/hotel-skill          # → ~/.ncc/packages/@you/hotel-skill/

# 4) 在 CI 中自动发布
ncc key create --label ci              # secret 仅显示一次，请妥善保存

# 5) 可选：名片与节点
ncc profile set --headline "把模糊需求落成能上线的 AI 系统" \
                --roles fde,agent-engineer --availability open
ncc profile                            # 查看名片与短链
ncc living --name my-agent --kind agent --capabilities mcp,api   # 注册一个节点
ncc nodes                              # 我的节点 + 连接的节点
ncc agent share ./my-agent             # 把我设计好的 Agent 分享给指定的人（点到点链接）
ncc agent add 'https://ncc.ai/a/AC-…'  # 收下别人给的 Agent：装包 + 连接节点
ncc sandbox init --host 10.0.0.5 --port 8282 --key <key>   # 云电脑：登记一台能接活的机器
ncc sandbox run --on office --cmd "docker build -t me/app . && docker push me/app" --reason "发版"
ncc services match "帮我订杭州的酒店"    # 按意图找服务（服务提供方打包的业务）
ncc terminal
```

接上内网节点时：

```bash
# 先看一眼我现在连着谁（云端 / 内网节点各是什么、各自声明了什么能力）
ncc target list

# 新建一个内网节点目标（地址写错当场提醒），并切过去
ncc target add office --base http://10.0.0.5:8282 --use
ncc --base http://127.0.0.1:8282 login --email you@corp.com --password '***'

# 临时用一下别的目标（不改默认）
ncc --target hub me
ncc hub publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --slug hotel-skill
```

## 目标：云端 ncc.ai 与内网节点

`ncc` 是一个客户端，而 ncc 有**两个世界**：

| 世界 | 默认目标名 | 它是什么 | 在它上面做什么 |
|---|---|---|---|
| **云端** | `hub` | ncc.ai 公共注册中心 | 服务市场（`services`）、名片（`profile`）、分享页、计费、运营后台 |
| **内网节点** | 自定（`local` / `office` …） | 自托管的 `ncc-registry` 单二进制 | 制品、节点、配置托管、分享链接、节点治理、集群 |

两者都叫 registry，接口名也有重名（`/api/nodes`、`/api/grants`、`/api/admin` …）但语义不同，
所以 CLI 用**目标（target）**把「跟谁说话」显式化。

```bash
ncc target list                  # 当前目标、各目标地址与登录身份、各自声明的能力
ncc target use office            # 切换（每个目标各自保存凭据，互不覆盖）
ncc target show                  # 当前目标的详情
ncc target add lab --base http://10.0.0.9:8282
ncc target rm lab
ncc hub                          # 「云端现在什么样」的快捷查看
```

三种临时指向（**不改默认目标**）：

| 写法 | 含义 |
|---|---|
| `ncc --target <名字> <命令>` | 本次命令跑在那个目标上 |
| `ncc hub <命令>` | 本次命令跑在云端（等价于 `--target hub`）|
| `ncc --base <URL>` | 本次用这个地址：已存在同名目标就复用，否则**新建一个目标**并切过去（会提示）|

### 能力：命令能不能跑，看目标声不声明

每个节点都在 `GET /api/meta` 里**声明自己的能力**（`registry` / `services` / `profile` /
`config` / `nodes` / `grants` / `share` / `access` / `cluster` / `admin` / `living` …）。
CLI 按这个清单放行：

```bash
ncc target use office && ncc services match "帮我订杭州的酒店"
#   ✗ 目标 office（ncc-registry · 内网节点）没有声明 `services` 能力
#     它声明的能力：registry · config · share · nodes · grants · access · cluster · admin
#     `services` 目前由云端（ncc.ai）提供。切过去：ncc target use hub
```

这是刻意的：**不是「云端专属」/「本地专属」的硬编码**，而是「这个节点声明了什么」。
本地 `ncc-registry` 将来说支持 `services` / `profile`，同一个命令在那台节点上就直接可用，
客户端不用改。老版本服务端没有 `/api/meta` 时按「未知 → 不限制」处理，不会把功能锁死。

## 命令一览

`<target>` 可以是注册中心 id（`R-…`），也可以是引用（`@命名空间/slug`）。

| 命令 | 说明 |
|---|---|
| `ncc target list` / `use` / `add` / `rm` / `show` | 目标管理：我连着哪些 ncc（云端 / 内网节点）|
| `ncc hub <命令>` | 把一条命令指到云端目标执行（一次性的）|
| `ncc register` | 注册账号；自动创建个人命名空间 |
| `ncc login` / `ncc logout` | 登录 / 登出 |
| `ncc me` | 显示当前用户、套餐与命名空间 |
| `ncc ns list` | 列出你拥有或加入的命名空间（组织会多一列**计划**）|
| `ncc ns plans` | 列出**组织计划**目录（含尚未开放的档：看得见，选不了）|
| `ncc ns create --slug <标识> --name <名称> [--plan free]` | 创建组织命名空间（计划缺省 `free`）|
| `ncc publish` | 通过上传文件或 BYO URL 发布制品 |
| `ncc search [query]` | 目录检索 |
| `ncc index publish <频道>` | 把一条服务 / 制品 / 需求登记进索引（先写平台，再推内网节点；节点失败会逐台报出来）|
| `ncc index list` / `show` / `rm` / `push` | 看我登记了什么 / 细节与已推节点 / 撤回（不动原件）/ 再推一次 |
| `ncc list users` / `needs` / `channels` | 索引里有哪些人 / 别人在找什么 / 有哪些检索空间 |
| `ncc match "…"` | 我有需求 → 谁能在（`--want need` 反过来找活儿；`--from <目标>` 用内网节点的索引）|
| `ncc info <target>` | 以 JSON 打印制品完整记录 |
| `ncc download <target>` | 下载制品字节 |
| `ncc install <target>` | 安装到本地包目录 |
| `ncc sign <文件>` | **签你发布出去的那份字节**（不限 kind：skill / mcp / …）。Minisign/ed25519，私钥不出设备；`--attach <引用>` 是唯一联网动作（只上传 `.minisig` 与公钥，制品字节一个字节都不传） |
| `ncc verify <文件 \| @命名空间/slug>` | 验签：本地文件全程离线；条目引用则下载字节 → 核摘要 → 验签。`--require-signature` 可进 CI；**被改过**的字节一律非 0 退出 |
| `ncc key list` / `create` / `revoke` / `scopes` | 管理能力令牌（类型 / 作用域 / 命名空间限定 / 过期） |
| `ncc living` | 把本机作为设备节点上报到你的命名空间 |
| `ncc p2p probe` / `p2p check <节点>` | 打洞条件预检（纯本地）/ 两端真实建连检查（互打 STUN，≈ ICE connectivity check） |
| `ncc p2p ticket create/list/verify/revoke` | P2P 票据：谁（哪个节点）能从你这里取哪条资源；连接 ≠ 授权 |
| `ncc registry p2p self` / `check --peer <ip:port>` | 在**目标节点那台机器**上出 NAT 画像 / 与对端映射真实对打（0 字节） |
| `ncc registry p2p serve [--on] [--peer <ip:port>]` | 开/关节点的**可被打洞入口**（只应答 STUN）；`--peer` 指定反向打洞对端 |
| `ncc profile [show <用户名>]` | 查看名片（默认自己，可看别人） |
| `ncc profile roles` | 列出工作角色目录 |
| `ncc profile set` | 设置名片字段（先读后写，只覆盖显式给出的字段） |
| `ncc profile username <名>` | 改用户名 |
| `ncc profile work list` / `add` / `rm` | 管理作品集 |
| `ncc nodes` / `kinds` / `discover` | 我的节点、类型目录、本实例上可连接的节点 |
| `ncc nodes link` / `label` / `unlink` | 连接节点并给 Name 标签 |
| `ncc nodes region` / `recommend` | 区域覆盖与推荐（Agent 面） |
| `ncc services` / `catalog` / `match` / `show` | 对外服务：目录 / 按意图匹配 / 单条接入信息 |
| `ncc services add` / `rm` | 声明 / 下架自己的对外服务（提供方） |
| `ncc grant list` / `set` / `rm` | 按人授权（`artifact` / `service` / `share`） |
| `ncc auth key new` / `ls` / `rm` | 绑定**持有证明凭据**（`CR-…`）：Ed25519 私钥落 `~/.ncc/cred/`（0600），只把公钥登记出去。与 `~/.harnessuse/keys` 的发布者签名密钥**故意分开**；绑定凭据 ≠ 有权用 |
| `ncc auth login --client <id>` | **设备码登录**（RFC 8628，同 `gh auth login`）：终端给出 `verification_uri` + `user_code`，浏览器里确认，CLI 轮询换令牌。令牌按目标单独存，只走数据面（账号面仍用 `ncc login`） |
| `ncc auth consents` / `revoke` / `status` | 我授给了哪些平台（scope + 绑定凭据）/ **按平台撤销**（立刻失效，且不影响别的平台） |
| `ncc --auth <命令>` | 以**对外令牌**跑这条命令（只走数据面）；令牌绑了凭据时会自动附持有证明 `NCC-Proof` |
| `ncc registry add` | 用一条内网短链（或 key/secret）把内网 registry 接进来 |
| `ncc registry login` / `join` | 登入自托管内网节点 / 把本机托管进去（注册 + 心跳） |
| `ncc registry status` / `nodes` | 本节点 + 集群（master/worker）/ 发现该实例上的节点 |
| `ncc registry catalog` / `route` | 聚合目录（本节点 + 各 worker）/ 这个能力该找哪个节点要 |
| `ncc registry ticket create` / `list` / `rm` | 签发 / 管理接入票据（key + secret + 内网短链） |
| `ncc registry config` / `kinds` / `get` / `set` | 配置托管：目录 / 取（默认打码）/ 写入（加版本） |
| `ncc registry config` `history` / `rollback` / `bundle` | 版本历史 / 回滚 / 按环境成组拉取 |
| `ncc registry share create` / `list` / `rm` / `info` | 分享：把一条制品变成**临时下载地址**（对方不用登录、不用装 CLI）|
| `ncc registry admin overview` / `users` / `nodes` / `services` / `audit` | 节点治理：用户 / 节点 / 服务 / 审计（需管理员账号或 admin key/secret）|
| `ncc registry admin disable` / `enable` / `passwd` / `rm-node` / `rm-service` | 禁用启用账号 / 重置密码 / 摘除节点 / 归档服务条目 |
| `ncc registry admin login` / `status` / `rotate` | 写入机器凭据 / 看自己是不是管理员 / 轮换 admin key+secret |
| `ncc registry replicate` | 把制品分发到 worker（副本） |
| `ncc registry rm` | 下架制品并回收各节点副本（`--yes`） |
| `ncc registry leave` | 下线我的节点（下次心跳会重新注册） |
| `ncc trace add --file <f.jsonl>` | 采集运行轨迹：原生 `ncc-trace/v1` 文档 / HUR 执行留痕 / 事件流（每行 `{type,name,ms,in,out}`）。**只写本机**，不联网 |
| `ncc trace ls` / `show` / `stats` / `export` / `label` / `rm` | 本地看 / 聚合 / 导出（JSONL 数据集）/ 打评测标注 / 删；加 `--remote` 则对这些动作走目标节点 |
| `ncc trace push` | 把采集到的轨迹上传到节点（幂等，每批 50 条）。要求目标声明 `trace` 能力 |
| `ncc trace kinds` / `status` | 词表与上限 / 本地暂存状态（采集了多少、传了多少） |
| `ncc kb set <slug> --title … [--file f]` | 写一篇**托管知识库**文档（存在即新版本）；`--public` 让它匿名可读 |
| `ncc kb ls` / `get` / `search` / `history` / `bundle` | 列表（公开 ∪ 我的 ∪ 被授权的）/ 取一篇（`--revision N` 取历史版）/ **关键词加权**检索 / 版本历史 / 成组拉一个命名空间 |
| `ncc kb archive` / `restore` / `rm` | 归档（默认不出现在列表与检索里）/ 恢复 / 删除 |
| `ncc kb pull --package <目录>` | **读包的 `state.kb` 声明并拉那几个库**到 `~/.ncc/kb`（按 `checksum` 增量）；包没声明就明确拒绝，不替它猜 |
| `ncc kb kinds` | 知识库类型 / 格式 / 上限（离线也能看） |
| `ncc mem set <key> <value>` | 写**记忆**（`(命名空间, subject, key)` upsert；`--ttl-days`、`--kind`、`--source`、`--confidence`、`--pin`） |
| `ncc mem get <key>` / `ls` / `rm` / `gc` | 按 key 读一条（Agent 读记忆的主路径）/ 列表（默认不含过期的）/ 删一条 / 真正清掉已过期的 |
| `ncc mem kinds` | 记忆种类与上限（记忆**没有公开档**） |
| `ncc ckpt save --name … --file …` | 打一个**检查点**：算 `sha256` → 建元数据 → 传字节（`--parent-last` 自动接血缘，`--meta k=v` 加自由元数据） |
| `ncc ckpt ls` / `show` / `lineage` | 列表 / 看一个（含短时签名 `bytesUrl`）/ 沿 `parent` 回溯到起点 |
| `ncc ckpt pull <id> --out <文件>` | 取回字节，**落盘前核对摘要** |
| `ncc ckpt prune --ref <@ns/slug> --keep N` / `rm` | 每个制品只留最新 N 个（其余标 `pruned`、删字节、留元数据）/ 彻底删一个 |
| `ncc ckpt kinds` | 检查点粒度与上限（不可变：没有"改"这个动作） |
| `ncc store declare <集合>` | **声明一个集合 —— 这就是新增一类内容的全部代价**（问题单 / 运行日志 / 复盘 / 备注…）。服务端不用改：`--field 'title:string!'` / `'status:enum:open\|closed'` / `'labels:string[]'` / `'body:text?search'` / `'owner:ref'`，`--index` 指定能过滤的字段，`--immutable` / `--append-only`、`--public`、`--max-bytes`、`--ttl-days` |
| `ncc store declare --file <文件>` / `--dir <目录>` | 声明是**配置**：从文件 apply（单件 / 数组 / `{"stores":[…]}` / 整个 `hur.json` 都收），或从一个目录逐件 apply。**声明是完整的一份说法，不是补丁** —— 文件里没写的项就是没有 |
| `ncc store declare … --check` | 只看**会改什么**（哪几项变了）而不改；有差异退出码 1，可以直接当 CI 门禁 |
| `ncc store export --dir <目录>` / `--file <文件>` | 把声明导出来（一件一个文件 + README），好进 git、好评审、再 apply。导的是**声明不是记录** —— 它不是备份 |
| `ncc store ls` | 这台节点上有哪些集合（记录数 / 形态 / 可见性 / 声明的字段） |
| `ncc store put <集合> <key>` | 写一条：`--body` / `--file` / stdin、`--field k=v`（**本地就按声明类型化与校验**）、`--meta k=v`（自由 JSON，**不过滤**）、`--tag`、`--ttl-days`、`--revision N`（乐观并发，对不上 409，不是静默覆盖）、`--note`（进历史） |
| `ncc store list <集合>` | 查：`--where 字段=值`（必须是声明过**且在 index 里**的字段，否则报错并列出能过滤的哪些，**不会给你个空结果**）、`--q`（**关键词匹配**：几个词都要出现；命中位置加权排序 key 3 / 标签与 `?search` 字段 2 / 正文 1 —— 不是索引检索，排序在最多 500 条候选上做）、`--tag`、`--prefix`、`--archived`、`--expired`、分页 |
| `ncc store get <集合> <key>` | 取一条（含正文） |
| `ncc store history <集合> <key>` | 改动历史 —— 只记元数据（谁 / 何时 / 哪个摘要 / 备注）。备注**跟着它自己的版本走**，所以建记录时写的那条备注永远看得到 |
| `ncc store rm <集合> <key>` | 归档（不列出、但还在）；`--hard` 才是真删 |
| `ncc store kinds` | 类型白名单 / 上限 / 三条不变量 —— **离线可读** |
| `ncc terminal [status\|setup]` | 打开能力命令台 / 查看 POSIX 运行时 |
| `ncc upgrade` | 把 CLI 二进制就地升级到最新发布版（`--check` 只检查不下载，`--force` 强制重装）|
| `ncc mcp` | 以 **MCP server**（stdio）启动，让任意 Agent 驱动 NCC。`--package <目录\|hur.json>` 把面收窄到这个包声明的集合（**模型面 = 声明面**：读工具按 `mode` 给、写工具要有声明才给，没声明的集合连名字都看不到，也调不动） |
| `ncc gateway init` / `check` / `run` / `status` / `audit` | **NCC Gateway（S2a）**：固定路由的白名单代理 —— 提供出口（`accept`）或借对端出口（`forward`）。调用方**不能指定目标地址**；出站凭据只来自配置 `inject`（调用方的 `Authorization` 不透传）；本地 JSONL 审计只记元数据 |
| `ncc gateway bind` / `heartbeat` | 接**控制面**（= ncc.ai）：注册网关 → 令牌写进 `~/.ncc/gateway.json`（0600，只回一次）/ 周期心跳（缺省语义 `online`，可自报 `draining`）|
| `ncc gateway report` / `usage` | 把本地审计聚合成**窗口摘要**签名上报（**先落盘再发送**：控制面不可达就进待传队列，恢复后补传）/ 用量汇总（写明是**自报计数**）|
| `ncc gateway audit --remote` / `unbind` | 看/导出控制面留存的摘要（`--csv`＝合规导出）/ 注销并清本地绑定（控制面不可达也能解绑）|
| `ncc app init` / `doctor` / `up` / `status` / `export` | **NCC 舱（`ncc app`）**：把「用户自己部署一个人助理」变成一条命令 —— 产品本体是你的（`app.json`）、引擎是 ncc、应用逻辑是 HUR 包（`hur.json`）、内容住节点、互联走平台；`up` 起本机控制台（loopback），`export` 出可交付目录 |
| `ncc help <command>` | 查看任意命令的自动生成帮助 |
| `ncc hur profile <包 \| @命名空间/slug>` | 读一份包**是什么**：要什么 / 给什么 / **怎么接**，外加**分级体检**（结构 · 自洽 · 签名分开报，不合成一个 ✅）；`--list` 列规范里的全部 profile |
| `ncc hur match --profile kb-seed` | 按 profile / 集成宿主 / 能力在目录里找包（**只读**） |
| `ncc hur data import --package <目录>` | 把**数据快照包**灌进节点（kb-seed / mem-seed / ckpt-set / trace-set）；默认只出计划，`--apply` 才真写 |
| `ncc kb bundle --as-package <目录>` | 把知识库导出成**快照包**（来源 / 快照时刻 / 隐私级别 / 许可），可签名可发布 |
| `ncc mem export --as-package <目录>` | 记忆快照（**默认 private** —— 能分发出去的记忆就不再是记忆了） |
| `ncc ckpt export --as-package <目录>` | 检查点集合：字节 + 血缘，进包前逐个核摘要 |
| `ncc trace export --as-package <目录>` | 轨迹数据集快照；`--payload digest\|preview\|full` 必须声明（`full` + `public` 直接拒） |

全局参数：

| 参数 | 说明 |
|---|---|
| `--base <URL>` | 本次命令用这个地址：已存在同名目标就复用，否则新建一个目标并切过去 |
| `--target <名字>` | 本次命令用这个目标（默认目标不变；改默认：`ncc target use <名字>`）|
| `-h, --help` / `-V, --version` | 帮助 / 版本 |

### `ncc publish`

| 参数 | 说明 |
|---|---|
| `--kind <KIND>` | **必填。** `api`、`harness`、`hur`、`skill`、`mcp`、`plugin`、`scaffold`、`docker-image`、`benchmark`、`living` |
| `--name <NAME>` | **必填。** 展示名称 |
| `--file <PATH>` | 上传本地文件字节 |
| `--url <URL>` | 自带存储：发布直链而不上传 |
| `--slug <SLUG>` | URL 安全 slug；省略时由注册中心依据 `--name` 生成 |
| `--version <VER>` | 默认 `1.0.0` |
| `--summary <TEXT>` | 一句话说明 |
| `--tags <a,b,c>` | 逗号分隔标签 |
| `--manifest <PATH>` | `--kind harness` 的封装契约 JSON（含 `harness.loader` / `harness.entry`） |
| `--namespace <SLUG>` | 目标命名空间，必须是你的所属命名空间。默认个人命名空间 |
| `--visibility <public\|private>` | 默认 `public`。`private` 需要付费套餐 |
| `--draft` | 以 `draft` 状态创建（默认 `published`） |

`--file` 与 `--url` 必须且只能提供一个。

### `ncc search`

| 参数 | 说明 |
|---|---|
| `[query]` | 关键词（可选位置参数） |
| `--kind <KIND>` | 按制品 kind 过滤 |
| `--tag <TAG>` | 按标签过滤 |
| `--namespace <SLUG>` | 限定某个命名空间 |
| `--mine` | 只看自己的制品（需要登录态） |

### `ncc install` 与 `ncc download`

| 参数 | 说明 |
|---|---|
| `-d, --dir <DIR>` | 安装根目录。默认 `~/.ncc/packages`（或 `NCC_PACKAGES_DIR`） |
| `--force` | 已安装时覆盖 |
| `-o, --out <PATH>` | `download` 的输出路径 |

`ncc install` 按 `<root>/<命名空间>/<slug>/` 组织目录，并在制品文件旁写入 `package.json`，记录来源引用、kind、版本、`sha256`、大小、安装时间，以及制品声明了封装契约时的 `manifest` / `harness` 块。

### `ncc sign` 与 `ncc verify`（给任意制品加签）

签名只有在「核对方不必信你」时才有意义。所以 `ncc sign` 签的就是**你发布出去的那份字节**，
拿到公钥的人用现成工具就能独立核对 —— 不需要 NCC，也不需要 `ncc-cli`：

```sh
ncc hur key gen                                  # 一次性：密钥在 ~/.harnessuse/keys
ncc sign SKILL.md --kind skill --reference @me/release-notes --version 0.1.0
#   ⇢ 写出 SKILL.md.minisig（离线；私钥不出设备）

ncc verify SKILL.md                              # ✔ 已验证
ncc verify SKILL.md --require-signature          # CI 门禁（没有可核对签名就非 0）
minisign -V -p ~/.harnessuse/keys/hur.pub -m SKILL.md   # 第三方的核对方式
```

签名对象里有 `format` / `keynum` / `signer` / `sha256`（制品摘要）、`.minisig` 正文与
公钥线索。把它放进发布清单，服务端会再核一次「摘要 == 刚上传的那份字节」：

```sh
ncc sign SKILL.md --kind skill --reference @me/release-notes --version 0.1.0 --json \
  | python3 -c 'import json,sys; json.dump({"signature": json.load(sys.stdin)["signature"]}, open("manifest.json","w"))'
ncc publish --file SKILL.md --kind skill --name release-notes --manifest manifest.json
```

事后加签是 `--attach`（唯一联网路径 —— 只上传 `.minisig` 与公钥，制品字节不传）：

```sh
ncc sign SKILL.md --kind skill --attach @me/release-notes
```

三件**故意不做**的事：

* **包不是一份文件。** 把 `ncc sign` 指到 HUR 包目录（或 `.hur` / `.hur.gz` 产物）它会转交 `ncc hur sign`——
  那里签的是**规范打包字节**，要连包身份、版本与 `hur.lock` 一起核对。两种形态各一套实现。
* **公钥不认识就不是"已验证"。** `ncc verify` 会如实报 `⚠️ 有签名，但公钥本机不认识`，
  并且带 `--require-signature` 时非 0 退出。
* **随签名一起给的公钥不是信任依据。** 签名旁边那个 `pubkey` 是发布方自己的声明；
  `ncc verify` 最多告诉你它**自洽**，仅此而已 —— 要认它，就用
  `--pubkey <你自己确认过的公钥>` 或 `ncc hur key trust`。

### `ncc living`

| 参数 | 说明 |
|---|---|
| `--daemon` | 按间隔持续心跳，而非只上报一次 |
| `--interval <SEC>` | 守护间隔，默认 `15` |
| `--name <NAME>` | 设备名。默认 `$HOSTNAME`，再退化到 `<os>-<arch>` |
| `--slug <SLUG>` | 设备 slug；省略时由 name 生成 |
| `--url <URL>` | 他人可直连本设备的地址 |
| `--capabilities <a,b>` | 本设备可提供的 kind，逗号分隔 |

`os`、`arch` 与 CLI 版本会自动附带。对外只发布状态与能力可见性 —— NCC 不会代传任何数据。

### `ncc profile`

你的公开名片：定位角色 + 作品集 + 已发布能力。它挂在注册中心的**顶层路径**上，所以用户名就是你的地址：

```
ncc.ai/aya          → 你的名片     ncc profile
ncc.ai/ns/@aya      → 你的能力条目  ncc profile roles
ncc install @aya/x  → 你的能力条目
```

| 命令 | 说明 |
|---|---|
| `ncc profile [show [<用户名>]]` | 打印名片（缺省是自己的） |
| `ncc profile roles [--group <id>]` | 列出 6 组 20 个角色 —— 即 `--roles` 的取值 |
| `ncc profile set` | 更新字段（见下） |
| `ncc profile username <名>` | 改用户名 |
| `ncc profile work list` | 列出作品及 id |
| `ncc profile work add --title …` | 添加作品 |
| `ncc profile work rm <id>` | 删除作品 |

`ncc profile set` 参数：

| 参数 | 说明 |
|---|---|
| `--username <NAME>` | 用户名：3–30 位小写字母/数字/连字符；保留字会被拒绝 |
| `--name <TEXT>` | 显示名 |
| `--headline <TEXT>` | 一句话定位 |
| `--bio <TEXT>` | 个人简介 |
| `--location <TEXT>` | 所在地 |
| `--roles <a,b>` | 定位角色；最多 5 个，首个为主角色 |
| `--skills <a,b>` | 自由技能标签；最多 12 个 |
| `--availability <open\|collab\|hiring\|busy>` | 接洽状态 |
| `--visibility <public\|unlisted>` | `public` 进人才目录；`unlisted` 仅直链可见 |
| `--email <ADDR>` | 联系邮箱（公开展示） |
| `--link <key=value>` | 外链，可重复 —— 如 `--link github=https://github.com/you`；传 `key=` 则清除 |

`ncc profile work add` 支持 `--title`、`--summary`、`--role`、`--tags`、`--year`，以及三选一的 `--url`（外链）/ `--share <S-…>`（NCC Share 页）/ `--item <R-…>`（Registry 条目）—— 作品可以直接指回你在 NCC 上发布的成果。

> **`set` 不会抹掉你没提到的字段。** 接口的 `PUT` 是整体替换，所以 CLI 先读现状、只覆盖你显式传入的字段。改用户名会连带改动个人命名空间（`@旧` → `@新`），使写成 `@旧/…` 的引用失效 —— 发生时会给出警告。

### `ncc nodes` / `ncc grant`

NCC **不做通讯录** —— 存在的意义是让不同的 Agent 节点能连起来。两层互相独立，
混淆这两者是经典错误：

- **连接** 代表「找得到」；
- **授权** 代表「拿得到」。

```
# 注册节点 —— 声明即注册（心跳自动续租）
ncc living --name my-agent --kind agent --capabilities mcp,api
ncc living --name delivery-svc --kind service --url https://svc.internal
ncc living --name client-a-bot --kind assigned --slug client-a

ncc nodes kinds                   # 类型目录
ncc nodes                         # 我的节点 + 我连接的节点
ncc nodes discover                # 本 NCC 实例上可连接的节点
ncc nodes link @aya/my-agent --label "交付助手" --note "接客户需求做初稿"
ncc nodes label NL-xxxx --label "交付助手 v2"
ncc nodes unlink NL-xxxx

ncc grant set --user @某人 --kind artifact --ns @you   # 可下载我的私有制品
ncc grant set --user @某人 --kind share                # 可看我的私有分享页
ncc grant set --user @某人 --kind service              # 可接入我的非公开服务
ncc grant list --in                                    # 别人给我的授权
```

| 概念 | 回答的问题 | 是否放行数据 |
|---|---|---|
| 节点 Node | 这个 Agent/服务是什么、在哪、能不能连 | ❌ 只是身份与地址 |
| 连接 Link | 我的 Agent 该连谁（含我给它起的 Name 标签） | ❌ 只代表「找得到」 |
| 授权 Grant | 谁能下载我的私有制品 / 看我的私有分享页 | ✅ 按类型放行 |
| API-Key | 某个程序以什么身份、能做什么 | ✅ 按作用域与命名空间放行 |

**节点类型**：节点在注册时声明自己是什么 —— `service`（API / MCP server / 网关 / 数据源）、
`agent`（为人服务的 Agent）、`assigned`（被指派给任务/团队/客户的 Agent）。
声明属于节点本身，不是连接方说了算。

**连接不需要对方审批**：同一个 NCC 实例就是同一个信任域，其上的公开节点彼此可连接。
连接是**你自己这边的条目** —— 给它一个 **Name 标签**和用途备注，好让 Agent 知道该连谁、
连过去干什么。别人的私有节点不会出现在 discover 里（那属于授权范畴）。断开只删你自己的条目。

**区域覆盖与推荐是 Agent 面能力**（`ncc nodes region` / `ncc nodes recommend`，
MCP 对应 `ncc_region_profile` / `ncc_recommend_nodes`）。节点的区域来自**归属者名片的所在地**；
`recommend` 的排序在服务端完成（按「你已在该区域有几个节点」降序），
CLI / MCP / 前端共用同一顺序。两者都不在网页上展示。

### `ncc services`

服务提供方（公司 / 连锁集团）可以把**多条业务分别声明成一条对外服务**，对其他 Agent 开放。
其他 Agent 按**匹配策略**找到「该找谁、怎么接、要不要授权」。

```bash
ncc services catalog                      # 业务分类（6 组 28 类）/ 接入方式 / 授权方式目录
ncc services                              # 浏览公开服务目录（或 --mine 看自己的）
ncc services match "帮我订杭州的酒店"      # 按意图匹配（服务端打分，带回理由与接入步骤）
ncc services show @aya/hotel-booking      # 一条服务的完整接入信息
ncc services match --json "…"             # 原始 JSON（score / reasons / howToUse）

# 提供方侧：声明与下架
ncc services add --name "酒店预订中台" --category booking --slug hotel-booking \
  --summary "华东区门店房态与预订能力" --tag 预订 --intent 订酒店 \
  --match "订华东区酒店时优先找我" --region "杭州 · 上海" \
  --protocol openapi --endpoint https://api.example.com/openapi/hotel.json \
  --access open --publish
ncc services rm SV-xxxx
```

| 概念 | 回答的问题 | 放行数据 |
|---|---|---|
| 服务 Service | 这门业务找谁、怎么接、要不要授权 | ❌ 声明；接入细节看授权 |
| 节点 Node | 它在哪跑（服务可绑定一个执行节点） | ❌ 只是地址 |
| 授权 Grant | 谁能看到非公开服务的接入细节 | ✅ `--kind service` |

要点：

- 声明默认 `draft`（仅自己可见），`--publish` 或改状态才对外开放；每条服务上限 30 条。
- 接入方式 `--protocol`：`http` / `openapi` / `mcp` / `artifact`（先 `ncc install` 能力包）/ `human`。
- 授权方式 `--access`：`open` 公开可调 / `grant` 需授权 / `invite` 定向（不进目录）。
- **非公开服务只露摘要**：未获授权时匹配结果只有名称、分类、区域与匹配策略，
  端点 / 执行节点 / 能力包与调用步骤要拿到 `--kind service` 授权才展开。
- **区域不限**写 `全国` 或留空：它会在任何区域查询里都被算作覆盖。

### `ncc index` / `ncc list` / `ncc match`

服务声明回答「我能提供什么」；**索引**回答「别人怎么搜到我」——把一份东西登记进一个**频道**
（自由名，如 `booking/hotel`），别人用一句需求就能检索到。同一条服务可以登记到不同频道、
换不同话术。

```bash
# 登记：先写平台（权威），再尽力推已接入的内网节点
ncc index publish booking/hotel --service @aya/hotel-booking
ncc index publish pptx/deck --item @aya/html-deck-to-pptx --intent 把 HTML 变 PPTX
ncc index publish booking/hotel --need "国庆要两间杭州的大床房"   # 需求也进索引

ncc index list --mine              # 我登记了什么
ncc index list --channel booking   # 这个频道里有什么（前缀匹配：booking → booking/hotel）
ncc index show @aya/aya-hotel      # 细节：怎么接过去 + 已推节点
ncc index push @aya/aya-hotel      # 再推一次到内网节点
ncc index rm @aya/aya-hotel        # 撤回索引（**不动原件**）

ncc list users                     # 索引里有哪些人（供给 / 需求 / 频道）
ncc list needs                     # 别人在找什么（找活儿用）
ncc list channels                  # 有哪些检索空间、各多少条

ncc match "帮我订杭州的酒店" --channel booking   # 我有需求 → 谁能在
ncc match "杭州酒店" --want need                 # 我在找活儿 → 谁要人
ncc match "…" --from office                      # 用内网节点上的索引匹配（默认当前目标）
```

要点：

- **三类 kind**：`--service`（索引我的一条对外服务，分类/关键词/摘要从它继承）、
  `--item`（索引一个制品）、`--need`（登记需求）。
- **频道是自由名但不是无规则**：`Booking/Hotel` = `booking/hotel`；`Food____RES` → `food-res`。
- **索引 ≠ 授权**：登记只代表检索得到 —— 私有制品要 `ncc grant`，非公开服务要 `service` 授权。
  引用非 `open` 服务时索引里**不带端点**（入口指向 `ncc services show`）。
- **平台权威、节点副本**：`publish` 先写平台，再推已接入的内网节点；
  **节点推失败不回滚平台，但会逐台报出来**（`✓/✗`），推成功的节点在 `index show` 里看得到。
- **评分不对外显示**：匹配排序含内部信誉权重，但接口与 CLI 都不会给出分数 ——
  接入照旧要授权。

### `ncc registry`

面向自托管内网节点 [`ncc-registry`](ncc-registry/) 的命令组。它只在 **kind=registry 的目标**
上跑（云端命令直接写成 `ncc hub …`，或先 `ncc target use hub`；在错误的目标上会直接告诉你该切到哪个）：
它把「制品托管 + 节点托管 + 多节点集群」当成一个内网服务来用：

```bash
# 登入某个内网节点（复用同一套账号体系）
ncc --base http://office-master:8282 registry login --email you@corp.com --password '***'

# 把这台机器作为一个节点托管进去（注册 + 心跳，--daemon 常驻）
ncc registry join --kind agent --name my-mac --region 上海-内网 --capabilities mcp,api
ncc registry join --kind service --name billing-svc --url http://10.0.0.9:9000

ncc registry status                # 本节点身份 + 集群（master/worker）+ 我的托管节点
ncc registry nodes --kind agent    # 在本实例上发现可连接的节点
ncc registry catalog               # 聚合目录（本节点 + 各 worker，条目带 via）
ncc registry route @alice/hotel-skill   # 这个能力该找哪个节点要
ncc registry leave --name my-mac   # 下线我的节点（下次心跳会重新注册）
```

要点：

- **注册与心跳是同一件事**：第一次 `join` 即注册，之后每次上报续租在线状态；
  在线与否由服务端 `NCCR_NODE_TTL` 判定。
- **master 是唯一入口**：worker 上的制品也能装 —— `ncc install` 拿到的地址落在 master 上，
  master 会把字节从持有它的 worker 代理回来（`sha256` 校验不变）。
- **`route` 是能力路由**：先看 master 本地有没有，再看哪个 worker 有，返回候选与统一入口。
- **连接 ≠ 授权**：`ncc nodes link` 只解决「找得到」，取私有制品仍需授权。

### `ncc registry config`（配置托管）

内网 registry 除了放制品，还能**托管团队的网络 / 基础设施 / Agent 配置**：默认私有、
每次写入留一版历史、可回滚、敏感值静态加密；Agent 拿到限定作用域的凭据就能自己读写。

```bash
ncc registry config kinds                      # 类型（network/gateway/infra/agent/ci/security…）+ 格式 + 环境
ncc registry config set @team/network --file ./network.yaml --kind network --env prod \
    --summary "内网网段/DNS/VLAN" --tags network,dns --note "初始版本"
ncc registry config list --mine                # 我的配置（含私有；内容默认打码）
ncc registry config get @team/network --reveal --out ./network.yaml    # 取明文落盘
ncc registry config history @team/network      # 谁在何时改了什么
ncc registry config rollback @team/network --to 2                      # 回滚（作为新版本写回）
ncc registry config bundle --ns @team --env prod --out ./conf          # 整套拉取（含 any 通用项）
ncc registry config rm @team/network --yes
```

| 动作 | 需要什么 |
|---|---|
| 读公开配置 | `visibility=public` 且 `status=active` → 谁都能读 |
| 读非公开配置 | 作用域 `config:read` **且**（命名空间成员 **或** `ncc grant set --kind config` 授权） |
| 写入 / 回滚 / 删除 | 作用域 `config:write` **且** 命名空间成员（外部只给读） |

给 Agent 的长效凭据就是一张限定作用域的票据（令牌的 `Sub` 是签发者本人，
所以它是「代表你在团队空间里管配置」）：
```bash
ncc registry ticket create --label agent-conf --scopes config:read,config:write,nodes:write
```

要点：

- **与制品的区别**：制品是分发的文件（公开、可 fan-out 到 worker）；配置是团队的权威数据
  （默认私有、就在被指向的那个节点上维护、不参与 fan-out）。
- **内容默认打码**：不加 `--reveal` 只回 `sha256` 与大小 —— 打码是默认，不是异常。
- **敏感值**：`--secret` 的配置落库前 AES-256-GCM 加密（密钥由 `jwt-secret` 派生）；
  备份库但不带 `jwt-secret` 是安全的，换机器就解不开。
- **bundle 默认跳过 `secret` 配置**：一次把凭据全下到磁盘不是好默认，要用就 `--secrets --reveal`。

### `ncc registry share`（分享链接）

把一条制品变成**临时下载地址**发出去：对方不用登录、不用装 CLI。

```bash
ncc registry share create @team/report --label "给合作方" --uses 1 --expires 7
#  → 说明页  http://<节点>/s/<token>        浏览器打开是说明页 + 下载按钮
#    直链    http://<节点>/s/<token>/raw    curl -OJ 就能拿到字节（只有它计数）
ncc registry share list                        # 我发的（--all 需管理员）
ncc registry share info "<链接>"                # 看一条链接的状态（公开，不消耗次数）
ncc registry share rm <SH-…|链接>               # 撤销，立即失效
```

- **分享 ≠ 授权**：分享是按链接的临时放行（可限次 / 限时 / 撤销，拿到字节即结束）；
  要给某个人长期权限用 `ncc grant`。
- **创建分享不是提权**：只有本来就能读这条制品的人能分享它。
- token 只存 sha256，只在创建时返回一次；撤销 / 过期 / 用尽即失效（`410`）。

### `ncc registry admin`（节点治理）

管一台内网 registry 上的**用户 / 节点 / 服务**，每个动作都进审计。

两种身份等价：**本节点第一个注册的账号**（自动成为管理员，直接用会话即可），
或一把**机器凭据** `AK-…` + secret（管理员首次出现时自动签发，之后可轮换）：

```bash
# 注册本节点第一个账号时会打印一次 admin 凭据（secret 只显示一次）
ncc --base http://<节点>:8282 register --email you@corp.com --password '***'
ncc registry admin login --key AK-XXXXXX --secret ****   # 写进 ~/.ncc/config.json（0600）
ncc registry admin status                                # 我是不是管理员 / 本机凭据能不能用
ncc registry admin rotate --label ops                    # 轮换：新 secret 生效、旧的立即失效

ncc registry admin overview                              # 用户 / 节点 / 服务 / 资产 / 审计 计数
ncc registry admin users --q bob                         # 谁在这台节点注册过（含被禁用的）
ncc registry admin disable bob@corp.com --note "违规发布"  # 禁用：旧令牌立即失效
ncc registry admin enable  bob@corp.com
ncc registry admin passwd  bob@corp.com                  # 重置密码（服务端生成，只显示一次）
ncc registry admin nodes --kind service                  # 全部托管节点（含私有与离线）
ncc registry admin rm-node ND-…                           # 摘除节点（连接记录一并清理）
ncc registry admin services                              # 节点侧 kind=service + 制品侧 kind=api
ncc registry admin rm-service ND-…  |  @team/hotel-api    # 节点摘除 / 制品归档（字节保留）
ncc registry admin audit --limit 20                      # 谁在什么时候把谁怎么了
```

两条服务端强制的规则：**不能禁用自己的账号**，**不能禁用最后一个可用管理员**。

### `ncc hur profile` 与数据快照包

一份包“**是什么**”，过去由一个只有三个值的 `kind`（`agent|harness|repo`）兼职。现在叫 **profile**，
共 11 个：`agent` `harness` `plugin` `mcp` `app` `scaffold` `skill` `kb-seed` `mem-seed`
`ckpt-set` `trace-set`。**封装不变**（确定性字节 + `hur.json` + `hur.lock` + 签名），profile 只决定
**要什么、能不能跑、怎么接、按什么匹配**。

```sh
ncc hur profile --list                 # 规范里的全部 profile
ncc hur profile ./my-plugin            # 是什么 / 要什么 / 给什么 / 怎么接 + 分级体检
ncc hur match --profile plugin --host cursor
```

**profile 的价值在约束，不在宽容**：数据类 profile 明确禁止 `entry` 与 `permissions.network` ——
一份知识库快照不该能跑代码、也不该自己出网（新规则 R12，老包不受影响）。

产物文件名也带上这一段：`dist/…-0.1.0.kb-seed.hur.gz`、`…-0.1.0.plugin.hur.gz` —— 一个 `dist/` 里
躺着几十个产物时，一眼看得出哪个是能跑的包、哪个是一份数据。**名字只是线索，清单才是权威**：
改名不会改变它是什么（只多一条提醒），认不出来的名字也不会被当成写错。

**`hur` 只是一种规范，文件本身用通用压缩包格式结尾**：`.hur` 回答“这是 HUR 产物”，末尾的 `.gz`
回答“外面这层是什么容器” —— `file x.hur.gz` 报 gzip，`gunzip -c x.hur.gz > x.zip` 出来的仍是
标准 zip（包内清单谁都能看）。外层负责压、内层只管结构与防穿越解包，**同样内容仍得同样 sha256**；
升级前打的老包（`.hur`，裸 zip）继续能核、能装。后缀由**容器格式**决定（只改一个常量就能换）。

数据类的四样（`kb` / `mem` / `ckpt` / `trace`）都能导出成**不可变快照包**，再灌回任意节点：

```sh
ncc kb bundle --namespace @me --as-package ./kbseed --privacy internal --license CC-BY-4.0
ncc hur verify ./kbseed && ncc hur sign ./kbseed && ncc hur publish ./kbseed
ncc hur data import --package ./kbseed            # 默认只出计划，一个字节都不写
ncc hur data import --package ./kbseed --apply    # 真写
```

四条边界：**活状态不出门**（kb/mem/ckpt 会被反复写、持续变大、默认私有 —— 能打包的是它们的快照）；
**快照必须说清**来源 / 时刻 / 隐私 / 许可；**隐私级别说了算**（`privacy != public` ⇒ 导入后全部 `private`）；
**载荷如实声明**（`trace-set` 的 `full` + `public` 直接拒）。设计见 `ncc-platform/prd/ncc-hur-spec.md`。

想拿一个**真包**照着改：`examples/html-deck-to-pptx`（`profile=skill`，带 python + node 脚本的
技能包）—— 怎么摆目录、怎么接进 Claude Code / Codex / Cursor、怎么签名发布都在那儿。
`scripts/examples-smoke.sh` 守着"示例必须真的能过 `ncc hur verify`"这条。

### `ncc key`

API-Key 是**能力令牌**，有两个独立约束：`scopes`（能做什么）与 `namespaces`（能拉谁的东西）。

```bash
# 发给客户或 Agent 的只读凭据
ncc key create --label "客户A-Agent" --kind distribution --ns @you --expires 30

ncc key list      # 类型 / 作用域 / 命名空间 / 过期 / 最近使用
ncc key scopes    # 全部作用域词表
ncc key revoke <id>
```

| 选项 | 说明 |
|---|---|
| `--label <TEXT>` | 备注名 |
| `--kind <user\|distribution>` | `user` 代表你自己；`distribution` 只读，用于对外分发 |
| `--scopes <a,b,…>` | 显式作用域；不传则取该类型的默认集 |
| `--ns <@slug>` | 限定命名空间，可重复；不传 = 不限 |
| `--note <TEXT>` | 用途备忘（给谁用、做什么） |
| `--expires <DAYS>` | 有效天数；不传 = 长期有效 |

作用域词表：`registry:read`、`registry:download`、`registry:publish`、`profile:read`、
`profile:write`、`contacts:read`、`contacts:write`、`grants:read`、`grants:write`、`living:write`、
`keys:write`；并带蕴含关系（避免旧令牌突然失效）：`registry:publish ⇒ registry:download ⇒ registry:read`、
`contacts:write ⇒ contacts:read`、`grants:write ⇒ grants:read`、`profile:write ⇒ profile:read`。

两个可以依赖的性质：

- **不能自我提权** —— `keys:write` 从不发给 key，所以泄露的令牌签不出更多令牌
  （用 key 调 `POST /api/auth/keys` 返回 403）。
- **过期即失效** —— `--expires 30` 后，到期时刻起立刻不可用。

把令牌交给 Agent：写进环境变量 `NCC_TOKEN`，或写进 `~/.ncc/config.json`。
一个只有 `registry:read` + `registry:download` 的分发 key 能在限定空间内检索与下载，
其它一律 403 —— 包括发布。

### `ncc terminal`

`ncc terminal` 打开能力命令台（官方包 `@ncc/terminal`）。在真实 TTY 下渲染全屏 TUI，支持 Tab 补全、历史（`↑`/`↓`）、输出区，`Ctrl+C` 退出；stdin 非 TTY（管道、CI）时自动降级为逐行 REPL。

命令台内可用：

| 输入 | 效果 |
|---|---|
| `help` | 内置帮助 |
| `runtime status` / `runtime setup` | POSIX 运行时状态 / 装配 |
| `ncc <cmd…>` | 调用预置 `ncc` 子命令（`publish`、`search`、`install`、`living` …） |
| `! <cmd>` 或其它任意行 | 交给系统 POSIX shell 执行 |
| `exit` / `quit` | 退出 |

`ncc terminal status` 不进入命令台，直接打印解析出的 base URL、操作系统与 POSIX 运行时。Unix 上运行时即原生；Windows 上会检测 WSL2，缺失时降级 MSYS2，并通过 `ncc terminal setup` 引导装配。

### `ncc gateway`（接控制面：摘要上报）

网关（S2a/S2b）默认只把审计落在**本机**。接上控制面（= ncc.ai）之后，它会把本地审计
**聚合成窗口摘要**（分钟/小时级）签名上报 —— **路径、载荷、凭据都不出本机**，控制面只看到
计数、字节数、**主机名**、状态桶、延迟分位。这条红线在服务端还有一道硬拦：
摘要里"域名"带 `/` 或空白 → 400。

```bash
ncc gateway init --accept llm            # 先有一份网关配置（S2a）
ncc gateway bind --namespace @你的组织    # 注册到控制面：令牌只回一次，写进 ~/.ncc/gateway.json（0600）
ncc gateway heartbeat                    # 心跳（缺省语义 online；--status draining 可自报）
ncc gateway run                          # 常驻：按 heartbeat_sec / report_sec 周期干活
ncc gateway report --dry-run             # 看会聚合出哪些窗口（不发送）
ncc gateway report                       # 真上报（先落盘再发送）
ncc gateway audit --remote --csv         # 看/导出控制面留存的摘要
ncc gateway usage                        # 用量汇总（写明是"自报计数"）
ncc gateway unbind                       # 注销 + 清本地绑定（本地审计文件不动）
```

几条**要记住的语义**：

| 语义 | 说明 |
|---|---|
| **断线不丢审计** | 本地账本 `~/.ncc/gateway-report.json` 记水位 + 待传队列；**先落盘再发送**。控制面不可达、令牌被吊销、机器重启，审计都还在，恢复后 `report` 自动补传（服务端按摘要 digest 判重复，**不双计**）|
| **上报失败不静默** | 失败会打印原因 + 待传条数 + 两条出路（恢复后补传 / 换绑）；不会看起来"什么都没发生" |
| **在线状态是推导的** | 控制面按 `last_seen_at` 超时判 `offline`；客户端**不能**自报 `offline` |
| **签名证明什么** | `HMAC-SHA256(key = sha256(网关令牌), 规范摘要)` —— 证明**来源与完整**（确实是持令牌的那台网关报的、没被改过），**不证明内容真实**（计数是网关自报的）|
| **换绑 = 换身份** | `bind` 会清空上报账本（本地审计文件不动），旧队列不会算到新网关上 |

---

### `ncc app`（NCC 舱：可自部署的个人 Agent 助理）

**产品是用户的，引擎是 ncc，应用逻辑是 HUR 包，平台只做安全互联。** 一个目录就是一台助理：

```bash
ncc app init --dir ./my-pod --namespace @me --share-target cloud   # 生成 app.json / hur.json / run.sh / README / SKILL
ncc app doctor --dir ./my-pod     # 逐项自检：缺什么、下一步跑什么，都会打出来
ncc app up     --dir ./my-pod     # 起舱：本机控制台 http://127.0.0.1:8487/
ncc app status --dir ./my-pod     # 看现状（不起服务）
ncc app export --dir ./my-pod --out ./my-pod-export   # 别人拿到就能部署一份自己的
```

**舱记着两个目标**（这条是设计要害）：

| 哪一侧 | 走哪个目标 | 住什么 |
|---|---|---|
| 内容 | **当前目标**（`ncc target use <节点名>`） | 画布/笔记 = 知识库 kb · 记忆 = mem · 交接点 = ckpt |
| 分享 | `app.json` 里的 `share.target`（默认 `hub` = 云端） | 点对点分享：把当前内容打成一份**只读快照**发出链接 |

控制台是本机 loopback 上的一个极小页面：四个面板（画布 / 记忆 / 检查点 / 给别人看）+ 一个
「生成快照链接」按钮（**舱里唯一的写动作，由人点**）。别人打开链接**不用账号**，看到的是
当前内容的静态拷贝；带 key 的链接要把 key 一起发给他（key 只回显一次）。对方看不到控制台，
也碰不到你的节点。

三条边界写在 `README.md`（舱里那份）与 `doctor` 输出里：**控制面只有摘要**、
**快照 ≠ 长期权限**（长期是 `ncc grant`）、**删舱 ≠ 删数据**（内容在节点上，要单独删）。

端到端验证：`bash scripts/app-smoke.sh`（三个东西一起跑：引擎 + ncc-registry 节点 + ncc-platform 平台；
隔离端口与 `NCC_HOME`，**51/51**）。

---

## 核心概念

| 术语 | 含义 |
|---|---|
| **制品 Artifact** | 可版本化、可发布的能力单元 —— Skill、MCP Server、API 描述、Harness 等 |
| **Kind** | 制品类别（`skill`、`mcp`、`harness` …），决定消费方如何解读它 |
| **命名空间 Namespace** | 发布范围，可为个人（`@you`）或组织（`@your-org`），以 `@slug` 寻址 |
| **引用 Reference** | `@命名空间/slug` —— 稳定、可移植的制品命名方式 |
| **可见性 Visibility** | `public` 任何人可解析；`private` 需要付费套餐 |
| **状态 Status** | `published`（可解析）、`draft`（仅自己可见）或 `archived` |
| **Manifest** | 制品可选的 JSON 契约。`kind harness` 时承载 `harness.loader` / `harness.entry` |

## 配置

| 路径 | 用途 |
|---|---|
| `~/.ncc/config.json` | **目标（target）清单**：每个目标一份地址与凭据（登录 token / admin key+secret），以及当前目标名。首次运行时自动创建（权限 0600）|
| `~/.ncc/bin/ncc` | 安装脚本或 npm 启动器放置的二进制 |
| `~/.ncc/packages/` | `ncc install` 的默认根目录 |

在本地节点登录**不会**把你从云端挤下线：凭据存在各自的目标里。
`--base` 也会按地址复用/新建目标，而不再默默改写你原来的目标。

### 环境变量

| 变量 | 使用者 | 作用 |
|---|---|---|
| `NCC_CONFIG` | CLI | 配置文件位置，默认 `~/.ncc/config.json` |
| `NCC_PACKAGES_DIR` | CLI | `ncc install` 的安装根目录，默认 `~/.ncc/packages` |
| `NCC_INVITE_CODE` | CLI | 省略 `--invite` 时，`ncc register` 使用的邀请码 |
| `NCC_BIN` | npm 包装 | 强制指定二进制路径（最先检查） |
| `NCC_RELEASE_BASE` | 安装脚本、npm 包装、`ncc upgrade` | 下载发布二进制的基址，默认本仓库的 GitHub Releases |
| `NCC_UPDATE_URL` | `ncc upgrade` | 最新版本查询端点，默认 GitHub releases API |
| `NCC_HOME` | 安装脚本、npm 包装、`ncc upgrade` | 覆盖用来定位 `~/.ncc/bin/ncc` 的用户目录。测试时用，必须与包装脚本看到的同一个值 |

`HOME`、`HOSTNAME`、`SHELL` 会被读取用于推导默认值（配置位置、设备名、POSIX 摘要），可按常规方式覆盖。

> **配置文件是纯文本且保存着 bearer token。** CLI 写入时会自动设为 `0600`（仅本人可读），
> 但如果它是从旧版本升上来的，建议自己确认一次：`chmod 600 ~/.ncc/config.json`。
> 在 CI 中更推荐用 `NCC_PACKAGES_DIR` / `NCC_CONFIG` 指向临时文件，而不是把凭据文件提交进仓库。

## 指向自己的注册中心

任何 NCC 兼容实例都可作为后端：

```bash
# 一次性的：不动当前目标
ncc --base https://registry.internal.example me

# 固定下来：建一个具名目标，之后直接 ncc target use internal
ncc target add internal --base https://registry.internal.example --use
ncc me
```

自托管实例还会提供自己的客户端分发，让用户装到的二进制天然知道正确的 base URL：

- `GET /install.sh` —— 安装脚本，基址改写为当前服务主机
- `GET /downloads/<file>` —— 发布二进制

要通过这两条路径分发自己的构建，把 `NCC_RELEASE_BASE` 设为你控制的镜像即可。

### 内网形态：`ncc-registry`

[`ncc-registry`](https://github.com/fusedmodel/ncc-registry) 是一个自包含的内网节点：单个 Go 二进制
（同时也是一个可 `go get` 的 Go 库），同时**托管制品**、**托管节点**（你的 Agent 与服务），
并让它们**互相发现、连起来**。它可以按「一个 `master` + 任意多个 `worker` 边缘节点」铺开。

它有自己的仓库，在这里以 **git submodule**（`ncc-registry/`）的形式引入 —— 所以直接 clone 的话
那个目录是空的：用 `git clone --recurse-submodules`，或在已有检出里跑 `git submodule update --init`：

```bash
cd ncc-registry && go build -o dist/ncc-registry ./cmd/ncc-registry

# master（权威：账号 / 制品 / 节点目录 / 集群视图）
NCCR_PORT=8282 NCCR_NODE_NAME=office-master ./dist/ncc-registry

# worker（自己也托管制品与节点，并把本地目录上报给 master）
NCCR_ROLE=worker NCCR_PORT=8283 NCCR_NODE_NAME=office-worker-a \
  NCCR_MASTER_URL=http://office-master:8282 ./dist/ncc-registry
```

然后把 CLI 接上去：

```bash
ncc --base http://office-master:8282 registry login --email you@corp.com --password '***'
ncc registry join --kind agent --name my-mac --region 上海-内网 --capabilities mcp,api --daemon
ncc registry status            # 本节点 + 集群 + 我的托管节点
ncc registry catalog           # 聚合目录（master + 各 worker）
ncc registry route @alice/hotel-skill   # 这个能力到底在哪个节点上
ncc install @alice/hotel-skill          # 字节由 master 代理回来
```

不用手把手教每个人填 `--base` + 密码：签一张**接入票据**，把链接发出去就行 ——
secret 放在 URL fragment 里，不进服务端日志、不进 Referer：

```bash
# 在内网节点上：签一次，把链接发给对方
ncc registry ticket create --label "alice 的 Agent" --uses 1 --expires 7
# → key NK-7F3A2C、secret（只显示一次）、链接 http://office-master:8282/j/NK-7F3A2C#<secret>

# 在对方机器上：一条命令直接接入
ncc registry add 'http://office-master:8282/j/NK-7F3A2C#<secret>' --join --kind agent
# 或分开填：
ncc registry add --base http://office-master:8282 --key NK-7F3A2C --secret <secret> --join
```

拿到的是**节点令牌**：只能上报自己的心跳、读公开制品，不能发布。链接用浏览器打开会有同样的说明，
外加一个「用本链接凭据接入」的验证按钮。

内网节点还管两件事：**分发**（把副本推到 worker，就近可拉）与**回收**（下架时把各处的副本收掉）：

```bash
ncc publish --file ./hotel.SKILL.md --kind skill --name "Hotel Skill" --replicate all
ncc registry replicate @alice/hotel-skill --to all
ncc registry rm @alice/hotel-skill --yes     # master 下架并回收全部副本
```

而连接仍然不等于授权：要看/取别人的私有制品或接入非公开服务，仍需要显式授权
（`ncc grant set --user @bob --kind artifact|service`）。

完整 API、配置表与部署说明见 [`ncc-registry/README.md`](ncc-registry/README.md)。

## 脚本与 CI

CLI 被设计为可被其它程序驱动：

- **退出码** —— 成功 `0`，任何失败 `1`。
- **错误** —— 写到 stderr，形如 `✗ [error_code] message`，其中 code 与 message 直接来自注册中心的 JSON 错误体（`{"error":{"code":…,"message":…}}`）；网络故障另行报告为 `网络错误: …`。
- **结构化数据** —— `ncc info <target>` 在 stdout 打印制品的 pretty JSON，可直接交给 `jq`。
- **非交互认证** —— 一次生成 API-Key（`ncc key create --label ci`，仅显示一次），用它替代登录态。
- **签名门禁** —— `ncc verify <文件 | @命名空间/slug> --require-signature`：签名不能在本机被核对就非 0 退出；
  **被改过**的制品不需要这个开关就失败。注意「有签名但公钥不认识」同样过不了门禁 —— 那正是它该有的行为。
- **面向人的文本** —— `search`、`publish`、`install` 等打印人类可读的中文输出；需要机器稳定的输出时请直接调用 HTTP API。注意 `ncc terminal` 会检测非 TTY 并降级为 REPL，而不是报错。

客户端网络行为：API 客户端连接超时 10s、整体超时 60s、最多 10 次重定向。制品字节以 60s 预算拉取，并在写盘前整体缓存在内存中，因此 `download` / `install` 目前不适合超大制品。

## 开发

```bash
cd cli

cargo check                   # 快速类型检查
cargo build --release         # → target/release/ncc
cargo fmt && cargo clippy     # 若已安装对应 rustup 组件
```

想在不安装的情况下对着运行中的注册中心试跑：

```bash
NCC_CONFIG=/tmp/ncc-dev.json ./target/release/ncc --base http://localhost:8181 me
```

目前还没有自动化测试 —— 一个对着真实注册中心驱动 CLI 的冒烟测试，是这里最有价值的贡献。

源码结构：

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 参数解析（clap）与所有命令实现 |
| `src/api.rs` | 基于 `ureq` 的轻量 HTTP 客户端：JSON 请求、raw 上传、错误解码 |
| `src/config.rs` | `~/.ncc/config.json` 读写与登录态处理 |
| `src/profile.rs` | 名片、作品集、角色目录 |
| `src/nodes.rs` | 节点连接（链接表 / 发现）、授权（grant）、区域聚合与推荐 |
| `src/services.rs` | 对外服务：目录 / 匹配 / 取用 / 声明与下架 |
| `src/registry.rs` `src/registryadd.rs` | 自托管内网节点（ncc-registry）：登录 / 入网 / 目录 / 路由 / 票据 |
| `src/configs.rs` | 配置托管：目录 / 取（默认打码）/ 写入与版本 / 回滚 / bundle |
| `src/admin.rs` | 节点治理（admin：用户 / 节点 / 服务 / 审计 / 凭据轮换）与分享链接 |
| `src/mcp.rs` | MCP server（stdio）：工具 schema 与分发 |
| `src/terminal.rs` | 能力命令台、POSIX 运行时探测、更新检查 |
| `src/tui.rs` | 全屏 ratatui TUI（stdin 为真实 TTY 时启用） |

值得保持的设计约束：依赖列表保持精简；所有操作都走公开 HTTP API，不另造私有协议；客户端永不成为机器之间的数据中转。

## 发版

```bash
bash scripts/build-release.sh          # 当前平台 → release/bin/ncc-<os>-<arch>
bash scripts/build-release.sh --all    # 交叉编译全部目标（需 `rustup target add …`）
```

脚本会写出 `release/bin/ncc-<os>-<arch>[.exe]` 并重新生成 `checksums.txt`。

完整发版流程：

1. 更新 `cli/Cargo.toml`（并刷新 `cli/Cargo.lock`）与 `packages/ncc-cli/package.json` 的版本号。
2. 运行 `scripts/build-release.sh --all`。
3. 打 tag 并发布 GitHub Release，附上这些二进制 —— 安装脚本、npm 包装与 `ncc upgrade` 都以此为准。
4. 在 `packages/ncc-cli` 目录执行 `npm publish --access public`。
5. 回来更新[当前状态](#当前状态)：一旦有了 Release，上面的安装脚本与 npm 路径就正式可用。

预编译目标：`darwin`（x86_64、arm64）、`linux`（x86_64、arm64）、`windows`（x86_64）。目前 `release/bin` 只入库了 `darwin-arm64`，其余由 `--all` 生成。

## 仓库结构

```
cli/                 Rust crate（bin: ncc）
packages/ncc-cli/    npm 包装（@fusedmodel/ncc-cli）—— 启动器 + 二进制下载
examples/            能跑的真包示例（每个子目录一个完整 HUR 工程，见 examples/README.md）
ncc-registry/        内网自托管节点 —— git submodule → github.com/fusedmodel/ncc-registry
                     （Go：单二进制 + 可 import 的库）：制品托管 + 节点托管 + master/worker 多节点
agent/               Agent 接入包（MCP 配置、SKILL.md、harness 契约）
release/bin/         入库的预编译二进制 + checksums.txt
scripts/             build-release.sh（交叉编译 + 校验和）
```

## 让 Agent 使用

`ncc mcp` 以 **MCP server** 方式（stdio）跑起 NCC，任何支持 MCP 的 Agent 都能检索目录、取回制品、
发布成果、查找同行，读它自己的知识库/记忆/检查点与运行轨迹，查组织网关的在线状态与合规审计摘要 ——
不需要额外服务（38 个工具，按**目标声明的能力**放行）：

```jsonc
{ "mcpServers": { "ncc": { "command": "ncc", "args": ["mcp"] } } }
```

| 工具 | 用途 |
|---|---|
| `ncc_list_kinds` | 目录里有哪些类型、各多少条 |
| `ncc_search_catalog` | 按关键词 / kind / tag / 命名空间检索 |
| `ncc_get_artifact` | 单个制品的完整元数据 |
| `ncc_fetch_artifact` | 取回制品正文（SKILL.md 可直接读进上下文） |
| `ncc_publish_artifact` | 发布制品（需凭据） |
| `ncc_whoami` | 当前账号与命名空间 |
| `ncc_list_roles` | 工作角色目录 |
| `ncc_find_people` | 按角色 / 技能 / 我关注的人找人 |
| `ncc_get_profile` | 某人的名片：角色 + 作品集 + 已发布能力 |
| `ncc_match_services` | 按意图匹配对外服务：分数、命中理由、接入步骤 |
| `ncc_list_services` | 浏览服务目录（分类 / 标签 / 区域） |
| `ncc_get_service` | 单条服务的完整接入信息 |
| `ncc_service_categories` | 业务分类目录（`category` 取值） |
| `ncc_match_index` | 按一句需求在**索引**里找人（`want=need` 则是找需求）；排序含内部信誉权重，**不给分数** |
| `ncc_list_index_channels` | 索引里的频道列表（各多少条、供给 / 需求分布） |
| `ncc_list_configs` | 托管配置目录（公开配置无需凭据；`mine` 看自己的） |
| `ncc_get_config` | 取一份配置（**默认打码**，`reveal` 才回明文） |
| `ncc_list_nodes` | 我的节点连接表（`mine` / `links`） |
| `ncc_discover_nodes` | 本实例上可连接的节点 |
| `ncc_region_profile` | 节点在哪些区域更厚 |
| `ncc_recommend_nodes` | 按区域推荐节点，同区域优先 |
| `ncc_list_grants` | 授权关系（给出的 / 收到的） |
| `ncc_list_kb` | 托管知识库：列表 / 关键词检索 |
| `ncc_get_kb` | 读一篇知识库文档（带 `checksum`） |
| `ncc_list_mem` | 托管记忆条目（键 / 值 / 种类 / TTL / 来源） |
| `ncc_get_mem` | 按 key 读一条记忆（Agent 读记忆的主路径） |
| `ncc_list_ckpt` | 托管检查点：元数据与摘要（字节走 `ncc ckpt pull`） |
| `ncc_list_traces` / `ncc_trace_stats` | 运行轨迹列表 / **聚合结论**（成功率、耗时、token、花费、按版本分组） |
| `ncc_p2p_probe` / `ncc_p2p_check` / `ncc_p2p_node` | 打洞条件预检（本机，纯本地）/ 真实对打实测（0 字节）/ 目标节点那台机器的画像与入口状态 |
| `ncc_list_gateways` | 网关控制面：我们有哪些网关、**在线吗**（状态由心跳**推导**）、用了多少 |
| `ncc_gateway_audit` | 某台网关留存的审计**摘要**：计数、**主机名**、状态桶、延迟分位 |
| `ncc_gateway_usage` | 某台网关的用量汇总（**自报计数**，口径写在返回里） |

节点、授权、服务与**状态**相关的工具**故意做成只读**。任何会改变「别人能拿到什么」的动作 ——
声明服务、连接节点、授权 —— 都留在 CLI 里，由用户明确执行。状态工具也是这个道理：
让一次工具调用悄悄改掉 Agent 的记忆或知识，事后没人能复盘是谁改的 ——
所以 `ncc kb set` / `ncc mem set` / `ncc ckpt save` 留在 CLI。

网关那三个工具读的是控制面留存的**窗口摘要**（不是完整审计）：路径、载荷与凭据留在**网关本机**，
所以「谁调了哪个 URL」在 Agent 侧回答不了 —— 要去那台机器上跑 `ncc gateway audit`。
另外两条口径：在线状态是控制面按心跳超时**推导**的，用量是网关**自报**的（签名只证明来源与完整，
不证明内容为真）。注册/吊销网关、让网关开始上报，都是**那台机器上**的人的动作。

检索、取回与人才目录**无需登录**（唯一的例外是 `following` 过滤 —— 它问的是「**我**关注了谁」，
就得知道你是谁）；发布则需要凭据。`ncc mcp` 的 stdout 只输出协议消息、
日志全部走 stderr —— 这是 MCP stdio 的硬要求。

[`agent/`](agent) 是可分发的接入包：MCP 配置、给不支持 MCP 的 Agent 用的 `SKILL.md`，
以及 `kind=harness` 契约（`mcp/stdio` loader），任何实现该 loader 的 runtime 都能加载。

## 参与贡献

欢迎贡献 —— 尤其是缺陷报告、文档修正与平台支持。

- 动手做较大的改动前，请先开 issue 对齐方案。
- PR 保持聚焦；沿用现有风格，优先使用标准库与当前依赖，而非引入新 crate。
- 注意 CLI 是注册中心 API 的*客户端*。改动线上格式需要服务端同步改动，请在 issue 中一并说明。
- CLI 输出字符串目前是中文，且尚未为翻译集中管理。若想做本地化，请先开 issue —— 这是已知缺口，不是疏忽。

## 安全

发现漏洞请**不要**开公开 issue。请通过本仓库的 [GitHub Security Advisories](https://github.com/fusedmodel/ncc/security/advisories/new) 私下报告，并附上复现步骤与受影响版本。

请注意 `~/.ncc/config.json` 以纯文本保存 bearer token，且 `ncc key create` 生成的 API-Key 只显示一次 —— 两者都请按密钥对待。

## 许可

Apache License 2.0 —— 见 [LICENSE](LICENSE)。
