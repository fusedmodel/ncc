//! `ncc hur` —— hur 制品工具链（内嵌 `hur-core`）。
//!
//! 设计见 `ncc-platform/prd/ncc-hur.md`。三条边界（别在实现里糊掉）：
//! 1. **离线铁律**：`verify / pack / sign / key` 全本地，一个字节都不上传；
//!    只有 `publish`（上传产物）与 `attach`（只上传签名文件与公钥）联网。
//! 2. **身份一次到底**：`publish` 用 ncc 自己的登录态与 registry，不读也不写
//!    hur 的 `~/.harnessuse/hur.json`（签名密钥在 `~/.harnessuse/keys`，那是**本地私钥**，本就不该进 ncc 配置）。
//! 3. **执行在本机**：`ncc` 是 harness-use 事实上的执行引擎 —— `ncc hur run --exec`
//!    用内置的 `hur-sandbox`（wasmtime）真跑，限额由策略算岀，跑完按
//!    `harness-use-run-trace/v1` 写留痕（`ncc hur task` 读得回来）。
//!    `--no-default-features` 才是不带沙箱的瘦身构建，那时 `--exec` 如实拒绝。
//!
//! 本模块是**薄壳**：能力全在 `hur_core`，这里只做参数映射与输出。
//! 
//! 命令面已覆盖独立 `hur` 二进制的全部能力（含 `export` / `mcp` / `interop` /
//! `install|uninstall|list`）—— 桌面端（harness-use）因此可以**删掉自己那份 hur-core**，
//! 改成子进程调 `ncc hur`（它继续只为"真执行"留一个本地沙箱）。

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use hur_core::{dep, install, interop, pack, policy, sign, spec, tpl};

use crate::api;
use crate::config::{self, CliConfig};
use crate::mcp;

#[derive(Subcommand)]
pub enum HurCmd {
    /// 校验包目录或 .hur 产物（R1~R9，全程离线；R9 = 制品签名）
    Verify(HurVerifyArgs),
    /// 读包：清单 / 依赖 / 权限面 / 安全策略 / 签名状态
    Inspect {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 列出包内文件（含 `hur.lock` 记录的 sha256）
    Ls {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 生成 / 刷新 hur.lock（依赖锁定，签名覆盖它）
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// 产包：dist/<id>-<version>.hur + .sha256（确定性字节）
    Pack {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 签名：签的是"规范打包字节"（Minisign/ed25519，第三方可独立核对）
    Sign(HurSignArgs),
    /// 生成一个合规包工程
    Init(HurInitArgs),
    /// 执行：默认只出可审计划；`--exec` 在本机 wasm 沙箱里真跑（限额来自策略，跑完留痕）
    Run {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// 请求真执行（本机 wasm 沙箱；限额/引擎由生效策略决定）
        #[arg(long)]
        exec: bool,
        #[arg(long)]
        json: bool,
    },
    /// 签名密钥与受信公钥（私钥只在本机 `~/.harnessuse/keys`）
    Key(HurKeyArgs),
    /// 依赖关系检查：声明 ↔ hur.lock ↔ 实际字节（R4 的对账单）
    Dep {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// 有提醒也算失败（未解析/未锁定/残留）
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        json: bool,
    },
    /// 沙箱治理视图：引擎 / 限额 / 登记环境 / 留痕（**控制面**；托管在客户端）
    Sandbox {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 沙箱环境登记（远程 Proxy sandbox）：ncc hur env ls | show | use | add | rm
    Env(HurEnvArgs),
    /// 执行留痕与投递任务：ncc hur task ls | inspect（谁在什么限额下跑了什么）
    Task(HurTaskArgs),
    /// 导出**可分发产物**：.hur + .minisig + .sha256 + export.json（离线；先自己校验）
    Export(HurExportArgs),
    /// 信任一个**已发布条目**的签名公钥（先下载产物与签名核对通过，才进信任表）
    Trust(HurTrustArgs),
    /// 策略：ncc hur policy show | check | path | presets | set
    Policy(HurPolicyArgs),
    /// 以 MCP server 方式暴露 hur 治理面（stdio，**只读**；写操作留在 CLI）
    Mcp {
        /// 只打印工具表（JSON）就退出，不起 server（给客户端生成提示用）
        #[arg(long)]
        list_tools: bool,
    },
    /// 安装包：`@ns/slug`（远端）或本地 `.hur` → `~/.ncc/packages/<id>/`
    Install(HurInstallArgs),
    /// 把**工程目录**写进包落点并登记（桌面端「写入本机」用；不产包）
    Write {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// 已有同 id 时覆盖
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// 卸载已安装的包（同时从桌面端登记里移除）
    Uninstall {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// 列出已安装的包（`~/.harnessuse/packages/*/_install.json`）
    List {
        #[arg(long)]
        json: bool,
    },
    /// 互操作导出：渲染成 Claude / Cursor / Cline / Codex / MCP 能直接用的产物（默认只预览）
    Interop(HurInteropArgs),
    /// 发布到 NCC Registry（kind=hur；先本地 verify → pack → 可选 sign）
    Publish(HurPublishArgs),
    /// 把本机签名附着到**已发布**条目上（不重发新版本；产物字节必须没变）
    Attach(HurAttachArgs),
}

#[derive(Args, Clone)]
pub struct HurEnvArgs {
    #[command(subcommand)]
    pub cmd: HurEnvCmd,
}

#[derive(Subcommand, Clone)]
pub enum HurEnvCmd {
    /// 列出已登记的沙箱环境
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// 看一个环境的详情（含证明与限额）
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// 设为默认环境
    Use { id: String },
    /// 登记/覆盖一个环境
    Add(HurEnvAddArgs),
    /// 移除登记
    Rm { id: String },
}

#[derive(Args, Clone)]
pub struct HurEnvAddArgs {
    pub id: String,
    /// proxy（远程 Proxy sandbox）| local（本机引擎，统一选择用）
    #[arg(long, default_value = "proxy")]
    pub kind: String,
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long, default_value = "")]
    pub url: String,
    /// 该环境提供哪些引擎（逗号/空格分隔；留空 = 未声明）
    #[arg(long, default_value = "")]
    pub engines: String,
    #[arg(long, default_value_t = 0)]
    pub max_memory_mb: u32,
    #[arg(long, default_value_t = 0)]
    pub max_wall_ms: u32,
    /// 证明：来源 registry
    #[arg(long, default_value = "")]
    pub registry: String,
    /// 证明：`@ns/slug` 引用
    #[arg(long, default_value = "")]
    pub reference: String,
    /// 证明：制品 sha256
    #[arg(long, default_value = "")]
    pub sha256: String,
    /// 该环境是否允许接收数据出境
    #[arg(long)]
    pub allow_data_leaving: bool,
    #[arg(long)]
    pub default: bool,
    #[arg(long, default_value = "")]
    pub note: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurTaskArgs {
    #[command(subcommand)]
    pub cmd: HurTaskCmd,
}

#[derive(Subcommand, Clone)]
pub enum HurTaskCmd {
    /// 列出执行留痕（最近在前）＋ 投递到本机的任务
    Ls {
        /// 包目录（用于解析策略里的 audit.trace_dir）
        #[arg(default_value = ".")]
        path: PathBuf,
        /// 直接指定留痕目录（覆盖策略里的 trace_dir）
        #[arg(long, default_value = "")]
        dir: String,
        /// 只看某个包（按 trace 里的 package 过滤）
        #[arg(long, default_value = "")]
        package: String,
        #[arg(long)]
        json: bool,
    },
    /// 看一条留痕的完整内容（`latest` / 文件名 / 序号）
    Inspect {
        /// 留痕文件、`latest`、或列表里的序号（从 1 开始）
        ///
        /// ⚠️ 字段名不能叫 `target`（与全局 `--target` 撞 clap arg id）；同 `ncc info` 的坑。
        reference: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "")]
        dir: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Clone)]
pub struct HurInstallArgs {
    /// `@命名空间/slug[@版本]`、条目 id，或本地 `.hur` 文件路径
    pub reference: String,
    /// 期望的 sha256（远端不给则用条目登记的；本地文件可不给）
    #[arg(long, default_value = "")]
    pub sha256: String,
    /// 已有同 id 时覆盖
    #[arg(long)]
    pub force: bool,
    /// 签名不通过也装（默认不：装进本机的东西得说得清出处）
    #[arg(long)]
    pub allow_bad_signature: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurInteropArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// `all`（默认）或 claude,cursor,cline,codex,mcp
    #[arg(long, default_value = "all")]
    pub targets: String,
    /// 落点目录（宿主工程根）
    #[arg(long, default_value = ".")]
    pub out: String,
    /// 真的写盘（默认只预览）
    #[arg(long)]
    pub write: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurExportArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 导出目录（默认 dist/export）
    #[arg(long, default_value = "dist/export")]
    pub out: String,
    /// 要求产物带**可核对**签名（不给则跟随生效策略）
    #[arg(long)]
    pub require_signature: bool,
    /// 有错也导出（默认不：R1~R9 不过就拒绝）
    #[arg(long)]
    pub allow_issues: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurTrustArgs {
    /// 条目引用：`@命名空间/slug`（可带 `@版本`）或条目 id
    pub reference: String,
    /// 信任表里的标签（默认写成 `@ns/slug@ver`）
    #[arg(long, default_value = "")]
    pub label: String,
    #[arg(long, default_value = "")]
    pub note: String,
    /// 跳过"下载产物 + 核对签名"这一步（会在信任表里留下"未核对"的证据）
    #[arg(long)]
    pub force: bool,
    /// 把核对用的产物/签名/公钥留在哪个目录（默认临时目录）
    #[arg(long, default_value = "")]
    pub dir: String,
    /// 条目里没带公钥时，用这一份（.pub 文件路径或单行 base64）
    #[arg(long, default_value = "")]
    pub pub_key: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurPolicyArgs {
    #[command(subcommand)]
    pub cmd: HurPolicyCmd,
}

#[derive(Subcommand, Clone)]
pub enum HurPolicyCmd {
    /// 看生效策略：逐层来源 + 收紧后的实际值
    Show {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 按生效策略跑 R1~R9（= verify 的策略视角）
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// 把提醒也当失败
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        json: bool,
    },
    /// 策略文件落点（工程层 / 机器层）与是否存在
    Path {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 列出内置预设
    Presets {
        #[arg(long)]
        json: bool,
    },
    /// 改策略（默认写工程层 hur.security.json；`--machine` 写 ~/.harnessuse/security.json）
    Set(HurPolicySetArgs),
}

#[derive(Args, Clone)]
pub struct HurPolicySetArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 写机器层（~/.harnessuse/security.json）而不是工程层
    #[arg(long)]
    pub machine: bool,
    /// 以某个内置预设为底（`ncc hur policy presets` 看有哪些）
    #[arg(long, default_value = "")]
    pub preset: String,
    #[arg(long, default_value = "")]
    pub id: String,
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long, default_value = "")]
    pub description: String,
    /// true/false：签名要求（R9）
    #[arg(long, default_value = "")]
    pub require_signature: String,
    /// true/false：必须有 hur.lock
    #[arg(long, default_value = "")]
    pub require_lock: String,
    /// true/false：提醒也算失败
    #[arg(long, default_value = "")]
    pub strict: String,
    #[arg(long, default_value = "")]
    pub max_errors: String,
    /// true/false：是否允许执行
    #[arg(long, default_value = "")]
    pub exec: String,
    /// 允许的引擎（逗号分隔；`none` = 都不许；`all` = 内置五种）
    #[arg(long, default_value = "")]
    pub engines: String,
    /// true/false：只在本地执行
    #[arg(long, default_value = "")]
    pub local_only: String,
    /// none | declared-only
    #[arg(long, default_value = "")]
    pub network: String,
    #[arg(long, default_value = "")]
    pub max_network_calls: String,
    #[arg(long, default_value = "")]
    pub max_output_chars: String,
    #[arg(long, default_value = "")]
    pub fuel: String,
    #[arg(long, default_value = "")]
    pub memory_mb: String,
    #[arg(long, default_value = "")]
    pub stack_kb: String,
    #[arg(long, default_value = "")]
    pub wall_ms: String,
    /// true/false：允许把执行发到远程 sandbox
    #[arg(long, default_value = "")]
    pub remote: String,
    /// true/false：远程环境必须带可核对的证明
    #[arg(long, default_value = "")]
    pub require_attestation: String,
    /// true/false：允许数据出境
    #[arg(long, default_value = "")]
    pub allow_data_leaving: String,
    /// 留痕目录（相对工程根）
    #[arg(long, default_value = "")]
    pub trace_dir: String,
    #[arg(long, default_value = "")]
    pub retain: String,
    /// 只看会写成什么，不落盘
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurVerifyArgs {
    /// 包目录，或 .hur 产物
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 要求产物带**可核对**签名（不给则跟随生效策略的 verify.require_signature）
    #[arg(long)]
    pub require_signature: bool,
    /// 用指定公钥核对签名
    #[arg(long, default_value = "")]
    pub pub_key: String,
    /// 把提醒也当失败
    #[arg(long)]
    pub strict: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurSignArgs {
    /// 包目录（会先按规范打包）或现成的 .hur 产物
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 私钥文件（默认 ~/.harnessuse/keys/hur.key）
    #[arg(long, default_value = "")]
    pub key: String,
    /// 私钥口令（更推荐环境变量 HUR_KEY_PASSWORD）
    #[arg(long, default_value = "")]
    pub password: String,
    /// 签名文件落点（默认 <产物>.minisig）
    #[arg(long, default_value = "")]
    pub out: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurInitArgs {
    #[arg(long, default_value = "agent", value_parser = ["agent", "harness", "repo"])]
    pub kind: String,
    #[arg(long, default_value = "My Agent")]
    pub name: String,
    #[arg(long, default_value = "")]
    pub domain: String,
    /// kind=agent：角色/职责（写进 agent.system_prompt）
    #[arg(long, default_value = "")]
    pub role: String,
    #[arg(long, default_value = "0.1.0")]
    pub version: String,
    #[arg(long, default_value = "")]
    pub summary: String,
    /// 写到哪个目录（默认按包名生成）
    #[arg(long, default_value = "")]
    pub dir: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub struct HurKeyArgs {
    #[command(subcommand)]
    pub cmd: HurKeyCmd,
}

#[derive(Subcommand, Clone)]
pub enum HurKeyCmd {
    /// 生成一对签名密钥（默认不加密；--password 才加密）
    Gen {
        #[arg(long, default_value = "")]
        password: String,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// 看本机密钥（指纹 / 是否加密）+ 受信公钥列表
    Show {
        #[arg(long)]
        json: bool,
    },
    /// 导出公钥（给别人核对）
    Pub {
        #[arg(long, default_value = "")]
        out: String,
    },
    /// 信任一个公钥（.pub 文件或单行 base64）
    Trust {
        source: String,
        #[arg(long, default_value = "")]
        label: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long)]
        json: bool,
    },
    /// 列出受信公钥
    Trusted {
        #[arg(long)]
        json: bool,
    },
    /// 移除受信公钥（按 keynum）
    Untrust { keynum: String },
}

#[derive(Args, Clone)]
pub struct HurAttachArgs {
    /// 包目录（签名从这份工程读；默认当前目录）
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 条目引用：@命名空间/slug[@版本] 或条目 id（留空则按包 id 在自己名下找）
    ///
    /// 用 `--ref` 而不是位置参数：两个位置参数都有默认值时，clap 按顺序填充，
    /// `ncc hur attach .` 里的 `.` 会被当成引用（踩过）。
    #[arg(long = "ref", default_value = "")]
    pub reference: String,
    /// 本机还没有签名时先签一次
    #[arg(long)]
    pub sign: bool,
    /// 签名私钥（默认 `~/.harnessuse/keys/hur.key`；只在 `--sign` 时有意义）
    #[arg(long, default_value = "")]
    pub key: String,
    /// 私钥口令（更推荐环境变量 HUR_KEY_PASSWORD；不做交互式提示）
    #[arg(long, default_value = "")]
    pub password: String,
    /// 核对用的公钥（不给就按 keynum 从本机密钥 / 受信列表里挑）
    #[arg(long = "pub-key", default_value = "")]
    pub pub_key: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HurPublishArgs {
    /// 包目录（会先 verify → pack → 可选 sign）
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 发布时的显示名（默认取包内 name）
    #[arg(long, default_value = "")]
    pub name: String,
    /// slug（默认取包内 id 的小写形式）
    #[arg(long, default_value = "")]
    pub slug: String,
    #[arg(long, default_value = "")]
    pub summary: String,
    #[arg(long, default_value = "")]
    pub tags: String,
    /// 目标命名空间（默认个人命名空间）
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long, default_value = "public", value_parser = ["public", "private"])]
    pub visibility: String,
    /// 以 draft 状态创建
    #[arg(long)]
    pub draft: bool,
    /// 发布前用本机密钥签名（默认：工程策略要求签名时才签）
    #[arg(long)]
    pub sign: bool,
    /// slug 已存在时改为**更新**（改版本/产物地址/状态；清单里的签名与权限面不会变，会提醒）
    #[arg(long)]
    pub update: bool,
    /// 有错也发（默认不：R1~R9 不过就拒绝）
    #[arg(long)]
    pub allow_issues: bool,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 入口 ---------------- */

impl HurCmd {
    /// 这条命令要不要服务器（`None` = 纯本地）。供 `required_capability` 判断。
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            HurCmd::Publish(_) => Some("registry"),
            // 加签要读写 registry 上的条目（上传签名文件 + PUT signature）
            HurCmd::Attach(_) => Some("registry"),
            // 信任**已发布条目**要读 registry（下载条目里的产物与签名去核对）
            HurCmd::Trust(_) => Some("registry"),
            // 装远端条目才需要 registry；装本地 .hur 是纯本地动作
            HurCmd::Install(i) => {
                if Path::new(i.reference.trim()).is_file() {
                    None
                } else {
                    Some("registry")
                }
            }
            // sandbox / dep / env / task / export / policy / mcp 都是**本地只读或本地登记**：不碰服务端
            // （沙箱登记文件在本机；将来若要"从 registry 拉环境清单"再加能力声明）
            _ => None,
        }
    }
}

pub fn run(cfg: &CliConfig, a: &HurCmd) -> Result<()> {
    match a {
        HurCmd::Verify(v) => verify(v.clone()),
        HurCmd::Inspect { path, json } => inspect(path, *json),
        HurCmd::Ls { path, json } => ls(path, *json),
        HurCmd::Build { path } => build(path),
        HurCmd::Pack { path, json } => pack_cmd(path, *json),
        HurCmd::Sign(s) => sign_cmd(s.clone()),
        HurCmd::Init(i) => init(i.clone()),
        HurCmd::Run { path, exec, json } => run_plan(path, *exec, *json),
        HurCmd::Key(k) => key(k.clone()),
        HurCmd::Dep { path, strict, json } => dep_check(path.clone(), *strict, *json),
        HurCmd::Sandbox { path, json } => sandbox(path.clone(), *json),
        HurCmd::Env(e) => env(e.clone()),
        HurCmd::Task(t) => task(t.clone()),
        HurCmd::Export(e) => export(e.clone()),
        HurCmd::Trust(t) => trust_item(cfg, t.clone()),
        HurCmd::Policy(p) => policy_cmd(p.clone()),
        HurCmd::Mcp { list_tools } => hur_mcp(*list_tools),
        HurCmd::Install(i) => install_cmd(cfg, i.clone()),
        HurCmd::Write { path, force, json } => write_cmd(path.clone(), *force, *json),
        HurCmd::Uninstall { id, json } => uninstall_cmd(id, *json),
        HurCmd::List { json } => list_cmd(*json),
        HurCmd::Interop(i) => interop_cmd(i.clone()),
        HurCmd::Publish(p) => publish(cfg, p.clone()),
        HurCmd::Attach(a) => attach(cfg, a.clone()),
    }
}

/* ---------------- 本地：校验 / 读包 / 打包 / 签名 ---------------- */

fn root_of(path: &Path) -> Result<PathBuf> {
    pack::find_root(path)
}

/// 打印检查项；返回 (错误数, 提醒数)。`Info`（如"签名有效"）不算问题。
fn print_issues(issues: &[spec::Issue]) -> (usize, usize) {
    let (mut errs, mut warns) = (0, 0);
    for i in issues {
        match i.level {
            spec::Level::Error => {
                errs += 1;
                println!("  [{} 错误] {}", i.rule, i.msg);
            }
            spec::Level::Warn => {
                warns += 1;
                println!("  [{} 提醒] {}", i.rule, i.msg);
            }
            spec::Level::Info => println!("  [{} 提示] {}", i.rule, i.msg),
        }
    }
    (errs, warns)
}

fn verify(a: HurVerifyArgs) -> Result<()> {
    let is_archive = a.path.extension().and_then(|e| e.to_str()) == Some("hur");
    let mut issues: Vec<spec::Issue> = Vec::new();
    let mut ctx = json!({});
    let dir: PathBuf;
    let mut tmp: Option<PathBuf> = None;

    if is_archive {
        let file = std::fs::canonicalize(&a.path).unwrap_or_else(|_| a.path.clone());
        let sha = spec::sha256_file(&file)?;
        if let Some(expect) = pack::sidecar_sha(&file) {
            if expect != sha {
                issues.push(spec::Issue::err("R6", format!("产物 sha256 与登记值不一致：登记 {expect}，实际 {sha}")));
            }
        }
        let t = std::env::temp_dir().join(format!("ncc-hur-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (files, id, version) = pack::unpack(&file, &t)?;
        dir = t.clone();
        tmp = Some(t);
        let pkg = spec::read_pkg(&dir)?;
        issues.extend(spec::validate(&pkg, &dir, spec::read_lock(&dir).as_ref(), false));
        ctx = json!({ "archive": file, "sha256": sha, "id": id, "version": version, "files": files.len() });
    } else {
        dir = root_of(&a.path)?;
        let pkg = spec::read_pkg(&dir)?;
        issues.extend(spec::validate(&pkg, &dir, spec::read_lock(&dir).as_ref(), false));
        ctx = json!({ "root": dir, "id": pkg.id, "version": pkg.version });
    }

    // R9：签名（要求与否：显式 flag 或生效策略）
    let pkg = spec::read_pkg(&dir)?;
    let pol = policy::resolve(&dir, Some(&pkg))?.policy;
    let required = a.require_signature || policy::effective(&pol).require_signature;
    let pub_override = if a.pub_key.trim().is_empty() { None } else { Some(PathBuf::from(a.pub_key.trim())) };
    let rep = sign::store().check_dir(&dir, &pkg, required, pub_override.as_deref());
    if let Some(i) = &rep.info {
        ctx["signature"] = json!({
            "verified": true, "keynum": i.keynum, "signer": i.signer, "trusted": i.trusted,
            "sha256": i.sha256, "file": i.signature,
        });
    } else if let Some(s) = &rep.signature {
        ctx["signature"] = json!({ "verified": false, "file": s, "required": required });
    }
    ctx["signatureRequired"] = json!(required);
    let summary = rep.summary.clone();
    issues.extend(rep.issues);
    if let Some(t) = &tmp {
        let _ = std::fs::remove_dir_all(t);
    }

    let (errs, warns) = print_issues(&issues);
    let ok = errs == 0 && (!a.strict || warns == 0);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "context": ctx, "ok": ok, "errors": errs, "warnings": warns,
                "issues": issues.iter().map(|i| json!({"rule": i.rule, "level": format!("{:?}", i.level), "msg": i.msg})).collect::<Vec<_>>(),
            }))?
        );
    } else if ok {
        println!("校验通过 ✔  {}", if is_archive { "（产物）" } else { "（工程）" });
        if !summary.is_empty() {
            println!("  签名      {summary}");
        }
    } else {
        println!("校验结果：{errs} 个错误，{warns} 个提醒");
        if !summary.is_empty() {
            println!("  签名      {summary}");
        }
    }
    if !ok {
        bail!("校验未通过（R1~R9）");
    }
    Ok(())
}

fn inspect(path: &Path, json_out: bool) -> Result<()> {
    let dir = root_of(path)?;
    let pkg = spec::read_pkg(&dir)?;
    let r = policy::resolve(&dir, Some(&pkg))?;
    let e = policy::effective(&r.policy);
    let sig = sign::store().check_dir(&dir, &pkg, e.require_signature, None);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "root": dir, "package": pkg, "policy": { "id": r.policy.id, "name": r.policy.name, "sources": r.sources },
                "signature": { "verified": sig.verified, "summary": sig.summary },
            }))?
        );
        return Ok(());
    }
    println!("{}（{} v{}）", pkg.name, pkg.id, pkg.version);
    println!("  类型      {}", pkg.kind);
    println!("  入口      {}", pkg.entry);
    println!("  权限      网络 {:?} · 本地 {:?}", pkg.permissions.network, pkg.permissions.local);
    if let Some(ag) = &pkg.agent {
        println!("  Agent     提示词 {} 字 · 工具 {:?} · 技能 {:?}", ag.system_prompt.chars().count(), ag.tools, ag.skills);
    }
    println!("  策略      {} · {}（{}）", r.policy.id, r.policy.name, r.sources.join(" → "));
    println!("  执行      {}", if e.exec_enabled { format!("允许（引擎 {}）", e.engines.join("+")) } else { "只读（不执行）".into() });
    println!("  签名      {}", if sig.summary.is_empty() { "未签名".into() } else { sig.summary });
    Ok(())
}

