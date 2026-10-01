//! `ncc conn` —— 通信基础设施：**到 Cloud instance 的连接通道**。
//!
//! 一条通道 = 目标机器上的一个会话：有工作目录、有有效期、有账本。通道上可以做三类事：
//!
//!   exec  跑命令（可反复跑，比"提交任务"轻）
//!   push  推文件（相对工作目录；`--exec` 还能顺手给可执行位）
//!   pull  拉文件（执行产物、日志、构建结果）
//!   run   把三者拼起来：**一批文件 + 一段脚本**，一次做完（用户要的"一系列命令与文件推送"）
//!
//! **两种运输层（同一条命令，按登记自动分流）**：
//!   · `--on <云电脑>` / `--url … --key …` → **HTTP 通道**：目标端跑着 `ncc-registry`，
//!     要显式开 `NCCR_CONN_ALLOW=1`；门禁与账本都在**服务端执行**，文件面由服务端的 `safeJoin` 把关。
//!   · `--ssh [user@]host[:port]` → **SSH 通道**：目标端只要有 `sshd`，**零服务端改动**；
//!     权限就是 SSH 的权限（不新增第二套门禁），账本跟通道一起落在目标机上（自报，不防篡改）。
//!   详见 `connssh.rs` 的文件头。
//!
//! 与 `ncc sandbox` 的分工：
//!   · `ncc sandbox` = 云电脑的**登记与一次性任务**（有 run 记录、看退出码）；
//!   · `ncc conn`    = 在目标机上**开一条会话**，反复干活、来回传文件。
//!   两者可以合起来用：`ncc conn open --on <已登记的云电脑>` 直接复用那台的地址与凭据。
//!
//! 三条边界（与 `prd/ncc-conn.md` 一致）：
//!  1. **这是最高权限**：通道能跑任意命令、写文件 —— HTTP 通道要目标端显式开 `NCCR_CONN_ALLOW=1`；
//!     SSH 通道的开头就是「你能 ssh 进去」这件事本身。
//!  2. **文件锁在工作目录里**：只走相对路径，`..` 与绝对路径一律被拒（HTTP 由服务端把关，
//!     SSH 是客户端自查 —— 那是防手滑，不是安全边界，见 `connssh.rs`）。
//!  3. **每个动作都要 reason**：账本要能回答「谁在这条通道上干了什么、为什么」。
use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::path::PathBuf;

use hur_core::policy;

use crate::api;
use crate::config::{self, CliConfig};
use crate::connssh;

/// `close --purge` 的文案要提到的那个标记文件（与 `connssh` 里的实现一致）。
const META_NOTE: &str = "`.ncc-channel.json`（认不出来就不肯删）";

#[derive(clap::Args)]
pub struct ConnCmd {
    #[command(subcommand)]
    pub action: ConnAction,
}

#[derive(Subcommand)]
pub enum ConnAction {
    /// 建一条通道（到某个云电脑/内网节点），或执行任务不建（不推荐：每次都重新握手）
    Open(OpenArgs),
    /// 我开着的通道
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// 看一条通道：状态、工作目录、这条通道上跑过什么
    Status {
        /// 通道 id（CN-…）或名字
        conn: String,
        #[arg(long)]
        json: bool,
    },
    /// 在通道上执行命令
    Exec(ExecArgs),
    /// 推一个文件到通道的工作目录（可给可执行位）
    Push(PushArgs),
    /// 从通道的工作目录拉一个文件
    Pull(PullArgs),
    /// 一批：推文件 + 跑脚本（用户场景：「一系列命令 + 文件推送 + 目标端执行」）
    Run(RunArgs),
    /// 关闭通道（--purge 连工作目录一起删；--forget 连本地登记一起删）
    Close {
        /// 通道 id 或名字
        conn: String,
        #[arg(long)]
        purge: bool,
        /// 连本地登记一起删掉（默认留着：关掉的通道还能 `status` 看账本）
        #[arg(long)]
        forget: bool,
    },
}

