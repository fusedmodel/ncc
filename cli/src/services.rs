// NCC Service：服务提供方（公司 / 连锁集团）打包声明的**对外服务**。
//
// 与节点、授权的分工（与服务端语义一致，别在这里加"聪明"推断）：
//   - 节点（ncc nodes）解决「我连得到谁」：找得到；
//   - 服务（本文件）解决「这门业务该找谁、怎么接、要不要授权」；
//   - 授权（ncc grant）解决「拿不拿得到」：非公开服务的接入细节要 kind=service 的授权。
//
// Agent 面的主入口是 `ncc services match "<意图>"`：服务端按匹配策略打分并解释理由，
// 顺带给出接入步骤；提供方面是 `ncc services add` / `rm` / `--mine` 查看自己的服务。
use crate::api::{self, urlenc};
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Result};
use serde_json::{json, Value};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct ListArgs {
    /// 业务分类 id（见 `ncc services catalog`）
    #[arg(long)]
    pub category: Option<String>,
    /// 能力标签
    #[arg(long)]
    pub tag: Option<String>,
    /// 覆盖区域，如 杭州 / 全国
    #[arg(long)]
    pub region: Option<String>,
    /// 接入方式：http | openapi | mcp | artifact | human
    #[arg(long)]
    pub protocol: Option<String>,
    /// 关键词（名称 / 摘要 / 匹配策略 / 标签）
    #[arg(long)]
    pub q: Option<String>,
    /// 只列我自己声明的服务（含草稿 / 定向）
    #[arg(long)]
    pub mine: bool,
    /// 返回条数
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    /// 输出原始 JSON（给脚本/Agent 用）
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct MatchArgs {
    /// 自然语言意图，如 "帮我订杭州的酒店"
    pub intent: String,
    /// 按业务分类过滤（见 `ncc services catalog`）
    #[arg(long)]
    pub category: Option<String>,
    /// 能力标签（可重复）
    #[arg(long)]
    pub tag: Vec<String>,
    /// 区域
    #[arg(long)]
    pub region: Option<String>,
    /// 接入方式偏好
    #[arg(long)]
    pub protocol: Option<String>,
    /// 返回条数（默认 5）
    #[arg(long, default_value_t = 5)]
    pub limit: u32,
    /// 输出原始 JSON（含 score / reasons / howToUse）
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ShowArgs {
    /// 服务引用：@提供方/slug 或 SV-… id
    pub target: String,
    /// 输出原始 JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct AddArgs {
    /// 服务名称
    #[arg(long)]
    pub name: String,
    /// 业务分类 id（见 `ncc services catalog`）
    #[arg(long)]
    pub category: String,
    /// 对外标识，引用写作 @你/标识（缺省由名称生成）
    #[arg(long)]
    pub slug: Option<String>,
    /// 一句话说明
    #[arg(long)]
    pub summary: Option<String>,
    /// 详细说明
    #[arg(long)]
    pub description: Option<String>,
    /// 能力标签（可重复）
    #[arg(long)]
    pub tag: Vec<String>,
    /// 匹配关键词：Agent 用什么话术能召到它（可重复）
    #[arg(long)]
    pub intent: Vec<String>,
    /// 匹配策略：什么情况下该找我（给 Agent 读）
    #[arg(long)]
    pub r#match: Option<String>,
    /// 覆盖区域，如 "杭州 · 上海" / 全国（缺省 = 不限）
    #[arg(long)]
    pub region: Option<String>,
    /// 支持语言（可重复，缺省 zh）
    #[arg(long)]
    pub lang: Vec<String>,
    /// 接入方式：http | openapi | mcp | artifact | human（缺省 http）
    #[arg(long)]
    pub protocol: Option<String>,
    /// 调用地址 / 规范地址
    #[arg(long)]
    pub endpoint: Option<String>,
    /// 关联执行节点：@命名空间/节点slug 或 LD-… id
    #[arg(long)]
    pub node: Option<String>,
    /// 关联能力包（Registry 条目）：@命名空间/slug 或 R-… id
    #[arg(long)]
    pub item: Option<String>,
    /// 响应与时效承诺
    #[arg(long)]
    pub sla: Option<String>,
    /// 计费说明
    #[arg(long)]
    pub pricing: Option<String>,
    /// 使用条款 / 限制
    #[arg(long)]
    pub terms: Option<String>,
    /// 每日可接单量（0 = 不限）
    #[arg(long, default_value_t = 0)]
    pub capacity: i64,
    /// 授权方式：open（公开可调）| grant（需授权）| invite（定向）
    #[arg(long, default_value = "open")]
    pub access: String,
    /// 可见性：public（进目录）| unlisted（仅直链 / 被授权方）
    #[arg(long, default_value = "public")]
    pub visibility: String,
    /// 直接发布（缺省存成 draft，确认后再发布）
    #[arg(long)]
    pub publish: bool,
}

#[derive(clap::Args)]
pub struct RmArgs {
    /// 服务 id（SV-…，见 `ncc services list --mine`）
    pub id: String,
}

/* ---------------- 小工具 ---------------- */

fn token(cfg: &CliConfig) -> Result<String> {
    config::require_token(cfg)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn arr(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str()).map(String::from).collect())
        .unwrap_or_default()
}

fn protocol_cn(p: &str) -> &str {
    match p {
        "http" => "HTTP",
        "openapi" => "OpenAPI",
        "mcp" => "MCP",
        "artifact" => "能力包",
        "human" => "人工服务",
        other => other,
    }
}

fn access_cn(a: &str) -> &str {
    match a {
        "open" => "公开可调",
        "grant" => "需授权",
        "invite" => "定向开放",
        other => other,
    }
}

/// 服务一行摘要（列表 / 匹配共用）。
/// `score` 为 None 时不打印分数（纯列表场景没有匹配分）。
fn service_line(v: &Value, indent: &str, score: Option<i64>) {
    let name = s(v, "name");
    let cat = s(&v["categoryLabel"], "zh");
    let proto = protocol_cn(s(v, "protocol"));
    let access = access_cn(s(v, "access"));
    let region = if s(v, "region").is_empty() { "不限区域" } else { s(v, "region") };
    match score {
        Some(sc) => println!("{indent}[{sc:>3}] {name}  ·  {cat}  ·  {proto}  ·  {access}  ·  {region}"),
        None => println!("{indent}{name}  ·  {cat}  ·  {proto}  ·  {access}  ·  {region}"),
    }
    println!("{indent}      引用 {}   提供方 {}", s(v, "ref"), s(&v["provider"], "displayName"));
    let summary = s(v, "summary");
    if !summary.is_empty() {
        println!("{indent}      {summary}");
    }
    let tags = arr(v.get("tags"));
    if !tags.is_empty() {
        println!("{indent}      标签 {}", tags.join(", "));
    }
    let reasons = arr(v.get("reasons"));
    if !reasons.is_empty() {
        println!("{indent}      命中 {}", reasons.join("；"));
    }
    // 接入方式：解锁时给步骤，未解锁时给申请办法
    let how = &v["howToUse"];
    let steps = arr(how.get("steps"));
    if !steps.is_empty() {
        println!("{indent}      怎么接：");
        for (i, st) in steps.iter().enumerate() {
            println!("{indent}        {}. {}", i + 1, st);
        }
    }
    let hint = s(how, "requestHint");
    if !hint.is_empty() {
        println!("{indent}      需授权：让提供方执行 {hint}");
    }
    if let Some(st) = v["status"].as_str() {
        if st != "published" {
            println!("{indent}      状态 {st}");
        }
    }
}

fn qs_of(pairs: Vec<(&str, Option<String>)>) -> String {
    let parts: Vec<String> = pairs
        .into_iter()
        .filter_map(|(k, v)| v.filter(|x| !x.is_empty()).map(|x| format!("{k}={}", urlenc(&x))))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/* ---------------- 目录与列表 ---------------- */

/// `ncc services catalog` —— 业务分类 / 接入方式 / 授权方式目录。
pub fn catalog(cfg: &CliConfig, json_out: bool) -> Result<()> {
    let d = api::get(cfg, "/api/services/catalog", None)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("业务分类（{} 类）", d["categories"].as_array().map(|a| a.len()).unwrap_or(0));
    let groups = d["groups"].as_array().cloned().unwrap_or_default();
    for g in &groups {
        println!("  {} · {}", s(g, "id"), s(g, "zh"));
        for c in d["categories"].as_array().cloned().unwrap_or_default() {
            if s(&c, "group") != s(g, "id") {
                continue;
            }
            let n = c["count"].as_i64().unwrap_or(0);
            let mark = if n > 0 { format!("（{n}）") } else { String::new() };
            println!("      {:<12} {}{}  {}", s(&c, "id"), s(&c, "zh"), mark, s(&c, "descZh"));
        }
    }
    println!("\n接入方式");
    for p in d["protocols"].as_array().cloned().unwrap_or_default() {
        println!("  {:<10} {:<16} {}", s(&p, "id"), s(&p, "zh"), s(&p, "descZh"));
    }
    println!("\n授权方式");
    for a in d["accessModes"].as_array().cloned().unwrap_or_default() {
        println!("  {:<10} {:<16} {}", s(&a, "id"), s(&a, "zh"), s(&a, "descZh"));
    }
    println!("\n目录里已有 {} 条公开服务，来自 {} 个提供方。", d["total"], d["providers"]);
    println!("找服务：ncc services match \"你想做什么\"   或   ncc services list --category booking");
    Ok(())
}

/// `ncc services [list]` —— 浏览公开服务目录，或 `--mine` 看自己的。
pub fn list(cfg: &CliConfig, a: &ListArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut qs = qs_of(vec![
        ("category", a.category.clone()),
        ("tag", a.tag.clone()),
        ("region", a.region.clone()),
        ("protocol", a.protocol.clone()),
        ("q", a.q.clone()),
    ]);
    if a.mine {
        qs = if qs.is_empty() {
            "?mine=1".to_string()
        } else {
            format!("{qs}&mine=1")
        };
    }
    let d = api::get(cfg, &format!("/api/services{qs}"), t.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d["services"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Value> = rows.into_iter().take(a.limit as usize).collect();
    if rows.is_empty() {
        if a.mine {
            println!("你还没有声明对外服务。声明一条：");
            println!("  ncc services add --name \"酒店预订中台\" --category booking --protocol openapi \\");
            println!("      --endpoint https://api.example.com/openapi.json --tag 预订 --intent 订酒店 --publish");
        } else {
            println!("没有符合条件的服务。业务分类见：ncc services catalog");
        }
        return Ok(());
    }
    println!("{}（{} 条）", if a.mine { "我声明的对外服务" } else { "对外服务" }, rows.len());
    for v in &rows {
        service_line(v, "  ", None);
        if a.mine {
            println!("      id {}", s(v, "id"));
        }
    }
    if a.mine {
        println!("\n删除：ncc services rm <id>");
    } else {
        println!("\n按意图找：ncc services match \"你想做什么\"；看细节：ncc services show @提供方/标识");
    }
    Ok(())
}

/// `ncc services match "<意图>"` —— 按匹配策略找服务（Agent 面主入口）。
///
/// 排序与解释都在服务端做（与 MCP / 网页同一套），CLI 只负责呈现，
/// 免得「换个入口顺序就变了」。
pub fn match_intent(cfg: &CliConfig, a: &MatchArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let mut pairs: Vec<(String, String)> = vec![
        ("intent".into(), a.intent.clone()),
        ("limit".into(), a.limit.to_string()),
    ];
    for (k, v) in [
        ("category", a.category.clone()),
        ("region", a.region.clone()),
        ("protocol", a.protocol.clone()),
    ] {
        if let Some(x) = v.filter(|x| !x.is_empty()) {
            pairs.push((k.into(), x));
        }
    }
    let mut qs: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={}", urlenc(v))).collect();
    for tag in a.tag.iter().filter(|x| !x.is_empty()) {
        qs.push(format!("tag={}", urlenc(tag)));
    }
    let d = api::get(cfg, &format!("/api/services/match?{}", qs.join("&")), t.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d["services"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("没有匹配到服务：{}", a.intent);
        println!("换个说法，或用分类浏览：ncc services catalog / ncc services list --category <分类>");
        return Ok(());
    }
    println!("匹配「{}」→ {} 条", a.intent, rows.len());
    for v in &rows {
        service_line(v, "  ", v["score"].as_i64());
    }
    // 只有真有需授权的服务时才提授权办法，否则是无用的噪音
    if rows.iter().any(|v| v["granted"].as_bool() == Some(false) && v["unlocked"].as_bool() == Some(false)) {
        println!("\n上面标「需授权」的服务：让提供方执行 ncc grant set --user @你 --kind service 后即可看到接入细节");
    }
    Ok(())
}

/// `ncc services show @提供方/slug` —— 一条服务的完整接入信息。
pub fn show(cfg: &CliConfig, a: &ShowArgs) -> Result<()> {
    let t = config::token_opt(cfg);
    let path = if a.target.starts_with("SV-") {
        // id 引用单独一条路由（避免与 /:provider/:slug 的位置冲突）
        format!("/api/services/id/{}", urlenc(&a.target))
    } else {
        let parts: Vec<&str> = a.target.trim_start_matches('@').splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            bail!("服务引用写成 @提供方/slug（或 SV-… id），如 ncc services show @aya/hotel-booking");
        }
        format!("/api/services/{}/{}", urlenc(parts[0]), urlenc(parts[1]))
    };
    let d = api::get(cfg, &path, t.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let v = &d["service"];
    service_line(v, "", None);
    let desc = s(v, "description");
    if !desc.is_empty() {
        println!("  {desc}");
    }
    let policy = s(v, "matchPolicy");
    if !policy.is_empty() {
        println!("\n  何时找我：{policy}");
    }
    let intents = arr(v.get("intents"));
    if !intents.is_empty() {
        println!("  匹配词：{}", intents.join(", "));
    }
    let how = &v["howToUse"];
    for (k, label) in [("sla", "响应承诺"), ("pricing", "计费"), ("terms", "条款")] {
        let x = s(how, k);
        if !x.is_empty() {
            println!("  {label}：{x}");
        }
    }
    let node = &v["node"];
    if !node.is_null() && node["missing"].is_null() {
        println!(
            "  执行节点：{}（{}，{}）",
            s(node, "ref"),
            s(node, "name"),
            if node["online"].as_bool().unwrap_or(false) { "在线" } else { "离线" }
        );
    }
    let item = &v["item"];
    if !item.is_null() && item["missing"].is_null() {
        println!("  能力包：{}@{}", s(item, "ref"), s(item, "version"));
    }
    let hint = s(how, "requestHint");
    if !hint.is_empty() {
        println!("\n  🔒 需授权：让提供方执行 {hint}");
    }
    let profile = s(&v["provider"], "profileUrl");
    println!("\n  提供方名片：{}{}", cfg.base_url().trim_end_matches('/'), profile);
    Ok(())
}

/* ---------------- 提供方：声明与删除 ---------------- */

/// 把 `@命名空间/slug` 的制品引用解析成条目 id（服务端存的是 id）。
fn resolve_item_id(cfg: &CliConfig, t: &str, r: &str) -> Result<String> {
    let r = r.trim();
    if r.starts_with("R-") {
        return Ok(r.to_string());
    }
    let parts: Vec<&str> = r.trim_start_matches('@').splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!("制品引用写成 @命名空间/slug 或 R-… id");
    }
    let d = api::get(cfg, &format!("/api/registry/@{}/{}", urlenc(parts[0]), urlenc(parts[1])), Some(t))?;
    let id = s(&d["item"], "id").to_string();
    if id.is_empty() {
        bail!("找不到制品 {r}");
    }
    Ok(id)
}

/// `ncc services add …` —— 声明一条对外服务（缺省 draft + open）。
pub fn add(cfg: &CliConfig, a: &AddArgs) -> Result<()> {
    let t = token(cfg)?;
    let item_id = match a.item.as_deref().filter(|x| !x.is_empty()) {
        Some(r) => Some(resolve_item_id(cfg, &t, r)?),
        None => None,
    };
    // 节点两种写法都收：LD-… 是 id，@命名空间/slug 交给服务端解析
    let (node_id, node_ref) = match a.node.as_deref().filter(|x| !x.is_empty()) {
        Some(x) if x.starts_with("LD-") => (Some(x.to_string()), None),
        Some(x) => (None, Some(x.to_string())),
        None => (None, None),
    };
    let body = json!({
        "name": a.name,
        "category": a.category,
        "slug": a.slug,
        "summary": a.summary,
        "description": a.description,
        "tags": a.tag,
        "intents": a.intent,
        "matchPolicy": a.r#match,
        "region": a.region,
        "languages": if a.lang.is_empty() { vec!["zh".to_string()] } else { a.lang.clone() },
        "protocol": a.protocol,
        "endpoint": a.endpoint,
        "nodeId": node_id,
        "nodeRef": node_ref,
        "itemId": item_id,
        "sla": a.sla,
        "pricing": a.pricing,
        "terms": a.terms,
        "capacity": a.capacity,
        "accessMode": a.access,
        "visibility": a.visibility,
        "status": if a.publish { "published" } else { "draft" },
    });
    let d = api::post_json(cfg, "/api/profile/me/services", Some(&t), &body)?;
    let v = &d["service"];
    println!("✅ 已声明服务 {}", s(v, "ref"));
    println!("   名称 {}", s(v, "name"));
    println!("   状态 {}   授权 {}   接入 {}", s(v, "status"), access_cn(s(v, "access")), protocol_cn(s(v, "protocol")));
    if s(v, "status") != "published" {
        println!("   发布给其他 Agent（Agent 也能找到它）：");
        println!("     PATCH 状态或重新 add（当前仅你自己可见）");
    }
    println!("\n验证 Agent 能不能匹配到它：");
    println!("  ncc services match \"{}\"", s(v, "name"));
    Ok(())
}

/// `ncc services rm <id>` —— 下架一条服务。
pub fn rm(cfg: &CliConfig, a: &RmArgs) -> Result<()> {
    let t = token(cfg)?;
    api::del(cfg, &format!("/api/profile/me/services/{}", urlenc(&a.id)), Some(&t))?;
    println!("✅ 已下架服务 {}（其他 Agent 立即无法接入）", a.id);
    Ok(())
}

/* ---------------- 供 MCP 复用 ----------------
   Agent 读的是文本，不是 JSON 表：这里给一对 fetch / render 函数，
   MCP 与 CLI 共用同一份渲染，避免两个入口说的不一样。
*/

pub fn fetch_list(cfg: &CliConfig, category: &str, tag: &str, region: &str, limit: u32) -> Result<Value> {
    let t = config::token_opt(cfg);
    let qs = qs_of(vec![
        ("category", Some(category.to_string())),
        ("tag", Some(tag.to_string())),
        ("region", Some(region.to_string())),
        ("limit", Some(limit.to_string())),
    ]);
    api::get(cfg, &format!("/api/services{qs}"), t.as_deref())
}

pub fn fetch_match(cfg: &CliConfig, intent: &str, category: &str, tags: &[String], region: &str, limit: u32) -> Result<Value> {
    let t = config::token_opt(cfg);
    let mut qs = vec![
        format!("intent={}", urlenc(intent)),
        format!("limit={limit}"),
    ];
    for (k, v) in [("category", category), ("region", region)] {
        if !v.is_empty() {
            qs.push(format!("{k}={}", urlenc(v)));
        }
    }
    for tag in tags.iter().filter(|x| !x.is_empty()) {
        qs.push(format!("tag={}", urlenc(tag)));
    }
    api::get(cfg, &format!("/api/services/match?{}", qs.join("&")), t.as_deref())
}

pub fn fetch_show(cfg: &CliConfig, target: &str) -> Result<Value> {
    let t = config::token_opt(cfg);
    let path = if target.starts_with("SV-") {
        format!("/api/services/id/{}", urlenc(target))
    } else {
        let parts: Vec<&str> = target.trim_start_matches('@').splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("服务引用写成 @提供方/slug 或 SV-… id");
        }
        format!("/api/services/{}/{}", urlenc(parts[0]), urlenc(parts[1]))
    };
    api::get(cfg, &path, t.as_deref())
}

/// 把匹配结果渲染成 Agent 可读文本（含理由与接入步骤）。
pub fn render_match(v: &Value) -> String {
    let rows = v["services"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return format!(
            "没有匹配到服务（意图：{}）。可以用 ncc_list_services 浏览，或换个说法。",
            v["intent"].as_str().unwrap_or("")
        );
    }
    let mut out = String::new();
    out.push_str(&format!(
        "匹配「{}」→ {} 条（分数越高越相关；理由来自服务端匹配策略）：\n",
        v["intent"].as_str().unwrap_or(""),
        rows.len()
    ));
    for x in &rows {
        out.push_str(&format!(
            "\n· {}（{}）  引用 {}  提供方 {}  分数 {}\n",
            x["name"].as_str().unwrap_or(""),
            x["categoryLabel"]["zh"].as_str().unwrap_or(""),
            x["ref"].as_str().unwrap_or(""),
            x["provider"]["displayName"].as_str().unwrap_or(""),
            x["score"].as_i64().unwrap_or(0)
        ));
        if let Some(sum) = x["summary"].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("  摘要：{sum}\n"));
        }
        let reasons = arr(x.get("reasons"));
        if !reasons.is_empty() {
            out.push_str(&format!("  命中：{}\n", reasons.join("；")));
        }
        out.push_str(&format!(
            "  接入：{} · {} · 区域 {}\n",
            protocol_cn(x["protocol"].as_str().unwrap_or("")),
            access_cn(x["access"].as_str().unwrap_or("")),
            x["region"].as_str().filter(|s| !s.is_empty()).unwrap_or("不限")
        ));
        let steps = arr(x["howToUse"].get("steps"));
        if !steps.is_empty() {
            out.push_str("  怎么接：\n");
            for (i, st) in steps.iter().enumerate() {
                out.push_str(&format!("    {}. {st}\n", i + 1));
            }
        }
        if let Some(h) = x["howToUse"]["requestHint"].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("  需授权：让提供方执行 {h}\n"));
        }
    }
    out.push_str("\n细节可用 ncc_get_service 取（含端点与条款）。");
    out
}

