mod admin;
mod agent;
mod api;
mod app;
mod auth;
mod authpkg;
mod capability;
mod config;
mod configs;
mod conn;
mod connssh;
mod feedback;
mod gateway;
mod gwreport;
mod httpsrv;
mod hur;
// 本机执行（`ncc hur run --exec`）。只在带 sandbox feature 时编译进来
// （默认开；`--no-default-features` 得到不含 wasmtime 的瘦身构建）。
#[cfg(feature = "sandbox")]
mod hurrun;
mod index;
mod mcp;
mod nodes;
mod p2p;
mod profile;
mod registry;
mod registryadd;
mod registryp2p;
mod rsi;
mod sandbox;
mod services;
mod signcmd;
mod state;
mod store;
mod target;
mod terminal;
mod trace;
mod tui;
mod upgrade;

use anyhow::{anyhow, bail, Context};
use clap::{Parser, Subcommand};
use config::CliConfig;
use serde_json::{json, Value};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// stdio 协议模式（`ncc mcp`）。
///
/// ⚠️ MCP 的硬要求：stdout **只能**出 JSON-RPC 帧。一行人类可读提示就会让客户端
/// 解析失败（实测踩过：`ncc mcp --base …` 的「已切到某目标」提示泄到 stdout）。
/// 所以一切「人读提示」都要过 `note()`，协议模式下自动改走 stderr。
static PROTOCOL_STDOUT: AtomicBool = AtomicBool::new(false);

/// 人读提示：正常走 stdout；协议模式下走 stderr（不污染帧流）。
fn note(msg: &str) {
    if PROTOCOL_STDOUT.load(Ordering::Relaxed) {
        eprintln!("{msg}");
    } else {
        println!("{msg}");
    }
}

#[derive(Parser)]
#[command(
    name = "ncc",
    version,
    about = "ncc.ai Registry 命令行客户端\n用法: ncc <command> [args...]\n目标：ncc target list（云端 ncc.ai / 内网 registry 节点各是一个目标）"
)]
struct Cli {
    /// 服务地址（本次命令用这个地址；已存在同名目标则复用，否则新建一个目标）
    #[arg(long, global = true)]
    base: Option<String>,
    /// 本次命令用哪个目标（ncc target list 看全部）
    #[arg(long, global = true)]
    target: Option<String>,
    /// 以**对外访问令牌**执行这条命令（`ncc auth login` 拿到的）
    ///
    /// 它受众是某一个第三方平台，**只能走数据面**（目录 / 服务 / 索引 / 名片）——
    /// 账号面（key、授权、凭据）会 403，这是设计而不是缺功能。
    /// 若该令牌绑定了凭据（`--credential`），CLI 会自动附上持有证明 `NCC-Proof`。
    #[arg(long, global = true)]
    auth: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 注册新账户（自动创建个人命名空间）
    Register {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        #[arg(long)]
        name: Option<String>,
        /// 注册邀请码（服务端启用门禁时必需；也可用环境变量 NCC_INVITE_CODE）
        #[arg(long)]
        invite: Option<String>,
    },
    /// 登录
    Login {
        #[arg(long, default_value = "")]
        email: String,
        #[arg(long, default_value = "")]
        password: String,
        /// 直接拿一个 API-Key 当登录态（机器人 / 桌面端绑定用，不必知道密码）
        #[arg(long, default_value = "")]
        api_key: String,
    },
    /// 登出
    Logout,
    /// 当前用户与命名空间
    Me,
    /// 命名空间与组织计划：ncc ns list | ns plans | ns create --slug … [--plan free]
    #[command(subcommand)]
    Ns(NsCmd),
    /// 发布条目：ncc publish --kind skill --name X --file ./x.SKILL.md
    Publish(PublishArgs),
    /// 目录检索：ncc search [query] [--kind api]
    Search(SearchArgs),
    /// 条目详情：ncc info <id | @org/slug>
    Info {
        /// 条目引用：R-… 或 @org/slug
        ///
        /// ⚠️ 这个位置参数的字段名**不能**叫 `target`：顶层 `--target` 是
        /// `global = true`，两者 arg id 相同时 clap 会把位置参数的值塞进全局项，
        /// 于是 `ncc info R-…` 会报「没有名为 R-… 的目标」（已实测的坑）。
        reference: String,
    },
    /// 下载条目字节：ncc download <id | @org/slug> [-o 文件]
    Download {
        /// 条目引用：R-… 或 @org/slug
        ///
        /// ⚠️ 同 `Info`：字段名**不能**叫 `target`。顶层 `--target` 是 `global = true`，
        /// 位置参数与它 arg id 相同时，clap 会把位置参数的值塞进全局项，于是
        /// `ncc download @you/x` 会去找一个**名叫 `@you/x` 的目标**并报
        /// 「没有名为 @you/x 的目标」—— 加 `--target`、加 `--base` 都绕不过去。
        /// （这个坑在 info / verify / mem rm 上都修过，download / install 漏了。）
        reference: String,
        #[arg(short = 'o', long)]
        out: Option<String>,
    },
    /// 安装条目到本地包目录：ncc install <id | @org/slug> [--dir 目录] [--force]
    Install {
        /// 条目引用：R-… 或 @org/slug
        ///
        /// ⚠️ 同 `Download`：字段名不能叫 `target`（理由见上）。
        reference: String,
        /// 本地安装根目录（默认 ~/.ncc/packages）
        #[arg(short = 'd', long)]
        dir: Option<String>,
        /// 已安装时也覆盖
        #[arg(long)]
        force: bool,
    },
    /// 打开 NCC Terminal 命令台（能力命令台 · 官方包 @ncc/terminal）
    Terminal {
        #[command(subcommand)]
        action: Option<TermAction>,
    },
    /// 自更新：把 CLI 二进制升级到发布通道里的最新版本（--check 只检查、不下载）
    ///
    /// `update` 是隐藏别名，老文档与脚本里的 `ncc update` 仍可用（但不再出现在 help 里）。
    #[command(alias = "update")]
    Upgrade(upgrade::UpgradeArgs),
    /// hur 制品工具链：ncc hur verify | pack | sign | key | run | publish
    ///
    /// 本地能力（verify/pack/sign/key）**全程离线**；只有 `publish`（上传产物）与
    /// `attach`（只上传签名文件与公钥）联网。
    /// 执行：默认只出可审计划；`--exec` 在本机 wasm 沙箱里真跑（限额来自策略）
    #[command(subcommand)]
    Hur(hur::HurCmd),
    /// 制品加签：签的是**发布出去的那份字节**（不限 kind —— Skill / MCP / 任意文件）
    ///
    /// 包（HUR 目录 / .hur）会转交给 `ncc hur sign`（那里签的是规范打包字节，
    /// 要连包身份/版本/lock 一起核对）。签名只写本机 `<制品>.minisig`，
    /// 第三方拿公钥 `minisign -V -p ncc.pub -m SKILL.md` 就能独立核对。
    ///
    /// `--attach` 是唯一联网动作：只上传签名文件与公钥，制品字节一个字节都不传。
    Sign(signcmd::SignArgs),
    /// 制品验签：本地文件走本机，条目引用则下载产物字节再核对（--require-signature 可进 CI）
    Verify(signcmd::VerifyArgs),
    /// API-Key：ncc key create --label ci | ncc key list | ncc key revoke <id>
    #[command(subcommand)]
    Key(KeyCmd),
    /// 设备接入（Living）：上报本设备心跳到你的命名空间（--daemon 周期守护）
    Living(LivingArgs),
    /// Agent 分享（点到点）：把我的 Agent 交给指定的人
    ///
    /// `share` 造一张名片（包 + 可选节点）→ 给对方一条 `https://ncc.ai/a/AC-…` 链接；
    /// `add` 收下别人的名片 → **装包** + **连接节点**（可 --no-install / --no-link 只做一半）。
    /// 平台**不展示、不检索**任何人的名片；撤销即删（连字节）。
    Agent(agent::AgentCmd),
    /// 云电脑（Remote Cloud Computer）：把任务丢到一台远程沙箱机器上跑
    ///
    /// `init --host … --port … --key …` 用 ip/port/key 登记一台能接活的 ncc 节点
    /// （登记进既有的沙箱环境表，`ncc hur env` 与策略对云电脑自动生效）；
    /// `run` 把任务送过去：`--package` 走 wasm 沙箱，`--cmd` 走 process/container
    /// （OS 敏感任务，例如 `docker build && docker push`，需要对方运维显式放行）。
    /// 退出码跟随远端，可直接用在 CI 里。
    Sandbox(sandbox::SandboxCmd),
    /// 通信基础设施：到 Cloud instance 的**连接通道**（跑命令 / 推拉文件 / 一批脚本）
    ///
    /// 两种运输层：`--on <已登记的云电脑>` / `--url … --key …`（HTTP：目标端跑 ncc-registry，
    /// 要开 `NCCR_CONN_ALLOW=1`）；或 `--ssh [user@]host[:port]`（**只要对方有 sshd**，
    /// 零服务端改动，复用你本机的 ssh 与 key）。建好后 `exec` / `push` / `pull` 反复用，
    /// `run` 把「推多个文件 + 跑一段脚本」一次做完；`close --purge` 收线。
    Conn(conn::ConnCmd),
    /// NCC RSI：**运行时安全与自改进**（Runtime Safety & Improvement）
    ///
    /// 一句话：**动作发生之前拦住它、记下来、事后算总账**。
    /// `rsi check`（决策门：给一份决策请求，拿 0/10/20 的裁决）、`rsi guard`（守着跑，
    /// block 就真的不跑）、`rsi goal`（不跑偏）、`rsi pref`（用户偏好）、
    /// `rsi report`（无人值守过后的总账）；`rsi hook install` 把它挂成宿主的 PreToolUse 钩子。
    /// **全部本地**（策略 / 账本 / 偏好 / 目标都是文件），离线也能拦。
    Rsi(rsi::RsiCmd),
    /// NCC Feedback：跨 Agent / 跨用户的反馈（CLI → 节点 → 云端 → 服务）
    /// 一句话：**说一句关于某个东西的话，它自己知道该落在哪儿。**
    /// `feedback send`（先落盘再发送，送不出去就留在本地队列）、`ls` / `get` / `reply`、
    /// `status`（只有目标拥有者能改处置状态）、`summary` / `inbox`（聚合与收件箱）、
    /// `relay`（把内网节点上的**公开**反馈搬到 hub：私有的永不出机器）、`spool` 看本地队列。
    #[command(subcommand)]
    Feedback(feedback::FeedbackCmd),
    /// NCC Profile：查看 / 设置名片（定位角色 + 作品集 + 已发布能力）
    Profile(ProfileArgs),
    /// NCC Node：节点连接（我的节点 + 连接别人的节点 + 发现 / 区域推荐）
    Nodes(NodesArgs),
    /// NCC Gateway：固定路由的白名单代理（S2a）—— 提供出口（accept）/ 借用出口（forward）
    ///
    /// 数据面只在 A↔B 之间直连；**控制面是可选的**：绑了才上报**摘要**（逐条审计留本机）。
    /// 调用方**不能指定目标地址**，只能给「路由名 + 路由内的路径」。
    #[command(subcommand)]
    Gateway(GatewayCmd),
    /// 个人 Agent 舱：把「用户自己部署一个人助理」变成一条命令（设计见 prd/ncc-personal-agent.md）
    ///
    /// 分工：产品是用户的（app.json）、引擎是 ncc、应用逻辑是 HUR 包（hur.json）、
    /// 互联走 ncc-platform（身份 / 点到点分享 / 网关 / P2P）—— 不新增数据面。
    #[command(subcommand)]
    App(AppCmd),
    /// NCC Service：对外服务（服务提供方打包的多条业务）—— 匹配找服务 / 声明自己的服务
    ///
    /// 需要目标声明 `services` 能力（云端 ncc.ai 已声明；内网节点将来也可以声明，
    /// 那时同一个命令在那台节点上直接可用）。
    Services(ServicesArgs),
    /// NCC Index：把「我有什么 / 我要什么」登记进一个**频道**，
    /// 别人用一句需求就能检索到（`ncc match` / `ncc list`）
    ///
    /// 平台是权威、内网节点是副本：`publish` 先写平台再尽力推已接入的节点，
    /// 节点推失败不回滚，但会逐台报出来。
    Index(IndexArgs),
    /// 已索引的人 / 需求 / 频道
    List(ListArgs),
    /// 我有需求 → 谁能在（或我在找活儿 → 谁要人）：
    /// ncc match "帮我订杭州的酒店" --channel booking/hotel
    Match(index::MatchArgs),
    /// NCC Trace：Agent / HUR 的运行轨迹（采集 → 本地暂存 → 上传 → 评测 / 训练数据集）
    ///
    /// 纯本地命令不少（add / ls / show / stats / export / label 都有本地形态），
    /// 只有 `push` 与带 `--remote` 的那些才联网 —— 轨迹默认留在你自己的机器上。
    #[command(subcommand)]
    Trace(trace::TraceCmd),
    /// NCC KB：托管知识库（语料）—— 列表 / 取用 / 写作 / 检索 / **按包的声明拉取**
    ///
    /// 知识库是**状态**不是制品：包只能在 hur.json 的 `state.kb` 里声明它要哪些，
    /// 字节住在节点上（`ncc kb pull --package .` 就是"读声明 → 取库"，按 checksum 增量）。
    #[command(subcommand)]
    Kb(state::KbAction),
    /// NCC Mem：托管的 Agent 记忆（键值 + TTL + 来源）—— 跨运行、跨机器记得住
    ///
    /// **没有公开档**：记忆只属于命名空间成员与拿到 `state` 授权的人。
    /// 写是 upsert（同键即更新，Revision+1），过期**读时**即生效。
    #[command(subcommand)]
    Mem(state::MemAction),
    /// NCC Ckpt：托管的检查点（不可变快照 + 血缘）—— 交接与回滚的落点
    ///
    /// 字节进 blob、元数据进库；取回时客户端**核对摘要**（检查点的价值就是
    /// "拿回来的是原来那份"）。不可变：要改就再打一个点。
    #[command(subcommand)]
    Ckpt(state::CkptAction),
    /// NCC Store：通用记录仓 —— **声明一个集合就是新增一类内容**（issue / log / 复盘 / 备注…）
    ///
    /// 与 kb / mem / ckpt / trace 的分工：那四类各是**一类内容**，这里承载的是
    /// 「还不值得单写一类」的那些东西 —— 集合（有哪些字段、能不能改、给谁看、放多久）
    /// 是**声明**出来的，服务端一行不用改。三条边界：动态≠无模式、不可变就是不可变、CRUD≠授权。
    #[command(subcommand)]
    Store(store::StoreCmd),
    /// NCC Registry 节点（内网托管节点）：登录 / 入网 / 目录 / 路由 / 配置 / 分享 / 管理
    ///
    /// 只在内网节点目标上跑（kind=registry）；云端命令见 `ncc hub …`。
    #[command(subcommand)]
    Registry(RegistryCmd),
    /// 目标管理：我连着哪些 ncc（云端 ncc.ai / 内网 registry 节点）
    Target(target::TargetArgs),
    /// 制品/分享授权：ncc grant set --user @someone --kind artifact | list | rm <id>
    #[command(subcommand)]
    Grant(GrantCmd),
    /// NCC Auth：对第三方平台的身份与授权颁发方（凭据 / 设备码登录 / 授权管理）
    ///
    /// 需要目标声明 `auth` 能力（云端要 NCC_AUTH_ENABLED 打开，默认关）。
    /// 三条边界：**注册凭据 ≠ 有权用**、**撤销按平台独立**、**私钥永不出本机**。
    Auth {
        #[command(subcommand)]
        action: auth::AuthCmd,
    },
    /// NCC P2P：跨局域网节点直连（打洞条件预检 / 真实建连检查 / 信令 / 票据）
    ///
    /// 需要目标声明 `p2p` 能力（ncc.ai 云端已声明）；`probe` 是纯本地命令，不需要服务器。
    #[command(subcommand)]
    P2p(p2p::P2pCmd),
    /// 以 MCP server 方式暴露 NCC（stdio），供 Claude Desktop / Cursor / VS Code / 任意 Agent 接入
    ///
    /// **模型面 = 声明面**：`--package <包>` 时只暴露那个包 `state.stores[]`
    /// 声明过的集合（读按 mode 给、写只有声明了写才给）；没声明 = 模型连名字都看不到。
    Mcp(McpArgs),
}

