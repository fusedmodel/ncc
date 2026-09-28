// ncc kb / ncc mem / ncc ckpt：Agent 的三样**状态**（知识库 / 记忆 / 检查点）。
//
//      ncc kb   ls / get / set / rm / archive / restore / history / search / bundle / pull / kinds
//      ncc mem  ls / get / set / rm / gc / kinds
//      ncc ckpt ls / show / save / pull / lineage / prune / rm / kinds
//
// ## 为什么这三样**不是** HUR 包（这是本文件真正的设计决定）
//
// 包 = **能力**：代码 + 清单 + 确定性字节 + 签名，"装上就能跑"，内容寻址、可分发、可复核。
// 状态 = **数据**：会被改写、会长大、默认私有、生命周期跟着 Agent 走 —— 一天改十次的东西
// 没有"版本"的意义，给它签名只会把签名变成噪声（签的是"昨天那份 KB"）。
// 把 KB 打成包会立刻自相矛盾：包要求字节不变（digest 证明"没被改"），而 KB 天经地义要变。
//
// 所以分工是：
//   · 包在 `hur.json` 的 `state{}` 里**声明**自己要哪几样（规则 R11）——
//     于是"这个 Agent 需要哪些知识库 / 记忆 / 检查点"本身是**可签名、可分发、可复核**的；
//   · 字节住在节点上（`ncc-registry` 的 kb_docs / mem_entries / checkpoints），
//     由本文件这套命令读写。
// `ncc kb pull` 就是两者的接缝：**读包的声明，去节点取它要的东西**。
//
// ## 权限边界（与服务端一致，别在这里另立一套）
//
//   · 读：公开 KB 谁都能读；非公开要 `kb:read` + 命名空间成员或 `state` 授权。
//     记忆与检查点**没有公开档**（记忆是私人状态）—— 只有成员或被授权者能读。
//   · 写：永远要在命名空间里（被授权者只有读）。
//   · 三样共用一个授权种类 `state`（细粒度取舍见 PRD）。
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use hur_core::{datapack, spec};

use crate::api;
use crate::config::{self, CliConfig};

/* ============================ 词表与上限 ============================ */

/// 与服务端 `model/state.go` 同一套字（本地校验一次，省一次往返；服务端仍会再校验）。
pub const KB_KINDS: [&str; 5] = ["doc", "faq", "notes", "spec", "transcript"];
pub const KB_FORMATS: [&str; 4] = ["markdown", "text", "json", "yaml"];
pub const MEM_KINDS: [&str; 5] = ["fact", "preference", "episode", "summary", "pointer"];
pub const CKPT_LABELS: [&str; 6] = ["episode", "step", "run", "release", "handoff", "manual"];

pub const MAX_KB_BYTES: usize = 1 << 20;
pub const MAX_MEM_BYTES: usize = 64 * 1024;
pub const MAX_CKPT_BYTES: u64 = 512 << 20;

/// 本地 KB 缓存的索引格式（`~/.ncc/kb/index.json`）。
///
/// 缓存要能**增量同步**：按 checksum 判断"这条变了没有"，而不是每次全量重写。
/// 索引里记住 checksum / revision，`pull` 就能只写变了的那几篇。
pub const KB_CACHE_SPEC: &str = "ncc-kb-cache/v1";

/* ============================ 通用小工具 ============================ */

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn arr(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// `@命名空间/slug` → (命名空间, slug)。给 `kb set` / `ckpt save --ref` 用。
fn split_ref(target: &str) -> (String, String) {
    let t = target.trim().trim_start_matches('@');
    let mut parts = t.splitn(2, '/');
    let ns = parts.next().unwrap_or("").trim().to_string();
    let slug = parts.next().unwrap_or("").trim().to_string();
    if slug.is_empty() {
        (String::new(), ns)
    } else {
        (ns, slug)
    }
}

/// 直接拼进路径的引用：**不要 urlenc** —— 斜杠是服务端路由的分隔符
/// （`/:id/:slug`），编码成 `%2F` 之后服务端只看到一个 id，会找不到东西。
fn ref_path(prefix: &str, target: &str) -> String {
    format!("{prefix}/{}", target.trim())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let d = h.finalize();
    let mut out = String::with_capacity(64);
    for b in d {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn size_txt(n: i64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f < KB {
        format!("{n} B")
    } else if f < KB * KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{:.1} MB", f / (KB * KB))
    }
}

/// 内容里的短摘要（列表里只给一眼看得懂的量）。按**字符**截断，别切坏 UTF-8。
fn clip(s: &str, n: usize) -> String {
    let one_line = s.replace('\n', " ");
    let mut out: String = one_line.chars().take(n).collect();
    if one_line.chars().count() > n {
        out.push('…');
    }
    out
}

fn json_out(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// `--namespace` 归一化：允许写 `@team` 或 `team`（服务端两种都认）。
fn ns_arg(v: &Option<String>) -> String {
    v.as_deref().unwrap_or("").trim().trim_start_matches('@').to_string()
}

/// 三样状态共用的可见范围查询串（`--all` 只有管理员能生效，服务端把关）。
fn scope_qs(all: bool, mine: bool) -> Vec<String> {
    let mut qs = Vec::new();
    if all {
        qs.push("all=1".into());
    }
    if mine {
        qs.push("mine=1".into());
    }
    qs
}

/* ============================ 知识库 kb ============================ */

#[derive(clap::Subcommand)]
pub enum KbAction {
    /// 列出文档（默认：我可见的；--all 管理员看全节点）
    Ls(KbLsArgs),
    /// 看一篇文档（含正文；`--revision N` 取历史版本）
    Get(KbGetArgs),
    /// 写一篇（存在即新版本：内容进库 + 追加一版历史）
    Set(KbSetArgs),
    /// 全文检索（服务端按标题/摘要/正文加权打分）
    Search(KbSearchArgs),
    /// 版本历史（只给"谁在什么时候改了什么"，不含正文）
    History(KbHistoryArgs),
    /// 归档 / 恢复（归档的默认不出现在列表与检索里）
    Archive(KbRefArgs),
    Restore(KbRefArgs),
    /// 删除（连同历史；KB 是数据不是审计，归属者有权删）
    Rm(KbRefArgs),
    /// 把一个命名空间的库打包拉下来（Agent 落地第一步）
    Bundle(KbBundleArgs),
    /// 按**包的声明**拉知识库：读 hur.json 的 `state.kb`，去节点取它要的库（增量）
    Pull(KbPullArgs),
    /// 文档类型 / 格式 / 上限（离线也能看：先报本地词表，取得到服务端就用服务端的）
    Kinds(KbKindsArgs),
}

#[derive(clap::Args)]
pub struct KbLsArgs {
    /// 只看某个命名空间（@team 或 team）
    #[arg(long)]
    pub namespace: Option<String>,
    /// 只看某种类型
    #[arg(long, value_parser = ["doc", "faq", "notes", "spec", "transcript"])]
    pub kind: Option<String>,
    /// 只看带某个标签的
    #[arg(long)]
    pub tag: Option<String>,
    /// 关键词（走服务端检索；等价于 `ncc kb search`）
    #[arg(long, short = 'q')]
    pub query: Option<String>,
    /// 连归档的一起列
    #[arg(long)]
    pub archived: bool,
    /// 只看我自己的（不给授权范围）
    #[arg(long)]
    pub mine: bool,
    /// 管理员：看整个节点（服务端要求 `all=1` + 管理员凭据）
    #[arg(long)]
    pub all: bool,
    #[arg(long, default_value = "50")]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbGetArgs {
    /// 引用：`@命名空间/slug` | `KD-…` | slug
    pub reference: String,
    /// 取历史版本（默认最新）
    #[arg(long)]
    pub revision: Option<i64>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbSetArgs {
    /// slug（不给则从 --title 生成；与 --namespace 一起决定引用 `@ns/slug`）
    pub slug: Option<String>,
    #[arg(long)]
    pub title: String,
    /// 目标命名空间（默认：你的个人命名空间）
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long, default_value = "doc", value_parser = ["doc", "faq", "notes", "spec", "transcript"])]
    pub kind: String,
    #[arg(long, default_value = "markdown", value_parser = ["markdown", "text", "json", "yaml"])]
    pub format: String,
    /// 一句话摘要（列表与检索里先看到的就是它，值得认真写）
    #[arg(long)]
    pub summary: Option<String>,
    #[arg(long = "tag", value_delimiter = ',')]
    pub tags: Vec<String>,
    /// 这篇知识从哪来（url / traceId / ckpt 引用）—— 知识可追溯才谈得上来路
    #[arg(long)]
    pub source: Option<String>,
    /// 本次改动的说明（进版本历史）
    #[arg(long)]
    pub note: Option<String>,
    /// 公开可读（默认私有；公开的谁都能读）
    #[arg(long)]
    pub public: bool,
    /// 正文从文件读
    #[arg(long)]
    pub file: Option<String>,
    /// 正文直接给（不方便用文件时）
    #[arg(long)]
    pub content: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbSearchArgs {
    /// 关键词（多个词用空格分开，服务端按词加分）
    pub query: String,
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long, value_parser = ["doc", "faq", "notes", "spec", "transcript"])]
    pub kind: Option<String>,
    #[arg(long, default_value = "20")]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbHistoryArgs {
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbRefArgs {
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbBundleArgs {
    /// 哪个命名空间（必填：库是按命名空间组织的）
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long, value_parser = ["doc", "faq", "notes", "spec", "transcript"])]
    pub kind: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    /// 落到本地目录（默认只打印）
    #[arg(long)]
    pub out: Option<String>,
    /// 打成一份**快照包**（目录）而不是散文件：profile=kb-seed，可签名/可发布/可灌回节点
    #[arg(long, value_name = "目录")]
    pub as_package: Option<String>,
    /// 快照的隐私级别（public | internal | private）—— 导入方按它决定能不能进公开档
    #[arg(long, default_value = "internal", value_parser = ["public", "internal", "private"])]
    pub privacy: String,
    /// 快照的许可声明（未声明就不该往外发；只提醒，不拦）
    #[arg(long, default_value = "")]
    pub license: String,
    /// 快照包的名字（默认「<命名空间> 知识库快照」）
    #[arg(long, default_value = "")]
    pub name: String,
    /// 快照包的版本（语义化版本；快照时刻记在 data.snapshot_at 里）
    #[arg(long, default_value = "")]
    pub version: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MemExportArgs {
    /// 打到哪个命名空间（默认当前用户的个人库）
    #[arg(long)]
    pub namespace: Option<String>,
    /// 谁的记忆（不指定就导出你能读到的全部）
    #[arg(long)]
    pub subject: Option<String>,
    /// 只看这个前缀的键
    #[arg(long)]
    pub prefix: Option<String>,
    /// 打成快照包的目录（必填：记忆快照只能以包的形式出门）
    #[arg(long, value_name = "目录")]
    pub as_package: String,
    /// **默认 private**：记忆是私密的，要公开得自己改（而且得说得清为什么）
    #[arg(long, default_value = "private", value_parser = ["public", "internal", "private"])]
    pub privacy: String,
    #[arg(long, default_value = "")]
    pub license: String,
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long, default_value = "")]
    pub version: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptExportArgs {
    /// 只导某个制品血缘上的点（`@命名空间/slug`）
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// 只导某一个点（id 或名字）
    #[arg(long)]
    pub id: Option<String>,
    /// 打到快照包的目录（必填）
    #[arg(long, value_name = "目录")]
    pub as_package: String,
    #[arg(long, default_value = "internal", value_parser = ["public", "internal", "private"])]
    pub privacy: String,
    #[arg(long, default_value = "")]
    pub license: String,
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long, default_value = "")]
    pub version: String,
    /// 最多导几个点（默认 5，避免一不小导出几十 GB）
    #[arg(long, default_value_t = 5)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbPullArgs {
    /// 包目录（含 hur.json），或直接给 hur.json 的路径
    #[arg(long)]
    pub package: String,
    /// 包里有 `*`（"任意可读的库"）时，用它兜底成具体命名空间
    #[arg(long)]
    pub namespace: Option<String>,
    /// 缓存目录（默认 ~/.ncc/kb）
    #[arg(long)]
    pub out: Option<String>,
    /// 只报要拉什么，不写盘
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct KbKindsArgs {
    #[arg(long)]
    pub json: bool,
}