fn ls(path: &Path, json_out: bool) -> Result<()> {
    let dir = root_of(path)?;
    let pkg = spec::read_pkg(&dir)?;
    let lock = spec::read_lock(&dir);
    let files: Vec<Value> = spec::content_files(&dir)
        .iter()
        .map(|f| {
            let rel = spec::rel(&dir, f);
            json!({
                "path": rel,
                "sha256": lock.as_ref().and_then(|l| l.files.get(&rel).cloned()),
                "bytes": std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();
    let dist = dir.join(spec::DIST);
    let artifacts: Vec<String> = std::fs::read_dir(&dist)
        .map(|rd| rd.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "package": pkg.id, "version": pkg.version, "files": files, "artifacts": artifacts }))?);
        return Ok(());
    }
    println!("{} v{}（{} 个内容文件）", pkg.id, pkg.version, files.len());
    for f in &files {
        println!("  {:<40} {}", f["path"].as_str().unwrap_or(""), f["sha256"].as_str().unwrap_or("（未锁定）"));
    }
    if !artifacts.is_empty() {
        println!("产物（{}）：{}", dist.display(), artifacts.join(" · "));
    }
    Ok(())
}

fn build(path: &Path) -> Result<()> {
    let dir = root_of(path)?;
    let lock = pack::build_lock(&dir)?;
    println!("已写 {}（{} 个文件已锁定）", dir.join(spec::LOCK).display(), lock.files.len());
    Ok(())
}

fn pack_cmd(path: &Path, json_out: bool) -> Result<()> {
    let dir = root_of(path)?;
    let out = pack::pack(&dir)?;
    // 如果这个目录本身就是"装在本机的包"（有 `_install.json`），顺手刷新登记里的产物 sha256 ——
    // 否则桌面端刚点完"打包"，登记里还写着上一版指纹（下次 `list` 看到的就是错的）。
    if dir.join(install::RECORD_FILE).is_file() {
        if let Ok(rec) = install::register_dir(&dir, None) {
            if json_out {
                println!("{}", serde_json::to_string_pretty(&json!({
                    "file": out.file, "sha256": out.sha256, "bytes": out.bytes,
                    "entries": out.entries, "record": rec,
                }))?);
                return Ok(());
            }
            println!("已打包 {}（{} 字节）", out.file.display(), out.bytes);
            println!("  sha256    {}", out.sha256);
            println!("  登记已刷新  ");
            return Ok(());
        }
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "file": out.file, "sha256": out.sha256, "bytes": out.bytes, "entries": out.entries }))?);
    } else {
        println!("已打包 {}（{} 字节）", out.file.display(), out.bytes);
        println!("  sha256    {}", out.sha256);
    }
    Ok(())
}

fn sign_cmd(a: HurSignArgs) -> Result<()> {
    let s = sign::store();
    let is_archive = a.path.extension().and_then(|e| e.to_str()) == Some("hur");
    let (artifact, pkg) = if is_archive {
        let file = std::fs::canonicalize(&a.path).unwrap_or_else(|_| a.path.clone());
        let t = std::env::temp_dir().join(format!("ncc-hur-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        pack::unpack(&file, &t)?;
        let pkg = spec::read_pkg(&t)?;
        let _ = std::fs::remove_dir_all(&t);
        (file, pkg)
    } else {
        let dir = root_of(&a.path)?;
        let pkg = spec::read_pkg(&dir)?;
        // 签名覆盖规范字节（含 hur.lock）：没有锁就让用户先 build
        let (bytes, name) = pack::archive_bytes(&dir, false).with_context(|| "先 `ncc hur build` 定下 hur.lock，再签名")?;
        let dist = dir.join(spec::DIST);
        std::fs::create_dir_all(&dist)?;
        let out = dist.join(&name);
        std::fs::write(&out, &bytes)?;
        let sha = spec::sha256_hex(&bytes);
        std::fs::write(dist.join(format!("{name}.sha256")), format!("{sha}  {name}\n"))?;
        (out, pkg)
    };
    let sha = spec::sha256_file(&artifact)?;
    let comment = sign::comment_for(&pkg, &sha);
    let password = if a.password.trim().is_empty() {
        std::env::var(sign::ENV_PASSWORD).ok().filter(|p| !p.trim().is_empty())
    } else {
        Some(a.password.clone())
    };
    let key = if a.key.trim().is_empty() { None } else { Some(PathBuf::from(a.key.trim())) };
    let sig = s.sign_file(&artifact, &comment, key.as_deref(), password.as_deref()).map_err(|e| anyhow!("{e:#}"))?;
    let sig = if a.out.trim().is_empty() {
        sig
    } else {
        let want = PathBuf::from(a.out.trim());
        std::fs::rename(&sig, &want).ok();
        want
    };
    let keynum = s.own_key().map(|k| k.keynum).unwrap_or_default();
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "artifact": artifact, "signature": sig, "sha256": sha, "keynum": keynum }))?);
    } else {
        println!("已签名 {} v{}（keynum {keynum}）", pkg.id, pkg.version);
        println!("  产物      {}", artifact.display());
        println!("  签名      {}", sig.display());
        println!("  sha256    {sha}");
        println!("  第三方核对  minisign -V -p {} -m {}", s.public_path().display(), artifact.display());
    }
    Ok(())
}

fn init(a: HurInitArgs) -> Result<()> {
    let input = tpl::InitInput {
        kind: a.kind.clone(),
        name: a.name.clone(),
        role: a.role.clone(),
        domain: a.domain.clone(),
        short: String::new(),
        version: a.version.clone(),
        summary: a.summary.clone(),
        registry: String::new(),
        namespace: String::new(),
    };
    let pkg = tpl::build_package(&input);
    let dir = if a.dir.trim().is_empty() { PathBuf::from(tpl::slug(&pkg.name)) } else { PathBuf::from(a.dir.trim()) };
    if dir.exists() {
        let non_empty = std::fs::read_dir(&dir).map(|rd| rd.flatten().next().is_some()).unwrap_or(false);
        if non_empty && !a.force {
            bail!("目录 {} 已存在且非空（加 --force 覆盖，或换 --dir）", dir.display());
        }
    }
    std::fs::create_dir_all(&dir)?;
    // 清单必须**自己**写：`tpl::files_for` 不含 hur.json（避免和清单双份来源）
    std::fs::write(dir.join(spec::MANIFEST), format!("{}\n", serde_json::to_string_pretty(&pkg)?))?;
    for (rel, body) in tpl::files_for(&pkg) {
        let p = dir.join(&rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, body)?;
    }
    // 锁也顺手建好：签名/打包都要求它存在
    pack::build_lock(&dir)?;
    println!("已生成 {}（{} v{} · {}）", dir.display(), pkg.id, pkg.version, pkg.kind);
    println!("  下一步    ncc hur verify . → ncc hur sign . → ncc hur publish");
    Ok(())
}

