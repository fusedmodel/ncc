// NCC Config（配置托管）—— `ncc registry config …`
//
// 把团队的网络 / 基础设施 / Agent 配置托管到 ncc-registry 上，并按需给 Agent 使用：
//
//	list      看目录（公开配置匿名可见；自己的配置加 --mine）
//	get       取一份配置（默认打码，--reveal 才出明文；--revision 取历史版本）
//	set       写入（有则改、无则建；内容来自 --file / --value / stdin）
//	history   版本历史（谁在什么时候改了什么）
//	rollback  回滚到某一版（作为新版本写回，历史只增不改）
//	bundle    把某个环境的整套配置一次拉全（Agent 落地基础设施的第一步）
//	kinds     配置类型 / 格式 / 环境目录
//	rm        删除（连同历史）
//
// 权限边界（与服务端一致，别在这里另立一套）：
//   - 读公开配置：谁都能读；
//   - 读非公开配置：需要 config:read 作用域，且是命名空间成员或拿到 config 授权；
//   - 写：需要 config:write 作用域，且是命名空间成员（外部只有读的授权）。
// 因而给 Agent 的长效凭据应该是**限定作用域的接入票据**：
//   ncc registry ticket create --label agent-conf --scopes config:read,config:write,nodes:write
use crate::api::{self, urlenc};
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct ConfigCmd {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(clap::Subcommand)]
pub enum ConfigAction {
    /// 配置目录（--mine 只看我的；--namespace 只看某个团队）
    List(ListArgs),
    /// 取一份配置：@命名空间/slug 或 C-… id
    Get(GetArgs),
    /// 写入（有则改、无则建）
    Set(SetArgs),
    /// 版本历史
    History { target: String, #[arg(long)] json: bool },
    /// 回滚到某一版（作为新版本写回）
    Rollback(RollbackArgs),
    /// 把一个环境的整套配置拉到本地目录
    Bundle(BundleArgs),
    /// 配置类型 / 格式 / 环境目录
    Kinds { #[arg(long)] json: bool },
    /// 删除（连同版本历史）
    Rm(RmArgs),
}

#[derive(clap::Args)]
pub struct ListArgs {
    /// 只看某个命名空间（如 @team）
    #[arg(long = "ns")]
    pub namespace: Option<String>,
    /// 按配置类型过滤（见 `ncc registry config kinds`）
    #[arg(long)]
    pub kind: Option<String>,
    /// 按环境过滤（any|dev|staging|prod，含 any 通用项）
    #[arg(long)]
    pub env: Option<String>,
    /// 按标签过滤
    #[arg(long)]
    pub tag: Option<String>,
    /// 关键词（名称 / slug / 说明 / 标签）
    #[arg(long)]
    pub q: Option<String>,
    /// 只看我（owner/成员）命名空间里的配置
    #[arg(long)]
    pub mine: bool,
    /// 只要含敏感值的配置
    #[arg(long)]
    pub secrets: bool,
    /// 直接输出原始 JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct GetArgs {
    /// 配置引用：@命名空间/slug 或 C-… id
    pub target: String,
    /// 取历史版本（见 history）
    #[arg(long)]
    pub revision: Option<i64>,
    /// 输出明文（默认打码：只回校验和与大小）
    #[arg(long)]
    pub reveal: bool,
    /// 写到文件（而不是打印）
    #[arg(long)]
    pub out: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct SetArgs {
    /// 配置引用：@命名空间/slug（建新配置时必须是这形态）
    pub target: String,
    /// 内容来自文件
    #[arg(long)]
    pub file: Option<String>,
    /// 内容直接给出（短配置；与 --file 二选一）
    #[arg(long)]
    pub value: Option<String>,
    /// 配置类型：network|gateway|infra|registry|agent|ci|observability|security|app|other
    #[arg(long)]
    pub kind: Option<String>,
    /// 环境：any|dev|staging|prod
    #[arg(long)]
    pub env: Option<String>,
    /// 格式：json|yaml|toml|env|ini|text|shell（缺省按 --file 扩展名猜）
    #[arg(long)]
    pub format: Option<String>,
    /// 展示名
    #[arg(long)]
    pub name: Option<String>,
    /// 一句话说明
    #[arg(long)]
    pub summary: Option<String>,
    /// 标签，逗号分隔
    #[arg(long)]
    pub tags: Option<String>,
    /// 含敏感值：内容静态加密、默认打码（会强制私有）
    #[arg(long)]
    pub secret: bool,
    /// 改为公开可读（含敏感值的配置不允许）
    #[arg(long)]
    pub public: bool,
    /// 变更说明（写进版本历史）
    #[arg(long)]
    pub note: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct RollbackArgs {
    pub target: String,
    /// 回到哪一版（见 history）
    #[arg(long)]
    pub to: i64,
    /// 变更说明
    #[arg(long)]
    pub note: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct BundleArgs {
    /// 哪个团队的配置（如 @team）
    #[arg(long = "ns")]
    pub namespace: String,
    /// 环境：命中该环境与 any 通用项
    #[arg(long)]
    pub env: Option<String>,
    /// 限定配置类型
    #[arg(long)]
    pub kind: Option<String>,
    /// 限定标签
    #[arg(long)]
    pub tag: Option<String>,
    /// 带上含敏感值的配置（默认跳过，避免一次把凭据全落盘）
    #[arg(long)]
    pub secrets: bool,
    /// 带明文（缺省则文件内容是空，只有元数据）
    #[arg(long)]
    pub reveal: bool,
    /// 写到目录（缺省只打印清单）
    #[arg(long)]
    pub out: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct RmArgs {
    /// 配置引用：@命名空间/slug 或 C-… id
    pub target: String,
    /// 不确认，直接删
    #[arg(long)]
    pub yes: bool,
}

/* ---------------- 小工具 ---------------- */

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn arr(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str()).map(String::from).collect())
        .unwrap_or_default()
}

fn split_csv(v: &str) -> Vec<String> {
    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// 配置引用的两种写法：C-… id 与 @命名空间/slug。
///
/// ⚠️ 直接拼进路径，别用 urlenc：斜杠是服务端路由本身的分隔符（`/:id/:slug`），
/// 编码成 `%2F` 之后服务端只看到一个 id，会找不到配置。
fn config_path(target: &str) -> String {
    format!("/api/configs/{}", target.trim())
}

/// 从引用里取命名空间（@team/x → team），供 set 建新配置用。
fn ns_of(target: &str) -> Option<String> {
    let t = target.trim().trim_start_matches('@');
    let mut parts = t.splitn(2, '/');
    let ns = parts.next().unwrap_or("");
    let slug = parts.next().unwrap_or("");
    if ns.is_empty() || slug.is_empty() {
        None
    } else {
        Some(ns.to_string())
    }
}

fn slug_of(target: &str) -> Option<String> {
    let t = target.trim().trim_start_matches('@');
    let mut parts = t.splitn(2, '/');
    parts.next();
    parts.next().map(|x| x.to_string()).filter(|x| !x.is_empty())
}

/// 由文件扩展名猜格式。
fn format_of_file(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?.to_lowercase();
    let f = match ext.as_str() {
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "env" => "env",
        "ini" | "conf" => "ini",
        "sh" | "bash" => "shell",
        "txt" | "md" => "text",
        _ => return None,
    };
    Some(f.to_string())
}

/// worker 节点上的配置是**本节点本地**的：多节点共享请指向 master。
fn warn_if_worker(cfg: &CliConfig) {
    if let Ok(meta) = api::get(cfg, "/api/meta", None) {
        if s(&meta["node"], "role") == "worker" {
            println!(
                "⚠ 你指向的是 worker 节点（{}）：配置以**本节点库**为准；多节点共享请指向 master。",
                cfg.base_url()
            );
        }
    }
}

/* ---------------- 目录 / 目录信息 ---------------- */

/// `ncc registry config kinds`
pub fn kinds(cfg: &CliConfig, json_out: bool) -> Result<()> {
    let d = api::get(cfg, "/api/configs/kinds", config::token_opt(cfg).as_deref())?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("配置类型（业务坐标系）");
    for k in d["kinds"].as_array().cloned().unwrap_or_default() {
        let c = k["count"].as_i64().unwrap_or(0);
        let mark = if c > 0 { format!("（{c}）") } else { String::new() };
        println!(
            "  {:<14} {:<12}{}  {}",
            s(&k, "kind"),
            s(&k, "label"),
            mark,
            s(&k, "desc")
        );
    }
    println!(
        "\n格式 {}",
        arr(d.get("formats")).join(" | ")
    );
    println!("环境 {}", arr(d.get("envs")).join(" | "));
    println!(
        "\n读取：公开且 active 的配置谁都能读；非公开需要 config:read + 成员身份或 config 授权。\n写入：需要 config:write + 命名空间成员身份。"
    );
    Ok(())
}

/// `ncc registry config list`
pub fn list(cfg: &CliConfig, a: &ListArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs: Vec<String> = Vec::new();
    for (k, v) in [
        ("namespace", a.namespace.clone()),
        ("kind", a.kind.clone()),
        ("env", a.env.clone()),
        ("tag", a.tag.clone()),
        ("q", a.q.clone()),
    ] {
        if let Some(x) = v.filter(|x| !x.is_empty()) {
            qs.push(format!("{k}={}", urlenc(x.trim_start_matches('@'))));
        }
    }
    if a.mine {
        qs.push("mine=1".into());
    }
    if a.secrets {
        qs.push("secrets=1".into());
    }
    let path = if qs.is_empty() {
        "/api/configs".to_string()
    } else {
        format!("/api/configs?{}", qs.join("&"))
    };
    let d = api::get(cfg, &path, t.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d["configs"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        if a.mine {
            println!("你还没有托管任何配置。写一份试试：");
            println!("  ncc registry config set @你的命名空间/network --file ./network.yaml \\");
            println!("      --kind network --env prod --note \"初始版本\"");
        } else {
            println!("没有符合条件的配置（默认只列公开配置；自己的加 --mine）。");
        }
        return Ok(());
    }
    println!("配置 {} 条（内容默认打码）", rows.len());
    for c in &rows {
        let flags = [
            if c["secret"].as_bool().unwrap_or(false) { "敏感" } else { "" },
            if s(c, "visibility") == "private" { "私有" } else { "公开" },
            if s(c, "status") == "archived" { "已归档" } else { "" },
        ]
        .iter()
        .filter(|x| !x.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
        println!(
            "\n  {:<28} {:<12} {:<8} {:<7} v{:<4} {:<9} {}",
            s(c, "ref"),
            s(c, "kind"),
            s(c, "environment"),
            s(c, "format"),
            c["revision"].as_i64().unwrap_or(0),
            size_txt(c["size"].as_i64().unwrap_or(0)),
            flags
        );
        let name = s(c, "name");
        let summary = s(c, "summary");
        if !name.is_empty() || !summary.is_empty() {
            println!("      {name}  {summary}");
        }
        if !s(c, "checksum").is_empty() {
            println!("      sha256 {}", &s(c, "checksum")[..12.min(s(c, "checksum").len())]);
        }
    }
    println!("\n取内容：ncc registry config get @命名空间/slug --reveal");
    println!("整套拉取：ncc registry config bundle --ns @命名空间 --env prod --out ./conf");
    Ok(())
}

fn size_txt(n: i64) -> String {
    if n <= 0 {
        "—".to_string()
    } else if n < 1024 {
        format!("{n} B")
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}

/* ---------------- 读 ---------------- */

/// `ncc registry config get`
pub fn get(cfg: &CliConfig, a: &GetArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut path = config_path(&a.target);
    let mut qs: Vec<String> = Vec::new();
    if let Some(r) = a.revision {
        qs.push(format!("revision={r}"));
    }
    if a.reveal {
        qs.push("reveal=1".into());
    }
    if !qs.is_empty() {
        path = format!("{path}?{}", qs.join("&"));
    }
    let d = api::get(cfg, &path, t.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let c = &d["config"];
    if c.is_null() {
        bail!("配置不存在");
    }
    if c["masked"].as_bool().unwrap_or(false) || c["content"].is_null() {
        println!("{}（{}）", s(c, "ref"), s(c, "name"));
        println!(
            "  类型 {} · 环境 {} · 格式 {} · v{} · {} · {}",
            s(c, "kind"),
            s(c, "environment"),
            s(c, "format"),
            c["revision"].as_i64().unwrap_or(0),
            size_txt(c["size"].as_i64().unwrap_or(0)),
            if c["secret"].as_bool().unwrap_or(false) { "内容已加密" } else { "内容未下发" }
        );
        println!("  sha256 {}", s(c, "checksum"));
        if let Some(err) = c["error"].as_str() {
            println!("  ⚠ {err}");
        }
        println!("\n要明文加 --reveal（需要读权限）：ncc registry config get {} --reveal", a.target);
        return Ok(());
    }
    let content = s(c, "content");
    let rev_meta = &c["revisionMeta"];
    if let Some(out) = a.out.as_deref() {
        let p = PathBuf::from(out);
        if let Some(dir) = p.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;
            }
        }
        std::fs::write(&p, content).with_context(|| format!("写文件失败: {}", p.display()))?;
        println!(
            "✅ 已写入 {}（{} · v{} · {}）",
            p.display(),
            s(c, "ref"),
            c["revisionRequested"].as_i64().unwrap_or(c["revision"].as_i64().unwrap_or(0)),
            size_txt(c["size"].as_i64().unwrap_or(0))
        );
        println!("   sha256 {}", s(c, "checksum"));
        if !rev_meta.is_null() {
            println!("   版本说明 {}（{}）", s(rev_meta, "note"), s(rev_meta, "authorName"));
        }
        return Ok(());
    }
    // 直接打到 stdout（方便 | 到别的命令或写进 Agent 上下文）
    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
    Ok(())
}

/* ---------------- 写 ---------------- */

fn read_content(a: &SetArgs) -> Result<Option<String>> {
    if let Some(v) = a.value.as_deref() {
        return Ok(Some(v.to_string()));
    }
    if let Some(f) = a.file.as_deref() {
        let p = Path::new(f);
        let bytes = std::fs::read(p).with_context(|| format!("读文件失败: {f}"))?;
        let text = String::from_utf8(bytes).with_context(|| format!("{f} 不是 UTF-8 文本"))?;
        return Ok(Some(text));
    }
    // 没有 --file/--value：从 stdin 读（管道用法：cat x.yaml | ncc registry config set …）
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("读 stdin 失败")?;
        return Ok(Some(buf));
    }
    Ok(None)
}

/// `ncc registry config set` —— 有则改（加版本）、无则建。
pub fn set(cfg: &CliConfig, a: &SetArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let content = read_content(a)?;
    let ns = ns_of(&a.target);
    let slug = slug_of(&a.target);

    // 先看这份配置在不在：决定 POST（建）还是 PATCH（改）
    let existing = api::get(cfg, &config_path(&a.target), Some(&t)).ok();
    let exists = existing
        .as_ref()
        .map(|d| !d["config"].is_null())
        .unwrap_or(false);

    if !exists {
        let (ns, slug) = match (ns, slug) {
            (Some(ns), Some(slug)) => (ns, slug),
            _ => bail!("新建配置要用「@命名空间/slug」形式的引用（如 @team/network）"),
        };
        let content = content.unwrap_or_default();
        let fmt = a
            .format
            .clone()
            .or_else(|| a.file.as_deref().and_then(format_of_file))
            .unwrap_or_else(|| "text".to_string());
        let body = json!({
            "namespace": ns, "slug": slug,
            "name": a.name.clone().unwrap_or_default(),
            "kind": a.kind.clone().unwrap_or_default(),
            "environment": a.env.clone().unwrap_or_default(),
            "format": fmt,
            "summary": a.summary.clone().unwrap_or_default(),
            "tags": a.tags.as_deref().map(split_csv).unwrap_or_default(),
            "secret": a.secret,
            "visibility": if a.public { "public" } else { "" },
            "content": content,
            "note": a.note.clone().unwrap_or_default(),
        });
        let d = api::post_json(cfg, "/api/configs", Some(&t), &body)?;
        report_write(&d, true, a.json)?;
        warn_if_worker(cfg);
        return Ok(());
    }

    if content.is_none() && a.name.is_none() && a.summary.is_none() && a.tags.is_none() && a.kind.is_none() && a.env.is_none() && !a.public && !a.secret {
        bail!("没有要写入的内容或字段：用 --file / --value / stdin 给内容，或传 --tags/--summary 等元数据");
    }
    let mut body = serde_json::Map::new();
    if let Some(c) = content {
        body.insert("content".into(), json!(c));
    }
    if let Some(v) = a.name.as_deref() {
        body.insert("name".into(), json!(v));
    }
    if let Some(v) = a.kind.as_deref() {
        body.insert("kind".into(), json!(v));
    }
    if let Some(v) = a.env.as_deref() {
        body.insert("environment".into(), json!(v));
    }
    if let Some(v) = a.format.as_deref() {
        body.insert("format".into(), json!(v));
    }
    if let Some(v) = a.summary.as_deref() {
        body.insert("summary".into(), json!(v));
    }
    if let Some(v) = a.tags.as_deref() {
        body.insert("tags".into(), json!(split_csv(v)));
    }
    if a.public {
        body.insert("visibility".into(), json!("public"));
    }
    if a.secret {
        body.insert("secret".into(), json!(true));
    }
    if let Some(v) = a.note.as_deref() {
        body.insert("note".into(), json!(v));
    }
    let d = api::request(
        cfg,
        "PATCH",
        &config_path(&a.target),
        Some(&t),
        Some(&Value::Object(body)),
        None,
        &[],
    )?;
    report_write(&d, false, a.json)?;
    warn_if_worker(cfg);
    Ok(())
}

fn report_write(d: &Value, created: bool, json_out: bool) -> Result<()> {
    let c = &d["config"];
    if json_out {
        println!("{}", serde_json::to_string_pretty(d)?);
        return Ok(());
    }
    let action = if created { "✅ 已创建配置" } else if d["revisionAdded"].as_bool().unwrap_or(false) { "✅ 已更新（新版本）" } else { "✅ 已更新（仅元数据）" };
    println!(
        "{action} {}  v{}",
        s(c, "ref"),
        c["revision"].as_i64().unwrap_or(0)
    );
    println!(
        "   类型 {} · 环境 {} · 格式 {} · {} · {}",
        s(c, "kind"),
        s(c, "environment"),
        s(c, "format"),
        size_txt(c["size"].as_i64().unwrap_or(0)),
        if s(c, "visibility") == "public" { "公开" } else { "私有（默认）" }
    );
    if c["secret"].as_bool().unwrap_or(false) {
        println!("   敏感内容已静态加密（读取默认打码，--reveal 才出明文）");
    }
    println!("   sha256 {}", s(c, "checksum"));
    println!("\n给别的 Agent 用：");
    println!("  ncc registry config get {} --reveal", s(c, "ref"));
    if s(c, "visibility") != "public" {
        println!("  # 对方要先拿到读权限：ncc grant set --user @对方 --kind config");
    }
    Ok(())
}

/// `ncc registry config history`
pub fn history(cfg: &CliConfig, target: &str, json_out: bool) -> Result<()> {
    let t = config::token_opt(cfg);
    let d = api::get(cfg, &format!("{}/revisions", config_path(target)), t.as_deref())?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d["revisions"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("没有版本历史（配置不存在或不可读）。");
        return Ok(());
    }
    println!("{} 的版本历史（当前 v{}）", s(&d, "ref"), d["current"].as_i64().unwrap_or(0));
    for r in &rows {
        println!(
            "  v{:<5} {:<20} {:<9} {}",
            r["revision"].as_i64().unwrap_or(0),
            crate::nodes::human_time(s(r, "createdAt")),
            size_txt(r["size"].as_i64().unwrap_or(0)),
            if r["current"].as_bool().unwrap_or(false) { "← 当前" } else { "" }
        );
        if !s(r, "note").is_empty() {
            println!("        {}", s(r, "note"));
        }
        if !s(r, "checksum").is_empty() {
            println!("        sha256 {}", &s(r, "checksum")[..12]);
        }
    }
    println!("\n回滚：ncc registry config rollback {} --to <版本>", target);
    Ok(())
}

/// `ncc registry config rollback --to N`
pub fn rollback(cfg: &CliConfig, a: &RollbackArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let body = json!({ "revision": a.to, "note": a.note.clone().unwrap_or_default() });
    let d = api::post_json(cfg, &format!("{}/rollback", config_path(&a.target)), Some(&t), &body)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let c = &d["config"];
    println!(
        "✅ 已回滚 {} 到 v{}（作为新版本 v{} 写回，历史不改写）",
        s(c, "ref"),
        c["rolledBackTo"].as_i64().unwrap_or(a.to),
        c["revision"].as_i64().unwrap_or(0)
    );
    println!("   sha256 {}", s(c, "checksum"));
    Ok(())
}

/// `ncc registry config rm`
pub fn rm(cfg: &CliConfig, a: &RmArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    if !a.yes {
        bail!("删除会连同版本历史一起清掉：确认后加 --yes 重跑");
    }
    api::del(cfg, &config_path(&a.target), Some(&t))?;
    println!("✅ 已删除配置 {}（含版本历史）", a.target);
    Ok(())
}

/* ---------------- bundle：成组拉取 ---------------- */

/// `ncc registry config bundle --ns @team [--env prod] [--out DIR]`
pub fn bundle(cfg: &CliConfig, a: &BundleArgs) -> Result<()> {
    let t = config::require_token(cfg)
        .context("拉取整套配置需要凭据（先 ncc login，或带 API Key / 节点令牌）")?;
    let ns = a.namespace.trim().trim_start_matches('@');
    let mut qs = vec![format!("namespace={}", urlenc(ns))];
    for (k, v) in [("env", a.env.clone()), ("kind", a.kind.clone()), ("tag", a.tag.clone())] {
        if let Some(x) = v.filter(|x| !x.is_empty()) {
            qs.push(format!("{k}={}", urlenc(&x)));
        }
    }
    if a.secrets {
        qs.push("secrets=1".into());
    }
    if a.reveal {
        qs.push("reveal=1".into());
    }
    let d = api::get(cfg, &format!("/api/configs/bundle?{}", qs.join("&")), Some(&t))?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d["configs"].as_array().cloned().unwrap_or_default();
    let ns_label = s(&d["namespace"], "slug");
    if rows.is_empty() {
        println!("{ns_label} 下没有符合条件的配置（环境 {}）。", a.env.clone().unwrap_or_else(|| "any".into()));
        return Ok(());
    }
    println!(
        "{ns_label} 的配置 {} 条（环境 {}；{}）",
        rows.len(),
        a.env.clone().unwrap_or_else(|| "全部".into()),
        if a.reveal { "含明文" } else { "只含元数据（加 --reveal 取明文）" }
    );
    let dir = a.out.as_deref().map(PathBuf::from);
    if let Some(dir) = dir.as_ref() {
        std::fs::create_dir_all(dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;
    }
    for c in &rows {
        let fname = s(c, "filename");
        let flags = [
            if c["secret"].as_bool().unwrap_or(false) { "敏感" } else { "" },
            if c["masked"].as_bool().unwrap_or(false) { "打码" } else { "" },
        ]
        .iter()
        .filter(|x| !x.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
        println!(
            "\n  {:<28} {:<12} {:<8} v{:<4} {}",
            s(c, "ref"),
            s(c, "kind"),
            s(c, "format"),
            c["revision"].as_i64().unwrap_or(0),
            flags
        );
        if let Some(dir) = dir.as_ref() {
            let target = dir.join(if fname.is_empty() { format!("{}.txt", s(c, "slug")) } else { fname.to_string() });
            let content = if c["content"].is_null() { "" } else { s(c, "content") };
            std::fs::write(&target, content)
                .with_context(|| format!("写文件失败: {}", target.display()))?;
            println!("      → {}", target.display());
        }
    }
    println!(
        "\nsha256 汇总：{}",
        rows.iter()
            .map(|c| format!("{} {}", s(c, "ref"), &s(c, "checksum")[..12.min(s(c, "checksum").len())]))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    if !a.reveal {
        println!("要落盘明文内容：重跑并加 --reveal");
    }
    Ok(())
}

/* ---------------- 供 MCP 复用（只读） ---------------- */

pub fn fetch_configs(cfg: &CliConfig, namespace: &str, kind: &str, env: &str, mine: bool) -> Result<Value> {
    let t = config::token_opt(cfg);
    let mut qs: Vec<String> = Vec::new();
    if !namespace.is_empty() {
        qs.push(format!("namespace={}", urlenc(namespace.trim_start_matches('@'))));
    }
    if !kind.is_empty() {
        qs.push(format!("kind={}", urlenc(kind)));
    }
    if !env.is_empty() {
        qs.push(format!("env={}", urlenc(env)));
    }
    if mine {
        qs.push("mine=1".into());
    }
    let path = if qs.is_empty() { "/api/configs".to_string() } else { format!("/api/configs?{}", qs.join("&")) };
    api::get(cfg, &path, t.as_deref())
}

pub fn fetch_config(cfg: &CliConfig, target: &str, reveal: bool, revision: Option<i64>) -> Result<Value> {
    let t = config::token_opt(cfg);
    let mut qs: Vec<String> = Vec::new();
    if reveal {
        qs.push("reveal=1".into());
    }
    if let Some(r) = revision {
        qs.push(format!("revision={r}"));
    }
    let path = if qs.is_empty() { config_path(target) } else { format!("{}?{}", config_path(target), qs.join("&")) };
    api::get(cfg, &path, t.as_deref())
}

/// 渲染成 Agent 可读文本（配置目录）。
pub fn render_configs(v: &Value) -> String {
    let rows = v["configs"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "没有符合条件的配置（公开配置匿名可见；自己命名空间的加 mine=true）。".to_string();
    }
    let mut out = format!("托管配置 {} 条（内容默认打码）：\n", rows.len());
    for c in &rows {
        out.push_str(&format!(
            "\n· {}（{} · {} · {} · v{}）\n",
            s(c, "ref"),
            s(c, "kind"),
            s(c, "environment"),
            s(c, "format"),
            c["revision"].as_i64().unwrap_or(0)
        ));
        if !s(c, "name").is_empty() {
            out.push_str(&format!("  {}\n", s(c, "name")));
        }
        if !s(c, "summary").is_empty() {
            out.push_str(&format!("  {}\n", s(c, "summary")));
        }
        out.push_str(&format!(
            "  {}{} · sha256 {}…\n",
            if s(c, "visibility") == "public" { "公开" } else { "私有" },
            if c["secret"].as_bool().unwrap_or(false) { " · 敏感（静态加密）" } else { "" },
            &s(c, "checksum")[..12.min(s(c, "checksum").len())]
        ));
    }
    out.push_str("\n取内容用 ncc_get_config（需要读权限；敏感配置要 reveal=true）。");
    out
}

/// 渲染成 Agent 可读文本（单份配置）。
pub fn render_config(v: &Value) -> String {
    let c = &v["config"];
    if c.is_null() {
        return "配置不存在或不可读。".to_string();
    }
    let mut out = format!(
        "{}（{} · {} · {} · v{}）\n{}\n",
        s(c, "ref"),
        s(c, "kind"),
        s(c, "environment"),
        s(c, "format"),
        c["revision"].as_i64().unwrap_or(0),
        if s(c, "name").is_empty() { s(c, "summary") } else { s(c, "name") }
    );
    out.push_str(&format!("sha256 {}\n", s(c, "checksum")));
    if c["masked"].as_bool().unwrap_or(false) || c["content"].is_null() {
        out.push_str("内容未下发（默认打码）。需要明文请用 reveal=true 再取一次。\n");
        if let Some(err) = c["error"].as_str() {
            out.push_str(&format!("注意：{err}\n"));
        }
        return out;
    }
    out.push_str("--- 内容开始 ---\n");
    out.push_str(s(c, "content"));
    if !s(c, "content").ends_with('\n') {
        out.push('\n');
    }
    out.push_str("--- 内容结束 ---\n");
    out
}
