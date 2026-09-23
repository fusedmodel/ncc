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
| `ncc ns list` | 列出你拥有或加入的命名空间 |
| `ncc ns create` | 创建命名空间 |
| `ncc publish` | 通过上传文件或 BYO URL 发布制品 |
| `ncc search [query]` | 目录检索 |
| `ncc info <target>` | 以 JSON 打印制品完整记录 |
| `ncc download <target>` | 下载制品字节 |
| `ncc install <target>` | 安装到本地包目录 |
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
| `ncc terminal [status\|setup]` | 打开能力命令台 / 查看 POSIX 运行时 |
| `ncc upgrade` | 把 CLI 二进制就地升级到最新发布版（`--check` 只检查不下载，`--force` 强制重装）|
| `ncc mcp` | 以 **MCP server**（stdio）启动，让任意 Agent 驱动 NCC |
| `ncc help <command>` | 查看任意命令的自动生成帮助 |

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

[`ncc-registry`](ncc-registry/) 是一个自包含的内网节点：单个 Go 二进制，同时**托管制品**、
**托管节点**（你的 Agent 与服务），并让它们**互相发现、连起来**。它可以按「一个 `master` +
任意多个 `worker` 边缘节点」铺开：

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
packages/ncc-cli/    npm 包装（@fusedmodel/ncc-cli）—— 启动器 + 二进制下载ncc-registry/        内网自托管节点（Go 单二进制）：制品托管 + 节点托管 + master/worker 多节点agent/               Agent 接入包（MCP 配置、SKILL.md、harness 契约）
release/bin/         入库的预编译二进制 + checksums.txt
scripts/             build-release.sh（交叉编译 + 校验和）
```

## 让 Agent 使用

`ncc mcp` 以 **MCP server** 方式（stdio）跑起 NCC，任何支持 MCP 的 Agent 都能检索目录、取回制品、
发布成果、查找同行 —— 不需要额外服务：

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
| `ncc_find_people` | 按角色 / 技能找人 |
| `ncc_get_profile` | 某人的名片：角色 + 作品集 + 已发布能力 |
| `ncc_match_services` | 按意图匹配对外服务：分数、命中理由、接入步骤 |
| `ncc_list_services` | 浏览服务目录（分类 / 标签 / 区域） |
| `ncc_get_service` | 单条服务的完整接入信息 |
| `ncc_service_categories` | 业务分类目录（`category` 取值） |
| `ncc_list_configs` | 托管配置目录（公开配置无需凭据；`mine` 看自己的） |
| `ncc_get_config` | 取一份配置（**默认打码**，`reveal` 才回明文） |
| `ncc_list_nodes` | 我的节点连接表（`mine` / `links`） |
| `ncc_discover_nodes` | 本实例上可连接的节点 |
| `ncc_region_profile` | 节点在哪些区域更厚 |
| `ncc_recommend_nodes` | 按区域推荐节点，同区域优先 |
| `ncc_list_grants` | 授权关系（给出的 / 收到的） |

节点、授权与服务相关的工具**故意做成只读**。任何会改变「别人能拿到什么」的动作 ——
声明服务、连接节点、授权 —— 都留在 CLI 里，由用户明确执行。

检索、取回与人才目录**无需登录**；只有发布需要凭据。`ncc mcp` 的 stdout 只输出协议消息、
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