#[derive(clap::Args)]
pub struct OpenArgs {
    /// 用已登记的云电脑（`ncc sandbox ls` 里的名字）：地址与凭据都从那张表来
    #[arg(long)]
    pub on: Option<String>,
    /// 或者直接用 ssh 连：`[user@]host[:port]` —— **零服务端改动**，目标端只要有 sshd
    ///
    /// 走的是你本机那个 ssh（~/.ssh/config、key、agent、跳板机、known_hosts 照旧生效），
    /// 通道 = 目标机上的一个目录 + 一个账本；不经云端。
    #[arg(long)]
    pub ssh: Option<String>,
    /// ssh 用的私钥（`-i`；不给就用 ssh 自己的默认/agent）
    #[arg(long)]
    pub identity: Option<String>,
    /// 通道目录（ss h 用；缺省 `.ncc/conn/<名字>`，相对目标机的 home）
    #[arg(long)]
    pub dir: Option<String>,
    /// 强制 BatchMode（CI：拿不到密钥就直接失败，不挂在那儿等密码）
    #[arg(long)]
    pub batch: bool,
    /// 或者直接给地址与 key
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long)]
    pub key: Option<String>,
    /// 通道名（缺省：<主机>-<序号>）
    #[arg(long)]
    pub name: Option<String>,
    /// 备注
    #[arg(long)]
    pub note: Option<String>,
    /// 有效期（秒；缺省用目标端的默认值，上限 8 小时）
    #[arg(long, default_value_t = 0)]
    pub ttl: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ExecArgs {
    /// 通道 id 或名字
    ///
    /// ⚠️ 字段**不能叫 `target`**：顶层 `--target` 是 global 的，clap arg id 会撞车，
    /// 位置参数被当成目标名（报「没有名为 cli-chan 的目标」）—— 这是本仓库踩过的坑，
    /// `ncc info` / `ncc mem rm` / `ncc verify` 都因为同一个原因改过字段名。
    pub conn: String,
    /// 要跑的命令
    pub cmd: String,
    /// 相对工作目录的子目录（当 cwd 用）
    #[arg(long)]
    pub cwd: Option<String>,
    /// 墙上限（秒）
    #[arg(long, default_value_t = 0)]
    pub timeout: i64,
    /// 为什么在这条通道上跑这条命令（必填）
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct PushArgs {
    /// 通道 id 或名字
    pub conn: String,
    /// 本地文件
    pub local: PathBuf,
    /// 远端相对路径（缺省用本地文件名）
    #[arg(long)]
    pub to: Option<String>,
    /// 给可执行位（0700）
    #[arg(long)]
    pub exec: bool,
    /// 为什么推这个文件（必填）
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct PullArgs {
    /// 通道 id 或名字
    pub conn: String,
    /// 远端相对路径
    pub remote: String,
    /// 存到本地（缺省用远端文件名）
    #[arg(long)]
    pub to: Option<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// 通道 id 或名字
    pub conn: String,
    /// 脚本文件（`-` = 从 stdin 读）；和 --sh 二选一
    #[arg(long)]
    pub script: Option<String>,
    /// 直接给一段命令（与 --script 二选一）
    #[arg(long)]
    pub sh: Option<String>,
    /// 要推的文件，可重复：本地路径[=远端相对路径]（不给远端名就用文件名）
    #[arg(long = "file")]
    pub files: Vec<String>,
    /// 推过去的脚本自动给可执行位（并 chmod 后执行）
    #[arg(long, default_value_t = true)]
    pub chmod: bool,
    #[arg(long, default_value_t = 0)]
    pub timeout: i64,
    /// 为什么跑这一批（必填）
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 本地通道登记（~/.ncc/connections.json） ---------------- */

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct SavedConn {
    id: String,
    #[serde(default)]
    name: String,
    /// 运输层：`http`（默认 —— 老记录没这个字段就是这个）| `ssh`
    #[serde(default)]
    transport: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    key: String,
    /// HTTP：远端工作目录；SSH：远端目录（相对 home 或绝对）
    #[serde(default)]
    work_dir: String,
    /// SSH 通道的连接信息（transport = ssh 时才有）
    #[serde(default)]
    ssh: Option<connssh::SshSpec>,
    #[serde(default)]
    created_unix: u64,
    /// 有效期（epoch 秒；0 = 不限）——HTTP 由服务端把关，SSH 只是 CLI 自己守
    #[serde(default)]
    expires_unix: u64,
}

impl SavedConn {
    fn is_ssh(&self) -> bool {
        self.transport == "ssh" && self.ssh.is_some()
    }

    fn spec(&self) -> Result<connssh::SshSpec> {
        self.ssh.clone().ok_or_else(|| anyhow!("这条通道登记里没有 ssh 信息（`ncc conn open --ssh …` 重开一条）"))
    }
}

/// epoch 秒 → 人读 UTC。不引时间库：这段是纯算术（Howard Hinnant 的 civil_from_days）。
pub fn fmt_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}Z")
}

pub fn now_unix() -> u64 {
    hur_core::policy::now_unix()
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ConnFile {
    #[serde(default)]
    connections: Vec<SavedConn>,
}

fn conn_path() -> PathBuf {
    hur_core::cfg::ncc_dir().join("connections.json")
}

fn load_conns() -> ConnFile {
    std::fs::read_to_string(conn_path())
        .ok()
        .and_then(|t| serde_json::from_str::<ConnFile>(&t).ok())
        .unwrap_or_default()
}

fn save_conns(f: &ConnFile) -> Result<()> {
    let dir = hur_core::cfg::ncc_dir();
    std::fs::create_dir_all(&dir)?;
    let text = format!("{}\n", serde_json::to_string_pretty(f)?);
    std::fs::write(conn_path(), text)?;
    // 里面有通道凭据（key）→ 只给本用户读
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(conn_path(), std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 解析「通道 id 或名字」→ 本地登记项（含 url 与 key）。
fn resolve(target: &str) -> Result<SavedConn> {
    let f = load_conns();
    let t = target.trim();
    if let Some(c) = f.connections.iter().find(|c| c.id == t || c.name == t).cloned() {
        return Ok(c);
    }
    // 也允许直接给 id 但本地没登记（比如在另一台机器上开的）：这时至少要能拿到底地址 ——
    // 拿不到就说清楚，而不是发一个空地址的请求。
    bail!(
        "本地没有登记过通道「{t}」（`ncc conn ls` 看有哪些；在另一台机器上开的通道，\
         需要在那边用，或者 `ncc conn open` 在这边重新建一条）"
    )
}

fn upd_local_drop(id: &str) {
    let mut f = load_conns();
    f.connections.retain(|c| c.id != id);
    let _ = save_conns(&f);
}

fn cfg_for(c: &SavedConn) -> CliConfig {
    sandbox_cfg(&c.url, &c.key)
}

/// 复用 sandbox 模块的"指向某个地址的 CliConfig"（同一份请求层，不另造 HTTP）。
fn sandbox_cfg(url: &str, token: &str) -> CliConfig {
    let mut t = config::Target::default();
    t.base_url = url.trim_end_matches('/').to_string();
    t.token = if token.trim().is_empty() { None } else { Some(token.to_string()) };
    let mut c = CliConfig::default();
    c.current = Some("conn".into());
    c.targets.insert("conn".into(), t);
    c
}

fn short(s: &str, n: usize) -> String {
    let r: Vec<char> = s.chars().collect();
    if r.len() <= n {
        s.to_string()
    } else {
        format!("{}…", r[..n].iter().collect::<String>())
    }
}

/// 按名字/ID 找一个已登记的云电脑（`ncc sandbox init` 写的那张表）。
fn env_of(name: &str) -> Result<policy::Environment> {
    let reg = policy::load_envs();
    reg.environments
        .iter()
        .find(|e| e.id == name.trim())
        .cloned()
        .ok_or_else(|| anyhow!("没有登记过环境「{name}」（先 `ncc sandbox init --host … --key …`）"))
}

/* ---------------- open / ls / status / close ---------------- */

fn open(a: &OpenArgs) -> Result<()> {
    if let Some(t) = a.ssh.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return open_ssh(a, t);
    }
    open_http(a)
}

/// SSH 通道：目标端**只要有 sshd**就够了 —— 零服务端改动。
///
/// 开门的是 sshd 自己（你的 key / agent / 跳板机），我们不新增第二套权限模型：
/// 能免密 ssh 进去的人本来就能在那台机器上干任何事，假装 ncc 多一道门是自欺欺人。
fn open_ssh(a: &OpenArgs, target: &str) -> Result<()> {
    let mut spec = connssh::SshSpec::parse(target)?;
    spec.identity = a.identity.clone().unwrap_or_default();
    spec.batch = a.batch;
    let host_slug = spec.host.replace([':', '.', '/'], "-");
    let name = a
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| host_slug.clone());
    spec.dir = a
        .dir
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!(".ncc/conn/{name}"));
    // TTL 默认与 HTTP 通道一致（1h）；SSH 上它是**软约束**（CLI 自己守，sshd 不参与）。
    let ttl = if a.ttl > 0 { a.ttl } else { 3600 };
    let expires = now_unix() + ttl as u64;
    let note = a.note.clone().unwrap_or_default();
    connssh::open(&spec, &name, &note, expires)?;

    let id = format!("SSH-{name}");
    let rec = SavedConn {
        id: id.clone(),
        name: name.clone(),
        transport: "ssh".into(),
        url: format!("ssh://{}", spec.label()),
        key: String::new(),
        work_dir: spec.dir.clone(),
        ssh: Some(spec.clone()),
        created_unix: now_unix(),
        expires_unix: expires,
    };
    let mut f = load_conns();
    f.connections.retain(|x| x.id != id && x.name != name);
    f.connections.push(rec);
    save_conns(&f)?;

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "connection": {
                    "id": id, "name": name, "transport": "ssh", "host": spec.label(),
                    "workDir": spec.dir, "ttlSec": ttl, "expiresAt": fmt_epoch(expires as i64),
                    "state": "open", "usable": true,
                },
                "note": "SSH 通道：权限就是 SSH 的权限（ncc 不再加一道门）；账本在通道目录里的 .ncc-ledger.jsonl。",
            }))?
        );
        return Ok(());
    }
    println!("✅ 通道已建立 {id}（{name} · SSH {})", spec.label());
    println!("   目录     {}（目标机上）· 有效期到 {}（TTL {ttl}s，SSH 上是软约束）", spec.dir, fmt_epoch(expires as i64));
    println!("   怎么连   用你本机的 ssh（~/.ssh/config / agent / 跳板机照旧生效）—— 不经云端，目标端也不需要装 ncc");
    println!("\n通道上能做什么");
    println!("   ncc conn exec {name} \"ls -la\" --reason \"看一眼\"");
    println!("   ncc conn push {name} ./deploy.sh --exec --reason \"部署脚本\"");
    println!("   ncc conn run  {name} --file ./app.tar.gz --script ./deploy.sh --reason \"发一版\"");
    println!("   ncc conn pull {name} build/out.tar --to ./out.tar");
    println!("   ncc conn close {name} --purge");
    Ok(())
}

