mod admin;
mod api;
mod capability;
mod config;
mod configs;
mod hur;
mod mcp;
mod nodes;
mod p2p;
mod profile;
mod registry;
mod registryadd;
mod registryp2p;
mod services;
mod target;
mod terminal;
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
#[command(name = "ncc", version, about = "ncc.ai Registry 命令行客户端\n用法: ncc <command> [args...]\n目标：ncc target list（云端 ncc.ai / 内网 registry 节点各是一个目标）")]
struct Cli {
    /// 服务地址（本次命令用这个地址；已存在同名目标则复用，否则新建一个目标）
    #[arg(long, global = true)]
    base: Option<String>,
    /// 本次命令用哪个目标（ncc target list 看全部）
    #[arg(long, global = true)]
    target: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 注册新账户（自动创建个人命名空间）
    Register {
        #[arg(long)] email: String,
        #[arg(long)] password: String,
        #[arg(long)] name: Option<String>,
        /// 注册邀请码（服务端启用门禁时必需；也可用环境变量 NCC_INVITE_CODE）
        #[arg(long)] invite: Option<String>,
    },
    /// 登录
    Login {
        #[arg(long)] email: String,
        #[arg(long)] password: String,
    },
    /// 登出
    Logout,
    /// 当前用户与命名空间
    Me,
    /// 命名空间管理：ncc ns list | ncc ns create --slug …
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
        target: String,
        #[arg(short = 'o', long)]
        out: Option<String>,
    },
    /// 安装条目到本地包目录：ncc install <id | @org/slug> [--dir 目录] [--force]
    Install {
        /// 条目引用：R-… 或 @org/slug
        target: String,
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
    /// 本地能力（verify/pack/sign/key）**全程离线**；只有 publish 联网，且只上传"已经打完包的字节"。
    /// 真执行不在 ncc（不内置沙箱运行时）：`ncc hur run` 只出可审计划。
    #[command(subcommand)]
    Hur(hur::HurCmd),
    /// API-Key：ncc key create --label ci | ncc key list | ncc key revoke <id>
    #[command(subcommand)]
    Key(KeyCmd),
    /// 设备接入（Living）：上报本设备心跳到你的命名空间（--daemon 周期守护）
    Living(LivingArgs),
    /// NCC Profile：查看 / 设置名片（定位角色 + 作品集 + 已发布能力）
    Profile(ProfileArgs),
    /// NCC Node：节点连接（我的节点 + 连接别人的节点 + 发现 / 区域推荐）
    Nodes(NodesArgs),
    /// NCC Service：对外服务（服务提供方打包的多条业务）—— 匹配找服务 / 声明自己的服务
    ///
    /// 需要目标声明 `services` 能力（云端 ncc.ai 已声明；内网节点将来也可以声明，
    /// 那时同一个命令在那台节点上直接可用）。
    Services(ServicesArgs),
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
    /// NCC P2P：跨局域网节点直连（打洞条件预检 / 真实建连检查 / 信令 / 票据）
    ///
    /// 需要目标声明 `p2p` 能力（ncc.ai 云端已声明）；`probe` 是纯本地命令，不需要服务器。
    #[command(subcommand)]
    P2p(p2p::P2pCmd),
    /// 以 MCP server 方式暴露 NCC（stdio），供 Claude Desktop / Cursor / VS Code / 任意 Agent 接入
    Mcp,
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
    /// 我的节点 + 我连接的节点（--kind / --q 过滤）
    List(nodes::ListArgs),
    /// 节点类型目录（上报时用 `ncc living --kind` 声明）
    Kinds,
    /// 发现本 NCC 实例上可连接的节点（--kind / --region / --q）
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
enum GrantCmd {
    /// 我的授权（--out 我给出的 / --in 别人给我的）
    List(nodes::GrantListArgs),
    /// 授权：--user <id|@handle> --kind share|artifact|service [--ns @slug] [--note …]
    Set(nodes::GrantSetArgs),
    /// 撤销授权
    Rm { id: String },
}

/// `ncc services` 子命令。不带子命令 = 浏览公开服务目录。
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
    Create {
        #[arg(long)] slug: String,
        #[arg(long)] name: Option<String>,
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
    /// 本设备可提供的能力 kind，逗号分隔（如 mcp,api）
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
    #[arg(long)] kind: String,
    #[arg(long)] name: String,
    #[arg(long)] slug: Option<String>,
    #[arg(long)] version: Option<String>,
    #[arg(long)] summary: Option<String>,
    #[arg(long)] tags: Option<String>,
    /// harness 封装契约 JSON（kind=harness，含 harness.loader/entry）
    #[arg(long)]
    manifest: Option<String>,
    /// 目标 namespace slug（默认个人命名空间）
    #[arg(long)] namespace: Option<String>,
    /// 可见性 public/private（private 需 Pro 付费）
    #[arg(long, default_value = "public", value_parser = ["public", "private"])]
    visibility: String,
    /// 以 draft 状态创建（默认 published）
    #[arg(long)] draft: bool,
    /// 发布后把副本分发到 worker：all 或名称/id（逗号分隔）；仅 ncc-registry 支持
    #[arg(long)]
    replicate: Option<String>,
}

#[derive(clap::Args)]
struct SearchArgs {
    /// 关键词
    query: Option<String>,
    #[arg(long)] kind: Option<String>,
    #[arg(long)] tag: Option<String>,
    #[arg(long)] namespace: Option<String>,
    #[arg(long)] mine: bool,
}

fn main() {
    // `ncc hub …` 是「本次用云端目标」的糖：在交给 clap 之前把 hub 摘掉。
    // 这样 ncc hub services match "…" 与 ncc --target hub services match "…" 等价，
    // 而不需要把整棵命令树再抄一份到 hub 底下。
    let (args, hub_prefix) = strip_hub_prefix(std::env::args().collect());
    // `ncc hub` 单独出现（没有子命令）= 看云端目标的状态
    if hub_prefix && args.len() <= 1 {
        let mut cfg = config::load();
        if let Err(e) = resolve_target(&mut cfg, &Cli { base: None, target: None, cmd: Cmd::Target(target::TargetArgs { action: None }) }, hub_prefix) {
            eprintln!("✗ {:#}", e);
            std::process::exit(1);
        }
        let action = Cmd::Target(target::TargetArgs {
            action: Some(target::TargetAction::Show { name: target::hub_target_name(&cfg), json: false }),
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
    // 先判定协议模式，再决定提示走哪个流：resolve_target 里的提示也算。
    if matches!(cli.cmd, Cmd::Mcp) {
        PROTOCOL_STDOUT.store(true, Ordering::Relaxed);
    }
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
                    config::Target { base_url: base.clone(), ..config::Target::default() },
                );
                probe_cfg.current = Some("probe".into());
                let m = capability::probe(&probe_cfg);
                let name = target::suggest_name_for(cfg, &base, &m.kind);
                cfg.targets.insert(
                    name.clone(),
                    config::Target {
                        kind: match m.kind.as_str() {
                            "node" => "registry".to_string(),
                            "hub" => "cloud".to_string(),
                            _ => if base.contains("ncc.ai") { "cloud".into() } else { "registry".into() },
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
        Cmd::Profile(_) => Some("profile"),
        Cmd::Nodes(_) => Some("nodes"),
        Cmd::Services(_) => Some("services"),
        Cmd::Grant(_) => Some("grants"),
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
            RegistryCmd::Join(_) | RegistryCmd::Status(_) | RegistryCmd::Nodes(_) | RegistryCmd::Leave(_) => {
                Some("nodes")
            }
            RegistryCmd::Login(_) | RegistryCmd::Replicate(_) | RegistryCmd::Rm(_) => Some("registry"),
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
        Cmd::Register { email, password, name, invite } => cmd_register(cfg, email, password, name.as_deref(), invite.as_deref()),
        Cmd::Login { email, password } => cmd_login(cfg, email, password),
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
        Cmd::Download { target, out } => cmd_download(cfg, target, out.as_deref()),
        Cmd::Install { target, dir, force } => cmd_install(cfg, target, dir.as_deref(), *force),
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
        Cmd::Key(k) => cmd_key(cfg, k),
        Cmd::Living(a) => cmd_living(cfg, a),
        Cmd::Profile(p) => match &p.action {
            None | Some(ProfileCmd::Show { username: None }) => profile::show(cfg, None),
            Some(ProfileCmd::Show { username: Some(u) }) => profile::show(cfg, Some(u.as_str())),
            Some(ProfileCmd::Roles { group }) => profile::roles(cfg, group.as_deref()),
            Some(ProfileCmd::Set(a)) => profile::set(cfg, a),
            Some(ProfileCmd::Username { name }) => profile::set_username(cfg, name),
            Some(ProfileCmd::Work(cmd)) => profile::work(cfg, cmd),
        },
        Cmd::Nodes(n) => match &n.action {
            None => nodes::list(cfg, &nodes::ListArgs { kind: None, q: None }),
            Some(NodesCmd::List(a)) => nodes::list(cfg, a),
            Some(NodesCmd::Kinds) => nodes::kinds(cfg),
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
        Cmd::P2p(p) => match p {
            p2p::P2pCmd::Probe(a) => p2p::probe(cfg, &a),
            p2p::P2pCmd::Ice => p2p::ice(cfg),
            p2p::P2pCmd::Check(a) => p2p::check(cfg, &a),
            p2p::P2pCmd::Signal(s) => p2p::signal(cfg, &s),
            p2p::P2pCmd::Ticket(t) => p2p::ticket(cfg, &t),
        },
        Cmd::Services(v) => match &v.action {
            None => services::list(cfg, &services::ListArgs {
                category: None, tag: None, region: None, protocol: None, q: None,
                mine: false, limit: 20, json: false,
            }),
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
                configs::ConfigAction::History { target, json } => configs::history(cfg, target, *json),
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
        Cmd::Mcp => mcp::serve(cfg),
    }
}

/* ---------------- 账号 ---------------- */
// 登录态写进**当前目标**：在本地 registry 登录不会把云端的会话挤掉（多目标的意义所在）。
fn save_session(cfg: &mut CliConfig, token: &str, email: &str, name: &str) -> anyhow::Result<()> {
    config::save_session(cfg, token, email, name)
}

fn cmd_register(cfg: &mut CliConfig, email: &str, password: &str, name: Option<&str>, invite: Option<&str>) -> anyhow::Result<()> {
    // 邀请码：--invite 优先，其次环境变量 NCC_INVITE_CODE（服务端未启用门禁时可留空）
    let invite = invite.map(|s| s.to_string()).or_else(|| std::env::var("NCC_INVITE_CODE").ok());
    let body = json!({ "email": email, "password": password, "name": name, "inviteCode": invite });
    let data = api::post_json(cfg, "/api/auth/register", None, &body)?;
    let token = data["token"].as_str().context("响应缺少 token")?;
    let u = &data["user"];
    save_session(cfg, token, u["email"].as_str().unwrap_or(email), u["name"].as_str().unwrap_or(name.unwrap_or("")))?;
    println!("✅ 注册成功：{}（token 已保存到 {}）", u["email"].as_str().unwrap_or(email), config::config_path().display());
    // 内网 registry 的第一个账号自动成为节点管理员，并在这里拿到机器用 admin key/secret。
    // secret 只在注册响应里出现一次 —— 不打印就等于让用户永久失去它。
    if let Some(admin) = data.get("admin") {
        if let Some(key) = admin["key"].as_str() {
            println!("\n👑 你是本节点的第一个账号 → 自动成为管理员");
            println!("   admin key      {key}");
            println!("   admin secret   {}", admin["secret"].as_str().unwrap_or(""));
            println!("   （secret 只显示这一次，请立刻保存；写进本机配置：）");
            println!("   ncc registry admin login --key {key} --secret <上面的 secret>");
        }
    }
    Ok(())
}

fn cmd_login(cfg: &mut CliConfig, email: &str, password: &str) -> anyhow::Result<()> {
    let body = json!({ "email": email, "password": password });
    let data = api::post_json(cfg, "/api/auth/login", None, &body)?;
    let token = data["token"].as_str().context("响应缺少 token")?;
    let u = &data["user"];
    save_session(cfg, token, u["email"].as_str().unwrap_or(email), u["name"].as_str().unwrap_or(""))?;
    println!("✅ 登录成功：{}", u["email"].as_str().unwrap_or(email));
    Ok(())
}

fn cmd_me(cfg: &CliConfig) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    let data = api::get(cfg, "/api/auth/me", Some(&token))?;
    let u = &data["user"];
    println!("用户: {} <{}>  ({})", u["name"].as_str().unwrap_or(""), u["email"].as_str().unwrap_or(""), u["id"].as_str().unwrap_or(""));
    println!("计划: {}", u["plan"].as_str().unwrap_or("free"));
    println!("Namespaces:");
    if let Some(ns) = data["namespaces"].as_array() {
        for n in ns {
            let ty = if n["type"] == "org" { "🏢" } else { "👤" };
            let owner = if n["owner"].as_bool().unwrap_or(false) { "owner" } else { "member" };
            println!("  {} {}  {}  {}", ty, n["slug"].as_str().unwrap_or(""), owner, n["visibility"].as_str().unwrap_or(""));
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
                    println!("{} {}\t{}\t{}\t{}",
                        ty, x["slug"].as_str().unwrap_or(""),
                        x["name"].as_str().unwrap_or(""),
                        if x["owner"].as_bool().unwrap_or(false) { "owner" } else { "member" },
                        x["visibility"].as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        NsCmd::Create { slug, name } => {
            let body = json!({ "slug": slug, "name": name });
            let data = api::post_json(cfg, "/api/namespaces", Some(&token), &body)?;
            println!("✅ namespace 创建：{} ({})", data["slug"].as_str().unwrap_or(slug), data["id"].as_str().unwrap_or(""));
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
        let fname = Path::new(f).file_name().and_then(|s| s.to_str()).unwrap_or("upload.bin").to_string();
        let up = api::request(cfg, "POST", "/api/registry/uploads", Some(&token), None, Some(&bytes), &[("X-Filename", &fname)])?;
        storage_url = up["storageUrl"].as_str().unwrap_or("").to_string();
        sha = up["sha256"].as_str().unwrap_or("").to_string();
        size = up["size"].as_i64().unwrap_or(0);
    }

    let mut ns_id: Option<String> = None;
    if let Some(slug) = &a.namespace {
        let mine = api::get(cfg, "/api/namespaces/mine", Some(&token))?;
        let hit = mine["namespaces"].as_array()
            .and_then(|arr| arr.iter().find(|x| x["slug"].as_str() == Some(slug.as_str())));
        match hit {
            Some(n) => ns_id = n["id"].as_str().map(|s| s.to_string()),
            None => bail!("你无权使用 namespace {}", slug),
        }
    }

    let tags: Vec<String> = a.tags.as_deref()
        .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
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
            json!(r.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        };
    }

    let data = api::post_json(cfg, "/api/registry", Some(&token), &body)?;
    let it = &data["item"];
    println!("✅ 已发布 [{}] {}/{}@{}  status={}  ({})",
        it["kind"].as_str().unwrap_or(""),
        it["namespace"]["slug"].as_str().unwrap_or(""),
        it["slug"].as_str().unwrap_or(""),
        it["version"].as_str().unwrap_or(""),
        it["status"].as_str().unwrap_or(""),
        it["id"].as_str().unwrap_or(""));
    println!("   存储: {}", it["storage"]["url"].as_str().unwrap_or(""));
    if let Some(reps) = data["replicated"].as_array() {
        if !reps.is_empty() {
            println!("   已分发到 {} 个节点：", reps.len());
            for r in reps {
                if r["ok"].as_bool().unwrap_or(false) {
                    println!("     ✓ {}  {} 字节", r["nodeName"].as_str().unwrap_or(""), r["size"].as_i64().unwrap_or(0));
                } else {
                    println!("     ✗ {}  {}", r["nodeName"].as_str().unwrap_or(""), r["error"].as_str().unwrap_or("失败"));
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
    let token = if a.mine { Some(config::require_token(cfg)?) } else { None };
    let mut qs: Vec<(String, String)> = Vec::new();
    if let Some(q) = &a.query { qs.push(("q".into(), q.clone())); }
    if let Some(k) = &a.kind { qs.push(("kind".into(), k.clone())); }
    if let Some(t) = &a.tag { qs.push(("tag".into(), t.clone())); }
    if let Some(n) = &a.namespace { qs.push(("namespace".into(), n.clone())); }
    if a.mine { qs.push(("mine".into(), "1".into())); }
    let q = if qs.is_empty() { String::new() } else {
        format!("?{}", qs.iter().map(|(k, v)| format!("{k}={}", urlenc(v))).collect::<Vec<_>>().join("&"))
    };
    let data = api::get(cfg, &format!("/api/registry{q}"), token.as_deref())?;
    println!("共 {} 条:", data["total"].as_i64().unwrap_or(0));
    if let Some(items) = data["items"].as_array() {
        for it in items {
            println!("  [{}] {}/{}@{}  {}  {}  {}⬇",
                it["kind"].as_str().unwrap_or(""),
                it["namespace"]["slug"].as_str().unwrap_or(""),
                it["slug"].as_str().unwrap_or(""),
                it["version"].as_str().unwrap_or(""),
                it["status"].as_str().unwrap_or(""),
                it["visibility"].as_str().unwrap_or(""),
                it["downloads"].as_i64().unwrap_or(0));
            if let Some(s) = it["summary"].as_str() {
                if !s.is_empty() { println!("      {s}"); }
            }
        }
    }
    Ok(())
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => out.push(b as char),
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
    let default_name = format!("{}-{}@{}",
        dl["namespaceSlug"].as_str().unwrap_or("x"),
        dl["slug"].as_str().unwrap_or("x"),
        dl["version"].as_str().unwrap_or(""));
    let dest = out.unwrap_or(&default_name).to_string();

    // 用同一 agent 拉取制品字节
    let resp = ureq_agent().get(url).call().map_err(|e| anyhow!("下载失败: {e}"))?;
    let mut buf: Vec<u8> = Vec::new();
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut buf)?;
    std::fs::write(&dest, &buf)?;
    println!("✅ 已保存 {} ({} bytes)", dest, buf.len());
    if let Some(s) = dl["sha256"].as_str() {
        if !s.is_empty() { println!("   sha256: {s}"); }
    }
    Ok(())
}

/* ---------------- 安装（把条目装进本地包目录） ---------------- */
/// 本地包根目录：$NCC_PACKAGES_DIR 或 ~/.ncc/packages
fn packages_dir() -> PathBuf {
    if let Ok(p) = std::env::var("NCC_PACKAGES_DIR") {
        return PathBuf::from(p);
    }
    // 走 config::home_dir() 而不是直接读 $HOME —— 后者在原生 Windows shell 上是空的，
    // 会算成相对路径 ./.ncc/packages（见 config::home_dir 的说明）。
    config::ncc_dir().join("packages")
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

fn cmd_install(cfg: &CliConfig, target: &str, dir: Option<&str>, force: bool) -> anyhow::Result<()> {
    // 私有条目可用登录 token 拉取；公开条目匿名即可
    let token = config::require_token(cfg).ok();
    let dl = api::get(cfg, &format!("/api/registry/{target}/download"), token.as_deref())?;
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
            kind = info.pointer("/item/kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
            summary = info.pointer("/item/summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
            manifest = info
                .pointer("/item/manifest")
                .and_then(|v| v.as_object())
                .map(|o| Value::Object(o.clone()));
        }
    }

    let root = dir.map(PathBuf::from).unwrap_or_else(packages_dir);
    let pkg_dir = root.join(&ns).join(&slug);
    let ext = ext_of_url(&url);
    let file_name = if ext.is_empty() { slug.clone() } else { format!("{slug}{ext}") };
    let dest = pkg_dir.join(&file_name);
    let manifest_path = pkg_dir.join("package.json");

    if dest.exists() && !force {
        bail!("{} 已安装（用 --force 覆盖）", dest.display());
    }

    // 拉取制品字节
    let resp = ureq_agent().get(&url).call().map_err(|e| anyhow!("下载失败: {e}"))?;
    let mut buf: Vec<u8> = Vec::new();
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut buf)?;

    fs::create_dir_all(&pkg_dir)?;
    fs::write(&dest, &buf)?;
    let installed_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
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

    let head = if kind.is_empty() { String::new() } else { format!("[{}] ", kind) };
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
    ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(60)).build()
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
            println!("{:<26} {:<16} {:<12} {:<10} {}", "ID", "备注", "类型", "命名空间", "过期");
            for x in &keys {
                let ns = x["namespaces"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                let exp = match x["expiresAt"].as_str() {
                    Some(e) => nodes::human_time(e),
                    None => "长期".to_string(),
                };
                println!("{:<26} {:<16} {:<12} {:<10} {}",
                    x["id"].as_str().unwrap_or(""),
                    x["label"].as_str().unwrap_or("-"),
                    if x["kind"].as_str().unwrap_or("user") == "distribution" { "分发" } else { "通用" },
                    if ns.is_empty() { "不限".to_string() } else { ns },
                    exp);
                let scopes = x["scopes"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "))
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
                ("nodes:read", "读我的节点与可连接节点（含区域聚合与推荐）"),
                ("nodes:write", "连 / 断节点、改 Name 标签、上报节点心跳"),
                ("grants:read", "查看授权关系"),
                ("grants:write", "授予 / 撤销授权"),
                ("living:write", "上报设备心跳（Living）"),
                ("keys:write", "签发 / 吊销 API-Key（默认不发给 key）"),
            ] {
                println!("  {s:<22} {d}");
            }
            println!("\n蕴含关系：publish ⇒ download ⇒ read；nodes:write ⇒ nodes:read；");
            println!("          grants:write ⇒ grants:read；profile:write ⇒ profile:read；* = 全部。");
            println!("\n提示：给 Agent/CI 的 key 一般只需 registry:read,registry:download；");
            println!("      要让它发布制品再加 registry:publish；要让它读节点与连接再加 nodes:read。");
            Ok(())
        }
        KeyCmd::Create(a) => {
            let kind = a.kind.as_str();
            let scopes: Option<Vec<String>> = a.scopes.as_deref().map(|s| {
                s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
            });
            if kind == "distribution" && scopes.as_ref().is_some_and(|s| s.iter().any(|x| x.ends_with(":publish") || x.ends_with(":write") || x == "*")) {
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
            let key = if data.get("key").is_some_and(|v| v.is_object()) { data["key"].clone() } else { data.clone() };
            println!("✅ API-Key 已创建");
            println!("   secret  {}", data["secret"].as_str().unwrap_or(""));
            println!("   id      {}", key["id"].as_str().unwrap_or(""));
            println!("   kind    {}", key["kind"].as_str().unwrap_or(kind));
            let scopes_got = key["scopes"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            println!("   scopes  {}", if scopes_got.is_empty() { key["scopes"].as_str().unwrap_or("-").to_string() } else { scopes_got });
            let ns = key["namespaces"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            println!("   范围    {}", if ns.is_empty() { "不限命名空间".to_string() } else { ns });
            if let Some(exp) = key["expiresAt"].as_str() {
                println!("   过期    {}", nodes::human_time(exp));
            }
            println!("\n   ⚠️  secret 仅显示这一次，请立即保存到密钥管理里。");
            println!("   给 Agent 用：export NCC_TOKEN={}", data["secret"].as_str().unwrap_or("<secret>"));
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

fn cmd_living(cfg: &CliConfig, a: &LivingArgs) -> anyhow::Result<()> {
    let token = config::require_token(cfg)?;
    let name = a.name.clone().unwrap_or_else(default_device_name);
    let caps: Vec<String> = a.capabilities
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    let body = json!({
        "name": name,
        "slug": a.slug.clone().unwrap_or_default(),
        "kind": a.kind,
        "url": a.url.clone().unwrap_or_default(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": caps,
    });
    let report = || -> anyhow::Result<()> {
        let data = api::post_json(cfg, "/api/namespaces/living", Some(&token), &body)?;
        let node = &data["node"];
        println!(
            "⬆ [{}/{}] {} · {}/{} · 能力 {} · lastSeen {}",
            node["slug"].as_str().unwrap_or("?"),
            node["status"].as_str().unwrap_or("?"),
            node["name"].as_str().unwrap_or("?"),
            node["os"].as_str().unwrap_or("?"),
            node["arch"].as_str().unwrap_or("?"),
            node["capabilities"].as_array().map(|a| a.len()).unwrap_or(0),
            node["lastSeen"].as_str().unwrap_or("?"),
        );
        Ok(())
    };
    if a.daemon {
        println!("守护心跳：每 {}s 上报到 {}（Ctrl+C 停止）", a.interval.max(1), cfg.base_url());
        loop {
            report()?;
            std::thread::sleep(Duration::from_secs(a.interval.max(1)));
        }
    }
    report()
}
