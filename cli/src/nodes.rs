// NCC Node：节点连接（不是通讯录）。
//
// 设计要点（与服务端一致，别在这里加"聪明"的推断）：
// - **节点声明自己是什么**：service（服务）/ agent（为人服务的 Agent）/ assigned（被分配的
//   Agent）。声明在上报时给出（`ncc living --kind`），不是连接方决定的。
// - **连接 = 我的本地清单**：把关心的节点收进来并给一个 Name 标签，好让我的 Agent 知道
//   「这个节点是谁、能干什么、怎么连过去」。同一 NCC 实例上的公开节点彼此可连接，
//   不需要对方审批。
// - **连接 ≠ 授权**：连上只代表找得到；要取对方的私有制品/分享页，仍然要 `ncc grant`。
// - **区域聚合与推荐不在网页上**：它们是 Agent 面能力（对应 MCP 的 ncc_region_profile /
//   ncc_recommend_nodes），CLI 侧就是 `ncc nodes region` / `ncc nodes recommend`。
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Result};
use serde_json::{json, Value};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct ListArgs {
    /// 按类型过滤：service | agent | assigned
    #[arg(long)]
    pub kind: Option<String>,
    /// 关键词（Name 标签 / 节点名 / 用户名）
    #[arg(long)]
    pub q: Option<String>,
}

#[derive(clap::Args)]
pub struct LinkArgs {
    /// 节点引用：LD-… 或 @命名空间/节点slug
    pub node: String,
    /// Name 标签：我给这个节点起的名字（缺省用节点自报的名字）
    #[arg(long)]
    pub label: Option<String>,
    /// 用途备注（给谁用、怎么用）
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(clap::Args)]
pub struct LabelArgs {
    /// 连接 id（见 `ncc nodes list` 里的 links 段）
    pub id: String,
    /// 新的 Name 标签
    #[arg(long)]
    pub label: Option<String>,
    /// 新的备注
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(clap::Args)]
pub struct DiscoverArgs {
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub region: Option<String>,
    #[arg(long)]
    pub q: Option<String>,
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

#[derive(clap::Args)]
pub struct RecommendArgs {
    #[arg(long)]
    pub region: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
}

/* ---------------- 工具 ---------------- */

fn token(cfg: &CliConfig) -> Result<String> {
    config::require_token(cfg)
}

fn arr(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str()).map(String::from).collect())
        .unwrap_or_default()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn kind_cn(k: &str) -> &str {
    match k {
        "service" => "服务",
        "agent" => "个人 Agent",
        "assigned" => "被分配的 Agent",
        other => other,
    }
}

/// 节点一行摘要：Name 标签 + 类型 + 在线状态 + 地址。
fn node_line(v: &Value, indent: &str) {
    let label = s(v, "label");
    let name = s(v, "name");
    let kind = kind_cn(s(v, "kind"));
    let dot = if v["online"].as_bool().unwrap_or(false) { "●" } else { "○" };
    let url = s(v, "url");
    println!("{indent}{dot} {:<24} {:<16} {}", label, kind, url);
    let handle = s(&v["owner"], "handle");
    let region = s(&v["owner"], "region");
    if !handle.is_empty() || !region.is_empty() {
        println!(
            "{indent}   归属 {}{}  节点名 {}",
            handle,
            if region.is_empty() { String::new() } else { format!(" · {region}") },
            name
        );
    }
    let caps = arr(v.get("capabilities"));
    if !caps.is_empty() {
        println!("{indent}   能力 {}", caps.join(", "));
    }
    let note = v.pointer("/link/note").and_then(|x| x.as_str()).unwrap_or("");
    if !note.is_empty() {
        println!("{indent}   备注 {note}");
    }
}

/* ---------------- 列表 ---------------- */