fn open_http(a: &OpenArgs) -> Result<()> {
    let (url, key, label) = if let Some(n) = a.on.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let e = env_of(n)?;
        (e.url.clone(), e.auth.token.clone(), e.id.clone())
    } else {
        let url = a
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "给个目标：--ssh [user@]host[:port]（只要对方有 sshd）\u{3001} \
                     --on <已登记的云电脑> 或 --url http://10.0.0.5:8282 --key <key>（对方要跑 ncc-registry）"
                )
            })?;
        let url = if url.starts_with("http") { url.to_string() } else { format!("http://{url}") };
        let key = a.key.as_deref().unwrap_or("").trim().to_string();
        (url, key, String::new())
    };
    let c = sandbox_cfg(&url, &key);
    let token = if key.trim().is_empty() { None } else { Some(key.as_str()) };

    // 建连前先问一句这台机器到底放行了什么引擎：通道上的 exec 走 process，
    // 没放行的话按下去就是 403 —— 不如现在就说清楚（但**不拦**：先建好再去运维那边开也行）。
    if let Ok(k) = api::get(&c, "/api/exec/kinds", token) {
        let engines = k["engines"].as_array().cloned().unwrap_or_default();
        let usable: Vec<String> = engines
            .iter()
            .filter(|e| e["enabled"].as_bool().unwrap_or(false))
            .map(|e| e["engine"].as_str().unwrap_or("?").to_string())
            .collect();
        if !usable.iter().any(|e| e == "process" || e == "container") {
            eprintln!(
                "  ! 这台机器现在没放行 process / container（已放行：{}）—— \
                 通道建得起来，但 `conn exec` 会被 403 拦下。运维那边要 NCCR_EXEC_ALLOW 放行。",
                if usable.is_empty() { "无".into() } else { usable.join(", ") }
            );
        }
    }
    let mut body = json!({ "note": a.note.clone().unwrap_or_default() });
    if let Some(n) = a.name.as_deref().filter(|s| !s.trim().is_empty()) {
        body["name"] = json!(n.trim());
    }
    if a.ttl > 0 {
        body["ttlSec"] = json!(a.ttl);
    }
    let d = api::post_json(&c, "/api/conn/connections", token, &body).map_err(|e| {
        anyhow!(
            "{e}\n  提示：通道默认是关的（它能跑任意命令、写文件）——目标端要 NCCR_CONN_ALLOW=1，\
             并确保 NCCR_EXEC_ALLOW 里放行了 process 或 container。"
        )
    })?;
    let conn = &d["connection"];
    let id = conn["id"].as_str().unwrap_or("").to_string();
    let name = conn["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(&id).to_string();
    let work = conn["workDir"].as_str().unwrap_or("").to_string();
    let mut f = load_conns();
    f.connections.retain(|x| x.id != id);
    f.connections.push(SavedConn {
        id: id.clone(),
        name: name.clone(),
        transport: "http".into(),
        url: url.clone(),
        key: key.clone(),
        work_dir: work.clone(),
        ssh: None,
        created_unix: now_unix(),
        // HTTP 通道的有效期由**服务端**算，这里只记下来供 `ls` 显示（不自己施法）
        expires_unix: 0,
    });
    save_conns(&f)?;

    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("✅ 通道已建立 {id}（{name}{}）", if label.is_empty() { String::new() } else { format!(" · 目标 {label}") });
    println!("   状态    {}（{}s，到 {}）", conn["state"].as_str().unwrap_or(""), conn["ttlSec"], short(conn["expiresAt"].as_str().unwrap_or(""), 19));
    println!("   工作目录 {}", work);
    println!("\n通道上能做什么");
    println!("   ncc conn exec {id} \"ls -la\" --reason \"看一眼\"");
    println!("   ncc conn push {id} ./deploy.sh --exec --reason \"部署脚本\"");
    println!("   ncc conn run  {id} --file ./app.tar.gz --script ./deploy.sh --reason \"发一版\"");
    println!("   ncc conn pull {id} build/out.tar --to ./out.tar");
    println!("   ncc conn close {id} --purge");
    Ok(())
}

