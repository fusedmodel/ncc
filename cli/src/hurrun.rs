//! `ncc hur run --exec` —— 本机真执行（L1 引擎，wasmtime 沙箱）。
//!
//! 2026-09-25 用户拍板：**`ncc` 是 harness-use 事实上的执行引擎**，所以执行侧从
//! harnessuse 搬进本仓（那边是桌面 Agent 的 `hurrun.rs`）。以前是桌面端反过来 shell 出
//! `ncc hur run --json` 拿计划、再自己跑沙箱；现在同一份计划在这里直接落地执行，
//! 「计划在 ncc、执行在别处」不再需要两套实现同步。
//!
//! 分工仍然写死在两行里：
//! - **策略与限额**由 `hur_core::policy` 算（引擎不解释策略，否则"允不允许跑、按多少限额跑"会有两套答案）；
//! - **真跑**归 `hur-sandbox`，它只吃「入口字节 + 限额 + 宿主桥」三样东西。
//!
//! 跑完按 `harness-use-run-trace/v1` 写留痕（`policy::write_trace`），
//! `ncc hur task ls|inspect` 原样读得回来 —— 这是跨产品验收的硬指标。

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hur_core::policy::{self, Effective, RunPlan};
use hur_core::spec::HurPackage;
use hur_sandbox::{HostBridge, Limits};

/// 本机提供给 guest 的工具（与桌面端 / 旧 CLI 同名同义；`provides` 与实现必须同步）
const HOST_TOOLS: [&str; 3] = ["kb.search", "task.create", "repo_search"];

/// 单次外呼最多读回多少字节（防一次 GET 把宿主内存拉满）
const MAX_HTTP_BYTES: u64 = 256 * 1024;
/// 外呼超时：比沙箱墙钟短，免得 guest 早就超时了宿主还挂着
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>()
    }
}

/* ---------------- 宿主能力桥 ---------------- */

/// 宿主给 guest 的能力。**默认全关**：只有这里实现了的才 `provides`。
struct Host {
    /// 包根：`repo_search` 只在这里面找 —— 检索也要做到"数据不出设备"
    dir: PathBuf,
    /// 策略是否允许出网。`local_only` 为真时**一个包外字节都不发**。
    allow_net: bool,
}

impl Host {
    fn repo_search(&self, arg: &str) -> Result<String> {
        let q = arg.trim().trim_matches('"').to_lowercase();
        if q.is_empty() {
            bail!("repo_search 需要一个关键词");
        }
        // 本机检索：只在包目录里找（技能正文、入口源码）。不发外呼。
        let mut hits: Vec<String> = Vec::new();
        let mut stack = vec![self.dir.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if name == "dist" || name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&p) {
                    if text.to_lowercase().contains(&q) {
                        let rel = p.strip_prefix(&self.dir).unwrap_or(&p).to_string_lossy().to_string();
                        hits.push(rel);
                    }
                }
                if hits.len() >= 10 {
                    break;
                }
            }
            if hits.len() >= 10 {
                break;
            }
        }
        if hits.is_empty() {
            Ok(format!("本机包内没有匹配「{q}」的文件"))
        } else {
            Ok(format!("本机包内命中 {} 个文件：{}", hits.len(), hits.join("、")))
        }
    }

    /// 投递任务：写进 `~/.harnessuse/tasks/*.json`，字段与旧 CLI 完全一致 ——
    /// 这样 `ncc hur task ls` 与桌面端的任务收件箱都认得出来。
    fn task_create(&self, arg: &str) -> Result<String> {
        let v: Value = serde_json::from_str(arg).unwrap_or_else(|_| json!({ "title": arg }));
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if title.is_empty() {
            bail!("task.create 需要 title");
        }
        let kind = match v.get("kind").and_then(|x| x.as_str()).unwrap_or("chat") {
            "kb" => "kb",
            "file" => "file",
            _ => "chat",
        };
        let dir = hur_core::cfg::home().join("tasks");
        std::fs::create_dir_all(&dir)?;
        let slug: String = title
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .take(32)
            .collect();
        let path = dir.join(format!("{}-{}.json", now_unix(), if slug.is_empty() { "task" } else { &slug }));
        let row = json!({
            "title": title,
            "kind": kind,
            "brief": v.get("brief").and_then(|x| x.as_str()).unwrap_or("").trim(),
            "ref": v.get("ref").and_then(|x| x.as_str()).unwrap_or("").trim(),
            "created_by": "ncc 本机沙箱",
        });
        std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&row)?))?;
        Ok(json!({ "file": path.display().to_string(), "title": title, "kind": kind }).to_string())
    }
}

