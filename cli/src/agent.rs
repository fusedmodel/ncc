//! `ncc agent` —— 把我设计好的 Agent **点到点**交给指定的人。
//!
//! 一条命令产出一张**自解释的名片链接**，对方一条命令收下（可以只做一半）：
//!   作者：`ncc agent share [包目录|.hur]` → `https://ncc.ai/a/AC-xxxx#key`
//!   接受：`ncc agent add <链接>` → **装包**（落 `~/.ncc/packages`）+ **连接节点**（收进连接表）
//!
//! 四条边界（写代码前先读，`prd/ncc-agent-share.md` §6）：
//! 1. **名片 ≠ 分发渠道**：平台不展示、不检索；没有列表页。要长期分发请 `ncc publish`。
//! 2. **名片 ≠ 授权**：收下只代表**找得到**；取私有制品 / 分享页仍要 `ncc grant`。
//! 3. **收下 ≠ 执行**：这里只装包 + 连接，**不跑任何东西**、不改任何策略。
//! 4. **字节即事实**：接受的包一律先核对名片里的 sha256，对不上就**拒装**。
//!
//! 另外两条实现上的规矩：装包与连接是**两件事**，一个失败不许掩盖另一个；
//! 每种失败都要给「下一步做什么」，而不是丢一个错误码。
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::path::PathBuf;

use hur_core::{install, pack, profile, spec};

use crate::api;
use crate::config;
use crate::config::CliConfig;

#[derive(clap::Args)]
pub struct AgentCmd {
    #[command(subcommand)]
    action: AgentAction,
}

#[derive(Subcommand)]
pub enum AgentAction {
    /// 造一张名片：把 Agent 包 +（可选）节点做成一条点到点链接
    Share(ShareArgs),
    /// 收下别人给的名片：装包 +（可选）连接节点
    Add(AddArgs),
    /// 我发出去的名片（用量 / 有效期 / 是否已撤销）
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// 撤销一张名片（记录与字节一起删：撤销不是「标记一下」）
    Rm {
        /// 名片 id（AC-…）
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Args)]
pub struct ShareArgs {
    /// 包目录，或已产出的 `.hur` 文件（缺省 = 当前目录）
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 附带的节点：LD-… / @命名空间/节点slug；`none` = 名片只有包
    /// （缺省：在「我的节点」里找**唯一**一台 kind=agent 的）
    #[arg(long)]
    pub node: Option<String>,
    /// 名片上显示的名字（缺省用包里的 name）
    #[arg(long)]
    pub name: Option<String>,
    /// 给对方的备注（一句「这是干什么的、怎么用」）
    #[arg(long)]
    pub note: Option<String>,
    /// 有效期：7d / 24h / 30m / none（缺省 7d）
    #[arg(long, default_value = "7d")]
    pub expires: String,
    /// 最多几个人收下（0 = 不限，缺省 0）
    #[arg(long, default_value_t = 0)]
    pub uses: i64,
    /// 访问 key（3-16 位）：设了就必须带上它才能看/收（只回显这一次）
    #[arg(long)]
    pub key: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct AddArgs {
    /// 名片链接（https://ncc.ai/a/AC-xxxx#key）、裸 token，或 API 地址
    pub link: String,
    /// 访问 key（也可写在链接的 `#` 后面）
    #[arg(long)]
    pub key: Option<String>,
    /// 我给这个节点起的 Name 标签（缺省用名片建议的名字）
    #[arg(long)]
    pub name: Option<String>,
    /// 只连接节点，不装包
    #[arg(long)]
    pub no_install: bool,
    /// 只装包，不连接节点
    #[arg(long)]
    pub no_link: bool,
    /// 本机已装同 id 的包时覆盖
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 工具 ---------------- */

fn token_of(cfg: &CliConfig) -> Result<String> {
    config::require_token(cfg)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

/// 有效期写法 → 服务端认的 `expires`（Go duration 或 `none`）。
///
/// 这里**不自己算绝对时间**：`7d` 换算成 `168h` 交给服务端换算成到期时刻，
/// 客户端与服务端的时钟偏一点也不会让名片提前/延后过期。
fn expires_param(v: &str) -> Result<String> {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok("none".into());
    }
    let (num, unit) = t.split_at(t.len().saturating_sub(1));
    let n: i64 = num
        .parse()
        .with_context(|| format!("有效期写法认不出来：{t}（用 7d / 24h / 30m / none）"))?;
    if n <= 0 {
        bail!("有效期要大于 0：{t}");
    }
    let out = match unit {
        "d" => format!("{}h", n * 24),
        "h" | "m" | "s" => format!("{n}{unit}"),
        _ => bail!("有效期写法认不出来：{t}（用 7d / 24h / 30m / none）"),
    };
    Ok(out)
}

/// 从链接里抠出 token 与 key。接受的写法：
///   `https://ncc.ai/a/AC-xxxx#key` / `https://ncc.ai/a/AC-xxxx?key=key` / `AC-xxxx`
fn parse_link(raw: &str) -> Result<(String, Option<String>)> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("给一条名片链接或 token：ncc agent add https://ncc.ai/a/AC-xxxx");
    }
    // `#` 后面是 key（不发给服务端，只在本地拼进查询串）—— 与节点接入票据同一写法。
    let (head, frag) = match raw.split_once('#') {
        Some((h, f)) => (h, Some(f.trim().to_string())),
        None => (raw, None),
    };
    let mut key = frag.filter(|f| !f.is_empty());
    let (path, query) = match head.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (head, None),
    };
    if key.is_none() {
        if let Some(q) = query {
            for kv in q.split('&') {
                if let Some((k, v)) = kv.split_once('=') {
                    if k == "key" && !v.is_empty() {
                        key = Some(v.to_string());
                    }
                }
            }
        }
    }
    let token = path
        .rsplit_once("/a/")
        .map(|(_, t)| t)
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path))
        .trim()
        .trim_end_matches('/')
        .to_string();
    if token.is_empty() {
        bail!("链接里没找到名片 token（形如 https://ncc.ai/a/AC-xxxx）");
    }
    Ok((token, key))
}