/// `ncc mcp` 的参数：模型面能不能收窄，取决于有没有告诉它「哪个包的声明」。
#[derive(clap::Args)]
struct McpArgs {
    /// 只暴露这个包（目录或 hur.json）`state.stores[]` 声明过的集合 ——
    /// **模型面 = 声明面**：没声明的一个都不给（读按 mode，写要有声明）
    #[arg(long, default_value = "")]
    package: String,
}

/// ncc nodes 的子命令。不跟子命令 = 列我的节点与连接。
#[derive(clap::Args)]
struct NodesArgs {
    #[command(subcommand)]
    action: Option<NodesCmd>,
}

/// 按节点模型组织：节点声明自己是什么，连接是我这边的清单。
#[derive(Subcommand)]
enum NodesCmd {
    /// 我的节点 + 我连接的节点（--kind / --q / --can 过滤）
    List(nodes::ListArgs),
    /// 节点类型目录（上报时用 `ncc living --kind` 声明）
    Kinds,
    /// 节点「提供能力」词表（上报用 --capabilities，检索用 --can）
    Offers,
    /// 发现本 NCC 实例上可连接的节点（--kind / --region / --q / --can）
    Discover(nodes::DiscoverArgs),
    /// 连接节点：ncc nodes link @命名空间/节点slug --label "我给它的名字"
    Link(nodes::LinkArgs),
    /// 改 Name 标签 / 备注
    Label(nodes::LabelArgs),
    /// 断开连接（只删我这边的连接表条目）
    Unlink { id: String },
    /// 区域覆盖：我的节点按归属者所在地聚合（Agent 面）
    Region,
    /// 按区域推荐可连接的节点（Agent 面，同区域优先）
    Recommend(nodes::RecommendArgs),
}

