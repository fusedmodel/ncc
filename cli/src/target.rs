// 目标（target）—— `ncc target …`
//
// 一台机器上通常同时连着两个世界：**云端**（ncc.ai 公共目录）与若干**内网 registry 节点**。
// 每个目标各自一份 base + 凭据，互不覆盖：
//
//	ncc target list                  # 我有哪些目标、现在在哪个、各自声明了什么能力
//	ncc target use office            # 切过去（只影响当前目标，不动别人的登录态）
//	ncc target add office --base http://10.0.0.5:8282
//	ncc target rm lab
//	ncc target show                  # 当前目标的详情（含能力与登录身份）
//
// 目标之外的三种「临时指向」：
//
//	--target <名字>   本次命令用这个目标
//	--base <URL>      本次命令用这个地址（已存在同名目标就复用，否则新建，见 main.rs）
//	ncc hub …         等价于「本次用 hub 目标」的糖（云端命令最好写成 ncc hub services match）
use crate::api;
use crate::capability;
use crate::config::{self, CliConfig, Target};
use anyhow::Result;
use serde_json::json;

#[derive(clap::Args)]
pub struct TargetArgs {
    #[command(subcommand)]
    pub action: Option<TargetAction>,
}

#[derive(clap::Subcommand)]
pub enum TargetAction {
    /// 列出全部目标（当前目标、kind、地址、登录身份、声明能力）
    List {
        #[arg(long)]
        json: bool,
        /// 不联网探测能力（离线时用本地记住的）
        #[arg(long)]
        offline: bool,
    },
    /// 看当前目标（可指定名字）
    Show {
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 切换当前目标
    Use {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// 新建/更新目标
    Add {
        name: String,
        /// 节点地址（缺省沿用当前目标）
        #[arg(long)]
        base: Option<String>,
        /// hub（云端公共目录）| node（内网 registry 节点）；缺省自动探测
        #[arg(long, value_parser = ["hub", "node"])]
        kind: Option<String>,
        /// 建好后立即切换过去
        #[arg(long)]
        r#use: bool,
        #[arg(long)]
        json: bool,
    },
    /// 删除目标（连同它保存的凭据）
    Rm {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cfg: &mut CliConfig, a: &TargetArgs) -> Result<()> {
    match &a.action {
        None | Some(TargetAction::List { .. }) => {
            let (json_out, offline) = match &a.action {
                Some(TargetAction::List { json, offline }) => (*json, *offline),
                _ => (false, false),
            };
            list(cfg, json_out, offline)
        }
        Some(TargetAction::Show { name, json }) => show(cfg, name.as_deref(), *json),
        Some(TargetAction::Use { name, json }) => r#use(cfg, name, *json),
        Some(TargetAction::Add { name, base, kind, r#use, json }) => {
            add(cfg, name, base.as_deref(), kind.as_deref(), *r#use, *json)
        }
        Some(TargetAction::Rm { name, json }) => rm(cfg, name, *json),
    }
}

/// 目标的能力（能联网就探测，顺便把结果记进配置；否则用记住的）。
fn meta_of(cfg: &CliConfig, name: &str, offline: bool) -> capability::Meta {
    let base = match cfg.by_name(name) {
        Some(t) => t.base_url.clone(),
        None => return capability::Meta::default(),
    };
    let mut cfg2 = cfg.clone();
    cfg2.current = Some(name.to_string());
    if offline {
        let caps = cfg.by_name(name).map(|t| t.capabilities.clone()).unwrap_or_default();
        let product = cfg.by_name(name).and_then(|t| t.product.clone()).unwrap_or_default();
        let kind = cfg.by_name(name).map(|t| t.kind.clone()).unwrap_or_default();
        return capability::Meta {
            product,
            kind,
            capabilities: caps,
            note: Some("未联网（用了本地记住的自述）".to_string()),
            ..capability::Meta::default()
        };
    }
    let m = capability::probe(&cfg2);
    if !m.product.is_empty() {
        capability::remember(&mut cfg2, &m);
        if let Some(t) = cfg2.targets.get_mut(name) {
            let _ = (&t.base_url, &t.capabilities); // 保持 t 与 cfg2 同步（下面直接复用 cfg2）
        }
    }
    let _ = base;
    m
}

fn list(cfg: &mut CliConfig, json_out: bool, offline: bool) -> Result<()> {
    let current = cfg.current_name();
    let names = cfg.names();
    // 先探测，再把结果写回配置（下次离线也有）—— 然后**用写回后的目标**渲染，
    // 否则第一次探测到的 kind 会晚一拍才显示出来。
    let mut metas = Vec::new();
    for name in &names {
        let m = meta_of(cfg, name, offline);
        metas.push((name.clone(), m));
    }
    for (name, m) in &metas {
        if !m.product.is_empty() {
            let mut tmp = cfg.clone();
            tmp.current = Some(name.clone());
            capability::remember(&mut tmp, m);
            if let Some(slot) = tmp.targets.get(name) {
                cfg.targets.insert(name.clone(), slot.clone());
            }
        }
    }
    let _ = config::save(cfg);
    let rows: Vec<(String, Target, capability::Meta)> = names
        .iter()
        .map(|n| {
            let t = cfg.by_name(n).cloned().unwrap_or_default();
            let m = metas
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, m)| m.clone())
                .unwrap_or_default();
            (n.clone(), t, m)
        })
        .collect();

    if json_out {
        let arr: Vec<_> = rows
            .iter()
            .map(|(name, t, m)| {
                json!({
                    "name": name,
                    "current": name == &current,
                    "kind": t.kind_label(),
                    "base": t.base_url,
                    "loggedIn": t.logged_in(),
                    "email": t.email,
                    "product": m.product,
                    "version": m.version,
                    "capabilities": m.capabilities,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::Value::Array(arr))?);
        return Ok(());
    }

    println!("当前目标：{current}");
    println!("{:<12} {:<9} {:<34} {:<16} 能力", "名字", "kind", "地址", "登录");
    for (name, t, m) in &rows {
        let mark = if name == &current { "*" } else { " " };
        let login = if t.logged_in() {
            t.email.clone().unwrap_or_else(|| "已登录".into())
        } else {
            "未登录".to_string()
        };
        let caps = if m.is_unknown() {
            "（未声明）".to_string()
        } else {
            m.capabilities.join(" ")
        };
        println!(
            "{mark:<1} {:<11} {:<9} {:<34} {:<16} {}",
            name,
            t.kind_label(),
            t.base_url,
            login,
            caps
        );
    }
    if rows.iter().any(|(_, _, m)| m.note.is_some()) {
        println!();
        for (name, _, m) in &rows {
            if let Some(n) = &m.note {
                println!("· {name}: {n}");
            }
        }
    }
    println!("\n切换：ncc target use <名字> · 新建：ncc target add office --base http://10.0.0.5:8282");
    println!("一次性的：ncc --target <名字> … / ncc --base <URL> … / ncc hub …");
    Ok(())
}

fn show(cfg: &CliConfig, name: Option<&str>, json_out: bool) -> Result<()> {
    let name = name.map(|s| s.to_string()).unwrap_or_else(|| cfg.current_name());
    let t = cfg
        .by_name(&name)
        .ok_or_else(|| anyhow::anyhow!("没有名为 {name} 的目标（ncc target list）"))?;
    let m = meta_of(cfg, &name, false);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": name,
                "kind": t.kind_label(),
                "base": t.base_url,
                "loggedIn": t.logged_in(),
                "email": t.email,
                "adminKey": t.admin_key,
                "hasAdminSecret": t.admin_secret.is_some(),
                "product": m.product,
                "version": m.version,
                "about": m.about,
                "capabilities": m.capabilities,
                "note": m.note,
            }))?
        );
        return Ok(());
    }
    println!("目标     {name}{}", if name == cfg.current_name() { "（当前）" } else { "" });
    println!("kind     {}{}", t.kind_label(), if t.is_node() { "（内网 registry 节点）" } else { "（云端公共目录）" });
    println!("地址     {}", t.base_url);
    if !m.product.is_empty() {
        println!("服务     {} {}", m.product, m.version);
    }
    if !m.about.is_empty() {
        println!("自我介绍 {}", m.about);
    }
    println!(
        "登录     {}",
        if t.logged_in() {
            format!("是（{}）", t.email.clone().unwrap_or_else(|| "?".into()))
        } else {
            "否（ncc login）".to_string()
        }
    );
    if t.admin_key.is_some() {
        println!("管理凭据 {}（secret 已保存）", t.admin_key.clone().unwrap_or_default());
    }
    println!("能力     {}", m.capability_line());
    if let Some(n) = &m.note {
        println!("备注     {n}");
    }
    Ok(())
}