fn run_plan(path: &Path, exec: bool, json_out: bool) -> Result<()> {
    let dir = root_of(path)?;
    let pkg = spec::read_pkg(&dir)?;
    let abs = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let r = policy::resolve(&dir, Some(&pkg))?;
    let issues = policy::collect_issues(&dir, &pkg, &r.policy)?;
    let plan = policy::plan(&pkg, &r.policy, &r.sources, &issues);
    // `--exec` 时**只输出一份 JSON**（执行结果里自带 `plan`）：计划一份 + 结果一份连在一起，
    // 没有任何机器能解析—— 而 `--exec --json` 恰恰是桌面端/脚本委派要用的形式（踩过）。
    if json_out && !exec {
        // 与「计划」分开：dir / entry / limits 是**执行**要的东西。
        // 这份 JSON 也是给外部宿主用的（老客户端就靠它拿计划再自己跑沙箱）；
        // 现在 `ncc hur run --exec` 自己就能跑，形状保持不变以免弄坏那些调用方。
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "plan": plan,
                "dir": abs,
                "entry": exec_entry(&pkg),
                "entry_manifest": pkg.entry,
                "package": pkg.id,
                "version": pkg.version,
            }))?
        );
    } else if !exec {
        print!("{}", policy::render_plan(&plan));
    }
    if !plan.allowed {
        bail!("当前策略不允许执行该包（理由见上）");
    }
    if exec {
        return exec_now(&abs, &pkg, &plan, &r.policy, json_out);
    }
    println!("\n（计划阶段；加 `--exec` 就在本机 wasm 沙箱里真跑）");
    Ok(())
}

/// `--exec` 的落地：策略 → 限额 → 真跑 → 留痕（实现见 `hurrun.rs`）。
#[cfg(feature = "sandbox")]
fn exec_now(abs: &Path, pkg: &spec::HurPackage, plan: &policy::RunPlan, pol: &policy::SecurityPolicy, json_out: bool) -> Result<()> {
    // 限额与 trace 落点从**生效策略**来（不另算一套，否则两处口径会漂）
    let e = policy::effective(pol);
    let out = crate::hurrun::exec(abs, pkg, plan, &e, &exec_entry(pkg))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", crate::hurrun::render(&out));
    }
    // 被沙箱拦下的失败也是「跑过了、结论可读」，但退出码必须非 0（脚本靠它判断）
    if !out["outcome"]["ok"].as_bool().unwrap_or(false) {
        bail!("执行未成功（结论见上；留痕已写入，`ncc hur task inspect latest` 可回看）");
    }
    Ok(())
}

/// 没带 sandbox feature 的瘦身构建：如实说清为什么跑不了（**绝不假装跑过**）
#[cfg(not(feature = "sandbox"))]
fn exec_now(_abs: &Path, _pkg: &spec::HurPackage, _plan: &policy::RunPlan, _pol: &policy::SecurityPolicy, _json_out: bool) -> Result<()> {
    bail!(
        "这个 ncc 构建没有本机沙箱运行时（用 `--no-default-features` 编的瘦身版）。\n  \
         要么用官方发布版（默认带 wasm 沙箱），要么交给 harness-use Agent 执行。"
    )
}

fn key(a: HurKeyArgs) -> Result<()> {
    let s = sign::store();
    match a.cmd {
        HurKeyCmd::Gen { password, force, json } => {
            let pw = if password.trim().is_empty() {
                std::env::var(sign::ENV_PASSWORD).ok().filter(|p| !p.trim().is_empty())
            } else {
                Some(password)
            };
            let k = s.gen_key(pw.as_deref(), force).map_err(|e| anyhow!("{e:#}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&k)?);
            } else {
                println!("已生成签名密钥 keynum {}", k.keynum);
                println!("  私钥      {}（0600）", s.secret_path().display());
                println!("  公钥      {}", s.public_path().display());
                if !k.encrypted {
                    println!("  提示      ⚠ 未加密：私钥文件本身就是能力凭据（`--password` 可加密）");
                }
            }
            Ok(())
        }
        HurKeyCmd::Show { json } => {
            let own = s.own_key();
            let trusted = s.trusted();
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "own": own, "trusted": trusted.keys, "trustedFile": s.trusted_file }))?);
                return Ok(());
            }
            match &own {
                Some(k) => println!("本机签名密钥 keynum {}{}", k.keynum, if k.encrypted { "（口令保护）" } else { "（未加密）" }),
                None => println!("本机还没有签名密钥（`ncc hur key gen`）"),
            }
            println!("受信公钥 {} 个（{}）", trusted.keys.len(), s.trusted_file.display());
            for k in &trusted.keys {
                println!("  {}  {}", k.keynum, k.label);
            }
            Ok(())
        }
        HurKeyCmd::Pub { out } => {
            if s.own_key().is_none() {
                bail!("本机还没有密钥：先 `ncc hur key gen`");
            }
            let text = std::fs::read_to_string(s.public_path())?;
            if out.trim().is_empty() {
                print!("{text}");
            } else {
                std::fs::write(out.trim(), &text)?;
                println!("已导出公钥 → {}", out.trim());
            }
            Ok(())
        }
        HurKeyCmd::Trust { source, label, note, json } => {
            let k = s.trust(&source, &label, &note).map_err(|e| anyhow!("{e:#}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&k)?);
            } else {
                println!("已信任 keynum {}（{}）", k.keynum, k.label);
                println!("  注意  没进列表的签名只会被记成「有签名但不可核对」");
            }
            Ok(())
        }
        HurKeyCmd::Trusted { json } => {
            let t = s.trusted();
            if json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else if t.keys.is_empty() {
                println!("受信公钥列表为空（{}）", s.trusted_file.display());
            } else {
                println!("受信公钥 {} 个（{}）", t.keys.len(), s.trusted_file.display());
                for k in &t.keys {
                    println!("  {}  {}", k.keynum, k.label);
                }
            }
            Ok(())
        }
        HurKeyCmd::Untrust { keynum } => {
            println!("{}", if s.untrust(&keynum)? { format!("已移除受信公钥 {keynum}") } else { format!("受信列表里没有 {keynum}") });
            Ok(())
        }
    }
}

/* ---------------- 治理视图：依赖 / 沙箱 / 环境 / 留痕 ---------------- */

/// 依赖对账（`hur_core::dep`）：声明 ↔ 锁 ↔ 实际字节。
fn dep_check(path: PathBuf, strict: bool, json_out: bool) -> Result<()> {
    let dir = root_of(&path)?;
    let pkg = spec::read_pkg(&dir)?;
    let lock = spec::read_lock(&dir);
    let rep = dep::inspect(&dir, &pkg, lock.as_ref())?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&rep)?);
    } else {
        print!("{}", dep::render(&rep));
    }
    if rep.errors > 0 {
        bail!("{} 条依赖有问题（缺失/漂移）", rep.errors);
    }
    if strict && rep.warns > 0 {
        bail!("{} 条依赖需要处理（未解析/未锁定/残留）", rep.warns);
    }
    Ok(())
}

/// 执行入口：优先包声明的 `security.entry`（真正要被沙箱跑的那个文件），
/// 没有就退回清单里的 `entry`（描述性入口，不一定是可执行的字节）。
fn exec_entry(pkg: &spec::HurPackage) -> String {
    let e = pkg.security.as_ref().map(|s| s.entry.clone()).unwrap_or_default();
    if e.trim().is_empty() {
        pkg.entry.clone()
    } else {
        e
    }
}

/// 沙箱治理视图的 JSON 形态（`ncc hur sandbox --json` 与 MCP 工具 `hur_sandbox` 共用）。
/// 写两份 json! 的下场是两处口径慢慢不一致 —— 所以只留这一个出处。
fn sandbox_view(dir: &Path) -> Result<Value> {
    let pkg = spec::read_pkg(dir).ok();
    let r = policy::resolve(dir, pkg.as_ref())?;
    let e = policy::effective(&r.policy);
    let envs = policy::load_envs();
    let here = policy::implemented_engines(); // 本二进制实际托管的引擎（默认构建 = ["wasm"]）
    let traces = read_traces(dir, &e.trace_dir)?;
    Ok(json!({
        "regulator": "ncc hur",
        "hosting": {
            "where": "local",
            "why": "ncc 就是 harness-use 事实上的执行引擎：本机 wasm 沙箱内置于本二进制（`ncc hur run --exec`）；登记/策略/审计仍在本命令",
            "engines_in_this_binary": here,
        },
        "known_engines": policy::ENGINES,
        "ownership": engine_ownership(),
        "policy": { "id": r.policy.id, "name": r.policy.name, "sources": r.sources },
        "limits": {
            "fuel": e.fuel, "memory_mb": e.memory_mb, "stack_kb": e.stack_kb, "wall_ms": e.wall_ms,
            "network": e.network, "max_network_calls": e.max_network_calls,
            "max_output_chars": e.max_output_chars, "readonly_preopen": e.readonly_preopen,
            "trace_dir": e.trace_dir, "retain": e.retain,
        },
        "exec_enabled": e.exec_enabled,
        "engines_allowed": e.engines,
        "environments": envs.environments.iter().map(|x| json!({
            "id": x.id, "kind": x.kind, "url": x.url, "engines": x.engines,
            "attested": x.attestation.present(), "default": envs.default == x.id,
        })).collect::<Vec<_>>(),
        "env_file": policy::env_path(),
        "traces": traces.len(),
    }))
}

/// 沙箱治理视图。**这里回答的是控制面的问题**（有哪些引擎/什么限额/登记了哪些环境/跑过什么），
/// 自身不执行任何代码 —— 执行要走 `ncc hur run --exec`（同一个二进制里的 `hur-sandbox`）。
fn sandbox(path: PathBuf, json_out: bool) -> Result<()> {
    let dir = root_of(&path).unwrap_or_else(|_| path.clone());
    let pkg = spec::read_pkg(&dir).ok();
    let r = policy::resolve(&dir, pkg.as_ref())?;
    let e = policy::effective(&r.policy);
    let envs = policy::load_envs();
    let here = policy::implemented_engines(); // 本二进制实际托管的引擎（默认构建 = ["wasm"]）
    let traces = read_traces(&dir, &e.trace_dir)?;

    if json_out {
        // JSON 形态与 MCP 工具共用同一份（见 `sandbox_view`），免得两处口径漂移
        println!("{}", serde_json::to_string_pretty(&sandbox_view(&dir)?)?);
        return Ok(());
    }

    println!("沙箱治理（登记 / 策略 / 审计）；真执行走 `ncc hur run --exec`");
    println!(
        "  本二进制   {} · 已登记引擎：{}",
        if here.is_empty() { "不托管执行（瘦身构建）".to_string() } else { format!("托管 {}", here.join("+")) },
        if here.is_empty() { "无".to_string() } else { here.join("+") }
    );
    println!("  认识的引擎 {}", policy::ENGINES.join(" / "));
    for (eng, who) in engine_ownership() {
        println!("    {:<10} {}", eng, who);
    }
    println!("  生效策略   {} · {}（{}）", r.policy.id, r.policy.name, r.sources.join(" → "));
    println!(
        "  限额       fuel {} · 内存 {}MB · 栈 {}KB · 墙钟 {}ms · 网络 {}（≤{} 次）· 输出 {} 字",
        e.fuel, e.memory_mb, e.stack_kb, e.wall_ms, e.network, e.max_network_calls, e.max_output_chars
    );
    println!("  执行       {}", if e.exec_enabled { format!("策略允许（引擎 {}）", e.engines.join("+")) } else { "只读（不执行）".into() });
    println!("  已登记环境 {} 个（{}）", envs.environments.len(), policy::env_path().display());
    for x in &envs.environments {
        println!(
            "    {}{:<14} {:<6} {:<26} 引擎 {:<12} 证明 {}",
            if envs.default == x.id { "*" } else { " " },
            x.id,
            x.kind,
            if x.url.is_empty() { "（本机）".into() } else { x.url.clone() },
            if x.engines.is_empty() { "（未声明）".into() } else { x.engines.join("+") },
            if x.attestation.present() { "有" } else { "无" }
        );
    }
    println!("  留痕       {} 条（{}）", traces.len(), trace_dir_of(&dir, &e.trace_dir).display());
    Ok(())
}

/// 引擎的托管归属（写死的产品口径）
fn engine_ownership() -> [(&'static str, &'static str); 5] {
    [
        ("wasm", "本机：ncc 内置的 wasm 沙箱（`ncc hur run --exec`）"),
        ("js", "客户端（本机未内置；`--exec` 会如实说本机没有该引擎）"),
        ("process", "企业自托管（cli/src/sandbox.rs 的清单模型）"),
        ("container", "企业自托管容器运行时（同上；要求签名）"),
        ("remote", "已登记的 Proxy sandbox 环境（本机通过 `ncc hur env` 管登记与证明）"),
    ]
}

