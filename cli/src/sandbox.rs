//! `ncc sandbox` —— 远程云电脑（Remote Cloud Computer）。
//!
//! 一台云电脑 = **一台能替别人跑东西的 ncc 节点**（内网 `ncc-registry` 或云端，跑着
//! `/api/exec/*`）。本命令做四件事：
//!
//!   init    用 `ip/port/key` 把这台机器**登记成一个沙箱环境**（并当场自检能不能跑）
//!   ls      我登记了哪些云电脑（在线/引擎/默认）
//!   status  现探一台：能跑哪些引擎、限额多少、runner 就绪没有
//!   run     把任务丢过去跑（HUR 包走沙箱；OS 敏感任务如 docker build 走 process/container）
//!
//! 三条边界（与 `prd/ncc-sandbox.md` 一致）：
//!  1. **登记进既有的沙箱环境表**（`~/.harnessuse/environments.json`），不另造一套配置 ——
//!     于是 `ncc hur env ls/use`、策略里的 `require_attestation` / `allow_data_leaving`
//!     对云电脑**自动生效**。云电脑只是「带凭据、能连上的 proxy 环境」。
//!  2. **能不能跑看节点自报的事实**（`/api/exec/kinds`），不看我们登记的引擎字符串 ——
//!     登记只说明「当初它开着什么」，`--require` 路由时**实时再问一次**。
//!  3. **任务不是制品**：跑了就跑了，日志与退出码是它的产物；制品分发仍走 `ncc publish`。
//!
//! 与本地留痕的关系：本地 `ncc hur run --exec` 的留痕在包目录里（`ncc hur task ls` 读它）；
//! 云电脑上的任务住在节点侧（`ncc sandbox run` 会把日志拉回来给你看）。
use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use hur_core::{pack, policy, spec};

use crate::api;
use crate::config::{self, CliConfig};

#[derive(clap::Args)]
pub struct SandboxCmd {
    #[command(subcommand)]
    pub action: SandboxAction,
}

#[derive(Subcommand)]
pub enum SandboxAction {
    /// 登记一台云电脑：--host/--port/--key（或直接 --url），并当场自检
    Init(InitArgs),
    /// 我登记了哪些云电脑
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// 现探一台（默认探全部）：能跑哪些引擎、限额、runner 就绪没有
    Status {
        /// 环境名（缺省 = 全部）
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 把任务丢到云电脑上跑（包走沙箱；命令走 process/container）
    Run(RunArgs),
    /// 移除一台云电脑的登记（不影响那台机器本身）
    Rm { name: String },
}

#[derive(clap::Args)]
pub struct InitArgs {
    /// 云电脑地址（二选一：直接给 --url，或用 --host [+ --port]）
    #[arg(long)]
    pub url: Option<String>,
    /// 主机（IP 或域名）
    #[arg(long)]
    pub host: Option<String>,
    /// 端口（缺省 8282，与 ncc-registry 默认一致）
    #[arg(long, default_value_t = 8282)]
    pub port: u16,
    /// 协议：http | https
    #[arg(long, default_value = "http")]
    pub scheme: String,
    /// 访问 key（节点令牌 / API-Key；对方给你的那串）
    #[arg(long)]
    pub key: Option<String>,
    /// 环境名（缺省由地址推出来，如 office-10-0-0-5）
    #[arg(long)]
    pub name: Option<String>,
    /// 备注（这台机器是干什么的、谁能用）
    #[arg(long)]
    pub note: Option<String>,
    /// 设成默认环境（`ncc sandbox run` 不给 --on/--any 时用它）
    #[arg(long)]
    pub default: bool,
    /// 对方还没开执行服务时也强行登记（默认拒绝：登记一台跑不了的环境没意义）
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// 用哪台（缺省 = 默认环境）
    #[arg(long)]
    pub on: Option<String>,
    /// 自动挑一台：在已登记的环境里按 --require 实时挑（不指定名字时的兜底）
    #[arg(long)]
    pub any: bool,
    /// 需要的引擎（可重复）：wasm | process | container
    #[arg(long = "require")]
    pub require: Vec<String>,
    /// 跑一个包（目录或 .hur）—— 走 wasm 沙箱
    #[arg(long)]
    pub package: Option<String>,
    /// 跑一条命令（OS 敏感任务，如 `docker build -t x . && docker push x`）
    #[arg(long)]
    pub cmd: Option<String>,
    /// container 引擎用的镜像
    #[arg(long)]
    pub image: Option<String>,
    /// 显式指定引擎（缺省：--package → wasm，--cmd → process）
    #[arg(long)]
    pub engine: Option<String>,
    /// 墙上限（秒；缺省用节点上限，超出会被节点拒绝）
    #[arg(long, default_value_t = 0)]
    pub timeout: i64,
    /// 提交理由（**必填**：节点侧账本要记「谁让这台机器干了什么、为什么」）
    #[arg(long)]
    pub reason: String,
    /// 只提交不等待（打印任务 id 与看日志的命令）
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    pub json: bool,
}

/* ---------------- 与云电脑说话的薄层 ---------------- */

/// 指向某个云电脑的 CliConfig（借 `api::request` 的请求/错误处理，不另造 HTTP 层）。
fn cfg_for(url: &str, token: Option<&str>) -> CliConfig {
    let mut t = config::Target::default();
    t.base_url = url.trim_end_matches('/').to_string();
    t.token = token.map(|s| s.to_string());
    let mut c = CliConfig::default();
    c.current = Some("sandbox".into());
    c.targets.insert("sandbox".into(), t);
    c
}

fn norm_url(raw: &str) -> String {
    let u = raw.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("http://{u}")
    }
}

