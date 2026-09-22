// 节点治理与分享 —— `ncc registry admin …` / `ncc registry share …`
//
// 两个能力，都挂在「一台内网 registry 的本地治理」这条线上：
//
//	admin   节点管理员管**用户 / 节点 / 服务**（禁用启停、重置密码、摘除、归档），
//	        每个动作都写审计。两种凭据等价：
//	          人   本节点管理员账号（第一个注册的用户）+ `ncc registry login`
//	          机器 admin key/secret（AK-… + secret）—— `ncc registry admin login` 写进本机配置
//	share   把一条制品变成**临时下载地址**发出去：对方不用登录、不用装 CLI。
//	        链接形如 <base>/s/<token>（说明页）与 /s/<token>/raw（直链，只有它计数）。
//
// 边界（与服务端一致，别在这里加"聪明"的推断）：
//   - 管理面（admin）看的是**整个节点**，不是「我的资产」；
//   - 分享是临时放行，不是授权：想给对方长期权限用 `ncc grant`；
//   - 分享只能由「本来就能读这条制品」的人创建，否则它就是提权通道。
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Result};
use serde_json::{json, Value};

/* ---------------- 参数：admin ---------------- */

#[derive(clap::Args)]
pub struct AdminArgs {
    /// admin key（缺省用本机保存的，或环境变量 NCC_ADMIN_KEY）
    #[arg(long, global = true)]
    pub key: Option<String>,
    /// admin secret（缺省用本机保存的，或环境变量 NCC_ADMIN_SECRET）
    #[arg(long, global = true)]
    pub secret: Option<String>,
    #[command(subcommand)]
    pub action: Option<AdminAction>,
}