/// 带 key 的路径（key 为空就原样返回）。
fn with_key(path: &str, key: &str) -> String {
    if key.is_empty() {
        path.to_string()
    } else {
        format!("{path}?key={}", api::urlenc(key))
    }
}

/// 作者侧的节点挑选：**不猜**。
/// 0 台 → 这张名片只有包；唯一一台 agent 节点 → 就用它；多台 → 让他显式 --node（并列出候选）。
fn pick_agent_node(cfg: &CliConfig, tk: &str) -> Result<Option<(String, String, String)>> {
    let d = api::get(cfg, "/api/nodes", Some(tk))?;
    let mine = d["mine"].as_array().cloned().unwrap_or_default();
    let agents: Vec<&Value> = mine
        .iter()
        .filter(|n| s(n, "kind") == "agent" || s(n, "kind") == "assigned")
        .collect();
    if agents.is_empty() {
        return Ok(None);
    }
    let refs: Vec<String> = agents
        .iter()
        .map(|n| format!("@{}/{}", s(n, "namespace"), s(n, "slug")))
        .collect();
    if agents.len() > 1 {
        bail!(
            "你有 {} 台 Agent 节点，得说清附带哪一台：\n  用 --node <引用> 指定：{}\n  或 --node none（这张名片只有包）",
            agents.len(),
            refs.join(" · ")
        );
    }
    let n = agents[0];
    Ok(Some((
        refs[0].clone(),
        s(n, "name").to_string(),
        s(n, "kind").to_string(),
    )))
}

/* ---------------- share ---------------- */