fn ls(json_out: bool) -> Result<()> {
    let f = load_conns();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&f)?);
        return Ok(());
    }
    if f.connections.is_empty() {
        println!("还没有开着的通道。开一条：ncc conn open --on <已登记的云电脑>");
        println!("  还没登记过云电脑？ncc sandbox init --host <ip> --port <n> --key <key>");
        return Ok(());
    }
    println!("本地登记的通道 {}（{}）", f.connections.len(), conn_path().display());
    for c in &f.connections {
        let live = if c.is_ssh() {
            // SSH 通道没有服务端可问：状态是**本地记的**（过期也只是 CLI 自己守 —— 软的）
            if c.expires_unix > 0 && now_unix() >= c.expires_unix {
                "expired".to_string()
            } else {
                "ssh".to_string()
            }
        } else {
            api::get(&cfg_for(c), &format!("/api/conn/connections/{}", c.id), Some(&c.key))
                .ok()
                .and_then(|d| d["connection"]["state"].as_str().map(String::from))
                .unwrap_or_else(|| "（目标不可达）".into())
        };
        let where_ = if c.is_ssh() { c.ssh.as_ref().map(|s| s.label()).unwrap_or_default() } else { c.url.clone() };
        println!("  {:<22} {:<10} {:<28} {}", c.name, live, where_, c.work_dir);
    }
    println!("  http 通道的状态取自目标端；ssh 通道的状态是本地记的（过期是软约束）");
    Ok(())
}

