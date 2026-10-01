//! `ncc feedback` —— 跨 Agent、跨用户的反馈。
//!
//! 为什么需要一条**链**（CLI → node → hub → service）：
//!
//!	说一句的地方（CLI，Agent 就在这儿跑）与东西放着的地方（内网节点 / 云端目录）
//!	**不是同一台机器**；被说的人（服务的提供方）又往往是第三台。
//!	所以反馈要能"就近落下、按出处往上走、最后到该看到的人手里"，
//!	而不是逼着每个 Agent 自己去拼三套 API。
//!
//! 四条规矩（与节点 / 控制面两侧**同一份**，客户端只负责把它们执行到位）：
//!
//!	① **先落盘再发送**：本地队列（`~/.ncc/feedback/spool.jsonl`）先写一条，
//!	   再尝试送出去。送失败 → **退出码非 0** + 队列里留着（`ncc feedback relay`）。
//!	   绝不"看起来发出去了"（与网关的"先落盘再上报"同一条教训）。
//!	② **默认私有**：不说 `--public` 就只有作者与目标拥有者看得到。
//!	③ **不发散**：一条反馈只落在**你当前说话的那台**目标上；要不要搬到 hub 由
//!	   `relay` 显式决定（且只搬公开的那些）。**不自动改用户的目标**。
//!	④ **服务在 hub 上**：`service:` 的目标要求那台目标声明了 `services`；
//!	   没声明就报出该敲的命令（含 hub 目标名），**不替你切目标**。
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api;
use crate::capability;
use crate::config::{self, CliConfig};

/* ---------------- 词表（离线可读；与两端服务端同一批） ---------------- */

const ABOUT_KINDS: [&str; 7] = ["artifact", "node", "service", "profile", "run", "agent", "topic"];
const KINDS: [&str; 5] = ["report", "praise", "request", "correction", "rating"];
const STATUSES: [&str; 4] = ["open", "ack", "resolved", "wontfix"];

/// 一条反馈最多多少跳（与服务端同一批上限：两端不一致 = 同一个客户端两种行为）。
const MAX_HOPS: usize = 8;
const MAX_BODY: usize = 4000;
const MAX_TAGS: usize = 8;

#[derive(Subcommand)]
pub enum FeedbackCmd {
    /// 说一句（先落本地队列，再送出去；送不出去会告诉你还剩几条待传）
    Send(SendArgs),
    /// 看一批反馈（默认看当前目标上、你能看到的那部分）
    Ls(LsArgs),
    /// 看一条（含回复）
    Get(GetArgs),
    /// 回一条（回复也是一条反馈，不改原记录）
    Reply(ReplyArgs),
    /// 改**处置状态**（只有目标拥有者能成；内容永远改不了）
    Status(StatusArgs),
    /// 聚合：多少条 / 什么性质 / 哪些 Agent 在说（**不是排名分**）
    Summary(SummaryArgs),
    /// 收件箱：别人对我（我的制品/服务/名片）说的
    Inbox(InboxArgs),
    /// 本地队列里还有哪些没送出去
    Spool(SpoolArgs),
    /// 把一台目标上的**公开**反馈搬到另一台（内网节点 → hub；私有的一律不搬）
    Relay(RelayArgs),
    /// 词表与上限（离线可读）
    Kinds,
}

impl FeedbackCmd {
    /// 命令 → 它需要的能力。词表与本地队列是本地的事，不需要目标声明什么。
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            FeedbackCmd::Kinds | FeedbackCmd::Spool(_) => None,
            _ => Some("feedback"),
        }
    }
}

/* ---------------- `--about` 的写法 ---------------- */

/// 解析 `--about`：
///
///	`service:@alice/stay`   带前缀（推荐；语义最清楚）
///	`artifact:@alice/tool`  ……
///	`@alice/tool`           不带前缀 = 制品（目录里最常见的那种）
///	`@alice`                不带斜杠 = 名片
///	`一次网络抖动`           什么都不是 = 词条（topic）
///
/// 刻意**不做**智能猜测之外的魔法：猜出来的结果会打印出来（`ncc feedback ls --about x` 一行就看得见）。
pub fn parse_about(raw: &str) -> (String, String) {
    let s = raw.trim();
    for k in ABOUT_KINDS {
        let p = format!("{k}:");
        if let Some(rest) = s.strip_prefix(&p) {
            return (k.to_string(), rest.trim().to_string());
        }
    }
    if let Some(rest) = s.strip_prefix('@') {
        if rest.contains('/') {
            return ("artifact".into(), s.to_string());
        }
        return ("profile".into(), s.to_string());
    }
    ("topic".into(), s.to_string())
}

/* ---------------- 本地队列（先落盘再发送） ---------------- */