impl KbAction {
    /// 命令 → 需要的能力。`kinds` 不要（离线也给本地词表）。
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            KbAction::Kinds(_) => None,
            _ => Some("kb"),
        }
    }
}

pub fn run_kb(cfg: &CliConfig, action: &KbAction) -> Result<()> {
    match action {
        KbAction::Ls(a) => kb_ls(cfg, a),
        KbAction::Get(a) => kb_get(cfg, a),
        KbAction::Set(a) => kb_set(cfg, a),
        KbAction::Search(a) => kb_search(cfg, a),
        KbAction::History(a) => kb_history(cfg, a),
        KbAction::Archive(a) => kb_status(cfg, &a.reference, "archived", a.json),
        KbAction::Restore(a) => kb_status(cfg, &a.reference, "active", a.json),
        KbAction::Rm(a) => kb_rm(cfg, a),
        KbAction::Bundle(a) => kb_bundle(cfg, a),
        KbAction::Pull(a) => kb_pull(cfg, a),
        KbAction::Kinds(a) => kb_kinds(cfg, a.json),
    }
}

fn kb_ls(cfg: &CliConfig, a: &KbLsArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs = scope_qs(a.all, a.mine);
    let ns = ns_arg(&a.namespace);
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(&ns)));
    }
    if let Some(k) = &a.kind {
        qs.push(format!("kind={k}"));
    }
    if let Some(tag) = &a.tag {
        qs.push(format!("tag={}", api::urlenc(tag)));
    }
    if let Some(q) = &a.query {
        qs.push(format!("q={}", api::urlenc(q)));
    }
    if a.archived {
        qs.push("archived=1".into());
    }
    qs.push(format!("size={}", a.limit.clamp(1, 200)));
    let q = if qs.is_empty() { String::new() } else { format!("?{}", qs.join("&")) };
    let d = api::get(cfg, &format!("/api/kb{q}"), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    render_kb_list(&d);
    Ok(())
}

fn render_kb_list(d: &Value) {
    let docs = d["docs"].as_array().cloned().unwrap_or_default();
    if docs.is_empty() {
        println!("（没有匹配的文档）");
        println!(
            "  可见范围 {}：默认只看**我的 + 被授权给我的**；公开文档谁都能读。",
            s(d, "scope")
        );
        return;
    }
    println!(
        "{:<28} {:<8} {:>9} {:<5} {}",
        "引用", "类型", "大小", "版本", "标题"
    );
    for x in &docs {
        println!(
            "{:<28} {:<8} {:>9} {:<5} {}",
            clip(s(x, "ref"), 28),
            s(x, "kind"),
            size_txt(i(x, "size")),
            format!("v{}", i(x, "revision")),
            clip(s(x, "title"), 46),
        );
        let sm = s(x, "summary");
        if !sm.is_empty() {
            println!("{:<28} {}", "", clip(sm, 70));
        }
    }
    println!(
        "\n共 {} 条（范围 {}）· 看全文 `ncc kb get <引用>` · 检索 `ncc kb search <词>`",
        d["total"],
        s(d, "scope")
    );
}

fn kb_get(cfg: &CliConfig, a: &KbGetArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut q = String::new();
    if let Some(r) = a.revision {
        q = format!("?revision={r}");
    }
    let d = api::get(cfg, &format!("{}{q}", ref_path("/api/kb", &a.reference)), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    println!("{}  {}", s(&d, "ref"), s(&d, "title"));
    println!(
        "  类型 {} · 格式 {} · 大小 {} · 版本 v{} · {}",
        s(&d, "kind"),
        s(&d, "format"),
        size_txt(i(&d, "size")),
        i(&d, "revision"),
        if s(&d, "visibility") == "public" { "公开" } else { "私有" }
    );
    let tags = arr(d.get("tags"));
    if !tags.is_empty() {
        println!("  标签 {}", tags.join(", "));
    }
    let src = s(&d, "source");
    if !src.is_empty() {
        println!("  来源 {src}");
    }
    println!("  摘要 {}", s(&d, "summary"));
    println!("  校验 {}", s(&d, "checksum"));
    println!("\n{}", s(&d, "content"));
    Ok(())
}

fn kb_set(cfg: &CliConfig, a: &KbSetArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    // 正文来源：--file > --content > stdin（管道里给也算）。
    let content = match (&a.file, &a.content) {
        (Some(p), _) => std::fs::read_to_string(p).with_context(|| format!("读不了 {p}"))?,
        (None, Some(c)) => c.clone(),
        (None, None) => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("读 stdin 失败")?;
            s
        }
    };
    if content.trim().is_empty() {
        bail!("正文是空的（用 --file / --content，或从 stdin 管道进来）");
    }
    if content.len() > MAX_KB_BYTES {
        bail!(
            "正文 {} 字节，超过上限 {} —— 大文件请走制品（`ncc publish`），KB 是**语料**不是大文件",
            content.len(),
            MAX_KB_BYTES
        );
    }
    let (ns_from_ref, slug_from_ref) = match &a.slug {
        Some(x) if x.contains('/') => split_ref(x),
        Some(x) => (String::new(), x.clone()),
        None => (String::new(), String::new()),
    };
    let ns = {
        let explicit = ns_arg(&a.namespace);
        if !explicit.is_empty() {
            explicit
        } else {
            ns_from_ref
        }
    };
    let mut body = json!({
        "namespace": ns,
        "slug": slug_from_ref,
        "title": a.title,
        "kind": a.kind,
        "format": a.format,
        "content": content,
        "tags": a.tags,
        "visibility": if a.public { "public" } else { "private" },
    });
    if let Some(x) = &a.summary {
        body["summary"] = json!(x);
    }
    if let Some(x) = &a.source {
        body["source"] = json!(x);
    }
    if let Some(x) = &a.note {
        body["note"] = json!(x);
    }
    let d = api::post_json(cfg, "/api/kb", Some(&t), &body)?;
    if a.json {
        return json_out(&d);
    }
    let created = d["created"].as_bool().unwrap_or(false);
    println!(
        "{} {}  v{}",
        if created { "✅ 新建" } else { "✅ 更新" },
        s(&d, "ref"),
        i(&d, "revision")
    );
    println!(
        "  校验 {} · 本地副本 `ncc kb get {}`",
        s(&d["doc"], "checksum"),
        s(&d, "ref")
    );
    Ok(())
}