fn id_of(url: &str, explicit: Option<&str>) -> String {
    if let Some(n) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return n.to_string();
    }
    // 从地址推一个 id：office.internal:8282 → office-internal-8282
    let host = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let s: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "sandbox".into()
    } else {
        s
    }
}

/// 问一台云电脑「你是谁、能跑什么」（不带凭据，公开）。
fn probe_meta(url: &str) -> (Value, Value) {
    let c = cfg_for(url, None);
    let meta = api::get(&c, "/api/meta", None);
    let kinds = api::get(&c, "/api/exec/kinds", None);
    (
        meta.unwrap_or(Value::Null),
        kinds.unwrap_or(Value::Null),
    )
}

/// 引擎可用性（从 kinds 里取 enabled 的引擎名）。
fn enabled_engines(kinds: &Value) -> Vec<String> {
    kinds["exec"]["kinds"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|k| k["enabled"].as_bool().unwrap_or(false))
                .filter_map(|k| k["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn engine_why(kinds: &Value, engine: &str) -> String {
    kinds["exec"]["kinds"]
        .as_array()
        .and_then(|a| a.iter().find(|k| k["id"].as_str() == Some(engine)))
        .and_then(|k| k["why"].as_str())
        .unwrap_or("")
        .to_string()
}

/// 用凭据试一次鉴权（拿得到自己的任务列表 = 凭据有效）。
fn auth_check(url: &str, token: Option<&str>) -> Result<usize> {
    let c = cfg_for(url, token);
    let d = api::get(&c, "/api/exec/runs?limit=1", token)
        .map_err(|e| anyhow!("凭据自检失败：{e}"))?;
    Ok(d["total"].as_i64().unwrap_or(0) as usize)
}

/// 字节摘要（“推文件先给指纹，目标端核对才落盘”两处都用它 —— 只该有一份实现）。
pub fn sha256_hex(b: &[u8]) -> String {
    spec::sha256_hex(b)
}

fn engine_of(a: &RunArgs) -> String {
    if let Some(e) = a.engine.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    if a.package.is_some() {
        return "wasm".into();
    }
    if a.cmd.is_some() {
        return "process".into();
    }
    if let Some(r) = a.require.first() {
        return r.trim().to_string();
    }
    "process".into()
}

/* ---------------- init ---------------- */

fn init(a: &InitArgs) -> Result<()> {
    let url = match (a.url.as_deref(), a.host.as_deref()) {
        (Some(u), _) if !u.trim().is_empty() => norm_url(u),
        (_, Some(h)) if !h.trim().is_empty() => norm_url(&format!("{}://{}:{}", a.scheme, h.trim(), a.port)),
        _ => bail!("给个地址：--url http://10.0.0.5:8282 或 --host 10.0.0.5 --port 8282"),
    };
    let name = id_of(&url, a.name.as_deref());

    // 1) 这是不是一台 ncc 节点？有没有执行服务？
    let (meta, kinds) = probe_meta(&url);
    if meta.is_null() {
        bail!("连不上 {url}（或它不是 ncc 节点）——先确认地址、端口与网络可达");
    }
    let product = meta["product"].as_str().unwrap_or("");
    let caps: Vec<String> = meta["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect())
        .unwrap_or_default();
    let has_exec = caps.iter().any(|c| c == "exec");
    if kinds.is_null() || !has_exec {
        if !a.force {
            bail!(
                "{} 这台（{product}）没开远程执行服务 —— 登记一台跑不了的环境没意义。\n  \
                 要么让对方运维打开执行服务（NCCR_EXEC_ALLOW=wasm[,…]），要么 --force 先登记着",
                url
            );
        }
        eprintln!("⚠️  {} 没声明 exec 能力，按 --force 先登记（现在它跑不了任务）", url);
    }

    // 2) 凭据自检（拿得到自己的任务列表才算有权限，不只是"能连上"）
    let token = a.key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut runs_seen = 0usize;
    if let Some(t) = token {
        runs_seen = auth_check(&url, Some(t))?;
    } else {
        eprintln!("ℹ  没给 --key：登记成**匿名**环境。提交任务需要凭据，之后 `ncc sandbox init` 再来一次即可。");
    }

    // 3) 登记进既有的沙箱环境表（不另造配置）
    let engines = enabled_engines(&kinds);
    let timeout_ms = kinds["limits"]["timeoutMs"].as_u64().unwrap_or(0) as u32;
    let env = policy::Environment {
        id: name.clone(),
        name: name.clone(),
        kind: "proxy".into(),
        url: url.clone(),
        engines: engines.clone(),
        limits: policy::EnvLimits { max_memory_mb: 0, max_wall_ms: timeout_ms, fuel: 0 },
        // 证明（attestation）空着：本节点的能力是自己报的，**自证不等于他证**。
        // 策略若要求 require_attestation，就明确不会被选中 —— 那是对的，别伪造。
        attestation: policy::Attestation::default(),
        allow_data_leaving: true, // 云电脑就是「把活送出去跑」，不设这一条等于登记了也不能用
        registered_at_unix: 0,
        note: a.note.clone().unwrap_or_default(),
        auth: match token {
            Some(t) => policy::Auth::bearer(t),
            None => policy::Auth::none(),
        },
    };
    policy::validate_env(&env).map_err(|e| anyhow!("{e:#}"))?;
    let saved = policy::add_env(env, a.default)?;

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "environment": saved,
                "node": kinds["node"], "engines": engines, "limits": kinds["limits"],
                "authed": token.is_some(), "myRuns": runs_seen,
            }))?
        );
        return Ok(());
    }
    println!("✅ 已登记云电脑「{}」（{}）", saved.id, url);
    println!("   节点      {} · {} · {}", meta["product"].as_str().unwrap_or("ncc"), meta["kind"].as_str().unwrap_or(""), saved.note);
    print_engines(&kinds, "   ");
    println!("   凭据      {}", if token.is_some() { "已登记（key 只写进本机环境表 0600，不回显）" } else { "无（匿名登记；提交任务需要 key）" });
    println!("   登记表    {}", policy::env_path().display());
    println!("\n下一步：ncc sandbox run --on {}{} --cmd \"echo hi\" --reason \"试试\"", saved.id, if a.default { "" } else { "（或先 ncc sandbox status）" });
    Ok(())
}