/// 队列里的一条。
///
/// `payload` 是**原样**的请求体：重传时不再重新拼装 —— 免得本地改了参数、线上却是旧内容
/// （"发出去的到底是哪一份"永远以落盘的那一份为准）。
#[derive(Serialize, Deserialize, Clone)]
pub struct Spooled {
    pub id: String,
    pub at: u64,
    /// 目标名（搬/重传时用它找回地址与凭据）。
    pub to: String,
    pub about: String,
    pub payload: Value,
    #[serde(default)]
    pub tries: u32,
    #[serde(default)]
    pub last_error: String,
}

/// 队列文件位置（`NCC_FEEDBACK_SPOOL` 可覆盖）。
pub fn spool_path() -> PathBuf {
    if let Ok(p) = std::env::var("NCC_FEEDBACK_SPOOL") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    config::ncc_dir().join("feedback").join("spool.jsonl")
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read_spool() -> Vec<Spooled> {
    let p = spool_path();
    let raw = match fs::read_to_string(&p) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Spooled>(l).ok())
        .collect()
}

fn write_spool(rows: &[Spooled]) -> Result<()> {
    let p = spool_path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    let mut out = String::new();
    for r in rows {
        out.push_str(&serde_json::to_string(r)?);
        out.push('\n');
    }
    fs::write(&p, out)?;
    // 队列里可能有人写的原话：收紧到 0600。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn append_spool(row: &Spooled) -> Result<()> {
    let p = spool_path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{}", serde_json::to_string(row)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/* ---------------- 参数 ---------------- */

#[derive(Args, Clone)]
pub struct SendArgs {
    /// 说的是哪个东西：`service:@alice/stay` / `artifact:@alice/tool` / `@alice`（名片）/ 一句话（词条）
    #[arg(long, default_value = "")]
    pub about: String,
    /// 性质：report（问题）/ praise（表扬）/ request（需求）/ correction（纠正）/ rating（打分）
    #[arg(long, default_value = "report")]
    pub kind: String,
    /// 正文（人话）
    #[arg(long, default_value = "")]
    pub body: String,
    /// 打分（1~5；kind=rating 时必填）
    #[arg(long, default_value_t = 0)]
    pub score: i64,
    /// 标签，可重复
    #[arg(long = "tag", value_name = "标签")]
    pub tags: Vec<String>,
    /// 哪个 Agent 说的（人不在场时必写；也可用环境变量 NCC_AGENT）
    #[arg(long = "as-agent", default_value = "")]
    pub agent: String,
    /// 公开（**默认私有**：只有作者与目标拥有者看得到）
    #[arg(long)]
    pub public: bool,
    /// 这条反馈是从哪次运行来的（轨迹 id）—— 只记引用，不搬内容
    #[arg(long, default_value = "")]
    pub trace: String,
    /// 关联的状态引用，可重复（如 `mem:key` / `ckpt:ID` / `kb:@ns/doc`）
    #[arg(long = "state", value_name = "引用")]
    pub states: Vec<String>,
    /// 回哪一条（回复也是一条反馈）
    #[arg(long, default_value = "")]
    pub reply_to: String,
    /// 送到哪台目标（默认：当前目标）
    #[arg(long, default_value = "")]
    pub to: String,
    /// 只落本地队列，不发送（离线攒着，等 `relay` 一起送）
    #[arg(long)]
    pub queue_only: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct LsArgs {
    /// 只看关于某个东西的（写法同 send --about）
    #[arg(long, default_value = "")]
    pub about: String,
    /// 只看我发的
    #[arg(long)]
    pub mine: bool,
    /// 只看发给我的（等于 inbox）
    #[arg(long)]
    pub inbox: bool,
    /// 只看某个性质
    #[arg(long, default_value = "")]
    pub kind: String,
    /// 只看某个处置状态
    #[arg(long, default_value = "")]
    pub status: String,
    /// 只看公开的
    #[arg(long)]
    pub public: bool,
    /// 只看还没处置的
    #[arg(long)]
    pub open: bool,
    #[arg(long, default_value_t = 1)]
    pub page: i64,
    #[arg(long, default_value_t = 20)]
    pub size: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct GetArgs {
    pub id: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct ReplyArgs {
    pub id: String,
    /// 回复正文
    #[arg(long, default_value = "")]
    pub body: String,
    /// 回复里带个分也行（不打算给分就别给）
    #[arg(long, default_value_t = 0)]
    pub score: i64,
    /// 哪个 Agent 回的
    #[arg(long = "as-agent", default_value = "")]
    pub agent: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StatusArgs {
    pub id: String,
    /// open / ack / resolved / wontfix（**只有目标拥有者能改**）
    #[arg(long, value_name = "状态")]
    pub set: String,
    /// 顺带说一句为什么（作为回复发出去 —— 状态是状态，话是话）
    #[arg(long, default_value = "")]
    pub note: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct SummaryArgs {
    #[arg(long, default_value = "")]
    pub about: String,
    /// 看发给我的那批
    #[arg(long)]
    pub inbox: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct InboxArgs {
    #[arg(long, default_value_t = 20)]
    pub size: i64,
    /// 只看还没处置的
    #[arg(long)]
    pub open: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct SpoolArgs {
    /// 把队列清空（**不可逆**：没送出去的话就真没了）
    #[arg(long)]
    pub clear: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct RelayArgs {
    /// 搬关于某个东西的（写法同 send --about）
    #[arg(long, default_value = "")]
    pub about: String,
    /// 搬我在这台目标上发过的全部公开反馈（与 --about 二选一）
    #[arg(long)]
    pub all_mine: bool,
    /// 从哪台搬（默认：当前目标）
    #[arg(long, default_value = "")]
    pub from: String,
    /// 搬到哪台（默认：hub 目标；没有就报出怎么建）
    #[arg(long, default_value = "")]
    pub to: String,
    /// 一次最多搬几条
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
    /// 先把本地队列里的送出去，再把公开的搬上去
    #[arg(long)]
    pub flush: bool,
    /// 只看会搬什么，不动
    #[arg(long)]
    pub dry: bool,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 小工具 ---------------- */

/// 指向某个目标的配置（**不改**用户当前目标）。
///
/// 与 `index.rs` 里那份同一个理由：往内网节点说话不能顺手把用户当前目标切走 ——
/// 切目标是用户的动作，不是命令的副作用。
fn config_for(cfg: &CliConfig, name: &str) -> Result<CliConfig> {
    let t = cfg.by_name(name).cloned().ok_or_else(|| {
        anyhow::anyhow!("没有名为 {name} 的目标（`ncc target list` 看全部）")
    })?;
    let mut c = cfg.clone();
    c.current = Some(name.to_string());
    c.targets.insert(name.to_string(), t);
    Ok(c)
}

/// 这次命令对着哪台目标说话（+ 它的名字）。
fn target_cfg(cfg: &CliConfig, to: &str) -> Result<(CliConfig, String)> {
    if to.trim().is_empty() {
        return Ok((cfg.clone(), cfg.current_name()));
    }
    let name = to.trim();
    let c = config_for(cfg, name)?;
    Ok((c, name.to_string()))
}

/// hub 目标名（做 relay / service 反馈时用）。
fn hub_name(cfg: &CliConfig) -> Result<String> {
    if let Some(n) = crate::target::hub_target_name(cfg) {
        return Ok(n);
    }
    bail!(
        "找不到云端（hub）目标。先接一个：`ncc target add hub --base https://ncc.ai`；\
         或本次直接给：`--to <目标名>`（`ncc target list` 看全部）"
    )
}

/// 这台目标声明了哪些能力（探测不到 = 未知 = 放行）。
fn caps_of(cfg: &CliConfig) -> Option<Vec<String>> {
    Some(capability::probe(cfg).capabilities)
}

/// 服务反馈只能落在声明了 `services` 的目标上（服务目录在那边）。
fn ensure_services(cfg: &CliConfig, name: &str, about: &str) -> Result<()> {
    if let Some(caps) = caps_of(cfg) {
        if !caps.is_empty() && !caps.iter().any(|c| c == "services") {
            let hub = crate::target::hub_target_name(cfg)
                .map(|h| format!("`ncc --target {h} feedback send --about {about} …`"))
                .unwrap_or_else(|| "`--to <云端目标名>`".to_string());
            bail!(
                "目标 {name} 没声明 services 能力 —— 服务目录在云端。\n  \
                 这样说：{hub}\n  \
                 （**不会替你切目标**：目标是你选的，不是命令的副作用）"
            );
        }
    }
    Ok(())
}

fn payload_of(a: &SendArgs, kind: &str, about: &str, visibility: &str, parent: &str) -> Value {
    let agent = if a.agent.trim().is_empty() {
        std::env::var("NCC_AGENT").unwrap_or_default()
    } else {
        a.agent.clone()
    };
    json!({
        "aboutKind": kind,
        "aboutRef": about,
        "kind": a.kind,
        "score": a.score,
        "body": a.body,
        "tags": a.tags.iter().take(MAX_TAGS).cloned().collect::<Vec<_>>(),
        "agent": agent,
        "visibility": visibility,
        "traceRef": a.trace,
        "stateRefs": a.states,
        "parentId": parent,
    })
}

/// 校验（本地先拦一道：错误说在离人近的地方，也少一次往返）。
fn validate(kind: &str, about: &str, body: &str, score: i64, req_kind: &str) -> Result<()> {
    if !ABOUT_KINDS.contains(&kind) {
        bail!("aboutKind 只能是 {}", ABOUT_KINDS.join("|"));
    }
    if about.trim().is_empty() {
        bail!("缺 --about（说的是哪个东西：`service:@alice/stay` / `@alice/tool` / `@alice` / 一句话）");
    }
    if !KINDS.contains(&req_kind) {
        bail!("--kind 只能是 {}", KINDS.join("|"));
    }
    if !(0..=5).contains(&score) {
        bail!("--score 必须在 0~5 之间（0 = 不打分）");
    }
    if req_kind == "rating" && score == 0 {
        bail!("--kind rating 就得给分（--score 1~5）");
    }
    if body.trim().is_empty() && score == 0 {
        bail!("--body 与 --score 不能都是空的（一条什么都没说的反馈没有意义）");
    }
    if body.len() > MAX_BODY {
        bail!("--body 太长（上限 {MAX_BODY} 字节）—— 长文请走文档 / 知识库");
    }
    Ok(())
}

/// 打印一条（人类看的形状）。
fn show_line(f: &Value) {
    let id = f["id"].as_str().unwrap_or("");
    let kind = f["kind"].as_str().unwrap_or("");
    let vis = f["visibility"].as_str().unwrap_or("");
    let status = f["status"].as_str().unwrap_or("");
    let about = format!(
        "{}:{}",
        f["aboutKind"].as_str().unwrap_or(""),
        f["aboutRef"].as_str().unwrap_or("")
    );
    let who = f["author"]["handle"].as_str().unwrap_or("");
    let agent = f["agent"].as_str().unwrap_or("");
    let score = f["score"].as_i64().unwrap_or(0);
    let replies = f["replies"].as_i64().unwrap_or(0);
    let body = f["body"].as_str().unwrap_or("");
    let relayed = if f["relayed"].as_bool().unwrap_or(false) {
        format!(" ⇄从{}搬来（原话：{}）", f["origin"].as_str().unwrap_or("?"), f["originAuthor"].as_str().unwrap_or("?"))
    } else {
        String::new()
    };
    println!(
        "{id}  [{kind}{}] {vis}/{status}  {about}  {who}{}{}{}  {}{}",
        if score > 0 { format!(" {score}分") } else { String::new() },
        if agent.is_empty() { String::new() } else { format!(" via {agent}") },
        if replies > 0 { format!("（{replies} 条回复）") } else { String::new() },
        String::new(),
        body,
        relayed
    );
    // 私有 / 归属状态这类"看不见的东西"要主动说，不然人会以为"就这样"。
    if f["self"].as_bool().unwrap_or(false) {
        println!("         （自己给自己的 —— 聚合里单独数）");
    }
}

fn brief_accepted(v: &Value) -> String {
    let id = v["feedback"]["id"].as_str().unwrap_or("");
    let resolved = v["resolved"].as_bool().unwrap_or(false);
    if resolved {
        format!("✅ 收下了：{id}")
    } else {
        format!(
            "✅ 收下了：{id}（没对上具体的东西：{}）",
            v["note"].as_str().unwrap_or("只有你自己看得到")
        )
    }
}

/* ---------------- 命令实现 ---------------- */

/// 把一条送出去（成功 = Ok，失败 = Err）。**payload 原样发**。
fn deliver(cfg: &CliConfig, payload: &Value, reply_to: &str) -> Result<Value> {
    let token = config::token_opt(cfg);
    let path = if reply_to.trim().is_empty() {
        "/api/feedback".to_string()
    } else {
        format!("/api/feedback/{}/reply", reply_to.trim())
    };
    api::post_json(cfg, &path, token.as_deref(), payload)
}

fn cmd_send(a: &SendArgs) -> Result<()> {
    let (kind, about) = parse_about(&a.about);
    validate(&kind, &about, &a.body, a.score, &a.kind)?;
    let (tcfg, tname) = target_cfg(&config::load(), &a.to)?;
    if kind == "service" {
        ensure_services(&tcfg, &tname, &a.about)?;
    }
    let vis = if a.public { "public" } else { "private" };
    let payload = payload_of(a, &kind, &about, vis, &a.reply_to);
    let row = Spooled {
        id: format!("SP-{}", now_unix()),
        at: now_unix(),
        to: tname.clone(),
        about: format!("{kind}:{about}"),
        payload: payload.clone(),
        tries: 0,
        last_error: String::new(),
    };
    // ① **先落盘**：发了没发出去是两件事，先保证"这条不会丢"。
    append_spool(&row)?;
    if a.queue_only {
        println!("📥 已存进本地队列（没发送）：{}", spool_path().display());
        println!("   等会儿一起送：`ncc feedback relay --flush`");
        return Ok(());
    }
    // ② 再送；送成了把这条从队列里摘掉。
    match deliver(&tcfg, &payload, &a.reply_to) {
        Ok(v) => {
            let mut rows = read_spool();
            rows.retain(|r| r.id != row.id);
            write_spool(&rows)?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{}", brief_accepted(&v));
                println!("   目标：{tname}（{}）", tcfg.base_url());
                if vis == "private" {
                    println!("   私有：只有你与目标拥有者看得到（要公开得说 --public）");
                }
            }
            Ok(())
        }
        Err(e) => {
            let mut rows = read_spool();
            for r in rows.iter_mut() {
                if r.id == row.id {
                    r.tries += 1;
                    r.last_error = e.to_string();
                }
            }
            write_spool(&rows)?;
            // **没送出去就要非 0**：这是"以为发出去了"的经典陷阱。
            bail!(
                "没送出去（已存进本地队列，共 {} 条待传）：{e}\n  队列：{}\n  稍后一起送：`ncc feedback relay --flush`",
                read_spool().len(),
                spool_path().display()
            )
        }
    }
}

fn cmd_ls(a: &LsArgs) -> Result<()> {
    let cfg = config::load();
    let mut q: Vec<String> = Vec::new();
    if !a.about.trim().is_empty() {
        let (kind, about) = parse_about(&a.about);
        q.push(format!("aboutKind={}", api::urlenc(&kind)));
        q.push(format!("aboutRef={}", api::urlenc(&about)));
    }
    if a.mine {
        q.push("mine=1".into());
    }
    if a.inbox {
        q.push("owner=me".into());
    }
    if !a.kind.trim().is_empty() {
        q.push(format!("kind={}", api::urlenc(a.kind.trim())));
    }
    if !a.status.trim().is_empty() {
        q.push(format!("status={}", api::urlenc(a.status.trim())));
    }
    if a.public {
        q.push("visibility=public".into());
    }
    if a.open {
        q.push("unresolved=1".into());
    }
    q.push(format!("page={}", a.page));
    q.push(format!("size={}", a.size));
    let token = config::token_opt(&cfg);
    let v = api::get(&cfg, &format!("/api/feedback?{}", q.join("&")), token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let total = v["total"].as_i64().unwrap_or(0);
    println!("反馈 {total} 条（目标 {}）", cfg.current_name());
    for f in v["feedback"].as_array().cloned().unwrap_or_default() {
        show_line(&f);
    }
    if total == 0 {
        println!("  （没有。默认看不到别人的私有反馈 —— 那是设计，不是坏了）");
    }
    Ok(())
}

fn cmd_get(a: &GetArgs) -> Result<()> {
    let cfg = config::load();
    let token = config::token_opt(&cfg);
    let v = api::get(&cfg, &format!("/api/feedback/{}", api::urlenc(&a.id)), token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let f = &v["feedback"];
    show_line(f);
    if let Some(tags) = f["tags"].as_array() {
        if !tags.is_empty() {
            println!(
                "         标签：{}",
                tags.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" · ")
            );
        }
    }
    if let Some(refs) = f["stateRefs"].as_array() {
        if !refs.is_empty() {
            println!(
                "         关联状态：{}",
                refs.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" · ")
            );
        }
    }
    if let Some(tr) = f["traceRef"].as_str() {
        if !tr.is_empty() {
            println!("         来自运行：{tr}");
        }
    }
    if let Some(hops) = f["hops"].as_array() {
        if !hops.is_empty() {
            println!(
                "         链路：{}",
                hops.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" → ")
            );
        }
    }
    let replies = v["replies"].as_array().cloned().unwrap_or_default();
    if replies.is_empty() {
        println!("         （还没有回复）");
    } else {
        println!("         回复（{} 条）：", replies.len());
        for r in replies {
            println!(
                "           · {}（{}）：{}",
                r["author"]["handle"].as_str().unwrap_or(""),
                r["createdAt"].as_str().unwrap_or(""),
                r["body"].as_str().unwrap_or("")
            );
        }
    }
    // 能做什么，直接说清楚（处置权在服务端判，这里只是把话说清楚）。
    if !f["canResolve"].as_bool().unwrap_or(false) {
        println!("         （你不是这条的目标拥有者：能回，但不能替对方改处置状态）");
    }
    Ok(())
}

fn cmd_reply(a: &ReplyArgs) -> Result<()> {
    let cfg = config::load();
    let token = config::token_opt(&cfg);
    let agent = if a.agent.trim().is_empty() {
        std::env::var("NCC_AGENT").unwrap_or_default()
    } else {
        a.agent.clone()
    };
    if a.body.trim().is_empty() && a.score <= 0 {
        bail!("回复也要有话或分（--body / --score）");
    }
    let body = json!({"body": a.body, "score": a.score, "agent": agent});
    let v = api::post_json(&cfg, &format!("/api/feedback/{}/reply", api::urlenc(&a.id)), token.as_deref(), &body)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("💬 回复已发：{}", v["feedback"]["id"].as_str().unwrap_or(""));
        println!("   （回复继承原帖的可见性：私有对话不会因为有人回一句就公开）");
    }
    Ok(())
}

fn cmd_status(a: &StatusArgs) -> Result<()> {
    if !STATUSES.contains(&a.set.as_str()) {
        bail!("--set 只能是 {}", STATUSES.join("|"));
    }
    let cfg = config::load();
    let token = config::require_token(&cfg)?;
    let v = api::request(
        &cfg,
        "PATCH",
        &format!("/api/feedback/{}", api::urlenc(&a.id)),
        Some(&token),
        Some(&json!({"status": a.set})),
        None,
        &[],
    )?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!(
            "🔧 {} → {}（{}）",
            a.id,
            a.set,
            v["feedback"]["status"].as_str().unwrap_or("")
        );
    }
    if !a.note.trim().is_empty() {
        // 状态是状态，话是话：想说为什么就回一条（内容永远只追加）。
        let body = json!({"body": a.note});
        api::post_json(&cfg, &format!("/api/feedback/{}/reply", api::urlenc(&a.id)), Some(&token), &body)?;
        println!("   （顺带回了一句，你说的那段话也在里面）");
    }
    Ok(())
}

fn cmd_summary(a: &SummaryArgs) -> Result<()> {
    let cfg = config::load();
    let mut q: Vec<String> = Vec::new();
    if !a.about.trim().is_empty() {
        let (kind, about) = parse_about(&a.about);
        q.push(format!("aboutKind={}", api::urlenc(&kind)));
        q.push(format!("aboutRef={}", api::urlenc(&about)));
    }
    if a.inbox {
        q.push("owner=me".into());
    }
    let token = config::token_opt(&cfg);
    let path = if q.is_empty() {
        "/api/feedback/summary".to_string()
    } else {
        format!("/api/feedback/summary?{}", q.join("&"))
    };
    let v = api::get(&cfg, &path, token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let s = &v["summary"];
    println!("反馈 {} 条（目标 {}）", s["count"].as_i64().unwrap_or(0), cfg.current_name());
    let by_kind = s["byKind"].as_object().cloned().unwrap_or_default();
    if !by_kind.is_empty() {
        let list = by_kind
            .iter()
            .map(|(k, n)| format!("{k} {}", n.as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" · ");
        println!("  性质：{list}");
    }
    let by_status = s["byStatus"].as_object().cloned().unwrap_or_default();
    if !by_status.is_empty() {
        let list = by_status
            .iter()
            .map(|(k, n)| format!("{k} {}", n.as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" · ");
        println!("  处置：{list}");
    }
    let scored = s["scored"].as_i64().unwrap_or(0);
    if scored > 0 {
        println!(
            "  带分：{} 条，均分 {:.1}",
            scored,
            s["scoreAvg"].as_f64().unwrap_or(0.0)
        );
    }
    println!(
        "  可见性：公开 {} · 私有 {}（自己给自己记的 {}）",
        s["publicCount"].as_i64().unwrap_or(0),
        s["privateCount"].as_i64().unwrap_or(0),
        s["selfCount"].as_i64().unwrap_or(0)
    );
    let agents = s["agents"].as_object().cloned().unwrap_or_default();
    if !agents.is_empty() {
        let list = agents
            .iter()
            .map(|(k, n)| format!("{k} {}", n.as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" · ");
        println!("  谁在说：{list}");
    }
    let relayed = s["relayed"].as_i64().unwrap_or(0);
    if relayed > 0 {
        println!("  其中从别处搬来的：{relayed} 条（author 记的是搬运者）");
    }
    println!("  {}", v["note"].as_str().unwrap_or(""));
    Ok(())
}

fn cmd_inbox(a: &InboxArgs) -> Result<()> {
    let ls = LsArgs {
        about: String::new(),
        mine: false,
        inbox: true,
        kind: String::new(),
        status: String::new(),
        public: false,
        open: a.open,
        page: 1,
        size: a.size,
        json: a.json,
    };
    cmd_ls(&ls)
}

fn cmd_spool(a: &SpoolArgs) -> Result<()> {
    let rows = read_spool();
    if a.clear {
        if rows.is_empty() {
            println!("队列本来就是空的：{}", spool_path().display());
            return Ok(());
        }
        write_spool(&[])?;
        println!("🧹 清空 {} 条（没送出去的也一起没了）", rows.len());
        return Ok(());
    }
    if a.json {
        let arr: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({"id": r.id, "at": r.at, "to": r.to, "about": r.about,
                       "tries": r.tries, "lastError": r.last_error, "payload": r.payload})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({"spool": arr, "path": spool_path().to_string_lossy()}))?);
        return Ok(());
    }
    println!("本地队列 {} 条：{}", rows.len(), spool_path().display());
    for r in &rows {
        println!(
            "  {}  {}  → {}  {}{}",
            r.id,
            r.about,
            r.to,
            if r.tries > 0 { format!("（试过 {} 次）", r.tries) } else { String::new() },
            if r.last_error.is_empty() { String::new() } else { format!("  上次失败：{}", r.last_error) }
        );
    }
    if !rows.is_empty() {
        println!("  一起送：`ncc feedback relay --flush`");
    }
    Ok(())
}

/// 把本地队列里还没送出去的逐条送到它自己的目标。
///
/// **不丢弃**：失败的原样留着（并把错误记在这条上），下次还能再试。
fn flush_spool(out: &mut Vec<String>) -> Result<()> {
    let cfg = config::load();
    let rows = read_spool();
    if rows.is_empty() {
        return Ok(());
    }
    let mut left: Vec<Spooled> = Vec::new();
    let (mut ok, mut bad) = (0, 0);
    for mut r in rows {
        let tcfg = match config_for(&cfg, &r.to) {
            Ok(c) => c,
            Err(e) => {
                r.tries += 1;
                r.last_error = e.to_string();
                bad += 1;
                left.push(r);
                continue;
            }
        };
        let parent = r.payload["parentId"].as_str().unwrap_or("").to_string();
        match deliver(&tcfg, &r.payload, &parent) {
            Ok(v) => {
                ok += 1;
                out.push(format!(
                    "  ✓ {} → {}：{}",
                    r.id,
                    r.to,
                    v["feedback"]["id"].as_str().unwrap_or("已收下")
                ));
            }
            Err(e) => {
                bad += 1;
                r.tries += 1;
                r.last_error = e.to_string();
                out.push(format!("  ✗ {} → {}：{e}", r.id, r.to));
                left.push(r);
            }
        }
    }
    write_spool(&left)?;
    println!("本地队列：送出 {ok} 条，还剩 {} 条没送出去", left.len());
    if bad > 0 {
        println!("  （没送出去的还在队列里，不会丢；修好网络再 `ncc feedback relay --flush`）");
    }
    Ok(())
}

fn cmd_relay(a: &RelayArgs) -> Result<()> {
    let cfg = config::load();
    let mut lines: Vec<String> = Vec::new();
    if a.flush {
        flush_spool(&mut lines)?;
        for l in &lines {
            println!("{l}");
        }
        lines.clear();
        // `--flush` 单独用 = 只把本地队列送出去（不搬东西）。
        // 想搬什么得再说清楚（`--about` / `--all-mine`）—— 不替你决定把什么传上云。
        if a.about.trim().is_empty() && !a.all_mine {
            return Ok(());
        }
    }
    // 从哪台搬：默认当前目标。
    let from_name = if a.from.trim().is_empty() {
        cfg.current_name()
    } else {
        a.from.trim().to_string()
    };
    let from_cfg = config_for(&cfg, &from_name)?;
    // 搬到哪台：默认 hub。
    let to_name = if a.to.trim().is_empty() {
        hub_name(&cfg)?
    } else {
        a.to.trim().to_string()
    };
    if to_name == from_name {
        bail!("`--from` 与 `--to` 是同一台（{from_name}）—— 搬运是跨机器的事");
    }
    let to_cfg = config_for(&cfg, &to_name)?;

    // 搬什么：显式说（不替你决定把什么传上云）。
    let mut q: Vec<String> = vec!["visibility=public".into()]; // **只搬公开的**
    if !a.about.trim().is_empty() {
        let (kind, about) = parse_about(&a.about);
        q.push(format!("aboutKind={}", api::urlenc(&kind)));
        q.push(format!("aboutRef={}", api::urlenc(&about)));
    } else if a.all_mine {
        q.push("mine=1".into());
    } else {
        bail!(
            "要说清搬什么：`--about <东西>`（关于它的公开反馈）或 `--all-mine`（我发的公开反馈）。\n  \
             私有的**永远不搬** —— 那是这条链的红线"
        );
    }
    q.push(format!("size={}", a.limit.clamp(1, 100)));
    let token_from = config::token_opt(&from_cfg);
    let list = api::get(&from_cfg, &format!("/api/feedback?{}", q.join("&")), token_from.as_deref())?;
    let items = list["feedback"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("没有可搬的公开反馈（{from_name}）—— 私有的不搬，这是设计");
        return Ok(());
    }
    let payload: Vec<Value> = items
        .iter()
        .map(|f| {
            json!({
                "aboutKind": f["aboutKind"], "aboutRef": f["aboutRef"],
                "kind": f["kind"], "score": f["score"], "body": f["body"],
                "tags": f["tags"], "agent": f["agent"],
                "visibility": "public",
                "traceRef": f["traceRef"], "stateRefs": f["stateRefs"],
                "hops": f["hops"],
                // 出处与原作者：重复搬运靠它认出来；原作者只作转述。
                "origin": from_name,
                "originId": f["id"],
                "originAuthor": f["author"]["handle"],
            })
        })
        .collect();
    if a.dry {
        println!("（--dry）会把 {} 条公开反馈从 {from_name} 搬到 {to_name}：", payload.len());
        for f in &items {
            show_line(f);
        }
        return Ok(());
    }
    let token_to = config::require_token(&to_cfg)?;
    let v = api::post_json(
        &to_cfg,
        "/api/feedback/relay",
        Some(&token_to),
        &json!({"items": payload}),
    )?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "⇄ {} → {}：新落 {} 条 · 已经搬过 {} 条 · 跳过 {} 条",
        from_name,
        to_name,
        v["created"].as_i64().unwrap_or(0),
        v["duplicated"].as_i64().unwrap_or(0),
        v["skipped"].as_array().map(|a| a.len()).unwrap_or(0)
    );
    for s in v["skipped"].as_array().cloned().unwrap_or_default() {
        println!("   · 跳过 {}：{}", s["aboutRef"].as_str().unwrap_or(""), s["why"].as_str().unwrap_or(""));
    }
    if let Some(note) = v["note"].as_str() {
        println!("  {note}");
    }
    Ok(())
}

fn cmd_kinds() -> Result<()> {
    println!("aboutKind（说的是什么）");
    println!("  artifact  制品（@命名空间/slug，可带版本）");
    println!("  node      节点 / 机器");
    println!("  service   对外服务（@提供方/slug）—— 只有声明了 services 的目标收");
    println!("  profile   名片（@handle）");
    println!("  run       一次运行（配 --trace <轨迹 id>）");
    println!("  agent     一个 Agent");
    println!("  topic     还没归到具体东西上的一句话");
    println!("\nkind（性质）");
    println!("  report 问题 · praise 表扬 · request 需求 · correction 纠正 · rating 打分（要 --score）");
    println!("\nstatus（处置，只有目标拥有者能改）");
    println!("  open 待处理 · ack 已确认 · resolved 已解决 · wontfix 不处理");
    println!("\n三条红线");
    println!("  1. 反馈只追加、不可改：要补充就再回一条（回复也是一条反馈）");
    println!("  2. 默认私有，公开要 --public；relay 上云**只搬公开的**，私有的永不出机器");
    println!("  3. 搬上来的人以令牌为准：原作者只作为转述写进 originAuthor");
    println!("\n上限：body ≤ {MAX_BODY} 字节 · tags ≤ {MAX_TAGS} · hops ≤ {MAX_HOPS}");
    println!("本地队列：{}", spool_path().display());
    println!("（服务端还会各自报一份：`ncc feedback kinds` 对着目标看，或看 /api/feedback/kinds）");
    Ok(())
}

/// 命令入口。
pub fn cmd(action: &FeedbackCmd) -> Result<()> {
    match action {
        FeedbackCmd::Send(a) => cmd_send(a),
        FeedbackCmd::Ls(a) => cmd_ls(a),
        FeedbackCmd::Get(a) => cmd_get(a),
        FeedbackCmd::Reply(a) => cmd_reply(a),
        FeedbackCmd::Status(a) => cmd_status(a),
        FeedbackCmd::Summary(a) => cmd_summary(a),
        FeedbackCmd::Inbox(a) => cmd_inbox(a),
        FeedbackCmd::Spool(a) => cmd_spool(a),
        FeedbackCmd::Relay(a) => cmd_relay(a),
        FeedbackCmd::Kinds => {
            // 对着目标再问一遍（词表也可能随版本变）；离线就只打本地那份。
            let cfg = config::load();
            let token = config::token_opt(&cfg);
            match api::get(&cfg, "/api/feedback/kinds", token.as_deref()) {
                Ok(v) => {
                    cmd_kinds()?;
                    println!("\n目标 {} 的声明：", cfg.current_name());
                    println!("{}", serde_json::to_string_pretty(&json!({
                        "redLines": v["redLines"], "limits": v["limits"],
                    }))?);
                    Ok(())
                }
                Err(_) => cmd_kinds().context("打印本地词表失败"),
            }
        }
    }
}

/// 给 `ncc rsi learn` 用：拉一批公开 / 我的反馈（读不到就返回空，不炸）。
pub fn fetch_for_learning(cfg: &CliConfig, about: Option<(&str, &str)>, limit: i64) -> Vec<Value> {
    let token = config::token_opt(cfg);
    let mut q: Vec<String> = vec![format!("size={}", limit.clamp(1, 100))];
    match about {
        Some((kind, ref_)) => {
            q.push(format!("aboutKind={}", api::urlenc(kind)));
            q.push(format!("aboutRef={}", api::urlenc(ref_)));
        }
        None => q.push("mine=1".into()),
    }
    match api::get(cfg, &format!("/api/feedback?{}", q.join("&")), token.as_deref()) {
        Ok(v) => v["feedback"].as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}