fn kb_search(cfg: &CliConfig, a: &KbSearchArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs = vec![
        format!("q={}", api::urlenc(&a.query)),
        format!("size={}", a.limit.clamp(1, 200)),
    ];
    let ns = ns_arg(&a.namespace);
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(&ns)));
    }
    if let Some(k) = &a.kind {
        qs.push(format!("kind={k}"));
    }
    let d = api::get(cfg, &format!("/api/kb?{}", qs.join("&")), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    println!("检索「{}」（关键词加权：标题 3 / 摘要 2 / 正文 1）", a.query);
    render_kb_list(&d);
    println!("\n⚠ 这是**关键词**检索，不是向量检索 —— 换个说法就不一定命中。");
    Ok(())
}

fn kb_history(cfg: &CliConfig, a: &KbHistoryArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let d = api::get(
        cfg,
        &ref_path("/api/kb", &format!("{}/revisions", a.reference.trim_matches('/'))),
        t.as_deref(),
    )?;
    if a.json {
        return json_out(&d);
    }
    println!("{} 版本历史（当前 v{}）", s(&d, "ref"), i(&d, "revision"));
    for r in d["revisions"].as_array().cloned().unwrap_or_default() {
        println!(
            "  v{:<4} {:<20} {:>9}  {}  {}",
            i(&r, "revision"),
            clip(s(&r, "at"), 20),
            size_txt(i(&r, "size")),
            clip(s(&r, "author"), 20),
            s(&r, "note")
        );
    }
    println!("\n取某一版正文：`ncc kb get {} --revision N`", s(&d, "ref"));
    Ok(())
}

fn kb_status(cfg: &CliConfig, reference: &str, status: &str, json_flag: bool) -> Result<()> {
    let t = config::require_token(cfg)?;
    let d = api::request(
        cfg,
        "PATCH",
        &ref_path("/api/kb", reference),
        Some(&t),
        Some(&json!({ "status": status })),
        None,
        &[],
    )?;
    if json_flag {
        return json_out(&d);
    }
    println!(
        "✅ {} → {}",
        s(&d["doc"], "ref"),
        if status == "archived" { "已归档（默认不出现在列表与检索里）" } else { "已恢复" }
    );
    Ok(())
}

fn kb_rm(cfg: &CliConfig, a: &KbRefArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let d = api::del(cfg, &ref_path("/api/kb", &a.reference), Some(&t))?;
    if a.json {
        return json_out(&d);
    }
    println!("✅ 删除 {}", s(&d, "ref"));
    Ok(())
}

fn kb_bundle(cfg: &CliConfig, a: &KbBundleArgs) -> Result<()> {
    let ns = ns_arg(&a.namespace);
    if ns.is_empty() {
        bail!("缺 --namespace（要拉哪个命名空间的库，如 --namespace @team）");
    }
    let t = config::token_opt(cfg);
    let mut qs = vec![format!("namespace={}", api::urlenc(&ns))];
    if let Some(k) = &a.kind {
        qs.push(format!("kind={k}"));
    }
    if let Some(tag) = &a.tag {
        qs.push(format!("tag={}", api::urlenc(tag)));
    }
    let d = api::get(cfg, &format!("/api/kb/bundle?{}", qs.join("&")), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    let docs = d["docs"].as_array().cloned().unwrap_or_default();
    if a.as_package.is_some() {
        return kb_as_package(cfg, a, &ns, &docs);
    }
    println!("@{ns} 的知识库：{} 篇", d["count"]);
    if let Some(dir) = &a.out {
        let root = PathBuf::from(dir);
        let report = write_kb_docs(&root, &docs, None, false)?;
        println!("{}", report.render(&root));
        return Ok(());
    }
    for x in &docs {
        println!(
            "  {:<30} {:<8} {:>9}  {}",
            clip(s(x, "ref"), 30),
            s(x, "kind"),
            size_txt(i(x, "size")),
            clip(s(x, "title"), 40)
        );
    }
    println!("\n落盘：`ncc kb bundle --namespace @{ns} --out ./kb`");
    Ok(())
}

/* ---------------- 数据快照包：kb / mem / ckpt 三个导出面 ---------------- */

/// 从文档记录里取字段（服务端返回的对象字段名不完全统一，容错取一次）。
pub(crate) fn dstr(d: &Value, k: &str) -> String {
    d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// 生成一份快照包并打印下一步。三个导出面共用（**同一套封装**，不写三遍）。
pub(crate) fn write_seed(
    profile: &str,
    dir: &str,
    name: &str,
    version: &str,
    summary: &str,
    source: &str,
    privacy: &str,
    license: &str,
    payload: Option<&str>,
    docs: Vec<(spec::DataDoc, Vec<u8>)>,
    json_out: bool,
) -> Result<()> {
    let target = config::load().current_name();
    let input = datapack::SeedInput {
        profile: profile.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        summary: summary.to_string(),
        source: source.to_string(),
        source_target: target,
        // 快照时刻用本机时间（RFC3339）——"什么时候的数据"是这份声明的一半价值
        snapshot_at: crate::gateway::now_iso(),
        privacy: privacy.to_string(),
        license: license.to_string(),
        payload: payload.map(str::to_string),
        note: String::new(),
    };
    let out = PathBuf::from(dir);
    let pkg = datapack::write(&out, &input, docs)?;
    let n = pkg.data.as_ref().map(|d| d.docs.len()).unwrap_or(0);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dir": out, "package": pkg, "docs": n,
                "next": format!("ncc hur verify {dir} → ncc hur sign {dir} → ncc hur publish --file {dir}/dist/*.hur.gz"),
            }))?
        );
        return Ok(());
    }
    println!("✅ 快照包已生成 {}", out.display());
    println!("   {}", datapack::describe(&pkg));
    println!("   {n} 份文件 → {} 下，清单 hur.json、锁 hur.lock 都在", spec::DATA_DIR);
    println!("   下一步   ncc hur verify {} · ncc hur sign {} · ncc hur publish --file …", out.display(), out.display());
    println!("   别人取用 ncc hur data import --package {} --apply", out.display());
    Ok(())
}

/// `ncc kb bundle --as-package`：知识库 → 快照包。
fn kb_as_package(cfg: &CliConfig, a: &KbBundleArgs, ns: &str, docs: &[Value]) -> Result<()> {
    let dir = a.as_package.clone().unwrap_or_default();
    let name = if a.name.trim().is_empty() {
        format!("@{ns} 知识库快照")
    } else {
        a.name.clone()
    };
    let mut out: Vec<(spec::DataDoc, Vec<u8>)> = Vec::new();
    for d in docs {
        let slug = dstr(d, "slug");
        if slug.is_empty() {
            continue;
        }
        let format = {
            let f = dstr(d, "format");
            if f.is_empty() { "markdown".to_string() } else { f }
        };
        let path = format!("{}{}.{}", spec::DATA_DIR, slug, kb_ext(&format));
        out.push((
            spec::DataDoc {
                path,
                slug,
                title: dstr(d, "title"),
                kind: dstr(d, "kind"),
                format,
                tags: d
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|v| v.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                    .unwrap_or_default(),
                visibility: dstr(d, "visibility"),
                summary: dstr(d, "summary"),
                ..Default::default()
            },
            dstr(d, "content").into_bytes(),
        ));
    }
    if out.is_empty() {
        bail!("@{ns} 里没有可打包的文档（空快照说明不了任何事）");
    }
    let _ = cfg;
    write_seed(
        "kb-seed",
        &dir,
        &name,
        &a.version,
        &format!("@{ns} 的知识库快照（{} 篇）", out.len()),
        &format!("@{ns}/*"),
        &a.privacy,
        &a.license,
        None,
        out,
        a.json,
    )
}

/// `ncc mem export --as-package`：记忆 → 快照包。
///
/// 默认 **private**：记忆是私密的。要把它变成 public 得自己显式改，
/// 而且改之前值得想清楚"这份记忆凭什么能公开"。
fn mem_export(cfg: &CliConfig, a: &MemExportArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let ns = ns_arg(&a.namespace);
    let mut qs: Vec<String> = scope_qs(true, true);
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(&ns)));
    }
    if let Some(s) = &a.subject {
        qs.push(format!("subject={}", api::urlenc(s)));
    }
    if let Some(p) = &a.prefix {
        qs.push(format!("prefix={}", api::urlenc(p)));
    }
    qs.push("limit=1000".into());
    let d = api::get(cfg, &format!("/api/mem?{}", qs.join("&")), t.as_deref())?;
    let rows = d["memories"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        bail!("没有可导出的记忆（记忆快照不导空集）");
    }
    let mut docs: Vec<(spec::DataDoc, Vec<u8>)> = Vec::new();
    for m in &rows {
        if m["expired"].as_bool().unwrap_or(false) {
            continue;
        }
        let subject = dstr(m, "subject");
        let key = dstr(m, "key");
        if key.is_empty() {
            continue;
        }
        let safe = |s: &str| s.replace(['/', '\\', ' '], "_");
        let path = format!("{}{}__{}.txt", spec::DATA_DIR, safe(&subject), safe(&key));
        docs.push((
            spec::DataDoc {
                path,
                subject,
                key,
                kind: dstr(m, "kind"),
                tags: m
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|v| v.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                    .unwrap_or_default(),
                visibility: "private".into(),
                summary: dstr(m, "source"),
                ..Default::default()
            },
            dstr(m, "value").into_bytes(),
        ));
    }
    if docs.is_empty() {
        bail!("读到的记忆全是过期的 —— 快照里没有任何一份可导出的条目");
    }
    let name = if a.name.trim().is_empty() {
        format!("{} 记忆快照", if ns.is_empty() { "我的".to_string() } else { format!("@{ns}") })
    } else {
        a.name.clone()
    };
    let n = docs.len();
    write_seed(
        "mem-seed",
        &a.as_package,
        &name,
        &a.version,
        &format!("记忆快照（{n} 条）"),
        &if ns.is_empty() { "local".to_string() } else { format!("@{ns}/*") },
        &a.privacy,
        &a.license,
        None,
        docs,
        a.json,
    )
}

