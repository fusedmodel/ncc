// NCC Index 的**状态、占用（事务）与状态队列** —— `ncc index {state,hold,release,advance,queue}`。
//
// 为什么这三样在 CLI 里也要有面：
//
//	匹配把人**找到**只是开始。真正的交互（订房、约讲师、切换工作状态）需要一个**原子的
//	占位**（房间不能被卖两遍），而且每次状态变化要能**同步**给关心它的人。
//
// 四条边界（与服务端 `model/indexstate.go`、`httpapi/indexstate.go` 一致）：
//  1. **占用是事务**：`hold` 在一个数据库事务里占名额，满员就是 409 no_slots ——
//     CLI 把服务端那句话原样报出来，不美化、不重试。
//  2. **认领幂等**：同一个 `--order-ref` 重复 `hold` 不会多占名额（重试是安全的）。
//  3. **占位是租约**：`--ttl` 到点自动归还，不用人来清；不办事了用 `release` 提前还。
//  4. **状态推进是 CAS**：`--expect` 对不上就失败 —— 两个人同时点"成交"只能成一个。
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::Result;
use serde_json::{json, Value};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct StateArgs {
    /// 条目引用：`@handle/slug` 或 `IX-…`
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct HoldArgs {
    /// 条目引用：`@handle/slug` 或 `IX-…`
    pub reference: String,
    /// 租约秒数（缺省用条目自己的 holdTtl）
    #[arg(long)]
    pub ttl: Option<u32>,
    /// 外部单号（订单号 / 工作单号）：**幂等键**，重复认领不会多占名额
    #[arg(long = "order-ref")]
    pub order_ref: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
    /// 代表谁发起的（Agent / 应用标识）
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ReleaseArgs {
    /// 条目引用：`@handle/slug` 或 `IX-…`
    pub reference: String,
    /// 指定释放哪一条占位（缺省按 `--order-ref`，再缺省按"我最早那条"）
    #[arg(long)]
    pub hold: Option<String>,
    #[arg(long = "order-ref")]
    pub order_ref: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct AdvanceArgs {
    /// 条目引用：`@handle/slug` 或 `IX-…`
    pub reference: String,
    /// 目标状态：done（已成）/ closed（关闭）/ open（重新开放）
    #[arg(long, value_parser = ["done", "closed", "open"])]
    pub to: String,
    /// 期望的当前状态（CAS）：对不上就报冲突，避免两个人各点一次「成交」
    #[arg(long)]
    pub expect: Option<String>,
    /// 成交凭据（订单号 / 工作单号），会进状态队列
    #[arg(long = "order-ref")]
    pub order_ref: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct QueueArgs {
    /// 游标：只要 seq 大于它的（0 = 从头）。用上一条返回的 cursor 继续拉
    #[arg(long, default_value_t = 0)]
    pub since: i64,
    /// 只看某个频道
    #[arg(long)]
    pub channel: Option<String>,
    /// 只看与我有关的主体（我登记的索引 / 我自己的名片）
    #[arg(long)]
    pub mine: bool,
    /// index | profile（缺省都要）
    #[arg(long, value_parser = ["index", "profile"])]
    pub subject: Option<String>,
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
    /// 跟着看：每 2 秒拉一次增量，Ctrl+C 停
    #[arg(long)]
    pub watch: bool,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 工具 ---------------- */

fn token(cfg: &CliConfig) -> Result<String> {
    config::require_token(cfg)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// 条目引用进查询串（`@handle/slug` 里的 `/` 会被转义 —— 服务端收到的是解码后的值）。
fn ref_q(reference: &str) -> String {
    api::urlenc(reference.trim().trim_start_matches('@'))
}

fn state_cn(st: &str) -> &str {
    match st {
        "open" => "可接",
        "held" => "已占满",
        "done" => "已成",
        "closed" => "已关闭",
        other => other,
    }
}

/// 打印一段状态（`state` 子命令与 `hold/release/advance` 的回执共用）。
fn print_state(st: &Value) {
    println!(
        "状态 {} · 名额 {}/{}（还能接 {}）",
        state_cn(s(st, "state")),
        i(st, "held"),
        i(st, "slots"),
        i(st, "remaining"),
    );
    if let Some(ttl) = st.get("holdTtl").and_then(|v| v.as_i64()) {
        if ttl > 0 {
            println!("占位默认租约 {ttl} 秒（到点自动归还）");
        }
    }
    let holds = st
        .get("holds")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if holds.is_empty() {
        println!("当前没有占位");
        return;
    }
    println!("当前占位（{}）", holds.len());
    for h in &holds {
        let who = s(&h["holder"], "handle");
        let who = if who.is_empty() {
            s(&h["holder"], "userId")
        } else {
            who
        };
        let agent = s(&h["holder"], "agent");
        let mark = if h["mine"].as_bool().unwrap_or(false) { "（我的）" } else { "" };
        println!(
            "  {} {}{}{}  到期 {}",
            s(h, "id"),
            who,
            if agent.is_empty() { String::new() } else { format!(" · {agent}") },
            mark,
            s(h, "expiresAt").get(0..19).unwrap_or(""),
        );
        if !s(h, "ref").is_empty() {
            println!("     单号 {}  {}", s(h, "ref"), s(h, "note"));
        }
    }
}

/* ---------------- 子命令 ---------------- */

/// `ncc index state <引用>` —— 这条现在能不能接、还剩几个名额、谁占着。
pub fn state(cfg: &CliConfig, a: &StateArgs) -> Result<()> {
    let t = token(cfg)?;
    let d = api::get(cfg, &format!("/api/index/state?entry={}", ref_q(&a.reference)), Some(&t))?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let e = &d["index"];
    println!("{}  [{}]  {}", s(e, "title"), s(e, "kind"), s(e, "channel"));
    print_state(&d["state"]);
    if e["accepting"].as_bool().unwrap_or(false) {
        println!("\n要接这一单：ncc index hold {} --order-ref <单号>", a.reference);
    } else {
        println!("\n现在不接单（已成 / 已关闭 / 占满）。");
    }
    Ok(())
}

/// `ncc index hold <引用> --order-ref <单号>` —— 认领一份名额（= 一次交互的开始）。
pub fn hold(cfg: &CliConfig, a: &HoldArgs) -> Result<()> {
    let t = token(cfg)?;
    let body = json!({
        "entry": a.reference,
        "ttl": a.ttl.unwrap_or(0),
        "ref": a.order_ref.clone().unwrap_or_default(),
        "note": a.note.clone().unwrap_or_default(),
        "agent": a.agent.clone().unwrap_or_default(),
    });
    let d = api::post_json(cfg, "/api/index/holds", Some(&t), &body)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    if d["reused"].as_bool().unwrap_or(false) {
        println!("✅ 这个单号已经占过了（幂等：没有多占名额）");
    } else {
        println!("✅ 已占位");
    }
    print_state(&d["state"]);
    println!("\n下一步：办完 → ncc index advance {} --to done；不办了 → ncc index release {}", a.reference, a.reference);
    Ok(())
}

/// `ncc index release <引用>` —— 释放占位（名额立刻回到池子里）。
pub fn release(cfg: &CliConfig, a: &ReleaseArgs) -> Result<()> {
    let t = token(cfg)?;
    let body = json!({
        "entry": a.reference,
        "holdId": a.hold.clone().unwrap_or_default(),
        "orderRef": a.order_ref.clone().unwrap_or_default(),
        "note": a.note.clone().unwrap_or_default(),
    });
    let d = api::post_json(cfg, "/api/index/holds/release", Some(&t), &body)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("✅ 已释放 {}", s(&d, "holdId"));
    print_state(&d["state"]);
    Ok(())
}

/// `ncc index advance <引用> --to done|closed|open` —— 推进状态（登记者本人，CAS）。
pub fn advance(cfg: &CliConfig, a: &AdvanceArgs) -> Result<()> {
    let t = token(cfg)?;
    let body = json!({
        "entry": a.reference,
        "to": a.to,
        "expect": a.expect.clone().unwrap_or_default(),
        "orderRef": a.order_ref.clone().unwrap_or_default(),
        "note": a.note.clone().unwrap_or_default(),
    });
    let d = api::post_json(
        cfg,
        "/api/index/state/advance",
        Some(&t),
        &body,
    )?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    match a.to.as_str() {
        "done" => println!("✅ 已成（这一单定了：不再被匹配推荐，占位记为成交）"),
        "closed" => println!("✅ 已关闭（占位都释放了，名额还回池子）"),
        _ => println!("✅ 已重新开放（已有占位不动 —— 那些是别人手上的约定）"),
    }
    print_state(&d["state"]);
    Ok(())
}

/// `ncc index queue [--since N] [--watch]` —— 增量拉状态队列。
///
/// 游标就是 `seq`：把上一条返回的 `cursor` 交给 `--since`，只拿新增的那几条。
/// 这是"同步"的那一半 —— 别人订完了房、谁切换了工作状态，都从这里看到。
pub fn queue(cfg: &CliConfig, a: &QueueArgs) -> Result<()> {
    let t = token(cfg)?;
    let mut since = a.since;
    let mut first = true;
    loop {
        let mut path = format!("since={}&limit={}", since, a.limit.clamp(1, 500));
        if let Some(c) = a.channel.as_deref().filter(|x| !x.trim().is_empty()) {
            path += &format!("&channel={}", api::urlenc(c));
        }
        if let Some(s2) = a.subject.as_deref().filter(|x| !x.trim().is_empty()) {
            path += &format!("&subject={}", api::urlenc(s2));
        }
        if a.mine {
            path += "&mine=1";
        }
        let d = api::get(cfg, &format!("/api/index/state/queue?{path}"), Some(&t))?;
        if a.json {
            println!("{}", serde_json::to_string_pretty(&d)?);
            if !a.watch {
                return Ok(());
            }
        } else {
            let events = d.get("events").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if first && events.is_empty() {
                println!("从 seq {since} 起没有新变化。");
            }
            for ev in &events {
                print_event(ev);
            }
            since = d.get("cursor").and_then(|v| v.as_i64()).unwrap_or(since);
            first = false;
            if !a.watch {
                println!(
                    "\n游标 {}（队列头 {}）；下次拉：ncc index queue --since {}",
                    since,
                    d.get("head").and_then(|v| v.as_i64()).unwrap_or(0),
                    since
                );
                return Ok(());
            }
        }
        if !a.watch {
            return Ok(());
        }
        if since == 0 {
            since = d.get("cursor").and_then(|v| v.as_i64()).unwrap_or(0);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn print_event(ev: &Value) {
    let subject = s(ev, "subject");
    let tag = if subject == "profile" { "人" } else { "件" };
    let at = s(ev, "at");
    let actor = s(&ev["actor"], "handle");
    let who = if actor.is_empty() { s(&ev["actor"], "userId") } else { actor };
    println!(
        "  #{}  {} {tag} {}",
        i(ev, "seq"),
        at.get(0..19).unwrap_or(""),
        s(ev, "line"),
    );
    let order = s(ev, "orderRef");
    let note = s(ev, "note");
    if !order.is_empty() || !note.is_empty() {
        println!("        单号 {order}  {note}");
    }
    if !who.is_empty() {
        println!("        发起人 {who}");
    }
}