impl HostBridge for Host {
    fn provides(&self, name: &str) -> bool {
        HOST_TOOLS.contains(&name)
    }

    fn tool(&mut self, name: &str, arg: &str) -> Result<String> {
        match name {
            "repo_search" => self.repo_search(arg),
            "task.create" => self.task_create(arg),
            // 知识库在 GUI 里（浏览器 localStorage），命令行执行时拿不到 —— 如实说，
            // 不要编一个"没找到"让人以为搜过了。
            "kb.search" => Ok("（应用侧知识库不参与本机沙箱执行；需要检索请在对话里用 /kb）".to_string()),
            other => bail!("工具「{other}」本机未提供（provides 与实现不同步才会走到这里）"),
        }
    }

    fn http_get(&mut self, url: &str) -> Result<String> {
        // 域名白名单与次数上限已在沙箱里判过（越权就轮不到这里）。这里管的是**要不要真发**：
        // `local_only` 是策略里"数据不出设备"的开关，默认档（wasm-local）就是开的。
        if !self.allow_net {
            bail!("策略 local_only：本机执行不代发网络请求（数据不出设备）。要出网请把生效策略的 exec.local_only 设为 false");
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(HTTP_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .build();
        let resp = agent.get(url).call().map_err(|e| anyhow::anyhow!("HTTP 请求失败：{e}"))?;
        let mut body = String::new();
        resp.into_reader()
            .take(MAX_HTTP_BYTES)
            .read_to_string(&mut body)
            .map_err(|e| anyhow::anyhow!("读响应失败：{e}"))?;
        Ok(body)
    }
}

/* ---------------- 限额与执行 ---------------- */

/// 生效策略 + 包声明 → 引擎限额。
/// 外呼白名单来自**包清单**（`permissions.network`）：策略说不许出网，清单说能去哪，
/// 两件事都要满足才发得出去。
pub fn limits_of(e: &Effective, network_allow: Vec<String>) -> Limits {
    Limits {
        fuel: e.fuel,
        memory_mb: e.memory_mb,
        stack_kb: e.stack_kb,
        wall_ms: e.wall_ms,
        max_output_chars: e.max_output_chars,
        max_network_calls: e.max_network_calls,
        network_allow,
    }
}

/// 留痕落点：策略里的 `audit.trace_dir`（相对包根解析）
fn trace_dir_of(abs: &Path, e: &Effective) -> PathBuf {
    let p = PathBuf::from(e.trace_dir.trim());
    if p.is_absolute() {
        p
    } else {
        abs.join(p)
    }
}

/// 真跑一个包。`plan` 是已经算好的执行计划（策略解释权在 `hur_core::policy`）。
pub fn exec(abs: &Path, pkg: &HurPackage, plan: &RunPlan, e: &Effective, entry: &str) -> Result<Value> {
    let engine = plan.engine.clone().unwrap_or_default();
    if engine.is_empty() {
        bail!("计划里没有选定引擎 —— `ncc hur run <目录>`（不带 --exec）能看到原因");
    }
    if !hur_sandbox::ENGINES.contains(&engine.as_str()) {
        bail!(
            "本机没有「{engine}」引擎的运行时（本机只有 {:?}）。\n  \
             本机引擎就是内置的 wasm 沙箱；container / process 按设计不在本机（要容器请另装运行时），\
             远程执行请用 `ncc hur env` 登记环境后选 remote 引擎。",
            hur_sandbox::ENGINES
        );
    }
    let entry = entry.trim();
    if entry.is_empty() {
        bail!("包没有可执行入口（`security.entry` 为空）—— 只读包不能 --exec");
    }
    let file = abs.join(entry);
    if !file.is_file() {
        bail!("入口文件不存在：{}", file.display());
    }
    let bytes = std::fs::read(&file).with_context(|| format!("读入口 {} 失败", file.display()))?;

    let limits = limits_of(e, pkg.permissions.network.clone());
    let host = Host { dir: abs.to_path_buf(), allow_net: !e.local_only };
    // 结构性缺陷（不是合法 wasm / ABI 不满足）才是 Err；被沙箱拦下的失败是 Ok(outcome)，
    // 因为那是"跑过了、结论是可读的"，要如实留痕。
    let outcome = hur_sandbox::run(&bytes, &limits, hur_sandbox::shared(host)).map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let outcome_v = serde_json::to_value(&outcome)?;

    // 留痕：跑过必留痕、留痕可回溯（policy 层只管"存下来、按 retain 裁剪"）
    let trace_dir = trace_dir_of(abs, e);
    let rec = policy::TraceRecord::new(pkg, &plan.policy, &engine, plan.limits.clone(), outcome_v.clone());
    let trace = policy::write_trace(&trace_dir, e.retain, &rec)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(json!({
        "package": pkg.id,
        "name": pkg.name,
        "version": pkg.version,
        "engine": engine,
        "entry": entry,
        "policy": plan.policy,
        "limits": plan.limits,
        "replyPreview": clip(outcome.reply.as_str(), 200),
        "outcome": outcome_v,
        "trace": trace,
        "checks": plan.checks,
        "planReasons": plan.reasons,
        // 整份计划也带上：`--exec --json` 只输出这一份 JSON，
        // 委派方（桌面端/脚本）需要「为什么允许跑、按什么限额」的完整依据。
        "plan": plan,
    }))
}

/// 人读的执行结论（`ncc hur run --exec` 不带 --json 时打印这个）
pub fn render(out: &Value) -> String {
    let o = &out["outcome"];
    let mut s = String::new();
    let ok = o["ok"].as_bool().unwrap_or(false);
    s.push_str(&format!(
        "{} {} v{} · 引擎 {} · 入口 {}\n",
        if ok { "✅ 已执行" } else { "❌ 执行失败" },
        out["package"].as_str().unwrap_or(""),
        out["version"].as_str().unwrap_or(""),
        out["engine"].as_str().unwrap_or(""),
        out["entry"].as_str().unwrap_or("")
    ));
    if !ok {
        let kind = o["error_kind"].as_str().unwrap_or("trap");
        s.push_str(&format!("   原因      [{kind}] {}\n", o["error"].as_str().unwrap_or("")));
    }
    let reply = o["reply"].as_str().unwrap_or("");
    if !reply.is_empty() {
        s.push_str(&format!(
            "   回复      {}{}\n",
            clip(reply, 400),
            if o["truncated"].as_bool().unwrap_or(false) { " …（已按 max_output_chars 截断）" } else { "" }
        ));
    }
    let logs = o["logs"].as_array().map(|a| a.len()).unwrap_or(0);
    s.push_str(&format!(
        "   用量      fuel {} / 内存 {} 字节 / 墙钟 {} ms / 日志 {} 条 / 宿主调用 {} 次\n",
        o["fuel_used"], o["memory_bytes"], o["wall_ms"], logs,
        o["host_calls"].as_array().map(|a| a.len()).unwrap_or(0)
    ));
    if let Some(calls) = o["host_calls"].as_array() {
        for c in calls.iter().take(5) {
            s.push_str(&format!(
                "     {} {} {}（{}）\n",
                if c["ok"].as_bool().unwrap_or(false) { "·" } else { "✗" },
                c["kind"].as_str().unwrap_or(""),
                clip(c["target"].as_str().unwrap_or(""), 60),
                clip(c["detail"].as_str().unwrap_or(""), 80)
            ));
        }
    }
    let t = out["trace"].as_str().unwrap_or("");
    s.push_str(&format!("   留痕      {}\n", if t.is_empty() { "（未写入）" } else { t }));
    s.push_str(&format!("   回看      ncc hur task ls{}", if t.is_empty() { "" } else { " · ncc hur task inspect latest" }));
    s
}
