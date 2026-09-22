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

/* ---------------- 查看 ---------------- */

/// `ncc profile [show] [<username>]` —— 看自己（需登录）或某个用户名。
pub fn show(cfg: &CliConfig, target: Option<&str>) -> Result<()> {
    let catalog = api::get(cfg, "/api/profile/roles", None).unwrap_or(Value::Null);

    let (p, works, items, shares) = match target {
        Some(name) => {
            let name = name.trim_start_matches('@');
            let d = api::get(cfg, &format!("/api/profiles/{}", name), config::token_opt(cfg).as_deref())?;
            let p = d.get("profile").cloned().context("响应缺少 profile")?;
            (
                p,
                d.get("works").cloned().unwrap_or(json!([])),
                d.get("items").cloned().unwrap_or(json!([])),
                d.get("shares").cloned().unwrap_or(json!([])),
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