/// `ncc ckpt export --as-package`：检查点 → 快照包（字节 + 血缘，逐个核摘要）。
fn ckpt_export(cfg: &CliConfig, a: &CkptExportArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs: Vec<String> = vec![format!("limit={}", a.limit.clamp(1, 50))];
    if let Some(r) = &a.reference {
        qs.push(format!("ref={}", api::urlenc(r)));
    }
    let d = api::get(cfg, &format!("/api/ckpt?{}", qs.join("&")), t.as_deref())?;
    let mut rows = d["checkpoints"].as_array().cloned().unwrap_or_default();
    if let Some(want) = a.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Ok(one) = api::get(cfg, &ref_path("/api/ckpt", want), t.as_deref()) {
            rows = vec![one["checkpoint"].clone()];
        }
    }
    if rows.is_empty() {
        bail!("没有可导出的检查点（用 --ref 或 --id 指定范围）");
    }
    let mut docs: Vec<(spec::DataDoc, Vec<u8>)> = Vec::new();
    for row in &rows {
        if dstr(row, "status") != "active" {
            continue; // 已 pruned 的字节服务端已经删了，别假装还在
        }
        let id = dstr(row, "id");
        // 列表行**不带** bytesUrl（短时地址是 show 才签发的），所以逐个取一次详情 ——
        // 与 `ncc ckpt pull` 同一条路。列表里的字段也一并换成详情里的（更全）。
        let one = api::get(cfg, &ref_path("/api/ckpt", &id), t.as_deref())?;
        let c = if one.get("checkpoint").is_some() { &one["checkpoint"] } else { row };
        let bytes = match c.get("bytesUrl").and_then(|x| x.as_str()) {
            Some(u) => api::get_bytes(cfg, u, None)?,
            None => mismatch_or_bytes(cfg, &id, t.as_deref())?,
        };
        if bytes.is_empty() {
            continue; // 只有元数据没有字节的点导不出去
        }
        // **落进包之前先核摘要**：检查点的全部价值就是"拿回来的是原来那份"
        let want = dstr(c, "digest");
        let got = format!("sha256:{}", spec::sha256_hex(&bytes));
        if !want.is_empty() && got != want {
            bail!(
                "检查点 {id} 的字节与它登记的摘要对不上，**没有写进包**：\n  登记 {want}\n  实际 {got}"
            );
        }
        let name = dstr(c, "name");
        let safe = |s: &str| s.replace(['/', '\\', ' '], "_");
        let file = if name.is_empty() { id.clone() } else { safe(&name) };
        docs.push((
            spec::DataDoc {
                path: format!("{}{}.bin", spec::DATA_DIR, file),
                name: if name.is_empty() { id.clone() } else { name },
                label: dstr(c, "label"),
                step: c.get("step").and_then(|x| x.as_i64()).unwrap_or(0),
                parent: dstr(c, "parent"),
                media_type: dstr(c, "mediaType"),
                summary: dstr(c, "summary"),
                tags: c
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|v| v.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                    .unwrap_or_default(),
                visibility: dstr(c, "visibility"),
                meta: c.get("meta").and_then(|m| m.as_object()).cloned().unwrap_or_default(),
                ..Default::default()
            },
            bytes,
        ));
    }
    if docs.is_empty() {
        bail!("列出来的检查点都没有字节可导（只有元数据的点导不出去）");
    }
    let name = if a.name.trim().is_empty() {
        format!("检查点集合（{} 个）", docs.len())
    } else {
        a.name.clone()
    };
    let src = a
        .reference
        .clone()
        .or_else(|| a.id.clone())
        .unwrap_or_else(|| "ckpt".to_string());
    write_seed(
        "ckpt-set",
        &a.as_package,
        &name,
        &a.version,
        &format!("检查点集合（{} 个）", docs.len()),
        &src,
        &a.privacy,
        &a.license,
        None,
        docs,
        a.json,
    )
}

fn mismatch_or_bytes(cfg: &CliConfig, id: &str, token: Option<&str>) -> Result<Vec<u8>> {
    api::get_bytes(cfg, &format!("/api/ckpt/{}/bytes", api::urlenc(id)), token)
}

/* ---------------- KB 本地缓存（pull 用） ---------------- */

/// 文档落盘：目录 + 索引（索引记 checksum，下次按它做增量）。
struct KbWriteReport {
    wrote: Vec<String>,
    unchanged: Vec<String>,
    index_written: bool,
}

impl KbWriteReport {
    fn render(&self, root: &Path) -> String {
        let mut out = format!(
            "✅ 落盘 {} 篇（新写/更新 {} · 未变 {}）\n   缓存 {}",
            self.wrote.len() + self.unchanged.len(),
            self.wrote.len(),
            self.unchanged.len(),
            root.display()
        );
        if self.index_written {
            out.push_str("\n   索引 index.json 已更新（下次只写变了的）");
        }
        out
    }
}

fn kb_ext(format: &str) -> &'static str {
    match format {
        "text" => "txt",
        "json" => "json",
        "yaml" => "yaml",
        _ => "md",
    }
}

/// 把文档写进 `root`：`<ns>/<slug>.<ext>`，并维护 `index.json`。
///
/// `index` 为 None 表示不做增量（bundle --out 的一次性导出）。
fn write_kb_docs(
    root: &Path,
    docs: &[Value],
    index: Option<&Value>,
    dry_run: bool,
) -> Result<KbWriteReport> {
    let mut rep = KbWriteReport {
        wrote: Vec::new(),
        unchanged: Vec::new(),
        index_written: false,
    };
    let mut idx: Map<String, Value> = index
        .and_then(|v| v.get("docs"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    for d in docs {
        let r = s(d, "ref").to_string();
        let (ns, slug) = split_ref(&r);
        if ns.is_empty() || slug.is_empty() {
            continue;
        }
        let sum = s(d, "checksum");
        let prev = idx.get(&r).and_then(|x| x.get("checksum")).and_then(|x| x.as_str()).unwrap_or("");
        if !sum.is_empty() && prev == sum {
            rep.unchanged.push(r.clone());
            continue;
        }
        let file = root.join(&ns).join(format!("{slug}.{}", kb_ext(s(d, "format"))));
        if !dry_run {
            if let Some(p) = file.parent() {
                std::fs::create_dir_all(p)
                    .with_context(|| format!("建不了目录 {}", p.display()))?;
            }
            std::fs::write(&file, s(d, "content"))
                .with_context(|| format!("写不了 {}", file.display()))?;
        }
        rep.wrote.push(r.clone());
        idx.insert(
            r.clone(),
            json!({
                "path": file.strip_prefix(root).unwrap_or(&file).to_string_lossy(),
                "checksum": sum,
                "revision": i(d, "revision"),
                "size": i(d, "size"),
                "kind": s(d, "kind"),
                "title": s(d, "title"),
            }),
        );
    }
    if index.is_some() && !dry_run {
        std::fs::create_dir_all(root).ok();
        let mut merged = index.cloned().unwrap_or_else(|| json!({}));
        merged["spec"] = json!(KB_CACHE_SPEC);
        merged["updatedAt"] = json!(crate::gateway::now_iso());
        merged["docs"] = Value::Object(idx);
        let p = root.join("index.json");
        std::fs::write(&p, serde_json::to_string_pretty(&merged)?)
            .with_context(|| format!("写不了 {}", p.display()))?;
        rep.index_written = true;
    }
    Ok(rep)
}

/// 读本地索引（没有就当成空）。
fn read_kb_index(root: &Path) -> Value {
    let p = root.join("index.json");
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}))
}

fn kb_cache_root(out: &Option<String>) -> PathBuf {
    match out {
        Some(x) => PathBuf::from(x),
        // 与轨迹的本地暂存同一套：都在 ~/.ncc 下面（NCC_HOME 可改根，测试用）。
        None => config::ncc_dir().join("kb"),
    }
}