/* ---------------- 看一条通道（两个运输层各一份展示） ---------------- */

fn status(target: &str, json_out: bool) -> Result<()> {
    let c = resolve(target)?;
    if c.is_ssh() {
        return status_ssh(&c, json_out);
    }
    let d = api::get(&cfg_for(&c), &format!("/api/conn/connections/{}", c.id), Some(&c.key))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let conn = &d["connection"];
    println!("{}（{}）{}", c.name, c.id, conn["state"].as_str().unwrap_or(""));
    println!("   地址     {}", c.url);
    println!("   工作目录 {}", conn["workDir"].as_str().unwrap_or(""));
    println!(
        "   统计     exec {} 次 · 上行 {} · 下行 {} · pull {} 次",
        conn["execCount"].as_i64().unwrap_or(0),
        conn["bytesUp"].as_i64().unwrap_or(0),
        conn["bytesDown"].as_i64().unwrap_or(0),
        conn["pulls"].as_i64().unwrap_or(0)
    );
    let exp = conn["execs"].as_array();
    let usable = conn["usable"].as_bool().unwrap_or(true);
    if !usable {
        println!(
            "   提示     这条通道已经{} —— 账本还能看，但 exec / push / pull 会被拒（410）；要干活就重新 open 一条新的",
            if let Some(b) = conn["blocked"].as_str() {
                match b {
                    "closed" => "被关闭",
                    "expired" => "过期",
                    _ => b,
                }
            } else {
                "不可用"
            }
        );
    }
    if let Some(list) = exp {
        if !list.is_empty() {
            println!("   这条通道上跑过：");
            for e in list.iter().take(10) {
                println!(
                    "     [{}] {} （exit {} · {}ms）",
                    e["status"].as_str().unwrap_or(""),
                    short(e["spec"].as_str().unwrap_or(""), 60),
                    e["exitCode"].as_i64().unwrap_or(-1),
                    e["durationMs"].as_i64().unwrap_or(0)
                );
            }
        }
    }
    Ok(())
}