#[derive(clap::Subcommand)]
pub enum AdminAction {
    /// 把 admin key/secret 写进本机配置（之后 admin 命令不用再带）
    Login {
        #[arg(long)]
        key: String,
        #[arg(long)]
        secret: String,
    },
    /// 清除本机保存的 admin 凭据
    Logout,
    /// 我是不是管理员 / 本机凭据能不能用
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 节点概览：用户 / 节点 / 服务 / 制品 / 配置 / 分享 / 审计 计数
    Overview {
        #[arg(long)]
        json: bool,
    },
    /// 用户列表（含被禁用的）
    Users {
        #[arg(long)]
        q: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// 禁用账号（--note 记原因，会带回给本人）
    Disable {
        /// 用户 id（U-…）或邮箱
        user: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 启用账号
    Enable {
        /// 用户 id（U-…）或邮箱
        user: String,
        #[arg(long)]
        json: bool,
    },
    /// 重置密码（不给 --password 则由服务端生成，只显示一次）
    Passwd {
        /// 用户 id（U-…）或邮箱
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 全部托管节点（含私有与离线）
    Nodes {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        region: Option<String>,
        #[arg(long)]
        q: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// 摘除一个托管节点（不论归属）
    RmNode {
        /// 节点 id（ND-…）
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// 服务一览：节点侧 kind=service + 制品侧 kind=api
    Services {
        /// all（默认）| node | artifact
        #[arg(long, default_value = "all", value_parser = ["all", "node", "artifact"])]
        source: String,
        #[arg(long)]
        q: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// 处理一条服务：节点侧摘除 / 制品侧归档（ND-… 或 @命名空间/slug）
    RmService {
        #[arg(value_name = "ND-…|@ns/slug")]
        r#ref: String,
        #[arg(long)]
        json: bool,
    },
    /// 审计日志（谁在什么时候把谁怎么了）
    Audit {
        #[arg(long)]
        action: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// 机器管理凭据（只有 key 前缀，secret 不可见）
    Keys {
        #[arg(long)]
        json: bool,
    },
    /// 轮换 admin key/secret（返回新的 secret，旧的立即失效）
    Rotate {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/* ---------------- 参数：share ---------------- */

#[derive(clap::Args)]
pub struct ShareArgs {
    #[command(subcommand)]
    pub action: Option<ShareAction>,
}

#[derive(clap::Subcommand)]
pub enum ShareAction {
    /// 建一条分享链接（对方不用登录）
    Create {
        /// 制品引用：@命名空间/slug 或 A-… id
        r#ref: String,
        /// 备注（给谁用、做什么）
        #[arg(long)]
        label: Option<String>,
        /// 限次（缺省不限）
        #[arg(long)]
        uses: Option<i64>,
        /// 有效天数（缺省不过期）
        #[arg(long)]
        expires: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    /// 我的分享链接（--all 看全部，需管理员）
    List {
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// 撤销一条分享（立即失效）
    Rm {
        /// 分享 id（SH-…）或 token / 链接
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// 看一条链接的状态（公开，不消耗次数）
    Info {
        /// 分享 token 或完整链接
        target: String,
        #[arg(long)]
        json: bool,
    },
}

/* ---------------- 工具 ---------------- */

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn si(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn dt(v: &Value, k: &str) -> String {
    match v.get(k).and_then(|x| x.as_str()) {
        Some(t) if t.len() >= 16 => t[..16].replace('T', " "),
        Some(t) => t.to_string(),
        None => "-".into(),
    }
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

/// 空值不输出括号（`（）` 看起来像 bug）。
fn paren(x: &str) -> String {
    if x.trim().is_empty() {
        String::new()
    } else {
        format!("（{x}）")
    }
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// 本机（或本次命令）可用的 admin 凭据：参数 > 环境变量 > **当前目标**保存的。
///
/// 凭据属于目标：在 office 节点上的管理凭据不该跟着你切到 lab 或云端去。
fn creds(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>) -> (Option<String>, Option<String>) {
    let (ck, cs) = config::admin_creds(cfg);
    let k = key
        .clone()
        .filter(|x| !x.trim().is_empty())
        .or_else(|| env_opt("NCC_ADMIN_KEY"))
        .or_else(|| ck);
    let s = secret
        .clone()
        .filter(|x| !x.trim().is_empty())
        .or_else(|| env_opt("NCC_ADMIN_SECRET"))
        .or_else(|| cs);
    (k, s)
}

/// 管理面请求：带 admin key/secret 就用它；否则用登录会话（管理员账号也要能管）。
fn admin_req(
    cfg: &CliConfig,
    method: &str,
    path: &str,
    key: &Option<String>,
    secret: &Option<String>,
    body: Option<&Value>,
) -> Result<Value> {
    let (k, sec) = creds(cfg, key, secret);
    let mut headers: Vec<(&str, &str)> = Vec::new();
    let mut token: Option<String> = None;
    match (&k, &sec) {
        (Some(kk), Some(ss)) => {
            headers.push(("X-NCC-Admin-Key", kk.as_str()));
            headers.push(("X-NCC-Admin-Secret", ss.as_str()));
        }
        (Some(_), None) => bail!("只给了 admin key，缺少 secret（用 --secret 或 ncc registry admin login）"),
        (None, Some(_)) => bail!("只给了 admin secret，缺少 key（用 --key 或 ncc registry admin login）"),
        (None, None) => {
            token = config::token_opt(cfg);
            if token.is_none() {
                bail!(
                    "需要节点管理员身份：\n  ① 管理员账号登录：ncc registry login --email <管理员邮箱> --password …\n  ② 或用机器凭据：ncc registry admin login --key AK-… --secret …"
                );
            }
        }
    }
    let out = api::request(
        cfg,
        method,
        path,
        token.as_deref(),
        body,
        None,
        &headers,
    )?;
    Ok(out)
}

fn resolve_user(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, who: &str) -> Result<Value> {
    // 邮箱 → 先查一次用户列表换 id（内网规模，一次查完够用）。
    if who.contains('@') && !who.starts_with("U-") {
        let d = admin_req(cfg, "GET", &format!("/api/admin/users?q={}", api::urlenc(who)), key, secret, None)?;
        let hit = d
            .get("users")
            .and_then(|u| u.as_array())
            .and_then(|arr| arr.iter().find(|u| s(u, "email") == who))
            .cloned();
        return match hit {
            Some(u) => Ok(u),
            None => bail!("找不到邮箱 {who} 对应的账号（用 ncc registry admin users 看一眼）"),
        };
    }
    Ok(json!({ "id": who }))
}

/* ---------------- admin 命令 ---------------- */

pub fn run(cfg: &mut CliConfig, a: &AdminArgs) -> Result<()> {
    let action = match &a.action {
        Some(x) => x,
        None => {
            status(cfg, &a.key, &a.secret, false)?;
            return Ok(());
        }
    };
    match action {
        AdminAction::Login { key, secret } => login(cfg, key, secret),
        AdminAction::Logout => logout(cfg),
        AdminAction::Status { json: j } => status(cfg, &a.key, &a.secret, *j),
        AdminAction::Overview { json: j } => overview(cfg, &a.key, &a.secret, *j),
        AdminAction::Users { q, limit, json: j } => users(cfg, &a.key, &a.secret, q.as_deref(), *limit, *j),
        AdminAction::Disable { user, note, json: j } => disable(cfg, &a.key, &a.secret, user, note.as_deref(), *j),
        AdminAction::Enable { user, json: j } => enable(cfg, &a.key, &a.secret, user, *j),
        AdminAction::Passwd { user, password, json: j } => passwd(cfg, &a.key, &a.secret, user, password.as_deref(), *j),
        AdminAction::Nodes { kind, region, q, limit, json: j } => nodes(
            cfg,
            &a.key,
            &a.secret,
            kind.as_deref(),
            region.as_deref(),
            q.as_deref(),
            *limit,
            *j,
        ),
        AdminAction::RmNode { id, json: j } => rm_node(cfg, &a.key, &a.secret, id, *j),
        AdminAction::Services { source, q, limit, json: j } => {
            services(cfg, &a.key, &a.secret, source, q.as_deref(), *limit, *j)
        }
        AdminAction::RmService { r#ref, json: j } => rm_service(cfg, &a.key, &a.secret, r#ref, *j),
        AdminAction::Audit { action, limit, json: j } => audit(cfg, &a.key, &a.secret, action.as_deref(), *limit, *j),
        AdminAction::Keys { json: j } => keys(cfg, &a.key, &a.secret, *j),
        AdminAction::Rotate { label, json: j } => rotate(cfg, &a.key, &a.secret, label.as_deref(), *j),
    }
}

fn login(cfg: &mut CliConfig, key: &str, secret: &str) -> Result<()> {
    if !key.starts_with("AK-") {
        bail!("admin key 应该形如 AK-XXXXXX（见注册首个账号时的输出，或 `ncc registry admin rotate`）");
    }
    // 先验证一次：写进配置之前确认这对凭据真能用。
    let mut c = cfg.clone();
    config::set_admin_creds(&mut c, key, secret)?;
    let d = admin_req(&c, "GET", "/api/admin/overview", &None, &None, None)?;
    config::set_admin_creds(cfg, key, secret)?;
    let node = d.get("node").cloned().unwrap_or(json!({}));
    println!(
        "✅ 已保存 admin 凭据到目标 {}（{}）",
        cfg.current_name(),
        config::config_path().display()
    );
    println!("   节点   {} · {}", s(&node, "nodeName"), s(&node, "base"));
    println!("   凭据   {}{}", key, paren(s(&d, "credential.name")));
    println!("\n接下来：ncc registry admin overview | users | nodes | services | audit");
    Ok(())
}

fn logout(cfg: &mut CliConfig) -> Result<()> {
    let name = cfg.current_name();
    config::clear_admin_creds(cfg)?;
    println!("✅ 已清除目标 {name} 上保存的 admin 凭据（服务端的凭据不受影响）");
    Ok(())
}

fn status(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, json_out: bool) -> Result<()> {
    let (k, sec) = creds(cfg, key, secret);
    let has_local = k.is_some() && sec.is_some();
    // 会话身份：/api/auth/me 会告诉我们这个账号是不是管理员。
    let me = match config::token_opt(cfg) {
        Some(t) => api::get(cfg, "/api/auth/me", Some(&t)).ok(),
        None => None,
    };
    let is_admin_user = me.as_ref().map(|m| b(&m["admin"], "isAdmin")).unwrap_or(false);
    // 机器凭据：能用才叫"有"。
    let mut key_ok = false;
    if has_local {
        key_ok = admin_req(cfg, "GET", "/api/admin/overview", key, secret, None).is_ok();
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "base": cfg.base_url(),
                "adminKey": k.clone().map(|x| x).unwrap_or_default(),
                "adminKeyUsable": key_ok,
                "isAdminUser": is_admin_user,
                "email": me.as_ref().map(|m| s(&m["user"], "email").to_string()).unwrap_or_default(),
            }))?
        );
        return Ok(());
    }
    println!("节点   {}", cfg.base_url());
    match me.as_ref() {
        Some(m) => println!(
            "账号   {} {}",
            s(&m["user"], "email"),
            if is_admin_user { "· 管理员" } else { "·（非管理员）" }
        ),
        None => println!("账号   未登录（ncc registry login）"),
    }
    match &k {
        Some(kk) if key_ok => println!("凭据   {kk} · 可用"),
        Some(kk) => println!("凭据   {kk} · 不可用（已被轮换或撤销？用 ncc registry admin login 换一份）"),
        None => println!("凭据   无（机器凭据：ncc registry admin login --key AK-… --secret …）"),
    }
    if !is_admin_user && !key_ok {
        println!("\n这个身份现在管不了本节点。两种办法：");
        println!("  ① 用本节点管理员账号登录：ncc registry login --email <管理员邮箱> --password …");
        println!("  ② 拿一份机器凭据：ncc registry admin login --key AK-… --secret …");
    }
    Ok(())
}

fn overview(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, json_out: bool) -> Result<()> {
    let d = admin_req(cfg, "GET", "/api/admin/overview", key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let node = &d["node"];
    let c = &d["counts"];
    println!("节点   {} · {}{}", s(node, "nodeName"), s(node, "base"), paren(s(node, "region")));
    println!(
        "用户   {}（管理员 {}） · 节点 {}（service {} / agent {} / assigned {}）",
        si(c, "users"),
        si(c, "admins"),
        si(c, "nodes"),
        si(&c["nodeKinds"], "service"),
        si(&c["nodeKinds"], "agent"),
        si(&c["nodeKinds"], "assigned"),
    );
    println!(
        "服务   {} 条（节点侧 {} · 制品侧 {}）",
        si(&c["services"], "hostedNodes") + si(&c["services"], "artifacts"),
        si(&c["services"], "hostedNodes"),
        si(&c["services"], "artifacts"),
    );
    println!(
        "资产   制品 {} · 配置 {} · 分享 {}（有效 {}） · 审计 {}",
        si(c, "artifacts"),
        si(c, "configs"),
        si(c, "shares"),
        si(c, "activeShares"),
        si(c, "auditActions"),
    );
    println!("凭据   {}{}", s(&d["credential"], "kind"), paren(s(&d["credential"], "name")));
    println!("\n明细：ncc registry admin users | nodes | services | audit");
    Ok(())
}

fn users(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    q: Option<&str>,
    limit: u32,
    json_out: bool,
) -> Result<()> {
    let mut path = format!("/api/admin/users?limit={limit}");
    if let Some(q) = q {
        path.push_str(&format!("&q={}", api::urlenc(q)));
    }
    let d = admin_req(cfg, "GET", &path, key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let empty = vec![];
    let list = d.get("users").and_then(|x| x.as_array()).unwrap_or(&empty);
    println!("用户 {} / {}", list.len(), si(&d, "total"));
    for u in list {
        let role = if b(u, "isAdmin") { "管理员" } else { "普通" };
        let state = if b(u, "disabled") {
            let note = s(u, "adminNote");
            if note.is_empty() {
                "已禁用".to_string()
            } else {
                format!("已禁用（{note}）")
            }
        } else {
            "正常".to_string()
        };
        println!(
            "  {}  ·  {}  ·  {}  ·  {}  ·  节点 {} / 制品 {}  ·  最近登录 {}",
            s(u, "email"),
            s(u, "name"),
            role,
            state,
            si(&u["stats"], "nodes"),
            si(&u["stats"], "artifacts"),
            dt(u, "lastLoginAt"),
        );
        println!("    id {}", s(u, "id"));
    }
    println!("\n禁用/启用：ncc registry admin disable <邮箱|id> --note \"原因\"｜admin enable <邮箱|id>");
    println!("重置密码：  ncc registry admin passwd <邮箱|id>");
    Ok(())
}

fn set_disabled(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    who: &str,
    disabled: bool,
    note: Option<&str>,
    json_out: bool,
) -> Result<()> {
    let u = resolve_user(cfg, key, secret, who)?;
    let id = s(&u, "id").to_string();
    let mut body = json!({ "disabled": disabled });
    if let Some(n) = note {
        body["adminNote"] = json!(n);
    }
    let d = admin_req(cfg, "PATCH", &format!("/api/admin/users/{id}"), key, secret, Some(&body))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let user = &d["user"];
    println!(
        "{} 已{} {}（{}）",
        if disabled { "🚫" } else { "✅" },
        if disabled { "禁用" } else { "启用" },
        s(user, "email"),
        s(user, "id")
    );
    if disabled {
        println!("   备注   {}", if note.unwrap_or("").is_empty() { "-" } else { note.unwrap_or("") });
        println!("   说明   旧令牌立即失效，本人登录会看到禁用原因");
    }
    Ok(())
}

fn disable(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    who: &str,
    note: Option<&str>,
    json_out: bool,
) -> Result<()> {
    set_disabled(cfg, key, secret, who, true, note, json_out)
}

fn enable(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, who: &str, json_out: bool) -> Result<()> {
    set_disabled(cfg, key, secret, who, false, None, json_out)
}

fn passwd(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    who: &str,
    password: Option<&str>,
    json_out: bool,
) -> Result<()> {
    let u = resolve_user(cfg, key, secret, who)?;
    let id = s(&u, "id").to_string();
    let mut body = json!({});
    if let Some(p) = password {
        body["password"] = json!(p);
    }
    let d = admin_req(cfg, "POST", &format!("/api/admin/users/{id}/password"), key, secret, Some(&body))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("🔑 已重置 {} 的密码", s(&d, "email"));
    println!("   新密码  {}", s(&d, "password"));
    if b(&d, "generated") {
        println!("   （由服务端生成，只显示这一次：请立刻告知本人，并让其自行修改）");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn nodes(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    kind: Option<&str>,
    region: Option<&str>,
    q: Option<&str>,
    limit: u32,
    json_out: bool,
) -> Result<()> {
    let mut path = format!("/api/admin/nodes?limit={limit}");
    if let Some(k) = kind {
        path.push_str(&format!("&kind={}", api::urlenc(k)));
    }
    if let Some(r) = region {
        path.push_str(&format!("&region={}", api::urlenc(r)));
    }
    if let Some(q) = q {
        path.push_str(&format!("&q={}", api::urlenc(q)));
    }
    let d = admin_req(cfg, "GET", &path, key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let empty = vec![];
    let list = d.get("nodes").and_then(|x| x.as_array()).unwrap_or(&empty);
    println!("托管节点 {} / {}", list.len(), si(&d, "total"));
    for n in list {
        let status = if b(n, "online") { "在线" } else { "离线" };
        println!(
            "  {}  ·  {}  ·  {}  ·  {}  ·  {}  ·  归属 {}",
            s(n, "name"),
            s(n, "kind"),
            status,
            s(n, "region"),
            s(n, "visibility"),
            s(n, "ownerEmail"),
        );
        println!(
            "    id {}  ·  命名空间 @{}  ·  最后心跳 {}",
            s(n, "id"),
            s(&n["namespace"], "slug"),
            dt(n, "lastSeen"),
        );
    }
    println!("\n摘除：ncc registry admin rm-node <ND-…>");
    Ok(())
}

fn rm_node(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, id: &str, json_out: bool) -> Result<()> {
    let d = admin_req(cfg, "DELETE", &format!("/api/admin/nodes/{id}"), key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("✅ 已摘除节点 {id}（指向它的连接记录一并清理）");
    Ok(())
}

fn services(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    source: &str,
    q: Option<&str>,
    limit: u32,
    json_out: bool,
) -> Result<()> {
    let mut path = format!("/api/admin/services?source={source}&limit={limit}");
    if let Some(q) = q {
        path.push_str(&format!("&q={}", api::urlenc(q)));
    }
    let d = admin_req(cfg, "GET", &path, key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let total = &d["total"];
    println!(
        "服务 {} 条（节点侧 {} · 制品侧 {}）",
        si(total, "all"),
        si(total, "nodeServices"),
        si(total, "apiArtifacts"),
    );
    let nempty = vec![];
    let nlist = d.get("nodeServices").and_then(|x| x.as_array()).unwrap_or(&nempty);
    if !nlist.is_empty() {
        println!("\n节点侧（kind=service，正在跑的服务）");
        for n in nlist {
            println!(
                "  {}  ·  {}  ·  {}  ·  归属 {}",
                s(n, "name"),
                s(n, "kind"),
                if b(n, "online") { "在线" } else { "离线" },
                s(n, "ownerEmail"),
            );
            println!("    id {}  ·  命名空间 @{}", s(n, "id"), s(&n["namespace"], "slug"));
        }
    }
    let aempty = vec![];
    let alist = d.get("apiArtifacts").and_then(|x| x.as_array()).unwrap_or(&aempty);
    if !alist.is_empty() {
        println!("\n制品侧（kind=api，声明/交付的服务接口）");
        for it in alist {
            println!(
                "  {}  ·  {}  ·  {}  ·  {}",
                s(it, "name"),
                s(it, "kind"),
                s(it, "status"),
                s(it, "ref"),
            );
            println!("    id {}  ·  命名空间 @{}", s(it, "id"), s(&it["namespace"], "slug"));
        }
    }
    println!("\n处理：ncc registry admin rm-service <ND-…|@命名空间/slug>（节点摘除 / 制品归档）");
    Ok(())
}

fn rm_service(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    r#ref: &str,
    json_out: bool,
) -> Result<()> {
    // @ns/slug 里的斜杠是路由分隔符：原样拼进去，别编码（编码后服务端只看到一个段）。
    let path = format!("/api/admin/services/{}", r#ref);
    let d = admin_req(cfg, "DELETE", &path, key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    match s(&d, "target") {
        "node" => println!("✅ 已摘除服务节点 {}（{}）", r#ref, s(&d, "id")),
        "artifact" => println!("✅ 已归档服务条目 {}（{} → archived，字节保留）", r#ref, s(&d, "id")),
        _ => println!("✅ 已处理 {}", r#ref),
    }
    Ok(())
}

fn audit(
    cfg: &CliConfig,
    key: &Option<String>,
    secret: &Option<String>,
    action: Option<&str>,
    limit: u32,
    json_out: bool,
) -> Result<()> {
    let mut path = format!("/api/admin/audit?limit={limit}");
    if let Some(a) = action {
        path.push_str(&format!("&action={}", api::urlenc(a)));
    }
    let d = admin_req(cfg, "GET", &path, key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let empty = vec![];
    let list = d.get("audit").and_then(|x| x.as_array()).unwrap_or(&empty);
    println!("审计 {} / {}", list.len(), si(&d, "total"));
    for a in list {
        println!(
            "  {}  ·  {}  ·  {}  ·  {}",
            dt(a, "createdAt"),
            s(a, "action"),
            s(&a["actor"], "name"),
            s(a, "summary"),
        );
        if !s(a, "targetName").is_empty() {
            println!("    → {}（{}）  ip {}", s(a, "targetName"), s(a, "target"), s(a, "ip"));
        }
    }
    Ok(())
}

fn keys(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, json_out: bool) -> Result<()> {
    let d = admin_req(cfg, "GET", "/api/admin/keys", key, secret, None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let empty = vec![];
    let list = d.get("keys").and_then(|x| x.as_array()).unwrap_or(&empty);
    println!("机器管理凭据 {}", si(&d, "total"));
    for k in list {
        println!(
            "  {}  ·  {}  ·  {}  ·  最近使用 {}",
            s(k, "key"),
            if s(k, "label").is_empty() { "-" } else { s(k, "label") },
            if b(k, "active") { "可用" } else { "已撤销" },
            dt(k, "lastUsedAt"),
        );
    }
    println!("\n轮换：ncc registry admin rotate --label ops（返回新 secret，旧的立即失效）");
    Ok(())
}

fn rotate(cfg: &CliConfig, key: &Option<String>, secret: &Option<String>, label: Option<&str>, json_out: bool) -> Result<()> {
    let mut path = String::from("/api/admin/keys/rotate");
    if let Some(l) = label {
        path.push_str(&format!("?label={}", api::urlenc(l)));
    }
    let d = admin_req(cfg, "POST", &path, key, secret, Some(&json!({})))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let k = &d["key"];
    println!("🔑 已轮换节点管理凭据（撤销旧凭据 {} 份）", si(&d, "revoked"));
    println!("   key      {}", s(k, "key"));
    println!("   secret   {}", s(&d, "secret"));
    println!("\nsecret 只显示这一次。写进本机配置：");
    println!("  ncc registry admin login --key {} --secret <上面的 secret>", s(k, "key"));
    Ok(())
}

/* ---------------- share 命令 ---------------- */

pub fn run_share(cfg: &CliConfig, a: &ShareArgs) -> Result<()> {
    match &a.action {
        Some(ShareAction::Create { r#ref, label, uses, expires, json: j }) => {
            share_create(cfg, r#ref, label.as_deref(), *uses, *expires, *j)
        }
        Some(ShareAction::List { all, limit, json: j }) => share_list(cfg, *all, *limit, *j),
        Some(ShareAction::Rm { id, json: j }) => share_rm(cfg, id, *j),
        Some(ShareAction::Info { target, json: j }) => share_info(cfg, target, *j),
        None => share_list(cfg, false, 50, false),
    }
}

fn normalize_token(target: &str) -> String {
    // 允许直接粘链接（/s/<token> 或 /s/<token>/raw），从里面抠出 token。
    let t = target.trim().trim_end_matches('/');
    if let Some(pos) = t.rfind("/s/") {
        return t[pos + 3..].trim_end_matches("/raw").to_string();
    }
    t.to_string()
}

fn share_create(
    cfg: &CliConfig,
    r#ref: &str,
    label: Option<&str>,
    uses: Option<i64>,
    expires: Option<i64>,
    json_out: bool,
) -> Result<()> {
    let t = config::require_token(cfg)?;
    let mut body = json!({ "ref": r#ref });
    if let Some(l) = label {
        body["label"] = json!(l);
    }
    if let Some(u) = uses {
        body["uses"] = json!(u);
    }
    if let Some(e) = expires {
        body["expiresInDays"] = json!(e);
    }
    let d = api::post_json(cfg, "/api/shares", Some(&t), &body)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let sh = &d["share"];
    let art = &sh["artifact"];
    println!("✅ 已创建分享链接：{}", s(art, "ref"));
    println!("   说明页  {}", s(&d, "link"));
    println!("   直链    {}", s(&d, "rawLink"));
    let limits = format!(
        "{} · {}",
        if si(&sh["uses"], "max") > 0 {
            format!("限 {} 次", si(&sh["uses"], "max"))
        } else {
            "不限次".to_string()
        },
        if s(sh, "expiresAt").is_empty() { "不过期".to_string() } else { format!("到期 {}", dt(sh, "expiresAt")) },
    );
    println!("   限制    {limits}");
    println!("   分享 id {}", s(sh, "id"));
    println!("\n给对方：把说明页链接发过去即可（不用登录、不用装 CLI）");
    println!("给 Agent：把直链交给它，curl -OJ 就能拿到字节");
    println!("撤销：   ncc registry share rm {}", s(sh, "id"));
    println!("长期授权请用 ncc grant（分享是临时放行，对方拿到字节即结束）");
    Ok(())
}

fn share_list(cfg: &CliConfig, all: bool, limit: u32, json_out: bool) -> Result<()> {
    let mut path = format!("/api/shares?limit={limit}");
    if all {
        path.push_str("&all=1");
    }
    // 看全部需要管理员；这时优先用 admin 凭据（配置里有就用），否则用登录会话。
    let d = if all {
        admin_req(cfg, "GET", &path, &None, &None, None)?
    } else {
        let t = config::require_token(cfg)?;
        api::get(cfg, &path, Some(&t))?
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let empty = vec![];
    let list = d.get("shares").and_then(|x| x.as_array()).unwrap_or(&empty);
    println!("分享链接 {} / {}{}", list.len(), si(&d, "total"), if all { "（全部）" } else { "（我的）" });
    for sh in list {
        let art = &sh["artifact"];
        let uses = if si(&sh["uses"], "max") > 0 {
            format!("{}/{} 次", si(&sh["uses"], "used"), si(&sh["uses"], "max"))
        } else {
            format!("{} 次（不限）", si(&sh["uses"], "used"))
        };
        println!(
            "  {}  ·  {}  ·  {}  ·  {}  ·  {}",
            s(sh, "id"),
            s(art, "ref"),
            if b(sh, "usable") { "有效" } else { "已失效" },
            uses,
            if s(sh, "expiresAt").is_empty() { "不过期".to_string() } else { format!("到期 {}", dt(sh, "expiresAt")) },
        );
        if all {
            println!("    创建者 {}  ·  hint {}", s(&sh["createdBy"], "name"), s(sh, "hint"));
        }
    }
    Ok(())
}

fn share_rm(cfg: &CliConfig, id: &str, json_out: bool) -> Result<()> {
    // 允许直接粘链接或 token：先查一次拿到分享 id（公开接口，不消耗次数）。
    let mut target = id.trim().to_string();
    if !target.starts_with("SH-") {
        let token = normalize_token(&target);
        let info = api::get(cfg, &format!("/api/shares/info/{}", api::urlenc(&token)), None)?;
        target = s(&info["share"], "id").to_string();
        if target.is_empty() {
            bail!("找不到这条分享（链接可能已被删除）");
        }
    }
    // 撤自己的分享用登录会话；本机配了 admin 凭据（且没有会话）就走管理面。
    let d = if let Some(t) = config::token_opt(cfg) {
        api::del(cfg, &format!("/api/shares/{target}"), Some(&t))?
    } else {
        admin_req(cfg, "DELETE", &format!("/api/shares/{target}"), &None, &None, None)?
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("✅ 已撤销分享 {target}（链接立即失效）");
    Ok(())
}

fn share_info(cfg: &CliConfig, target: &str, json_out: bool) -> Result<()> {
    let token = normalize_token(target);
    let d = api::get(cfg, &format!("/api/shares/info/{}", api::urlenc(&token)), None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let sh = &d["share"];
    let art = &sh["artifact"];
    println!("分享   {}", s(sh, "id"));
    println!("制品   {}  ·  {}", s(art, "ref"), s(art, "kind"));
    println!("状态   {}", if b(&d, "usable") { "有效" } else { "已失效" });
    println!(
        "用量   {} 次 · 剩余 {}",
        si(&sh["uses"], "used"),
        if si(&sh["uses"], "max") == 0 {
            "不限".to_string()
        } else {
            si(&sh["uses"], "remaining").to_string()
        }
    );
    println!("直链   {}/s/{}/raw", cfg.base_url().trim_end_matches('/'), token);
    Ok(())
}
