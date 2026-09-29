//! NCC Index（索引）与匹配：`ncc index` / `ncc list` / `ncc match`。
//!
//! 三件事，一句话各自说清：
//!
//! ```text
//! ncc index publish booking/hotel --service @me/hotel-booking   我有什么 → 登记进频道
//! ncc list users --channel booking                              索引里有哪些人
//! ncc match "帮我订杭州的酒店" --channel booking                 我有需求 → 谁能在
//! ```
//!
//! 四条口径（与 `ncc services` 的分工别混）：
//!
//! - **service 是声明、index 是登记**：服务声明「我能提供什么」；索引决定它**进哪个
//!   频道、被哪些词检索到**。所以同一条服务可以被索引多次（不同频道 / 不同话术）。
//! - **索引 ≠ 授权**：登记只代表「检索得到」。私有制品要 `ncc grant`，非公开服务要
//!   `service` 授权 —— 与「连接 ≠ 授权」是同一条边界。
//! - **平台权威、节点副本**：`publish` 先写平台（跨网可查），再尽力推已加入的内网节点；
//!   节点推失败**不回滚**，但要**逐台报出来**，并把成功的节点回报给平台。报出来的叫
//!   状态，不报的才叫假装。
//! - **评分不对外显示**：它只作为匹配的内部信誉权重（服务端算分时用），CLI 不打印、
//!   接口不返回。所以 `ncc match` 的排序解释里永远看不到分数。
use anyhow::{bail, Result};
use clap::Args;
use serde_json::{json, Value};

use crate::api;
use crate::config::{self, CliConfig};

/* ---------------- 参数 ---------------- */