/// 沙箱环境登记（`~/.harnessuse/environments.json`）：inspect + 管理
fn env(a: HurEnvArgs) -> Result<()> {
    match a.cmd {
        HurEnvCmd::Ls { json } => {
            let reg = policy::load_envs();
            if json {
                println!("{}", serde_json::to_string_pretty(&reg)?);
                return Ok(());
            }
            if reg.environments.is_empty() {
                println!("还没有登记任何沙箱环境（{}）", policy::env_path().display());
                println!("  例：ncc hur env add corp-wasm --url http://10.0.0.9:8787 --engines wasm --registry http://localhost:8181 --reference @team/sandbox --sha256 …");
                return Ok(());
            }
            println!("已登记沙箱环境 {} 个（{}）", reg.environments.len(), policy::env_path().display());
            for e in &reg.environments {
                println!(
                    "  {}{:<16} {:<6} {:<24} 引擎 {:<14} 证明 {}",
                    if reg.default == e.id { "*" } else { " " },
                    e.id,
                    e.kind,
                    if e.url.is_empty() { "（本机）".into() } else { e.url.clone() },
                    if e.engines.is_empty() { "（未声明）".into() } else { e.engines.join("+") },
                    if e.attestation.present() { "有" } else { "无" }
                );
            }
            println!("  （* = 默认；策略 `require_attestation` 时缺证明的环境不会被选中）");
            Ok(())
        }
        HurEnvCmd::Show { id, json } => {
            let reg = policy::load_envs();
            let e = reg.environments.iter().find(|x| x.id == id).ok_or_else(|| anyhow!("没有登记过环境「{id}」"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(e)?);
                return Ok(());
            }
            println!("{}（{}）{}", e.id, e.kind, if reg.default == e.id { " · 默认" } else { "" });
            println!("  名称     {}", e.name);
            println!("  地址     {}", if e.url.is_empty() { "（本机）".into() } else { e.url.clone() });
            println!("  引擎     {}", if e.engines.is_empty() { "（未声明）".into() } else { e.engines.join("+") });
            println!("  限额     memory {}MB · wall {}ms", e.limits.max_memory_mb, e.limits.max_wall_ms);
            println!("  证明     {}", if e.attestation.present() { format!("{} {}", e.attestation.registry, e.attestation.reference) } else { "无".into() });
            println!("  允许出境 {}", e.allow_data_leaving);
            if !e.note.is_empty() {
                println!("  备注     {}", e.note);
            }
            Ok(())
        }
        HurEnvCmd::Use { id } => {
            policy::set_default_env(&id)?;
            println!("默认沙箱环境已设为 {id}");
            Ok(())
        }
        HurEnvCmd::Add(x) => {
            let engines: Vec<String> = x.engines.split([',', '，', ' ', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let env = policy::Environment {
                id: x.id.clone(),
                name: if x.name.trim().is_empty() { x.id.clone() } else { x.name.clone() },
                kind: x.kind.clone(),
                url: x.url.clone(),
                engines,
                limits: policy::EnvLimits { max_memory_mb: x.max_memory_mb, max_wall_ms: x.max_wall_ms, fuel: 0 },
                attestation: policy::Attestation { registry: x.registry.clone(), reference: x.reference.clone(), sha256: x.sha256.clone(), note: String::new() },
                allow_data_leaving: x.allow_data_leaving,
                registered_at_unix: 0,
                note: x.note.clone(),
            };
            policy::validate_env(&env).map_err(|e| anyhow!("{e:#}"))?;
            let saved = policy::add_env(env, x.default)?;
            if x.json {
                println!("{}", serde_json::to_string_pretty(&saved)?);
            } else {
                println!("已登记沙箱环境 {}（{}）", saved.id, saved.kind);
                println!("  文件  {}", policy::env_path().display());
                if !saved.attestation.present() {
                    println!("  提醒  策略若要求证明（require_attestation），缺证明的环境不会被选中");
                }
            }
            Ok(())
        }
        HurEnvCmd::Rm { id } => {
            println!("{}", if policy::remove_env(&id)? { format!("已移除环境 {id}") } else { format!("没有登记过环境 {id}") });
            Ok(())
        }
    }
}

/// 执行留痕（`audit.trace_dir/*.json`）+ 投递到本机的任务（`~/.harnessuse/tasks/*.json`）
fn task(a: HurTaskArgs) -> Result<()> {
    match a.cmd {
        HurTaskCmd::Ls { path, dir, package, json } => {
            let base = root_of(&path).unwrap_or_else(|_| path.clone());
            let r = policy::resolve(&base, None)?;
            let trace_dir = if dir.trim().is_empty() { trace_dir_of(&base, &policy::effective(&r.policy).trace_dir) } else { PathBuf::from(dir.trim()) };
            let mut traces = read_traces(&base, &trace_dir.to_string_lossy())?;
            if !package.trim().is_empty() {
                traces.retain(|(_, t)| t.package == package.trim());
            }
            let inbox = read_task_inbox()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({
                    "traceDir": trace_dir, "traces": traces, "tasks": inbox,
                }))?);
                return Ok(());
            }
            if traces.is_empty() {
                println!("暂无执行留痕（{}）", trace_dir.display());
                println!("  留痕由 `ncc hur run --exec` 写入（审计策略 audit.trace_dir）—— 这里按时间倒序读回来");
            } else {
                println!("执行留痕 {} 条（最近在前 · {}）", traces.len(), trace_dir.display());
                for (i, (_, t)) in traces.iter().enumerate() {
                    let o = &t.outcome;
                    println!(
                        "  {:>2}. {} {} v{} · 引擎 {} · {} · fuel {} · {}ms{}",
                        i + 1,
                        fmt_ts(t.at_unix),
                        t.package,
                        t.version,
                        t.engine,
                        if o["ok"].as_bool().unwrap_or(false) { "成功" } else { "失败" },
                        o["fuel_used"].as_u64().unwrap_or(0),
                        o["wall_ms"].as_u64().unwrap_or(0),
                        match o["error_kind"].as_str() {
                            Some(k) => format!(" · 拦下：{k}"),
                            None => String::new(),
                        }
                    );
                }
                println!("  看详情：ncc hur task inspect latest（或序号）");
            }
            if !inbox.is_empty() {
                println!("\n投递到本机的任务 {} 条（~/.harnessuse/tasks/ · 由包内 `task.create` 写入）", inbox.len());
                for t in inbox.iter().take(10) {
                    println!("  [{}] {} — {}", t["kind"].as_str().unwrap_or("chat"), t["title"].as_str().unwrap_or(""), t["brief"].as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        HurTaskCmd::Inspect { reference, path, dir, json } => {
            let base = root_of(&path).unwrap_or_else(|_| path.clone());
            let r = policy::resolve(&base, None)?;
            let trace_dir = if dir.trim().is_empty() { trace_dir_of(&base, &policy::effective(&r.policy).trace_dir) } else { PathBuf::from(dir.trim()) };
            let traces = read_traces(&base, &trace_dir.to_string_lossy())?;
            if traces.is_empty() {
                bail!("{} 里没有留痕（还没跑过，或 --dir 指错了）", trace_dir.display());
            }
            let want = reference.trim();
            let picked = if want == "latest" || want.is_empty() {
                traces.first()
            } else if let Ok(n) = want.parse::<usize>() {
                traces.get(n.saturating_sub(1))
            } else {
                // 按**真实文件名**匹配（带不带 .json 都认），并兼容老式 `{时间戳}-{包 id}` 写法
                traces.iter().find(|(name, t)| {
                    name == want
                        || name.trim_end_matches(".json") == want
                        || format!("{}-{}", t.at_unix, t.package) == want
                })
            };
            let (trace_file, t) = picked.ok_or_else(|| anyhow!("找不到留痕「{want}」（用 `ncc hur task ls` 看可选值）"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(t)?);
                return Ok(());
            }
            let o = &t.outcome;
            println!("{} v{}（{}）", t.package, t.version, t.name);
            println!("  文件      {trace_file}");
            println!("  时间      {}", fmt_ts(t.at_unix));
            println!("  策略      {} · 引擎 {}", t.policy, t.engine);
            println!("  结论      {}", if o["ok"].as_bool().unwrap_or(false) { "成功".into() } else { format!("失败（{}）", o["error_kind"].as_str().unwrap_or("?")) });
            println!(
                "  用量      fuel {} · {}ms · 内存 {} · 宿主调用 {}",
                o["fuel_used"].as_u64().unwrap_or(0),
                o["wall_ms"].as_u64().unwrap_or(0),
                o["memory_bytes"].as_u64().map(|b| format!("{}KB", b / 1024)).unwrap_or_else(|| "-".into()),
                o["host_calls"].as_array().map(|a| a.len()).unwrap_or(0)
            );
            println!("  当时的限额 {}", serde_json::to_string(&t.limits).unwrap_or_default());
            for c in o["host_calls"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                println!(
                    "  call {} {} {} — {}",
                    if c["ok"].as_bool().unwrap_or(false) { "✔" } else { "✘" },
                    c["kind"].as_str().unwrap_or(""),
                    c["target"].as_str().unwrap_or(""),
                    c["detail"].as_str().unwrap_or("")
                );
            }
            if let Some(err) = o["error"].as_str() {
                println!("  错误      {err}");
            }
            if let Some(reply) = o["reply"].as_str().filter(|s| !s.is_empty()) {
                println!("  回复      {}", if reply.chars().count() > 400 { format!("{}…", reply.chars().take(400).collect::<String>()) } else { reply.to_string() });
            }
            if let Some(logs) = o["logs"].as_array() {
                for l in logs.iter().filter_map(|x| x.as_str()) {
                    println!("  log       {l}");
                }
            }
            Ok(())
        }
    }
}

fn trace_dir_of(base: &Path, trace_dir: &str) -> PathBuf {
    let p = PathBuf::from(trace_dir);
    if p.is_absolute() { p } else { base.join(p) }
}

/// 读留痕（按时间倒序）。坏文件跳过，不让一条脏记录挡住整个视图。
/// **同时返回文件名**：`task inspect <文件名>` 要靠事实匹配，而不是自己拿
/// `{时间戳}-{包 id}` 去猜（同秒多条时猜不准）。
fn read_traces(_base: &Path, dir: &str) -> Result<Vec<(String, policy::TraceRecord)>> {
    let dir = PathBuf::from(dir);
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut out: Vec<(String, policy::TraceRecord)> = rd
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let text = std::fs::read_to_string(e.path()).ok()?;
            let rec = serde_json::from_str::<policy::TraceRecord>(&text).ok()?;
            Some((name, rec))
        })
        .collect();
    out.sort_by(|a, b| b.1.at_unix.cmp(&a.1.at_unix).then(b.0.cmp(&a.0)));
    Ok(out)
}

/// 包内 `task.create` 投递的任务（宿主工具写进 `~/.harnessuse/tasks/`）
fn read_task_inbox() -> Result<Vec<Value>> {
    let dir = hur_core::cfg::home().join("tasks");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut out: Vec<Value> = rd
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v["created_at"].as_str().unwrap_or("").to_string()));
    Ok(out)
}

fn fmt_ts(unix: u64) -> String {
    // 只做人读近似（不引 chrono）：按 UTC 算到分钟够用
    if unix == 0 {
        return "-".into();
    }
    let secs = unix as i64;
    let days = secs / 86400;
    let rem = secs % 86400;
    let (mut y, mut d) = (1970i64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if d < len {
            break;
        }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && d >= months[m] {
        d -= months[m];
        m += 1;
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}Z", y, m + 1, d + 1, rem / 3600, (rem % 3600) / 60)
}

/* ---------------- 分发 / 信任条目 / 策略 / MCP（P-23） ---------------- */

/// sha256 → 小写 hex（CLI 侧只此一处要用，不为此再引一个依赖）
fn hex_of(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().iter().map(|x| format!("{x:02x}")).collect()
}

/// 逗号分隔列表
fn csv(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// 字符串形式的布尔（true/false/1/0/yes/no/on/off）→ bool。解析不了就报错，不猜。
fn parse_bool(flag: &str, v: &str) -> Result<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" | "on" => Ok(true),
        "false" | "0" | "no" | "n" | "off" => Ok(false),
        other => bail!("--{flag} 只认 true/false（收到「{other}」）"),
    }
}

/// 导出**可分发产物**：`.hur` + `.minisig`（有签名时）+ `.sha256` + `export.json`。
///
/// 为什么不是只给一个 `.hur`：收件人要能**自己**判断"这份字节是谁做的、有没有被换过"。
/// 所以公钥、签名、摘要、当时的策略一起给（export.json 就是那张"说明书"，不用回来问你）。
/// 全程离线；先自己按 R1~R9 验一遍，不过就拒绝导出（`--allow-issues` 可明确覆盖）。
fn export(a: HurExportArgs) -> Result<()> {
    let dir = root_of(&a.path)?;
    let pkg = spec::read_pkg(&dir)?;
    let r = policy::resolve(&dir, Some(&pkg))?;
    let e = policy::effective(&r.policy);
    let issues = policy::collect_issues(&dir, &pkg, &r.policy)?;
    let errs = issues.iter().filter(|i| i.level == spec::Level::Error).count();
    let warns = issues.iter().filter(|i| i.level == spec::Level::Warn).count();
    if errs > 0 && !a.allow_issues {
        print_issues(&issues);
        bail!("有 {errs} 个错误，拒绝导出（修好；或用 --allow-issues 明确覆盖）");
    }

    let out = pack::pack(&dir)?;
    // 签名：显式要求，或工程策略要求。要求了就得真通过——不给"差不多"。
    let need = a.require_signature || e.require_signature;
    let sig = sign::store().check_dir(&dir, &pkg, need, None);
    if need && !sig.verified {
        bail!("要求签名但没通过：{}（先 `ncc hur sign .`）", sig.summary);
    }
    let sig_file = if sig.verified { sign::find_sig(&out.file) } else { None };
    let sig_json = match (&sig.verified, &sig.info) {
        (true, Some(i)) => json!({
            "format": "minisign", "keynum": i.keynum, "signer": i.signer,
            "trusted": i.trusted, "sha256": i.sha256,
        }),
        _ => Value::Null,
    };

    let outdir = PathBuf::from(a.out.trim());
    std::fs::create_dir_all(&outdir).with_context(|| format!("创建 {} 失败", outdir.display()))?;
    let base = format!("{}-{}", pkg.id, pkg.version);
    let art_name = format!("{base}.hur");
    let dest = outdir.join(&art_name);
    std::fs::copy(&out.file, &dest).with_context(|| format!("复制 {} 失败", out.file.display()))?;
    std::fs::write(outdir.join(format!("{art_name}.sha256")), format!("{}  {art_name}\n", out.sha256))?;
    let mut files = vec![art_name.clone(), format!("{art_name}.sha256")];

    // 公钥一起带上：否则收件人还得回来找我们要"keynum 对应的那把钥匙"
    let mut pub_name = String::new();
    if let Some(s) = &sig_file {
        let n = format!("{art_name}.minisig");
        std::fs::copy(s, outdir.join(&n)).with_context(|| format!("复制 {} 失败", s.display()))?;
        files.push(n);
        let store = sign::store();
        if let Some(own) = store.own_key() {
            if sig_json["keynum"].as_str() == Some(own.keynum.as_str()) && store.public_path().is_file() {
                pub_name = format!("{art_name}.pub");
                std::fs::copy(store.public_path(), outdir.join(&pub_name))?;
                files.push(pub_name.clone());
            }
        }
    }

    let record = json!({
        "spec": "harness-use-export/v1",
        "created_at": fmt_ts(policy::now_unix()),
        "tool": { "name": "ncc hur export", "version": env!("CARGO_PKG_VERSION") },
        "package": { "id": pkg.id, "name": pkg.name, "version": pkg.version, "kind": pkg.kind, "entry": pkg.entry },
        "capabilities": pkg.capabilities,
        "permissions": { "network": pkg.permissions.network, "local": pkg.permissions.local },
        "artifact": { "name": art_name, "sha256": out.sha256, "bytes": out.bytes },
        "signature": sig_json,
        "public_key": pub_name,
        "policy": {
            "id": r.policy.id, "name": r.policy.name, "sources": r.sources,
            "engines": e.engines, "exec": e.exec_enabled, "require_signature": e.require_signature,
        },
        "issues": { "errors": errs, "warns": warns },
    });
    std::fs::write(outdir.join("export.json"), format!("{}\n", serde_json::to_string_pretty(&record)?))?;
    files.push("export.json".into());

    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "dir": outdir, "files": files, "export": record }))?);
        return Ok(());
    }
    println!("✅ 已导出到 {}", outdir.display());
    for f in &files {
        println!("   {f}");
    }
    println!("   校验      {}（{} 字节）", out.sha256, out.bytes);
    if sig.verified {
        println!("   签名      ✔ {}", sig.summary);
    } else {
        println!("   签名      未签名（收件人只能核对「有没有被换过」，核不了「谁做的」）");
    }
    println!("\n收件人怎么核对：");
    println!(
        "  ncc hur verify {} --require-signature{}",
        dest.display(),
        if sig.verified { "" } else { "   # 这份没签名，暂时别加 --require-signature" }
    );
    if !pub_name.is_empty() {
        println!("  ncc hur key trust {}   # 先核对再信任", outdir.join(&pub_name).display());
    }
    Ok(())
}

/// 在自己名下按 slug 找条目 id（`--update` 用；服务端没有"按 slug 查我的条目"接口，只能列表筛）
fn find_mine(cfg: &CliConfig, token: &str, slug: &str) -> Result<Option<String>> {
    let d = api::get(cfg, "/api/registry?mine=1&size=100", Some(token))?;
    Ok(d["items"].as_array().and_then(|arr| {
        arr.iter()
            .find(|it| it["slug"].as_str() == Some(slug))
            .and_then(|it| it["id"].as_str())
            .map(|s| s.to_string())
    }))
}

/// 在自己名下按**包 id** 找条目：slug 是发布时定的（可以改），包 id 才是这份工程的稳定身份。
/// 同一 id 有多条时，优先版本一致的那条。
fn find_mine_by_pkg(cfg: &CliConfig, token: &str, pkg_id: &str, version: &str) -> Result<Option<String>> {
    let d = api::get(cfg, "/api/registry?mine=1&size=100", Some(token))?;
    let items = d["items"].as_array().cloned().unwrap_or_default();
    let hits: Vec<&Value> = items
        .iter()
        .filter(|it| it["manifest"]["hur"]["id"].as_str() == Some(pkg_id))
        .collect();
    Ok(hits
        .iter()
        .find(|it| it["version"].as_str() == Some(version))
        .or_else(|| hits.first())
        .and_then(|it| it["id"].as_str())
        .map(|s| s.to_string()))
}

