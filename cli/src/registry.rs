// NCC Registry 节点（内网托管节点）—— `ncc registry …`
//
// 这一组命令把 CLI 当成「Agent 插件」接进一个内网 ncc-registry：
//
//	login    以账号身份登入某个 ncc-registry 节点（复用既有账号体系）
//	join     把本机作为一个节点托管进去（注册 + 心跳，可 --daemon 常驻）
//	status   看这个节点是什么、集群里有谁、我的节点是谁
//	nodes    在本实例上发现可连接的节点（Agent 发现）
//	catalog  看聚合目录（本节点 + 各 worker 的制品）
//	route    问「这个能力该找哪个节点要」（能力路由）
//	leave    把我的节点从本节点下线
//
// 边界（与服务端一致，别在这里加"聪明"的推断）：
// - 注册与心跳是同一件事：第一次 `join` 即注册，之后每次上报续租在线状态。
// - 节点自己声明类型：service（服务）/ agent（为人服务的 Agent）/ assigned（被分配的 Agent）。
// - 连接 ≠ 授权：`ncc nodes link` 只代表找得到；取私有制品仍要 `ncc grant`。
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct LoginArgs {
    /// 账号邮箱
    #[arg(long)]
    pub email: String,
    /// 密码
    #[arg(long)]
    pub password: String,
}

#[derive(clap::Args)]
pub struct JoinArgs {
    /// 节点声明类型：service（服务）| agent（为人服务的 Agent）| assigned（被分配的 Agent）
    #[arg(long, default_value = "agent", value_parser = ["service", "agent", "assigned"])]
    pub kind: String,
    /// 节点名（默认：HOSTNAME 或 os-arch）
    #[arg(long)]
    pub name: Option<String>,
    /// 节点 slug（默认由 name 生成）
    #[arg(long)]
    pub slug: Option<String>,
    /// 所在区域（如 上海-内网 / 机房A）——发现与推荐按它聚合
    #[arg(long)]
    pub region: Option<String>,
    /// 对外地址（可选，便于他人直连这个节点）
    #[arg(long)]
    pub url: Option<String>,
    /// 跑在哪个 Agent 形态里（可选，如 harness-use / cursor）
    #[arg(long)]
    pub agent: Option<String>,
    /// 本节点可提供的能力 kind，逗号分隔（如 mcp,api）
    #[arg(long)]
    pub capabilities: Option<String>,
    /// 托管到哪个命名空间（默认个人命名空间）
    #[arg(long)]
    pub namespace: Option<String>,
    /// 可见性：public（本实例可发现）| private（只有自己看得到）
    #[arg(long, default_value = "public", value_parser = ["public", "private"])]
    pub visibility: String,
    /// 常驻心跳（Ctrl+C 停止）
    #[arg(long)]
    pub daemon: bool,
    /// 常驻心跳间隔（秒）
    #[arg(long, default_value_t = 15)]
    pub interval: u64,
}

#[derive(clap::Args)]
pub struct StatusArgs {
    /// 直接输出原始 JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct NodesArgs {
    /// 按类型过滤：service | agent | assigned
    #[arg(long)]
    pub kind: Option<String>,
    /// 按区域过滤（模糊匹配）
    #[arg(long)]
    pub region: Option<String>,
    /// 关键词（节点名 / slug）
    #[arg(long)]
    pub q: Option<String>,
}

#[derive(clap::Args)]
pub struct CatalogArgs {
    /// 关键词
    pub query: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
}

#[derive(clap::Args)]
pub struct LeaveArgs {
    /// 节点 id（LD-…；缺省按 --name/--slug 或本机默认设备名找我自己的节点）
    pub id: Option<String>,
    /// 节点名
    #[arg(long)]
    pub name: Option<String>,
    /// 节点 slug
    #[arg(long)]
    pub slug: Option<String>,
}

/* ---------------- 工具 ---------------- */

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn arr(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect())
        .unwrap_or_default()
}

fn kind_cn(k: &str) -> &str {
    match k {
        "service" => "服务",
        "agent" => "为人服务的 Agent",
        "assigned" => "被分配的 Agent",
        other => other,
    }
}