fn print_engines(kinds: &Value, indent: &str) {
    let list = kinds["exec"]["kinds"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("{indent}引擎      （这台没开执行服务）");
        return;
    }
    for k in &list {
        let on = k["enabled"].as_bool().unwrap_or(false);
        println!(
            "{indent}引擎      {} {:<10} {}",
            if on { "●" } else { "○" },
            k["id"].as_str().unwrap_or(""),
            if on { k["provider"].as_str().unwrap_or("") } else { k["why"].as_str().unwrap_or("不可用") }
        );
    }
    let runner = &kinds["exec"]["runner"];
    println!(
        "{indent}HUR 运行时 {} {}",
        if runner["ok"].as_bool().unwrap_or(false) { "就绪" } else { "未就绪" },
        runner["path"].as_str().or(runner["note"].as_str()).unwrap_or("")
    );
    println!(
        "{indent}限额      单任务 {} 秒 · 日志上限 {} 字节",
        kinds["limits"]["timeoutMs"].as_u64().unwrap_or(0) / 1000,
        kinds["limits"]["maxOutputBytes"].as_u64().unwrap_or(0)
    );
}

/* ---------------- ls / status / rm ---------------- */

fn ls(json_out: bool) -> Result<()> {
    let reg = policy::load_envs();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&reg)?);
        return Ok(());
    }
    if reg.environments.is_empty() {
        println!("还没登记任何云电脑。加一台：ncc sandbox init --host 10.0.0.5 --port 8282 --key <对方给你的 key>");
        return Ok(());
    }
    println!("已登记 {} 个沙箱环境（{}）", reg.environments.len(), policy::env_path().display());
    for e in &reg.environments {
        println!(
            "  {}{:<18} {:<7} {:<28} 引擎 {:<22} 凭据 {}",
            if reg.default == e.id { "*" } else { " " },
            e.id,
            e.kind,
            if e.url.is_empty() { "（本机）".into() } else { e.url.clone() },
            if e.engines.is_empty() { "（未声明）".into() } else { e.engines.join("+") },
            if e.auth.present() { "有" } else { "无" }
        );
    }
    println!("  （* = 默认；`ncc sandbox status` 现探一台能跑什么）");
    Ok(())
}