/// SSH 通道的细节：元数据 + 账本（都在目标机的通道目录里，不经云端）。
fn status_ssh(c: &SavedConn, json_out: bool) -> Result<()> {
    let spec = c.spec()?;
    let meta = connssh::meta(&spec).unwrap_or(Value::Null);
    let ledger = connssh::ledger_tail(&spec, 10).unwrap_or_default();
    let expired = c.expires_unix > 0 && now_unix() >= c.expires_unix;
    let state = if expired { "expired" } else { "open" };

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "connection": {
                    "id": c.id, "name": c.name, "transport": "ssh", "host": spec.label(),
                    "workDir": c.work_dir, "state": state, "usable": !expired,
                    "createdAtUnix": c.created_unix, "expiresAtUnix": c.expires_unix,
                    "expiresAt": if c.expires_unix > 0 { fmt_epoch(c.expires_unix as i64) } else { "不限".into() },
                    "blocked": if expired { "expired" } else { "" },
                },
                "remoteMeta": meta,
                "ledger": ledger,
                "note": "SSH 通道：账本由 CLI 自己写在目标机上（自报，不防篡改）；TTL 是软约束。",
            }))?
        );
        return Ok(());
    }

    println!("{}（{}）{state}  ssh", c.name, c.id);
    println!("   目标     {}", spec.label());
    println!("   目录     {}（目标机上）", c.work_dir);
    println!(
        "   有效期   {}（TTL 是**软约束**：sshd 不知道我们在计时，过期后 CLI 自己拒绝）",
        if c.expires_unix > 0 {
            format!("到 {}", fmt_epoch(c.expires_unix as i64))
        } else {
            "不限".to_string()
        }
    );
    println!("   建立于   {}", fmt_epoch(c.created_unix as i64));
    if meta.is_null() {
        println!("   元数据   读不到（目录不在了？还是这条通道是用另一个 --dir 开的？）");
    }
    if ledger.is_empty() {
        println!("   账本     还没有记录（没在通道上干过活）");
    } else {
        println!("   账本     最近 {} 条（目标机上的 .ncc-ledger.jsonl）：", ledger.len());
        for e in &ledger {
            let act = e["act"].as_str().unwrap_or("?");
            let what = e["cmd"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| e["path"].as_str())
                .unwrap_or("");
            let at = e["at"].as_i64().map(fmt_epoch).unwrap_or_default();
            println!(
                "     [{}] {} · {} · {}",
                act,
                e["exit"].as_i64().map(|c| format!("exit {c}")).unwrap_or_else(|| "-".into()),
                short(at.as_str(), 19),
                short(what, 60)
            );
            if let Some(r) = e["reason"].as_str().filter(|s| !s.is_empty()) {
                println!("         理由 {r}");
            }
        }
    }
    if expired {
        println!("   提示     已过期 —— 要接着用就 `ncc conn open --ssh {}` 重开一条（`--ttl <秒>` 给更长）", spec.label());
    }
    Ok(())
}

fn close(target: &str, purge: bool, forget: bool) -> Result<()> {
    let c = resolve(target)?;
    if c.is_ssh() {
        let spec = c.spec()?;
        let purged = connssh::close(&spec, purge)?;
        if forget {
            upd_local_drop(&c.id);
        }
        println!("✅ 已收线 SSH 通道 {}（{}）", c.name, c.id);
        println!(
            "   远端     {}",
            if purged {
                format!("工作目录 {} 已删（删之前先确认过里面有 {META_NOTE}）", spec.dir)
            } else {
                "什么都没动 —— SSH 通道没有服务端要通知；要连目录一起删：--purge".to_string()
            }
        );
        println!(
            "   本地登记 {}",
            if forget { "也删了".to_string() } else { format!("留着（`ncc conn status {}` 还能看账本；要清掉：--forget）", c.name) }
        );
        return Ok(());
    }
    let path = format!("/api/conn/connections/{}{}", c.id, if purge { "?purge=1" } else { "" });
    api::del(&cfg_for(&c), &path, Some(&c.key))?;
    // 默认**不删本地登记**：关掉只是不能动手了，账本（这条通道上跑过什么）还要能看，
    // 而看账本需要地址与凭据 —— 删了就再也找不回来了。要清掉给 --forget。
    if forget {
        upd_local_drop(&c.id);
    }
    println!(
        "✅ 已关闭通道 {}（{}）{}",
        c.name,
        c.id,
        if purge { "，工作目录也删了" } else { "（工作目录留着；要删：--purge）" }
    );
    if forget {
        println!("   本地登记也删了");
    } else {
        println!("   本地登记留着（`ncc conn ls` 里显示 closed，`ncc conn status {}` 还能看账本；要清掉：--forget）", c.name);
    }
    Ok(())
}

/* ---------------- exec / push / pull / run ---------------- */

/// 打印一次通道执行的日志与结论，返回远端退出码。
fn show_exec(v: &Value, json_out: bool) -> i32 {
    let e = &v["exec"];
    let code = e["exitCode"].as_i64().unwrap_or(-1) as i32;
    if json_out {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        return code;
    }
    if let Some(tail) = e["logTail"].as_str() {
        for line in tail.lines().filter(|l| !l.trim().is_empty()) {
            println!("  │ {line}");
        }
    }
    println!(
        "{} {} · exit {} · {}ms · 日志 {} 字节{}",
        match e["status"].as_str().unwrap_or("") {
            "succeeded" => "✅",
            "failed" => "✗",
            "timeout" => "✗ 超时",
            "canceled" => "✗ 已取消",
            _ => "…",
        },
        e["id"].as_str().unwrap_or(""),
        code,
        e["durationMs"].as_i64().unwrap_or(0),
        e["logBytes"].as_i64().unwrap_or(0),
        if e["logTruncated"].as_bool().unwrap_or(false) { "（已截断）" } else { "" }
    );
    code
}