/// slugify：与服务端 store.Slugify 同规则（小写、非字母数字变连字符）。
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "x".to_string() } else { out }
}

/// 相对时间（心跳时间戳对人更可读）。
fn ago(t: &str) -> String {
    match secs_since(t) {
        Some(secs) if secs < 60 => format!("{secs} 秒前"),
        Some(secs) if secs < 3600 => format!("{} 分钟前", secs / 60),
        Some(secs) if secs < 86400 => format!("{} 小时前", secs / 3600),
        Some(secs) => format!("{} 天前", secs / 86400),
        None => t.to_string(),
    }
}

/// 距现在多少秒（RFC3339）。解析不了就返回 None。
fn secs_since(t: &str) -> Option<u64> {
    let ts = parse_rfc3339(t)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    if now >= ts {
        Some((now - ts) as u64)
    } else {
        Some(0)
    }
}

/// RFC3339（含时区偏移）→ unix 秒。
fn parse_rfc3339(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    // 取 t[a..z] 并解析成整数；越界或非法都给 None。
    let num = |a: usize, z: usize| -> Option<i64> { t.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // 闰年/月长按民用历算法
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut secs = days * 86400 + h * 3600 + mi * 60 + se;
    // 时区：Z 或 ±HH:MM
    let tz = &t[19..];
    if let Some(rest) = tz.strip_prefix('+') {
        if let Some((hh, mm)) = rest.split_once(':') {
            secs -= hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60;
        }
    } else if let Some(rest) = tz.strip_prefix('-') {
        if let Some((hh, mm)) = rest.split_once(':') {
            secs += hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60;
        }
    }
    Some(secs)
}

/* ---------------- login ---------------- */

/// `ncc registry login` —— 以账号身份登入一个 ncc-registry 节点。
pub fn login(cfg: &CliConfig, a: &LoginArgs) -> Result<()> {
    let body = json!({ "email": a.email, "password": a.password });
    let d = api::post_json(cfg, "/api/auth/login", None, &body)?;
    let token = d["token"].as_str().context("响应缺少 token")?;
    let u = &d["user"];
    crate::save_session(
        cfg,
        token,
        u["email"].as_str().unwrap_or(a.email.as_str()),
        u["name"].as_str().unwrap_or(""),
    )?;
    println!(
        "✅ 已登入 {}（{}）",
        u["email"].as_str().unwrap_or(""),
        u["name"].as_str().unwrap_or("")
    );
    println!("   目标节点 {}", cfg.base_url);
    if let Ok(meta) = api::get(cfg, "/api/meta", Some(token)) {
        let n = &meta["node"];
        println!(
            "   {} · {} · {} · 角色 {}",
            s(n, "name"),
            s(n, "id"),
            s(n, "version"),
            s(n, "role")
        );
        println!("   控制台 {}", s(&meta, "console"));
    }
    println!("\n下一步：把自己这台机器托管进去 → ncc registry join --kind agent --region <你的区域>");
    Ok(())
}

/* ---------------- join ---------------- */

fn report_body(a: &JoinArgs, name: &str) -> Value {
    let caps: Vec<String> = a
        .capabilities
        .as_deref()
        .map(|x| x.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    json!({
        "name": name,
        "slug": a.slug.clone().unwrap_or_default(),
        "kind": a.kind,
        "region": a.region.clone().unwrap_or_default(),
        "url": a.url.clone().unwrap_or_default(),
        "agent": a.agent.clone().unwrap_or_default(),
        "capabilities": caps,
        "visibility": a.visibility,
        "namespace": a.namespace.clone().unwrap_or_default(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// `ncc registry join` —— 把本机作为一个节点托管进 ncc-registry。
pub fn join(cfg: &CliConfig, a: &JoinArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let name = a.name.clone().unwrap_or_else(crate::default_device_name);
    let body = report_body(a, &name);

    let report = |token: &str| -> Result<()> {
        let d = api::post_json(cfg, "/api/nodes/heartbeat", Some(token), &body)?;
        let n = &d["node"];
        let created = d["created"].as_bool().unwrap_or(false);
        println!(
            "{} {} · {} · {} · 能力 {} · {}",
            if created { "⬆ 已托管" } else { "⬆ 心跳" },
            format!("@{}", s(&n["namespace"], "slug")),
            format!("{}/{}", s(&n["namespace"], "slug"), s(n, "slug")),
            kind_cn(s(n, "kind")),
            arr(n.get("capabilities")).join(","),
            {
                let ls = s(n, "lastSeen");
                if ls.is_empty() { String::new() } else { format!("lastSeen {}", ago(ls)) }
            },
        );
        match d.get("registry") {
            Some(r) => println!(
                "   托管到 {}（{}·{}）",
                s(r, "name"),
                s(r, "url"),
                if s(r, "role") == "master" { "master" } else { "worker" }
            ),
            None => {}
        }
        Ok(())
    };

    if a.daemon {
        println!(
            "守护心跳：每 {}s 上报到 {}（Ctrl+C 停止）",
            a.interval.max(1),
            cfg.base_url
        );
        loop {
            report(&t)?;
            std::thread::sleep(Duration::from_secs(a.interval.max(1)));
        }
    }
    report(&t)?;
    println!("\n看集群与我的节点：ncc registry status");
    Ok(())
}

/* ---------------- status ---------------- */

/// `ncc registry status` —— 本节点 + 集群 + 我的托管节点。
pub fn status(cfg: &CliConfig, a: &StatusArgs) -> Result<()> {
    let meta = api::get(cfg, "/api/meta", config::token_opt(cfg).as_deref())?;
    let cluster = api::get(cfg, "/api/cluster", config::token_opt(cfg).as_deref())?;

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "meta": meta, "cluster": cluster }))?
        );
        return Ok(());
    }

    let n = &meta["node"];
    let role = if s(n, "role") == "master" { "master（权威节点）" } else { "worker（边缘托管点）" };
    println!("NCC Registry 节点  {}", s(&meta, "console"));
    println!("  角色   {role}");
    println!("  节点   {}  {}  区域 {}", s(n, "name"), s(n, "id"), {
        let r = s(n, "region");
        if r.is_empty() { "—".to_string() } else { r.to_string() }
    });
    println!("  地址   {}  版本 {}", s(n, "url"), s(n, "version"));
    let c = &meta["counts"];
    println!(
        "  规模   制品（公开已发布）{} · 托管节点 {} · 账号 {}",
        c["artifacts"].as_i64().unwrap_or(0),
        c["hostedNodes"].as_i64().unwrap_or(0),
        c["users"].as_i64().unwrap_or(0)
    );

    println!("\n集群（多节点）");
    let dot = |online: bool| if online { "●" } else { "○" };
    println!(
        "  {} {:<20} {:<28} 制品 {:<5} 节点 {:<5} {}",
        dot(true),
        s(&cluster["self"], "name"),
        s(&cluster["self"], "url"),
        cluster["self"]["artifacts"].as_i64().unwrap_or(0),
        cluster["self"]["nodes"].as_i64().unwrap_or(0),
        s(&cluster["self"], "role")
    );
    for w in cluster["workers"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {} {:<20} {:<28} 制品 {:<5} 节点 {:<5} worker · {}",
            dot(w["online"].as_bool().unwrap_or(false)),
            s(&w, "name"),
            s(&w, "url"),
            w["artifacts"].as_i64().unwrap_or(0),
            w["nodes"].as_i64().unwrap_or(0),
            ago(s(&w, "lastSeen"))
        );
    }
    let t = &cluster["totals"];
    println!(
        "  合计 {} 个节点（在线 {}）· 制品 {}",
        t["nodes"].as_i64().unwrap_or(0),
        t["onlineNodes"].as_i64().unwrap_or(1),
        t["artifacts"].as_i64().unwrap_or(0)
    );

    if let Some(m) = cluster.get("master").filter(|m| !m.is_null()) {
        println!(
            "  master {}（{}）{}",
            s(m, "name"),
            s(m, "url"),
            if m["online"].as_bool().unwrap_or(false) { "在线" } else { "暂不可达" }
        );
        if let Some(e) = cluster.get("lastError").and_then(|x| x.as_str()) {
            if !e.is_empty() {
                println!("  ⚠ 最近心跳失败：{e}");
            }
        }
    }

    if config::token_opt(cfg).is_some() {
        if let Ok(d) = api::get(cfg, "/api/nodes", config::token_opt(cfg).as_deref()) {
            let mine = d["nodes"].as_array().cloned().unwrap_or_default();
            let linked = d["linked"].as_array().cloned().unwrap_or_default();
            println!("\n我的托管节点（{}）", mine.len());
            if mine.is_empty() {
                println!("  还没有：ncc registry join --kind agent --region <你的区域>");
            }
            for x in &mine {
                println!(
                    "  {} {:<20} {:<22} {:<14} {}",
                    dot(x["online"].as_bool().unwrap_or(false)),
                    format!("@{}/{}", s(&x["namespace"], "slug"), s(x, "slug")),
                    kind_cn(s(x, "kind")),
                    s(x, "region"),
                    ago(s(x, "lastSeen"))
                );
                let caps = arr(x.get("capabilities"));
                if !caps.is_empty() {
                    println!("     能力 {}", caps.join(", "));
                }
            }
            println!("\n我连接的节点（{}）", linked.len());
            for x in &linked {
                println!(
                    "  {} {:<20} {:<22} {}",
                    dot(x["online"].as_bool().unwrap_or(false)),
                    s(&x["link"], "label"),
                    kind_cn(s(x, "kind")),
                    s(x, "url")
                );
            }
        }
    } else {
        println!("\n（未登录：看不到「我的托管节点」。先 ncc registry login）");
    }
    Ok(())
}

