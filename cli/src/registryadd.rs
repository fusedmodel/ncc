// NCC Registry 接入与集群写：`ncc registry add / ticket / replicate / rm`
//
// 两条接入路径，同一张票据（服务端 /api/access/*）：
//
//	短链     ncc registry add http://registry.internal:8282/j/NK-7F3A2C#<secret> --join
//	key/secret  ncc registry add --base http://registry.internal:8282 --key NK-7F3A2C --secret <secret> --join
//
// 短链里的 secret 放在 URL fragment（#）—— 不进服务端日志、不进 Referer；
// 兑换出来的是一枚**节点令牌**：默认只能上报自己的心跳与读公开制品，不能发布。
//
// 集群写（master 是发布入口）：
//
//	ncc registry replicate @ns/slug --to all      # 把制品分发到 worker（副本）
//	ncc registry rm @ns/slug                      # 下架 + 回收各节点副本
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct AddArgs {
    /// 接入短链（http://host/j/<key>#<secret>）或注册中心地址（配 --key/--secret）
    pub target: Option<String>,
    /// 注册中心地址（与 --key/--secret 搭配时用；也可由 target 给出）
    #[arg(long)]
    pub base: Option<String>,
    /// 票据 key（如 NK-7F3A2C）
    #[arg(long)]
    pub key: Option<String>,
    /// 票据 secret
    #[arg(long)]
    pub secret: Option<String>,
    /// 接入后立刻把本机作为一个节点托管进去
    #[arg(long)]
    pub join: bool,
    /// 节点声明类型（--join 时生效）
    #[arg(long, default_value = "agent", value_parser = ["service", "agent", "assigned"])]
    pub kind: String,
    /// 节点名（--join 时生效，默认 HOSTNAME）
    #[arg(long)]
    pub name: Option<String>,
    /// 所在区域
    #[arg(long)]
    pub region: Option<String>,
    /// 可提供的能力 kind，逗号分隔
    #[arg(long)]
    pub capabilities: Option<String>,
}

#[derive(clap::Args)]
pub struct TicketCmd {
    #[command(subcommand)]
    pub action: TicketAction,
}

#[derive(clap::Subcommand)]
pub enum TicketAction {
    /// 签发一张接入票据（返回 key + secret + 接入短链；secret 只显示一次）
    Create(TicketCreateArgs),
    /// 列出我签发的票据（不含 secret）
    List,
    /// 删除票据（已兑换的令牌到期前仍有效）
    Rm { id: String },
}

#[derive(clap::Args)]
pub struct TicketCreateArgs {
    /// 备注（发给谁 / 干什么）
    #[arg(long)]
    pub label: Option<String>,
    /// 可用次数（0 = 不限次）
    #[arg(long)]
    pub uses: Option<i64>,
    /// 有效天数（0 = 不过期）
    #[arg(long)]
    pub expires: Option<i64>,
    /// 落到的命名空间（默认你的个人命名空间）
    #[arg(long = "ns")]
    pub namespace: Option<String>,
    /// 兑换出的节点令牌作用域，逗号分隔（默认 nodes:write,registry:read,registry:download）
    #[arg(long)]
    pub scopes: Option<String>,
}

#[derive(clap::Args)]
pub struct ReplicateArgs {
    /// 制品引用：@命名空间/slug 或 A-… id
    pub target: String,
    /// 目标节点：all 或 worker 名称/id（逗号分隔）
    #[arg(long, default_value = "all")]
    pub to: String,
}

#[derive(clap::Args)]
pub struct RmArgs {
    /// 制品引用：@命名空间/slug 或 A-… id
    pub target: String,
    /// 不确认，直接下架
    #[arg(long)]
    pub yes: bool,
}

/* ---------------- 接入 ---------------- */

/// 解析接入短链：http://host/j/<key>#<secret>（fragment 里也可能写成 s=<secret>）。
fn parse_target(raw: &str) -> (String, String, String) {
    let s = raw.trim();
    let (head, frag) = match s.split_once('#') {
        Some((h, f)) => (h, f),
        None => (s, ""),
    };
    let secret = frag.strip_prefix("s=").unwrap_or(frag).to_string();
    if let Some(i) = head.find("/j/") {
        let base = head[..i].trim_end_matches('/').to_string();
        let key = head[i + 3..].trim_matches('/').to_string();
        return (base, key, secret);
    }
    (head.trim_end_matches('/').to_string(), String::new(), secret)
}