fn exec(a: &ExecArgs) -> Result<()> {
    let c = resolve(&a.conn)?;
    let code = exec_cmd(&c, &a.cmd, a.cwd.as_deref(), a.timeout, &a.reason, a.json)?;
    if code != 0 {
        std::process::exit(if code > 0 { code } else { 1 });
    }
    Ok(())
}

fn push(a: &PushArgs) -> Result<()> {
    let c = resolve(&a.conn)?;
    let bytes = std::fs::read(&a.local).map_err(|e| anyhow!("读 {} 失败：{e}", a.local.display()))?;
    let rel = a
        .to
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| a.local.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".into()));
    let mode = if a.exec { 700 } else { 600 };
    let sha = put_file(&c, &rel, &bytes, mode, &a.reason)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "path": rel, "bytes": bytes.len(), "mode": format!("{mode:o}"), "sha256": sha,
                "transport": transport_of(&c), "host": host_of(&c),
            }))?
        );
    } else {
        println!(
            "✅ 已推送 {} → {}（{} 字节 · {:o} · 指纹 {}）",
            a.local.display(),
            rel,
            bytes.len(),
            mode,
            short(&sha, 12)
        );
    }
    Ok(())
}

fn pull(a: &PullArgs) -> Result<()> {
    let c = resolve(&a.conn)?;
    let bytes = get_file(&c, &a.remote, "pull")?;
    let to = a.to.clone().unwrap_or_else(|| PathBuf::from(a.remote.rsplit('/').next().unwrap_or("file")));
    std::fs::write(&to, &bytes)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "remote": a.remote, "local": to, "bytes": bytes.len(),
                "sha256": crate::sandbox::sha256_hex(&bytes), "transport": transport_of(&c),
            }))?
        );
    } else {
        println!("✅ 已拉取 {} → {}（{} 字节）", a.remote, to.display(), bytes.len());
    }
    Ok(())
}

/// 一批：推文件（可多个）→ 推脚本（给可执行位）→ 跑脚本。任一失败就把原因说清并停在那里。
///
/// 两个运输层（HTTP / SSH）共用这一份编排 —— 差别只在 `put_file` / `exec_cmd` 里面。
fn run(a: &RunArgs) -> Result<()> {
    let c = resolve(&a.conn)?;

    // 1) 推文件（--file 本地[=远端]）
    let mut pushed: Vec<String> = Vec::new();
    for spec in &a.files {
        let (local, remote) = match spec.split_once('=') {
            Some((l, r)) => (l.to_string(), r.to_string()),
            None => {
                let l = spec.clone();
                let r = PathBuf::from(&l).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
                (l, r)
            }
        };
        let bytes = std::fs::read(&local).map_err(|e| anyhow!("读 {local} 失败：{e}"))?;
        let reason = format!("{}（批量推送）", a.reason);
        put_file(&c, &remote, &bytes, 600, &reason).map_err(|e| anyhow!("推文件 {remote} 失败：{e}"))?;
        if !a.json {
            println!("  ↑ {remote}（{} 字节）", bytes.len());
        }
        pushed.push(remote);
    }

    // 2) 组装脚本：把"推过去的文件"和"要跑的命令"合成一个脚本，落到目标机再执行 ——
    //    这样 chmod/解释器/相对路径都在目标机上按它自己的规矩来，而不是我们替它猜。
    let body = match (a.script.as_deref(), a.sh.as_deref()) {
        (Some("-"), None) => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
        (Some(p), None) => std::fs::read_to_string(p).map_err(|e| anyhow!("读脚本 {p} 失败：{e}"))?,
        (None, Some(s)) => {
            if s.trim().is_empty() {
                bail!("--sh 是空的");
            }
            s.to_string()
        }
        (Some(_), Some(_)) => bail!("--script 与 --sh 只能给一个"),
        (None, None) => bail!("给个脚本：--script <文件|-> 或 --sh \"<命令>\""),
    };
    let remote_script = ".ncc-run.sh";
    let script_reason = format!("{}（批量脚本）", a.reason);
    // 脚本默认给可执行位（700），但**在目标端 chmod**（HTTP 由服务端落盘时给、SSH 由远端 chmod）；
    // 跑的时候仍然显式 `sh` 一次，不靠可执行位与内核 shebang。
    put_file(&c, remote_script, body.as_bytes(), if a.chmod { 700 } else { 600 }, &script_reason)
        .map_err(|e| anyhow!("推脚本失败：{e}"))?;

    // 3) 跑：显式 sh 一次（不依赖可执行位/内核 shebang，行为更可预期）
    let code = exec_cmd(&c, &format!("sh {remote_script}"), None, a.timeout, &a.reason, a.json)?;
    if !a.json {
        println!("   （推过去的文件：{}）", if pushed.is_empty() { "无".into() } else { pushed.join(" · ") });
    }
    if code != 0 {
        std::process::exit(if code > 0 { code } else { 1 });
    }
    Ok(())
}