/* ---------------- nodes / catalog / route ---------------- */

/// `ncc registry nodes` —— 在本实例上发现可连接的节点。
pub fn nodes(cfg: &CliConfig, a: &NodesArgs) -> Result<()> {
    let mut qs: Vec<String> = Vec::new();
    if let Some(k) = a.kind.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("kind={}", api::urlenc(k)));
    }
    if let Some(r) = a.region.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("region={}", api::urlenc(r)));
    }
    if let Some(q) = a.q.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("q={}", api::urlenc(q)));
    }
    let path = if qs.is_empty() {
        "/api/nodes/discover".to_string()
    } else {
        format!("/api/nodes/discover?{}", qs.join("&"))
    };
    let d = api::get(cfg, &path, config::token_opt(cfg).as_deref())?;
    let list = d["nodes"].as_array().cloned().unwrap_or_default();
    println!("本实例上可连接的节点（{}）", d["total"].as_i64().unwrap_or(0));
    if list.is_empty() {
        println!("  没有匹配的节点。换个 --kind/--region，或让同事 `ncc registry join`。");
        return Ok(());
    }
    for x in &list {
        println!(
            "  {} {:<22} {:<24} {:<14} {}",
            if x["online"].as_bool().unwrap_or(false) { "●" } else { "○" },
            format!("@{}/{}", s(&x["namespace"], "slug"), s(x, "slug")),
            x["name"].as_str().unwrap_or(""),
            kind_cn(s(x, "kind")),
            s(x, "url")
        );
        let mut meta = vec![];
        if !s(x, "region").is_empty() {
            meta.push(format!("区域 {}", s(x, "region")));
        }
        if !s(&x["owner"], "name").is_empty() {
            meta.push(format!("归属 {}", s(&x["owner"], "name")));
        }
        let caps = arr(x.get("capabilities"));
        if !caps.is_empty() {
            meta.push(format!("能力 {}", caps.join(",")));
        }
        meta.push(format!("心跳 {}", ago(s(x, "lastSeen"))));
        println!("     {}", meta.join(" · "));
    }
    println!("\n收进我的连接表：ncc nodes link @命名空间/节点slug --label \"我给它的名字\"");
    Ok(())
}

