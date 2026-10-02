// NCC 节点编号（NodeID）：`ncc-intra-aya-hz01` / `ncc-ai-aya-office-gw`。
//
// 编号回答的是**「这台机器叫什么、是谁的」** —— 一个稳定、可被人念出来、能跨机器核对的
// 名字（库里的主键 `LD-…` 只能给机器看）。三条边界（与服务端一致）：
//
// - **唯一性按人分域**：编号里带用户名，所以 aya 的 `hz01` 与 kai 的 `hz01` 是两个编号，
//   谁也挡不着谁；同一个人的同一个短码只有一次（重复申领会拿到 409）。
// - **申领 ≠ 授权**：拿到编号不代表能连别人、更不代表能取别人的数据 ——
//   那是 `ncc nodes link` 与 `ncc grant set` 的事。
// - **释放 ≠ 删除**：`ncc nodes release` 之后记录还在（历史引用不作废），
//   你以后可以用同一个短码捡回来。
//
// 编号申领在 ncc.ai（平台是权威）；节点在别处上报时把编号**报上去**，平台才知道这台机器是谁。
use crate::api;
use crate::config;
use crate::config::CliConfig;
use anyhow::{bail, Result};
use serde_json::{json, Value};

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct ClaimArgs {
    /// 前缀：intra（内网自托管节点）| ai（托管在 ncc.ai 的节点）
    #[arg(long, default_value = "ai", value_parser = ["intra", "ai"])]
    pub prefix: String,
    /// 自编短码：2-24 位小写字母 / 数字 / 连字符（HZ_01、hz.01 会归一成 hz-01）
    #[arg(long)]
    pub code: String,
    /// 备注：这台机器在哪、干什么用（给人看的）
    #[arg(long)]
    pub note: Option<String>,
    /// 只查看会得到什么编号，不真的申领
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(clap::Args)]
pub struct IdsArgs {
    /// 连释放过的一起列（默认只列还活着的）
    #[arg(long)]
    pub released: bool,
}

/* ---------------- 工具 ---------------- */

fn token(cfg: &CliConfig) -> Result<String> {
    config::require_token(cfg)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn status_cn(st: &str) -> &str {
    match st {
        "active" => "在用",
        "released" => "已释放",
        other => other,
    }
}

/// 一行编号：完整编号 + 状态 + 备注 + 最近使用。
fn id_line(v: &Value, indent: &str) {
    let full = s(v, "full");
    let st = s(v, "status");
    println!("{indent}{:<28} {:<8} {}", full, status_cn(st), s(v, "note"));
    if let Some(t) = v.get("lastUsedAt").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            println!("{indent}   最近上报 {t}");
        }
    }
}

/* ---------------- 申领 / 查看 / 释放 ---------------- */

/// `ncc nodes claim --prefix ai --code hz01` —— 申请一个节点编号。
pub fn claim(cfg: &CliConfig, a: &ClaimArgs) -> Result<()> {
    let t = token(cfg)?;
    let code = a.code.trim();
    if code.is_empty() {
        bail!("--code 不能为空（2-24 位小写字母 / 数字 / 连字符）");
    }
    if a.dry_run {
        let d = api::get(
            cfg,
            &format!(
                "/api/node-ids/preview?prefix={}&code={}",
                api::urlenc(&a.prefix),
                api::urlenc(code)
            ),
            Some(&t),
        )?;
        let full = s(&d, "full");
        if d["available"].as_bool().unwrap_or(false) {
            println!("✅ 可以用：{full}");
        } else {
            println!("✗ 现在拿不到 {full}");
            let why = s(&d, "reason");
            if !why.is_empty() {
                println!("  原因 {why}");
            }
            if d["reclaimable"].as_bool().unwrap_or(false) {
                println!("  你可以直接申领，会把之前释放的这个编号捡回来。");
            }
        }
        return Ok(());
    }
    let body = json!({ "prefix": a.prefix, "code": code, "note": a.note });
    let d = api::post_json(cfg, "/api/node-ids", Some(&t), &body)?;
    let n = &d["nodeId"];
    let full = s(n, "full");
    if d["reused"].as_bool().unwrap_or(false) {
        println!("✅ 已捡回之前释放的编号：{full}");
    } else {
        println!("✅ 已申领节点编号：{full}");
    }
    println!("   内部 id {} · 状态 {}", s(n, "id"), status_cn(s(n, "status")));
    println!("\n让这台机器用上它（上报时带上，平台才知道这台机器是谁）：");
    println!("  ncc living --name 我的机器 --kind service --node-id {full}");
    println!("\n提示：{}", s(&d, "hint"));
    Ok(())
}