/// `ncc kb pull`：**按包的声明**去节点取知识库。
///
/// 这是"包声明状态、节点持有状态"两条边的接缝：
///   1. 读 `hur.json` 的 `state.kb`（没有就明说没声明 —— 包不声明就不该有状态，别替它猜）；
///   2. 逐条解析：具体引用 `@ns/slug` 走 bundle；`*` 走"我能读的全部"；
///   3. 只拉 `read` / `readwrite` 的（**只写不读的库不该被拉下来**）；
///   4. 按 checksum 增量写进本地缓存，并核对每篇的 `checksum` 与正文一致。
fn kb_pull(cfg: &CliConfig, a: &KbPullArgs) -> Result<()> {
    let dir = pkg_dir(&a.package);
    let pkg = hur_core::spec::read_pkg(&dir)
        .with_context(|| format!("读不了包的清单：{}", dir.join("hur.json").display()))?;
    let state = match &pkg.state {
        Some(st) if !st.kb.is_empty() => st.clone(),
        _ => bail!(
            "这个包没有声明 `state.kb`（{}）—— 包不声明就不拉东西：\n    \
             要让它有知识库，在 hur.json 里写：\n      \"state\": {{ \"kb\": [{{ \"ref\": \"@team/manual\", \"mode\": \"read\" }}] }}",
            dir.join("hur.json").display()
        ),
    };
    // 记忆 / 检查点的声明：pull 不动它们（那是运行时的行为），但要说一句 —— 
    // 否则用户会以为"声明了但没生效"。
    let mut notes: Vec<String> = Vec::new();
    if let Some(m) = &state.memory {
        notes.push(format!(
            "记忆声明：subject={} ttl={} 天 kinds={} —— 运行时由 Agent 用 `ncc mem` 读写，pull 不动它",
            if m.subject.is_empty() { "self" } else { &m.subject },
            m.ttl_days,
            if m.kinds.is_empty() { "不限".to_string() } else { m.kinds.join("|") }
        ));
    }
    if let Some(c) = &state.checkpoints {
        notes.push(format!(
            "检查点声明：{} label={} keep_local={} —— `ncc ckpt save` 才是打点动作",
            if c.enabled { "启用" } else { "关闭" },
            if c.label.is_empty() { "manual" } else { &c.label },
            c.keep_local
        ));
    }

    let t = config::token_opt(cfg);
    let fallback_ns = ns_arg(&a.namespace);
    let mut picked: Vec<(String, Value)> = Vec::new(); // (来源说明, 文档)
    let mut skipped: Vec<String> = Vec::new();

    for req in &state.kb {
        let mode = if req.mode.is_empty() { "read" } else { req.mode.as_str() };
        if mode == "write" {
            skipped.push(format!("{}（mode=write：只写不读，不该拉下来）", req.r#ref));
            continue;
        }
        let (ns, slug) = split_ref(&req.r#ref);
        if req.r#ref == "*" || (ns.is_empty() && slug.is_empty()) {
            // `*` = 这个节点上我能读的任意库。没有命名空间时用 --namespace 兜底。
            if fallback_ns.is_empty() {
                let d = api::get(cfg, "/api/kb?size=200", t.as_deref())?;
                for x in d["docs"].as_array().cloned().unwrap_or_default() {
                    let full = fetch_kb_doc(cfg, s(&x, "ref"), t.as_deref())?;
                    picked.push(("*".into(), full));
                }
                continue;
            }
            let d = fetch_kb_bundle(cfg, &fallback_ns, t.as_deref())?;
            for x in d["docs"].as_array().cloned().unwrap_or_default() {
                picked.push((format!("* → @{fallback_ns}"), x));
            }
            continue;
        }
        if slug.is_empty() {
            // 只给了命名空间：整库拉。
            let d = fetch_kb_bundle(cfg, &ns, t.as_deref())?;
            for x in d["docs"].as_array().cloned().unwrap_or_default() {
                picked.push((format!("@{ns}"), x));
            }
            continue;
        }
        let full = fetch_kb_doc(cfg, &req.r#ref, t.as_deref())?;
        picked.push((req.r#ref.clone(), full));
    }

    // 核对：正文的 sha256 要等于 checksum（服务端算的，但**客户端也得自己核一遍** ——
    // 知识是要喂给模型的东西，"拿到的是什么"不能只信传输层）。
    let root = kb_cache_root(&a.out);
    let index = read_kb_index(&root);
    let mut docs: Vec<Value> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    for (from, d) in &picked {
        let content = s(d, "content");
        let sum = s(d, "checksum");
        if !sum.is_empty() {
            let got = format!("sha256:{}", sha256_hex(content.as_bytes()));
            if got != sum {
                bad.push(format!("{}（声明 {sum}，本地算出 {got}）", s(d, "ref")));
                continue;
            }
        }
        let mut x = d.clone();
        x["from"] = json!(from);
        docs.push(x);
    }
    if !bad.is_empty() {
        bail!("摘要对不上，已中止（什么都没写）：\n  {}", bad.join("\n  "));
    }

    if a.json {
        return json_out(&json!({
            "package": pkg.id, "cache": root.to_string_lossy(),
            "docs": docs, "skipped": skipped, "notes": notes,
            "dryRun": a.dry_run,
        }));
    }

    println!("包 {} v{} 声明了 {} 条知识库要求", pkg.id, pkg.version, state.kb.len());
    for n in &notes {
        println!("  · {n}");
    }
    for x in &skipped {
        println!("  · 跳过 {x}");
    }
    if docs.is_empty() {
        println!("\n没有可拉取的库（声明里的库要么只写、要么不在你可见范围内）。");
        return Ok(());
    }
    let rep = write_kb_docs(&root, &docs, Some(&index), a.dry_run)?;
    if a.dry_run {
        println!(
            "\n（--dry-run）会写 {} 篇，跳过未变的 {} 篇 —— 落点 {}",
            rep.wrote.len(),
            rep.unchanged.len(),
            root.display()
        );
        for r in &rep.wrote {
            println!("  + {r}");
        }
        return Ok(());
    }
    println!("\n{}", rep.render(&root));
    let fresh = docs.iter().filter(|d| rep.wrote.iter().any(|r| r == s(d, "ref"))).count();
    println!("   本次新写/更新 {fresh} 篇；未变的不重写（按 checksum 判断）");
    println!("   给 Agent 用：把 {} 传给它的工具/提示（每篇带 checksum，可复核）", root.display());
    Ok(())
}

/// 包目录（`--package` 既可给目录也可给 hur.json）。
fn pkg_dir(p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_file() {
        path.parent().map(|x| x.to_path_buf()).unwrap_or(path)
    } else {
        path
    }
}

fn fetch_kb_bundle(cfg: &CliConfig, ns: &str, token: Option<&str>) -> Result<Value> {
    api::get(
        cfg,
        &format!("/api/kb/bundle?namespace={}", api::urlenc(ns)),
        token,
    )
}

fn fetch_kb_doc(cfg: &CliConfig, reference: &str, token: Option<&str>) -> Result<Value> {
    api::get(cfg, &ref_path("/api/kb", reference), token)
}

fn kb_kinds(cfg: &CliConfig, json_flag: bool) -> Result<()> {
    // 服务端有就报服务端的（含它实际的类型计数口径），没有就报本地词表 —— 离线可用。
    let remote = api::get(cfg, "/api/kb/kinds", config::token_opt(cfg).as_deref()).ok();
    if json_flag {
        let d = remote.unwrap_or_else(|| json!({ "kinds": KB_KINDS, "formats": KB_FORMATS }));
        return json_out(&d);
    }
    println!("文档类型");
    if let Some(d) = &remote {
        for k in d["kinds"].as_array().cloned().unwrap_or_default() {
            println!("  {:<12} {:<10} {}", s(&k, "id"), s(&k, "zh"), s(&k, "descZh"));
        }
        println!("\n格式 {}", arr(d.get("formats")).join(" | "));
        println!("上限 {}", d["limits"]["maxBytes"]);
        println!("检索 {}", s(d, "search"));
    } else {
        println!("  {}（离线：本地词表）", KB_KINDS.join(" | "));
        println!("\n格式 {}", KB_FORMATS.join(" | "));
    }
    println!(
        "\n默认**私有**（`--public` 才公开）。检索是关键词加权，不是向量检索。\n\
         单篇上限 {} —— 更大的内容该走制品。",
        MAX_KB_BYTES
    );
    Ok(())
}

/* ============================ 记忆 mem ============================ */

#[derive(clap::Subcommand)]
pub enum MemAction {
    /// 导出一份**记忆快照包**（不可变、可签名；活记忆永远留在节点）
    Export(MemExportArgs),
    /// 列出记忆（默认不含过期的）
    Ls(MemLsArgs),
    /// 读一条（`ncc mem get <key>`，默认 subject=self）—— Agent 读记忆的主路径
    Get(MemGetArgs),
    /// 写一条（同 (命名空间, subject, key) 即更新，Revision+1）
    Set(MemSetArgs),
    /// 删一条（按 id 或 key）
    Rm(MemRmArgs),
    /// 清理已过期的（读时已判过期，这只是真正删掉）
    Gc(MemGcArgs),
    /// 记忆种类与上限
    Kinds(MemKindsArgs),
}

#[derive(clap::Args)]
pub struct MemLsArgs {
    #[arg(long)]
    pub namespace: Option<String>,
    /// 谁的记忆（默认全部 subject）
    #[arg(long)]
    pub subject: Option<String>,
    /// 键前缀（按前缀找一族记忆）
    #[arg(long)]
    pub prefix: Option<String>,
    #[arg(long, value_parser = ["fact", "preference", "episode", "summary", "pointer"])]
    pub kind: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    /// 只看某来源（如某个 traceId 写下的）
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub pinned: bool,
    /// 连过期的也列出来（过期的按规矩视为不存在）
    #[arg(long)]
    pub expired: bool,
    #[arg(long)]
    pub mine: bool,
    #[arg(long)]
    pub all: bool,
    #[arg(long, default_value = "100")]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MemGetArgs {
    /// 记忆的键
    pub key: String,
    /// 谁的记忆（默认 self = 整个包共一份）
    #[arg(long, default_value = "self")]
    pub subject: String,
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MemSetArgs {
    pub key: String,
    pub value: String,
    #[arg(long, default_value = "self")]
    pub subject: String,
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long, default_value = "fact", value_parser = ["fact", "preference", "episode", "summary", "pointer"])]
    pub kind: String,
    #[arg(long = "tag", value_delimiter = ',')]
    pub tags: Vec<String>,
    /// 从哪来（traceId / ckpt 引用 / 人手写）—— 记忆可追溯才谈得上可信
    #[arg(long)]
    pub source: Option<String>,
    /// 存活天数（0 = 不过期）
    #[arg(long, default_value = "0")]
    pub ttl_days: i64,
    /// 置信度千分位（0..1000；与轨迹 score 同一口径，避免浮点）
    #[arg(long, default_value = "0")]
    pub confidence: i64,
    /// 钉住（列表里排在最前；普通写入不会把它摘下来）
    #[arg(long)]
    pub pin: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MemRmArgs {
    /// id（ME-…）或 key
    ///
    /// ⚠️ 字段名不能叫 `target`：顶层 `--target` 是 global 的，同 id 时 clap 会把
    /// 位置参数的值塞进全局目标，于是 `ncc mem rm timezone` 会报「没有名为 timezone 的目标」。
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MemGcArgs {
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MemKindsArgs {
    #[arg(long)]
    pub json: bool,
}

impl MemAction {
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            MemAction::Kinds(_) => None,
            // 导出是"读"：有 ckpt/kb 之外的分支就说明这个能力面还得再想
            MemAction::Export(_) => None,
            _ => Some("mem"),
        }
    }
}

pub fn run_mem(cfg: &CliConfig, action: &MemAction) -> Result<()> {
    match action {
        MemAction::Export(a) => mem_export(cfg, a),
        MemAction::Ls(a) => mem_ls(cfg, a),
        MemAction::Get(a) => mem_get(cfg, a),
        MemAction::Set(a) => mem_set(cfg, a),
        MemAction::Rm(a) => mem_rm(cfg, a),
        MemAction::Gc(a) => mem_gc(cfg, a),
        MemAction::Kinds(a) => mem_kinds(cfg, a.json),
    }
}

fn mem_ls(cfg: &CliConfig, a: &MemLsArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs = scope_qs(a.all, a.mine);
    let ns = ns_arg(&a.namespace);
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(&ns)));
    }
    if let Some(x) = &a.subject {
        qs.push(format!("subject={}", api::urlenc(x)));
    }
    if let Some(x) = &a.prefix {
        qs.push(format!("prefix={}", api::urlenc(x)));
    }
    if let Some(x) = &a.kind {
        qs.push(format!("kind={x}"));
    }
    if let Some(x) = &a.tag {
        qs.push(format!("tag={}", api::urlenc(x)));
    }
    if let Some(x) = &a.source {
        qs.push(format!("source={}", api::urlenc(x)));
    }
    if a.pinned {
        qs.push("pinned=1".into());
    }
    if a.expired {
        qs.push("expired=1".into());
    }
    qs.push(format!("limit={}", a.limit.clamp(1, 1000)));
    let d = api::get(cfg, &format!("/api/mem?{}", qs.join("&")), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    let rows = d["memories"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("（没有记忆）");
        return Ok(());
    }
    println!("{:<10} {:<22} {:<11} {:>4} {}", "subject", "key", "kind", "rev", "value");
    for m in &rows {
        let expired = m["expired"].as_bool().unwrap_or(false);
        println!(
            "{:<10} {:<22} {:<11} {:>4} {}{}",
            clip(s(m, "subject"), 10),
            clip(s(m, "key"), 22),
            s(m, "kind"),
            i(m, "revision"),
            clip(s(m, "value"), 44),
            if expired { "  ⚠ 已过期" } else { "" }
        );
    }
    println!("\n共 {} 条（范围 {}）", d["total"], s(&d, "scope"));
    println!("读一条：`ncc mem get <key> [--subject s]` · 写：`ncc mem set <key> <value>`");
    Ok(())
}

fn mem_get(cfg: &CliConfig, a: &MemGetArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let ns = ns_arg(&a.namespace);
    let mut qs = vec![
        format!("subject={}", api::urlenc(&a.subject)),
        format!("key={}", api::urlenc(&a.key)),
    ];
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(&ns)));
    }
    let d = api::get(cfg, &format!("/api/mem/lookup?{}", qs.join("&")), Some(&t))?;
    if a.json {
        return json_out(&d);
    }
    let m = &d["memory"];
    println!("{} / {}", s(m, "subject"), s(m, "key"));
    println!(
        "  {} · rev {} · 置信度 {}‰ · 来源 {}",
        s(m, "kindLabel"),
        i(m, "revision"),
        i(m, "confidence"),
        if s(m, "source").is_empty() { "—" } else { s(m, "source") }
    );
    if let Some(exp) = m.get("expiresAt").and_then(|x| x.as_str()) {
        println!("  过期 {exp}");
    }
    println!("\n{}", s(m, "value"));
    Ok(())
}

