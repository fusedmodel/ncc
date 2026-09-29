// NCC Profile：命令行查看 / 设置自己的名片（定位角色 + 作品集 + 已发布能力）。
//
// 关键约定：PUT /api/profile/me 是**整体替换**，所以 `ncc profile set` 一律
// 先读现状、只覆盖显式给出的字段再写回 —— 否则会把没提到的字段清空。
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct SetArgs {
    /// 用户名（决定短链 ncc.ai/{username} 与发布命名空间 @username）
    #[arg(long)]
    pub username: Option<String>,
    /// 显示名
    #[arg(long = "name")]
    pub display_name: Option<String>,
    /// 一句话定位
    #[arg(long)]
    pub headline: Option<String>,
    /// 个人简介
    #[arg(long)]
    pub bio: Option<String>,
    /// 所在地
    #[arg(long)]
    pub location: Option<String>,
    /// 定位角色，逗号分隔（id 见 `ncc profile roles`）
    #[arg(long)]
    pub roles: Option<String>,
    /// 技能标签，逗号分隔
    #[arg(long)]
    pub skills: Option<String>,
    /// 接洽状态：open | collab | hiring | busy（传空串清除）
    #[arg(long)]
    pub availability: Option<String>,
    /// 可见性：public（进人才目录）| unlisted（仅直链可见）
    #[arg(long)]
    pub visibility: Option<String>,
    /// 联系邮箱
    #[arg(long)]
    pub email: Option<String>,
    /// 外链，可重复：--link github=https://github.com/you
    #[arg(long = "link", value_name = "KEY=VALUE")]
    pub links: Vec<String>,
}

#[derive(clap::Subcommand)]
pub enum WorkCmd {
    /// 列出我的作品
    List,
    /// 添加作品
    Add(WorkAddArgs),
    /// 删除作品（id 见 `ncc profile work list`）
    Rm { id: String },
}

#[derive(clap::Args)]
pub struct WorkAddArgs {
    #[arg(long)]
    pub title: String,
    /// 一句话说明
    #[arg(long)]
    pub summary: Option<String>,
    /// 该作品对应的角色 id
    #[arg(long)]
    pub role: Option<String>,
    /// 标签，逗号分隔
    #[arg(long)]
    pub tags: Option<String>,
    /// 年份
    #[arg(long)]
    pub year: Option<String>,
    /// 外链地址（不关联站内对象时使用）
    #[arg(long)]
    pub url: Option<String>,
    /// 关联到 NCC Share 的 id（S-…）
    #[arg(long)]
    pub share: Option<String>,
    /// 关联到 Registry 条目 id（R-…）
    #[arg(long)]
    pub item: Option<String>,
}

#[derive(clap::Args)]
pub struct FollowArgs {
    /// 用户名（不带 @）
    pub handle: String,
    /// 备注，**只有你自己看得到**（不传则不动已有备注）
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(clap::Args)]
pub struct RateArgs {
    /// 用户名（不带 @）
    pub handle: String,
    /// 评分，1~5 的整数
    #[arg(long)]
    pub score: i64,
    /// 公开评语（不传则**清空**已有评语 —— 与服务端「整行覆盖」一致）
    #[arg(long)]
    pub note: Option<String>,
}

/* ---------------- 工具 ---------------- */

fn csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 短链展示：注册中心基址 + 用户名。
/// 自托管时基址就是实例地址，公网则形如 https://ncc.ai/{username}。
fn short_link(cfg: &CliConfig, username: &str) -> String {
    format!("{}/{}", cfg.base_url().trim_end_matches('/'), username)
}

/// 取 JSON 里的字符串数组；缺失返回空。
fn arr(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str()).map(String::from).collect())
        .unwrap_or_default()
}

fn role_label(catalog: &Value, id: &str) -> String {
    catalog
        .get("roles")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.iter().find(|r| r.get("id").and_then(|v| v.as_str()) == Some(id)))
        // 角色目录的中英标签都在服务端；CLI 输出统一用中文标签
        .and_then(|r| r.get("zh").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(|| id.to_string())
}