/// `ncc nodes [list]` —— 我的节点 + 我连接的节点。
pub fn list(cfg: &CliConfig, a: &ListArgs) -> Result<()> {
    let t = token(cfg)?;
    let mut qs: Vec<String> = Vec::new();
    if let Some(k) = a.kind.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("kind={}", api::urlenc(k)));
    }
    if let Some(q) = a.q.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("q={}", api::urlenc(q)));
    }
    let path = if qs.is_empty() {
        "/api/nodes".to_string()
    } else {
        format!("/api/nodes?{}", qs.join("&"))
    };
    let d = api::get(cfg, &path, Some(&t))?;
    let mine = d["mine"].as_array().cloned().unwrap_or_default();
    let links = d["links"].as_array().cloned().unwrap_or_default();

    println!("我的节点（{}）", mine.len());
    if mine.is_empty() {
        println!("  还没有。上报一个：ncc living --name my-agent --kind agent");
    } else {
        for n in &mine {
            node_line(n, "  ");
        }
    }

    println!("\n我连接的节点（{}）", links.len());
    if links.is_empty() {
        println!("  还没有。找找本实例上能连的：ncc nodes discover");
    } else {
        for n in &links {
            node_line(n, "  ");
            println!("     连接 {}", s(&n["link"], "id"));
        }
    }
    println!("\n连接只代表「找得到」；要取对方的私有制品仍需授权：ncc grant set --user @某人 --kind artifact");
    Ok(())
}

/// `ncc nodes kinds` —— 节点类型目录。
pub fn kinds(cfg: &CliConfig) -> Result<()> {
    let t = token(cfg)?;
    let d = api::get(cfg, "/api/nodes/kinds", Some(&t))?;
    println!("节点类型（上报时用 --kind 声明）");
    for k in d["kinds"].as_array().cloned().unwrap_or_default() {
        println!("  {:<10} {:<18} {}", s(&k, "id"), s(&k, "zh"), s(&k, "descZh"));
    }
    println!("\n用法：ncc living --name my-agent --kind agent --capabilities mcp,api");
    Ok(())
}

/// `ncc nodes discover` —— 本 NCC 实例上可连接的节点。
pub fn discover(cfg: &CliConfig, a: &DiscoverArgs) -> Result<()> {
    let t = token(cfg)?;
    let mut qs = vec![format!("limit={}", a.limit)];
    if let Some(k) = a.kind.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("kind={}", api::urlenc(k)));
    }
    if let Some(r) = a.region.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("region={}", api::urlenc(r)));
    }
    if let Some(q) = a.q.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("q={}", api::urlenc(q)));
    }
    let d = api::get(cfg, &format!("/api/nodes/discover?{}", qs.join("&")), Some(&t))?;
    let rows = d["nodes"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("本实例上没有可连接的新节点（都是你自己的、已连过的，或没有公开节点）。");
        println!("提示：节点要用公共可见性上报才能被连接：ncc living --name x --kind service");
        return Ok(());
    }
    println!("可连接 {} 个节点", rows.len());
    for n in &rows {
        node_line(n, "  ");
        println!("     引用 {}/{}", s(n, "namespace"), s(n, "slug"));
    }
    println!("\n连接：ncc nodes link @命名空间/节点slug --label \"我给它的名字\"");
    Ok(())
}

/// `ncc nodes link @ns/slug --label …` —— 收进我的连接表并起个 Name 标签。
pub fn link(cfg: &CliConfig, a: &LinkArgs) -> Result<()> {
    let t = token(cfg)?;
    let body = json!({ "ref": a.node, "label": a.label, "note": a.note });
    let d = api::post_json(cfg, "/api/nodes/links", Some(&t), &body)?;
    println!("✅ 已连接节点：{}", s(&d["node"], "label"));
    println!("   连接 id {}", s(&d, "linkId"));
    println!("   类型 {}", kind_cn(s(&d["node"], "kind")));
    println!("   提示：连接不等于授权 —— 要取对方私有制品请用 `ncc grant set`。");
    Ok(())
}

/// `ncc nodes label <连接id> --label …` —— 改 Name 标签 / 备注。
pub fn label(cfg: &CliConfig, a: &LabelArgs) -> Result<()> {
    let t = token(cfg)?;
    if a.label.is_none() && a.note.is_none() {
        bail!("至少要给 --label 或 --note");
    }
    let body = json!({ "label": a.label, "note": a.note });
    let d = api::request(cfg, "PATCH", &format!("/api/nodes/links/{}", a.id), Some(&t), Some(&body), None, &[])?;
    println!("✅ 已更新连接 {}：{}", a.id, s(&d["node"], "label"));
    Ok(())
}