fn status(name: Option<String>, json_out: bool) -> Result<()> {
    let reg = policy::load_envs();
    let want: Vec<policy::Environment> = match name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => vec![reg.environments.iter().find(|e| e.id == n).cloned().ok_or_else(|| anyhow!("没有登记过环境「{n}」（ncc sandbox ls 看可选值）"))?],
        None => reg.environments.clone(),
    };
    if want.is_empty() {
        println!("还没登记任何云电脑（ncc sandbox init --host … --key …）");
        return Ok(());
    }
    let mut out = Vec::new();
    for e in &want {
        if e.url.trim().is_empty() {
            out.push(json!({"id": e.id, "kind": e.kind, "note": "本机环境（没有 url，不用探）"}));
            if !json_out {
                println!("{}（{}）本机环境", e.id, e.kind);
            }
            continue;
        }
        let (meta, kinds) = probe_meta(&e.url);
        let online = !meta.is_null();
        let auth_ok = if e.auth.present() { auth_check(&e.url, Some(&e.auth.token)).is_ok() } else { false };
        let enabled = enabled_engines(&kinds);
        if !json_out {
            println!("{}（{}）{}", e.id, e.url, if online { "在线" } else { "**连不上**" });
            if online {
                print_engines(&kinds, "   ");
                println!("   凭据      {}", if !e.auth.present() { "无".into() } else if auth_ok { "有效".to_string() } else { "**无效/过期**（重新 init）".to_string() });
                println!("   登记引擎  {}", if e.engines.is_empty() { "（无）".into() } else { e.engines.join("+") });
                if !e.engines.is_empty() && !enabled.is_empty() && e.engines != enabled {
                    println!("   ⚠️  登记的引擎与它现在报的不一样（以现探的为准）：{}", enabled.join("+"));
                }
            }
        }
        out.push(json!({
            "id": e.id, "url": e.url, "online": online, "authOk": auth_ok,
            "registeredEngines": e.engines, "enginesEnabled": enabled,
            "exec": kinds["exec"], "limits": kinds["limits"],
        }));
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "sandboxes": out }))?);
    }
    Ok(())
}

fn rm(name: &str) -> Result<()> {
    if policy::remove_env(name.trim())? {
        println!("✅ 已移除云电脑登记 {name}（那台机器本身不受影响）");
    } else {
        println!("没有登记过环境 {name}");
    }
    Ok(())
}

/* ---------------- run ---------------- */