/// 按引用取条目：`@命名空间/slug[@版本]`（列命名空间再挑）或条目 id。
fn fetch_item(cfg: &CliConfig, token: Option<&str>, reference: &str) -> Result<Value> {
    let r = reference.trim();
    if r.is_empty() {
        bail!("给个条目引用：@命名空间/slug（可带 @版本）或条目 id");
    }
    if let Some(rest) = r.strip_prefix('@') {
        let (ns_slug, want_ver) = match rest.rsplit_once('@') {
            Some((a, b)) => (a, Some(b.to_string())),
            None => (rest, None),
        };
        let (ns, slug) = ns_slug
            .split_once('/')
            .ok_or_else(|| anyhow!("引用要写成 @命名空间/slug，收到「{r}」"))?;
        // 命名空间 slug 在服务端是**带 @ 的**（`@p23`）。用户写 `@p23/x` 时 ns 已是 `p23`，
        // 所以这里统一补上 @ 再查；比对时两边都去掉 @，免得"@p23"和"p23"看着一样却不相等。
        let want_ns = if ns.starts_with('@') { ns.to_string() } else { format!("@{ns}") };
        let strip = |s: &str| s.trim_start_matches('@').to_string();
        let mut items = api::get(cfg, &format!("/api/registry?namespace={}&size=100", api::urlenc(&want_ns)), token)?["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if items.is_empty() && want_ns != ns {
            items = api::get(cfg, &format!("/api/registry?namespace={}&size=100", api::urlenc(ns)), token)?["items"]
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
        let hit = items.into_iter().find(|it| {
            strip(it["namespace"]["slug"].as_str().unwrap_or("")) == strip(ns)
                && it["slug"].as_str() == Some(slug)
                && want_ver.as_deref().map(|v| it["version"].as_str() == Some(v)).unwrap_or(true)
        });
        return hit.ok_or_else(|| {
            anyhow!(
                "在命名空间 {ns} 里没找到 {slug}{}",
                want_ver.map(|v| format!("@{v}")).unwrap_or_default()
            )
        });
    }
    let one = api::get(cfg, &format!("/api/registry/{}", api::urlenc(r)), token)?;
    Ok(one["item"].clone())
}

/// `ncc hur trust <条目>`：信任一个**已发布条目**的签名公钥。
///
/// 与 `ncc hur key trust <pub 文件>` 的区别在**顺序**：这里先下载产物与签名、核对通过，
/// 才把公钥写进信任表。反过来的话，「我信谁」就交给了发布方，那不叫信任，叫服从。
/// `--force` 可以跳过核对，但它会在信任表里留下"未核对"的证据。
fn trust_item(cfg: &CliConfig, a: HurTrustArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let item = fetch_item(cfg, token.as_deref(), &a.reference)?;
    let id = item["id"].as_str().unwrap_or("").to_string();
    let full = format!(
        "{}/{}@{}",
        item["namespace"]["slug"].as_str().unwrap_or(""),
        item["slug"].as_str().unwrap_or(""),
        item["version"].as_str().unwrap_or("")
    );
    let hur = item["manifest"]["hur"].clone();
    if hur.is_null() {
        bail!("{full} 不是 hur 条目（manifest 里没有 hur）—— 没有可信任的签名语义");
    }
    if hur["signature"].is_null() {
        bail!("{full} 没带签名：没有公钥可信任（作者可 `ncc hur attach --ref {full}` 补签，或 `ncc hur publish --sign` 重发）");
    }
    let sigv = &hur["signature"];
    let keynum = sigv["keynum"].as_str().unwrap_or("").to_string();
    let art_sha = sigv["sha256"].as_str().unwrap_or("").to_string();
    let sig_url = sigv["url"].as_str().unwrap_or("").to_string();
    let art_url = item["storage"]["url"].as_str().unwrap_or("").to_string();

    // 公钥来源：① 条目里带的（发布时一并写进 manifest）② `--pub-key` 明确指定
    let pubkey = if !a.pub_key.trim().is_empty() {
        let p = PathBuf::from(a.pub_key.trim());
        if p.is_file() {
            std::fs::read_to_string(&p).with_context(|| format!("读 {} 失败", p.display()))?
        } else {
            a.pub_key.clone()
        }
    } else {
        let k = sigv["pubkey"].as_str().unwrap_or("").to_string();
        if k.trim().is_empty() {
            bail!(
                "条目里没带公钥（老版本 ncc 发的）。要么让作者重发，要么 `--pub-key <文件|单行 base64>` 明确指定后再信任"
            );
        }
        k
    };

    let store = sign::store();
    // 先确认这份公钥自己解析得出来、并且和签名里的 keynum 是**同一把钥匙**。
    // 不先验这一步的话，"核对通过"可能是用本机密钥或受信列表里的别的钥匙通过的
    // （`inspect` 会在候选里按 keynum 挑），于是就会把一把**没被核对过**的公钥写进信任表。
    let pk = sign::parse_public(&pubkey)
        .map_err(|e| anyhow!("这份公钥解析失败（应是 minisign 公钥或单行 base64）：{e:#}"))?;
    let pub_keynum = sign::keynum_hex(pk.keynum());
    if !keynum.is_empty() && pub_keynum != keynum {
        bail!("签名 keynum {keynum} 与待信任公钥 {pub_keynum} 不是同一把钥匙 —— 不信任（别用 --pub-key 换掉条目自带的公钥）");
    }
    let mut checked = Value::Null;
    if !a.force {
        if art_url.is_empty() || sig_url.is_empty() {
            bail!(
                "条目没有可直接下载的产物/签名地址（老版本发的）：核对不了就不进信任表。\n\
                 来源确实可信的话用 `--force`，或自己 `--pub-key` + 手工核对"
            );
        }
        let work = if a.dir.trim().is_empty() {
            std::env::temp_dir().join(format!("ncc-hur-trust-{}", std::process::id()))
        } else {
            PathBuf::from(a.dir.trim())
        };
        std::fs::create_dir_all(&work).with_context(|| format!("创建 {} 失败", work.display()))?;
        let art_path = work.join("artifact.hur");
        let sig_path = work.join("artifact.hur.minisig");
        let pub_path = work.join("signer.pub");
        std::fs::write(&art_path, api::get_bytes(cfg, &art_url, None)?)?;
        std::fs::write(&sig_path, api::get_bytes(cfg, &sig_url, None)?)?;
        std::fs::write(&pub_path, format!("{}\n", pubkey.trim()))?;
        // ① 字节是不是清单里说的那份（signature.sha256 在发布时记下，改一个字节就对不上）
        let got = hex_of(&std::fs::read(&art_path)?);
        if !art_sha.is_empty() && got != art_sha {
            bail!("产物 sha256 与清单不一致（清单 {art_sha}，实际 {got}）—— 不信任");
        }
        // ② 是不是这把公钥签的、签的正是这份字节。
        // 注意 `inspect` 的语义：它返回的是**结论**（不抛错），公钥对不上会落到 `UnknownKey`。
        // 所以这里必须显式要求 Verified —— 只看 `is_ok()` 会把「根本不认识这把钥匙」
        // 当成核对通过，那这个命令就成了摆设。
        let outcome = store
            .inspect(&art_path, Some(&sig_path), Some(&pub_path))
            .map_err(|e| anyhow!("核对过程失败：{e:#}"))?;
        let info = match outcome {
            sign::SigOutcome::Verified(i) => i,
            sign::SigOutcome::UnknownKey { keynum: got, .. } => bail!(
                "签名里的 keynum 是 {got}，与这份公钥对不上（清单里写的是 {keynum}）—— 不信任。\n\
                 想用条目自带的公钥就不要传 --pub-key。"
            ),
            sign::SigOutcome::Bad { msg, .. } => {
                bail!("签名核对不通过：{msg} —— 不信任（内容可能被改过，或用错了钥匙）")
            }
            sign::SigOutcome::Unsigned { .. } => bail!("下载下来的产物没有签名 —— 不信任"),
        };
        checked = json!({
            "artifact": art_path, "signature": sig_path, "public_key": pub_path,
            "sha256": got, "keynum": info.keynum, "signer": info.signer,
        });
        if info.keynum != pub_keynum {
            bail!("核对通过的是另一把钥匙（{}），不是要信任的这把（{pub_keynum}）—— 不信任", info.keynum);
        }
    }

    let label = if a.label.trim().is_empty() { full.clone() } else { a.label.trim().to_string() };
    let note = if !a.note.trim().is_empty() {
        a.note.trim().to_string()
    } else if a.force {
        format!("ncc hur trust {id}（--force，未核对）")
    } else {
        format!("ncc hur trust {id}（已核对产物 sha256 {art_sha}）")
    };
    let k = store.trust(&pubkey, &label, &note).map_err(|e| anyhow!("{e:#}"))?;

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "item": id, "reference": full, "keynum": k.keynum, "label": k.label,
                "trusted_file": store.trusted_file, "checked": checked, "forced": a.force,
            }))?
        );
        return Ok(());
    }
    println!("已信任 keynum {}（{}）", k.keynum, k.label);
    println!("  条目       {full}  ({id})");
    println!(
        "  本地核对   {}",
        if a.force { "跳过（--force）".to_string() } else { format!("产物 + 签名（sha256 {art_sha}）") }
    );
    if !keynum.is_empty() && keynum != k.keynum {
        println!("  ⚠ 清单里的 keynum {keynum} 与公钥实际 {} 不一致（以公钥为准）", k.keynum);
    }
    println!("  现在起     这个 keynum 的签名会被判为「可核对」，R9 不再只是提醒");
    println!("  撤销       ncc hur key untrust {}", k.keynum);
    Ok(())
}

/// 把「用户要的值」与「收紧后的实际值」对齐比较 —— 让"写了但没生效"当场可见。
fn eff_of(e: &policy::Effective, field: &str) -> String {
    match field {
        "require_signature" => e.require_signature.to_string(),
        "require_lock" => e.require_lock.to_string(),
        "strict" => e.strict.to_string(),
        "max_errors" => e.max_errors.to_string(),
        "exec" => e.exec_enabled.to_string(),
        "engines" => e.engines.join(","),
        "local_only" => e.local_only.to_string(),
        "network" => e.network.clone(),
        "max_network_calls" => e.max_network_calls.to_string(),
        "max_output_chars" => e.max_output_chars.to_string(),
        "fuel" => e.fuel.to_string(),
        "memory_mb" => e.memory_mb.to_string(),
        "stack_kb" => e.stack_kb.to_string(),
        "wall_ms" => e.wall_ms.to_string(),
        "remote" => e.remote_allowed.to_string(),
        "require_attestation" => e.require_attestation.to_string(),
        "allow_data_leaving" => e.allow_data_leaving.to_string(),
        "trace_dir" => e.trace_dir.clone(),
        "retain" => e.retain.to_string(),
        _ => "—".into(),
    }
}

/// 策略：show / check / path / presets / set
fn policy_cmd(a: HurPolicyArgs) -> Result<()> {
    match a.cmd {
        HurPolicyCmd::Show { path, json } => {
            let dir = root_of(&path).unwrap_or(path);
            let pkg = spec::read_pkg(&dir).ok();
            let r = policy::resolve(&dir, pkg.as_ref())?;
            let e = policy::effective(&r.policy);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "policy": r.policy, "effective": e, "sources": r.sources,
                        "requested": r.requested, "host": r.host, "overridden": r.overridden,
                        "request_stacked": r.request_stacked,
                    }))?
                );
                return Ok(());
            }
            println!("生效策略   {} · {}（{}）", r.policy.id, r.policy.name, r.sources.join(" → "));
            if r.overridden {
                println!("  ⚠ 包内请求的策略「{}」被宿主层更严的策略覆盖", r.requested);
            }
            println!(
                "  verify     required={} strict={} max_errors={} require_lock={} require_signature={}",
                e.verify_required, e.strict, e.max_errors, e.require_lock, e.require_signature
            );
            println!(
                "  exec       enabled={} engines=[{}] local_only={}",
                e.exec_enabled,
                e.engines.join("+"),
                e.local_only
            );
            println!(
                "  沙箱       fuel {} · 内存 {}MB · 栈 {}KB · 墙钟 {}ms · 网络 {}（≤{} 次）· 输出 ≤{} 字",
                e.fuel, e.memory_mb, e.stack_kb, e.wall_ms, e.network, e.max_network_calls, e.max_output_chars
            );
            println!(
                "  远程       允许={} 环境=[{}] 要求证明={} 允许出境={}",
                e.remote_allowed,
                e.remote_environments.join("+"),
                e.require_attestation,
                e.allow_data_leaving
            );
            println!("  审计       trace_dir={} 保留 {} 条", e.trace_dir, e.retain);
            Ok(())
        }
        HurPolicyCmd::Check { path, strict, json } => {
            let dir = root_of(&path)?;
            let pkg = spec::read_pkg(&dir)?;
            let r = policy::resolve(&dir, Some(&pkg))?;
            let v = &r.policy.verify;
            let issues = policy::collect_issues(&dir, &pkg, &r.policy)?;
            let errs = issues.iter().filter(|i| i.level == spec::Level::Error).count();
            let warns = issues.iter().filter(|i| i.level == spec::Level::Warn).count();
            let infos = issues.iter().filter(|i| i.level == spec::Level::Info).count();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "policy": r.policy.id,
                        "sources": r.sources,
                        "require": {
                            "required": v.required.unwrap_or(true),
                            "strict": v.strict.unwrap_or(false),
                            "max_errors": v.max_errors.unwrap_or(0),
                            "require_lock": v.require_lock.unwrap_or(false),
                            "require_signature": v.require_signature.unwrap_or(false),
                        },
                        "errors": errs, "warns": warns, "infos": infos,
                        "issues": issues.iter().map(|i| json!({
                            "rule": i.rule, "level": format!("{:?}", i.level), "msg": i.msg,
                        })).collect::<Vec<_>>(),
                    }))?
                );
            } else {
                println!("策略       {}（{}）", r.policy.id, r.sources.join(" → "));
                println!(
                    "要求       required={} strict={} max_errors={} require_lock={} require_signature={}",
                    v.required.unwrap_or(true),
                    v.strict.unwrap_or(false),
                    v.max_errors.unwrap_or(0),
                    v.require_lock.unwrap_or(false),
                    v.require_signature.unwrap_or(false)
                );
                if issues.is_empty() {
                    println!("检查项     全部通过（R1~R9）");
                } else {
                    print_issues(&issues);
                }
                println!("结论       {errs} 错误 · {warns} 提醒 · {infos} 已核对项");
            }
            if errs > 0 {
                bail!("{errs} 条错误（策略：{}）", r.policy.id);
            }
            if strict && warns > 0 {
                bail!("{warns} 条提醒（--strict 视为失败）");
            }
            Ok(())
        }
        HurPolicyCmd::Path { path, json } => {
            let dir = root_of(&path).unwrap_or(path);
            let machine = policy::machine_policy_path();
            let project = policy::project_policy_path(&dir);
            let found = policy::find_project_policy(&dir);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "machine": machine, "machine_exists": machine.is_file(),
                        "project": project, "project_exists": project.is_file(),
                        "project_in_effect": found,
                        "env": policy::env_path(),
                        "keys": sign::store().trusted_file,
                    }))?
                );
                return Ok(());
            }
            println!("机器层（上限）  {}  {}", machine.display(), if machine.is_file() { "存在" } else { "不存在" });
            println!("工程层（本目录）{}  {}", project.display(), if project.is_file() { "存在" } else { "不存在" });
            match &found {
                Some(p) => println!("实际生效的工程层 {}", p.display()),
                None => println!("实际生效的工程层 无（只有 builtin:strict）"),
            }
            println!("环境登记        {}", policy::env_path().display());
            println!("受信公钥        {}", sign::store().trusted_file.display());
            println!("层序：builtin:strict → 机器层 → 工程层 → 包内 security（**下层只能收紧**）");
            Ok(())
        }
        HurPolicyCmd::Presets { json } => {
            let ps = policy::presets();
            if json {
                println!("{}", serde_json::to_string_pretty(&ps)?);
                return Ok(());
            }
            println!("内置策略预设 {} 个（`ncc hur policy set --preset <id>`）", ps.len());
            for p in &ps {
                let e = policy::effective(p);
                println!(
                    "  {:<16} {:<18} 执行={} 引擎=[{}] 签名要求={} 网络={}",
                    p.id,
                    p.name,
                    e.exec_enabled,
                    e.engines.join("+"),
                    e.require_signature,
                    e.network
                );
                if !p.description.is_empty() {
                    println!("      {}", p.description);
                }
            }
            Ok(())
        }
        HurPolicyCmd::Set(s) => policy_set(s),
    }
}