pub fn render_list(v: &Value) -> String {
    let rows = v["services"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "目录里没有符合条件的服务。业务分类见 ncc_service_categories。".to_string();
    }
    let mut out = format!("对外服务 {} 条：\n", rows.len());
    for x in &rows {
        out.push_str(&format!(
            "\n· {}（{}）  引用 {}  提供方 {}  {} · {} · {}\n",
            x["name"].as_str().unwrap_or(""),
            x["categoryLabel"]["zh"].as_str().unwrap_or(""),
            x["ref"].as_str().unwrap_or(""),
            x["provider"]["displayName"].as_str().unwrap_or(""),
            protocol_cn(x["protocol"].as_str().unwrap_or("")),
            access_cn(x["access"].as_str().unwrap_or("")),
            x["region"].as_str().filter(|s| !s.is_empty()).unwrap_or("不限区域")
        ));
        if let Some(sum) = x["summary"].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("  {sum}\n"));
        }
        if let Some(p) = x["matchPolicy"].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("  何时找我：{p}\n"));
        }
    }
    out
}

pub fn render_show(v: &Value) -> String {
    let s1 = &v["service"];
    if s1.is_null() {
        return "服务不存在。".to_string();
    }
    let mut out = format!(
        "{}（{}）\n引用 {}  提供方 {}（{}）\n接入 {} · {} · 区域 {}\n",
        s1["name"].as_str().unwrap_or(""),
        s1["categoryLabel"]["zh"].as_str().unwrap_or(""),
        s1["ref"].as_str().unwrap_or(""),
        s1["provider"]["displayName"].as_str().unwrap_or(""),
        s1["provider"]["handle"].as_str().unwrap_or(""),
        protocol_cn(s1["protocol"].as_str().unwrap_or("")),
        access_cn(s1["access"].as_str().unwrap_or("")),
        s1["region"].as_str().filter(|s| !s.is_empty()).unwrap_or("不限")
    );
    for k in ["summary", "description", "matchPolicy"] {
        if let Some(x) = s1[k].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("{k}: {x}\n"));
        }
    }
    let tags = arr(s1.get("tags"));
    if !tags.is_empty() {
        out.push_str(&format!("标签：{}\n", tags.join(", ")));
    }
    let steps = arr(s1["howToUse"].get("steps"));
    if !steps.is_empty() {
        out.push_str("怎么接：\n");
        for (i, st) in steps.iter().enumerate() {
            out.push_str(&format!("  {}. {st}\n", i + 1));
        }
    }
    if let Some(h) = s1["howToUse"]["requestHint"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(&format!("需授权：让提供方执行 {h}\n"));
    }
    for (k, label) in [("sla", "响应承诺"), ("pricing", "计费"), ("terms", "条款")] {
        if let Some(x) = s1["howToUse"][k].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("{label}：{x}\n"));
        }
    }
    out
}