fn share(cfg: &CliConfig, a: &ShareArgs) -> Result<()> {
    let p = &a.path;
    // 包：目录 → 读清单 + 校验 + 产包；`.hur` → 直接用那份字节（已经产出过了就不要再产一遍）
    let (bytes, pkg) = if p.is_file() {
        let b = std::fs::read(p).with_context(|| format!("读 {} 失败", p.display()))?;
        let t = std::env::temp_dir().join(format!("ncc-agent-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        pack::unpack(p, &t).map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let pkg = spec::read_pkg(&t)?;
        let _ = std::fs::remove_dir_all(&t);
        (b, pkg)
    } else {
        let dir = pack::find_root(p)?;
        let pkg = spec::read_pkg(&dir)?;
        // 先问「是不是 Agent」，再问「包好不好」：这条命令的语义是「把我的 Agent 给人」，
        // 「你给我的是个 harness」是更根本的错，先把这句说清楚。
        let prof = pkg
            .profile
            .clone()
            .unwrap_or_else(|| profile::from_kind(&pkg.kind).to_string());
        if prof != "agent" && pkg.kind != "agent" {
            bail!(
                "这不是 Agent 包（profile={prof}）：`ncc agent share` 只分享 Agent。\n  \
                 要分发这个包：ncc hur publish {}（或 ncc publish --file …）",
                p.display()
            );
        }
        let issues = spec::validate(&pkg, &dir, None, false);
        let errs: Vec<&spec::Issue> = issues
            .iter()
            .filter(|i| matches!(i.level, spec::Level::Error))
            .collect();
        if !errs.is_empty() {
            bail!(
                "包没通过校验，先修好再分享（{} 个错误）——`ncc hur verify {}` 看全部",
                errs.len(),
                dir.display()
            );
        }
        let out = pack::pack(&dir).map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let b = std::fs::read(&out.file).with_context(|| format!("读 {} 失败", out.file.display()))?;
        (b, pkg)
    };

    // profile：清单里没写时由这里解析好一并上报（服务端不重复实现 hur-core 的推导规则）。
    let kind = pkg
        .profile
        .clone()
        .unwrap_or_else(|| profile::from_kind(&pkg.kind).to_string());
    if p.is_file() && kind != "agent" && pkg.kind != "agent" {
        bail!(
            "这不是 Agent 包（profile={kind}）：`ncc agent share` 只分享 Agent。\n  \
             要分发这个包：ncc hur publish {}（或 ncc publish --file …）",
            p.display()
        );
    }

    let tk = token_of(cfg)?;
    let expires = expires_param(&a.expires)?;

    // 节点那一侧
    let node_ref = match a.node.as_deref().map(str::trim) {
        Some("none") => String::new(),
        Some(r) if !r.is_empty() => r.to_string(),
        _ => match pick_agent_node(cfg, &tk)? {
            Some((r, _, _)) => r,
            None => String::new(),
        },
    };

    let name = a.name.clone().unwrap_or_else(|| pkg.name.clone());
    let mut qs = vec![
        format!("name={}", api::urlenc(&name)),
        format!("expires={}", api::urlenc(&expires)),
        format!("profile={}", api::urlenc(&kind)),
    ];
    if let Some(n) = a.note.as_deref().filter(|x| !x.trim().is_empty()) {
        qs.push(format!("note={}", api::urlenc(n.trim())));
    }
    if !node_ref.is_empty() {
        qs.push(format!("node={}", api::urlenc(&node_ref)));
    }
    if a.uses > 0 {
        qs.push(format!("uses={}", a.uses));
    }
    if let Some(k) = a.key.as_deref().filter(|x| !x.trim().is_empty()) {
        qs.push(format!("key={}", api::urlenc(k.trim())));
    }

    let d = api::request(
        cfg,
        "POST",
        &format!("/api/agent-cards?{}", qs.join("&")),
        Some(&tk),
        None,
        Some(&bytes),
        &[("Content-Type", "application/octet-stream")],
    )?;
    let card = &d["card"];
    let url = s(&d, "url");
    let key = s(&d, "key");
    let link = if key.is_empty() {
        url.to_string()
    } else {
        format!("{url}#{key}")
    };

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "card": card, "url": url, "key": key, "addCommand": format!("ncc agent add '{link}'"),
            }))?
        );
        return Ok(());
    }
    println!("✅ 名片已生成（点到点：平台不展示、不检索，只有拿到链接的人能看）");
    println!("   名片    {} · {} v{}（{} 字节）", s(card, "name"), s(&card["agent"], "id"), s(&card["agent"], "version"), d["card"]["agent"]["bytes"]);
    println!("   指纹    {}", s(&card["agent"], "sha256"));
    if !node_ref.is_empty() {
        println!("   节点    {node_ref}（对方收下时会收进他自己的连接表）");
    } else {
        println!("   节点    无（这张名片只有包；要带上节点：ncc living --kind agent，再加 --node）");
    }
    let exp = s(card, "expiresAt");
    let uses = card["maxUses"].as_i64().unwrap_or(0);
    println!(
        "   有效期  {}{}",
        if exp.is_empty() { "长期".to_string() } else { exp.replace('T', " ").trim_end_matches('Z').to_string() + "Z" },
        if uses > 0 { format!(" · 限 {uses} 人") } else { " · 不限次".to_string() }
    );
    println!("\n   链接（发给对方）");
    println!("     {link}");
    if !key.is_empty() {
        println!("     ⚠️ 访问 key 只在**这一次**显示（库里只存哈希，之后取不回来）");
    }
    println!("\n   对方要做的");
    println!("     ncc agent add '{link}'");
    println!("\n   撤销    ncc agent rm {}", s(card, "id"));
    Ok(())
}