/// 改一处策略。写**工程层**（默认）或**机器层**（`--machine`）。
/// 写完后重新解析并逐项对照「你要的」与「实际生效的」——分层的意义就是下层只能收紧，
/// 所以"写了更松的值"必须当场说出来，而不是让人以为改上了。
fn policy_set(s: HurPolicySetArgs) -> Result<()> {
    let dir = root_of(&s.path).unwrap_or_else(|_| s.path.clone());
    let pkg = spec::read_pkg(&dir).ok();
    let requested = pkg
        .as_ref()
        .and_then(|p| p.security.as_ref())
        .map(|x| x.policy.trim().to_string())
        .unwrap_or_default();
    let target = if s.machine { policy::machine_policy_path() } else { policy::project_policy_path(&s.path) };
    let existing = policy::load_policy_file(&target)?;
    let mut pol = if !s.preset.trim().is_empty() {
        policy::preset(s.preset.trim())
            .ok_or_else(|| anyhow!("没有内置预设「{}」（`ncc hur policy presets` 看有哪些）", s.preset.trim()))?
    } else if let Some(e) = &existing {
        e.clone()
    } else {
        // 新建时**沿用包内请求的策略 id**（或已有的同名预设）：
        // 否则光写个 id 就把包请求的具名策略静默顶掉了（`请求只记录、不叠加`）。
        let id = if !requested.is_empty() {
            requested.clone()
        } else if s.machine {
            "house".to_string()
        } else {
            "project".to_string()
        };
        let name = policy::preset(&id)
            .map(|p| format!("{}（本层调整）", p.name))
            .unwrap_or_else(|| if s.machine { "本机策略".into() } else { "工程策略".into() });
        policy::SecurityPolicy { spec: policy::POLICY_SPEC.to_string(), id, name, ..Default::default() }
    };
    if !s.id.trim().is_empty() {
        pol.id = s.id.trim().to_string();
    }
    if !s.name.trim().is_empty() {
        pol.name = s.name.trim().to_string();
    }
    if !s.description.trim().is_empty() {
        pol.description = s.description.trim().to_string();
    }

    // 只动显式给了的字段；每一项都记下「要什么」，稍后和生效值对照
    let mut want: Vec<(String, String)> = Vec::new();
    let set_bool = |flag: &str, raw: &str, slot: &mut Option<bool>, want: &mut Vec<(String, String)>| -> Result<()> {
        if raw.trim().is_empty() {
            return Ok(());
        }
        let v = parse_bool(flag, raw)?;
        *slot = Some(v);
        want.push((flag.replace('-', "_"), v.to_string()));
        Ok(())
    };
    set_bool("require-signature", &s.require_signature, &mut pol.verify.require_signature, &mut want)?;
    set_bool("require-lock", &s.require_lock, &mut pol.verify.require_lock, &mut want)?;
    set_bool("strict", &s.strict, &mut pol.verify.strict, &mut want)?;
    if !s.max_errors.trim().is_empty() {
        let v: u32 = s.max_errors.trim().parse().context("--max-errors 要整数")?;
        pol.verify.max_errors = Some(v);
        want.push(("max_errors".into(), v.to_string()));
    }
    set_bool("exec", &s.exec, &mut pol.exec.enabled, &mut want)?;
    set_bool("local-only", &s.local_only, &mut pol.exec.local_only, &mut want)?;
    if !s.engines.trim().is_empty() {
        let list = match s.engines.trim() {
            "none" => Vec::new(),
            "all" => policy::ENGINES.iter().map(|e| e.to_string()).collect(),
            other => csv(other),
        };
        for e in &list {
            if !policy::valid_engine(e) {
                bail!("引擎「{e}」不在词表里：{}", policy::ENGINES.join(" / "));
            }
        }
        want.push(("engines".into(), list.join(",")));
        pol.exec.engines = Some(list);
    }
    if !s.network.trim().is_empty() {
        let n = s.network.trim();
        if n != "none" && n != "declared-only" {
            bail!("--network 只认 none / declared-only（收到「{n}」）");
        }
        pol.sandbox.network = Some(n.to_string());
        want.push(("network".into(), n.to_string()));
    }
    if !s.max_network_calls.trim().is_empty() {
        let v: u32 = s.max_network_calls.trim().parse().context("--max-network-calls 要整数")?;
        pol.sandbox.max_network_calls = Some(v);
        want.push(("max_network_calls".into(), v.to_string()));
    }
    if !s.max_output_chars.trim().is_empty() {
        let v: u32 = s.max_output_chars.trim().parse().context("--max-output-chars 要整数")?;
        pol.exec.max_output_chars = Some(v);
        want.push(("max_output_chars".into(), v.to_string()));
    }
    if !s.fuel.trim().is_empty() {
        let v: u64 = s.fuel.trim().parse().context("--fuel 要整数")?;
        pol.sandbox.fuel = Some(v);
        want.push(("fuel".into(), v.to_string()));
    }
    if !s.memory_mb.trim().is_empty() {
        let v: u32 = s.memory_mb.trim().parse().context("--memory-mb 要整数")?;
        pol.sandbox.memory_mb = Some(v);
        want.push(("memory_mb".into(), v.to_string()));
    }
    if !s.stack_kb.trim().is_empty() {
        let v: u32 = s.stack_kb.trim().parse().context("--stack-kb 要整数")?;
        pol.sandbox.stack_kb = Some(v);
        want.push(("stack_kb".into(), v.to_string()));
    }
    if !s.wall_ms.trim().is_empty() {
        let v: u32 = s.wall_ms.trim().parse().context("--wall-ms 要整数")?;
        pol.sandbox.wall_ms = Some(v);
        want.push(("wall_ms".into(), v.to_string()));
    }
    set_bool("remote", &s.remote, &mut pol.remote.allowed, &mut want)?;
    set_bool("require-attestation", &s.require_attestation, &mut pol.remote.require_attestation, &mut want)?;
    set_bool("allow-data-leaving", &s.allow_data_leaving, &mut pol.remote.allow_data_leaving, &mut want)?;
    if !s.trace_dir.trim().is_empty() {
        pol.audit.trace_dir = Some(s.trace_dir.trim().to_string());
        want.push(("trace_dir".into(), s.trace_dir.trim().to_string()));
    }
    if !s.retain.trim().is_empty() {
        let v: u32 = s.retain.trim().parse().context("--retain 要整数")?;
        pol.audit.retain = Some(v);
        want.push(("retain".into(), v.to_string()));
    }
    if want.is_empty() && s.preset.trim().is_empty() {
        bail!("没给任何要改的项（例：`ncc hur policy set --require-signature true`）；用 `ncc hur policy show` 看现状");
    }

    if s.dry_run {
        println!("（--dry-run：会写到 {}，未落盘）", target.display());
        println!("{}", serde_json::to_string_pretty(&pol)?);
        return Ok(());
    }
    policy::save_policy_file(&target, &pol).map_err(|e| anyhow!("{e:#}"))?;

    // 写完重新解析：分层的意义是「下层只能收紧」，所以要把"写了但没生效"当场说清
    let dir = root_of(&s.path).unwrap_or(s.path.clone());
    let pkg = spec::read_pkg(&dir).ok();
    let r = policy::resolve(&dir, pkg.as_ref())?;
    let e = policy::effective(&r.policy);
    let mut loose: Vec<String> = Vec::new();

    if s.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": target, "policy": pol, "effective": e, "sources": r.sources,
                "overridden": r.overridden, "requested": r.requested, "host": r.host,
                "fields": want.iter().map(|(k, v)| json!({ "field": k, "want": v, "effective": eff_of(&e, k) })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    println!("已写入 {}", target.display());
    if !want.is_empty() {
        println!("逐项对照（你要的 → 实际生效的）：");
        for (k, v) in &want {
            let got = eff_of(&e, k);
            let ok = &got == v || got == v.replace(' ', "");
            if ok {
                println!("  ✔ {k:<20} {v}");
            } else {
                println!("  ⚠ {k:<20} 写了 {v}，实际生效 {got}（上层更严 —— 收紧优先，松的不生效）");
                loose.push(k.clone());
            }
        }
    }
    println!("生效策略   {} · {}（{}）", r.policy.id, r.policy.name, r.sources.join(" → "));
    if r.overridden {
        println!(
            "  ⚠ 本层写的是策略 id「{}」，包内请求的是「{}」—— 宿主选定后包请求**不再叠加**；\n     想保留包请求的那套规则，就把 id 改成「{}」（或干脆不写 id）。",
            r.host, r.requested, r.requested
        );
    }
    if !loose.is_empty() {
        println!("  提醒  {} 项被上层更严的层挡回去了（要放开就先改更靠前的那层）", loose.len());
    }
    Ok(())
}

const HUR_MCP_INSTRUCTIONS: &str = "\
`ncc hur mcp` 暴露的是 **hur 制品的治理面**（控制面），不是执行面。

怎么用：
1. 看清一个包：hur_inspect（清单 / 入口 / 权限面 / 依赖）→ hur_verify（R1~R9，含签名）→ hur_dep（声明↔锁↔实际字节）。
2. 看清约束：hur_policy（生效策略 + 逐层来源 + 限额）；hur_plan（执行计划：允许不允许、用哪个引擎、什么限额、为什么）。
3. 看清环境与留痕：hur_sandbox（引擎托管归属 / 已登记沙箱环境 / 留痕数）；hur_tasks（谁在什么限额下跑了什么、留痕详情）；hur_envs（登记的环境与证明）；hur_keys（本机密钥指纹 + 受信公钥）。

要记住三条事实（别猜）：
- **ncc 自己就能执行**：`ncc hur run --exec` 用**内置的 wasm 沙箱**真跑，限额来自生效策略。
  但这个 MCP **只暴露治理面（只读）**：它不会替你执行任何代码；要真跑请让用户跑 `ncc hur run --exec`。
- **沙箱两面都在 ncc**：治理（登记 / 策略 / 计划 / 审计）与托管（真跑 guest 的 wasm 引擎）在同一个二进制。
  hur_plan 的 engine_ready 与 hur_sandbox 的 engines_in_this_binary 会如实说明本机能不能跑
  （瘦身构建不带沙箱时引擎列表为空、engine_ready=false）。
- **本工具全是只读**。签名、信任公钥、改策略、发布这类会改变「谁能拿到什么 / 谁被信任」的动作，让用户自己跑 CLI
  （ncc hur sign / ncc hur trust / ncc hur policy set / ncc hur publish），并先征得同意。
\n路径参数：package 目录（默认当前目录），会自动向上找 hur.json。";

/// hur 治理面的 MCP 工具表（全读操作）
fn hur_mcp_tools() -> Vec<Value> {
    let path_arg = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "包目录（默认当前目录，自动向上找 hur.json）" }
        },
        "additionalProperties": false
    });
    vec![
        json!({
            "name": "hur_inspect",
            "description": "读一个 hur 包：清单 / 入口 / 权限面（网络与本地目录）/ 依赖声明 / 锁定情况 / 安全策略。先看这个再决定要不要碰它。",
            "inputSchema": path_arg.clone()
        }),
        json!({
            "name": "hur_verify",
            "description": "按生效策略跑 R1~R9 校验（全程本地、不联网、不执行）：R9 是制品签名。返回错误/提醒/已核对项与签名状态（谁签的、可不可核对）。",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "pub_key": { "type": "string", "description": "用指定的公钥核对签名（.pub 文件路径或单行 base64；不给就用本机受信列表）" }
                },
                "additionalProperties": false
            })
        }),
        json!({
            "name": "hur_dep",
            "description": "依赖对账：包内声明 ↔ hur.lock ↔ 实际字节。六种状态 ok/unlocked/missing/drift/unresolved/stale；missing|drift 是错误（字节对不上，说明有人改过）。",
            "inputSchema": path_arg.clone()
        }),
        json!({
            "name": "hur_policy",
            "description": "生效策略（逐层收紧后的结果）：来源（builtin/机器/工程/包内）、签名要求、允许的引擎、沙箱限额、远程与审计设置。想知道\"这个包被允许做什么\"就问它。",
            "inputSchema": path_arg.clone()
        }),
        json!({
            "name": "hur_plan",
            "description": "执行计划（**不执行**）：是否允许、选定引擎、本机是否已实现该引擎、当时的限额、拒绝理由与已核对项。engine_ready=false 时别声称能跑。",
            "inputSchema": path_arg.clone()
        }),
        json!({
            "name": "hur_sandbox",
            "description": "沙箱治理视图：各引擎的托管归属（谁跑 wasm/container）、生效策略与限额、已登记的沙箱环境与证明、留痕条数。ncc 自带 wasm 执行引擎，真跑用 `ncc hur run --exec`。",
            "inputSchema": path_arg.clone()
        }),
        json!({
            "name": "hur_tasks",
            "description": "执行留痕与投递任务：最近谁在什么限额下跑了什么（成败/用量/宿主调用/错误），以及包内 task.create 投递到本机的任务。",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "dir": { "type": "string", "description": "直接指定留痕目录（覆盖策略里的 audit.trace_dir）" },
                    "limit": { "type": "integer", "description": "最多返回几条留痕（默认 20，上限 200）" }
                },
                "additionalProperties": false
            })
        }),
        json!({
            "name": "hur_envs",
            "description": "已登记的沙箱环境（远程 Proxy sandbox）：地址、提供的引擎、限额、证明（registry + 引用 + sha256）、是否允许数据出境、默认是哪一个。",
            "inputSchema": json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }),
        json!({
            "name": "hur_keys",
            "description": "本机签名密钥的指纹（是否口令保护）+ 受信公钥列表。**只给指纹与标签，不给私钥**。",
            "inputSchema": json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }),
    ]
}

/// MCP 工具的文本结果（超长截断，别把整包内容灌进模型上下文）
fn mcp_text(v: Value) -> String {
    let s = match v {
        Value::String(s) => s,
        other => serde_json::to_string_pretty(&other).unwrap_or_else(|_| "（无法序列化）".into()),
    };
    const MAX: usize = 24000;
    if s.chars().count() > MAX {
        format!("{}…（已截断）", s.chars().take(MAX).collect::<String>())
    } else {
        s
    }
}