fn r#use(cfg: &mut CliConfig, name: &str, json_out: bool) -> Result<()> {
    config::set_current(cfg, name)?;
    config::save(cfg)?;
    let t = cfg.target().clone();
    let m = capability::probe(cfg);
    if !m.product.is_empty() {
        capability::remember(cfg, &m);
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "current": cfg.current_name(), "kind": t.kind_label(), "base": t.base_url,
                "loggedIn": t.logged_in(), "capabilities": m.capabilities,
            }))?
        );
        return Ok(());
    }
    println!(
        "✅ 已切到目标 {}（{} · {}）{}",
        cfg.current_name(),
        t.kind_label(),
        t.base_url,
        if t.logged_in() { "" } else { " · 尚未登录" }
    );
    if !m.capabilities.is_empty() {
        println!("   它声明了：{}", m.capabilities.join(" · "));
    }
    println!("\n看一眼：ncc target list · 这个目标上能做什么：ncc hub usage（云端）/ ncc registry status（节点）");
    Ok(())
}

fn add(cfg: &mut CliConfig, name: &str, base: Option<&str>, kind: Option<&str>, use_it: bool, json_out: bool) -> Result<()> {
    let base = match base {
        Some(b) => b.trim_end_matches('/').to_string(),
        None => match cfg.by_name(name) {
            Some(t) => t.base_url.clone(),
            None => cfg.base_url(),
        },
    };
    if !base.starts_with("http") {
        anyhow::bail!("--base 需要是 http(s) 地址，收到 {base}");
    }
    let existed = cfg.by_name(name).is_some();
    let existing = cfg.by_name(name).cloned().unwrap_or_default();
    // 先探一下可达性：建目标时就把「写错地址」当场暴露，而不是等下一次命令。
    let reachable = ping(&base).ok();
    cfg.targets.insert(
        name.to_string(),
        Target {
            kind: kind.map(|k| if k == "hub" { "cloud" } else { "registry" }.to_string()).unwrap_or(existing.kind.clone()),
            base_url: base.clone(),
            ..existing
        },
    );
    if use_it || !cfg.targets.contains_key(&cfg.current_name()) {
        cfg.current = Some(name.to_string());
    }
    config::save(cfg)?;
    // 探测一次（顺便确定 kind 与能力）
    let mut probe_cfg = cfg.clone();
    probe_cfg.current = Some(name.to_string());
    let m = capability::probe(&probe_cfg);
    if !m.product.is_empty() {
        capability::remember(&mut probe_cfg, &m);
        if let Some(t) = probe_cfg.targets.get(name) {
            cfg.targets.insert(name.to_string(), t.clone());
        }
        let _ = config::save(cfg);
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({
            "name": name, "base": base, "kind": cfg.by_name(name).map(|t| t.kind_label().to_string()),
            "product": m.product, "capabilities": m.capabilities, "current": cfg.current_name(),
        }))?);
        return Ok(());
    }
    println!(
        "✅ 已{}目标 {}（{} · {}）{}",
        if existed { "更新" } else { "新建" },
        name,
        cfg.by_name(name).map(|t| t.kind_label().to_string()).unwrap_or_default(),
        base,
        if cfg.current_name() == name { " · 已切换" } else { "" }
    );
    if !m.capabilities.is_empty() {
        println!("   它声明了：{}", m.capabilities.join(" · "));
    }
    match &reachable {
        Some(service) => println!("   可达     {service}"),
        None => println!("   ⚠️ 现在连不上（会先存下地址，网络恢复后即可用）"),
    }
    if let Some(n) = &m.note {
        println!("   ⚠️ {n}");
    }
    if !cfg.by_name(name).map(|t| t.logged_in()).unwrap_or(false) {
        println!("   下一步：ncc login --email … --password …（登录写入的是这个目标）");
    }
    Ok(())
}