/// `ncc registry catalog` —— 聚合目录（本节点 + 各 worker）。
pub fn catalog(cfg: &CliConfig, a: &CatalogArgs) -> Result<()> {
    let mut qs: Vec<String> = Vec::new();
    if let Some(q) = a.query.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("q={}", api::urlenc(q)));
    }
    if let Some(k) = a.kind.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("kind={}", api::urlenc(k)));
    }
    if let Some(t) = a.tag.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("tag={}", api::urlenc(t)));
    }
    let path = if qs.is_empty() {
        "/api/cluster/directory".to_string()
    } else {
        format!("/api/cluster/directory?{}", qs.join("&"))
    };
    let d = api::get(cfg, &path, config::token_opt(cfg).as_deref())?;
    println!(
        "聚合目录 {} 条（本地 {} · 远端 {}）",
        d["total"].as_i64().unwrap_or(0),
        d["local"].as_i64().unwrap_or(0),
        d["remote"].as_i64().unwrap_or(0)
    );
    for it in d["items"].as_array().cloned().unwrap_or_default() {
        let via = &it["via"];
        let src = if s(via, "role") == "self" {
            "本节点".to_string()
        } else {
            format!("worker {}", s(via, "nodeName"))
        };
        println!(
            "  [{}] {}  {}  {}⬇  ← {}",
            s(&it, "kind"),
            s(&it, "ref"),
            s(&it, "status"),
            it["downloads"].as_i64().unwrap_or(0),
            src
        );
        if !s(&it, "summary").is_empty() {
            println!("      {}", s(&it, "summary"));
        }
    }
    println!("\n装下来（master 会自动代理持有者的字节）：ncc install @命名空间/slug");
    Ok(())
}