/// `ncc nodes unlink <连接id>` —— 断开（只删我这边的连接表条目）。
pub fn unlink(cfg: &CliConfig, id: &str) -> Result<()> {
    let t = token(cfg)?;
    api::del(cfg, &format!("/api/nodes/links/{}", id), Some(&t))?;
    println!("✅ 已断开连接 {id}（只影响我的连接表，对方节点不受影响）");
    Ok(())
}

/* ---------------- 区域聚合与推荐（Agent 面） ---------------- */

/// `ncc nodes region` —— 我的节点覆盖图（按归属者所在地聚合）。
pub fn region(cfg: &CliConfig) -> Result<()> {
    let t = token(cfg)?;
    let d = api::get(cfg, "/api/nodes/region-profile", Some(&t))?;
    println!(
        "节点 {} 个（自有 {} · 连接 {}），其中 {} 个可知所在地",
        d["total"].as_i64().unwrap_or(0),
        d["mineTotal"].as_i64().unwrap_or(0),
        d["linkTotal"].as_i64().unwrap_or(0),
        d["withRegion"].as_i64().unwrap_or(0)
    );
    let regions = d["regions"].as_array().cloned().unwrap_or_default();
    if regions.is_empty() {
        println!("还没有区域数据：节点的区域来自**归属者名片的所在地**。");
        return Ok(());
    }
    println!("\n区域覆盖");
    for r in &regions {
        let n = r["count"].as_i64().unwrap_or(0);
        let names: Vec<String> = r["nodes"]
            .as_array()
            .map(|ns| ns.iter().map(|x| s(x, "label").to_string()).collect())
            .unwrap_or_default();
        println!(
            "  {:<8} {} {}  {}",
            s(r, "region"),
            "█".repeat(std::cmp::min(n as usize, 20)),
            n,
            names.join(", ")
        );
    }
    if let Some(top) = d["topRegion"].as_str().filter(|x| !x.is_empty()) {
        println!("\n节点最密的区域：{top}");
    }
    if let Some(ks) = d["kinds"].as_object().filter(|m| !m.is_empty()) {
        let mut v: Vec<(&String, i64)> = ks.iter().map(|(k, n)| (k, n.as_i64().unwrap_or(0))).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let line: Vec<String> = v.iter().map(|(k, n)| format!("{}×{n}", kind_cn(k))).collect();
        println!("类型分布：{}", line.join("  "));
    }
    Ok(())
}

/// `ncc nodes recommend` —— 按区域推荐可连接的节点（同区域优先）。
pub fn recommend(cfg: &CliConfig, a: &RecommendArgs) -> Result<()> {
    let t = token(cfg)?;
    let mut qs = vec![format!("limit={}", a.limit)];
    if let Some(r) = a.region.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("region={}", api::urlenc(r)));
    }
    if let Some(k) = a.kind.as_deref().filter(|x| !x.is_empty()) {
        qs.push(format!("kind={}", api::urlenc(k)));
    }
    let d = api::get(cfg, &format!("/api/nodes/recommend?{}", qs.join("&")), Some(&t))?;
    let rows = d["nodes"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("没有可推荐的节点。");
        return Ok(());
    }
    println!("推荐 {} 个节点（同区域已有节点的排在前）", rows.len());
    for n in &rows {
        let same = n["regionNodes"].as_i64().unwrap_or(0);
        let mark = if same > 0 {
            format!("同区域已有 {same} 个节点")
        } else {
            String::new()
        };
        node_line(n, "  ");
        println!("     引用 {}/{}  {mark}", s(n, "namespace"), s(n, "slug"));
    }
    println!("\n连接：ncc nodes link @命名空间/节点slug --label \"…\"");
    Ok(())
}

/* ---------------- 授权（Grant） ----------------
   与节点连接分得很开：连接只代表「找得到」，授权才是「拿得到」。
   谁能取我的私有制品 / 看我的私有分享页，一律在这里显式给。
*/