fn rm(cfg: &mut CliConfig, name: &str, json_out: bool) -> Result<()> {
    if cfg.by_name(name).is_none() {
        anyhow::bail!("没有名为 {name} 的目标");
    }
    if cfg.names().len() == 1 {
        anyhow::bail!("至少要留一个目标（这是唯一的一个）");
    }
    cfg.targets.remove(name);
    if cfg.current_name() == name {
        cfg.current = cfg.names().first().cloned();
    }
    config::save(cfg)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({"removed": name, "current": cfg.current_name()}))?);
        return Ok(());
    }
    println!("✅ 已删除目标 {name}（连同它保存的凭据）；当前目标：{}", cfg.current_name());
    Ok(())
}

/// 给新目标起个短名字：cloud / local / host 短名（避免撞已有名字）。
pub fn suggest_name_for(cfg: &CliConfig, base: &str, kind: &str) -> String {
    let host = base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split([':', '/'])
        .next()
        .unwrap_or("node")
        .to_string();
    let stem = if kind == "hub" || base.contains("ncc.ai") {
        "cloud".to_string()
    } else if host == "localhost" || host == "127.0.0.1" {
        "local".to_string()
    } else {
        host.split('.').next().unwrap_or("node").to_string()
    };
    if cfg.by_name(&stem).is_none() {
        return stem;
    }
    for i in 2..100 {
        let candidate = format!("{stem}{i}");
        if cfg.by_name(&candidate).is_none() {
            return candidate;
        }
    }
    format!("{stem}-x")
}