/// `ncc registry add` —— 用一条短链（或 key/secret）把内网 registry 接进来。
pub fn add(cfg: &CliConfig, a: &AddArgs) -> Result<()> {
    let (mut base, mut key, mut secret) = ("".to_string(), String::new(), String::new());
    if let Some(t) = a.target.as_deref() {
        let (b, k, s) = parse_target(t);
        base = b;
        key = k;
        secret = s;
    }
    if let Some(b) = a.base.as_deref() {
        base = b.trim_end_matches('/').to_string();
    }
    if let Some(k) = a.key.as_deref() {
        key = k.trim().to_string();
    }
    if let Some(s) = a.secret.as_deref() {
        secret = s.trim().to_string();
    }
    if base.is_empty() {
        base = cfg.base_url.clone();
    }
    if key.is_empty() || secret.is_empty() {
        bail!(
            "需要完整的接入凭据。用短链：ncc registry add <http://host/j/KEY#SECRET>；\n\
             或分开给：ncc registry add --base {} --key <key> --secret <secret>",
            base
        );
    }

    // 兑换发生在目标节点上，所以先切到目标 base 再发请求。
    let mut target = cfg.clone();
    target.base_url = base.clone();

    let mut body = json!({ "key": key, "secret": secret });
    if a.join {
        let name = a.name.clone().unwrap_or_else(crate::default_device_name);
        let caps: Vec<String> = a
            .capabilities
            .as_deref()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default();
        body["node"] = json!({
            "name": name,
            "kind": a.kind,
            "region": a.region.clone().unwrap_or_default(),
            "capabilities": caps,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "version": env!("CARGO_PKG_VERSION"),
        });
    }

    let d = api::post_json(&target, "/api/access/redeem", None, &body)
        .context("兑换接入票据失败（检查 key/secret 与网络连通性）")?;
    let token = d["token"].as_str().context("响应缺少 token")?;
    let reg = &d["registry"];
    let owner = &d["owner"];

    // 语出同源：把「这个 registry + 这枚令牌」写进配置，后续命令直接可用。
    let mut c = cfg.clone();
    c.base_url = reg["base"].as_str().unwrap_or(base.as_str()).trim_end_matches('/').to_string();
    c.token = Some(token.to_string());
    c.email = Some(format!("@{}", owner["namespace"].as_str().unwrap_or("")));
    c.name = Some(owner["name"].as_str().unwrap_or("").to_string());
    config::save(&c)?;

    println!("✅ 已接入内网 registry");
    println!(
        "   节点     {}  {}  {}",
        reg["nodeName"].as_str().unwrap_or(""),
        reg["nodeId"].as_str().unwrap_or(""),
        reg["version"].as_str().unwrap_or("")
    );
    println!("   地址     {}   控制台 {}/", c.base_url, c.base_url);
    println!(
        "   身份     {} @{} 的节点令牌（作用域 {}）",
        owner["name"].as_str().unwrap_or(""),
        owner["namespace"].as_str().unwrap_or(""),
        d["scopes"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    );
    println!("   到期     {}", d["expiresAt"].as_str().unwrap_or("—"));
    if let Some(n) = d.get("node").filter(|n| !n.is_null()) {
        if !n["id"].as_str().unwrap_or("").is_empty() {
            println!(
                "   已托管   @{}/{}  {}  {}",
                n["namespace"]["slug"].as_str().unwrap_or(""),
                n["slug"].as_str().unwrap_or(""),
                n["kind"].as_str().unwrap_or(""),
                if n["online"].as_bool().unwrap_or(false) { "在线" } else { "离线" }
            );
        }
    }
    println!("\n看集群与我的节点：ncc registry status");
    if !a.join {
        println!("把本机也托管进去：ncc registry join --kind agent --region <你的区域>");
    }
    Ok(())
}

/* ---------------- 票据管理 ---------------- */

/// `ncc registry ticket …`
pub fn ticket(cfg: &CliConfig, cmd: &TicketCmd) -> Result<()> {
    let t = config::require_token(cfg)?;
    match &cmd.action {
        TicketAction::Create(a) => {
            let scopes: Vec<String> = a
                .scopes
                .as_deref()
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            let mut body = json!({
                "label": a.label.clone().unwrap_or_default(),
                "uses": a.uses.unwrap_or(0),
                "expiresInDays": a.expires.unwrap_or(0),
            });
            if !scopes.is_empty() {
                body["scopes"] = json!(scopes);
            }
            if let Some(ns) = a.namespace.as_deref().filter(|x| !x.is_empty()) {
                body["namespace"] = json!(ns);
            }
            let d = api::post_json(cfg, "/api/access/tickets", Some(&t), &body)?;
            let tk = &d["ticket"];
            println!("✅ 已签发接入票据");
            println!("   id       {}", tk["id"].as_str().unwrap_or(""));
            println!("   key      {}", tk["key"].as_str().unwrap_or(""));
            println!("   secret   {}   ← 只显示这一次，请现在保存", d["secret"].as_str().unwrap_or(""));
            println!("   作用域   {}", join_str(&tk["scopes"]));
            println!(
                "   次数     {}{}",
                tk["uses"]["max"].as_i64().unwrap_or(0),
                if tk["uses"]["max"].as_i64().unwrap_or(0) == 0 { "（不限）" } else { "" }
            );
            println!("   到期     {}", tk["expiresAt"].as_str().unwrap_or("不过期"));
            println!("\n接入短链（发给对方，粘贴即可接入）：");
            println!("   {}", d["link"].as_str().unwrap_or(""));
            println!("\n对方拿到链接后：");
            println!("   ncc registry add '<上面这条链接>' --join");
            Ok(())
        }
        TicketAction::List => {
            let d = api::get(cfg, "/api/access/tickets", Some(&t))?;
            let rows = d["tickets"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("还没有票据。签发一张：ncc registry ticket create --label \"给 alice 的 Agent\"");
                return Ok(());
            }
            println!("我签发的接入票据（{} 张）", rows.len());
            for x in &rows {
                println!(
                    "  {:<14} {:<18} {:<10} {:<20} {}",
                    x["id"].as_str().unwrap_or(""),
                    x["key"].as_str().unwrap_or(""),
                    use_txt(x),
                    if x["usable"].as_bool().unwrap_or(false) { "可用" } else { "已失效" },
                    x["label"].as_str().unwrap_or("")
                );
                println!(
                    "     作用域 {} · 到期 {} · 最近使用 {}",
                    join_str(&x["scopes"]),
                    x["expiresAt"].as_str().unwrap_or("不过期"),
                    x["lastUsedAt"].as_str().unwrap_or("—")
                );
            }
            println!("\n删除（已兑换的令牌到期前仍有效）：ncc registry ticket rm <id>");
            Ok(())
        }
        TicketAction::Rm { id } => {
            api::del(cfg, &format!("/api/access/tickets/{}", id), Some(&t))?;
            println!("✅ 已删除票据 {id}（未兑换的链接立即失效）");
            Ok(())
        }
    }
}

fn use_txt(v: &Value) -> String {
    let max = v["uses"]["max"].as_i64().unwrap_or(0);
    let used = v["uses"]["used"].as_i64().unwrap_or(0);
    if max == 0 {
        format!("已用 {used}/不限")
    } else {
        format!("已用 {used}/{max}")
    }
}

fn join_str(v: &Value) -> String {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/* ---------------- 集群写：分发 / 下架回收 ---------------- */

/// `ncc registry replicate <ref> --to all|a,b`
pub fn replicate(cfg: &CliConfig, a: &ReplicateArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let targets: Value = if a.to.trim().eq_ignore_ascii_case("all") {
        json!("all")
    } else {
        json!(a.to.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
    };
    let d = api::post_json(
        cfg,
        "/api/cluster/replicate",
        Some(&t),
        &json!({ "ref": a.target, "targets": targets }),
    )?;
    let results = d["results"].as_array().cloned().unwrap_or_default();
    println!("分发 {} → {} 个目标", d["ref"].as_str().unwrap_or(&a.target), results.len());
    if results.is_empty() {
        println!("  没有可用的 worker（先在别的机器上以 NCCR_ROLE=worker 启动并指向本节点）");
    }
    for r in &results {
        if r["ok"].as_bool().unwrap_or(false) {
            println!(
                "  ✓ {:<20} {}  {}",
                r["nodeName"].as_str().unwrap_or(""),
                r["nodeUrl"].as_str().unwrap_or(""),
                human_bytes(r["size"].as_i64().unwrap_or(0))
            );
        } else {
            println!(
                "  ✗ {:<20} {}",
                r["nodeName"].as_str().unwrap_or(""),
                r["error"].as_str().unwrap_or("失败")
            );
        }
    }
    println!("\n看聚合目录：ncc registry catalog");
    Ok(())
}

/// `ncc registry rm <ref>` —— 下架 + 回收各节点上的副本。
pub fn rm(cfg: &CliConfig, a: &RmArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    if !a.yes {
        println!("即将下架 {}（本节点记录 + 字节，并回收其它节点上的副本）", a.target);
        println!("确认加 --yes 再跑一次。");
        return Ok(());
    }
    let d = api::request(cfg, "DELETE", &format!("/api/registry/{}", a.target), Some(&t), None, None, &[])?;
    println!("✅ 已下架 {}", d["ref"].as_str().unwrap_or(&a.target));
    let revoked = d["revoked"].as_array().cloned().unwrap_or_default();
    for r in &revoked {
        let mark = if r["ok"].as_bool().unwrap_or(false) { "✓" } else { "✗" };
        let detail = if r["ok"].as_bool().unwrap_or(false) {
            if r["removed"].as_bool().unwrap_or(false) { "副本已回收" } else { "无副本" }.to_string()
        } else {
            r["error"].as_str().unwrap_or("失败").to_string()
        };
        println!("  {mark} {:<20} {}", r["nodeName"].as_str().unwrap_or(""), detail);
    }
    if revoked.is_empty() {
        println!("  （集群里没有持有该制品的其它节点）");
    }
    Ok(())
}

fn human_bytes(n: i64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}