#[derive(clap::Args)]
pub struct GrantListArgs {
    /// 我给出的授权（默认）
    #[arg(long)]
    pub out: bool,
    /// 别人给我的授权
    #[arg(long)]
    pub r#in: bool,
}

#[derive(clap::Args)]
pub struct GrantSetArgs {
    /// 被授权人：@用户名 或用户 id
    #[arg(long)]
    pub user: String,
    /// 授权类型（看指向哪个服务端）：
    /// 平台 ncc.ai：artifact（私有制品）| service（非公开服务）| share（私有分享页）| p2p（与我的私有节点直连）
    /// 内网 ncc-registry：artifact | node（私有节点）| config（非公开配置）
    #[arg(long, value_parser = ["artifact", "service", "share", "p2p", "node", "config"])]
    pub kind: String,
    /// 限定命名空间（缺省=该类型的全部；share 不支持限定）
    #[arg(long = "ns")]
    pub namespace: Option<String>,
    /// 备注
    #[arg(long)]
    pub note: Option<String>,
}

/* ---------------- 供 MCP 复用 ---------------- */

fn need_login(what: &str) -> anyhow::Error {
    anyhow::anyhow!("需要登录后{}（先 ncc login，或配置 API-Key）", what)
}

pub fn fetch_nodes(cfg: &CliConfig) -> Result<Value> {
    let t = config::token_opt(cfg).ok_or_else(|| need_login("查看节点"))?;
    api::get(cfg, "/api/nodes", Some(&t))
}

pub fn fetch_region_profile(cfg: &CliConfig) -> Result<Value> {
    let t = config::token_opt(cfg).ok_or_else(|| need_login("查看节点区域"))?;
    api::get(cfg, "/api/nodes/region-profile", Some(&t))
}

pub fn fetch_recommend(cfg: &CliConfig, region: &str, kind: &str, limit: u32) -> Result<Value> {
    let t = config::token_opt(cfg).ok_or_else(|| need_login("查看节点推荐"))?;
    let mut qs = vec![format!("limit={limit}")];
    if !region.is_empty() {
        qs.push(format!("region={}", api::urlenc(region)));
    }
    if !kind.is_empty() {
        qs.push(format!("kind={}", api::urlenc(kind)));
    }
    api::get(cfg, &format!("/api/nodes/recommend?{}", qs.join("&")), Some(&t))
}

pub fn fetch_discover(cfg: &CliConfig, kind: &str, limit: u32) -> Result<Value> {
    let t = config::token_opt(cfg).ok_or_else(|| need_login("发现节点"))?;
    let mut qs = vec![format!("limit={limit}")];
    if !kind.is_empty() {
        qs.push(format!("kind={}", api::urlenc(kind)));
    }
    api::get(cfg, &format!("/api/nodes/discover?{}", qs.join("&")), Some(&t))
}

pub fn fetch_grants(cfg: &CliConfig, direction: &str) -> Result<Value> {
    let t = config::token_opt(cfg).ok_or_else(|| need_login("查看授权"))?;
    let dir = if direction == "incoming" { "incoming" } else { "outgoing" };
    api::get(cfg, &format!("/api/grants?direction={dir}"), Some(&t))
}

/// `ncc grant list` —— 授权关系。
pub fn grant_list(cfg: &CliConfig, a: &GrantListArgs) -> Result<()> {
    let t = token(cfg)?;
    let dir = if a.r#in { "incoming" } else { "outgoing" };
    let d = api::get(cfg, &format!("/api/grants?direction={dir}"), Some(&t))?;
    let rows = d["grants"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        if dir == "outgoing" {
            println!("你还没有给出任何授权。");
            println!("授权别人取你的私有制品：ncc grant set --user @某人 --kind artifact");
        } else {
            println!("还没有人给你授权。");
        }
        return Ok(());
    }
    println!(
        "{}（{} 条）",
        if dir == "outgoing" { "我给出的授权" } else { "别人给我的授权" },
        rows.len()
    );
    for g in &rows {
        let kind = match s(g, "kind") {
            "artifact" => "制品下载",
            "service" => "非公开服务接入",
            "share" => "私有分享页",
            "p2p" => "私有节点直连",
            "node" => "私有节点可见",
            "config" => "非公开配置读取",
            other => other,
        };
        let ns = s(&g["namespace"], "slug");
        println!(
            "  {:<26} {:<12} {:<16} {}",
            s(g, "id"),
            kind,
            if ns.is_empty() { "全部命名空间" } else { ns },
            party_label(&g["grantee"])
        );
        let note = s(g, "note");
        if !note.is_empty() {
            println!("      备注 {note}");
        }
    }
    if dir == "outgoing" {
        println!("\n撤销：ncc grant rm <id>");
    }
    Ok(())
}