#[derive(Subcommand)]
enum AppCmd {
    /// 在目录里生成舱：app.json + hur.json + README.md + SKILL.md（不覆盖已有文件，除非 --force）
    Init {
        #[arg(long)]
        dir: Option<String>,
        /// 舱名（默认 personal-agent）
        #[arg(long)]
        name: Option<String>,
        /// 内容住哪个命名空间（默认取当前身份；不给则让服务端按「我的」处理）
        #[arg(long)]
        namespace: Option<String>,
        /// 分享走哪个目标（默认 hub = 云端 ncc.ai；内容永远走当前目标）
        #[arg(long)]
        share_target: Option<String>,
        /// 控制台端口（默认 8487，只监听 127.0.0.1）
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// 逐项自检：部署描述 / 包声明（走 hur-core）/ 凭据 / 目标与能力 / 三样内容 / 出口 / 端口
    Doctor {
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 起舱（常驻）：本机控制台 + （绑定了才起）网关心跳与摘要上报
    Up {
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// 看舱的现状（不起服务）：命名空间 / 入口 / 三样内容 / 出口 / 自检提醒
    Status {
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 导出成可交付目录（含 sha256 清单）—— 别人拿到就能部署一份自己的
    Export {
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum GatewayCmd {
    /// 写一份示例配置到 ~/.ncc/gateway.json（--force 覆盖）
    Init {
        #[arg(long)]
        force: bool,
    },
    /// 只校验配置（不启动）：能启动但不安全的配置会在这里被拒
    Check,
    /// 启动网关（常驻；Ctrl+C 停止）
    Run,
    /// 看配置摘要 + 是否在跑
    Status,
    /// 换一家上游供应商（只对多供应商的 accept 路由）：改配置，运行中的网关会热应用
    Switch {
        /// 路由名（出现在 URL 里的那个，如 llm）
        route: String,
        /// 供应商名（写在该路由 providers 里的键）
        provider: String,
    },
    /// 看审计（只读；本地只记元数据不含载荷，--remote 看控制面留存的**摘要**）
    Audit {
        #[arg(long, default_value_t = 20)]
        tail: usize,
        #[arg(long)]
        json: bool,
        /// 看控制面那侧的留存摘要（要登录）
        #[arg(long)]
        remote: bool,
        /// 只看某个时间之后的（RFC3339 / 2026-09-26 / unix 秒）
        #[arg(long)]
        since: Option<String>,
        /// 列表条数（--remote 时用）
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// 导出 CSV（合规报告；只有 --remote 有意义）
        #[arg(long)]
        csv: bool,
    },
    /// 绑定控制面：注册网关 → 拿一次性令牌 → 写进 gateway.json（之后心跳/上报都认它）
    Bind {
        /// 登记到哪个命名空间（默认个人空间；组织空间需要 Pro）
        #[arg(long)]
        namespace: Option<String>,
        /// 网关名（默认主机名）
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 发一次心跳（`run` 会按 heartbeat_sec 自动发；这条是手动/排障用）
    Heartbeat {
        /// online（默认）| draining（准备下线：还在心跳，但别派新活）
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 把本地 JSONL 审计聚合成**摘要**、签名、上报控制面（断线时进待传队列，恢复后补传）
    Report {
        /// 从什么时候之后开始聚合（默认：上次的水位）
        #[arg(long)]
        since: Option<String>,
        /// 单个窗口最长多少分钟（服务端拒收 > 24h 的窗口）
        #[arg(long)]
        window_minutes: Option<i64>,
        /// 只算不发送：看看会报什么（**不推进水位**）
        #[arg(long)]
        dry_run: bool,
        /// 丢掉被控制面拒绝的摘要（默认留着并报错；拒收是数据问题，不该静默吞掉）
        #[arg(long)]
        drop_rejected: bool,
        #[arg(long)]
        json: bool,
    },
    /// 看控制面记的用量摘要（**按自报计数**，口径写在输出里）
    Usage {
        #[arg(long)]
        json: bool,
    },
    /// 注销：吊销网关凭据（本地审计不动；控制面已留存摘要保留到留存期）
    Unbind {
        /// 连本地上报账本一起删（默认保留：里面可能有还没传出去的窗口）
        #[arg(long)]
        purge_state: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum GrantCmd {
    /// 我的授权（--out 我给出的 / --in 别人给我的）
    List(nodes::GrantListArgs),
    /// 授权：--user <id|@handle> --kind share|artifact|service [--ns @slug] [--note …]
    Set(nodes::GrantSetArgs),
    /// 撤销授权
    Rm { id: String },
}

/// `ncc index` 子命令。不带子命令 = 列出我登记的索引。
#[derive(clap::Args)]
struct IndexArgs {
    #[command(subcommand)]
    action: Option<IndexCmd>,
}

#[derive(Subcommand)]
enum IndexCmd {
    /// 登记一条索引（先写平台，再推内网节点）
    #[command(alias = "add")]
    Publish(index::PublishArgs),
    /// 列出索引（缺省列公开的；--mine 只看自己）
    List(index::ListArgs),
    /// 看一条索引：@我/slug 或 IX-…
    Show {
        reference: String,
        #[arg(long)]
        json: bool,
    },
    /// 撤回一条索引（原服务 / 制品不受影响）
    Rm { reference: String },
    /// 把已有索引再推一次到内网节点
    Push(index::PushArgs),
}

/// `ncc list` 子命令：索引里有什么。
#[derive(clap::Args)]
struct ListArgs {
    #[command(subcommand)]
    action: Option<ListCmd>,
}

#[derive(Subcommand)]
enum ListCmd {
    /// 已索引的人（供给方 / 需求方都在索引里，用 --side 区分）
    Users(index::UsersArgs),
    /// 索引里的**需求**（找活儿：看别人要什么）
    Needs(index::ListArgs),
    /// 频道清单（有哪些检索空间、各有多少条）
    Channels(index::ChannelsArgs),
}

#[derive(clap::Args)]
struct ServicesArgs {
    #[command(subcommand)]
    action: Option<ServicesCmd>,
}

/// 服务提供方把多条业务打包声明；其他 Agent 按匹配策略找到并接入。
#[derive(Subcommand)]
enum ServicesCmd {
    /// 浏览公开服务目录（--category / --tag / --region / --protocol / --q / --mine）
    List(services::ListArgs),
    /// 按意图匹配服务（Agent 主入口）：ncc services match "帮我订杭州的酒店"
    Match(services::MatchArgs),
    /// 看一条服务的完整接入信息：@提供方/slug 或 SV-… id
    Show(services::ShowArgs),
    /// 业务分类 / 接入方式 / 授权方式目录
    Catalog {
        #[arg(long)]
        json: bool,
    },
    /// 声明一条对外服务（提供方侧）
    Add(services::AddArgs),
    /// 下架一条服务（提供方侧）
    Rm(services::RmArgs),
}

#[derive(clap::Args)]
struct ProfileArgs {
    #[command(subcommand)]
    action: Option<ProfileCmd>,
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// 查看名片（缺省看自己；可指定用户名）
    Show { username: Option<String> },
    /// 列出工作角色目录（--roles 的取值来源）
    Roles {
        #[arg(long)]
        group: Option<String>,
    },
    /// 设置名片字段（先读后写，只覆盖显式给出的字段）
    Set(profile::SetArgs),
    /// 改用户名（短链与发布命名空间同步变更）
    Username { name: String },
    /// 作品集：list | add | rm
    #[command(subcommand)]
    Work(profile::WorkCmd),
    /// 关注某人（单向、幂等；--note 只有自己看得到）
    Follow(profile::FollowArgs),
    /// 取消关注
    Unfollow {
        /// 用户名（不带 @）
        handle: String,
    },
    /// 谁关注了我（可指定用户名看别人的粉丝）
    Followers {
        /// 省略则看自己
        handle: Option<String>,
    },
    /// 我关注的人（带只给自己看的备注）
    Following {
        /// 省略则看自己
        handle: Option<String>,
    },
    /// 给某人打分（1~5 星，一人一条，重复提交是覆盖）
    Rate(profile::RateArgs),
    /// 撤销自己给出的评价
    Unrate {
        /// 用户名（不带 @）
        handle: String,
    },
    /// 某人收到的评价（均分 + 分布 + 评语）
    Ratings {
        /// 省略则看自己收到的
        handle: Option<String>,
    },
}

/// `ncc registry` 子命令。
///
/// 把一个内网 ncc-registry 当成「托管节点 + 制品仓库 + 配置库 + Agent 目录」来用：
/// 登录它、把本机托管进去、看集群、发现别人的节点、查聚合目录、问能力路由，
/// 以及托管配置、分享制品、当管理员治理用户 / 节点 / 服务。
#[derive(Subcommand)]
enum RegistryCmd {
    /// 用接入短链（或 key/secret）把内网 registry 接进来
    Add(registryadd::AddArgs),
    /// 登入一个 ncc-registry 节点（用 --base 指定地址）
    Login(registry::LoginArgs),
    /// 把本机作为一个节点托管进去（注册 + 心跳；--daemon 常驻）
    Join(registry::JoinArgs),
    /// 看本节点、集群（master/worker）、我的托管节点
    Status(registry::StatusArgs),
    /// 发现本实例上可连接的节点（Agent 发现）
    Nodes(registry::NodesArgs),
    /// 聚合目录：本节点 + 各 worker 的制品
    Catalog(registry::CatalogArgs),
    /// 这个能力该找哪个节点要（能力路由）
    Route {
        /// 制品引用：@命名空间/slug 或 A-… id
        target: String,
    },
    /// 接入票据：签发 / 列出 / 删除（给对方 key+secret 或一条内网短链）
    Ticket(registryadd::TicketCmd),
    /// 配置托管：list | get | set | history | rollback | bundle | kinds | rm
    Config(configs::ConfigCmd),
    /// 分享链接：create | list | rm | info（发给别人，对方不用登录）
    Share(admin::ShareArgs),
    /// 节点管理：用户 / 节点 / 服务（需管理员账号或 admin key/secret）
    Admin(admin::AdminArgs),
    /// 把制品分发到 worker（副本）：--to all|<名称,名称>
    Replicate(registryadd::ReplicateArgs),
    /// 打洞条件与入口：self（节点侧 NAT 画像）| check（从节点对打）| serve（开/关可被打洞入口）
    P2p(registryp2p::P2pArgs),
    /// 下架制品并回收各节点上的副本（需 --yes）
    Rm(registryadd::RmArgs),
    /// 下线我的节点（下次心跳会重新注册）
    Leave(registry::LeaveArgs),
}

#[derive(Subcommand)]
enum NsCmd {
    List,
    /// 列出组织计划目录（含尚未开放的档：看得见，选不了）
    Plans,
    Create {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: Option<String>,
        /// 组织计划 id（默认 free；服务端只认目录里的，且必须是已开放的档）
        #[arg(long, default_value = "free")]
        plan: String,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// 列出所有 key（含类型 / 作用域 / 命名空间 / 过期时间）
    List,
    /// 创建 key：ncc key create --label ci [--kind distribution --ns @you --scopes registry:read,registry:download] [--expires 30]
    Create(KeyCreateArgs),
    /// 吊销 key
    Revoke { id: String },
    /// 列出可用作用域
    Scopes,
}

#[derive(clap::Args)]
struct KeyCreateArgs {
    /// 备注名（如 ci / 给某客户的分发）
    #[arg(long)]
    label: Option<String>,
    /// 类型：user（通用）| distribution（只用于分发制品给他人）
    #[arg(long, default_value = "user", value_parser = ["user", "distribution"])]
    kind: String,
    /// 作用域，逗号分隔（缺省按 kind 取默认值；见 `ncc key scopes`）
    #[arg(long)]
    scopes: Option<String>,
    /// 限定可访问的命名空间，可重复：--ns @you --ns @team（缺省=不限）
    #[arg(long = "ns", value_name = "@slug")]
    namespaces: Vec<String>,
    /// 备忘（给谁用、做什么）
    #[arg(long)]
    note: Option<String>,
    /// 有效天数（缺省=长期有效）
    #[arg(long)]
    expires: Option<u32>,
}

#[derive(clap::Args)]
struct LivingArgs {
    /// 守护模式：按间隔周期上报（Ctrl+C 停止）
    #[arg(long)]
    daemon: bool,
    /// 守护模式上报间隔（秒）
    #[arg(long, default_value_t = 15)]
    interval: u64,
    /// 设备名（默认：HOSTNAME 或 os-arch）
    #[arg(long)]
    name: Option<String>,
    /// 节点声明类型：service（服务）| agent（为人服务的 Agent）| assigned（被分配的 Agent）
    #[arg(long, default_value = "service", value_parser = ["service", "agent", "assigned"])]
    kind: String,
    /// 设备 slug（默认由 name 生成）
    #[arg(long)]
    slug: Option<String>,
    /// 对外地址 url（可选，便于他人直连）
    #[arg(long)]
    url: Option<String>,
    /// 本设备可提供的能力（提供能力），逗号分隔；取值见 `ncc nodes offers`
    ///
    /// 规范 id 形如 run:wasm / egress:llm / serve:mcp；历史短名（mcp,api,wasm,llm…）照旧可用。
    /// 别人按这些能力找你：`ncc nodes discover --can egress:llm`。
    #[arg(long)]
    capabilities: Option<String>,
}

/// ncc terminal 动作（缺省进入交互命令台）
#[derive(Subcommand)]
enum TermAction {
    /// 打印 POSIX 运行时 / 环境状态
    Status,
    /// 装配 POSIX 运行时（Windows：WSL2/MSYS2；Unix：原生）
    Setup,
}

#[derive(clap::Args)]
struct PublishArgs {
    /// 本地文件路径（上传字节）
    #[arg(long)]
    file: Option<String>,
    /// 直链（BYO storage url）
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    name: String,
    #[arg(long)]
    slug: Option<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    summary: Option<String>,
    #[arg(long)]
    tags: Option<String>,
    /// harness 封装契约 JSON（kind=harness，含 harness.loader/entry）
    #[arg(long)]
    manifest: Option<String>,
    /// 目标 namespace slug（默认个人命名空间）
    #[arg(long)]
    namespace: Option<String>,
    /// 可见性 public/private（private 需 Pro 付费）
    #[arg(long, default_value = "public", value_parser = ["public", "private"])]
    visibility: String,
    /// 以 draft 状态创建（默认 published）
    #[arg(long)]
    draft: bool,
    /// 发布后把副本分发到 worker：all 或名称/id（逗号分隔）；仅 ncc-registry 支持
    #[arg(long)]
    replicate: Option<String>,
}

#[derive(clap::Args)]
struct SearchArgs {
    /// 关键词
    query: Option<String>,
    #[arg(long)]
    kind: Option<String>,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    namespace: Option<String>,
    #[arg(long)]
    mine: bool,
}

fn main() {
    // `ncc hub …` 是「本次用云端目标」的糖：在交给 clap 之前把 hub 摘掉。
    // 这样 ncc hub services match "…" 与 ncc --target hub services match "…" 等价，
    // 而不需要把整棵命令树再抄一份到 hub 底下。
    let (args, hub_prefix) = strip_hub_prefix(std::env::args().collect());
    // `ncc hub` 单独出现（没有子命令）= 看云端目标的状态
    if hub_prefix && args.len() <= 1 {
        let mut cfg = config::load();
        if let Err(e) = resolve_target(
            &mut cfg,
            &Cli {
                base: None,
                target: None,
                auth: false,
                cmd: Cmd::Target(target::TargetArgs { action: None }),
            },
            hub_prefix,
        ) {
            eprintln!("✗ {:#}", e);
            std::process::exit(1);
        }
        let action = Cmd::Target(target::TargetArgs {
            action: Some(target::TargetAction::Show {
                name: target::hub_target_name(&cfg),
                json: false,
            }),
        });
        if let Err(e) = run(&mut cfg, &action) {
            eprintln!("✗ {:#}", e);
            std::process::exit(1);
        }
        return;
    }
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            // 解析失败时按 clap 自己的输出走（含 help / version）
            e.exit();
        }
    };

    let mut cfg = config::load();
    // 告诉 hur-core「这个二进制里有什么执行引擎」：注册之后 `ncc hur run` 的计划才会
    // 说 engine=wasm 且 engine_ready=true（否则计划如实标注本机没有该引擎，
    // `--exec` 也就无从跑起）。放在这里是因为 CLI 与 `ncc mcp` 都要看到同一份事实。
    #[cfg(feature = "sandbox")]
    hur_core::policy::register_engines(hur_sandbox::ENGINES);
    // 先判定协议模式，再决定提示走哪个流：resolve_target 里的提示也算。
    if matches!(cli.cmd, Cmd::Mcp(_)) {
        PROTOCOL_STDOUT.store(true, Ordering::Relaxed);
    }
    // `--auth`：这条命令以「被授权的 Agent」身份跑（用对外令牌而不是会话）。
    // 在 resolve_target 之前设定，因为目标解析本身也可能发请求。
    config::set_auth_mode(cli.auth);
    if let Err(e) = resolve_target(&mut cfg, &cli, hub_prefix) {
        eprintln!("✗ {:#}", e);
        std::process::exit(1);
    }

    let result = run(&mut cfg, &cli.cmd);
    if let Err(e) = result {
        eprintln!("✗ {:#}", e);
        std::process::exit(1);
    }
}

/// 把 `ncc hub …` 里的 `hub` 摘掉（只认第一个位置参数）。
fn strip_hub_prefix(args: Vec<String>) -> (Vec<String>, bool) {
    let mut out = Vec::with_capacity(args.len());
    let mut hub = false;
    for (i, a) in args.into_iter().enumerate() {
        if i == 1 && a == "hub" {
            hub = true;
            continue;
        }
        out.push(a);
    }
    (out, hub)
}

/// 决定这次命令跑在哪个目标上：
///
///	--target <名字>   **本次**命令用这个目标（不改默认；要改默认用 ncc target use）
///	--base <URL>      用这个地址：已存在同名目标就复用，否则新建一个目标（**会**保存，并提示）
///	ncc hub …         **本次**用云端目标
///	什么都不给        用配置里的当前目标
fn resolve_target(cfg: &mut CliConfig, cli: &Cli, hub_prefix: bool) -> anyhow::Result<()> {
    if let Some(name) = &cli.target {
        config::set_current(cfg, name)?; // 只改内存，不写盘
    }
    if hub_prefix && cli.target.is_none() {
        match target::hub_target_name(cfg) {
            Some(n) => config::set_current(cfg, &n)?,
            None => {
                anyhow::bail!(
                    "还没有云端目标。新建：ncc target add hub --base https://ncc.ai（或先 ncc target list 看一眼）"
                );
            }
        }
    }
    if let Some(b) = &cli.base {
        let base = b.trim_end_matches('/').to_string();
        match cfg.name_of_base(&base) {
            Some(existing) => {
                // 同一台机器已经有目标了：切过去，而不是再造一个
                if existing != cfg.current_name() {
                    config::set_current(cfg, &existing)?;
                    note(&format!("（--base 命中已有目标 {existing}，本次已切到它）"));
                }
            }
            None => {
                // 新地址：建一个新目标并切过去。**不会覆盖**已有的云端目标与凭据。
                let mut probe_cfg = cfg.clone();
                probe_cfg.targets.insert(
                    "probe".into(),
                    config::Target {
                        base_url: base.clone(),
                        ..config::Target::default()
                    },
                );
                probe_cfg.current = Some("probe".into());
                let m = capability::probe(&probe_cfg);
                // 名字撞上已有目标就换一个：**绝不覆盖**（覆盖会抹掉那台机器上的登录态）。
                let name = target::free_name(cfg, &target::suggest_name_for(cfg, &base, &m.kind));
                cfg.targets.insert(
                    name.clone(),
                    config::Target {
                        kind: match m.kind.as_str() {
                            "node" => "registry".to_string(),
                            "hub" => "cloud".to_string(),
                            _ => {
                                if base.contains("ncc.ai") {
                                    "cloud".into()
                                } else {
                                    "registry".into()
                                }
                            }
                        },
                        base_url: base.clone(),
                        ..config::Target::default()
                    },
                );
                config::set_current(cfg, &name)?;
                config::save(cfg)?;
                note(&format!(
                    "（--base {base} 已作为新目标 {name} 保存并切换；以后直接 ncc target use {name}）"
                ));
                // 地址打错时当场提醒一次，而不是等下一次命令才发现。
                if m.product.is_empty() {
                    if let Some(note) = &m.note {
                        eprintln!("⚠️  这个地址现在认不出服务：{note}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// 命令 → 它需要的能力。返回 None 表示不需要服务器（本地命令）。
///
/// 只声明「明确属于某个能力」的命令；账号类（register/login/me/ns/key）两边都有，不管。
fn required_capability(cmd: &Cmd) -> Option<&'static str> {
    match cmd {
        Cmd::Hur(h) => h.capability(),
        Cmd::Publish(_)
        | Cmd::Search(_)
        | Cmd::Info { .. }
        | Cmd::Download { .. }
        | Cmd::Install { .. } => Some("registry"),
        Cmd::Living(_) => Some("living"),
        // Agent 名片是 Share 面的一条能力（平台/节点都靠 share 声明这一族接口）。
        Cmd::Agent(_) => Some("share"),
        // 云电脑：不门禁（见 run() 里的说明）。
        Cmd::Sandbox(_) => None,
        // 连接通道：同样不门禁（地址与凭据来自本地登记，连通性由 open/status 自己报）。
        Cmd::Conn(_) => None,
        // RSI 一律本地：策略、账本、偏好、目标都是文件，不依赖服务端能力面。
        Cmd::Rsi(_) => None,
        // 纯本地：打洞预检一样不依赖 NCC 服务端。
        Cmd::Gateway(_) => None,
        // 舱的命令自己报告能力（doctor 的职责就是回答「这个目标有没有这些能力」），不在这里门禁
        Cmd::App(_) => None,
        // 加签是**本地**动作（私钥不出设备）；核对也不依赖服务端声明的能力面，
        // 所以两者都不在这里门禁。
        Cmd::Sign(_) | Cmd::Verify(_) => None,
        Cmd::Trace(t) => t.capability(),
        // 三样状态各是一个能力（节点可以只托管知识库、不托管记忆）。
        Cmd::Kb(k) => k.capability(),
        Cmd::Mem(m) => m.capability(),
        Cmd::Ckpt(c) => c.capability(),
        Cmd::Store(s) => s.capability(),
        // 反馈：词表与本地队列是本地的事（`FeedbackCmd::capability` 自己答）。
        Cmd::Feedback(f) => f.capability(),
        Cmd::Profile(_) => Some("profile"),
        Cmd::Nodes(_) => Some("nodes"),
        Cmd::Services(_) => Some("services"),
        Cmd::Index(_) | Cmd::List(_) | Cmd::Match(_) => Some("index"),
        Cmd::Grant(_) => Some("grants"),
        // 对外授权颁发方（OIDC / device flow）只在目标声明 auth 时才放行。
        //
        // ⚠️ 例外：**授权包**（`ncc auth pkg`）完全在本地 —— 包是文件、锁是文件、
        // 加解密在客户端，一个字节都不发给服务端。所以它**不门禁**（在离线机器上
        // 也必须能用，否则"把凭据封进包里发给同事"这件事就依赖服务端了）。
        Cmd::Auth { action } => match action {
            auth::AuthCmd::Pkg { .. } => None,
            // 离线生成身份密钥同样不碰服务端（授权包用得上，离线机器也要能用）
            auth::AuthCmd::Key { action: auth::KeyCmd::New { offline: true, .. } } => None,
            _ => Some("auth"),
        },
        Cmd::P2p(p) => match p {
            // 预检纯本地（要 STUN，但不要 NCC 服务端）：老服务端/离线环境也应当能用。
            p2p::P2pCmd::Probe(_) => None,
            _ => Some("p2p"),
        },
        Cmd::Registry(r) => match r {
            RegistryCmd::Config(_) => Some("config"),
            RegistryCmd::Admin(_) => Some("admin"),
            RegistryCmd::Share(_) => Some("share"),
            RegistryCmd::Ticket(_) | RegistryCmd::Add(_) => Some("access"),
            RegistryCmd::Catalog(_) | RegistryCmd::Route { .. } => Some("cluster"),
            RegistryCmd::Join(_)
            | RegistryCmd::Status(_)
            | RegistryCmd::Nodes(_)
            | RegistryCmd::Leave(_) => Some("nodes"),
            RegistryCmd::Login(_) | RegistryCmd::Replicate(_) | RegistryCmd::Rm(_) => {
                Some("registry")
            }
            RegistryCmd::P2p(_) => Some("p2p"),
        },
        _ => None,
    }
}

fn run(cfg: &mut CliConfig, cmd: &Cmd) -> anyhow::Result<()> {
    // 能力门禁：目标声明了这份清单且其中没有该能力 → 直接说清「该切到哪儿」，
    // 而不是把 404 丢给用户。未知（老服务端没声明）则放行。
    if let Some(cap) = required_capability(cmd) {
        capability::ensure(cfg, cap)?;
    }
    // `ncc registry …` 只在内网节点目标上跑
    if matches!(cmd, Cmd::Registry(_)) && !cfg.target().is_node() {
        let nodes = cfg.node_names();
        anyhow::bail!(
            "当前目标是 {}（{} · {}）——`ncc registry …` 要在内网节点目标上跑\n  节点目标：{}",
            cfg.current_name(),
            cfg.target().kind_label(),
            cfg.base_url(),
            if nodes.is_empty() {
                "（还没有）新建一个：ncc target add office --base http://<内网 IP>:8282".to_string()
            } else {
                format!("{}（ncc target use <名字>）", nodes.join(" · "))
            }
        );
    }
    match cmd {
        Cmd::Register {
            email,
            password,
            name,
            invite,
        } => cmd_register(cfg, email, password, name.as_deref(), invite.as_deref()),
        Cmd::Login {
            email,
            password,
            api_key,
        } => cmd_login(cfg, email, password, api_key),
        Cmd::Logout => {
            let name = cfg.current_name();
            config::clear_session(cfg)?;
            println!("已登出目标 {name}（其它目标的登录态不受影响）");
            Ok(())
        }
        Cmd::Me => cmd_me(cfg),
        Cmd::Ns(n) => cmd_ns(cfg, n),
        Cmd::Publish(a) => cmd_publish(cfg, a),
        Cmd::Search(a) => cmd_search(cfg, a),
        Cmd::Info { reference } => {
            let token = config::require_token(cfg).ok();
            let path = format!("/api/registry/{}", reference);
            let data = api::get(cfg, &path, token.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&data["item"])?);
            Ok(())
        }
        Cmd::Download { reference, out } => cmd_download(cfg, reference, out.as_deref()),
        Cmd::Install {
            reference,
            dir,
            force,
        } => cmd_install(cfg, reference, dir.as_deref(), *force),
        Cmd::Terminal { action } => match action {
            Some(TermAction::Status) => {
                println!("{}", terminal::status(cfg));
                Ok(())
            }
            Some(TermAction::Setup) => terminal::setup(cfg),
            None => {
                // 真实终端 → 全屏 TUI；非 TTY（管道/CI）→ REPL
                if std::io::stdin().is_terminal() {
                    tui::run(cfg)
                } else {
                    terminal::run(cfg)
                }
            }
        },
        Cmd::Upgrade(a) => upgrade::run(a),
        Cmd::Hur(h) => hur::run(cfg, h),
        Cmd::Sign(s) => signcmd::sign(cfg, s),
        Cmd::Verify(v) => signcmd::verify(cfg, v),
        Cmd::Key(k) => cmd_key(cfg, k),
        Cmd::Living(a) => cmd_living(cfg, a),
        Cmd::Agent(a) => agent::run(cfg, a),
        // 云电脑的地址来自本机环境表（不是当前目标）——连通性由 init/status 自己报，
        // 所以这里不做能力门禁（否则当前目标正好是个没声明 exec 的节点就全哑了）。
        Cmd::Sandbox(s) => sandbox::cmd(&s.action),
        Cmd::Conn(s) => conn::cmd(&s.action),
        // RSI 全在本地（策略/账本/偏好/目标都是文件）：不门禁、离线也要能拦人。
        // 退出码 0/10/20 是**给宿主看的裁决**，直接透出去。
        Cmd::Rsi(a) => {
            let mut code = None;
            rsi::cmd(&a.action, &mut code)?;
            match code {
                Some(c) => std::process::exit(c),
                None => Ok(()),
            }
        }
        Cmd::Trace(t) => trace::run(cfg, t),
        Cmd::Kb(k) => state::run_kb(cfg, k),
        Cmd::Mem(m) => state::run_mem(cfg, m),
        Cmd::Ckpt(c) => state::run_ckpt(cfg, c),
        Cmd::Store(s) => store::run(cfg, s),
        Cmd::Feedback(f) => feedback::cmd(f),
        // Gateway 是纯本地命令（不需要服务器）：不查能力面，也不读写 NCC 配置。
        Cmd::App(a) => match a {
            AppCmd::Init {
                dir,
                name,
                namespace,
                share_target,
                port,
                force,
                json,
            } => app::init(
                cfg,
                &app::InitArgs {
                    dir: dir.clone(),
                    name: name.clone(),
                    namespace: namespace.clone(),
                    share_target: share_target.clone(),
                    port: *port,
                    force: *force,
                    json: *json,
                },
            ),
            AppCmd::Doctor { dir, json } => app::doctor(
                cfg,
                &app::DoctorArgs {
                    dir: dir.clone(),
                    json: *json,
                },
            ),
            AppCmd::Up { dir, port } => app::up(
                cfg,
                &app::UpArgs {
                    dir: dir.clone(),
                    port: *port,
                },
            ),
            AppCmd::Status { dir, json } => app::status(
                cfg,
                &app::StatusArgs {
                    dir: dir.clone(),
                    json: *json,
                },
            ),
            AppCmd::Export { dir, out, json } => app::export(
                cfg,
                &app::ExportArgs {
                    dir: dir.clone(),
                    out: out.clone(),
                    json: *json,
                },
            ),
        },
        Cmd::Gateway(g) => match g {
            GatewayCmd::Init { force } => gateway::init(*force),
            GatewayCmd::Check => gateway::check(),
            GatewayCmd::Run => gateway::run(),
            GatewayCmd::Status => gateway::status(),
            GatewayCmd::Switch { route, provider } => gateway::switch(route, provider),
            GatewayCmd::Audit {
                tail,
                json,
                remote,
                since,
                limit,
                csv,
            } => {
                if *remote {
                    gwreport::audit_remote(
                        cfg,
                        &gwreport::RemoteAuditArgs {
                            since: since.clone(),
                            limit: *limit,
                            csv: *csv,
                            json: *json,
                        },
                    )
                } else {
                    gateway::audit_cmd(*tail, *json)
                }
            }
            GatewayCmd::Bind {
                namespace,
                name,
                version,
                json,
            } => gwreport::bind(
                cfg,
                &gwreport::BindArgs {
                    namespace: namespace.clone(),
                    name: name.clone(),
                    version: version.clone(),
                    json: *json,
                },
            ),
            GatewayCmd::Heartbeat { status, json } => gwreport::heartbeat(
                cfg,
                &gwreport::HeartbeatArgs {
                    status: status.clone(),
                    json: *json,
                },
            ),
            GatewayCmd::Report {
                since,
                window_minutes,
                dry_run,
                drop_rejected,
                json,
            } => gwreport::report(
                cfg,
                &gwreport::ReportArgs {
                    since: since.clone(),
                    window_minutes: *window_minutes,
                    dry_run: *dry_run,
                    drop_rejected: *drop_rejected,
                    json: *json,
                },
            ),
            GatewayCmd::Usage { json } => gwreport::usage(cfg, *json),
            GatewayCmd::Unbind { purge_state, json } => gwreport::unbind(cfg, *purge_state, *json),
        },
        Cmd::Profile(p) => match &p.action {
            None | Some(ProfileCmd::Show { username: None }) => profile::show(cfg, None),
            Some(ProfileCmd::Show { username: Some(u) }) => profile::show(cfg, Some(u.as_str())),
            Some(ProfileCmd::Roles { group }) => profile::roles(cfg, group.as_deref()),
            Some(ProfileCmd::Set(a)) => profile::set(cfg, a),
            Some(ProfileCmd::Username { name }) => profile::set_username(cfg, name),
            Some(ProfileCmd::Work(cmd)) => profile::work(cfg, cmd),
            Some(ProfileCmd::Follow(a)) => profile::follow(cfg, a),
            Some(ProfileCmd::Unfollow { handle }) => profile::unfollow(cfg, handle),
            Some(ProfileCmd::Followers { handle }) => profile::followers(cfg, handle.as_deref()),
            Some(ProfileCmd::Following { handle }) => profile::following(cfg, handle.as_deref()),
            Some(ProfileCmd::Rate(a)) => profile::rate(cfg, a),
            Some(ProfileCmd::Unrate { handle }) => profile::unrate(cfg, handle),
            Some(ProfileCmd::Ratings { handle }) => profile::ratings(cfg, handle.as_deref()),
        },
        Cmd::Nodes(n) => match &n.action {
            None => nodes::list(
                cfg,
                &nodes::ListArgs {
                    kind: None,
                    q: None,
                    can: Vec::new(),
                },
            ),
            Some(NodesCmd::List(a)) => nodes::list(cfg, a),
            Some(NodesCmd::Kinds) => nodes::kinds(cfg),
            Some(NodesCmd::Offers) => nodes::offers(cfg),
            Some(NodesCmd::Discover(a)) => nodes::discover(cfg, a),
            Some(NodesCmd::Link(a)) => nodes::link(cfg, a),
            Some(NodesCmd::Label(a)) => nodes::label(cfg, a),
            Some(NodesCmd::Unlink { id }) => nodes::unlink(cfg, id),
            Some(NodesCmd::Region) => nodes::region(cfg),
            Some(NodesCmd::Recommend(a)) => nodes::recommend(cfg, a),
        },
        Cmd::Grant(g) => match g {
            GrantCmd::List(a) => nodes::grant_list(cfg, a),
            GrantCmd::Set(a) => nodes::grant_set(cfg, a),
            GrantCmd::Rm { id } => nodes::grant_rm(cfg, id),
        },
        // NCC Auth：NCC 当**授权颁发方**（第三方平台用 NCC 登录 + Agent 用凭据接入）。
        // 能力位 `auth` —— 关着的时候服务端一律 404，命令面给的也是可读的报错。
        Cmd::Auth { action } => auth::run(cfg, action),
        Cmd::P2p(p) => match p {
            p2p::P2pCmd::Probe(a) => p2p::probe(cfg, &a),
            p2p::P2pCmd::Ice => p2p::ice(cfg),
            p2p::P2pCmd::Check(a) => p2p::check(cfg, &a),
            p2p::P2pCmd::Signal(s) => p2p::signal(cfg, &s),
            p2p::P2pCmd::Ticket(t) => p2p::ticket(cfg, &t),
        },
        Cmd::Index(v) => match &v.action {
            // 不带子命令：先看自己的 —— 这是「我刚登记了什么」的常见问题
            None => index::list(
                cfg,
                &index::ListArgs {
                    mine: true,
                    ..Default::default()
                },
            ),
            Some(IndexCmd::Publish(a)) => index::publish(cfg, a),
            Some(IndexCmd::List(a)) => index::list(cfg, a),
            Some(IndexCmd::Show { reference, json }) => index::show(cfg, reference, *json),
            Some(IndexCmd::Rm { reference }) => index::rm(cfg, reference),
            Some(IndexCmd::Push(a)) => index::push(cfg, a),
        },
        Cmd::List(v) => match &v.action {
            // 不带子命令：看索引里的人（大多数人想问的就是这个）
            None => index::users(
                cfg,
                &index::UsersArgs {
                    channel: None,
                    kind: None,
                    side: None,
                    q: None,
                    limit: 30,
                    json: false,
                },
            ),
            Some(ListCmd::Users(a)) => index::users(cfg, a),
            Some(ListCmd::Needs(a)) => index::list(
                cfg,
                &index::ListArgs {
                    side: Some("need".into()),
                    ..a.clone()
                },
            ),
            Some(ListCmd::Channels(a)) => index::channels(cfg, a),
        },
        Cmd::Match(a) => index::match_intent(cfg, a),
        Cmd::Services(v) => match &v.action {
            None => services::list(
                cfg,
                &services::ListArgs {
                    category: None,
                    tag: None,
                    region: None,
                    protocol: None,
                    q: None,
                    mine: false,
                    limit: 20,
                    json: false,
                },
            ),
            Some(ServicesCmd::List(a)) => services::list(cfg, a),
            Some(ServicesCmd::Match(a)) => services::match_intent(cfg, a),
            Some(ServicesCmd::Show(a)) => services::show(cfg, a),
            Some(ServicesCmd::Catalog { json }) => services::catalog(cfg, *json),
            Some(ServicesCmd::Add(a)) => services::add(cfg, a),
            Some(ServicesCmd::Rm(a)) => services::rm(cfg, a),
        },
        Cmd::Registry(r) => match r {
            RegistryCmd::Add(a) => registryadd::add(cfg, a),
            RegistryCmd::Login(a) => registry::login(cfg, a),
            RegistryCmd::Join(a) => registry::join(cfg, a),
            RegistryCmd::Status(a) => registry::status(cfg, a),
            RegistryCmd::Nodes(a) => registry::nodes(cfg, a),
            RegistryCmd::Catalog(a) => registry::catalog(cfg, a),
            RegistryCmd::Route { target } => registry::route(cfg, target),
            RegistryCmd::Ticket(t) => registryadd::ticket(cfg, t),
            RegistryCmd::Config(c) => match &c.action {
                configs::ConfigAction::List(a) => configs::list(cfg, a),
                configs::ConfigAction::Get(a) => configs::get(cfg, a),
                configs::ConfigAction::Set(a) => configs::set(cfg, a),
                configs::ConfigAction::History { target, json } => {
                    configs::history(cfg, target, *json)
                }
                configs::ConfigAction::Rollback(a) => configs::rollback(cfg, a),
                configs::ConfigAction::Bundle(a) => configs::bundle(cfg, a),
                configs::ConfigAction::Kinds { json } => configs::kinds(cfg, *json),
                configs::ConfigAction::Rm(a) => configs::rm(cfg, a),
            },
            RegistryCmd::Replicate(a) => registryadd::replicate(cfg, a),
            RegistryCmd::Rm(a) => registryadd::rm(cfg, a),
            RegistryCmd::P2p(a) => registryp2p::p2p(cfg, a),
            RegistryCmd::Share(s) => admin::run_share(cfg, s),
            RegistryCmd::Admin(a) => admin::run(cfg, a),
            RegistryCmd::Leave(a) => registry::leave(cfg, a),
        },
        Cmd::Target(t) => target::run(cfg, t),
        Cmd::Mcp(a) => mcp::serve(
            cfg,
            &mcp::McpOptions {
                package: a.package.clone(),
            },
        ),
    }
}

/* ---------------- 账号 ---------------- */
// 登录态写进**当前目标**：在本地 registry 登录不会把云端的会话挤掉（多目标的意义所在）。
fn save_session(cfg: &mut CliConfig, token: &str, email: &str, name: &str) -> anyhow::Result<()> {
    config::save_session(cfg, token, email, name)
}

fn cmd_register(
    cfg: &mut CliConfig,
    email: &str,
    password: &str,
    name: Option<&str>,
    invite: Option<&str>,
) -> anyhow::Result<()> {
    // 邀请码：--invite 优先，其次环境变量 NCC_INVITE_CODE（服务端未启用门禁时可留空）
    let invite = invite
        .map(|s| s.to_string())
        .or_else(|| std::env::var("NCC_INVITE_CODE").ok());
    let body = json!({ "email": email, "password": password, "name": name, "inviteCode": invite });
    let data = api::post_json(cfg, "/api/auth/register", None, &body)?;
    let token = data["token"].as_str().context("响应缺少 token")?;
    let u = &data["user"];
    save_session(
        cfg,
        token,
        u["email"].as_str().unwrap_or(email),
        u["name"].as_str().unwrap_or(name.unwrap_or("")),
    )?;
    println!(
        "✅ 注册成功：{}（token 已保存到 {}）",
        u["email"].as_str().unwrap_or(email),
        config::config_path().display()
    );
    // 内网 registry 的第一个账号自动成为节点管理员，并在这里拿到机器用 admin key/secret。
    // secret 只在注册响应里出现一次 —— 不打印就等于让用户永久失去它。
    if let Some(admin) = data.get("admin") {
        if let Some(key) = admin["key"].as_str() {
            println!("\n👑 你是本节点的第一个账号 → 自动成为管理员");
            println!("   admin key      {key}");
            println!(
                "   admin secret   {}",
                admin["secret"].as_str().unwrap_or("")
            );
            println!("   （secret 只显示这一次，请立刻保存；写进本机配置：）");
            println!("   ncc registry admin login --key {key} --secret <上面的 secret>");
        }
    }
    Ok(())
}

fn cmd_login(
    cfg: &mut CliConfig,
    email: &str,
    password: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    // API-Key 直接当 token 存进当前目标的登录态：机器人 / 桌面端绑定用，不必知道密码。
    // 身份（email/name）尽力用 /api/auth/me 补全，取不到也不影响用。
    if !api_key.trim().is_empty() {
        let token = api_key.trim().to_string();
        let (mail, name) = match api::get(cfg, "/api/auth/me", Some(&token)) {
            Ok(d) => (
                d["user"]["email"].as_str().unwrap_or(email).to_string(),
                d["user"]["name"].as_str().unwrap_or("").to_string(),
            ),
            Err(_) => (email.to_string(), String::new()),
        };
        save_session(cfg, &token, &mail, &name)?;
        println!(
            "✅ 已用 API-Key 登录{}（token 已保存到 {}）",
            if mail.is_empty() {
                "（身份未取到）".to_string()
            } else {
                format!("：{mail}")
            },
            config::config_path().display()
        );
        return Ok(());
    }
    if email.trim().is_empty() || password.is_empty() {
        anyhow::bail!("请给 `--email` + `--password`，或直接 `--api-key <key>`");
    }
    let body = json!({ "email": email, "password": password });
    let data = api::post_json(cfg, "/api/auth/login", None, &body)?;
    let token = data["token"].as_str().context("响应缺少 token")?;
    let u = &data["user"];
    save_session(
        cfg,
        token,
        u["email"].as_str().unwrap_or(email),
        u["name"].as_str().unwrap_or(""),
    )?;
    println!("✅ 登录成功：{}", u["email"].as_str().unwrap_or(email));
    Ok(())
}

fn cmd_me(cfg: &CliConfig) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    let data = api::get(cfg, "/api/auth/me", Some(&token))?;
    let u = &data["user"];
    println!(
        "用户: {} <{}>  ({})",
        u["name"].as_str().unwrap_or(""),
        u["email"].as_str().unwrap_or(""),
        u["id"].as_str().unwrap_or("")
    );
    println!("计划: {}", u["plan"].as_str().unwrap_or("free"));
    println!("Namespaces:");
    if let Some(ns) = data["namespaces"].as_array() {
        for n in ns {
            let ty = if n["type"] == "org" { "🏢" } else { "👤" };
            let owner = if n["owner"].as_bool().unwrap_or(false) {
                "owner"
            } else {
                "member"
            };
            println!(
                "  {} {}  {}  {}",
                ty,
                n["slug"].as_str().unwrap_or(""),
                owner,
                n["visibility"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

/* ---------------- 命名空间 ---------------- */
fn cmd_ns(cfg: &CliConfig, n: &NsCmd) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    match n {
        NsCmd::List => {
            let data = api::get(cfg, "/api/namespaces/mine", Some(&token))?;
            if let Some(ns) = data["namespaces"].as_array() {
                for x in ns {
                    let ty = if x["type"] == "org" { "🏢" } else { "👤" };
                    // 组织才打计划：个人空间固定 free，打出来只是噪声
                    let plan = if x["type"] == "org" {
                        format!("{}", x["plan"].as_str().unwrap_or("free"))
                    } else {
                        "-".to_string()
                    };
                    println!(
                        "{} {}\t{}\t{}\t{}\t{}",
                        ty,
                        x["slug"].as_str().unwrap_or(""),
                        x["name"].as_str().unwrap_or(""),
                        if x["owner"].as_bool().unwrap_or(false) {
                            "owner"
                        } else {
                            "member"
                        },
                        x["visibility"].as_str().unwrap_or(""),
                        plan
                    );
                }
            }
            Ok(())
        }
        NsCmd::Plans => {
            // 目录来自服务端（单一真源）：CLI 不另抄一份计划表。
            // 登录时顺手把 token 带上 —— 目录本身公开，但带上才看得到
            // 「我已用几个 / 还能建几个」那两行。
            let tk = cfg.target().token.clone().filter(|t| !t.is_empty());
            let data = api::get(cfg, "/api/namespaces/plans", tk.as_deref())?;
            if let Some(plans) = data["plans"].as_array() {
                for p in plans {
                    let avail = p["available"].as_bool().unwrap_or(false);
                    let mark = if avail { "✓" } else { "·" };
                    println!(
                        "{} {:<6} {:<10} {} 个组织 / 每组织 {} 人  {}",
                        mark,
                        p["id"].as_str().unwrap_or(""),
                        p["nameZh"].as_str().unwrap_or(""),
                        p["maxOrgs"].as_i64().unwrap_or(0),
                        p["maxMembers"].as_i64().unwrap_or(0),
                        p["priceZh"].as_str().unwrap_or("")
                    );
                    if !avail {
                        if let Some(note) = p["noteZh"].as_str() {
                            if !note.is_empty() {
                                println!("    └ 未开放：{note}");
                            }
                        }
                    }
                }
            }
            if let Some(used) = data["used"].as_i64() {
                println!(
                    "已拥有 {} / {} 个组织（额度按所有者算）",
                    used,
                    data["quota"].as_i64().unwrap_or(0)
                );
            }
            println!("计划里写着但选不了的档 = 还没开放；开放在服务端目录里改。");
            Ok(())
        }
        NsCmd::Create { slug, name, plan } => {
            let body = json!({ "slug": slug, "name": name, "plan": plan });
            let data = api::post_json(cfg, "/api/namespaces", Some(&token), &body)?;
            println!(
                "✅ namespace 创建：{} ({})　计划 {}",
                data["slug"].as_str().unwrap_or(&slug),
                data["id"].as_str().unwrap_or(""),
                data["plan"].as_str().unwrap_or(&plan)
            );
            Ok(())
        }
    }
}

/* ---------------- 发布 / 检索 / 下载 ---------------- */
fn cmd_publish(cfg: &CliConfig, a: &PublishArgs) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    if a.kind.is_empty() || a.name.is_empty() {
        bail!("需要 --kind 与 --name");
    }
    if a.file.is_none() && a.url.is_none() {
        bail!("需提供 --file（上传）或 --url（直链）");
    }

    let mut storage_url = a.url.clone().unwrap_or_default();
    let mut sha = String::new();
    let mut size: i64 = 0;

    if let Some(f) = &a.file {
        let bytes = std::fs::read(f).with_context(|| format!("读取文件失败: {f}"))?;
        let fname = Path::new(f)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("upload.bin")
            .to_string();
        let up = api::request(
            cfg,
            "POST",
            "/api/registry/uploads",
            Some(&token),
            None,
            Some(&bytes),
            &[("X-Filename", &fname)],
        )?;
        storage_url = up["storageUrl"].as_str().unwrap_or("").to_string();
        sha = up["sha256"].as_str().unwrap_or("").to_string();
        size = up["size"].as_i64().unwrap_or(0);
    }

    let mut ns_id: Option<String> = None;
    if let Some(slug) = &a.namespace {
        let mine = api::get(cfg, "/api/namespaces/mine", Some(&token))?;
        let hit = mine["namespaces"].as_array().and_then(|arr| {
            arr.iter()
                .find(|x| x["slug"].as_str() == Some(slug.as_str()))
        });
        match hit {
            Some(n) => ns_id = n["id"].as_str().map(|s| s.to_string()),
            None => bail!("你无权使用 namespace {}", slug),
        }
    }

    let tags: Vec<String> = a
        .tags
        .as_deref()
        .map(|t| {
            t.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let status = if a.draft { "draft" } else { "published" };

    let mut body = json!({
        "kind": a.kind, "name": a.name,
        "slug": a.slug, "version": a.version.as_deref().unwrap_or("1.0.0"),
        "summary": a.summary.as_deref().unwrap_or(""), "tags": tags,
        "status": status, "visibility": a.visibility,
        "storage": { "url": storage_url, "sha256": if sha.is_empty() { Value::Null } else { Value::String(sha.clone()) }, "size": if size == 0 { Value::Null } else { json!(size) } },
    });
    if let Some(id) = ns_id {
        body["namespaceId"] = json!(id);
    }

    // harness 封装契约（kind=harness）
    if let Some(p) = &a.manifest {
        let raw = std::fs::read_to_string(p).with_context(|| format!("读取 manifest 失败: {p}"))?;
        let v: Value = serde_json::from_str(&raw).with_context(|| "manifest 不是合法 JSON")?;
        body["manifest"] = v;
    }

    // 发布即分发（可选）："all" 或 [worker 名称/id]；其它后端会忽略这个字段。
    if let Some(r) = a.replicate.as_deref().filter(|x| !x.is_empty()) {
        body["replicate"] = if r.eq_ignore_ascii_case("all") {
            json!("all")
        } else {
            json!(r
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>())
        };
    }

    let data = api::post_json(cfg, "/api/registry", Some(&token), &body)?;
    let it = &data["item"];
    println!(
        "✅ 已发布 [{}] {}/{}@{}  status={}  ({})",
        it["kind"].as_str().unwrap_or(""),
        it["namespace"]["slug"].as_str().unwrap_or(""),
        it["slug"].as_str().unwrap_or(""),
        it["version"].as_str().unwrap_or(""),
        it["status"].as_str().unwrap_or(""),
        it["id"].as_str().unwrap_or("")
    );
    println!("   存储: {}", it["storage"]["url"].as_str().unwrap_or(""));
    if let Some(reps) = data["replicated"].as_array() {
        if !reps.is_empty() {
            println!("   已分发到 {} 个节点：", reps.len());
            for r in reps {
                if r["ok"].as_bool().unwrap_or(false) {
                    println!(
                        "     ✓ {}  {} 字节",
                        r["nodeName"].as_str().unwrap_or(""),
                        r["size"].as_i64().unwrap_or(0)
                    );
                } else {
                    println!(
                        "     ✗ {}  {}",
                        r["nodeName"].as_str().unwrap_or(""),
                        r["error"].as_str().unwrap_or("失败")
                    );
                }
            }
        }
    }
    if let Some(e) = data["replicateError"].as_str() {
        println!("   ⚠ 分发未执行：{e}");
    }
    Ok(())
}

fn cmd_search(cfg: &CliConfig, a: &SearchArgs) -> anyhow::Result<()> {
    let token = if a.mine {
        Some(config::require_token(cfg)?)
    } else {
        None
    };
    let mut qs: Vec<(String, String)> = Vec::new();
    if let Some(q) = &a.query {
        qs.push(("q".into(), q.clone()));
    }
    if let Some(k) = &a.kind {
        qs.push(("kind".into(), k.clone()));
    }
    if let Some(t) = &a.tag {
        qs.push(("tag".into(), t.clone()));
    }
    if let Some(n) = &a.namespace {
        qs.push(("namespace".into(), n.clone()));
    }
    if a.mine {
        qs.push(("mine".into(), "1".into()));
    }
    let q = if qs.is_empty() {
        String::new()
    } else {
        format!(
            "?{}",
            qs.iter()
                .map(|(k, v)| format!("{k}={}", urlenc(v)))
                .collect::<Vec<_>>()
                .join("&")
        )
    };
    let data = api::get(cfg, &format!("/api/registry{q}"), token.as_deref())?;
    println!("共 {} 条:", data["total"].as_i64().unwrap_or(0));
    if let Some(items) = data["items"].as_array() {
        for it in items {
            println!(
                "  [{}] {}/{}@{}  {}  {}  {}⬇",
                it["kind"].as_str().unwrap_or(""),
                it["namespace"]["slug"].as_str().unwrap_or(""),
                it["slug"].as_str().unwrap_or(""),
                it["version"].as_str().unwrap_or(""),
                it["status"].as_str().unwrap_or(""),
                it["visibility"].as_str().unwrap_or(""),
                it["downloads"].as_i64().unwrap_or(0)
            );
            if let Some(s) = it["summary"].as_str() {
                if !s.is_empty() {
                    println!("      {s}");
                }
            }
        }
    }
    Ok(())
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn cmd_download(cfg: &CliConfig, target: &str, out: Option<&str>) -> anyhow::Result<()> {
    let token = config::require_token(cfg).ok();
    let path = format!("/api/registry/{}/download", target);
    let dl = api::get(cfg, &path, token.as_deref())?;
    let url = dl["url"].as_str().context("响应缺少 url")?;
    println!("⬇ {url}");
    let default_name = format!(
        "{}-{}@{}",
        dl["namespaceSlug"].as_str().unwrap_or("x"),
        dl["slug"].as_str().unwrap_or("x"),
        dl["version"].as_str().unwrap_or("")
    );
    let dest = out.unwrap_or(&default_name).to_string();

    // 用同一 agent 拉取制品字节
    let resp = ureq_agent()
        .get(url)
        .call()
        .map_err(|e| anyhow!("下载失败: {e}"))?;
    let mut buf: Vec<u8> = Vec::new();
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut buf)?;
    std::fs::write(&dest, &buf)?;
    println!("✅ 已保存 {} ({} bytes)", dest, buf.len());
    if let Some(s) = dl["sha256"].as_str() {
        if !s.is_empty() {
            println!("   sha256: {s}");
        }
    }
    Ok(())
}

/* ---------------- 安装（把条目装进本地包目录） ---------------- */
/// 本地包根目录（= `~/.ncc/packages`）：与 `ncc hur` 共用同一个实现 —— 两个入口装出来的包必须互相可见
fn packages_dir() -> PathBuf {
    hur_core::cfg::packages_dir()
}

/// 从存储 URL 推断文件扩展名（如 .md / .json），无则返回空串
fn ext_of_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("");
    if let Some(i) = name.rfind('.') {
        let ext = &name[i..];
        if !ext.is_empty() && ext.len() <= 12 && !ext[1..].contains('/') {
            return ext.to_string();
        }
    }
    String::new()
}

fn cmd_install(
    cfg: &CliConfig,
    target: &str,
    dir: Option<&str>,
    force: bool,
) -> anyhow::Result<()> {
    // 私有条目可用登录 token 拉取；公开条目匿名即可
    let token = config::require_token(cfg).ok();
    let dl = api::get(
        cfg,
        &format!("/api/registry/{target}/download"),
        token.as_deref(),
    )?;
    let id = dl["id"].as_str().unwrap_or("").to_string();
    let ns = dl["namespaceSlug"].as_str().unwrap_or("x").to_string();
    let slug = dl["slug"].as_str().unwrap_or("x").to_string();
    let version = dl["version"].as_str().unwrap_or("").to_string();
    let url = dl["url"].as_str().context("下载响应缺少 url")?.to_string();
    let sha = dl["sha256"].as_str().unwrap_or("").to_string();
    let dl_name = dl["name"].as_str().unwrap_or(&slug).to_string();

    // 补充 kind / summary / manifest（可选，公开条目可直接读）
    let mut kind = String::new();
    let mut summary = String::new();
    let mut manifest: Option<Value> = None;
    if !id.is_empty() {
        if let Ok(info) = api::get(cfg, &format!("/api/registry/{id}"), token.as_deref()) {
            kind = info
                .pointer("/item/kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            summary = info
                .pointer("/item/summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            manifest = info
                .pointer("/item/manifest")
                .and_then(|v| v.as_object())
                .map(|o| Value::Object(o.clone()));
        }
    }

    let root = dir.map(PathBuf::from).unwrap_or_else(packages_dir);
    let pkg_dir = root.join(&ns).join(&slug);
    let ext = ext_of_url(&url);
    let file_name = if ext.is_empty() {
        slug.clone()
    } else {
        format!("{slug}{ext}")
    };
    let dest = pkg_dir.join(&file_name);
    let manifest_path = pkg_dir.join("package.json");

    if dest.exists() && !force {
        bail!("{} 已安装（用 --force 覆盖）", dest.display());
    }

    // 拉取制品字节
    let resp = ureq_agent()
        .get(&url)
        .call()
        .map_err(|e| anyhow!("下载失败: {e}"))?;
    let mut buf: Vec<u8> = Vec::new();
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut buf)?;

    fs::create_dir_all(&pkg_dir)?;
    fs::write(&dest, &buf)?;
    let installed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut meta = json!({
        "source": target, "id": id, "name": dl_name, "kind": kind,
        "namespace": ns, "slug": slug, "version": version, "summary": summary,
        "url": url, "sha256": if sha.is_empty() { Value::Null } else { Value::String(sha.clone()) },
        "size": buf.len() as i64, "file": file_name,
        "installedAt": installed_at,
        "root": root.to_string_lossy(),
    });
    // harness 封装契约写入本地 package.json
    if let Some(m) = &manifest {
        meta["manifest"] = m.clone();
        if let Some(h) = m.get("harness") {
            meta["harness"] = h.clone();
        }
    }
    fs::write(&manifest_path, serde_json::to_string_pretty(&meta)?)?;

    let head = if kind.is_empty() {
        String::new()
    } else {
        format!("[{}] ", kind)
    };
    println!("⬇ {head}{ns}/{slug}@{version}");
    println!("✅ 已安装 → {}", dest.display());
    if !summary.is_empty() {
        println!("   {summary}");
    }
    if !sha.is_empty() {
        println!("   sha256: {sha}");
    }
    Ok(())
}

fn ureq_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build()
}

/* ---------------- API-Key ---------------- */
fn cmd_key(cfg: &CliConfig, k: &KeyCmd) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    match k {
        KeyCmd::List => {
            let data = api::get(cfg, "/api/auth/keys", Some(&token))?;
            let keys = data["keys"].as_array().cloned().unwrap_or_default();
            if keys.is_empty() {
                println!("还没有 API-Key。用 `ncc key create --label ci` 创建。");
                return Ok(());
            }
            println!(
                "{:<26} {:<16} {:<12} {:<10} {}",
                "ID", "备注", "类型", "命名空间", "过期"
            );
            for x in &keys {
                let ns = x["namespaces"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let exp = match x["expiresAt"].as_str() {
                    Some(e) => nodes::human_time(e),
                    None => "长期".to_string(),
                };
                println!(
                    "{:<26} {:<16} {:<12} {:<10} {}",
                    x["id"].as_str().unwrap_or(""),
                    x["label"].as_str().unwrap_or("-"),
                    if x["kind"].as_str().unwrap_or("user") == "distribution" {
                        "分发"
                    } else {
                        "通用"
                    },
                    if ns.is_empty() {
                        "不限".to_string()
                    } else {
                        ns
                    },
                    exp
                );
                let scopes = x["scopes"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                println!("    scopes: {}", scopes);
                if let Some(note) = x["note"].as_str().filter(|s| !s.is_empty()) {
                    println!("    备注: {note}");
                }
            }
            Ok(())
        }
        KeyCmd::Scopes => {
            println!("可用作用域（--scopes）");
            for (s, d) in [
                ("registry:read", "检索条目"),
                ("registry:download", "下载条目字节"),
                ("registry:publish", "发布/修改/删除条目（含上传字节）"),
                ("profile:read", "读名片"),
                ("profile:write", "改自己的名片"),
                (
                    "social:write",
                    "关注 / 取关他人、给名片打分（不放行任何数据）",
                ),
                ("nodes:read", "读我的节点与可连接节点（含区域聚合与推荐）"),
                ("nodes:write", "连 / 断节点、改 Name 标签、上报节点心跳"),
                ("grants:read", "查看授权关系"),
                ("grants:write", "授予 / 撤销授权"),
                ("living:write", "上报设备心跳（Living）"),
                (
                    "social:write",
                    "关注 / 取关他人、给名片打分（不放行任何数据）",
                ),
                ("keys:write", "签发 / 吊销 API-Key（默认不发给 key）"),
            ] {
                println!("  {s:<22} {d}");
            }
            println!("\n蕴含关系：publish ⇒ download ⇒ read；nodes:write ⇒ nodes:read；");
            println!(
                "          grants:write ⇒ grants:read；profile:write ⇒ profile:read；* = 全部。"
            );
            println!("          social:write 不被任何作用域蕴含（要关注 / 打分就得显式给）。");
            println!("\n提示：给 Agent/CI 的 key 一般只需 registry:read,registry:download；");
            println!(
                "      要让它发布制品再加 registry:publish；要让它读节点与连接再加 nodes:read。"
            );
            Ok(())
        }
        KeyCmd::Create(a) => {
            let kind = a.kind.as_str();
            let scopes: Option<Vec<String>> = a.scopes.as_deref().map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            });
            if kind == "distribution"
                && scopes.as_ref().is_some_and(|s| {
                    s.iter()
                        .any(|x| x.ends_with(":publish") || x.ends_with(":write") || x == "*")
                })
            {
                println!("⚠️  分发 key 建议只给只读作用域（registry:read,registry:download）。");
            }
            let body = json!({
                "label": a.label,
                "kind": kind,
                "scopes": scopes,
                "namespaces": a.namespaces,
                "note": a.note,
                "expiresInDays": a.expires,
            });
            let data = api::post_json(cfg, "/api/auth/keys", Some(&token), &body)?;
            // 响应是扁平的：{id,label,kind,scopes,namespaces,note,expiresAt,secret}
            // （宁可兼容一下嵌套写法，也不要因为它改坏输出）
            let key = if data.get("key").is_some_and(|v| v.is_object()) {
                data["key"].clone()
            } else {
                data.clone()
            };
            println!("✅ API-Key 已创建");
            println!("   secret  {}", data["secret"].as_str().unwrap_or(""));
            println!("   id      {}", key["id"].as_str().unwrap_or(""));
            println!("   kind    {}", key["kind"].as_str().unwrap_or(kind));
            let scopes_got = key["scopes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            println!(
                "   scopes  {}",
                if scopes_got.is_empty() {
                    key["scopes"].as_str().unwrap_or("-").to_string()
                } else {
                    scopes_got
                }
            );
            let ns = key["namespaces"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            println!(
                "   范围    {}",
                if ns.is_empty() {
                    "不限命名空间".to_string()
                } else {
                    ns
                }
            );
            if let Some(exp) = key["expiresAt"].as_str() {
                println!("   过期    {}", nodes::human_time(exp));
            }
            println!("\n   ⚠️  secret 仅显示这一次，请立即保存到密钥管理里。");
            println!(
                "   给 Agent 用：export NCC_TOKEN={}",
                data["secret"].as_str().unwrap_or("<secret>")
            );
            Ok(())
        }
        KeyCmd::Revoke { id } => {
            api::del(cfg, &format!("/api/auth/keys/{}", id), Some(&token))?;
            println!("✅ 已吊销 {}", id);
            Ok(())
        }
    }
}

/* ---------------- Living 设备接入 ---------------- */
fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

/// 本机**能自证**的提供能力：**只能由硬事实推导，运营者填的不算**。
///
/// 今天唯一能自证的是 `run:wasm` —— 这个二进制真的编进了 wasm 沙箱
/// （`--no-default-features` 编的瘦身版没有，所以它自证不出任何执行能力）。
/// 其余一律无法自证：
/// - `egress:*` 要真实网络可达性（需要显式探活，不能默认发请求），
/// - `run:remote` 要有一个真的在接活的远程执行服务（今天只到「登记环境」），
/// - `serve:*` / 托管类要看该节点是否真在对外服务。
///
/// 宁少不假：自证少了只是搜不到，自证多了就会让别人的请求转到一个跑不了的节点。
fn verified_offers() -> Vec<String> {
    if cfg!(feature = "sandbox") {
        vec!["run:wasm".to_string()]
    } else {
        Vec::new()
    }
}

fn cmd_living(cfg: &CliConfig, a: &LivingArgs) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    let name = a.name.clone().unwrap_or_else(default_device_name);
    let caps: Vec<String> = a
        .capabilities
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let verified = verified_offers();
    // 声明了 run:wasm 但二进制里没编进沙箱：不报错，但要说清楚它不会被算作自证。
    let claims_wasm = caps.iter().any(|c| {
        let c = c.trim().to_ascii_lowercase();
        c == "run:wasm" || c == "wasm"
    });
    if claims_wasm && verified.is_empty() {
        println!("⚠ 你声明了 run:wasm，但这个二进制没编进沙箱（--no-default-features），因此不会被算作自证。");
    }
    let body = json!({
        "name": name,
        "slug": a.slug.clone().unwrap_or_default(),
        "kind": a.kind,
        "url": a.url.clone().unwrap_or_default(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": caps,
        "offersVerified": verified,
    });
    let report = || -> anyhow::Result<()> {
        let data = api::post_json(cfg, "/api/namespaces/living", Some(&token), &body)?;
        let node = &data["node"];
        let declared = node["capabilities"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        let proved = node["capabilitiesVerified"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        println!(
            "⬆ [{}/{}] {} · {}/{} · 声明 {} · 自证 {} · lastSeen {}",
            node["slug"].as_str().unwrap_or("?"),
            node["status"].as_str().unwrap_or("?"),
            node["name"].as_str().unwrap_or("?"),
            node["os"].as_str().unwrap_or("?"),
            node["arch"].as_str().unwrap_or("?"),
            declared,
            proved,
            node["lastSeen"].as_str().unwrap_or("?"),
        );
        Ok(())
    };
    if a.daemon {
        println!(
            "守护心跳：每 {}s 上报到 {}（Ctrl+C 停止）",
            a.interval.max(1),
            cfg.base_url()
        );
        loop {
            report()?;
            std::thread::sleep(Duration::from_secs(a.interval.max(1)));
        }
    }
    report()
}
