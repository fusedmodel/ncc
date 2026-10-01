//! `ncc conn` —— 通信基础设施：**到 Cloud instance 的连接通道**。
//!
//! 一条通道 = 目标机器上的一个会话：有工作目录、有有效期、有账本。通道上可以做三类事：
//!
//!   exec  跑命令（可反复跑，比"提交任务"轻 —— 但要目标端 `NCCR_CONN_ALLOW=1`）
//!   push  推文件（相对工作目录；`--exec` 还能顺手给可执行位）
//!   pull  拉文件（执行产物、日志、构建结果）
//!   run   把三者拼起来：**一批文件 + 一段脚本**，一次做完（用户要的"一系列命令与文件推送"）
//!
//! 与 `ncc sandbox` 的分工：
//!   · `ncc sandbox` = 云电脑的**登记与一次性任务**（有 run 记录、看退出码）；
//!   · `ncc conn`    = 在目标机上**开一条会话**，反复干活、来回传文件。
//!   两者可以合起来用：`ncc conn open --on <已登记的云电脑>` 直接复用那台的地址与凭据。
//!
//! 三条边界（与 `prd/ncc-conn.md` 一致）：
//!  1. **这是最高权限**：通道能跑任意命令、写文件 —— 目标端必须显式开 `NCCR_CONN_ALLOW=1`。
//!  2. **文件锁在工作目录里**：只走相对路径，`..` 与绝对路径一律被目标端拒绝。
//!  3. **每个动作都要 reason**：那台机器的账本要能回答「谁在这条通道上干了什么、为什么」。
use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::path::PathBuf;

use hur_core::policy;

use crate::api;
use crate::config::{self, CliConfig};

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
    #[serde(default)]
    url: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    work_dir: String,
    #[serde(default)]
    created_unix: u64,
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
    let (url, key, label) = if let Some(n) = a.on.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let e = env_of(n)?;
        (e.url.clone(), e.auth.token.clone(), e.id.clone())
    } else {
        let url = a
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("给个目标：--on <已登记的云电脑> 或 --url http://10.0.0.5:8282 --key <key>"))?;
        let url = if url.starts_with("http") { url.to_string() } else { format!("http://{url}") };
        let key = a.key.as_deref().unwrap_or("").trim().to_string();
        (url, key, String::new())
    };
    let c = sandbox_cfg(&url, &key);
    let token = if key.trim().is_empty() { None } else { Some(key.as_str()) };

    // 一次建连前先问清楚"这台能不能收通道"（不然只拿到一个 403，还得自己去猜配置项）
    if let Ok(k) = api::get(&c, "/api/exec/kinds", token) {
        let _ = k;
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
        url: url.clone(),
        key: key.clone(),
        work_dir: work.clone(),
        created_unix: hur_core::policy::now_unix(),
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
        let live = api::get(&cfg_for(c), &format!("/api/conn/connections/{}", c.id), Some(&c.key))
            .ok()
            .and_then(|d| d["connection"]["state"].as_str().map(String::from))
            .unwrap_or_else(|| "（目标不可达）".into());
        println!("  {:<22} {:<10} {:<28} {}", c.name, live, c.url, c.work_dir);
    }
    println!("  状态取自目标端（关掉/过期的会显示出来）；`ncc conn status <名字>` 看细节");
    Ok(())
}