/* ---------------- add ---------------- */

fn add(cfg: &CliConfig, a: &AddArgs) -> Result<()> {
    let (tok, frag_key) = parse_link(&a.link)?;
    let key = a
        .key
        .as_deref()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .or(frag_key)
        .unwrap_or_default();
    let tk = token_of(cfg)?;

    // 1) 取名片（匿名可读；带登录态是为了让作者自己也能看自己的）
    let d = api::get(cfg, &with_key(&format!("/api/agent-cards/{tok}"), &key), Some(&tk))?;
    let card = d["card"].clone();
    let agent = card["agent"].clone();
    let sha = s(&agent, "sha256").to_string();
    let agent_id = s(&agent, "id").to_string();
    let agent_ver = s(&agent, "version").to_string();
    let card_id = s(&card, "id").to_string();

    if !a.json {
        println!(
            "📇 {} · {} v{}",
            s(&card, "name"),
            if agent_id.is_empty() { "?" } else { &agent_id },
            agent_ver
        );
        let by = s(&card["author"], "displayName");
        let handle = s(&card["author"], "handle");
        if !by.is_empty() {
            println!("   来自  {}{}", by, if handle.is_empty() { String::new() } else { format!("（@{handle}）") });
        }
        if !s(&card, "note").is_empty() {
            println!("   备注  {}", s(&card, "note"));
        }
    }

    // 2) 扣一次额度：**在下载之前**。额度是「投递次数」不是「安装次数」，
    //    先传后扣等于没限次。
    let acc = api::request(
        cfg,
        "POST",
        &with_key(&format!("/api/agent-cards/{tok}/accept"), &key),
        Some(&tk),
        None,
        None,
        &[],
    )?;

    let mut installed: Option<install::InstallRecord> = None;
    let mut link_id = String::new();
    let mut link_warn = String::new();

    // 3) 装包：核对 sha256 —— 这是唯一的硬门禁，对不上就一律不装。
    if !a.no_install {
        let blob = s(&agent, "blob");
        if blob.is_empty() {
            bail!("名片里没有包（只有节点）——要只连接节点请加 --no-install");
        }
        let bytes = api::get_bytes(cfg, &with_key(blob, &key), None)?;
        let work = std::env::temp_dir().join(format!("ncc-agent-add-{}", std::process::id()));
        std::fs::create_dir_all(&work).with_context(|| format!("创建 {} 失败", work.display()))?;
        let file = work.join(format!("{agent_id}-{agent_ver}.hur"));
        std::fs::write(&file, &bytes)?;
        // install_archive 自己核对 expected_sha：不一致就不落地（名片的指纹 = 服务端算的那份）
        let rec = install::install_archive(
            &file,
            if sha.is_empty() { None } else { Some(sha.as_str()) },
            Some(install::Source { registry: cfg.base_url(), slug: card_id.clone() }),
            a.force,
        )
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let _ = std::fs::remove_dir_all(&work);
        installed = Some(rec);
    }

    // 4) 连接节点：装包成功与否都不影响这一步，失败也**不许**掩盖装包成功。
    if !a.no_link {
        let node = card["node"].clone();
        let r = s(&node, "ref");
        if !r.is_empty() {
            let label = a
                .name
                .as_deref()
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                // 缺省用**名片上的名字**：那才是作者想让别人认出的叫法；
                // 节点自报的名字（hotel-agent 这种）只是它的机器名。
                .or_else(|| {
                    let n = s(&card, "name").trim().to_string();
                    if n.is_empty() { None } else { Some(n) }
                })
                .unwrap_or_else(|| s(&node, "label").to_string());
            let body = json!({ "ref": r, "label": label, "note": "由 ncc agent add 收下" });
            match api::post_json(cfg, "/api/nodes/links", Some(&tk), &body) {
                Ok(d) => link_id = s(&d, "linkId").to_string(),
                Err(e) => link_warn = format!("{e}"),
            }
        }
    }

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "card": card,
                "accept": acc,
                "install": installed.as_ref().map(|r| json!({
                    "id": r.id, "name": r.name, "version": r.version, "path": r.path, "sha256": r.sha256,
                })),
                "linkId": if link_id.is_empty() { Value::Null } else { json!(link_id) },
                "linkError": if link_warn.is_empty() { Value::Null } else { json!(link_warn) },
            }))?
        );
        return Ok(());
    }

    if let Some(rec) = &installed {
        println!("\n✅ 已装包 {} v{}", rec.name, rec.version);
        println!("   目录  {}", rec.path);
        println!("   校验  sha256 {}（与名片一致）", rec.sha256);
        println!("   怎么接 ncc hur profile {}", rec.path);
    }
    if !link_id.is_empty() {
        println!("\n✅ 已连接节点（在**你的**连接表里，与我给它起的名字一起）");
        println!("   连接 id {link_id} · 改名字：ncc nodes label {link_id} --label \"…\"");
    } else if !link_warn.is_empty() {
        println!("\n✗ 节点没连上：{link_warn}");
        println!("   （包已经装好了，这一步可以单独重试：ncc nodes link <引用> --label \"…\"）");
    }
    println!("\n提醒：收下只代表**找得到**；要取对方的私有制品仍需单独授权：ncc grant set --user <对方> --kind artifact");
    Ok(())
}