fn mem_set(cfg: &CliConfig, a: &MemSetArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    if a.value.len() > MAX_MEM_BYTES {
        bail!(
            "value {} 字节，超过上限 {} —— 更大的内容该是 kb 文档或 ckpt",
            a.value.len(),
            MAX_MEM_BYTES
        );
    }
    if !(0..=1000).contains(&a.confidence) {
        bail!("--confidence 是千分位 0..1000（收到 {}）", a.confidence);
    }
    let mut body = json!({
        "namespace": ns_arg(&a.namespace),
        "subject": a.subject,
        "key": a.key,
        "value": a.value,
        "kind": a.kind,
        "tags": a.tags,
        "confidence": a.confidence,
        "pinned": a.pin,
        "ttl_days": a.ttl_days,
    });
    if let Some(x) = &a.source {
        body["source"] = json!(x);
    }
    // 写用 PUT：语义就是"把这个键设成这个值"（upsert），不是"新建一条资源"。
    let d = api::request(cfg, "PUT", "/api/mem", Some(&t), Some(&body), None, &[])?;
    if a.json {
        return json_out(&d);
    }
    let created = d["created"].as_bool().unwrap_or(false);
    println!(
        "{} {} / {}  rev {}",
        if created { "✅ 记下" } else { "✅ 更新" },
        s(&d["memory"], "subject"),
        s(&d["memory"], "key"),
        i(&d, "revision")
    );
    if a.ttl_days > 0 {
        println!("  {} 天后过期（过期即视为不存在）", a.ttl_days);
    }
    Ok(())
}

fn mem_rm(cfg: &CliConfig, a: &MemRmArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    // 既要能按 id 删，也要能按 key 删：key 得先解析成 id（服务端 DELETE 认 id）。
    let id = if a.reference.starts_with("ME-") {
        a.reference.clone()
    } else {
        let d = api::get(
            cfg,
            &format!("/api/mem?prefix={}&limit=200", api::urlenc(&a.reference)),
            Some(&t),
        )?;
        let hit = d["memories"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|m| s(m, "key") == a.reference)
            .map(|m| s(&m, "id").to_string());
        match hit {
            Some(x) => x,
            None => bail!(
                "没找到 key = {} 的记忆（也可能是别人的：记忆只有成员/被授权者能看到）",
                a.reference
            ),
        }
    };
    let d = api::del(cfg, &format!("/api/mem/{}", api::urlenc(&id)), Some(&t))?;
    if a.json {
        return json_out(&d);
    }
    println!("✅ 删除 {}（{}）", id, s(&d, "key"));
    Ok(())
}

fn mem_gc(cfg: &CliConfig, a: &MemGcArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let ns = ns_arg(&a.namespace);
    let path = if ns.is_empty() {
        "/api/mem/gc".to_string()
    } else {
        format!("/api/mem/gc?namespace={}", api::urlenc(&ns))
    };
    let d = api::post_json(cfg, &path, Some(&t), &json!({}))?;
    if a.json {
        return json_out(&d);
    }
    println!("✅ 清掉 {} 条过期记忆（@{}）", d["removed"], s(&d, "namespace"));
    Ok(())
}

fn mem_kinds(cfg: &CliConfig, json_flag: bool) -> Result<()> {
    let remote = api::get(cfg, "/api/mem/kinds", config::token_opt(cfg).as_deref()).ok();
    if json_flag {
        let d = remote.unwrap_or_else(|| json!({ "kinds": MEM_KINDS, "limits": { "maxValueBytes": MAX_MEM_BYTES } }));
        return json_out(&d);
    }
    println!("记忆种类");
    if let Some(d) = &remote {
        for k in d["kinds"].as_array().cloned().unwrap_or_default() {
            println!("  {:<12} {:<10} {}", s(&k, "id"), s(&k, "zh"), s(&k, "descZh"));
        }
        println!("\n上限 value {} 字节", d["limits"]["maxValueBytes"]);
    } else {
        println!("  {}（离线：本地词表）", MEM_KINDS.join(" | "));
    }
    println!(
        "\n唯一性在 (命名空间, subject, key)：同键再写就是**更新**（Revision+1）。\n\
         **记忆没有公开档** —— 它只属于命名空间成员与拿到 state 授权的人。\n\
         TTL 是**读时判定**（过期即视为不存在），另有 `ncc mem gc` 真正删掉。"
    );
    Ok(())
}

/* ============================ 检查点 ckpt ============================ */

#[derive(clap::Subcommand)]
pub enum CkptAction {
    /// 导出一份**检查点集合包**（字节 + 血缘；检查点天然不可变）
    Export(CkptExportArgs),
    /// 列出检查点（默认只列 active 的）
    Ls(CkptLsArgs),
    /// 看一个检查点（含签名下载地址）
    Show(CkptShowArgs),
    /// 打一个点：算摘要 → 建元数据 → 上传字节（一步到位）
    Save(CkptSaveArgs),
    /// 把一个点的字节取回本地（**落盘前核对摘要**）
    Pull(CkptPullArgs),
    /// 血缘：从某个点沿 parent 一路回溯
    Lineage(CkptLineageArgs),
    /// 清理：每个制品只留最新 N 个（标 pruned + 删字节，元数据留下）
    Prune(CkptPruneArgs),
    /// 删一个点（元数据也删；要"留个记录"请用 prune）
    Rm(CkptRmArgs),
    /// 打点粒度与上限
    Kinds(CkptKindsArgs),
}