/// `ncc registry route` —— 这个能力该找哪个节点要。
pub fn route(cfg: &CliConfig, target: &str) -> Result<()> {
    let d = api::get(
        cfg,
        &format!("/api/nodes/route?ref={}", api::urlenc(target)),
        config::token_opt(cfg).as_deref(),
    )?;
    if !d["resolved"].as_bool().unwrap_or(false) {
        println!("✗ 集群里没人持有 {target}");
        println!("  本节点与各 worker 的目录里都没有它。");
        return Ok(());
    }
    println!("{} → {} 个候选", target, d["count"].as_i64().unwrap_or(0));
    for c in d["candidates"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {:<6} {:<20} {:<28} {}",
            s(&c, "role"),
            s(&c, "nodeName"),
            s(&c, "nodeUrl"),
            if c["online"].is_null() {
                "在线".to_string()
            } else if c["online"].as_bool().unwrap_or(false) {
                "在线".to_string()
            } else {
                "离线".to_string()
            }
        );
        println!("     下载 {}", s(&c, "download"));
    }
    println!("\n统一入口（master 会代理到持有者）：{}", s(&d, "download"));
    Ok(())
}

/* ---------------- leave ---------------- */

/// `ncc registry leave` —— 把我的节点从本节点下线。
pub fn leave(cfg: &CliConfig, a: &LeaveArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let id = match a.id.clone() {
        Some(id) if !id.is_empty() => id,
        _ => {
            let want_name = a.name.clone().unwrap_or_else(crate::default_device_name);
            let d = api::get(cfg, "/api/nodes", Some(&t))?;
            let mine = d["nodes"].as_array().cloned().unwrap_or_default();
            let hit = mine.iter().find(|n| {
                let by_slug = a.slug.as_deref().map(|sl| s(n, "slug") == sl).unwrap_or(false);
                by_slug || s(n, "name") == want_name || s(n, "slug") == slugify(&want_name)
            });
            match hit {
                Some(n) => s(n, "id").to_string(),
                None => bail!(
                    "没找到你的节点「{}」（用 ncc registry status 看看节点名，或 --id 指定）",
                    want_name
                ),
            }
        }
    };
    let d = api::request(cfg, "DELETE", &format!("/api/nodes/{id}"), Some(&t), None, None, &[])?;
    if d["ok"].as_bool().unwrap_or(false) {
        println!("✅ 已下线节点 {id}（下次心跳会重新注册）");
    } else {
        println!("✗ 下线失败：{d}");
    }
    Ok(())
}