/// `ncc grant set --user @someone --kind artifact` —— 授予访问权。
pub fn grant_set(cfg: &CliConfig, a: &GrantSetArgs) -> Result<()> {
    let t = token(cfg)?;
    if a.kind == "share" && a.namespace.is_some() {
        bail!("share 授权不支持 --ns（分享页属于账号，不属于命名空间）");
    }
    let body = json!({
        "ref": a.user, // 服务端字段名是 ref（兼容 userId），不是 user
        "kind": a.kind,
        "namespace": a.namespace,
        "note": a.note,
    });
    let d = api::post_json(cfg, "/api/grants", Some(&t), &body)?;
    let g = &d["grant"];
    let kind = match s(g, "kind") {
        "artifact" => "可下载你的私有制品",
        "service" => "可接入你的非公开服务",
        "share" => "可查看你的私有分享页",
        "p2p" => "可与你的私有节点直连（P2P）",
        "node" => "可发现并连接你的私有节点",
        "config" => "可读取你的非公开配置",
        _ => "",
    };
    println!("✅ 已授权 {}：{}", party_label(&g["grantee"]), kind);
    println!("   id {}", s(g, "id"));
    let ns = s(&g["namespace"], "slug");
    println!("   范围 {}", if ns.is_empty() { "全部命名空间" } else { ns });
    println!("   撤销：ncc grant rm {}", s(g, "id"));
    Ok(())
}

/// `ncc grant rm <id>`
pub fn grant_rm(cfg: &CliConfig, id: &str) -> Result<()> {
    let t = token(cfg)?;
    api::del(cfg, &format!("/api/grants/{}", id), Some(&t))?;
    println!("✅ 已撤销授权 {id}（对方立即失去访问权）");
    Ok(())
}

/// 给 MCP 用的紧凑文本渲染（Agent 读文本，不是 JSON 表）。
pub fn render_nodes(v: &Value) -> String {
    let mine = v["mine"].as_array().cloned().unwrap_or_default();
    let links = v["links"].as_array().cloned().unwrap_or_default();
    let mut out = format!("我的节点（{}）\n", mine.len());
    let line = |n: &Value, out: &mut String| {
        let caps = arr(n.get("capabilities")).join(",");
        out.push_str(&format!(
            "- {} | id={} | 类型={} | 在线={} | 地址={} | 能力={} | 归属={} | 备注={}\n",
            s(n, "label"),
            s(n, "id"),
            s(n, "kind"),
            n["online"].as_bool().unwrap_or(false),
            s(n, "url"),
            caps,
            s(&n["owner"], "handle"),
            n.pointer("/link/note").and_then(|x| x.as_str()).unwrap_or("")
        ));
    };
    if mine.is_empty() {
        out.push_str("（无）\n");
    }
    for n in &mine {
        line(n, &mut out);
    }
    out.push_str(&format!("\n我连接的节点（{}）\n", links.len()));
    if links.is_empty() {
        out.push_str("（无）\n");
    }
    for n in &links {
        line(n, &mut out);
    }
    out
}