#[derive(clap::Args)]
pub struct CkptLsArgs {
    #[arg(long)]
    pub namespace: Option<String>,
    /// 只看属于某个制品的点（@命名空间/slug）
    #[arg(long = "ref")]
    pub reference: Option<String>,
    #[arg(long, value_parser = ["episode", "step", "run", "release", "handoff", "manual"])]
    pub label: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    /// 名字里含这个词
    #[arg(long, short = 'q')]
    pub query: Option<String>,
    /// 也列被清理过的（status=pruned）
    #[arg(long)]
    pub pruned: bool,
    #[arg(long)]
    pub mine: bool,
    #[arg(long)]
    pub all: bool,
    #[arg(long, default_value = "50")]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptShowArgs {
    /// id（CK-…）或名字（字段名不能叫 target：见 MemRmArgs 的说明）
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptSaveArgs {
    /// 名字（列表里先看到的就是它）
    #[arg(long)]
    pub name: String,
    /// 这个点属于谁（制品引用 @命名空间/slug；不给 = 纯粹的一次运行快照）
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// 与哪个制品版本对齐
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long, default_value = "manual", value_parser = ["episode", "step", "run", "release", "handoff", "manual"])]
    pub label: String,
    /// 第 N 步（label=step 时有意义）
    #[arg(long, default_value = "0")]
    pub step: i64,
    #[arg(long)]
    pub summary: Option<String>,
    #[arg(long = "tag", value_delimiter = ',')]
    pub tags: Vec<String>,
    /// 上一个点（血缘）：`--parent <id>`，或 `--parent-last` 自动接最新一个
    #[arg(long)]
    pub parent: Option<String>,
    /// 自动接"同 ref 的最近一个点"作为 parent
    #[arg(long)]
    pub parent_last: bool,
    /// 快照内容（不给则只建元数据，之后再传字节）
    #[arg(long)]
    pub file: Option<String>,
    /// 自由元数据（可重复：`--meta loss=0.42 --meta steps=1200`）
    #[arg(long = "meta")]
    pub meta: Vec<String>,
    #[arg(long)]
    pub public: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptPullArgs {
    /// id 或名字（字段名不能叫 target：见 MemRmArgs 的说明）
    pub reference: String,
    /// 落到哪个文件（默认 ./<名字>）
    #[arg(long)]
    pub out: Option<String>,
    /// 不核对摘要（**不建议**：检查点的全部价值就是"拿回来的是原来那份"）
    #[arg(long)]
    pub no_verify: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptLineageArgs {
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptPruneArgs {
    /// 哪个制品（@命名空间/slug）
    #[arg(long = "ref")]
    pub reference: String,
    /// 每个制品留最新几个
    #[arg(long, default_value = "5")]
    pub keep: i64,
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptRmArgs {
    pub reference: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CkptKindsArgs {
    #[arg(long)]
    pub json: bool,
}

impl CkptAction {
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            CkptAction::Kinds(_) => None,
            CkptAction::Export(_) => None,
            _ => Some("ckpt"),
        }
    }
}

pub fn run_ckpt(cfg: &CliConfig, action: &CkptAction) -> Result<()> {
    match action {
        CkptAction::Export(a) => ckpt_export(cfg, a),
        CkptAction::Ls(a) => ckpt_ls(cfg, a),
        CkptAction::Show(a) => ckpt_show(cfg, a),
        CkptAction::Save(a) => ckpt_save(cfg, a),
        CkptAction::Pull(a) => ckpt_pull(cfg, a),
        CkptAction::Lineage(a) => ckpt_lineage(cfg, a),
        CkptAction::Prune(a) => ckpt_prune(cfg, a),
        CkptAction::Rm(a) => ckpt_rm(cfg, a),
        CkptAction::Kinds(a) => ckpt_kinds(cfg, a.json),
    }
}

fn ckpt_ls(cfg: &CliConfig, a: &CkptLsArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs = scope_qs(a.all, a.mine);
    let ns = ns_arg(&a.namespace);
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(&ns)));
    }
    if let Some(x) = &a.reference {
        qs.push(format!("ref={}", api::urlenc(x)));
    }
    if let Some(x) = &a.label {
        qs.push(format!("label={x}"));
    }
    if let Some(x) = &a.tag {
        qs.push(format!("tag={}", api::urlenc(x)));
    }
    if let Some(x) = &a.query {
        qs.push(format!("q={}", api::urlenc(x)));
    }
    if a.pruned {
        qs.push("status=pruned".into());
    }
    qs.push(format!("limit={}", a.limit.clamp(1, 200)));
    let d = api::get(cfg, &format!("/api/ckpt?{}", qs.join("&")), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    let rows = d["checkpoints"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("（没有检查点）");
        return Ok(());
    }
    println!("{:<14} {:<10} {:<8} {:>9} {:<5} {}", "id", "label", "step", "大小", "字节", "名字");
    for c in &rows {
        println!(
            "{:<14} {:<10} {:<8} {:>9} {:<5} {}",
            clip(s(c, "id"), 14),
            s(c, "label"),
            i(c, "step"),
            size_txt(i(c, "size")),
            if s(c, "digest").is_empty() { "—" } else { "有" },
            clip(s(c, "name"), 40)
        );
    }
    println!("\n共 {} 个（范围 {}）", d["total"], s(&d, "scope"));
    println!("取回字节：`ncc ckpt pull <id> --out ./restore.bin`（落盘前核对摘要）");
    Ok(())
}

fn ckpt_show(cfg: &CliConfig, a: &CkptShowArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let d = api::get(cfg, &ref_path("/api/ckpt", &a.reference), t.as_deref())?;
    if a.json {
        return json_out(&d);
    }
    let c = &d["checkpoint"];
    println!("{}  {}", s(c, "id"), s(c, "name"));
    println!(
        "  {} · {}{} · 状态 {}",
        s(c, "label"),
        if s(c, "subjectRef").is_empty() { "未绑定制品" } else { s(c, "subjectRef") },
        if s(c, "subjectVersion").is_empty() { String::new() } else { format!(" v{}", s(c, "subjectVersion")) },
        s(c, "status")
    );
    println!("  摘要 {} · 大小 {}", s(c, "digest"), size_txt(i(c, "size")));
    let parent = s(c, "parent");
    println!(
        "  血缘 {}",
        if parent.is_empty() { "（起点）".to_string() } else { parent.to_string() }
    );
    if !s(c, "summary").is_empty() {
        println!("  说明 {}", s(c, "summary"));
    }
    let tags = arr(c.get("tags"));
    if !tags.is_empty() {
        println!("  标签 {}", tags.join(", "));
    }
    println!("  时间 {}", s(c, "createdAt"));
    if let Some(u) = c.get("bytesUrl").and_then(|x| x.as_str()) {
        println!("\n  字节地址（短时签名，{} 秒有效）\n    {u}", i(c, "bytesTtlSec"));
    } else {
        println!("\n  这个点还没有字节（建了元数据，字节没传）");
    }
    println!("  取回：`ncc ckpt pull {} --out ./restore.bin`", s(c, "id"));
    Ok(())
}