/// `ncc nodes ids` —— 我的编号。
pub fn ids(cfg: &CliConfig, a: &IdsArgs) -> Result<()> {
    let t = token(cfg)?;
    let path = if a.released {
        "/api/node-ids?include=released"
    } else {
        "/api/node-ids"
    };
    let d = api::get(cfg, path, Some(&t))?;
    let rows = d["nodeIds"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("还没有节点编号。申请一个：ncc nodes claim --prefix ai --code hz01");
        println!("（前缀取值见 `ncc nodes prefixes`；编号形如 ncc-ai-<用户名>-<短码>）");
        return Ok(());
    }
    println!("我的节点编号（{}）", rows.len());
    for v in &rows {
        id_line(v, "  ");
    }
    println!(
        "\n每个前缀下最多 {} 个；短码归一化后判重，所以 HZ_01 与 hz-01 是同一个编号。",
        d["quotaPerPrefix"].as_i64().unwrap_or(20)
    );
    Ok(())
}

/// `ncc nodes id <编号>` —— 核对一个编号属于谁（不需要登录，公开可查）。
pub fn show(cfg: &CliConfig, r: &str) -> Result<()> {
    let d = api::get(cfg, &format!("/api/node-ids/{}", api::urlenc(r)), None)?;
    let n = &d["nodeId"];
    println!("编号 {} （{}）", s(n, "full"), status_cn(s(n, "status")));
    println!("  归属 {}", s(&n["owner"], "handle"));
    println!("  前缀 {} · 短码 {}", s(n, "prefix"), s(n, "code"));
    if !s(n, "note").is_empty() {
        println!("  备注 {}", s(n, "note"));
    }
    println!("  申领于 {}", s(n, "createdAt"));
    if !n["usable"].as_bool().unwrap_or(false) {
        println!("  ⚠ 这个编号已释放：记录保留（历史引用不作废），但不该再用它标识新节点。");
    }
    Ok(())
}

/// `ncc nodes release <编号>` —— 释放（记录保留，以后可以用同一短码捡回来）。
pub fn release(cfg: &CliConfig, r: &str) -> Result<()> {
    let t = token(cfg)?;
    let d = api::del(cfg, &format!("/api/node-ids/{}", api::urlenc(r)), Some(&t))?;
    println!("✅ 已释放 {}", s(&d["nodeId"], "full"));
    println!("   {}", s(&d, "hint"));
    Ok(())
}

/// `ncc nodes prefixes` —— 编号前缀目录（取值来源 + 规则）。
pub fn prefixes(cfg: &CliConfig) -> Result<()> {
    let t = token(cfg)?;
    let d = api::get(cfg, "/api/node-ids/prefixes", Some(&t))?;
    println!("节点编号前缀（形如 {}）", s(&d, "shape"));
    for p in d["prefixes"].as_array().cloned().unwrap_or_default() {
        println!("  {:<8} {:<12} {}", s(&p, "id"), s(&p, "zh"), s(&p, "descZh"));
        println!("  {:<8} 例 {}", "", s(&p, "example"));
    }
    println!("\n规则");
    for r in d["rules"].as_array().cloned().unwrap_or_default() {
        if let Some(r) = r.as_str() {
            println!("  · {r}");
        }
    }
    println!(
        "\n每个前缀下最多 {} 个（申领在 ncc.ai）；短码判重在归一化之后。",
        d["quotaPerPrefix"].as_i64().unwrap_or(20)
    );
    println!("用法：ncc nodes claim --prefix ai --code hz01 [--note \"机房在杭州\"]");
    Ok(())
}