fn availability_label(v: &str) -> &str {
    match v {
        "open" => "开放机会",
        "collab" => "寻找协作",
        "hiring" => "正在招募",
        "busy" => "暂不接洽",
        _ => "",
    }
}

/// handle 规范化：接口路径统一用不带 `@` 的写法。
fn norm_handle(h: &str) -> String {
    h.trim().trim_start_matches('@').to_string()
}

/// 星级渲染。冒烟断言的 `★★★★★` 就是它 —— 满星正好五个字符。
fn stars(score: i64) -> String {
    let n = score.clamp(0, 5) as usize;
    format!("{}{}", "★".repeat(n), "☆".repeat(5 - n))
}

/// 取当前登录账号的 handle。服务端没有 `/me/followers` 这类变体，
/// 「看自己的粉丝 / 自己收到的评价」只能先拿 handle 再查。
fn self_handle(cfg: &CliConfig, token: &str) -> Result<String> {
    let d = api::get(cfg, "/api/profile/me", Some(token))?;
    d.get("handle")
        .and_then(|v| v.as_str())
        .map(norm_handle)
        .filter(|h| !h.is_empty())
        .context("当前账号还没有 handle")
}

/* ---------------- 查看 ---------------- */

/// `ncc profile [show] [<username>]` —— 看自己（需登录）或某个用户名。
pub fn show(cfg: &CliConfig, target: Option<&str>) -> Result<()> {
    let catalog = api::get(cfg, "/api/profile/roles", None).unwrap_or(Value::Null);

    let (p, works, items, shares, social) = match target {
        Some(name) => {
            let name = name.trim_start_matches('@');
            let d = api::get(cfg, &format!("/api/profiles/{}", name), config::token_opt(cfg).as_deref())?;
            let p = d.get("profile").cloned().context("响应缺少 profile")?;
            (
                p,
                d.get("works").cloned().unwrap_or(json!([])),
                d.get("items").cloned().unwrap_or(json!([])),
                d.get("shares").cloned().unwrap_or(json!([])),
                d.get("social").cloned().unwrap_or(Value::Null),
            )
        }
        None => {
            let token = config::require_token(cfg)?;
            let d = api::get(cfg, "/api/profile/me", Some(&token))?;
            if d.get("profile").map(|v| v.is_null()).unwrap_or(true) {
                let who = d.get("handle").and_then(|v| v.as_str()).unwrap_or("@you");
                println!("你还没有名片（{who}）。用 `ncc profile set --headline \"…\" --roles fde` 创建。");
                return Ok(());
            }
            (
                d.get("profile").cloned().unwrap_or(Value::Null),
                d.get("works").cloned().unwrap_or(json!([])),
                json!([]),
                json!([]),
                d.get("social").cloned().unwrap_or(Value::Null),
            )
        }
    };

    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let username = s("username");
    let handle = s("handle");
    let vis = if s("visibility") == "unlisted" { "不列出" } else { "公开" };

    println!("{}  {}", handle, s("displayName"));
    if !username.is_empty() {
        println!("短链 {}", short_link(cfg, username));
    }
    if !s("headline").is_empty() {
        println!("定位 {}", s("headline"));
    }
    let mut meta: Vec<String> = Vec::new();
    if !s("location").is_empty() {
        meta.push(format!("所在地 {}", s("location")));
    }
    let avail = availability_label(s("availability"));
    if !avail.is_empty() {
        meta.push(avail.to_string());
    }
    meta.push(vis.to_string());
    if let Some(v) = p.get("views").and_then(|v| v.as_i64()) {
        meta.push(format!("{v} 次浏览"));
    }
    println!("{}", meta.join(" · "));

    // 社交面。注意 `social` 在响应**顶层**，不在 `profile` 里。
    if let Some(so) = social.as_object() {
        let num = |v: Option<&Value>| v.and_then(|x| x.as_i64()).unwrap_or(0);
        let mut line = format!(
            "粉丝 {} · 关注 {}",
            num(so.get("followers")),
            num(so.get("following"))
        );
        if let Some(r) = so.get("rating").and_then(|v| v.as_object()) {
            let cnt = num(r.get("count"));
            if cnt > 0 {
                let avg = r.get("avg").and_then(|v| v.as_f64()).unwrap_or(0.0);
                line.push_str(&format!(" · 评分 {avg:.1}（{cnt} 人）"));
            }
        }
        println!("{line}");
        if let Some(v) = so.get("viewer").and_then(|v| v.as_object()) {
            let viewer = |k: &str| v.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
            if viewer("canFollow") {
                println!("     关注 TA：ncc profile follow {}（单向，不放行任何数据）", norm_handle(handle));
            }
            if viewer("canRate") {
                println!("     给 TA 打分：ncc profile rate {} --score 1~5", norm_handle(handle));
            }
        }
    }

    let roles = arr(p.get("roles"));
    if !roles.is_empty() {
        let labels: Vec<String> = roles.iter().map(|r| role_label(&catalog, r)).collect();
        println!("定位角色 {}", labels.join(" / "));
    }
    let skills = arr(p.get("skills"));
    if !skills.is_empty() {
        println!("技能 {}", skills.join(", "));
    }
    if let Some(links) = p.get("links").and_then(|v| v.as_object()) {
        if !links.is_empty() {
            let l: Vec<String> = links.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect();
            println!("外链 {}", l.join("  "));
        }
    }
    if !s("bio").is_empty() {
        println!("简介 {}", s("bio"));
    }

    if let Some(ws) = works.as_array() {
        if !ws.is_empty() {
            println!("作品集 ({})", ws.len());
            for w in ws {
                let t = w.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let year = w.get("year").and_then(|v| v.as_str()).unwrap_or("");
                let href = w.get("href").and_then(|v| v.as_str()).unwrap_or("");
                let kind = w.get("linkKind").and_then(|v| v.as_str()).unwrap_or("");
                let prefix = if year.is_empty() { String::new() } else { format!("[{year}] ") };
                let suffix = if href.is_empty() { String::new() } else { format!("  → {href}") };
                let k = if kind == "link" { String::new() } else { format!(" ({kind})") };
                println!("  - {prefix}{t}{k}{suffix}");
            }
        }
    }
    if let Some(is) = items.as_array() {
        for i in is {
            let kind = i.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let ns = i.pointer("/namespace/slug").and_then(|v| v.as_str()).unwrap_or("");
            let slug = i.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            let ver = i.get("version").and_then(|v| v.as_str()).unwrap_or("");
            println!("能力 [{kind}] {ns}/{slug}@{ver}");
        }
    }
    if let Some(ss) = shares.as_array() {
        if !ss.is_empty() {
            println!("分享页 ({})", ss.len());
            for sh in ss {
                let t = sh.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let u = sh.get("url").and_then(|v| v.as_str()).unwrap_or("");
                println!("  - {t}  → {u}");
            }
        }
    }
    Ok(())
}