/// `ncc hub …`：把一条普通命令指到云端目标执行（见 main.rs 的前缀重写）。
/// 这里只负责「云端目标是谁」。
///
/// 注意别只看名字：从旧配置迁移过来的人可能把 `hub` 这个名字指向了内网节点
/// （旧配置只有一个地址），所以**优先挑 kind 不是 node 的目标**。
pub fn hub_target_name(cfg: &CliConfig) -> Option<String> {
    if let Some(t) = cfg.by_name(config::DEFAULT_TARGET) {
        if !t.is_node() {
            return Some(config::DEFAULT_TARGET.to_string());
        }
    }
    cfg.targets
        .iter()
        .find(|(_, t)| !t.is_node())
        .map(|(n, _)| n.clone())
}

/// 用一条网内的健康探测确认目标可达（`target add` 之后给个明确的成败）。
pub fn ping(base: &str) -> Result<String> {
    let d = api::request(
        &CliConfig {
            current: Some("probe".into()),
            targets: [(
                "probe".to_string(),
                Target { base_url: base.trim_end_matches('/').to_string(), ..Target::default() },
            )]
            .into_iter()
            .collect(),
            ..CliConfig::default()
        },
        "GET",
        "/api/health",
        None,
        None,
        None,
        &[],
    )?;
    Ok(d.get("service").and_then(|s| s.as_str()).unwrap_or("").to_string())
}