/// 挑一台能跑的：`--on` 指名；否则 `--any`/默认环境里，按 `--require` **实时**挑。
fn pick(reg: &policy::EnvRegistry, a: &RunArgs, engine: &str) -> Result<policy::Environment> {
    if let Some(n) = a.on.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return reg
            .environments
            .iter()
            .find(|e| e.id == n)
            .cloned()
            .ok_or_else(|| anyhow!("没有登记过环境「{n}」（ncc sandbox ls 看可选值）"));
    }
    let want: Vec<String> = if a.require.is_empty() { vec![engine.to_string()] } else { a.require.clone() };
    let mut candidates: Vec<&policy::Environment> = reg.environments.iter().filter(|e| !e.url.trim().is_empty()).collect();
    // 默认环境排前面（我指定过的那台就是我的偏好）
    candidates.sort_by_key(|e| if reg.default == e.id { 0 } else { 1 });
    if candidates.is_empty() {
        bail!("还没有登记任何云电脑：ncc sandbox init --host <ip> --port <n> --key <key>");
    }
    let mut why: Vec<String> = Vec::new();
    for e in candidates {
        let (meta, kinds) = probe_meta(&e.url);
        if meta.is_null() {
            why.push(format!("{}（连不上）", e.id));
            continue;
        }
        let enabled = enabled_engines(&kinds);
        let missing: Vec<&String> = want.iter().filter(|w| !enabled.contains(w)).collect();
        if missing.is_empty() {
            return Ok(e.clone());
        }
        let need = missing.iter().map(|m| format!("{m}（{}）", engine_why(&kinds, m))).collect::<Vec<_>>().join("；");
        why.push(format!("{}（缺 {need}）", e.id));
    }
    bail!("没有一台云电脑能满足 --require {}：\n  {}", want.join("+"), why.join("\n  "))
}