/// `ncc index publish <频道> …`
#[derive(Args)]
pub struct PublishArgs {
    /// 频道（自由名，1-3 段小写字母 / 数字 / 连字符，如 booking/hotel）
    pub channel: String,
    /// 索引我的一条对外服务（`@我/slug` 或 `SV-…`）—— 标题 / 分类 / 关键词从它继承
    #[arg(long)]
    pub service: Option<String>,
    /// 索引一个制品 / 能力包（`@我/slug` 或 `R-…`）
    #[arg(long)]
    pub item: Option<String>,
    /// 登记一条**需求**（我要找人办这件事）
    #[arg(long, value_name = "一句话", conflicts_with_all = ["service", "item"])]
    pub need: Option<String>,
    /// 标题（给了引用时可以省略：那时用原件名字）
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub slug: Option<String>,
    #[arg(long)]
    pub summary: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
    /// 可选绑定业务分类（`ncc services catalog` 看取值）
    #[arg(long)]
    pub category: Option<String>,
    #[arg(long)]
    pub region: Option<String>,
    /// 能力标签，可重复：--tag hotel --tag booking
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// 匹配关键词（Agent 用什么话术能召到它），可重复：--intent 订酒店
    #[arg(long = "intent")]
    pub intents: Vec<String>,
    /// 不进频道清单（只有拿到引用的人能查）
    #[arg(long)]
    pub unlisted: bool,
    /// 存着但不参与检索（draft）
    #[arg(long)]
    pub draft: bool,
    /// 只写平台，不推内网节点
    #[arg(long)]
    pub no_push: bool,
    /// 只推给这些节点目标（可重复；缺省推所有 kind=registry 的目标）
    #[arg(long = "node")]
    pub nodes: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

/// `ncc index list` / `ncc list needs`
#[derive(Args, Clone, Default)]
pub struct ListArgs {
    /// 频道（可用前缀：`booking` 命中 `booking/hotel`）
    #[arg(long)]
    pub channel: Option<String>,
    /// service | capability | need
    #[arg(long)]
    pub kind: Option<String>,
    /// supply（供给方，默认）| need（需求方）
    #[arg(long)]
    pub side: Option<String>,
    /// 只看我自己登记的
    #[arg(long)]
    pub mine: bool,
    /// 关键词（标题 / 摘要 / 标签 / 关键词 / 频道）
    #[arg(long)]
    pub q: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

/// `ncc index push <引用>`
#[derive(Args)]
pub struct PushArgs {
    /// 索引引用：`@我/slug` 或 `IX-…`
    pub reference: String,
    /// 只推给这些节点目标（可重复；缺省推所有 kind=registry 的目标）
    #[arg(long = "node")]
    pub nodes: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

/// `ncc list users`
#[derive(Args)]
pub struct UsersArgs {
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    /// supply | need
    #[arg(long)]
    pub side: Option<String>,
    /// 按名字 / 简介 / 频道过滤（本地过滤）
    #[arg(long)]
    pub q: Option<String>,
    #[arg(long, default_value_t = 30)]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

/// `ncc list channels`
#[derive(Args)]
pub struct ChannelsArgs {
    /// 只看某个频道前缀（`booking` 命中 `booking/hotel`）
    #[arg(long)]
    pub prefix: Option<String>,
    #[arg(long)]
    pub json: bool,
}

/// `ncc match "…"`
#[derive(Args)]
pub struct MatchArgs {
    /// 一句需求（自然语言）
    pub intent: String,
    /// 限定频道（前缀匹配：给 `booking` 也能命中 `booking/hotel`）
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub region: Option<String>,
    /// service | capability | need
    #[arg(long)]
    pub kind: Option<String>,
    /// supply = 我在找人办事（默认）；need = 我在找活儿
    #[arg(long, default_value = "supply", value_parser = ["supply", "need"])]
    pub want: String,
    #[arg(long, default_value_t = 10)]
    pub limit: i64,
    /// 匹配源：目标名（内网节点走 `--from <节点名>`）；缺省用当前目标，
    /// 也可以由环境变量 `NCC_INDEX_SOURCE` 指定（系统设置机制）
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 工具 ---------------- */

/// 按名字取一份「指向该目标」的配置（**不改**用户当前目标）。
///
/// 推送与 `--from` 都要用它：往内网节点说话时不能顺手把用户当前目标切走 ——
/// 那是两件事（切目标是用户的动作，不是命令的副作用）。
fn config_for(cfg: &CliConfig, name: &str) -> Result<CliConfig> {
    let t = cfg.by_name(name).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "没有名为 {name} 的目标。看全部：ncc target list；接一个节点：ncc registry add <短链>"
        )
    })?;
    let mut c = cfg.clone();
    c.current = Some(name.to_string());
    c.targets.insert(name.to_string(), t);
    Ok(c)
}

/// 推送目标：显式给的，或所有 kind=registry 的目标。
fn push_targets(cfg: &CliConfig, only: &[String]) -> Vec<String> {
    if !only.is_empty() {
        return only.to_vec();
    }
    cfg.node_names()
}

/// 一次节点推送的结果（**失败也是结果**，必须报出来）。
struct PushReport {
    name: String,
    base: String,
    outcome: Result<String>,
}

/// 把一条索引推到各个内网节点。
///
/// 边界：平台已经写成功才走到这里 —— 节点失败**不回滚**（跨网可查是主目标，
/// 内网副本是加分项），但必须逐台说出来。
fn push_entry(cfg: &CliConfig, targets: &[String], entry: &Value) -> Vec<PushReport> {
    let mut out = Vec::new();
    for name in targets {
        let report = match config_for(cfg, name) {
            Ok(node_cfg) => {
                let token = node_cfg.target().token.clone();
                if token.is_none() {
                    PushReport {
                        name: name.clone(),
                        base: node_cfg.base_url(),
                        outcome: Err(anyhow::anyhow!(
                            "这个目标还没登录（ncc target use {name} && ncc registry login）"
                        )),
                    }
                } else {
                    let body = node_body(entry);
                    let res =
                        api::post_json(&node_cfg, "/api/index", token.as_deref(), &body).map(|d| {
                            d.get("id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "已接收".to_string())
                        });
                    PushReport {
                        name: name.clone(),
                        base: node_cfg.base_url(),
                        outcome: res,
                    }
                }
            }
            Err(e) => PushReport {
                name: name.clone(),
                base: String::new(),
                outcome: Err(e),
            },
        };
        out.push(report);
    }
    out
}

/// 给节点的载荷：平台条目原样送过去（节点只需要存与检索），
/// 但**去掉我们的机器可读细节**（hits / push 状态是平台这边的账）。
fn node_body(entry: &Value) -> Value {
    let mut body = entry.clone();
    if let Some(obj) = body.as_object_mut() {
        for k in ["hits", "push", "mine"] {
            obj.remove(k);
        }
    }
    body
}

fn print_push_reports(reports: &[PushReport]) {
    for r in reports {
        match &r.outcome {
            Ok(v) => println!("   ✓ 节点 {}（{}）已接收：{}", r.name, r.base, v),
            Err(e) => println!("   ✗ 节点 {}（{}）推送失败：{e}", r.name, r.base),
        }
    }
}

/* ---------------- 登记 ---------------- */

/// `ncc index publish <频道> …`
pub fn publish(cfg: &CliConfig, a: &PublishArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    if a.service.is_none() && a.item.is_none() && a.need.is_none() {
        bail!(
            "要说清索引什么：--service @我/slug（我有这项服务）| --item @我/slug（我有这个能力包）| --need \"我要什么\""
        );
    }
    let kind = if a.service.is_some() {
        "service"
    } else if a.item.is_some() {
        "capability"
    } else {
        "need"
    };

    let mut body = json!({
        "kind": kind,
        "channel": a.channel,
        "visibility": if a.unlisted { "unlisted" } else { "public" },
        "status": if a.draft { "draft" } else { "active" },
    });
    // 不用闭包：闭包会一直握着 obj 的可变借用，后面就没法再插字段了（实测的坑）
    let opts: [(&str, &Option<String>); 7] = [
        ("title", &a.title),
        ("slug", &a.slug),
        ("summary", &a.summary),
        ("description", &a.description),
        ("category", &a.category),
        ("region", &a.region),
        ("serviceRef", &a.service),
    ];
    {
        let obj = body.as_object_mut().unwrap();
        for (k, v) in opts {
            if let Some(s) = v {
                if !s.trim().is_empty() {
                    obj.insert(k.to_string(), json!(s.trim()));
                }
            }
        }
        if let Some(i) = &a.item {
            obj.insert("itemRef".into(), json!(i.trim()));
        }
        if let Some(n) = &a.need {
            // 需求：短句当标题与摘要（用户写了就用自己的）
            if a.title.is_none() {
                obj.insert("title".into(), json!(n.trim()));
            }
            if a.summary.is_none() {
                obj.insert("summary".into(), json!(n.trim()));
            }
        }
        if !a.tags.is_empty() {
            obj.insert("tags".into(), json!(a.tags));
        }
        if !a.intents.is_empty() {
            obj.insert("intents".into(), json!(a.intents));
        }
    }

    let d = api::post_json(cfg, "/api/index", Some(&token), &body)?;
    let entry = d.get("index").cloned().unwrap_or(Value::Null);
    let id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let channel = entry
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let reference = entry
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if a.json {
        // 推送结果也要进 JSON（脚本要能判断「推到哪几台了」）
        let reports = if a.no_push {
            Vec::new()
        } else {
            push_entry(cfg, &push_targets(cfg, &a.nodes), &entry)
        };
        let mut out = d.clone();
        out["push"] = json!(reports
            .iter()
            .map(|r| json!({
                "target": r.name, "base": r.base, "ok": r.outcome.is_ok(),
                "detail": match &r.outcome { Ok(v) => v.clone(), Err(e) => e.to_string() },
            }))
            .collect::<Vec<_>>());
        mark_pushed(cfg, &token, &id, &reports);
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("✅ 已索引：{reference}（频道 {channel}）");
    let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let status = entry.get("status").and_then(|v| v.as_str()).unwrap_or("");
    println!("   {title} · {status} · {kind}");
    if let Some(src) = entry.get("source") {
        if let Some(cmd) = src.get("command").and_then(|v| v.as_str()) {
            println!("   原件：{cmd}");
        }
    }
    let intents: Vec<&str> = entry
        .get("intents")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !intents.is_empty() {
        println!("   匹配关键词：{}", intents.join("、"));
    }

    if a.no_push {
        println!("（--no-push：只写了平台，没推内网节点）");
        return Ok(());
    }
    let targets = push_targets(cfg, &a.nodes);
    if targets.is_empty() {
        println!(
            "   内网节点：没接（`ncc registry add <短链>` 接一台，之后 publish 会自动推过去）"
        );
        return Ok(());
    }
    println!("推送内网节点（平台已写成功，节点失败不影响平台）：");
    let reports = push_entry(cfg, &targets, &entry);
    print_push_reports(&reports);
    mark_pushed(cfg, &token, &id, &reports);
    let ok_n = reports.iter().filter(|r| r.outcome.is_ok()).count();
    println!(
        "   {ok_n}/{} 台成功。看看别人搜到什么：ncc match \"…\" --channel {channel}",
        reports.len()
    );
    Ok(())
}

/// 把「推成功的那几台」回报给平台 —— 平台是权威，副本状态得由它记着。
fn mark_pushed(cfg: &CliConfig, token: &str, id: &str, reports: &[PushReport]) {
    if id.is_empty() || reports.is_empty() {
        return;
    }
    let ok: Vec<String> = reports
        .iter()
        .filter(|r| r.outcome.is_ok())
        .map(|r| r.name.clone())
        .collect();
    let _ = api::post_json(
        cfg,
        &format!("/api/index/{id}/pushed"),
        Some(token),
        &json!({ "nodes": ok }),
    );
}

/* ---------------- 列表 / 详情 / 撤回 ---------------- */

/// `ncc index list`
pub fn list(cfg: &CliConfig, a: &ListArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let mut path = format!("/api/index?limit={}", a.limit.clamp(1, 100));
    if let Some(c) = &a.channel {
        path += &format!("&channel={}", urlencode(c));
    }
    if let Some(k) = &a.kind {
        path += &format!("&kind={}", urlencode(k));
    }
    if let Some(s) = &a.side {
        path += &format!("&side={}", urlencode(s));
    }
    if let Some(q) = &a.q {
        path += &format!("&q={}", urlencode(q));
    }
    if a.mine {
        path += "&mine=1";
    }
    let d = api::get(cfg, &path, token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    print_index_rows(&d);
    Ok(())
}

/// 索引表格（列表共用：索引列表 / 需求列表）。
fn print_index_rows(d: &Value) {
    let rows = d
        .get("index")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("（没有匹配的索引）登记一条：ncc index publish <频道> --service @我/slug");
        return;
    }
    let total = d
        .get("total")
        .and_then(|v| v.as_i64())
        .unwrap_or(rows.len() as i64);
    let channel = d.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let scope = if channel.is_empty() {
        "全部频道".to_string()
    } else {
        format!("频道 {channel}")
    };
    println!("共 {total} 条（{scope}）：");
    for r in &rows {
        let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let ch = r.get("channel").and_then(|v| v.as_str()).unwrap_or("");
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let reference = r.get("ref").and_then(|v| v.as_str()).unwrap_or("");
        let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let handle = r
            .pointer("/provider/handle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let hits = r.get("hits").and_then(|v| v.as_i64()).unwrap_or(0);
        let mark = if status == "active" {
            ""
        } else {
            "（未召回）"
        };
        println!("  [{kind}] {ch}  {title}{mark}");
        println!("      {reference}  由 {handle} 登记   被召回 {hits} 次");
    }
}

/// `ncc index show <引用>`
pub fn show(cfg: &CliConfig, reference: &str, as_json: bool) -> Result<()> {
    let token = config::token_opt(cfg);
    let path = index_path(reference)?;
    let d = api::get(cfg, &path, token.as_deref())?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let e = d.get("index").cloned().unwrap_or(Value::Null);
    let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("");
    println!("{}（{}）", s("title"), s("ref"));
    println!(
        "  类型 {} · 频道 {} · 状态 {} · 可见性 {}",
        s("kind"),
        s("channel"),
        s("status"),
        s("visibility")
    );
    if !s("summary").is_empty() {
        println!("  摘要 {}", s("summary"));
    }
    if !s("region").is_empty() {
        println!("  区域 {}", s("region"));
    }
    let list = |k: &str| -> Vec<String> {
        e.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    // 嵌套字段必须走 pointer：`e.get("push.nodes")` 找的是**字面量键** "push.nodes"，
    // 永远不命中 —— 实测踩过：平台已经记下"推到 local"，这里却显示"（没有）"。
    let plist = |p: &str| -> Vec<String> {
        e.pointer(p)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    let tags = list("tags");
    if !tags.is_empty() {
        println!("  标签 {}", tags.join("、"));
    }
    let intents = list("intents");
    if !intents.is_empty() {
        println!("  匹配关键词 {}", intents.join("、"));
    }
    if let Some(h) = e.pointer("/howTo/step").and_then(|v| v.as_str()) {
        println!("  怎么接过去：{h}");
    }
    if let Some(h) = e.pointer("/howTo/endpoint").and_then(|v| v.as_str()) {
        println!("  端点 {h}");
    }
    if let Some(src) = e.pointer("/source/name").and_then(|v| v.as_str()) {
        if !src.is_empty() {
            println!("  原件 {src}");
        }
    }
    if e.get("mine").and_then(|v| v.as_bool()).unwrap_or(false) {
        let nodes = plist("/push/nodes");
        let when = e
            .pointer("/push/lastPushedAt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if nodes.is_empty() {
            println!("  已推节点：（没有）—— ncc index push {} 推一次", s("ref"));
        } else {
            println!("  已推节点 {}（{}）", nodes.join("、"), when);
        }
    }
    Ok(())
}

/// `ncc index rm <引用>` —— 撤回索引（原服务 / 制品不受影响）。
pub fn rm(cfg: &CliConfig, reference: &str) -> Result<()> {
    let token = config::require_token(cfg)?;
    let id = resolve_id(cfg, &token, reference)?;
    api::del(cfg, &format!("/api/index/{id}"), Some(&token))?;
    println!("✅ 已撤回索引 {reference}（原服务 / 制品不受影响：`ncc index publish` 可以再登记）");
    Ok(())
}

/// `ncc index push <引用>` —— 再推一次到内网节点。
pub fn push(cfg: &CliConfig, a: &PushArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    let d = api::get(cfg, &index_path(&a.reference)?, Some(&token))?;
    let entry = d.get("index").cloned().unwrap_or(Value::Null);
    let id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let targets = push_targets(cfg, &a.nodes);
    if targets.is_empty() {
        println!("没有可推的内网节点目标。接一台：ncc registry add <内网短链>");
        return Ok(());
    }
    println!("推送 {} 到 {} 台节点：", a.reference, targets.len());
    let reports = push_entry(cfg, &targets, &entry);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!(reports
                .iter()
                .map(|r| json!({
                    "target": r.name, "base": r.base,
                    "ok": r.outcome.is_ok(),
                    "detail": match &r.outcome { Ok(v) => v.clone(), Err(e) => e.to_string() },
                }))
                .collect::<Vec<_>>()))?
        );
    } else {
        print_push_reports(&reports);
    }
    mark_pushed(cfg, &token, &id, &reports);
    Ok(())
}

/// 把引用换成 id：`IX-…` 直接用，`@handle/slug` 先查一次。
fn resolve_id(cfg: &CliConfig, token: &str, reference: &str) -> Result<String> {
    if reference.starts_with("IX-") {
        return Ok(reference.to_string());
    }
    let d = api::get(cfg, &index_path(reference)?, Some(token))?;
    d.pointer("/index/id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("响应里没有 id"))
}

/* ---------------- 已索引的人 / 频道 ---------------- */

/// `ncc list users` —— 「谁在索引里」。
pub fn users(cfg: &CliConfig, a: &UsersArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let mut path = format!("/api/index/users?limit={}", a.limit.clamp(1, 200));
    if let Some(c) = &a.channel {
        path += &format!("&channel={}", urlencode(c));
    }
    if let Some(k) = &a.kind {
        path += &format!("&kind={}", urlencode(k));
    }
    if let Some(s) = &a.side {
        path += &format!("&side={}", urlencode(s));
    }
    if let Some(q) = &a.q {
        path += &format!("&q={}", urlencode(q));
    }
    let d = api::get(cfg, &path, token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d
        .get("users")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("还没有人登记索引。第一条：ncc index publish <频道> --service @我/slug");
        return Ok(());
    }
    println!(
        "索引里有 {} 个人（供给 {} / 需求 {}）：",
        rows.len(),
        rows.iter()
            .filter_map(|r| r.get("supply").and_then(|v| v.as_i64()))
            .sum::<i64>(),
        rows.iter()
            .filter_map(|r| r.get("needs").and_then(|v| v.as_i64()))
            .sum::<i64>()
    );
    for r in &rows {
        let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let chans: Vec<&str> = r
            .get("channels")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let kinds: Vec<&str> = r
            .get("kinds")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let supply = r.get("supply").and_then(|v| v.as_i64()).unwrap_or(0);
        let needs = r.get("needs").and_then(|v| v.as_i64()).unwrap_or(0);
        println!(
            "  {:<16} {:<14} 供 {} / 需 {}   {}  {}",
            s("handle"),
            s("displayName"),
            supply,
            needs,
            kinds.join("+"),
            chans.join("、")
        );
        if !s("headline").is_empty() {
            println!("      {}", s("headline"));
        }
    }
    println!("看某个人登记了什么：ncc index list --q <名字>；找能办事的人：ncc match \"…\"");
    Ok(())
}

/// `ncc list channels` —— 有哪些检索空间。
pub fn channels(cfg: &CliConfig, a: &ChannelsArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let mut path = "/api/index/channels".to_string();
    if let Some(p) = &a.prefix {
        path += &format!("?prefix={}", urlencode(p));
    }
    let d = api::get(cfg, &path, token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d
        .get("channels")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!("还没有频道。建一个：ncc index publish booking/hotel --service @我/slug");
        return Ok(());
    }
    println!("共 {} 个频道：", rows.len());
    for r in &rows {
        let ch = r.get("channel").and_then(|v| v.as_str()).unwrap_or("");
        let entries = r.get("entries").and_then(|v| v.as_i64()).unwrap_or(0);
        let supply = r.get("supply").and_then(|v| v.as_i64()).unwrap_or(0);
        let needs = r.get("needs").and_then(|v| v.as_i64()).unwrap_or(0);
        let providers = r.get("providers").and_then(|v| v.as_i64()).unwrap_or(0);
        let cats: Vec<&str> = r
            .get("categories")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("zh").and_then(|v| v.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        print!("  {:<22} {entries} 条（供 {supply} / 需 {needs}）· {providers} 人", ch);
        if !cats.is_empty() {
            print!(" · {}", cats.join("/"));
        }
        println!();
    }
    println!("按频道找人：ncc match \"…\" --channel <频道>");
    Ok(())
}

/* ---------------- 匹配 ---------------- */

/// `ncc match "…"` —— 我有需求 → 谁能在（或我在找活儿 → 谁要人）。
pub fn match_intent(cfg: &CliConfig, a: &MatchArgs) -> Result<()> {
    // 匹配源：显式 --from > 环境变量（系统设置机制）> 当前目标
    let source = a.from.clone().or_else(|| {
        std::env::var("NCC_INDEX_SOURCE")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    let (view, source_label) = match source.as_deref() {
        None => (None, cfg.current_name()),
        Some(name) if name == cfg.current_name() => (None, name.to_string()),
        Some(name) => (Some(config_for(cfg, name)?), name.to_string()),
    };
    let use_cfg = view.as_ref().unwrap_or(cfg);
    let token = config::token_opt(use_cfg);

    let mut path = format!(
        "&intent={}&side={}&limit={}",
        urlencode(&a.intent),
        urlencode(&a.want),
        a.limit.clamp(1, 50)
    );
    if let Some(c) = &a.channel {
        path += &format!("&channel={}", urlencode(c));
    }
    if let Some(r) = &a.region {
        path += &format!("&region={}", urlencode(r));
    }
    if let Some(k) = &a.kind {
        path += &format!("&kind={}", urlencode(k));
    }
    let d = api::get(
        use_cfg,
        &format!("/api/match?{}", path.trim_start_matches('&')),
        token.as_deref(),
    )?;

    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }

    let rows = d
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let side_zh = if a.want == "need" {
        "需求"
    } else {
        "能办这件事的人"
    };
    if rows.is_empty() {
        println!("没匹配到{side_zh}。可以：");
        println!("   · 换个说法（把「帮我订杭州的酒店」拆成关键词）");
        println!(
            "   · 放宽频道：ncc match \"{0}\"（去掉 --channel）",
            a.intent
        );
        println!("   · 看索引里都有谁：ncc list users / ncc list channels");
        return Ok(());
    }
    println!(
        "为「{}」找到 {} 个{side_zh}（匹配源 {source_label}，扫了 {} 条候选）：",
        a.intent,
        rows.len(),
        d.get("candidates").and_then(|v| v.as_i64()).unwrap_or(0)
    );
    for (i, r) in rows.iter().enumerate() {
        let e = r.get("index").cloned().unwrap_or(Value::Null);
        let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let score = r.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
        let handle = e
            .pointer("/provider/handle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = e
            .pointer("/provider/displayName")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!();
        println!(
            "{}. {}  [{score} 分] {}{}",
            i + 1,
            s("title"),
            s("kind"),
            if s("channel").is_empty() {
                String::new()
            } else {
                format!(" · {}", s("channel"))
            }
        );
        let dest = ref_of(&e);
        let tail = if dest.is_empty() { String::new() } else { format!("   {dest}") };
        println!("   {handle} {name}{tail}");
        let reasons: Vec<&str> = r
            .get("reasons")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if !reasons.is_empty() {
            println!("   命中：{}", reasons.join(" · "));
        }
        if let Some(step) = e.pointer("/howTo/step").and_then(|v| v.as_str()) {
            println!("   下一步：{step}");
        }
    }
    println!();
    println!("排序 = 相关度 + 内部信誉权重（评分不对外显示，只做加权）；接入仍然照旧要授权。");
    Ok(())
}

/// 取一份匹配结果（**给 MCP 复用**：Agent 面与 CLI 面读同一份数据、同一套排序）。
pub fn fetch_match(
    cfg: &CliConfig,
    intent: &str,
    channel: &str,
    region: &str,
    side: &str,
    limit: i64,
) -> Result<Value> {
    let token = config::token_opt(cfg);
    let mut path = format!(
        "intent={}&side={}&limit={}",
        urlencode(intent),
        urlencode(side),
        limit.clamp(1, 50)
    );
    if !channel.trim().is_empty() {
        path += &format!("&channel={}", urlencode(channel));
    }
    if !region.trim().is_empty() {
        path += &format!("&region={}", urlencode(region));
    }
    api::get(cfg, &format!("/api/match?{path}"), token.as_deref())
}

/// 把匹配结果渲染成 Agent 读的文本（CLI 与 MCP 共用，避免两套说法）。
///
/// ⚠️ 这里**不会出现任何评分数字**：评分只做内部权重。
pub fn render_match_text(v: &Value) -> String {
    let rows = v
        .get("results")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        return "没匹配到人。换个说法，或去掉 --channel 放宽范围（ncc list users 看索引里都有谁）。"
            .to_string();
    }
    let mut out = String::new();
    for (i, r) in rows.iter().enumerate() {
        let e = r.get("index").cloned().unwrap_or(Value::Null);
        let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let handle = e
            .pointer("/provider/handle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = e
            .pointer("/provider/displayName")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push_str(&format!(
            "{}. {}（{} · {}）\n",
            i + 1,
            s("title"),
            s("kind"),
            s("channel")
        ));
        out.push_str(&format!("   登记人 {handle} {name}  {}\n", s("ref")));
        let reasons: Vec<&str> = r
            .get("reasons")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        if !reasons.is_empty() {
            out.push_str(&format!("   命中：{}\n", reasons.join(" · ")));
        }
        if let Some(step) = e.pointer("/howTo/step").and_then(|v| v.as_str()) {
            out.push_str(&format!("   下一步：{step}\n"));
        }
        if let Some(dest) = e.pointer("/howTo/endpoint").and_then(|v| v.as_str()) {
            out.push_str(&format!("   端点：{dest}\n"));
        }
        // 同一份原件被登记多次会被合并展示，这里说明白免得以为漏了
        if let Some(also) = r.get("alsoIndexed").and_then(|v| v.as_i64()) {
            out.push_str(&format!("   （同一原件另有 {also} 条登记）\n"));
        }
    }
    out.push_str("排序 = 相关度 + 内部信誉权重（评分不对外显示）；接入仍然照旧要授权。\n");
    out
}

fn ref_of(e: &Value) -> String {
    let src = e
        .pointer("/source/ref")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !src.is_empty() {
        return format!("→ {src}");
    }
    e.pointer("/howTo/endpoint")
        .and_then(|v| v.as_str())
        .map(|s| format!("→ {s}"))
        .unwrap_or_default()
}

/* ---------------- 小工具 ---------------- */

/// 引用 → 请求路径。
///
/// **两种引用对应两条路由**（服务端也是这样注册的）：
///   `IX-…`        → `/api/index/id/IX-…`
///   `@我/slug`    → `/api/index/@我/slug`（两段：handle + slug）
///
/// 别把 `/` 转义成 `%2F`：gin 的路径参数是**按段**匹配的，转义之后整串会被当成
/// 一个段，于是查不到（而且不会报错，就是 404）。
fn index_path(reference: &str) -> Result<String> {
    let r = reference.trim();
    if r.is_empty() {
        bail!("引用不能为空（写法：@我/slug 或 IX-…）");
    }
    if r.starts_with("IX-") {
        return Ok(format!("/api/index/id/{}", urlencode_path(r)));
    }
    let r = r.strip_prefix('@').unwrap_or(r);
    let mut segs = r.splitn(2, '/');
    let handle = segs.next().unwrap_or("");
    let slug = segs.next().unwrap_or("");
    if handle.is_empty() || slug.is_empty() {
        bail!("引用要写成 @我/slug（或 IX-…），收到：{reference}");
    }
    Ok(format!(
        "/api/index/{}/{}",
        urlencode_path(handle),
        urlencode_path(slug)
    ))
}

/// 查询串里的值：只转义会破坏查询串的字符（`&`、`#`、`+`、空格、`/`）。
/// 刻意不引 percent-encoding 库：这几个字符之外都能原样发。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

/// 路径里的单个段：只转义会改变「段」语义的字符。
fn urlencode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' => out.push_str("%2F"),
            ' ' => out.push_str("%20"),
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            _ => out.push(c),
        }
    }
    out
}