fn ckpt_save(cfg: &CliConfig, a: &CkptSaveArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    // 字节先读进来（要算摘要），再一步下单 —— 顺序不能反：
    // 服务端建元数据时就要求 digest，上传时再用字节重算一遍核对。
    let bytes: Option<Vec<u8>> = match &a.file {
        Some(p) => {
            let b = std::fs::read(p).with_context(|| format!("读不了 {p}"))?;
            if b.is_empty() {
                bail!("{p} 是空文件（空快照没有意义）");
            }
            if b.len() as u64 > MAX_CKPT_BYTES {
                bail!(
                    "{} 有 {} 字节，超过上限 {} —— 模型权重那种大家伙该走对象存储/制品",
                    p,
                    b.len(),
                    MAX_CKPT_BYTES
                );
            }
            Some(b)
        }
        None => None,
    };
    let digest = match &bytes {
        Some(b) => format!("sha256:{}", sha256_hex(b)),
        None => String::new(),
    };
    let size = bytes.as_ref().map(|b| b.len() as i64).unwrap_or(0);

    // parent：--parent 显式给，或 --parent-last 自动接同 ref 的最近一个点。
    let mut parent = a.parent.clone().unwrap_or_default();
    if parent.is_empty() && a.parent_last {
        let mut qs: Vec<String> = vec!["limit=1".into()];
        if let Some(r) = &a.reference {
            qs.push(format!("ref={}", api::urlenc(r)));
        }
        let d = api::get(cfg, &format!("/api/ckpt?{}", qs.join("&")), Some(&t))?;
        if let Some(first) = d["checkpoints"].as_array().and_then(|x| x.first()) {
            parent = s(first, "id").to_string();
        }
    }

    let mut meta = Map::new();
    for kv in &a.meta {
        let mut it = kv.splitn(2, '=');
        let k = it.next().unwrap_or("").trim();
        let v = it.next().unwrap_or("").trim();
        if k.is_empty() || v.is_empty() {
            bail!("--meta 要写 k=v（收到 {kv}）");
        }
        // 数值就当数值，别的当字符串 —— 元数据只展示，不做判断。
        let parsed = if let Ok(n) = v.parse::<i64>() {
            json!(n)
        } else if let Ok(f) = v.parse::<f64>() {
            json!(f)
        } else {
            json!(v)
        };
        meta.insert(k.to_string(), parsed);
    }

    let mut body = json!({
        "namespace": ns_arg(&a.namespace),
        "name": a.name,
        "label": a.label,
        "step": a.step,
        "tags": a.tags,
        "visibility": if a.public { "public" } else { "private" },
        "parent": parent,
        "digest": digest,
        "size": size,
        "meta": Value::Object(meta),
    });
    if let Some(x) = &a.summary {
        body["summary"] = json!(x);
    }
    if let Some(x) = &a.reference {
        body["subject_ref"] = json!(x);
    }
    if let Some(x) = &a.version {
        body["subject_version"] = json!(x);
    }
    if let Some(f) = &a.file {
        body["media_type"] = json!(media_type_of(f));
    }
    let created = api::post_json(cfg, "/api/ckpt", Some(&t), &body)?;
    let id = s(&created["checkpoint"], "id").to_string();
    if id.is_empty() {
        bail!("服务端没返回检查点 id：{}", created);
    }

    if let Some(b) = &bytes {
        let d = api::request(
            cfg,
            "PUT",
            &format!("/api/ckpt/{}/blob", api::urlenc(&id)),
            Some(&t),
            None,
            Some(b),
            &[],
        )?;
        if a.json {
            return json_out(&d);
        }
        let c = &d["checkpoint"];
        println!("✅ 打点 {}  {}", s(c, "id"), s(c, "name"));
        println!("  {} · {}", s(c, "label"), s(c, "digest"));
        println!(
            "  字节 {} · {} · 血缘 {}",
            size_txt(i(c, "size")),
            s(c, "mediaType"),
            if s(c, "parent").is_empty() { "起点" } else { s(c, "parent") }
        );
        if let Some(u) = c.get("bytesUrl").and_then(|x| x.as_str()) {
            println!("\n  短时地址（10 分钟）\n    {u}");
        }
        println!("  取回：`ncc ckpt pull {id} --out ./restore.bin`");
        return Ok(());
    }
    if a.json {
        return json_out(&created);
    }
    println!("✅ 建了点 {}  {}（**还没有字节**）", id, s(&created["checkpoint"], "name"));
    println!("  {}", s(&created, "next"));
    Ok(())
}

/// 由文件名猜 media type（只做常见的几种，别的当二进制）。
fn media_type_of(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "json" => "application/json",
        "tar" => "application/x-tar",
        "gz" | "tgz" => "application/gzip",
        "zip" => "application/zip",
        "txt" | "md" | "log" => "text/plain",
        "jsonl" => "application/x-ndjson",
        "bin" | "safetensors" => "application/octet-stream",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn ckpt_pull(cfg: &CliConfig, a: &CkptPullArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let d = api::get(cfg, &ref_path("/api/ckpt", &a.reference), t.as_deref())?;
    let c = &d["checkpoint"];
    let digest = s(c, "digest").to_string();
    let meta = if let Some(u) = c.get("bytesUrl").and_then(|x| x.as_str()) {
        // 签名地址：对方不用带凭据（与制品分享同一套）。
        api::get_bytes(cfg, u, None)?
    } else {
        api::get_bytes(
            cfg,
            &format!("/api/ckpt/{}/bytes", api::urlenc(s(c, "id"))),
            t.as_deref(),
        )?
    };
    if meta.is_empty() {
        bail!("{} 这个点没有字节（建了元数据，字节没传）", s(c, "id"));
    }
    let got = format!("sha256:{}", sha256_hex(&meta));
    if !a.no_verify && !digest.is_empty() && got != digest {
        bail!(
            "摘要对不上，**没有落盘**：\n  声明 {digest}\n  实际 {got}\n\
             检查点的全部价值就是「拿回来的是原来那份」—— 对不上就别写。"
        );
    }
    let out = PathBuf::from(a.out.clone().unwrap_or_else(|| {
        let n = s(c, "name");
        if n.is_empty() { s(c, "id").to_string() } else { n.replace('/', "_").replace(' ', "_") }
    }));
    if let Some(p) = out.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p).ok();
        }
    }
    std::fs::write(&out, &meta).with_context(|| format!("写不了 {}", out.display()))?;
    if a.json {
        return json_out(&json!({
            "id": s(c, "id"), "name": s(c, "name"), "out": out.to_string_lossy(),
            "bytes": meta.len(), "digest": got, "verified": !a.no_verify,
        }));
    }
    println!("✅ {} → {}", s(c, "id"), out.display());
    println!(
        "  {} · {}{}",
        size_txt(meta.len() as i64),
        got,
        if a.no_verify { "（--no-verify：没核对）" } else { "（已核对）" }
    );
    Ok(())
}

fn ckpt_lineage(cfg: &CliConfig, a: &CkptLineageArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let d = api::get(
        cfg,
        &ref_path("/api/ckpt", &format!("{}/lineage", a.reference.trim_matches('/'))),
        t.as_deref(),
    )?;
    if a.json {
        return json_out(&d);
    }
    let rows = d["lineage"].as_array().cloned().unwrap_or_default();
    println!("血缘（{} 个点，最新在前）", d["count"]);
    for (idx, c) in rows.iter().enumerate() {
        let mark = if idx == 0 { "◉" } else { "○" };
        println!(
            "  {} {:<14} {:<10} {:<20} {}  {}",
            mark,
            clip(s(c, "id"), 14),
            s(c, "label"),
            clip(s(c, "createdAt"), 20),
            s(c, "digest").get(..14).unwrap_or(""),
            clip(s(c, "name"), 30)
        );
    }
    Ok(())
}

fn ckpt_prune(cfg: &CliConfig, a: &CkptPruneArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    if a.keep <= 0 {
        bail!("--keep 必须大于 0（要全删请逐个 `ncc ckpt rm`）");
    }
    let mut path = format!(
        "/api/ckpt/prune?ref={}&keep={}",
        api::urlenc(&a.reference),
        a.keep
    );
    let ns = ns_arg(&a.namespace);
    if !ns.is_empty() {
        path.push_str(&format!("&namespace={}", api::urlenc(&ns)));
    }
    let d = api::post_json(cfg, &path, Some(&t), &json!({}))?;
    if a.json {
        return json_out(&d);
    }
    println!(
        "✅ {} 保留最新 {} 个：标 pruned {} 个，删掉字节 {} 份",
        s(&d, "ref"),
        a.keep,
        d["pruned"],
        d["bytesRemoved"]
    );
    println!("  元数据留下（被清理过的点仍然可查 —— 否则历史会有无法解释的空洞）");
    Ok(())
}

fn ckpt_rm(cfg: &CliConfig, a: &CkptRmArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    let d = api::del(cfg, &ref_path("/api/ckpt", &a.reference), Some(&t))?;
    if a.json {
        return json_out(&d);
    }
    println!(
        "✅ 删除 {}（{}）{}",
        s(&d, "id"),
        s(&d, "name"),
        if d["bytes"].as_bool().unwrap_or(false) { "，字节一并删掉" } else { "" }
    );
    Ok(())
}

fn ckpt_kinds(cfg: &CliConfig, json_flag: bool) -> Result<()> {
    let remote = api::get(cfg, "/api/ckpt/kinds", config::token_opt(cfg).as_deref()).ok();
    if json_flag {
        let d = remote.unwrap_or_else(|| json!({ "labels": CKPT_LABELS }));
        return json_out(&d);
    }
    println!("打点粒度");
    if let Some(d) = &remote {
        for k in d["labels"].as_array().cloned().unwrap_or_default() {
            println!("  {:<10} {:<8} {}", s(&k, "id"), s(&k, "zh"), s(&k, "descZh"));
        }
    } else {
        println!("  {}（离线：本地词表）", CKPT_LABELS.join(" | "));
    }
    println!(
        "\n检查点**不可变**：没有「改」这个动作 —— 要改就再打一个点。\n\
         字节进 blob、元数据进库；取回时客户端会核对摘要。"
    );
    Ok(())
}

/* ============================ 测试 ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ref_handles_both_forms() {
        assert_eq!(split_ref("@team/manual"), ("team".into(), "manual".into()));
        assert_eq!(split_ref("team/manual"), ("team".into(), "manual".into()));
        assert_eq!(split_ref("manual"), (String::new(), "manual".into()));
        assert_eq!(split_ref("@team"), ("".into(), "team".into()));
    }

    #[test]
    fn ref_path_keeps_the_slash_unencoded() {
        // 斜杠是服务端路由的分隔符：编码成 %2F 服务端就找不到东西了。
        assert_eq!(ref_path("/api/kb", "@team/manual"), "/api/kb/@team/manual");
    }

    #[test]
    fn clip_never_breaks_utf8() {
        let s = "知识库文档";
        assert_eq!(clip(s, 2), "知识…");
        assert_eq!(clip(s, 99), s);
    }

    #[test]
    fn kb_ext_maps_formats() {
        assert_eq!(kb_ext("markdown"), "md");
        assert_eq!(kb_ext("text"), "txt");
        assert_eq!(kb_ext("yaml"), "yaml");
    }

    #[test]
    fn media_type_guesses_common_snapshots() {
        assert_eq!(media_type_of("snap.tar"), "application/x-tar");
        assert_eq!(media_type_of("state.json"), "application/json");
        assert_eq!(media_type_of("weird"), "application/octet-stream");
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