/* ---------------- 两个运输层共用的原语 ---------------- */

fn transport_of(c: &SavedConn) -> &'static str {
    if c.is_ssh() {
        "ssh"
    } else {
        "http"
    }
}

fn host_of(c: &SavedConn) -> String {
    if c.is_ssh() {
        c.ssh.as_ref().map(|s| s.label()).unwrap_or_default()
    } else {
        c.url.clone()
    }
}

/// 推一个文件（两条路都是：字节上去 → 拿回指纹）。
fn put_file(c: &SavedConn, rel: &str, bytes: &[u8], mode: u32, reason: &str) -> Result<String> {
    if c.is_ssh() {
        return connssh::put(&c.spec()?, c.expires_unix, rel, bytes, mode, reason);
    }
    let qs = vec![
        format!("path={}", api::urlenc(rel)),
        format!("reason={}", api::urlenc(reason)),
        format!("sha256={}", crate::sandbox::sha256_hex(bytes)),
        format!("mode={mode}"),
    ];
    let d = api::request(
        &cfg_for(c),
        "POST",
        &format!("/api/conn/connections/{}/files?{}", c.id, qs.join("&")),
        Some(&c.key),
        None,
        Some(bytes),
        &[("Content-Type", "application/octet-stream")],
    )?;
    Ok(d["sha256"].as_str().unwrap_or("").to_string())
}

/// 拉一个文件。
fn get_file(c: &SavedConn, rel: &str, reason: &str) -> Result<Vec<u8>> {
    if c.is_ssh() {
        return connssh::get(&c.spec()?, c.expires_unix, rel, reason);
    }
    let url = format!(
        "{}/api/conn/connections/{}/files?path={}",
        c.url.trim_end_matches('/'),
        c.id,
        api::urlenc(rel)
    );
    api::get_bytes(&cfg_for(c), &url, Some(&c.key))
}

/// 在通道上跑一条命令，**返回远端退出码**（两条路都让它如实透传）。
fn exec_cmd(c: &SavedConn, cmd: &str, cwd: Option<&str>, timeout: i64, reason: &str, json_out: bool) -> Result<i32> {
    if c.is_ssh() {
        let out = connssh::exec(&c.spec()?, c.expires_unix, cmd, cwd, timeout.max(0) as u64, reason)?;
        if json_out {
            println!("{}", serde_json::to_string_pretty(&crate::connssh::exec_json(&c.spec()?, cmd, &out))?);
        } else {
            print_ssh_out(&out);
        }
        return Ok(out.code);
    }
    let mut body = json!({ "cmd": cmd, "reason": reason });
    if let Some(w) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        body["cwd"] = json!(w);
    }
    if timeout > 0 {
        body["timeoutSec"] = json!(timeout);
    }
    let d = api::post_json(&cfg_for(c), &format!("/api/conn/connections/{}/exec", c.id), Some(&c.key), &body)?;
    Ok(show_exec(&d, json_out))
}

/// SSH 执行结果的展示：与 HTTP 通道同一种排版（`│` 输出 / `!` 错误）。
fn print_ssh_out(out: &connssh::SshOut) {
    for line in String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()) {
        println!("  │ {line}");
    }
    for line in out.stderr.lines().filter(|l| !l.trim().is_empty()) {
        println!("  ! {line}");
    }
    let mark = if out.timed_out {
        "✗ 超时（已把它停掉）".to_string()
    } else if out.code == 0 {
        "✅".to_string()
    } else if out.ssh_error {
        "✗ ssh 本身没连上".to_string()
    } else if out.code < 0 {
        "✗ 被信号终止".to_string()
    } else {
        "✗".to_string()
    };
    println!("{mark} exit {} · {}ms · SSH {}", out.code, out.ms, transport_note(out));
}

fn transport_note(out: &connssh::SshOut) -> &'static str {
    if out.ssh_error {
        "（远端没跑起来）"
    } else {
        "（远端退出码如实透传）"
    }
}

pub fn cmd(action: &ConnAction) -> Result<()> {
    match action {
        ConnAction::Open(a) => open(a),
        ConnAction::Ls { json } => ls(*json),
        ConnAction::Status { conn, json } => status(conn, *json),
        ConnAction::Exec(a) => exec(a),
        ConnAction::Push(a) => push(a),
        ConnAction::Pull(a) => pull(a),
        ConnAction::Run(a) => run(a),
        ConnAction::Close { conn, purge, forget } => close(conn, *purge, *forget),
    }
}