fn hur_mcp_call(params: Option<&Value>) -> Result<Value> {
    let p = params.cloned().unwrap_or(json!({}));
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let args = p.get("arguments").cloned().unwrap_or(json!({}));
    let sarg = |k: &str| -> String {
        args.get(k).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).unwrap_or_default()
    };

    let res: Result<Value> = (|| {
        let raw = sarg("path");
        let path = if raw.is_empty() { PathBuf::from(".") } else { PathBuf::from(raw) };
        let dir = root_of(&path).unwrap_or_else(|_| path.clone());
        match name.as_str() {
            "hur_inspect" => {
                let pkg = spec::read_pkg(&dir)?;
                let r = policy::resolve(&dir, Some(&pkg))?;
                let lock = spec::read_lock(&dir);
                Ok(json!({
                    "dir": dir,
                    "package": {
                        "id": pkg.id, "name": pkg.name, "version": pkg.version, "kind": pkg.kind,
                        "summary": pkg.summary, "entry": pkg.entry, "capabilities": pkg.capabilities,
                    },
                    "permissions": { "network": pkg.permissions.network, "local": pkg.permissions.local },
                    "security": pkg.security,
                    "deps": pkg.deps.iter().into_iter().map(|(k, r)| json!({ "kind": k, "reference": r })).collect::<Vec<_>>(),
                    "locked": lock.is_some(),
                    "policy": { "id": r.policy.id, "name": r.policy.name, "sources": r.sources },
                }))
            }
            "hur_verify" => {
                let pkg = spec::read_pkg(&dir)?;
                let r = policy::resolve(&dir, Some(&pkg))?;
                let issues = policy::collect_issues(&dir, &pkg, &r.policy)?;
                let pk = sarg("pub_key");
                let sig = sign::store().check_dir(
                    &dir,
                    &pkg,
                    r.policy.verify.require_signature.unwrap_or(false),
                    if pk.is_empty() { None } else { Some(Path::new(&pk)) },
                );
                Ok(json!({
                    "dir": dir, "policy": r.policy.id,
                    "errors": issues.iter().filter(|i| i.level == spec::Level::Error).count(),
                    "warns": issues.iter().filter(|i| i.level == spec::Level::Warn).count(),
                    "infos": issues.iter().filter(|i| i.level == spec::Level::Info).count(),
                    "issues": issues.iter().map(|i| json!({ "rule": i.rule, "level": format!("{:?}", i.level), "msg": i.msg })).collect::<Vec<_>>(),
                    "signature": { "verified": sig.verified, "summary": sig.summary, "info": sig.info },
                }))
            }
            "hur_dep" => {
                let pkg = spec::read_pkg(&dir)?;
                let lock = spec::read_lock(&dir);
                let rep = dep::inspect(&dir, &pkg, lock.as_ref())?;
                serde_json::to_value(&rep).map_err(|e| anyhow!("{e}"))
            }
            "hur_policy" => {
                let pkg = spec::read_pkg(&dir).ok();
                let r = policy::resolve(&dir, pkg.as_ref())?;
                Ok(json!({
                    "dir": dir, "policy": r.policy, "effective": policy::effective(&r.policy),
                    "sources": r.sources, "requested": r.requested, "host": r.host,
                    "overridden": r.overridden, "request_stacked": r.request_stacked,
                    "files": {
                        "machine": policy::machine_policy_path(),
                        "project": policy::project_policy_path(&dir),
                        "project_in_effect": policy::find_project_policy(&dir),
                    },
                }))
            }
            "hur_plan" => {
                let pkg = spec::read_pkg(&dir)?;
                let r = policy::resolve(&dir, Some(&pkg))?;
                let issues = policy::collect_issues(&dir, &pkg, &r.policy)?;
                let plan = policy::plan(&pkg, &r.policy, &r.sources, &issues);
                Ok(json!({
                    "dir": dir, "entry": exec_entry(&pkg), "entry_manifest": pkg.entry, "plan": plan,
                    "note": "这份是执行计划（只读）。要真跑用 `ncc hur run --exec`；本 MCP 不执行任何代码，engine_ready 会如实说明本机有没有该引擎。",
                }))
            }
            "hur_sandbox" => sandbox_view(&dir),
            "hur_tasks" => {
                let r = policy::resolve(&dir, None)?;
                let d = sarg("dir");
                let trace_dir = if d.is_empty() {
                    trace_dir_of(&dir, &policy::effective(&r.policy).trace_dir)
                } else {
                    PathBuf::from(d)
                };
                let mut traces = read_traces(&dir, &trace_dir.to_string_lossy())?;
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).clamp(1, 200) as usize;
                let total = traces.len();
                traces.truncate(limit);
                Ok(json!({
                    "traceDir": trace_dir, "total": total, "returned": traces.len(),
                    // 文件名单独给一份（按 `inspect <文件名>` 回看要用），记录本身形状不变
                    "traceFiles": traces.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                    "traces": traces.iter().map(|(_, t)| t).collect::<Vec<_>>(),
                    "tasks": read_task_inbox()?,
                }))
            }
            "hur_envs" => {
                let reg = policy::load_envs();
                Ok(json!({ "file": policy::env_path(), "registry": reg }))
            }
            "hur_keys" => {
                let s = sign::store();
                Ok(json!({
                    "secret_path": s.secret_path(), "public_path": s.public_path(),
                    "own": s.own_key(), "trusted": s.trusted().keys, "trusted_file": s.trusted_file,
                    "note": "只给指纹与标签；私钥不出本机。要信任某人的公钥：ncc hur key trust <pub>（或 ncc hur trust <条目>）。",
                }))
            }
            other => bail!("没有这个工具：{other}"),
        }
    })();

    Ok(match res {
        Ok(v) => json!({ "content": [{ "type": "text", "text": mcp_text(v) }], "isError": false }),
        Err(e) => json!({ "content": [{ "type": "text", "text": format!("调用失败：{e:#}") }], "isError": true }),
    })
}

/// `ncc hur mcp`：把 hur 治理面暴露成 MCP 工具（stdio）。
/// 与 `ncc mcp`（registry 工具面）共用同一套收发实现（`mcp::serve_face`）。
/// `--list-tools` 只把工具表打出来（JSON）—— 客户端（如 harness-use GUI）要生成
/// `.mcp.json` 提示时用得上，免得把工具名再抄一份到客户端里。
fn hur_mcp(list_tools: bool) -> Result<()> {
    if list_tools {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "server": "ncc-hur",
                "command": "ncc",
                "args": ["hur", "mcp"],
                "instructions": HUR_MCP_INSTRUCTIONS,
                "tools": hur_mcp_tools().iter().map(|t| json!({
                    "name": t["name"], "description": t["description"],
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    eprintln!("[ncc-hur-mcp] ready · hur 治理面（只读）· 等待 MCP 客户端握手");
    mcp::serve_face(
        mcp::Face {
            name: "ncc-hur",
            instructions: HUR_MCP_INSTRUCTIONS.to_string(),
            tools: hur_mcp_tools(),
        },
        hur_mcp_call,
    )
}

/* ---------------- 安装 / 互操作导出（桌面端的同一套落点与产物） ---------------- */

/// 装包：`ncc hur install @ns/slug[@ver] | <条目 id> | <本地 .hur>`。
///
/// 落点与桌面端**同一个**：`~/.harnessuse/packages/<id>/`（`HUR_HOME` 可覆盖），并顺手登记进
/// 桌面端 `config.json` 的 `packages[]` —— 这样 CLI 装完，客户端不用重启也能看到。
/// 两条硬条件：① 字节要合上登记的 sha256；② 条目带签名时签名必须对得上
///（"本机认不认识这把钥匙"另说：不认识只提醒，信任与否是用户自己的事）。
fn install_cmd(cfg: &CliConfig, a: HurInstallArgs) -> Result<()> {
    let r = a.reference.trim();
    if r.is_empty() {
        bail!("给个引用：@命名空间/slug（可带 @版本）、条目 id，或本地 .hur 路径");
    }
    let local = PathBuf::from(r);
    if local.is_file() {
        let rec = install::install_archive(
            &local,
            if a.sha256.trim().is_empty() { None } else { Some(a.sha256.trim()) },
            None,
            a.force,
        )
        .map_err(|e| anyhow!("{e:#}"))?;
        return report_install(&rec, None, a.json);
    }

    // 远端条目：用 ncc 自己的目标与登录态取（不碰 hur 自己的 registry 配置）
    let token = config::token_opt(cfg);
    let item = fetch_item(cfg, token.as_deref(), r)?;
    let hur = item["manifest"]["hur"].clone();
    if hur.is_null() {
        bail!("{r} 不是 hur 条目（manifest 里没有 hur）—— 装不了");
    }
    let url = item["storage"]["url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        bail!("条目没有可下载的产物地址");
    }
    let sha = if a.sha256.trim().is_empty() {
        item["storage"]["sha256"].as_str().unwrap_or("").to_string()
    } else {
        a.sha256.trim().to_string()
    };
    let fname = hur["artifact"]["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "{}-{}.hur",
                item["slug"].as_str().unwrap_or("pkg"),
                item["version"].as_str().unwrap_or("0")
            )
        });

    let work = std::env::temp_dir().join(format!("ncc-hur-install-{}", std::process::id()));
    std::fs::create_dir_all(&work).with_context(|| format!("创建 {} 失败", work.display()))?;
    let file = work.join(&fname);
    std::fs::write(&file, api::get_bytes(cfg, &url, None)?)?;

    let mut sig_state = Value::Null;
    let sigv = hur["signature"].clone();
    if !sigv.is_null() {
        let sig_url = sigv["url"].as_str().unwrap_or("").to_string();
        let pubkey = sigv["pubkey"].as_str().unwrap_or("").to_string();
        if !sig_url.is_empty() && !pubkey.trim().is_empty() {
            let sig_path = work.join(format!("{fname}.minisig"));
            let pub_path = work.join("signer.pub");
            std::fs::write(&sig_path, api::get_bytes(cfg, &sig_url, None)?)?;
            std::fs::write(&pub_path, format!("{}\n", pubkey.trim()))?;
            let outcome = sign::store()
                .inspect(&file, Some(&sig_path), Some(&pub_path))
                .map_err(|e| anyhow!("核对签名失败：{e:#}"))?;
            let (ok, detail) = match &outcome {
                sign::SigOutcome::Verified(i) => (true, format!("签名有效（keynum {}）", i.keynum)),
                sign::SigOutcome::UnknownKey { keynum, .. } => (
                    true,
                    format!("签名在，但本机不认识 keynum {keynum}（要核对就先 `ncc hur trust …`）"),
                ),
                sign::SigOutcome::Bad { msg, .. } => (false, format!("签名不通过：{msg}")),
                sign::SigOutcome::Unsigned { .. } => (false, "条目登记了签名但没有可用的签名文件".to_string()),
            };
            if !ok && !a.allow_bad_signature {
                bail!("{detail} —— 拒绝安装（确认来源可信后可加 --allow-bad-signature）");
            }
            sig_state = json!({ "verified": ok, "detail": detail });
        } else {
            sig_state = json!({
                "verified": false,
                "detail": "条目只有指纹，没有可核对的签名文件（老版本发布的）",
            });
        }
    }

    let rec = install::install_archive(
        &file,
        if sha.is_empty() { None } else { Some(&sha) },
        Some(install::Source { registry: cfg.base_url(), slug: r.to_string() }),
        a.force,
    )
    .map_err(|e| anyhow!("{e:#}"))?;
    report_install(&rec, Some(sig_state), a.json)
}

/// 把工程目录写进包落点（`~/.ncc/packages/<id>/`）并登记。
/// 桌面端「写入本机」就是这一步（它在临时目录里拼好文件再落盘）；与 `install` 的区别：
/// 这里不产包、不碰网络 —— 只是把目录放到正确的位置并把登记补上。
fn write_cmd(path: PathBuf, force: bool, json_out: bool) -> Result<()> {
    let src = root_of(&path)?;
    let pkg = spec::read_pkg(&src)?;
    let dest = hur_core::cfg::packages_dir().join(&pkg.id);
    let same = match (std::fs::canonicalize(&src), std::fs::canonicalize(&dest)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same {
        if dest.exists() {
            if !force {
                bail!("已装 {}（{}）—— 要覆盖请加 --force", pkg.id, dest.display());
            }
            std::fs::remove_dir_all(&dest).with_context(|| format!("清理旧版本 {} 失败", dest.display()))?;
        }
        copy_tree(&src, &dest)?;
    }
    let rec = install::register_dir(&dest, None).map_err(|e| anyhow!("{e:#}"))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "record": rec, "dir": dest, "copied": !same }))?);
        return Ok(());
    }
    println!("✅ 已写入 {}（{} v{}）", dest.display(), pkg.name, pkg.version);
    println!("  登记      {}", dest.join(install::RECORD_FILE).display());
    println!("  下一步    ncc hur pack {}（产包）· ncc hur verify {}", dest.display(), dest.display());
    Ok(())
}

/// 递归复制目录（跳过 `dist/` 与旧的登记文件：产物由 `pack` 重生成，登记由 `register_dir` 重写）
fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("创建 {} 失败", dest.display()))?;
    for e in std::fs::read_dir(src).with_context(|| format!("读 {} 失败", src.display()))?.flatten() {
        let from = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name == "dist" || name == install::RECORD_FILE {
            continue;
        }
        let to = dest.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).with_context(|| format!("复制 {} 失败", from.display()))?;
        }
    }
    Ok(())
}

fn report_install(rec: &install::InstallRecord, sig: Option<Value>, json_out: bool) -> Result<()> {
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "record": rec,
                "signature": sig.unwrap_or(Value::Null),
                "packagesDir": hur_core::cfg::packages_dir(),
            }))?
        );
        return Ok(());
    }
    println!("✅ 已安装 {} {} v{}（{}）", rec.name, rec.kind, rec.version, rec.id);
    println!("  目录      {}", rec.path);
    if !rec.registry.is_empty() {
        println!("  来源      {}  {}", rec.registry, rec.slug);
    }
    println!("  校验      sha256 {}", rec.sha256);
    if let Some(d) = sig.as_ref().and_then(|s| s["detail"].as_str()) {
        println!("  签名      {d}");
    }
    println!("  下一步    ncc hur verify {} · ncc hur policy show {}", rec.path, rec.path);
    Ok(())
}

/// 卸载：删目录 + 从桌面端登记里移除
fn uninstall_cmd(id: &str, json_out: bool) -> Result<()> {
    let id = id.trim();
    install::uninstall(id).map_err(|e| anyhow!("{e:#}"))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "id": id, "removed": true }))?);
    } else {
        println!("已卸载 {id}");
    }
    Ok(())
}

/// 列出已安装的包
fn list_cmd(json_out: bool) -> Result<()> {
    let dir = hur_core::cfg::packages_dir();
    let recs = install::list_installed();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "dir": dir, "packages": recs }))?);
        return Ok(());
    }
    if recs.is_empty() {
        println!("本机还没有安装任何包（{}）", dir.display());
        println!("  装一个    ncc hur install @命名空间/slug");
        return Ok(());
    }
    println!("已安装 {} 个包（{}）", recs.len(), dir.display());
    for r in &recs {
        println!(
            "  {:<30} {:<8} v{:<10} {}{}",
            r.id,
            r.kind,
            r.version,
            r.path,
            if r.enabled { "" } else { "  （已禁用）" }
        );
    }
    Ok(())
}