/* ---------------- 角色目录 ---------------- */

/// `ncc profile roles` —— 打印分组角色（--roles 的取值来源）。
pub fn roles(cfg: &CliConfig, group: Option<&str>) -> Result<()> {
    let d = api::get(cfg, "/api/profile/roles", None)?;
    let groups = d.get("groups").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let all = d.get("roles").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    for g in &groups {
        let gid = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(want) = group {
            if want != gid {
                continue;
            }
        }
        println!("{} ({})", g.get("zh").and_then(|v| v.as_str()).unwrap_or(gid), gid);
        for r in all.iter().filter(|r| r.get("group").and_then(|v| v.as_str()) == Some(gid)) {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let zh = r.get("zh").and_then(|v| v.as_str()).unwrap_or("");
            let desc = r.get("descZh").and_then(|v| v.as_str()).unwrap_or("");
            println!("  {id:<20} {zh}  — {desc}");
        }
    }
    println!("\n用法：ncc profile set --roles fde,agent-engineer（最多 5 个）");
    Ok(())
}

/* ---------------- 保存 ---------------- */

/// `ncc profile set …` —— 先读现状，只覆盖显式给出的字段。
pub fn set(cfg: &CliConfig, a: &SetArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    let cur = api::get(cfg, "/api/profile/me", Some(&token))?;
    let p = cur.get("profile").filter(|v| !v.is_null()).cloned();

    let cur_str = |k: &str| -> String {
        p.as_ref().and_then(|p| p.get(k)).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let pick = |arg: &Option<String>, k: &str| -> Value {
        match arg {
            Some(v) => json!(v),
            None => json!(cur_str(k)),
        }
    };

    // 用户名：没给就沿用现状（未创建名片时用个人命名空间 slug）
    let username = a
        .username
        .clone()
        .unwrap_or_else(|| cur.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string());

    // 外链：在现有基础上覆盖 --link 指定的键
    let mut links: Map<String, Value> = p
        .as_ref()
        .and_then(|p| p.get("links"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for kv in &a.links {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--link 需要 key=value 形式，收到：{kv}"))?;
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() {
            bail!("--link 的 key 不能为空");
        }
        if v.is_empty() {
            links.remove(k);
        } else {
            links.insert(k.to_string(), json!(v));
        }
    }

    let roles = match &a.roles {
        Some(s) => json!(csv(s)),
        None => p.as_ref().and_then(|p| p.get("roles")).cloned().unwrap_or(json!([])),
    };
    let skills = match &a.skills {
        Some(s) => json!(csv(s)),
        None => p.as_ref().and_then(|p| p.get("skills")).cloned().unwrap_or(json!([])),
    };

    let body = json!({
        "username": username,
        "displayName": pick(&a.display_name, "displayName"),
        "headline": pick(&a.headline, "headline"),
        "bio": pick(&a.bio, "bio"),
        "location": pick(&a.location, "location"),
        "roles": roles,
        "skills": skills,
        "links": Value::Object(links),
        "availability": pick(&a.availability, "availability"),
        "contactEmail": pick(&a.email, "contactEmail"),
        "visibility": pick(&a.visibility, "visibility"),
    });

    let d = api::request(cfg, "PUT", "/api/profile/me", Some(&token), Some(&body), None, &[])?;
    let q = d.get("profile").cloned().unwrap_or(Value::Null);
    let u = q.get("username").and_then(|v| v.as_str()).unwrap_or("");
    println!("✅ 名片已保存：{}", q.get("handle").and_then(|v| v.as_str()).unwrap_or(""));
    if !u.is_empty() {
        println!("   短链 {}", short_link(cfg, u));
    }
    if !a.links.is_empty() {
        println!("   提示：未列在 --link 里的外链保持不变；传 key= 可清除。");
    }
    Ok(())
}

/// `ncc profile username <name>` —— 只改用户名（短链与发布命名空间同步变更）。
pub fn set_username(cfg: &CliConfig, name: &str) -> Result<()> {
    let token = config::require_token(cfg)?;
    let cur = api::get(cfg, "/api/profile/me", Some(&token))?;
    let old = cur.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let a = SetArgs {
        username: Some(name.trim().trim_start_matches('@').to_string()),
        display_name: None,
        headline: None,
        bio: None,
        location: None,
        roles: None,
        skills: None,
        availability: None,
        visibility: None,
        email: None,
        links: Vec::new(),
    };
    set(cfg, &a)?;
    // 服务端会把用户名规范化为小写，提示里也用规范化后的值
    let new_name = a.username.as_deref().unwrap_or("").to_lowercase();
    if !old.is_empty() && old != new_name {
        println!("   ⚠ 旧的发布引用 @{old}/… 已失效，条目现挂在 @{new_name}/… 下。");
    }
    Ok(())
}

/* ---------------- 作品集 ---------------- */

pub fn work(cfg: &CliConfig, cmd: &WorkCmd) -> Result<()> {
    let token = config::require_token(cfg)?;
    match cmd {
        WorkCmd::List => {
            let d = api::get(cfg, "/api/profile/me", Some(&token))?;
            let ws = d.get("works").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if ws.is_empty() {
                println!("还没有作品。用 `ncc profile work add --title \"…\"` 添加。");
                return Ok(());
            }
            println!("共 {} 个作品：", ws.len());
            for w in &ws {
                let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let t = w.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let year = w.get("year").and_then(|v| v.as_str()).unwrap_or("");
                let href = w.get("href").and_then(|v| v.as_str()).unwrap_or("");
                let kind = w.get("linkKind").and_then(|v| v.as_str()).unwrap_or("");
                let prefix = if year.is_empty() { String::new() } else { format!("[{year}] ") };
                let arrow = if href.is_empty() { String::new() } else { format!("  → {href}") };
                println!("  {id}  {prefix}{t}  ({kind}){arrow}");
            }
            Ok(())
        }
        WorkCmd::Add(a) => {
            if a.share.is_some() && a.item.is_some() {
                bail!("--share 与 --item 只能二选一");
            }
            let (link_kind, link_ref) = if let Some(s) = &a.share {
                ("share", s.clone())
            } else if let Some(i) = &a.item {
                ("item", i.clone())
            } else {
                ("link", String::new())
            };
            let body = json!({
                "title": a.title,
                "summary": a.summary.clone().unwrap_or_default(),
                "role": a.role.clone().unwrap_or_default(),
                "tags": a.tags.as_deref().map(csv).unwrap_or_default(),
                "year": a.year.clone().unwrap_or_default(),
                "linkKind": link_kind,
                "linkRef": link_ref,
                "url": a.url.clone().unwrap_or_default(),
            });
            let d = api::post_json(cfg, "/api/profile/me/works", Some(&token), &body)?;
            let w = d.get("work").cloned().unwrap_or(Value::Null);
            println!(
                "✅ 已添加作品：{}  ({})",
                w.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                w.get("id").and_then(|v| v.as_str()).unwrap_or("")
            );
            if let Some(h) = w.get("href").and_then(|v| v.as_str()) {
                if !h.is_empty() {
                    println!("   → {h}");
                }
            }
            Ok(())
        }
        WorkCmd::Rm { id } => {
            api::del(cfg, &format!("/api/profile/me/works/{id}"), Some(&token))?;
            println!("✅ 已删除作品 {id}");
            Ok(())
        }
    }
}

/* ---------------- 关注 ---------------- */

/// 人脉行（粉丝 / 关注列表共用）。`note` 只有「看自己」时才会有值 ——
/// 服务端对他人一律置空，所以这里不作承诺、有就显示。
fn print_person(r: &Value) {
    let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let name = if s("displayName").is_empty() { s("name") } else { s("displayName") };

    let mut bits = vec![format!(
        "粉丝 {}",
        r.get("followers").and_then(|v| v.as_i64()).unwrap_or(0)
    )];
    if let Some(rt) = r.get("rating") {
        let cnt = rt.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        if cnt > 0 {
            let avg = rt.get("avg").and_then(|v| v.as_f64()).unwrap_or(0.0);
            bits.push(format!("{avg:.1} 分（{cnt} 人）"));
        }
    }
    println!("  {:<16} {:<12} {}", s("handle"), name, bits.join(" · "));
    if !s("headline").is_empty() {
        println!("       {}", s("headline"));
    }
    if !s("note").is_empty() {
        println!("       备注：{}", s("note"));
    }
}

/// `ncc profile follow <用户名> [--note "…"]` —— 单向、幂等。
pub fn follow(cfg: &CliConfig, a: &FollowArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    let h = norm_handle(&a.handle);
    // 备注只给关注方自己看。不传就**不发这个键**：服务端虽把空串当「不改动已有备注」，
    // 但少发一个键意图更清楚（也不会在将来服务端改语义时踩坑）。
    let body = match &a.note {
        Some(n) => json!({ "note": n }),
        None => json!({}),
    };
    let d = api::post_json(cfg, &format!("/api/profiles/{h}/follow"), Some(&token), &body)?;
    // handle 由服务端规范化，用返回的，别自己拼
    let shown = d
        .get("handle")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("@{h}"));
    println!("✅ 已关注 {shown}");
    let n = |k: &str| d.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    println!("   TA 的粉丝 {} · 你的关注 {}", n("followers"), n("myFollowing"));
    if a.note.is_some() {
        println!("   备注只给你自己看：`ncc profile following` 能看到。");
    }
    Ok(())
}

/// `ncc profile unfollow <用户名>` —— 幂等，没关注过也算成功。
pub fn unfollow(cfg: &CliConfig, handle: &str) -> Result<()> {
    let token = config::require_token(cfg)?;
    let h = norm_handle(handle);
    let d = api::del(cfg, &format!("/api/profiles/{h}/follow"), Some(&token))?;
    let shown = d
        .get("handle")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("@{h}"));
    println!("✅ 已取消关注 {shown}");
    if !d.get("removed").and_then(|v| v.as_bool()).unwrap_or(true) {
        println!("   （本来就没关注，无需改动）");
    }
    Ok(())
}

/// `ncc profile followers [<用户名>]` —— 谁关注了 TA，缺省是你。
pub fn followers(cfg: &CliConfig, handle: Option<&str>) -> Result<()> {
    let (h, token) = match handle {
        Some(x) => (norm_handle(x), config::token_opt(cfg)),
        None => {
            let t = config::require_token(cfg)?;
            (self_handle(cfg, &t)?, Some(t))
        }
    };
    let d = api::get(cfg, &format!("/api/profiles/{h}/followers"), token.as_deref())?;
    let rows = d.get("followers").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let total = d.get("total").and_then(|v| v.as_i64()).unwrap_or(rows.len() as i64);
    if rows.is_empty() {
        println!("还没有人关注 @{h}。");
        return Ok(());
    }
    println!("关注 @{h} 的人（{total}）：");
    for r in &rows {
        print_person(r);
    }
    Ok(())
}

/// `ncc profile following [<用户名>]` —— TA 关注的人，缺省是你。
///
/// 看自己时走 `/api/profile/me/following`：那是唯一会带出**私密备注**的接口
/// （`/api/profiles/<handle>/following` 只在「你 == 名片主人」时保留 note，
/// 而且不建名片也能用）。
pub fn following(cfg: &CliConfig, handle: Option<&str>) -> Result<()> {
    let (path, token, who) = match handle {
        Some(x) => {
            let h = norm_handle(x);
            (
                format!("/api/profiles/{h}/following"),
                config::token_opt(cfg),
                format!("@{h}"),
            )
        }
        None => {
            let t = config::require_token(cfg)?;
            ("/api/profile/me/following".to_string(), Some(t), "你".to_string())
        }
    };
    let d = api::get(cfg, &path, token.as_deref())?;
    let rows = d.get("following").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let total = d.get("total").and_then(|v| v.as_i64()).unwrap_or(rows.len() as i64);
    if rows.is_empty() {
        println!("{who}还没有关注任何人。用 `ncc profile follow <用户名>` 关注一个。");
        return Ok(());
    }
    println!("{who}关注的人（{total}）：");
    for r in &rows {
        print_person(r);
    }
    Ok(())
}

/* ---------------- 评价 ---------------- */

/// `ncc profile rate <用户名> --score 1~5 [--note "…"]` —— 一人一条，再打是改分。
pub fn rate(cfg: &CliConfig, a: &RateArgs) -> Result<()> {
    // 本地就拦：越界不该发请求等服务端 400
    if !(1..=5).contains(&a.score) {
        bail!("--score 必须在 1~5 之间，收到 {}", a.score);
    }
    let token = config::require_token(cfg)?;
    let h = norm_handle(&a.handle);
    // 与服务端的「整行覆盖」一致：不带 --note 就是**清空**已有评语
    // （与关注的备注相反：那边空串是不改动）
    let body = json!({ "score": a.score, "note": a.note.clone().unwrap_or_default() });
    let d = api::request(
        cfg,
        "PUT",
        &format!("/api/profiles/{h}/rating"),
        Some(&token),
        Some(&body),
        None,
        &[],
    )?;
    let created = d.get("created").and_then(|v| v.as_bool()).unwrap_or(false);
    let sum = d.get("summary");
    let avg = sum.and_then(|v| v.get("avg")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cnt = sum.and_then(|v| v.get("count")).and_then(|v| v.as_i64()).unwrap_or(0);
    println!("✅ 已评价 @{h} {} {avg:.1} 分（{cnt} 人）", stars(a.score));
    if created {
        println!("   首次评价。");
    } else {
        println!("   已更新 —— 一人一条，重复提交是覆盖，不是加一票。");
        if a.note.is_none() {
            println!("   ⚠ 这次没带 --note，原有评语已被清空（评语是公开的）。");
        }
    }
    Ok(())
}

/// `ncc profile unrate <用户名>` —— 撤销自己的评价，幂等。
pub fn unrate(cfg: &CliConfig, handle: &str) -> Result<()> {
    let token = config::require_token(cfg)?;
    let h = norm_handle(handle);
    let d = api::del(cfg, &format!("/api/profiles/{h}/rating"), Some(&token))?;
    println!("✅ 已撤销对 @{h} 的评价");
    if !d.get("removed").and_then(|v| v.as_bool()).unwrap_or(true) {
        println!("   （本来就没评价过，无需改动）");
    }
    Ok(())
}

/// `ncc profile ratings [<用户名>]` —— TA 收到的评价，缺省是你自己收到的。
pub fn ratings(cfg: &CliConfig, handle: Option<&str>) -> Result<()> {
    let (h, token) = match handle {
        Some(x) => (norm_handle(x), config::token_opt(cfg)),
        None => {
            let t = config::require_token(cfg)?;
            (self_handle(cfg, &t)?, Some(t))
        }
    };
    let d = api::get(cfg, &format!("/api/profiles/{h}/ratings"), token.as_deref())?;
    let rows = d.get("ratings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let sum = d.get("summary");
    let avg = sum.and_then(|v| v.get("avg")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cnt = sum.and_then(|v| v.get("count")).and_then(|v| v.as_i64()).unwrap_or(0);
    if rows.is_empty() {
        println!("@{h} 还没有收到评价。用 `ncc profile rate {h} --score 1~5` 给一个。");
        return Ok(());
    }
    // 均分直接用服务端的（它已 round1），不要自己算 —— 「5.0 分」这个写法就是它
    println!("@{h} 的评价：{avg:.1} 分（{cnt} 人）");
    if let Some(dist) = sum.and_then(|v| v.get("dist")).and_then(|v| v.as_array()) {
        let bars: Vec<String> = dist
            .iter()
            .enumerate()
            .filter_map(|(i, v)| match v.as_i64().unwrap_or(0) {
                0 => None,
                n => Some(format!("{}星 {n}", i + 1)),
            })
            .collect();
        if !bars.is_empty() {
            println!("   分布 {}", bars.join(" · "));
        }
    }
    if let Some(mine) = d.get("mine").and_then(|v| v.as_i64()) {
        if mine > 0 {
            println!("   你给的是 {mine} 星");
        }
    }
    for r in &rows {
        let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let name = if s("displayName").is_empty() { s("name") } else { s("displayName") };
        let score = r.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
        println!("  {} {:<10} {}", stars(score), name, s("note"));
    }
    Ok(())
}