fn run(a: &RunArgs) -> Result<()> {
    if a.package.is_none() && a.cmd.is_none() {
        bail!("给个载荷：--package <目录|.hur>（走沙箱）或 --cmd \"<命令>\"（走本机进程/容器）");
    }
    if a.package.is_some() && a.cmd.is_some() {
        bail!("--package 与 --cmd 只能选一个（包走 wasm 沙箱；命令走 process/container）");
    }
    let reg = policy::load_envs();
    let engine = engine_of(a);
    let env = pick(&reg, a, &engine)?;

    // 环境是不是允许数据出境（策略语义；云电脑默认 true，但登记表里可以被改成 false）
    if !env.allow_data_leaving {
        bail!("环境「{}」登记为**不允许数据出境** —— 把一个包/命令送过去跑属于出境；要放行请重新 init（或改登记表）", env.id);
    }

    let token = env.auth.token.clone();
    let token_opt = if token.trim().is_empty() { None } else { Some(token.as_str()) };
    let urlc = cfg_for(&env.url, token_opt);

    // 提交
    let created: Value;
    if let Some(p) = a.package.as_deref() {
        if engine != "wasm" {
            bail!("--package 只能配 wasm 引擎（现在推导出来的是 {engine}）");
        }
        let bytes = package_bytes(p)?;
        let mut qs = vec![
            "engine=wasm".to_string(),
            format!("reason={}", api::urlenc(&a.reason)),
        ];
        if a.timeout > 0 {
            qs.push(format!("timeoutSec={}", a.timeout));
        }
        created = api::request(
            &urlc,
            "POST",
            &format!("/api/exec/runs?{}", qs.join("&")),
            token_opt,
            None,
            Some(&bytes),
            &[("Content-Type", "application/octet-stream")],
        )
        .with_context(|| format!("提交到 {}（{}）失败", env.id, env.url))?;
    } else {
        let mut body = json!({
            "engine": engine,
            "cmd": a.cmd.clone().unwrap_or_default(),
            "reason": a.reason,
        });
        if let Some(img) = a.image.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            body["image"] = json!(img);
        }
        if a.timeout > 0 {
            body["timeoutSec"] = json!(a.timeout);
        }
        created = api::post_json(&urlc, "/api/exec/runs", token_opt, &body)
            .with_context(|| format!("提交到 {}（{}）失败", env.id, env.url))?;
    }
    let id = created["run"]["id"].as_str().unwrap_or("").to_string();
    if id.is_empty() {
        bail!("提交后没拿到任务 id：{created}");
    }

    if a.detach {
        if a.json {
            println!("{}", serde_json::to_string_pretty(&created)?);
        } else {
            println!("✅ 已提交 {}（{} · {}）", id, env.id, engine);
            println!("   看进度  ncc sandbox run --on {} …（或直接 GET {}/api/exec/runs/{}）", env.id, env.url, id);
            println!("   日志    {}/api/exec/runs/{}/log", env.url, id);
        }
        return Ok(());
    }

    // 等待：轮询状态，带日志尾巴 diff（人看得见"在动"）
    let mut shown = 0usize;
    let mut last = Value::Null;
    for _ in 0..600 {
        let d = api::get(&urlc, &format!("/api/exec/runs/{id}"), token_opt)?;
        let runv = d["run"].clone();
        if !a.json {
            if let Some(tail) = runv["logTail"].as_str() {
                if tail.len() > shown {
                    let fresh = &tail[shown..];
                    for line in fresh.lines().filter(|l| !l.trim().is_empty()) {
                        println!("  │ {line}");
                    }
                    shown = tail.len();
                }
            }
        }
        let st = runv["status"].as_str().unwrap_or("");
        last = runv.clone();
        if matches!(st, "succeeded" | "failed" | "timeout" | "canceled") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let st = last["status"].as_str().unwrap_or("unknown").to_string();
    let code = last["exitCode"].as_i64().unwrap_or(-1);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "sandbox": env.id, "run": last }))?);
    } else {
        println!();
        println!(
            "{} {} · 引擎 {} · 退出码 {} · {}ms · 日志 {} 字节{}",
            match st.as_str() {
                "succeeded" => "✅ 成功",
                "failed" => "✗ 失败",
                "timeout" => "✗ 超时",
                "canceled" => "✗ 已取消",
                _ => "… 未结束",
            },
            id,
            last["engine"].as_str().unwrap_or(""),
            code,
            last["durationMs"].as_i64().unwrap_or(0),
            last["logBytes"].as_i64().unwrap_or(0),
            if last["logTruncated"].as_bool().unwrap_or(false) { "（已截断）" } else { "" }
        );
        println!("   日志    {}/api/exec/runs/{}/log", env.url, id);
        println!("   工作目录 {}/{}（在云电脑上；日志与中间产物都在里面）", last["workDir"].as_str().unwrap_or(""), "");
        if let Some(err) = last["error"].as_str() {
            println!("   原因    {err}");
        }
    }
    // 退出码跟随远端：CI 里 `ncc sandbox run … && echo ok` 才说得通。
    if st != "succeeded" {
        std::process::exit(if code > 0 { code as i32 } else { 1 });
    }
    Ok(())
}

/// 载荷字节：给目录就现场产包（与 `ncc hur pack` 同一份实现），给 .hur 就直接读。
fn package_bytes(p: &str) -> Result<Vec<u8>> {
    let path = Path::new(p);
    if path.is_file() {
        return std::fs::read(path).with_context(|| format!("读 {} 失败", path.display()));
    }
    let dir = pack::find_root(path)?;
    let pkg = spec::read_pkg(&dir)?;
    let issues = spec::validate(&pkg, &dir, None, false);
    let errs = issues.iter().filter(|i| matches!(i.level, spec::Level::Error)).count();
    if errs > 0 {
        bail!("包没通过校验（{errs} 个错误）——`ncc hur verify {}` 看全部", dir.display());
    }
    let out = pack::pack(&dir).map_err(|e| anyhow!("{e:#}"))?;
    std::fs::read(&out.file).with_context(|| format!("读 {} 失败", out.file.display()))
}

pub fn cmd(action: &SandboxAction) -> Result<()> {
    match action {
        SandboxAction::Init(a) => init(a),
        SandboxAction::Ls { json } => ls(*json),
        SandboxAction::Status { name, json } => status(name.clone(), *json),
        SandboxAction::Rm { name } => rm(name),
        SandboxAction::Run(a) => run(a),
    }
}