/* ---------------- ls / rm ---------------- */

fn ls(cfg: &CliConfig, json_out: bool) -> Result<()> {
    let tk = token_of(cfg)?;
    let d = api::get(cfg, "/api/agent-cards", Some(&tk))?;
    let rows = d["cards"].as_array().cloned().unwrap_or_default();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("还没有发出去的名片。造一张：ncc agent share ./my-agent");
        return Ok(());
    }
    println!("我发出去的名片（{}）", rows.len());
    for c in &rows {
        let state = s(c, "state");
        let mark = match state {
            "revoked" => "已撤销",
            "expired" => "已过期",
            "exhausted" => "已用完",
            _ => "可用",
        };
        let uses = c["maxUses"].as_i64().unwrap_or(0);
        println!(
            "  {:<22} {:<8} {:<10} 收 {}",
            s(c, "name"),
            mark,
            s(c, "id"),
            if uses > 0 { format!("{}/{}", c["uses"].as_i64().unwrap_or(0), uses) } else { format!("{}", c["uses"].as_i64().unwrap_or(0)) }
        );
        println!("     链接 {}", s(c, "url"));
    }
    println!("\n撤销：ncc agent rm <AC-…>（连字节一起删）");
    Ok(())
}

fn rm(cfg: &CliConfig, id: &str, json_out: bool) -> Result<()> {
    let tk = token_of(cfg)?;
    let d = api::del(cfg, &format!("/api/agent-cards/{}", api::urlenc(id.trim())), Some(&tk))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
    } else {
        println!("✅ 已撤销 {}（字节已删，链接现在 410 revoked；记录留着，ls 里还看得到它）", id.trim());
    }
    Ok(())
}

pub fn run(cfg: &CliConfig, cmd: &AgentCmd) -> Result<()> {
    match &cmd.action {
        AgentAction::Share(a) => share(cfg, a),
        AgentAction::Add(a) => add(cfg, a),
        AgentAction::Ls { json } => ls(cfg, *json),
        AgentAction::Rm { id, json } => rm(cfg, id, *json),
    }
}

/// 单元测试：链接解析是「用户手抄链接」这条路上最容易出错的一环，钉死它。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_link_forms() {
        let (t, k) = parse_link("https://ncc.ai/a/AC-1234").unwrap();
        assert_eq!(t, "AC-1234");
        assert_eq!(k, None);

        let (t, k) = parse_link("https://ncc.ai/a/AC-1234#s3cret").unwrap();
        assert_eq!(t, "AC-1234");
        assert_eq!(k.as_deref(), Some("s3cret"));

        let (t, k) = parse_link("https://ncc.ai/a/AC-1234?key=s3cret").unwrap();
        assert_eq!(t, "AC-1234");
        assert_eq!(k.as_deref(), Some("s3cret"));

        // 裸 token（有人只抄了那一段）
        let (t, k) = parse_link("AC-1234").unwrap();
        assert_eq!(t, "AC-1234");
        assert_eq!(k, None);
    }

    #[test]
    fn expires_forms() {
        assert_eq!(expires_param("7d").unwrap(), "168h");
        assert_eq!(expires_param("24h").unwrap(), "24h");
        assert_eq!(expires_param("30m").unwrap(), "30m");
        assert_eq!(expires_param("none").unwrap(), "none");
        assert_eq!(expires_param("").unwrap(), "none");
        assert!(expires_param("7x").is_err());
        assert!(expires_param("0d").is_err());
    }
}