pub fn render_discover(v: &Value) -> String {
    let rows = v["nodes"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "本实例上没有可连接的新节点。".to_string();
    }
    let mut out = format!("可连接 {} 个节点\n", rows.len());
    for n in &rows {
        out.push_str(&format!(
            "- {} | 引用={}/{} | 类型={} | 在线={} | 地址={} | 归属={} | 区域={}\n",
            s(n, "label"),
            s(n, "namespace"),
            s(n, "slug"),
            s(n, "kind"),
            n["online"].as_bool().unwrap_or(false),
            s(n, "url"),
            s(&n["owner"], "handle"),
            s(&n["owner"], "region")
        ));
    }
    out
}

pub fn render_region_profile(v: &Value) -> String {
    let top = s(v, "topRegion");
    let mut out = format!(
        "节点 {} 个（自有 {} · 连接 {}），{} 个可知所在地，最密区域：{}\n",
        v["total"].as_i64().unwrap_or(0),
        v["mineTotal"].as_i64().unwrap_or(0),
        v["linkTotal"].as_i64().unwrap_or(0),
        v["withRegion"].as_i64().unwrap_or(0),
        if top.is_empty() { "（暂无）" } else { top }
    );
    for r in v["regions"].as_array().cloned().unwrap_or_default() {
        let names: Vec<String> = r["nodes"]
            .as_array()
            .map(|ns| ns.iter().map(|x| s(x, "label").to_string()).collect())
            .unwrap_or_default();
        out.push_str(&format!(
            "- {} × {}：{}\n",
            s(&r, "region"),
            r["count"].as_i64().unwrap_or(0),
            names.join(", ")
        ));
    }
    if let Some(ks) = v["kinds"].as_object().filter(|m| !m.is_empty()) {
        let mut pairs: Vec<(&String, i64)> = ks.iter().map(|(k, n)| (k, n.as_i64().unwrap_or(0))).collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        out.push_str(&format!(
            "类型分布：{}\n",
            pairs.iter().map(|(k, n)| format!("{k}×{n}")).collect::<Vec<_>>().join(" ")
        ));
    }
    out
}

pub fn render_recommend(v: &Value) -> String {
    let rows = v["nodes"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "没有可推荐的节点。".to_string();
    }
    let mut out = format!("推荐 {} 个节点（同区域已有节点的排在前）\n", rows.len());
    for n in &rows {
        out.push_str(&format!(
            "- {} | 引用={}/{} | 类型={} | 区域={} | 同区域已有节点={} | 地址={}\n",
            s(n, "label"),
            s(n, "namespace"),
            s(n, "slug"),
            s(n, "kind"),
            s(&n["owner"], "region"),
            n["regionNodes"].as_i64().unwrap_or(0),
            s(n, "url")
        ));
    }
    out
}

pub fn render_grants(v: &Value, direction: &str) -> String {
    let rows = v["grants"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return if direction == "incoming" {
            "没有人给你授权。".to_string()
        } else {
            "你还没有给出任何授权。".to_string()
        };
    }
    let mut out = format!(
        "{}（{} 条）\n",
        if direction == "incoming" { "别人给我的授权" } else { "我给出的授权" },
        rows.len()
    );
    for g in &rows {
        let ns = s(g, "namespace");
        out.push_str(&format!(
            "- id={} | 类型={} | 范围={} | 对象={} | 备注={}\n",
            s(g, "id"),
            s(g, "kind"),
            if ns.is_empty() { "全部命名空间" } else { ns },
            party_label(&g["grantee"]),
            s(g, "note")
        ));
    }
    out
}

/// 授权对象的显示名（handle 优先，其次展示名 / 账号名 / id）。
fn party_label(v: &Value) -> String {
    let h = s(v, "handle");
    let d = s(v, "displayName");
    let n = s(v, "name");
    match (h.is_empty(), d.is_empty(), n.is_empty()) {
        (false, false, _) => format!("{h} {d}"),
        (false, _, _) => h.to_string(),
        (_, false, _) => d.to_string(),
        (_, _, false) => n.to_string(),
        _ => s(v, "id").to_string(),
    }
}

/// 时间显示：RFC3339 → 到分钟（key 列表等复用）。
pub fn human_time(e: &str) -> String {
    match e.len() >= 16 {
        true => e[..16].replace('T', " "),
        false => e.to_string(),
    }
}