/// 互操作导出：把包渲染成别的宿主能直接用的产物（Claude / Cursor / Cline / Codex / MCP）。
/// 默认**只预览**：写完要不要落盘是用户的决定（`--write`）。
fn interop_cmd(a: HurInteropArgs) -> Result<()> {
    let dir = root_of(&a.path)?;
    let pkg = spec::read_pkg(&dir)?;
    let targets = interop::parse_targets(&a.targets).map_err(|e| anyhow!("{e:#}"))?;
    let files: Vec<(String, String)> = spec::content_files(&dir)
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (spec::rel(&dir, &p), t)))
        .collect();
    let arts = interop::render(&pkg, &files, &targets).map_err(|e| anyhow!("{e:#}"))?;
    let out = if a.out.trim().is_empty() { ".".to_string() } else { a.out.trim().to_string() };

    if a.write {
        let written = interop::apply(Path::new(&out), &arts).map_err(|e| anyhow!("{e:#}"))?;
        if a.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "package": pkg.id, "targets": targets, "out": out,
                    "written": written, "count": arts.len(),
                }))?
            );
        } else {
            println!("已写入 {} 个产物到 {out}", written.len());
            for p in &written {
                println!("  {}", p.display());
            }
        }
        return Ok(());
    }

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "package": pkg.id, "targets": targets, "out": out,
                "artifacts": arts.iter().map(|x| json!({
                    "target": x.target, "path": x.path, "mode": x.mode_label(),
                    "bytes": x.content.len(), "content": x.content,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    // hur-core 的提示文案里写的是独立二进制的名字（`hur install …`）；
    // 在 ncc 里把它说成 `ncc hur …`，免得用户照着敲却找不到 `hur` 这个命令。
    let plan = interop::render_plan(&pkg, &arts, &out)
        .replace("`hur ", "`ncc hur ")
        .replace("：hur ", "：ncc hur ");
    print!("{plan}");
    Ok(())
}

/* ---------------- 联网：发布（用 ncc 的身份与条目模型） ---------------- */

/// `ncc hur publish` = 本地 verify(R1~R9) → pack（确定性）→ 可选 sign → 走 **ncc 的 registry 条目模型**。
///
/// 刻意与 `ncc publish` 共用同一条上传/建档路径（`/api/registry/uploads` + `/api/registry`），
/// 只是 kind 固定为 `hur`、manifest 里带 hur 包元数据与签名指纹 —— 这样目录侧能显示"谁签的"。
fn publish(cfg: &CliConfig, a: HurPublishArgs) -> Result<()> {
    let dir = root_of(&a.path)?;
    let pkg = spec::read_pkg(&dir)?;
    let r = policy::resolve(&dir, Some(&pkg))?;
    let e = policy::effective(&r.policy);

    // ① 先校验（本地）：R1~R9。不过就不发（除非 --allow-issues，且错误数为 0 也不行）
    let issues = policy::collect_issues(&dir, &pkg, &r.policy)?;
    let errs = issues.iter().filter(|i| i.level == spec::Level::Error).count();
    let warns = issues.iter().filter(|i| i.level == spec::Level::Warn).count();
    if errs > 0 || warns > 0 {
        println!("本地校验：{errs} 个错误，{warns} 个提醒");
        print_issues(&issues);
    }
    if errs > 0 && !a.allow_issues {
        bail!("有 {errs} 个错误，拒绝发布（修好；或用 --allow-issues 明确覆盖）");
    }

    // ② 打包（确定性字节）
    let out = pack::pack(&dir)?;

    // ③ 签名：显式 --sign，或工程策略要求签名时（要求签名却没签 → 拒绝）
    let need_sig = a.sign || e.require_signature;
    let mut sig_info: Option<Value> = None;
    let mut sig = sign::store().check_dir(&dir, &pkg, need_sig, None);
    if need_sig && !sig.verified {
        let comment = sign::comment_for(&pkg, &out.sha256);
        let path = sign::store()
            .sign_file(&out.file, &comment, None, std::env::var(sign::ENV_PASSWORD).ok().as_deref())
            .map_err(|e| anyhow!("签名失败：{e:#}\n（没有密钥就先 `ncc hur key gen`）"))?;
        println!("已签名 {}", path.display());
        sig = sign::store().check_dir(&dir, &pkg, true, None);
    }
    if sig.verified {
        if let Some(i) = &sig.info {
            sig_info = Some(json!({
                "format": "minisign", "keynum": i.keynum, "signer": i.signer,
                "trusted": i.trusted, "sha256": i.sha256,
            }));
        }
    } else if e.require_signature {
        bail!("生效策略要求签名（verify.require_signature），但签名没通过：{}", sig.summary);
    }

    // ④ 上传 + 建档（ncc 的身份与 registry）
    let token = config::require_token(cfg)?;

    // ③b 签名不止写进清单：**公钥与 .minisig 字节一起上传**。
    // 目录侧只写 keynum 的话，下载方只能"相信我们自述的指纹"——那是叙述，不是证据。
    // 带上公钥 + 签名，收件人（或 `ncc hur trust <条目>`）就能独立核对。
    if let Some(info) = sig_info.as_mut() {
        let store = sign::store();
        if store.public_path().is_file() {
            if let Ok(pk) = std::fs::read_to_string(store.public_path()) {
                if !pk.trim().is_empty() {
                    info["pubkey"] = json!(pk.trim());
                }
            }
        }
        if let Some(sigfile) = sign::find_sig(&out.file) {
            match std::fs::read(&sigfile) {
                Ok(sb) => {
                    let sname = sigfile.file_name().and_then(|s| s.to_str()).unwrap_or("pkg.hur.minisig").to_string();
                    match api::request(cfg, "POST", "/api/registry/uploads", Some(&token), None, Some(&sb), &[("X-Filename", &sname)]) {
                        Ok(sup) => {
                            info["url"] = sup["storageUrl"].clone();
                            info["sigSha256"] = sup["sha256"].clone();
                            info["sigBytes"] = sup["size"].clone();
                            info["name"] = json!(sname);
                        }
                        Err(err) => {
                            // 签名传不上去不该让整次发布「看起来成功了却没签名可核对」
                            println!("⚠ 签名文件上传失败（{err:#}）：条目只有指纹，下载方核对不了\n  本地仍保留 {}，可手动分发", sigfile.display());
                        }
                    }
                }
                Err(err) => println!("⚠ 读签名文件失败（{err}）：条目只有指纹"),
            }
        }
    }

    let bytes = std::fs::read(&out.file).with_context(|| format!("读 {} 失败", out.file.display()))?;
    let fname = out.file.file_name().and_then(|s| s.to_str()).unwrap_or("pkg.hur").to_string();
    let up = api::request(cfg, "POST", "/api/registry/uploads", Some(&token), None, Some(&bytes), &[("X-Filename", &fname)])?;
    let storage_url = up["storageUrl"].as_str().unwrap_or("").to_string();
    let sha = up["sha256"].as_str().unwrap_or(&out.sha256).to_string();
    let size = up["size"].as_i64().unwrap_or(bytes.len() as i64);

    let mut body = json!({
        "kind": "hur",
        "name": if a.name.trim().is_empty() { pkg.name.clone() } else { a.name.trim().to_string() },
        "slug": if a.slug.trim().is_empty() { pkg.id.to_lowercase() } else { a.slug.trim().to_string() },
        "version": pkg.version,
        "summary": if a.summary.trim().is_empty() { pkg.summary.clone() } else { a.summary.trim().to_string() },
        "tags": a.tags.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "status": if a.draft { "draft" } else { "published" },
        "visibility": a.visibility,
        "storage": { "url": storage_url, "sha256": sha, "size": size },
        // 目录侧要看的 hur 元数据（规范 / 入口 / 权限面 / 产物摘要 / 签名指纹）
        "manifest": {
            "hur": {
                "spec": spec::PKG_SPEC,
                "id": pkg.id, "kind": pkg.kind, "entry": pkg.entry,
                "capabilities": pkg.capabilities,
                "permissions": { "network": pkg.permissions.network, "local": pkg.permissions.local },
                "security": pkg.security,
                "artifact": { "name": fname, "sha256": out.sha256, "bytes": out.bytes },
                "signature": sig_info,
                "policy": { "id": r.policy.id, "engines": e.engines, "exec": e.exec_enabled },
            }
        },
    });
    if !a.namespace.trim().is_empty() {
        let mine = api::get(cfg, "/api/namespaces/mine", Some(&token))?;
        let hit = mine["namespaces"].as_array().and_then(|arr| arr.iter().find(|x| x["slug"].as_str() == Some(a.namespace.trim())));
        match hit.and_then(|n| n["id"].as_str()) {
            Some(id) => body["namespaceId"] = json!(id),
            None => bail!("你无权使用 namespace {}", a.namespace.trim()),
        }
    }
    // 建档。slug 撞了且给了 `--update` → 在自己名下找到那条改它。
    // ⚠️ PATCH 只认 version / status / storage 三样：**清单里的签名与权限面不会跟着变** ——
    // 改过签名就得重发（或用新 slug/版本），不能假装"更新一下"就换了个签名。
    let (data, updated) = match api::post_json(cfg, "/api/registry", Some(&token), &body) {
        Ok(d) => (d, false),
        Err(e) if a.update => {
            let slug = body["slug"].as_str().unwrap_or("").to_string();
            let id = find_mine(cfg, &token, &slug)?
                .ok_or_else(|| anyhow!("{e:#}（也没在自己名下找到 slug「{slug}」，无法 --update）"))?;
            let patched = api::request(
                cfg,
                "PATCH",
                &format!("/api/registry/{}", api::urlenc(&id)),
                Some(&token),
                Some(&json!({
                    "version": body["version"],
                    "status": body["status"],
                    "storage": body["storage"],
                })),
                None,
                &[],
            )?;
            (patched, true)
        }
        Err(e) => return Err(e),
    };
    let it = &data["item"];
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "item": it, "artifact": out.file, "sha256": out.sha256,
                "signature": sig_info, "updated": updated,
            }))?
        );
    } else {
        println!(
            "{} [hur] {}/{}@{}  status={}  ({})",
            if updated { "✅ 已更新" } else { "✅ 已发布" },
            it["namespace"]["slug"].as_str().unwrap_or(""),
            it["slug"].as_str().unwrap_or(""),
            it["version"].as_str().unwrap_or(""),
            it["status"].as_str().unwrap_or(""),
            it["id"].as_str().unwrap_or("")
        );
        if updated {
            println!("  注意      更新只改「版本 / 产物地址 / 状态」——清单里的签名与权限面保持原样，改过签名请重发");
        }
        println!("   产物      {}（{} 字节）", out.file.display(), out.bytes);
        println!("   sha256    {}", out.sha256);
        match &sig_info {
            Some(s) => println!("   签名      ✔ keynum {}（{}）", s["keynum"].as_str().unwrap_or(""), s["signer"].as_str().unwrap_or("")),
            None => println!("   签名      未签名（需要的话：ncc hur key gen && ncc hur sign .）"),
        }
        println!("   检索      ncc search --kind hur");
    }
    Ok(())
}

/* ---------------- 联网：加签（给已发布条目补签名） ---------------- */

/// `ncc hur attach` —— 把本机签名补到一个**已发布**的条目上。
///
/// 为什么单独一条命令、而不是让 `sign` 顺手联网：`sign` 的语义是"私钥不出设备的本地动作"，
/// 让它联网就把这条边界糊掉了。联网只发生在这里，而且**只上传签名文件本身与公钥**（公开产物），
/// 包内容一个字节都不传 —— 与 `publish` 的离线铁律一致。
///
/// 服务端还会再核对一次「签名覆盖的摘要 == 条目产物的摘要」，所以这里先把本地的不一致挡掉。
fn attach(cfg: &CliConfig, a: HurAttachArgs) -> Result<()> {
    let dir = root_of(&a.path)?;
    let pkg = spec::read_pkg(&dir)?;
    let store = sign::store();
    // 与 `ncc hur sign` 同一套参数：私钥/口令可覆盖，核对用的公钥也可指定
    // （用非本机密钥签的时候，光有私钥不够 —— `check_dir` 得能找到配对的公钥，
    //  否则结论是「有签名但不可核对」，attach 会拒）。
    let key = if a.key.trim().is_empty() { None } else { Some(PathBuf::from(a.key.trim())) };
    let pub_override = if a.pub_key.trim().is_empty() { None } else { Some(PathBuf::from(a.pub_key.trim())) };
    let password = if a.password.trim().is_empty() {
        std::env::var(sign::ENV_PASSWORD).ok().filter(|p| !p.trim().is_empty())
    } else {
        Some(a.password.clone())
    };

    // ① 本机签名（没有就按需先签一次）
    let mut rep = store.check_dir(&dir, &pkg, false, pub_override.as_deref());
    if !(rep.verified && rep.info.is_some()) && a.sign {
        let out = pack::pack(&dir)?;
        let comment = sign::comment_for(&pkg, &out.sha256);
        let path = store
            .sign_file(&out.file, &comment, key.as_deref(), password.as_deref())
            .map_err(|e| anyhow!("签名失败：{e:#}\n（没有密钥就先 `ncc hur key gen`）"))?;
        println!("已签名 {}", path.display());
        rep = store.check_dir(&dir, &pkg, true, pub_override.as_deref());
    }
    let info = match (rep.verified, rep.info.as_ref()) {
        (true, Some(i)) => i.clone(),
        _ => bail!(
            "本机没有可用签名：{}\n  先 `ncc hur sign .`，或在这里签一次：`ncc hur attach --sign`",
            if rep.summary.is_empty() { "未签名".to_string() } else { rep.summary.clone() }
        ),
    };

    // ② 本地工程变过就得重签：把旧签名贴到新产物上不是"加签"，是造假。
    let (bytes, fname) = pack::archive_bytes(&dir, false)
        .with_context(|| "先 `ncc hur build` 定下 hur.lock，再签名")?;
    let digest = spec::sha256_hex(&bytes);
    if !digest.eq_ignore_ascii_case(&info.sha256) {
        bail!(
            "本地工程已经变过：产物 {fname} 现在是 {digest}，而签名覆盖的是 {}。\n  先 `ncc hur sign .` 重新签名再附着（旧签名属于旧的字节）",
            info.sha256
        );
    }

    // ③ 只上传签名文件（+ 公钥进清单），产物字节不传
    let token = config::require_token(cfg)?;
    let sig_path = if info.signature.is_file() {
        info.signature.clone()
    } else {
        sign::find_sig(&dir.join(spec::DIST).join(&fname))
            .ok_or_else(|| anyhow!("找不到签名文件（{} 附近），先 `ncc hur sign .`", info.signature.display()))?
    };
    let sig_bytes = std::fs::read(&sig_path).with_context(|| format!("读签名 {} 失败", sig_path.display()))?;
    let sig_name = sig_path.file_name().and_then(|s| s.to_str()).unwrap_or("pkg.hur.minisig").to_string();
    let up = api::request(
        cfg,
        "POST",
        "/api/registry/uploads",
        Some(&token),
        None,
        Some(&sig_bytes),
        &[("X-Filename", &sig_name)],
    )?;
    let mut sig = json!({
        "format": "minisign",
        "keynum": info.keynum,
        "signer": info.signer,
        "trusted": info.trusted,
        "sha256": info.sha256,
        "name": sig_name,
        "url": up["storageUrl"].clone(),
        "sigSha256": up["sha256"].clone(),
        "sigBytes": up["size"].clone(),
    });
    if let Ok(pk) = std::fs::read_to_string(store.public_path()) {
        if !pk.trim().is_empty() {
            sig["pubkey"] = json!(pk.trim());
        }
    }

    // ④ 定位条目：给了引用就按引用，否则先按包 id、再按 slug 在自己名下找
    let id = if a.reference.trim().is_empty() {
        let by_pkg = find_mine_by_pkg(cfg, &token, &pkg.id, &pkg.version)?;
        let by_slug = if by_pkg.is_none() { find_mine(cfg, &token, &pkg.id.to_lowercase())? } else { None };
        by_pkg.or(by_slug).ok_or_else(|| {
            anyhow!(
                "自己名下没有这个包的条目（包 id {}）：用 `ncc hur attach --ref @命名空间/slug` 指明，或先 `ncc hur publish`",
                pkg.id
            )
        })?
    } else {
        let it = fetch_item(cfg, Some(&token), a.reference.trim())?;
        it["id"].as_str().unwrap_or("").to_string()
    };
    if id.is_empty() {
        bail!("没能定位条目 id（引用写错？）");
    }

    let data = api::request(
        cfg,
        "PUT",
        &format!("/api/registry/{}/signature", api::urlenc(&id)),
        Some(&token),
        Some(&json!({ "signature": sig })),
        None,
        &[],
    )?;
    let it = &data["item"];
    let full = format!(
        "{}/{}@{}",
        it["namespace"]["slug"].as_str().unwrap_or(""),
        it["slug"].as_str().unwrap_or(""),
        it["version"].as_str().unwrap_or("")
    );
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "item": it, "signature": sig }))?);
    } else {
        println!("✅ 已加签 {full}（keynum {}）", info.keynum);
        println!("   签名文件   {sig_name}");
        println!("   产物 sha256 {}", info.sha256);
        println!("   别人核对   ncc hur trust {full}");
    }
    Ok(())
}