fn status(target: &str, json_out: bool) -> Result<()> {
    let c = resolve(target)?;
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

fn close(target: &str, purge: bool, forget: bool) -> Result<()> {
    let c = resolve(target)?;
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
    let mut body = json!({ "cmd": a.cmd, "reason": a.reason });
    if let Some(w) = a.cwd.as_deref().filter(|s| !s.trim().is_empty()) {
        body["cwd"] = json!(w.trim());
    }
    if a.timeout > 0 {
        body["timeoutSec"] = json!(a.timeout);
    }
    let d = api::post_json(&cfg_for(&c), &format!("/api/conn/connections/{}/exec", c.id), Some(&c.key), &body)?;
    let code = show_exec(&d, a.json);
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
    let qs = vec![
        format!("path={}", api::urlenc(&rel)),
        format!("reason={}", api::urlenc(&a.reason)),
        format!("sha256={}", crate::sandbox::sha256_hex(&bytes)),
        format!("mode={}", if a.exec { "700" } else { "600" }),
    ];
    let d = api::request(
        &cfg_for(&c),
        "POST",
        &format!("/api/conn/connections/{}/files?{}", c.id, qs.join("&")),
        Some(&c.key),
        None,
        Some(&bytes),
        &[("Content-Type", "application/octet-stream")],
    )?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
    } else {
        println!(
            "✅ 已推送 {} → {}（{} 字节 · {} · 指纹 {}）",
            a.local.display(),
            rel,
            d["bytes"].as_i64().unwrap_or(0),
            d["mode"].as_str().unwrap_or("600"),
            short(d["sha256"].as_str().unwrap_or(""), 12)
        );
    }
    Ok(())
}

fn pull(a: &PullArgs) -> Result<()> {
    let c = resolve(&a.conn)?;
    let url = format!("{}/api/conn/connections/{}/files?path={}", c.url.trim_end_matches('/'), c.id, api::urlenc(&a.remote));
    let c2 = cfg_for(&c);
    let bytes = api::get_bytes(&c2, &url, Some(&c.key))?;
    let to = a.to.clone().unwrap_or_else(|| PathBuf::from(a.remote.rsplit('/').next().unwrap_or("file")));
    std::fs::write(&to, &bytes)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"remote": a.remote, "local": to, "bytes": bytes.len(), "sha256": crate::sandbox::sha256_hex(&bytes)}))?
        );
    } else {
        println!("✅ 已拉取 {} → {}（{} 字节）", a.remote, to.display(), bytes.len());
    }
    Ok(())
}

/// 一批：推文件（可多个）→ 推脚本（给可执行位）→ 跑脚本。任一失败就把原因说清并停在那里。
fn run(a: &RunArgs) -> Result<()> {
    let c = resolve(&a.conn)?;
    let cc = cfg_for(&c);
    let ep = |p: String| format!("/api/conn/connections/{}/{}", c.id, p);

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
        let qs = vec![
            format!("path={}", api::urlenc(&remote)),
            format!("reason={}", api::urlenc(&format!("{}（批量推送）", a.reason))),
            format!("sha256={}", crate::sandbox::sha256_hex(&bytes)),
            "mode=600".to_string(),
        ];
        api::request(
            &cc,
            "POST",
            &format!("{}?{}", ep("files".into()), qs.join("&")),
            Some(&c.key),
            None,
            Some(&bytes),
            &[("Content-Type", "application/octet-stream")],
        )
        .map_err(|e| anyhow!("推文件 {remote} 失败：{e}"))?;
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
    let qs = vec![
        format!("path={}", api::urlenc(remote_script)),
        format!("reason={}", api::urlenc(&format!("{}（批量脚本）", a.reason))),
        format!("sha256={}", crate::sandbox::sha256_hex(body.as_bytes())),
        "mode=700".to_string(),
    ];
    api::request(
        &cc,
        "POST",
        &format!("{}?{}", ep("files".into()), qs.join("&")),
        Some(&c.key),
        None,
        Some(body.as_bytes()),
        &[("Content-Type", "application/octet-stream")],
    )
    .map_err(|e| anyhow!("推脚本失败：{e}"))?;

    // 3) 跑：显式 sh 一次（不依赖可执行位/内核 shebang，行为更可预期）
    let cmd = format!("sh {remote_script}");
    let mut payload = json!({ "cmd": cmd, "reason": a.reason });
    if a.timeout > 0 {
        payload["timeoutSec"] = json!(a.timeout);
    }
    let d = api::post_json(&cc, &ep("exec".into()), Some(&c.key), &payload)?;
    let code = show_exec(&d, a.json);
    if !a.json {
        println!("   （推过去的文件：{}）", if pushed.is_empty() { "无".into() } else { pushed.join(" · ") });
    }
    if code != 0 {
        std::process::exit(if code > 0 { code } else { 1 });
    }
    Ok(())
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
