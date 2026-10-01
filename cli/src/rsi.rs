//! `ncc rsi` —— **运行时安全与自改进**（Runtime Safety & Improvement）。
//!
//! 名字里那个 "self-improvement" 说的是 AI 圈那句 *recursive self-improvement*，
//! 但我们只做**无聊的那一半**：**在动作发生之前把它拦住、记下来、事后算总账。**
//! 不做自我改写、不自动改策略、不替人做决定 —— 三件事都刻意留给人。
//!
//! 它解决的具体问题（用户的原话）：
//!
//! > 「在 agent 的安全决策、开发决策等，在**无人值守**时候可以通过 `ncc rsi` 的系列功能
//! > 实现提升与增强，防止安全事故，以及用户偏好跟踪，以及目标的正确性执行以及不跑偏。」
//!
//! 于是四块：
//!
//! | 块 | 命令 | 回答什么 |
//! |---|---|---|
//! | **Policy 接入** | `rsi check` / `rsi policy` | 「这一步**能不能做**」（安全/开发决策的门） |
//! | **账本** | `rsi guard` / `rsi report` | 「没人看着的时候**到底做了什么**」 |
//! | **偏好** | `rsi pref` | 「这个人以前说过**不要**什么」 |
//! | **目标** | `rsi goal` | 「这一步**还在不在**原来的目标上」（不跑偏） |
//!
//! ## 怎么接进 agent（这是重点，不是附属品）
//!
//! `rsi check` 收一份**决策请求 JSON**（stdin，也支持命令行简写），吐一份**裁决 JSON**，
//! 并用**退出码**表达结果，任何宿主都能接：
//!
//! ```text
//!   0  = allow（放行）
//!   10 = warn（可以做，但要留痕；无人值守时通常会被策略升级成 block）
//!   20 = block（**别做**）
//!   1  = 用法/配置错误（与"拦住了"分开，免得宿主把配置错误当成合规）
//! ```
//!
//! `ncc rsi hook install --host claude|cursor|generic` 会装一个 30 行的 sh shim，
//! 把它挂到宿主的 PreToolUse 钩子上即可（脚本自己会用 `NCC_RSI_UNATTENDED` 判断是不是
//! 无人值守）。
//!
//! ## 落地在哪（项目级 / 用户级两层）
//!
//! 从当前目录往上找最近的 `.ncc-rsi/`；找到就用它（**项目自带策略**），
//! 找不到就用用户级 `~/.harnessuse/rsi/`（可用 `NCC_RSI_DIR` 覆盖）。
//! 目录里三样东西：`policy.json`（策略）、`prefs.json`（偏好）、`ledger.jsonl`（账本）、
//! `goal.json`（当前目标）。写入一律 tmp → fsync → rename，0600。
//!
//! ## 红线（改代码前先读）
//!
//!  1. **门在动作之前**：`check` 只回答"能不能做"，它**不执行**任何东西；
//!     要连执行一起管，用 `rsi guard -- <命令>`（先 check，block 就真的不跑）。
//!  2. **不确定就说不知道**：策略里没写、目标没设、偏好冲突 —— 都要在 `reasons` 里写清楚。
//!     绝不用"没匹配到规则"冒充"安全"。
//!  3. **无人值守只能更严**：`--unattended`（或 `NCC_RSI_UNATTENDED=1`）只做一件事 ——
//!     把 warn 升级成 block。**反向不成立**（没有"无人值守就放宽"这种开关）。
//!  4. **账本只记事实**：动作、裁决、理由、目标、偏好命中。**不记密钥、不记文件内容**。
//!  5. **不替人下结论**：`report` 只汇总，`pref infer` 不存在 —— 偏好由人写进来说，
//!     或由人从账本里挑出来（`rsi pref add --from-ledger`）。
//!  6. **失败要能看出来**：读不懂的策略/请求一律非 0 退出，避免宿主把"配置坏了"当"放行"。
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/* ---------------- 常量 ---------------- */

/// 退出码（宿主靠它分支；与 `prd/ncc-rsi.md` 一致）
pub const EXIT_ALLOW: i32 = 0;
pub const EXIT_WARN: i32 = 10;
pub const EXIT_BLOCK: i32 = 20;

const POLICY: &str = "policy.json";
const PREFS: &str = "prefs.json";
const LEDGER: &str = "ledger.jsonl";
const GOAL: &str = "goal.json";
const DIR_NAME: &str = ".ncc-rsi";

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn fmt_epoch(secs: i64) -> String {
    crate::conn::fmt_epoch(secs)
}

/* ---------------- 目录发现（项目级优先） ---------------- */

/// 当前生效的 rsi 目录：从 cwd 往上找最近的 `.ncc-rsi/`，没有就落到用户级。
fn rsi_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(d) = explicit {
        return d.to_path_buf();
    }
    if let Ok(d) = std::env::var("NCC_RSI_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for dir in cwd.ancestors() {
        let c = dir.join(DIR_NAME);
        if c.is_dir() {
            return c;
        }
    }
    hur_home().join("rsi")
}

fn hur_home() -> PathBuf {
    std::env::var("HUR_HOME")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".harnessuse")
        })
}

fn is_project_dir(dir: &Path) -> bool {
    dir.file_name().and_then(|s| s.to_str()) == Some(DIR_NAME)
}

#[cfg(unix)]
fn ensure_dir(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(p)?;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
    Ok(())
}
#[cfg(not(unix))]
fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow!("路径没有父目录：{}", path.display()))?;
    ensure_dir(dir)?;
    let tmp = dir.join(format!(".{}.tmp.{}", path.file_name().and_then(|s| s.to_str()).unwrap_or("f"), std::process::id()));
    fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    fs::rename(&tmp, path).with_context(|| format!("落位到 {} 失败", path.display()))?;
    Ok(())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/* ---------------- 策略 ---------------- */

/// 内置预设。**默认那套是给无人值守用的**：陌生动作一律不放行。
pub fn presets() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "unattended-safe",
            "无人值守默认：常规动作放行；踩到危险命令/敏感路径/风险词或要人确认的事就拦（无人值守时 warn→block）",
            json!({
                "version": 1,
                "name": "unattended-safe",
                // 默认放行：门的作用是**在有事的时候拦人**，不是把人锁在门外。
                // 真正敏感的环境用 strict-prod（默认 block + 白名单）。
                "default": "allow",
                "unattended": { "upgradeWarnToBlock": true },
                "rules": {
                    "denyCommands": [
                        "rm -rf /", "rm -rf ~", "rm -rf /*", ":(){", "mkfs", "dd if=",
                        "> /dev/sd", "chmod -R 777 /", "shutdown", "reboot",
                        "git push --force", "git push -f", "git reset --hard",
                        "curl*|sh", "curl*| bash", "wget*|sh",
                        "DROP TABLE", "DROP DATABASE", "TRUNCATE "
                    ],
                    "denyPaths": ["/etc/**", "/usr/**", "/System/**", "~/.ssh/**", "**/.env", "**/id_rsa"],
                    "confirmCommands": ["git push", "npm publish", "docker push", "kubectl delete", "terraform apply", "rm -rf"],
                    "riskWords": ["production", "prod", "线上", "删除", "drop", "force"]
                },
                // requireGoal：无人值守时**必须有目标** —— 否则“在干什么”没人说得清。
                // onOffGoal=allow：和目标看不出关系的普通动作仍然放行（不然每一步都得写目的，太吵）；
                // 真正的跑偏信号（reject 命中）与风险信号照旧拦。
                "goal": { "requireGoal": true, "onDrift": "block", "onOffGoal": "allow" },
                "prefs": { "apply": true, "onConflict": "warn" }
            }),
        ),
        (
            "dev-loose",
            "本机开发：只拦最危险的一小撮，其余放行（仍会记进账本）",
            json!({
                "version": 1,
                "name": "dev-loose",
                "default": "allow",
                "unattended": { "upgradeWarnToBlock": true },
                "rules": {
                    "denyCommands": ["rm -rf /", "mkfs", "dd if=", "shutdown", "reboot"],
                    "denyPaths": ["/etc/**", "~/.ssh/**"],
                    "confirmCommands": ["git push --force", "npm publish"],
                    "riskWords": ["production"]
                },
                "goal": { "requireGoal": false, "onDrift": "warn", "onOffGoal": "allow" },
                "prefs": { "apply": true, "onConflict": "warn" }
            }),
        ),
        (
            "strict-prod",
            "生产环境：默认不放行，只放行明确 allowlist 里的读操作",
            json!({
                "version": 1,
                "name": "strict-prod",
                "default": "block",
                "unattended": { "upgradeWarnToBlock": true },
                "rules": {
                    "allowCommands": ["kubectl get", "kubectl describe", "docker ps", "docker logs", "ls", "cat", "git status", "git log"],
                    "denyCommands": ["rm -rf", "DROP ", "TRUNCATE ", "kubectl delete", "kubectl apply"],
                    "denyPaths": ["/etc/**", "~/.ssh/**", "**/.env"],
                    "confirmCommands": ["kubectl rollout", "docker restart"],
                    "riskWords": ["production", "prod"]
                },
                "goal": { "requireGoal": true, "onDrift": "block", "onOffGoal": "block" },
                "prefs": { "apply": true, "onConflict": "block" }
            }),
        ),
    ]
}

fn load_policy(dir: &Path) -> Result<Value> {
    let p = dir.join(POLICY);
    let Some(v) = read_json(&p) else {
        if !p.exists() {
            // 没写策略：明确说清"没有策略"，而不是假装一切安全（红线 2）
            return Ok(json!({
                "version": 1, "name": "none", "default": "warn", "_missing": true,
                "unattended": {"upgradeWarnToBlock": true},
                "rules": {}, "goal": {"requireGoal": false, "onDrift": "warn"}, "prefs": {"apply": true, "onConflict": "warn"}
            }));
        }
        bail!("{} 不是合法 JSON —— 先修它（`ncc rsi policy check`）", p.display());
    };
    let v: Value = v;
    let ver = v["version"].as_u64().unwrap_or(0);
    if ver == 0 || ver > 1 {
        bail!(
            "{} 的 version={ver} 不认识（本机认 1）—— 策略格式变了就不该猜着读",
            p.display()
        );
    }
    Ok(v)
}

fn rules_of(p: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for (k, label) in [
        ("allowCommands", "allowCommands（白名单）"),
        ("denyCommands", "denyCommands"),
        ("confirmCommands", "confirmCommands（要人确认）"),
        ("denyPaths", "denyPaths"),
        ("riskWords", "riskWords"),
    ] {
        let items: Vec<String> = p["rules"][k].as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().unwrap_or("").to_string()).collect();
        out.push(format!("{label}: {}", if items.is_empty() { "（空）".into() } else { items.join(" · ") }));
    }
    out
}

/* ---------------- 匹配 ---------------- */

/// 极简通配：`*` 任意多字符，`?` 一个字符。不引 glob 库（这里只需要这一种）。
fn wildcard_match(pat: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    // 命令类规则常写成 `curl*|sh`（中间可能有空格），用「子串 + 通配」两段式匹配更贴近人写规则的方式
    let (pl, tl) = (pat.trim().to_ascii_lowercase(), text.trim().to_ascii_lowercase());
    if pl.is_empty() {
        return false;
    }
    if !pl.contains('*') && !pl.contains('?') {
        // 无通配：命令类看子串（`git push --force` 收得住 `git push --force origin main`），
        // 路径类看整串或子串（两种写法都常见）
        return tl.contains(&pl);
    }
    if go(pl.as_bytes(), tl.as_bytes()) {
        return true;
    }
    // 逐词窗口再试一次（规则里带空格时，整串匹配会失败但不该漏判）
    let words: Vec<&str> = tl.split_whitespace().collect();
    for w in 1..=words.len().min(6) {
        for start in 0..=(words.len().saturating_sub(w)) {
            let seg = words[start..start + w].join(" ");
            if go(pl.as_bytes(), seg.as_bytes()) {
                return true;
            }
        }
    }
    false
}

fn path_hits(patterns: &[String], paths: &[String], home: &str) -> Option<String> {
    for pat in patterns {
        let pat = pat.replace('~', home);
        for p in paths {
            let expanded = p.replace('~', home);
            if wildcard_match(&pat, &expanded) || wildcard_match(&pat, &p) {
                return Some(format!("路径「{p}」命中 denyPaths「{pat}」"));
            }
        }
    }
    None
}

/* ---------------- 决策请求 ---------------- */

#[derive(Debug, Clone)]
struct Request {
    action: String,
    command: String,
    paths: Vec<String>,
    text: String,
    goal_id: String,
    unattended: bool,
    host: String,
    /// 只评估、不写账本（宿主自己会记，或者在做干跑）
    dry: bool,
}

impl Request {
    /// 从 stdin 的 JSON 里读一份决策请求。字段都给宽松的名字 —— 宿主各式各样，
    /// 别逼它们改字段名。
    fn from_json(v: &Value) -> Request {
        let s = |keys: &[&str]| -> String {
            for k in keys {
                if let Some(x) = v.get(*k).and_then(Value::as_str) {
                    return x.to_string();
                }
            }
            String::new()
        };
        let paths: Vec<String> = ["paths", "path", "files", "targets"]
            .iter()
            .find_map(|k| v.get(*k))
            .map(|x| match x {
                Value::Array(a) => a.iter().filter_map(|i| i.as_str().map(str::to_string)).collect(),
                Value::String(s) => vec![s.clone()],
                _ => Vec::new(),
            })
            .unwrap_or_default();
        // 有些宿主把整条工具调用塞在 `tool_input` 里
        let (mcmd, mtext, mpaths) = if v.get("tool_input").is_some() {
            let ti = &v["tool_input"];
            (
                ti.get("command").and_then(Value::as_str).unwrap_or("").to_string(),
                ti.get("prompt").and_then(Value::as_str).or_else(|| ti.get("description").and_then(Value::as_str)).unwrap_or("").to_string(),
                ti.get("file_path").and_then(Value::as_str).map(|s| vec![s.to_string()]).unwrap_or_default(),
            )
        } else {
            (String::new(), String::new(), Vec::new())
        };
        let mut all_paths = paths;
        all_paths.extend(mpaths);
        Request {
            action: {
                let a = s(&["action", "tool", "tool_name", "kind"]);
                if a.is_empty() { "shell".into() } else { a }
            },
            command: {
                let c = s(&["command", "cmd", "line"]);
                if c.is_empty() { mcmd } else { c }
            },
            paths: all_paths,
            text: {
                let t = s(&["text", "intent", "note", "summary"]);
                if t.is_empty() { mtext } else { t }
            },
            goal_id: s(&["goalId", "goal_id", "goal"]),
            unattended: v.get("unattended").and_then(Value::as_bool).unwrap_or(false),
            host: s(&["host", "agent", "source"]),
            dry: v.get("dry").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

/* ---------------- 偏好 ---------------- */

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Pref {
    id: String,
    statement: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    kind: String, // avoid（不要）/ prefer（最好）
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    confidence: String, // user-stated | inferred | low
    #[serde(default)]
    hits: u64,
    #[serde(default)]
    created_unix: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct PrefFile {
    #[serde(default)]
    prefs: Vec<Pref>,
}

fn load_prefs(dir: &Path) -> PrefFile {
    fs::read(dir.join(PREFS))
        .ok()
        .and_then(|b| serde_json::from_slice::<PrefFile>(&b).ok())
        .unwrap_or_default()
}

fn save_prefs(dir: &Path, f: &PrefFile) -> Result<()> {
    write_atomic(&dir.join(PREFS), format!("{}\n", serde_json::to_string_pretty(f)?).as_bytes(), 0o600)
}

/// 偏好命中：把「不要 X」拆成关键词，跟这一步的命令/说明/路径比。
fn pref_hits(prefs: &[Pref], req: &Request) -> Vec<(String, String)> {
    let hay = format!("{} {} {}", req.command, req.text, req.paths.join(" ")).to_ascii_lowercase();
    let mut out = Vec::new();
    for p in prefs {
        if p.kind == "prefer" {
            continue; // 只拿"不要"这类硬约束去拦；"最好"是建议，不该拦人
        }
        let keys: Vec<String> = p
            .statement
            .split(['，', ',', '。', '.', '；', ';', ' ', '、'])
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| s.chars().count() >= 2)
            .collect();
        // 至少要命中一个"实词"才算命中（避免"不要"这种词自己命中自己）
        let stop: Vec<&str> = vec!["不要", "别", "不能", "避免", "不许", "禁止", "don't", "do", "not"];
        let hit = keys
            .iter()
            .filter(|k| !stop.contains(&k.as_str()))
            .any(|k| hay.contains(k.as_str()));
        if hit {
            out.push((p.id.clone(), p.statement.clone()));
        }
    }
    out
}

/* ---------------- 目标 ---------------- */

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Goal {
    id: String,
    statement: String,
    #[serde(default)]
    accept: Vec<String>,
    #[serde(default)]
    reject: Vec<String>,
    #[serde(default)]
    created_unix: u64,
    #[serde(default)]
    status: String, // active | done | abandoned
    #[serde(default)]
    steps: u64,
    #[serde(default)]
    drifts: u64,
    #[serde(default)]
    last_step_unix: u64,
}

fn load_goal(dir: &Path) -> Option<Goal> {
    fs::read(dir.join(GOAL)).ok().and_then(|b| serde_json::from_slice::<Goal>(&b).ok())
}

fn save_goal(dir: &Path, g: &Goal) -> Result<()> {
    write_atomic(&dir.join(GOAL), format!("{}\n", serde_json::to_string_pretty(g)?).as_bytes(), 0o600)
}

/// 偏向性判定：命中 reject ⇒ drift；没命中 accept ⇒ off（无关联）；否则 on。
fn goal_alignment(g: &Goal, req: &Request) -> &'static str {
    let hay = format!("{} {} {}", req.command, req.text, req.paths.join(" ")).to_ascii_lowercase();
    if g.reject.iter().any(|k| !k.trim().is_empty() && hay.contains(&k.trim().to_ascii_lowercase())) {
        return "drift";
    }
    if g.accept.is_empty() {
        return "unknown";
    }
    if g.accept.iter().any(|k| !k.trim().is_empty() && hay.contains(&k.trim().to_ascii_lowercase())) {
        return "on";
    }
    "off"
}

/* ---------------- 账本 ---------------- */

fn ledger_append(dir: &Path, entry: &Value) -> Result<()> {
    ensure_dir(dir)?;
    let p = dir.join(LEDGER);
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{entry}")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn ledger_all(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join(LEDGER))
        .map(|t| t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect())
        .unwrap_or_default()
}

/// 解析 `24h` / `7d` / `90m` / epoch 秒。
fn since_epoch(s: &str) -> Result<u64> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(0);
    }
    if let Ok(n) = t.parse::<u64>() {
        return Ok(n);
    }
    let (num, unit) = t.split_at(t.len() - 1);
    let n: u64 = num.parse().map_err(|_| anyhow!("时间窗「{s}」看不懂（写 24h / 7d / 90m）"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => bail!("时间窗「{s}」的单位不认识（可选 s / m / h / d）"),
    };
    Ok(now_unix().saturating_sub(secs))
}

/* ---------------- 命令行 ---------------- */

#[derive(clap::Args)]
pub struct RsiCmd {
    #[command(subcommand)]
    pub action: RsiAction,
}

#[derive(clap::Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    pub action: PolicyAction,
}

#[derive(clap::Subcommand)]
pub enum PolicyAction {
    /// 看当前生效的策略（含"从哪个目录读的"）
    Show {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 策略文件的路径（脚本里 source 它）
    Path {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 内置预设
    Presets {
        #[arg(long)]
        json: bool,
    },
    /// 写入策略：`--preset <名>` 或 `--file <json>`
    Set {
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// 写到项目级 `.ncc-rsi/`（默认写用户级）
        #[arg(long)]
        here: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 体检：读不读得懂、规则有没有自相矛盾、当前目录会读到哪一份
    Check {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
pub struct CheckArgs {
    /// 决策请求 JSON（不给就从 stdin 读；`-` 也读 stdin）
    #[arg(long)]
    file: Option<PathBuf>,
    /// 命令行简写：动作的"一句话"（命令 / 工具名 + 参数）
    #[arg(long)]
    action: Option<String>,
    /// 命令行简写：具体命令
    #[arg(long)]
    command: Option<String>,
    /// 命令行简写：涉及的文件/目录（可重复）
    #[arg(long = "path")]
    paths: Vec<String>,
    /// 命令行简写：意图说明（人话）
    #[arg(long)]
    text: Option<String>,
    /// 目标 id（不给就用当前生效的目标）
    #[arg(long)]
    goal: Option<String>,
    /// 无人值守：**只做一件事** —— 把 warn 升级成 block
    #[arg(long)]
    unattended: bool,
    /// 只评估，不写账本
    #[arg(long)]
    dry: bool,
    /// 输出 JSON（宿主集成用；人读时给表格）
    #[arg(long)]
    json: bool,
    /// 宿主/调用方名字（记账本用）
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct GuardArgs {
    /// 为什么做这一步（记进账本；无人值守时的"谁批准过"就靠它）
    #[arg(long, default_value = "")]
    reason: String,
    /// 无人值守（会把 warn 升级成 block）
    #[arg(long)]
    unattended: bool,
    /// 失败算不算事故（默认算：异常退出会记一条 incident）
    #[arg(long)]
    no_incident_on_failure: bool,
    /// 只做检查，真拦住时也不跑（默认就是这个行为；这个开关给人看"确实拦住了"）
    #[arg(long)]
    explain: bool,
    /// 要跑的命令（`-- cmd arg…`）
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cmd: Vec<String>,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct GoalCmd {
    #[command(subcommand)]
    pub action: GoalAction,
}

#[derive(clap::Subcommand)]
pub enum GoalAction {
    /// 立一个目标（`--accept` 是"算在目标上"的词，`--reject` 是"跑偏"的词）
    Set {
        #[arg(long)]
        statement: String,
        #[arg(long = "accept")]
        accept: Vec<String>,
        #[arg(long = "reject")]
        reject: Vec<String>,
        /// 目标 id（缺省 g-<时间戳>）
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        here: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 当前目标 + 走了多少步 / 偏了几次
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 收尾：`--result ok|failed|abandoned`
    Done {
        #[arg(long, default_value = "ok")]
        result: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
pub struct PrefCmd {
    #[command(subcommand)]
    pub action: PrefAction,
}

#[derive(clap::Subcommand)]
pub enum PrefAction {
    /// 记一条偏好（`--kind avoid` 的会被当成硬约束参与 check）
    Add {
        /// 一句话（人写的，别让工具替你猜）
        statement: String,
        #[arg(long, default_value = "avoid")]
        kind: String,
        #[arg(long, default_value = "")]
        scope: String,
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        /// user-stated（人写的）/ inferred（从账本里挑出来的）
        #[arg(long, default_value = "user-stated")]
        confidence: String,
        #[arg(long)]
        here: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 我记过哪些偏好（带命中次数 —— 命中多说明它真的在起作用）
    Ls {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 删一条
    Rm {
        id: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 把账本里反复出现的拒绝理由捞出来，给人挑（**不自动写进偏好**）
    Suggest {
        #[arg(long, default_value = "30d")]
        since: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

/// 从反馈与状态里学 —— 命令树。
///
/// **默认什么都不学**：没在 `learn.json` 里声明过的来源，一律不读（声明即许可）。
#[derive(clap::Args)]
pub struct LearnArgs {
    #[command(subcommand)]
    pub action: LearnAction,
}

#[derive(clap::Subcommand)]
pub enum LearnAction {
    /// 同意声明：`show` / `on` / `off` / `set`（读哪些、读多少、脱敏、谁设的）
    Consent(LearnConsentArgs),
    /// 读已授权的来源，出一份**提案**（绝不自动生效）
    Plan(LearnPlanArgs),
    /// 显式应用某几条提案（策略类永远不自动写）
    Apply(LearnApplyArgs),
    /// 只读摘要：读了哪些来源、得出几条提案、最近几条教训
    Digest(LearnDigestArgs),
    /// 把「进化用的数据」导成一个数据集目录（来源、时刻、同意、条目）
    Export(LearnExportArgs),
}

#[derive(clap::Args)]
pub struct LearnConsentArgs {
    #[command(subcommand)]
    pub action: LearnConsentAction,
}

#[derive(clap::Subcommand)]
pub enum LearnConsentAction {
    /// 现在允许学什么（含"谁设的、什么时候、什么时候过期"）
    Show {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 打开（保留已有的来源声明）
    On {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 关掉（**声明不删**：下次开还是同一批来源）
    Off {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 设来源：`--source feedback:mine` / `mem:@me` / `kb:@me/notes` / `ckpt:@me/app` / `log:ledger`
    Set {
        #[arg(long = "source", value_name = "kind:where")]
        sources: Vec<String>,
        /// 先清空再设（不写就是增量替换同名来源）
        #[arg(long)]
        replace: bool,
        /// 脱敏词（命中的字段在导出/摘要里打码），可重复
        #[arg(long = "redact", value_name = "词")]
        redact: Vec<String>,
        /// 一次最多读多少条（预算制）
        #[arg(long, default_value_t = 200)]
        max_items: i64,
        /// 多少天后自动失效（0 = 不过期）
        #[arg(long, default_value_t = 0)]
        expires_in_days: i64,
        /// 谁设的（Agent 代设时写清楚；缺省取 NCC_AGENT）
        #[arg(long = "by-agent", default_value = "")]
        by_agent: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
pub struct LearnPlanArgs {
    /// 来源不够就明说不够，不硬凑（缺省：来源全空也算"学完了"）
    #[arg(long)]
    json: bool,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct LearnApplyArgs {
    /// 提案 id（`P-1`）或 `all`
    pub id: String,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct LearnDigestArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct LearnExportArgs {
    /// 导到哪个目录（会写 manifest.json + items.jsonl）
    #[arg(long)]
    dir: String,
    /// 只看会导出什么（写文件前先看一眼）
    #[arg(long)]
    dry: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct ReportArgs {
    #[arg(long, default_value = "24h")]
    since: String,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub action: HookAction,
}

#[derive(clap::Subcommand)]
pub enum HookAction {
    /// 装一个 sh shim（把它挂到宿主的 PreToolUse 钩子上）
    Install {
        /// claude / cursor / generic
        ///
        /// ⚠️ 叫 `--host` 而不是 `--target`：顶层 `--target` 是 `global = true` 的
        /// 目标选择器，子命令再叫 target 的话，`--target claude` 会被顶层先吃掉，
        /// 报成「没有名为 claude 的目标」（踩过）。
        #[arg(long, default_value = "generic")]
        host: String,
        /// 装到哪（缺省：项目级 `.ncc-rsi/hooks/`，没有项目级就用户级）
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
pub struct InitArgs {
    /// 在本目录建一个项目级 `.ncc-rsi/`（策略 + 目标 + 偏好都跟着这个项目走）
    #[arg(long)]
    preset: Option<String>,
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
pub enum RsiAction {
    /// 在当前目录建项目级 `.ncc-rsi/`
    Init(InitArgs),
    /// 策略：show / path / presets / set / check
    Policy(PolicyArgs),
    /// **决策门**：给一份决策请求，拿一份裁决（退出码 0/10/20）
    Check(CheckArgs),
    /// **守着跑**：先 check，block 就真的不跑；跑完记进账本
    Guard(GuardArgs),
    /// 目标：set / status / done（"不跑偏"就靠它）
    Goal(GoalCmd),
    /// 偏好：add / ls / rm / suggest
    Pref(PrefCmd),
    /// **从反馈与状态里学**：同意声明 / 出提案 / 显式应用 / 摘要 / 导出数据集
    Learn(LearnArgs),
    /// 无人值守过后的总账：拦了多少、出了几次事故、哪些偏好起了作用
    Report(ReportArgs),
    /// 接进 agent 宿主：装 PreToolUse 钩子
    Hook(HookArgs),
}

pub fn cmd(action: &RsiAction, code_out: &mut Option<i32>) -> Result<()> {
    match action {
        RsiAction::Init(a) => init(a),
        RsiAction::Policy(p) => policy_cmd(p),
        RsiAction::Check(a) => {
            let (verdict, _) = check_cmd(a)?;
            // `check` 的退出码就是裁决（宿主靠它分支）
            *code_out = Some(match verdict.as_str() {
                "allow" => EXIT_ALLOW,
                "warn" => EXIT_WARN,
                _ => EXIT_BLOCK,
            });
            Ok(())
        }
        RsiAction::Guard(a) => {
            let code = guard(a)?;
            *code_out = Some(code);
            Ok(())
        }
        RsiAction::Goal(g) => goal_cmd(g),
        RsiAction::Pref(p) => pref_cmd(p),
        RsiAction::Learn(l) => learn_cmd(l),
        RsiAction::Report(r) => report(r),
        RsiAction::Hook(h) => hook_cmd(h),
    }
}

/* ---------------- init ---------------- */

fn init(a: &InitArgs) -> Result<()> {
    let dir = a.dir.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(DIR_NAME)
    });
    if dir.join(POLICY).exists() {
        bail!("{} 里已经有策略了（要看：`ncc rsi policy show`）", dir.display());
    }
    ensure_dir(&dir)?;
    let preset = a.preset.clone().unwrap_or_else(|| "unattended-safe".into());
    let p = presets()
        .into_iter()
        .find(|(n, _, _)| *n == preset)
        .ok_or_else(|| anyhow!("没有内置预设「{preset}」（`ncc rsi policy presets` 看有哪些）"))?
        .2;
    write_atomic(&dir.join(POLICY), format!("{}\n", serde_json::to_string_pretty(&p)?).as_bytes(), 0o600)?;
    // 账本先立一个空文件，免得"还没跑过"与"读不到"分不清
    if !dir.join(LEDGER).exists() {
        write_atomic(&dir.join(LEDGER), b"", 0o600)?;
    }
    println!("✅ 项目级 rsi 已建好 {}", dir.display());
    println!("   策略     {preset}");
    println!("   接着     ncc rsi goal set --statement \"...\" --accept … --reject …");
    println!("            ncc rsi check --file request.json      # 让 agent/宿主来问");
    println!("            ncc rsi guard --reason \"发版\" -- ./deploy.sh");
    println!("   ⚠️ 这个目录（{DIR_NAME}）里会记账本 —— 里面有动作与理由，**没有密钥**；要提交就自己判断。");
    Ok(())
}

/* ---------------- policy ---------------- */

fn policy_cmd(a: &PolicyArgs) -> Result<()> {
    match &a.action {
        PolicyAction::Show { json: j, dir } => {
            let d = rsi_dir(dir.as_deref());
            let p = load_policy(&d)?;
            if *j {
                println!("{}", serde_json::to_string_pretty(&json!({"dir": d.to_string_lossy(), "policy": p}))?);
                return Ok(());
            }
            println!("策略目录 {}", d.display());
            println!("   名字     {}（version {}）", p["name"].as_str().unwrap_or("?"), p["version"].as_u64().unwrap_or(0));
            println!("   默认     {}（没命中任何规则时）", p["default"].as_str().unwrap_or("warn"));
            println!(
                "   无人值守 warn→block：{}",
                if p["unattended"]["upgradeWarnToBlock"].as_bool().unwrap_or(false) { "开" } else { "关" }
            );
            println!("   目标     requireGoal={} onDrift={}", p["goal"]["requireGoal"].as_bool().unwrap_or(false), p["goal"]["onDrift"].as_str().unwrap_or("warn"));
            println!("   偏好     命中时 {}", p["prefs"]["onConflict"].as_str().unwrap_or("warn"));
            if p["_missing"].as_bool().unwrap_or(false) {
                println!("   ⚠️ 这里**没有**策略文件 —— 现在按保守默认走（陌生动作 warn）；建一份：`ncc rsi init`");
            }
            for line in rules_of(&p) {
                println!("     {line}");
            }
            Ok(())
        }
        PolicyAction::Path { dir } => {
            println!("{}", rsi_dir(dir.as_deref()).join(POLICY).display());
            Ok(())
        }
        PolicyAction::Presets { json: j } => {
            let ps = presets();
            if *j {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ps.iter().map(|(n, d, v)| json!({"name": n, "summary": d, "policy": v})).collect::<Vec<_>>())?
                );
                return Ok(());
            }
            println!("内置策略预设");
            for (n, d, _) in ps {
                println!("  {n:<18} {d}");
            }
            Ok(())
        }
        PolicyAction::Set { preset, file, here, dir } => {
            let target = if *here {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(DIR_NAME)
            } else {
                rsi_dir(dir.as_deref())
            };
            let v = match (preset, file) {
                (Some(p), None) => presets()
                    .into_iter()
                    .find(|(n, _, _)| n == p)
                    .ok_or_else(|| anyhow!("没有内置预设「{p}」（`ncc rsi policy presets` 看有哪些）"))?
                    .2,
                (None, Some(f)) => {
                    let t = fs::read_to_string(f).with_context(|| format!("读 {} 失败", f.display()))?;
                    let v: Value = serde_json::from_str(&t).with_context(|| format!("{} 不是合法 JSON", f.display()))?;
                    if v["version"].as_u64().unwrap_or(0) != 1 {
                        bail!("策略要写 version: 1（现在读到的是 {}）", v["version"]);
                    }
                    v
                }
                (Some(_), Some(_)) => bail!("--preset 与 --file 只能给一个"),
                (None, None) => bail!("给个来源：`--preset <名>` 或 `--file <json>`"),
            };
            write_atomic(&target.join(POLICY), format!("{}\n", serde_json::to_string_pretty(&v)?).as_bytes(), 0o600)?;
            println!("✅ 策略已写入 {}", target.join(POLICY).display());
            println!("   名字 {}", v["name"].as_str().unwrap_or("(自定义)"));
            Ok(())
        }
        PolicyAction::Check { dir } => {
            let d = rsi_dir(dir.as_deref());
            let p = load_policy(&d)?;
            let mut errs: Vec<String> = Vec::new();
            if p["_missing"].as_bool().unwrap_or(false) {
                println!("  ⚠ 没有策略文件（按保守默认走）：{}", d.join(POLICY).display());
            }
            let dl = p["default"].as_str().unwrap_or("");
            if !["allow", "warn", "block"].contains(&dl) {
                errs.push(format!("default「{dl}」不合法（allow / warn / block）"));
            }
            for k in ["goal", "prefs", "unattended", "rules"] {
                if p.get(k).is_none() {
                    errs.push(format!("缺 {k} 段"));
                }
            }
            for k in ["onDrift", "onConflict", "onOffGoal"] {
                let v = match k {
                    "onDrift" => p["goal"]["onDrift"].as_str().unwrap_or(""),
                    "onOffGoal" => p["goal"]["onOffGoal"].as_str().unwrap_or(""),
                    _ => p["prefs"]["onConflict"].as_str().unwrap_or(""),
                };
                if !["allow", "warn", "block"].contains(&v) {
                    errs.push(format!("{k}「{v}」不合法（allow / warn / block）"));
                }
            }
            // 自相矛盾：同一条规则既在白名单又在拒绝名单
            let allow: Vec<String> = p["rules"]["allowCommands"].as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().unwrap_or("").to_string()).collect();
            let deny: Vec<String> = p["rules"]["denyCommands"].as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().unwrap_or("").to_string()).collect();
            for a in &allow {
                for b in &deny {
                    if a == b || wildcard_match(a, b) || wildcard_match(b, a) {
                        errs.push(format!("「{a}」同时出现在 allowCommands 与 denyCommands"));
                    }
                }
            }
            if p["name"].as_str().unwrap_or("") == "strict-prod" && allow.is_empty() {
                errs.push("strict-prod 但 allowCommands 是空的 —— 那什么都放行不了".into());
            }
            println!("策略体检 {}", d.display());
            println!("   目录     {}（项目级）", if is_project_dir(&d) { "是" } else { "否（用户级）" });
            if errs.is_empty() {
                println!("   ✔ 没问题：规则 {} 条，默认 {}，无人值守升级 {}", allow.len() + deny.len(), dl, if p["unattended"]["upgradeWarnToBlock"].as_bool().unwrap_or(false) { "开" } else { "关" });
                Ok(())
            } else {
                for e in &errs {
                    println!("   ✖ {e}");
                }
                bail!("策略有问题（{} 条）", errs.len());
            }
        }
    }
}

/* ---------------- check：决策门 ---------------- */

fn read_request(a: &CheckArgs) -> Result<Request> {
    // 命令行简写优先（好打字），否则读 JSON
    if a.action.is_some() || a.command.is_some() || !a.paths.is_empty() || a.text.is_some() {
        return Ok(Request {
            action: a.action.clone().unwrap_or_else(|| "shell".into()),
            command: a.command.clone().unwrap_or_default(),
            paths: a.paths.clone(),
            text: a.text.clone().unwrap_or_default(),
            goal_id: a.goal.clone().unwrap_or_default(),
            unattended: a.unattended,
            host: a.host.clone().unwrap_or_else(|| "cli".into()),
            dry: a.dry,
        });
    }
    let mut buf = String::new();
    match a.file.as_deref() {
        Some(p) if p.as_os_str() != "-" => {
            buf = fs::read_to_string(p).with_context(|| format!("读 {} 失败", p.display()))?;
        }
        _ => {
            std::io::stdin().read_to_string(&mut buf).context("读 stdin 失败（给 --file 或把请求 JSON 管道进来）")?;
        }
    }
    if buf.trim().is_empty() {
        bail!("没有决策请求：把请求 JSON 管道进来（或 `--file request.json`，或 `--command \"…\"` 简写）");
    }
    let v: Value = serde_json::from_str(&buf).context("决策请求不是合法 JSON")?;
    let mut req = Request::from_json(&v);
    if a.unattended {
        req.unattended = true;
    }
    if a.dry {
        req.dry = true;
    }
    if let Some(g) = &a.goal {
        req.goal_id = g.clone();
    }
    if a.host.is_some() {
        req.host = a.host.clone().unwrap_or_default();
    }
    Ok(req)
}

/// 无人值守判定：命令行 / 请求字段 / 环境变量三者任一为真。
fn unattended_now(a_flag: bool, req: &Request) -> bool {
    a_flag
        || req.unattended
        || std::env::var("NCC_RSI_UNATTENDED").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

struct Verdict {
    verdict: String,
    reasons: Vec<String>,
    prefs_hit: Vec<String>,
    goal_state: String,
    goal_id: String,
    policy_name: String,
    unattended: bool,
    upgraded: bool,
}

fn evaluate(dir: &Path, req: &Request, forced_unattended: bool) -> Result<Verdict> {
    let policy = load_policy(dir)?;
    let unattended = unattended_now(forced_unattended, req);
    let mut reasons: Vec<String> = Vec::new();
    let mut verdict = policy["default"].as_str().unwrap_or("warn").to_string();

    let rl = |k: &str| -> Vec<String> {
        policy["rules"][k].as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let hay = format!("{} {} {}", req.command, req.text, req.paths.join(" "));

    // ① 白名单：命中就基本放行（但仍会看 deny —— deny 优先）
    let allow = rl("allowCommands");
    let allowed = !allow.is_empty() && allow.iter().any(|p| wildcard_match(p, &req.command));
    if allowed {
        verdict = "allow".into();
        reasons.push(format!("命中 allowCommands（{}）", req.command));
    }

    // ② 拒绝名单（命令）
    for pat in rl("denyCommands") {
        if wildcard_match(&pat, &req.command) || wildcard_match(&pat, &hay) {
            verdict = "block".into();
            reasons.push(format!("命中 denyCommands「{pat}」"));
        }
    }
    // ③ 拒绝名单（路径）
    if let Some(msg) = path_hits(&rl("denyPaths"), &req.paths, &home) {
        verdict = "block".into();
        reasons.push(msg);
    }
    // ④ 要人确认的
    if !allowed {
        for pat in rl("confirmCommands") {
            if wildcard_match(&pat, &req.command) {
                if verdict != "block" {
                    verdict = "warn".into();
                }
                reasons.push(format!("命中 confirmCommands「{pat}」—— 要人点头（无人值守时会被拦）"));
            }
        }
    }
    // ⑤ 风险词（只在没有更强结论时抬到 warn）
    let risk = rl("riskWords");
    let hit_words: Vec<String> = risk.iter().filter(|w| !w.trim().is_empty() && hay.to_ascii_lowercase().contains(&w.to_ascii_lowercase())).cloned().collect();
    if !hit_words.is_empty() {
        if verdict == "allow" && !allowed {
            verdict = "warn".into();
        }
        reasons.push(format!("提到风险词：{}", hit_words.join(" / ")));
    }

    // ⑥ 偏好（人说过"不要"的）
    let pref_file = load_prefs(dir);
    let mut prefs_hit: Vec<String> = Vec::new();
    if policy["prefs"]["apply"].as_bool().unwrap_or(true) && !pref_file.prefs.is_empty() {
        let hits = pref_hits(&pref_file.prefs, req);
        for (id, st) in hits {
            prefs_hit.push(id.clone());
            let on = policy["prefs"]["onConflict"].as_str().unwrap_or("warn");
            reasons.push(format!("与偏好 {id} 冲突：「{st}」"));
            match on {
                "block" => verdict = "block".into(),
                "warn" if verdict == "allow" => verdict = "warn".into(),
                _ => {}
            }
        }
    }

    // ⑦ 目标（不跑偏）
    let goal = load_goal(dir);
    let (goal_state, goal_id) = match &goal {
        Some(g) if g.status == "active" && (req.goal_id.is_empty() || req.goal_id == g.id) => {
            let st = goal_alignment(g, req);
            match st {
                "drift" => {
                    let on = policy["goal"]["onDrift"].as_str().unwrap_or("block");
                    reasons.push(format!("与目标「{}」的 reject 词命中 —— 这一步在跑偏", g.statement));
                    match on {
                        "block" => verdict = "block".into(),
                        "warn" if verdict == "allow" => verdict = "warn".into(),
                        _ => {}
                    }
                }
                "off" => {
                    reasons.push(format!("没看出和当前目标「{}」的关系（accept 词没命中）", g.statement));
                    match policy["goal"]["onOffGoal"].as_str().unwrap_or("warn") {
                        "block" => verdict = "block".into(),
                        "allow" => {}
                        _ if verdict == "allow" => verdict = "warn".into(),
                        _ => {}
                    }
                }
                "unknown" => reasons.push("当前目标没写 accept 词 —— 只能判「是否跑偏」，判不了「是否在目标上」".into()),
                _ => {}
            }
            (st.to_string(), g.id.clone())
        }
        Some(g) if policy["goal"]["requireGoal"].as_bool().unwrap_or(false) && req.goal_id != g.id && !req.goal_id.is_empty() => {
            reasons.push(format!("指定了目标 {}，但当前生效的是 {}", req.goal_id, g.id));
            if verdict == "allow" {
                verdict = "warn".into();
            }
            ("mismatch".into(), req.goal_id.clone())
        }
        _ => {
            if policy["goal"]["requireGoal"].as_bool().unwrap_or(false) {
                reasons.push("策略要求有目标（goal.requireGoal），但现在没有生效的目标 —— 「在做什么」没人说得清".into());
                if verdict == "allow" {
                    verdict = "warn".into();
                }
            }
            ("none".into(), req.goal_id.clone())
        }
    };

    // ⑧ 无人值守：只升不降
    let mut upgraded = false;
    if unattended && verdict == "warn" && policy["unattended"]["upgradeWarnToBlock"].as_bool().unwrap_or(true) {
        verdict = "block".into();
        upgraded = true;
        reasons.push("无人值守：按策略把 warn 升级成 block（**只会更严，不会更松**）".into());
    }
    if reasons.is_empty() {
        reasons.push(if allowed { "命中白名单".into() } else { "没有命中任何规则".into() });
    }
    Ok(Verdict {
        verdict,
        reasons,
        prefs_hit,
        goal_state,
        goal_id,
        policy_name: policy["name"].as_str().unwrap_or("none").to_string(),
        unattended,
        upgraded,
    })
}

fn check_cmd(a: &CheckArgs) -> Result<(String, Verdict)> {
    let req = read_request(a)?;
    let dir = rsi_dir(a.dir.as_deref());
    let v = evaluate(&dir, &req, a.unattended)?;
    if !req.dry {
        ledger_append(
            &dir,
            &json!({
                "at": now_unix(), "act": "check", "by": whoami(), "host": req.host,
                "action": req.action, "command": req.command, "paths": req.paths, "text": req.text,
                "verdict": v.verdict, "reasons": v.reasons, "policy": v.policy_name,
                "goalId": v.goal_id, "goalState": v.goal_state, "prefs": v.prefs_hit,
                "unattended": v.unattended, "upgraded": v.upgraded,
            }),
        )?;
        // 偏好命中计数（"这条偏好真的在起作用"要有证据）
        if !v.prefs_hit.is_empty() {
            let mut f = load_prefs(&dir);
            for p in f.prefs.iter_mut() {
                if v.prefs_hit.contains(&p.id) {
                    p.hits += 1;
                }
            }
            let _ = save_prefs(&dir, &f);
        }
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "verdict": v.verdict,
                "exitCode": match v.verdict.as_str() { "allow" => EXIT_ALLOW, "warn" => EXIT_WARN, _ => EXIT_BLOCK },
                "reasons": v.reasons,
                "policy": v.policy_name,
                "goal": {"id": v.goal_id, "state": v.goal_state},
                "prefs": v.prefs_hit,
                "unattended": v.unattended,
                "upgraded": v.upgraded,
                "dir": dir.to_string_lossy(),
            }))?
        );
    } else {
        let icon = match v.verdict.as_str() {
            "allow" => "✅ 放行",
            "warn" => "⚠️ 可以做，但留痕（无人值守会被拦）",
            _ => "⛔ 拦住",
        };
        println!("{icon}（策略 {} · 目标 {} · 退出码 {}）", v.policy_name, if v.goal_state.is_empty() { "无" } else { &v.goal_state }, match v.verdict.as_str() { "allow" => 0, "warn" => 10, _ => 20 });
        for r in &v.reasons {
            println!("   · {r}");
        }
        if !v.prefs_hit.is_empty() {
            println!("   （命中偏好：{}）", v.prefs_hit.join(" · "));
        }
    }
    Ok((v.verdict.clone(), v))
}

/* ---------------- guard：守着跑 ---------------- */

fn guard(a: &GuardArgs) -> Result<i32> {
    if a.cmd.is_empty() {
        bail!("要跑什么：`ncc rsi guard --reason \"…\" -- <命令> [参数…]`");
    }
    let dir = rsi_dir(a.dir.as_deref());
    let cmdline = a.cmd.join(" ");
    let check = CheckArgs {
        file: None,
        action: Some("shell".into()),
        command: Some(cmdline.clone()),
        paths: vec![],
        text: Some(a.reason.clone()),
        goal: None,
        unattended: a.unattended,
        dry: false,
        json: false,
        host: Some("rsi-guard".into()),
        dir: a.dir.clone(),
    };
    let (verdict, v) = check_cmd(&check)?;
    if verdict == "block" {
        println!("⛔ 没有执行：{}", a.cmd.join(" "));
        for r in &v.reasons {
            println!("   · {r}");
        }
        println!("   （这是 `rsi guard` 的全部意义：**先判再跑**。要人工放行就跑命令本身，或改策略。）");
        return Ok(EXIT_BLOCK);
    }
    if a.explain {
        println!("（--explain：裁决是 {verdict}，本该执行；这里只看不跑）");
        return Ok(EXIT_ALLOW);
    }
    println!("▶ 执行：{}（裁决 {verdict}）", a.cmd.join(" "));
    let t0 = std::time::Instant::now();
    let st = std::process::Command::new(&a.cmd[0]).args(&a.cmd[1..]).status();
    let code = st.as_ref().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    let ms = t0.elapsed().as_millis() as u64;
    let failed = code != 0;
    ledger_append(
        &dir,
        &json!({
            "at": now_unix(), "act": "guard", "by": whoami(), "host": "rsi-guard",
            "command": cmdline, "reason": a.reason, "exitCode": code, "ms": ms,
            "verdict": verdict, "goalId": v.goal_id,
            "incident": failed && !a.no_incident_on_failure,
        }),
    )?;
    if let Some(e) = st.err() {
        bail!("起不了命令：{e}");
    }
    if failed && !a.no_incident_on_failure {
        println!("⚠️ 这一步非 0 退出（{code}），已按**事故**记进账本：`ncc rsi report` 能看到");
    } else {
        println!("✔ 退出码 {code}（{ms}ms）");
    }
    Ok(code)
}

/* ---------------- goal ---------------- */

/// `--accept "deploy,release"` 与 `--accept deploy --accept release` 两种写法都要认。
/// （差点就只认后者：冒烟里按常用写法给了一串逗号，结果一个都没拆开，目标永远判 off。）
fn split_list(items: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for it in items {
        for piece in it.split([',', '，']) {
            let p = piece.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn goal_cmd(a: &GoalCmd) -> Result<()> {
    match &a.action {
        GoalAction::Set { statement, accept, reject, id, here, dir } => {
            let d = if *here {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(DIR_NAME)
            } else {
                rsi_dir(dir.as_deref())
            };
            if let Some(old) = load_goal(&d) {
                if old.status == "active" {
                    bail!(
                        "已经有一个进行中的目标：{}（{}）——\n  先收尾：`ncc rsi goal done --result ok`，或者把旧目标挪走",
                        old.statement,
                        old.id
                    );
                }
            }
            if statement.trim().is_empty() {
                bail!("目标要写清楚（--statement）");
            }
            let g = Goal {
                id: id.clone().unwrap_or_else(|| format!("g-{}", now_unix())),
                statement: statement.trim().to_string(),
                accept: split_list(accept),
                reject: split_list(reject),
                created_unix: now_unix(),
                status: "active".into(),
                steps: 0,
                drifts: 0,
                last_step_unix: 0,
            };
            save_goal(&d, &g)?;
            println!("🎯 目标 {}", g.id);
            println!("   {statement}");
            if !g.accept.is_empty() {
                println!("   算在目标上：{}", g.accept.join(" · "));
            }
            if !g.reject.is_empty() {
                println!("   跑偏信号  ：{}", g.reject.join(" · "));
            }
            println!("   之后的 `ncc rsi check` / `guard` 都会拿它判「在不在目标上」");
            Ok(())
        }
        GoalAction::Status { json: j, dir } => {
            let d = rsi_dir(dir.as_deref());
            let g = load_goal(&d);
            let led = ledger_all(&d);
            let steps = led.iter().filter(|e| e["act"].as_str() == Some("guard") || e["act"].as_str() == Some("check")).count() as u64;
            let drifts = led.iter().filter(|e| e["goalState"].as_str() == Some("drift")).count() as u64;
            let blocked = led.iter().filter(|e| e["verdict"].as_str() == Some("block")).count() as u64;
            if *j {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"dir": d.to_string_lossy(), "goal": g, "steps": steps, "drifts": drifts, "blocked": blocked}))?
                );
                return Ok(());
            }
            match g {
                None => println!("当前没有目标（`ncc rsi goal set --statement \"…\" --accept … --reject …`）\n  目录 {}", d.display()),
                Some(g) => {
                    println!("🎯 {}（{}）", g.statement, g.status);
                    println!("   id       {}", g.id);
                    println!("   立的时间 {}", fmt_epoch(g.created_unix as i64));
                    if !g.accept.is_empty() {
                        println!("   算在目标上 {}", g.accept.join(" / "));
                    }
                    if !g.reject.is_empty() {
                        println!("   跑偏信号   {}", g.reject.join(" / "));
                    }
                    println!("   账本     {steps} 步 · 跑偏 {drifts} 次 · 被拦 {blocked} 次");
                    if drifts > 0 {
                        println!("   ⚠️ 跑偏过 {drifts} 次 —— `ncc rsi report` 看是哪几步");
                    }
                }
            }
            Ok(())
        }
        GoalAction::Done { result, note, dir } => {
            let d = rsi_dir(dir.as_deref());
            let mut g = load_goal(&d).ok_or_else(|| anyhow!("当前没有目标"))?;
            let st = match result.as_str() {
                "ok" => "done",
                "abandoned" | "abort" => "abandoned",
                "failed" => "failed",
                other => bail!("--result 只认 ok / failed / abandoned（给的是 {other}）"),
            };
            g.status = st.into();
            save_goal(&d, &g)?;
            ledger_append(
                &d,
                &json!({"at": now_unix(), "act": "goal-done", "by": whoami(), "goalId": g.id, "result": st, "note": note}),
            )?;
            println!("🎯 目标 {} 收尾：{st}{}", g.id, if note.is_empty() { String::new() } else { format!("（{note}）") });
            Ok(())
        }
    }
}

/* ---------------- pref ---------------- */

fn pref_cmd(a: &PrefCmd) -> Result<()> {
    match &a.action {
        PrefAction::Add { statement, kind, scope, evidence, confidence, here, dir } => {
            let d = if *here {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(DIR_NAME)
            } else {
                rsi_dir(dir.as_deref())
            };
            if !["avoid", "prefer"].contains(&kind.as_str()) {
                bail!("--kind 只认 avoid（硬约束，会参与 check）/ prefer（建议，不拦人）");
            }
            if statement.trim().chars().count() < 4 {
                bail!("偏要写成人话（至少几个字）：偏好要能被人复核，太短了没法判");
            }
            let mut f = load_prefs(&d);
            let id = format!("P-{}", f.prefs.len() + 1);
            f.prefs.push(Pref {
                id: id.clone(),
                statement: statement.trim().to_string(),
                scope: scope.clone(),
                kind: kind.clone(),
                evidence: evidence.clone(),
                confidence: confidence.clone(),
                hits: 0,
                created_unix: now_unix(),
            });
            save_prefs(&d, &f)?;
            ledger_append(&d, &json!({"at": now_unix(), "act": "pref-add", "by": whoami(), "prefId": id, "statement": statement, "kind": kind}))?;
            println!("📝 记下偏好 {id}（{kind}）：{statement}");
            if confidence == "inferred" {
                println!("   ⚠️ 标的是 inferred —— 这是**工具猜的**，请人复核后再让它拦人");
            }
            Ok(())
        }
        PrefAction::Ls { json: j, dir } => {
            let d = rsi_dir(dir.as_deref());
            let f = load_prefs(&d);
            if *j {
                println!("{}", serde_json::to_string_pretty(&f)?);
                return Ok(());
            }
            if f.prefs.is_empty() {
                println!("还没记过偏好（`ncc rsi pref add \"不要把凭据写进仓库\"`）\n  目录 {}", d.display());
                return Ok(());
            }
            println!("偏好 {} 条（{}）", f.prefs.len(), d.display());
            for p in &f.prefs {
                println!(
                    "  {:<8} {:<7} 命中 {:<3} {}",
                    p.id,
                    p.kind,
                    p.hits,
                    p.statement
                );
                if !p.evidence.is_empty() {
                    println!("           依据 {}", p.evidence.join(" · "));
                }
                if p.confidence == "inferred" {
                    println!("           ⚠️ inferred（工具猜的，等人复核）");
                }
            }
            Ok(())
        }
        PrefAction::Rm { id, dir } => {
            let d = rsi_dir(dir.as_deref());
            let mut f = load_prefs(&d);
            let before = f.prefs.len();
            f.prefs.retain(|p| p.id != *id);
            if f.prefs.len() == before {
                bail!("没有偏好 {id}");
            }
            save_prefs(&d, &f)?;
            println!("已删 {id}");
            Ok(())
        }
        PrefAction::Suggest { since, json: j, dir } => {
            let d = rsi_dir(dir.as_deref());
            let from = since_epoch(since)?;
            let led = ledger_all(&d);
            // 从账本里找"反复被拦的理由"，给它计数 —— 给人挑，不自动写进偏好
            let mut counts: std::collections::BTreeMap<String, u64> = Default::default();
            for e in &led {
                if e["at"].as_u64().unwrap_or(0) < from {
                    continue;
                }
                if e["verdict"].as_str() == Some("block") || e["verdict"].as_str() == Some("warn") {
                    for r in e["reasons"].as_array().cloned().unwrap_or_default() {
                        if let Some(s) = r.as_str() {
                            *counts.entry(s.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }
            let mut v: Vec<(String, u64)> = counts.into_iter().filter(|(_, n)| *n >= 2).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            if *j {
                println!("{}", serde_json::to_string_pretty(&v.iter().map(|(r, n)| json!({"reason": r, "count": n})).collect::<Vec<_>>())?);
                return Ok(());
            }
            if v.is_empty() {
                println!("账本里还没有「反复出现的理由」（同一理由至少出现 2 次才提）");
                return Ok(());
            }
            println!("这些理由出现过不止一次 —— 看着像「该写进偏好」的东西（**要人确认**）：");
            for (r, n) in v.iter().take(10) {
                println!("  {n:>3} 次  {r}");
            }
            println!("\n真要记：`ncc rsi pref add \"...\" --evidence \"来自账本 {}\" --confidence inferred`", fmt_epoch(from as i64));
            Ok(())
        }
    }
}

/* ---------------- report ---------------- */

/// **给 Agent 面用的决策门**（`ncc mcp` 的 `ncc_rsi_check`）：给一份决策请求，拿一份裁决 JSON。
///
/// 与 `rsi check` 同一套判断、同一本账：
///   · 只回答「能不能做」，**不执行任何东西**（Agent 拿到 block 就该换做法，而不是绕过去）；
///   · 仍然写账本（Agent 问过门这件事本身是事实，审计要靠它）；
///   · 仍然给偏好计数（"这条偏好真的在拦人"要有证据）。
///
/// 为什么不在 MCP 里另写一套判断：那样就会出现「CLI 拦住、工具放行」这种最坏的不一致。
pub fn check_json(
    action: &str,
    command: &str,
    text: &str,
    paths: &[String],
    unattended: bool,
    dry: bool,
    dir: Option<&Path>,
) -> Result<Value> {
    let d = rsi_dir(dir);
    let req = Request {
        action: action.to_string(),
        command: command.to_string(),
        text: text.to_string(),
        paths: paths.to_vec(),
        host: std::env::var("NCC_AGENT").unwrap_or_else(|_| "mcp".into()),
        dry,
        unattended,
        goal_id: String::new(),
    };
    let v = evaluate(&d, &req, unattended)?;
    // `dry` 真的不写（连偏好计数也不加）：否则"只看一眼"会留下痕迹。
    if !dry {
        ledger_append(
            &d,
            &json!({
                "at": now_unix(), "act": "check", "by": whoami(), "host": req.host,
                "via": "mcp",
                "action": req.action, "command": req.command, "paths": req.paths, "text": req.text,
                "verdict": v.verdict, "reasons": v.reasons, "policy": v.policy_name,
                "goalId": v.goal_id, "goalState": v.goal_state, "prefs": v.prefs_hit,
                "unattended": v.unattended, "upgraded": v.upgraded,
            }),
        )?;
        if !v.prefs_hit.is_empty() {
            let mut f = load_prefs(&d);
            for p in f.prefs.iter_mut() {
                if v.prefs_hit.contains(&p.id) {
                    p.hits += 1;
                }
            }
            let _ = save_prefs(&d, &f);
        }
    }
    Ok(json!({
        "verdict": v.verdict,
        "exitCode": match v.verdict.as_str() { "allow" => EXIT_ALLOW, "warn" => EXIT_WARN, _ => EXIT_BLOCK },
        "reasons": v.reasons,
        "policy": v.policy_name,
        "goal": {"id": v.goal_id, "state": v.goal_state},
        "prefs": v.prefs_hit,
        "unattended": v.unattended,
        "upgraded": v.upgraded,
        "dir": d.to_string_lossy(),
        "note": "这是**裁决**不是执行：block 就别做那一步（换做法），别绕过去",
    }))
}

/// **给 Agent 面用的总账**（`ncc mcp` 的 `ncc_rsi_report`）：与 `rsi report --json` 同一份。
pub fn report_json(since: &str, dir: Option<&Path>) -> Result<Value> {
    let d = rsi_dir(dir);
    let from = since_epoch(since)?;
    let led: Vec<Value> = ledger_all(&d).into_iter().filter(|e| e["at"].as_u64().unwrap_or(0) >= from).collect();
    let count_verdict = |want: &str| -> u64 {
        led.iter()
            .filter(|e| e["verdict"].as_str() == Some(want) && e["act"].as_str() != Some("guard"))
            .count() as u64
    };
    let goal = load_goal(&d);
    let prefs_hits: u64 = load_prefs(&d).prefs.iter().map(|p| p.hits).sum();
    Ok(json!({
        "dir": d.to_string_lossy(), "since": since,
        "checks": {"allow": count_verdict("allow"), "warn": count_verdict("warn"), "block": count_verdict("block")},
        "guards": led.iter().filter(|e| e["act"].as_str() == Some("guard")).count() as u64,
        "incidents": led.iter().filter(|e| e["incident"].as_bool().unwrap_or(false)).count() as u64,
        "drifts": led.iter().filter(|e| e["goalState"].as_str() == Some("drift")).count() as u64,
        "prefHits": prefs_hits,
        "goal": goal.map(|g| json!({"id": g.id, "statement": g.statement, "status": g.status})),
        "note": "账本只记动作与理由，**不记密钥、不记文件内容**",
    }))
}

fn report(a: &ReportArgs) -> Result<()> {
    let d = rsi_dir(a.dir.as_deref());
    let from = since_epoch(&a.since)?;
    let led: Vec<Value> = ledger_all(&d).into_iter().filter(|e| e["at"].as_u64().unwrap_or(0) >= from).collect();
    let count_verdict = |want: &str| -> u64 { led.iter().filter(|e| e["verdict"].as_str() == Some(want) && e["act"].as_str() != Some("guard")).count() as u64 };
    let allowed = count_verdict("allow");
    let warned = count_verdict("warn");
    let blocked = count_verdict("block");
    let guards = led.iter().filter(|e| e["act"].as_str() == Some("guard")).count() as u64;
    let incidents = led.iter().filter(|e| e["incident"].as_bool().unwrap_or(false)).count() as u64;
    let drifts = led.iter().filter(|e| e["goalState"].as_str() == Some("drift")).count() as u64;
    let goal = load_goal(&d);
    let prefs_hits: u64 = load_prefs(&d).prefs.iter().map(|p| p.hits).sum();

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dir": d.to_string_lossy(), "since": a.since,
                "checks": {"allow": allowed, "warn": warned, "block": blocked},
                "guards": guards, "incidents": incidents, "drifts": drifts,
                "prefHits": prefs_hits,
                "goal": goal.map(|g| json!({"id": g.id, "statement": g.statement, "status": g.status})),
                "note": "账本只记动作与理由，**不记密钥、不记文件内容**",
            }))?
        );
        return Ok(());
    }
    println!("RSI 总账（{} · 窗口 {}）", d.display(), a.since);
    println!("   决策     {} 次：放行 {allowed} · 留痕 {warned} · **拦住 {blocked}**", allowed + warned + blocked);
    println!("   守着跑   {guards} 次");
    println!("   事故     {incidents} 次{}", if incidents > 0 { "（非 0 退出，账本里能查到是哪一步）" } else { "" });
    println!("   跑偏     {drifts} 次");
    println!("   偏好命中 {prefs_hits} 次（命中多说明它真的在拦人）");
    match goal {
        Some(g) => println!("   目标     {}（{}）", g.statement, g.status),
        None => println!("   目标     没立（「在做什么」没人说得清 —— `ncc rsi goal set`）"),
    }
    if let Some(last) = led.last() {
        println!("   最近一条 {} {}", fmt_epoch(last["at"].as_u64().unwrap_or(0) as i64), last["command"].as_str().or_else(|| last["text"].as_str()).unwrap_or(""));
    }
    Ok(())
}

/* ---------------- hook ---------------- */

const SHIM: &str = r#"#!/bin/sh
# ncc rsi 的 PreToolUse 钩子（由 `ncc rsi hook install` 生成，可以随时改）
#
# 宿主把工具调用以 JSON 从 stdin 喂进来 → 交给 `ncc rsi check` → 用退出码回答：
#   0  放行
#   10 留痕放行（无人值守时策略通常会把它升级成 20）
#   20 拦住（Claude Code 的 PreToolUse 约定：退出码 2 才是 block，见下面 NCC_RSI_BLOCK_CODE）
#   1  配置/用法错误 —— **不要**把它当成放行
#
# 无人值守：把 NCC_RSI_UNATTENDED=1 放进环境（cron / CI / 定时任务里设上就行）。
NCC_BIN="${NCC_BIN:-ncc}"
BLOCK_CODE="${NCC_RSI_BLOCK_CODE:-2}"   # Claude Code 用 2；generic 用 20

out="$("$NCC_BIN" rsi check --json --host "${NCC_RSI_HOST:-hook}" 2>&1)"
rc=$?
# 裁决原文给宿主看（很多宿主会把 stderr 显示给模型/人）
echo "$out" >&2
case "$rc" in
  0)  exit 0 ;;
  10) exit 0 ;;                    # 留痕放行：写成 0 让宿主继续；账本里已经记了 warn
  20) exit "$BLOCK_CODE" ;;        # 拦住
  *)  echo "rsi: check 自己失败了（退出码 $rc）—— 按「拦住」处理" >&2; exit "$BLOCK_CODE" ;;
esac
"#;

fn hook_cmd(a: &HookArgs) -> Result<()> {
    match &a.action {
        HookAction::Install { host, dir } => {
            let d = dir.clone().unwrap_or_else(|| {
                let cand = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(DIR_NAME);
                if cand.is_dir() {
                    cand
                } else {
                    rsi_dir(None)
                }
            });
            if !["claude", "cursor", "generic"].contains(&host.as_str()) {
                bail!("--host 只认 claude / cursor / generic（给的是 {host}）");
            }
            let hd = d.join("hooks");
            ensure_dir(&hd)?;
            let shim = hd.join("pretooluse.sh");
            write_atomic(&shim, SHIM.as_bytes(), 0o700)?;
            println!("✅ 钩子脚本 {}", shim.display());
            match host.as_str() {
                "claude" => {
                    println!("\n挂到 Claude Code（项目级）：把这段并进 .claude/settings.json");
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "hooks": {
                                "PreToolUse": [{
                                    "matcher": "Bash|Edit|Write",
                                    "hooks": [{ "type": "command", "command": format!("NCC_RSI_BLOCK_CODE=2 {}", shim.display()) }]
                                }]
                            }
                        }))?
                    );
                    println!("\n（无人值守：在跑 agent 的环境里 `export NCC_RSI_UNATTENDED=1`）");
                }
                "cursor" => {
                    println!("\n挂到 Cursor：它认 beforeShellExecution 之类的钩子 —— 命令里写");
                    println!("  {}（stdin 给 JSON、退出码 20 = 拦）", shim.display());
                    println!("  具体字段名以你那份 Cursor 版本为准（这里不猜它）。");
                }
                _ => {
                    println!("\n通用接法：任何宿主，把工具调用 JSON 从 stdin 喂进来就行");
                    println!("  echo '{{\"command\":\"rm -rf /\"}}' | {}   # 退出码 0/10/20", shim.display());
                }
            }
            println!("\n⚠️ 钩子是**你自己的**脚本，随时可改；ncc 只提供 `rsi check` 这一个判据。");
            Ok(())
        }
    }
}

/* ================================================================
   learn：从**反馈**与**状态**里学（RSI 的另一半：增强与提升）
   ================================================================

   用户的原话：

   > 「RSI 可以通过 Feedback、state（mem, ckpt, log, knowledgebase）等内容进行自改进和学习；
   >   用户可以设置或通过 Agent 设置 RSI 可以用于进化的轨迹与数据。」

   于是这里做四件事：**声明（同意）/ 读取 / 出提案 / 显式应用**。四条红线：

     1. **默认关闭**：没有 `learn.json` 或 `enabled=false` → 一个来源都不读。
     2. **声明即许可**：`sources` 里没写的来源**一律不读**（还要在输出里说清跳过了哪些）。
     3. **只读**：学习不改任何来源数据；写只写 `.ncc-rsi/` 里的文件。
     4. **提案不自动生效**：`plan` 只出提案；`apply` 要人（或人授权的 Agent）点名。
        **策略类提案永远不自动写** —— 那等于让工具自己给自己松绑。
*/

const LEARN: &str = "learn.json";
const PROPOSALS: &str = "proposals.json";
const LESSONS: &str = "lessons.jsonl";

/// 允许的学习来源种类（少而清楚：多一种就要多一份"它凭什么被读"的说法）。
const LEARN_KINDS: [&str; 5] = ["feedback", "mem", "kb", "ckpt", "log"];

/// 一条来源声明。
#[derive(Serialize, Deserialize, Clone)]
struct LearnSource {
    id: String,
    kind: String,
    /// 取哪儿：`mine`（我的）/ `@ns/slug`（某个东西）/ `ledger`（本机账本）/ 空（默认范围）
    #[serde(default)]
    r#where: String,
    #[serde(default)]
    limit: i64,
}

/// 同意声明（`.ncc-rsi/learn.json`）。
///
/// 刻意把 `set_by_*` 与 `set_at` 写进文件：**谁允许的、什么时候允许的**要留痕 ——
/// Agent 代设时更要留（`--by-agent`），因为"它能读我的记忆"这件事必须有人认账。
#[derive(Serialize, Deserialize, Clone)]
struct LearnConsent {
    #[serde(default = "one")]
    version: u64,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    sources: Vec<LearnSource>,
    #[serde(default)]
    redact: Vec<String>,
    #[serde(default = "default_max_items")]
    max_items: i64,
    #[serde(default)]
    expires_unix: u64,
    #[serde(default)]
    set_by_user: String,
    #[serde(default)]
    set_by_agent: String,
    #[serde(default)]
    set_at: u64,
}

fn one() -> u64 {
    1
}

fn default_max_items() -> i64 {
    200
}

impl Default for LearnConsent {
    fn default() -> Self {
        LearnConsent {
            version: 1,
            enabled: false,
            sources: Vec::new(),
            redact: Vec::new(),
            max_items: default_max_items(),
            expires_unix: 0,
            set_by_user: String::new(),
            set_by_agent: String::new(),
            set_at: 0,
        }
    }
}

fn learn_file(dir: &Path) -> PathBuf {
    dir.join(LEARN)
}

fn load_consent(dir: &Path) -> Option<LearnConsent> {
    fs::read(learn_file(dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<LearnConsent>(&b).ok())
}

fn save_consent(dir: &Path, c: &LearnConsent) -> Result<()> {
    write_atomic(&learn_file(dir), format!("{}\n", serde_json::to_string_pretty(c)?).as_bytes(), 0o600)
}

fn consent_expired(c: &LearnConsent) -> bool {
    c.expires_unix > 0 && now_unix() > c.expires_unix
}

/// 读同意声明并检查"现在能不能学"（不能就明说为什么，别默默什么都不做）。
fn require_consent(dir: &Path) -> Result<LearnConsent> {
    let c = load_consent(dir).ok_or_else(|| {
        anyhow!(
            "这里还没有学习同意声明（{}）—— 默认**什么都不学**。\n  \
             先看一眼要读什么：`ncc rsi learn consent show`\n  \
             再打开并声明来源：`ncc rsi learn consent set --source feedback:mine --source log:ledger` 然后 `ncc rsi learn consent on`",
            learn_file(dir).display()
        )
    })?;
    if !c.enabled {
        bail!(
            "学习是关着的（{} 里 enabled=false）。要开：`ncc rsi learn consent on`（声明还在，来源不用重配）",
            learn_file(dir).display()
        );
    }
    if consent_expired(&c) {
        bail!(
            "这份同意已经在 {} 过期了 —— 过期就不再读（要续：`ncc rsi learn consent set --expires-in-days <天数>`）",
            fmt_epoch(c.expires_unix as i64)
        );
    }
    Ok(c)
}

/// `kind:where` → 一条来源声明（`where` 可省：`feedback` = `feedback:mine`）。
fn parse_source(raw: &str) -> Result<LearnSource> {
    let s = raw.trim();
    let (kind, w) = match s.split_once(':') {
        Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
        None => (s.to_string(), String::new()),
    };
    if !LEARN_KINDS.contains(&kind.as_str()) {
        bail!("来源种类只认 {}（给的是 {kind}）", LEARN_KINDS.join("|"));
    }
    let w = match (kind.as_str(), w.as_str()) {
        ("feedback", "") => "mine".to_string(),
        ("log", "") => "ledger".to_string(),
        (_, v) => v.to_string(),
    };
    Ok(LearnSource {
        id: format!("L{}", 0),
        kind,
        r#where: w,
        limit: 0,
    })
}

/// 脱敏（导出与摘要里都走它）：命中词的字段打码，**原值不进任何输出**。
fn redact_text(s: &str, words: &[String]) -> String {
    let mut out = s.to_string();
    for w in words {
        let w = w.trim();
        if w.is_empty() {
            continue;
        }
        out = out.replace(w, "***");
    }
    out
}

/// 收集到的一条"学习材料"。
///
/// `text` 是**引用级别的摘要**（谁、什么时候、去了哪儿），不是原文搬运 —— 学习要能说清依据，
/// 但不该把一份语料整本拷进 `.ncc-rsi/`。
#[derive(Serialize, Deserialize, Clone)]
struct LearnItem {
    source: String,
    kind: String,
    r#ref: String,
    at: u64,
    text: String,
    /// 额外事实（如 `kind=preference` / `agent=claude-code` / 标签）
    #[serde(default)]
    meta: BTree<String, String>,
}

/// BTreeMap 的别名（保序的 key-value，打印稳定）。
type BTree<K, V> = std::collections::BTreeMap<K, V>;

fn item(source: &str, kind: &str, r#ref: &str, at: u64, text: &str) -> LearnItem {
    LearnItem {
        source: source.to_string(),
        kind: kind.to_string(),
        r#ref: r#ref.to_string(),
        at,
        text: text.to_string(),
        meta: BTree::new(),
    }
}

/// 一台目标的能力清单（探测不到 = 未知 = 不拦，交给具体请求报错）。
fn target_caps(cfg: &crate::config::CliConfig) -> Vec<String> {
    crate::capability::probe(cfg).capabilities
}

/// 读本机账本（log:ledger）：把"发生过什么"折成学习材料。
fn collect_ledger(dir: &Path, limit: i64) -> (Vec<LearnItem>, BTree<String, i32>) {
    let raw = fs::read_to_string(dir.join(LEDGER)).unwrap_or_default();
    let mut items = Vec::new();
    // 理由 → 次数（反复出现的才算"值得学的信号"）
    let mut why_count: BTree<String, i32> = BTree::new();
    for line in raw.lines().rev() {
        if items.len() as i64 >= limit.max(1) {
            break;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let at = v["at"].as_u64().unwrap_or(0);
        let act = v["act"].as_str().unwrap_or("");
        let verdict = v["verdict"].as_str().unwrap_or("");
        let reasons: Vec<String> = v["reasons"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.as_str().map(str::to_string))
            .collect();
        let mut it = item("log:ledger", if act == "guard" { "run" } else { "decision" },
            &v["action"].as_str().unwrap_or(""), at,
            &format!("{act} {verdict}: {}", reasons.join("；")));
        it.meta.insert("verdict".into(), verdict.to_string());
        if let Some(g) = v["goalState"].as_str() {
            if !g.is_empty() {
                it.meta.insert("goalState".into(), g.to_string());
            }
        }
        items.push(it);
        for r in reasons {
            // 只把"要人点头"这种**可以固化成策略**的理由单独计数
            if let Some(p) = r.split('「').nth(1).and_then(|s| s.split('」').next()) {
                if r.contains("confirmCommands") {
                    *why_count.entry(p.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    (items, why_count)
}

/// 读反馈（feedback:mine / feedback:@ns/slug）：跨 Agent、跨用户说过的话。
fn collect_feedback(cfg: &crate::config::CliConfig, s: &LearnSource, limit: i64, redact: &[String]) -> Vec<LearnItem> {
    let about = if s.r#where.is_empty() || s.r#where == "mine" {
        None
    } else {
        let (k, r) = crate::feedback::parse_about(&s.r#where);
        Some((k, r))
    };
    let kind_ref = about.clone();
    let rows = crate::feedback::fetch_for_learning(cfg, about.as_ref().map(|(k, r)| (k.as_str(), r.as_str())), limit);
    rows.iter()
        .map(|f| {
            let at = 0;
            let mut it = item(
                &s.id,
                f["kind"].as_str().unwrap_or("report"),
                f["aboutRef"].as_str().unwrap_or(""),
                at,
                &redact_text(f["body"].as_str().unwrap_or(""), redact),
            );
            it.meta.insert("feedbackId".into(), f["id"].as_str().unwrap_or("").to_string());
            it.meta.insert("aboutKind".into(), f["aboutKind"].as_str().unwrap_or("").to_string());
            if let Some(a) = f["agent"].as_str() {
                if !a.is_empty() {
                    it.meta.insert("agent".into(), a.to_string());
                }
            }
            if let Some(st) = f["status"].as_str() {
                it.meta.insert("status".into(), st.to_string());
            }
            if !kind_ref.is_none() {
                it.meta.insert("scope".into(), s.r#where.clone());
            }
            it
        })
        .collect()
}

/// 读记忆（mem:*）：`kind=preference` 的记忆是**人/Agent 写过的偏好**，最值得正式化。
fn collect_mem(cfg: &crate::config::CliConfig, s: &LearnSource, limit: i64, redact: &[String]) -> Result<Vec<LearnItem>> {
    let token = crate::config::token_opt(cfg);
    let d = crate::api::get(cfg, &format!("/api/mem?limit={}", limit.clamp(1, 500)), token.as_deref())?;
    let rows = d["memories"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .filter(|m| !m["expired"].as_bool().unwrap_or(false))
        .map(|m| {
            let key = m["key"].as_str().unwrap_or("");
            let body = m["value"].as_str().or_else(|| m["text"].as_str()).unwrap_or("");
            let mut it = item(
                &s.id,
                m["kind"].as_str().unwrap_or("fact"),
                key,
                0,
                &redact_text(body, redact),
            );
            it.meta.insert("subject".into(), m["subject"].as_str().unwrap_or("").to_string());
            if let Some(src) = m["source"].as_str() {
                if !src.is_empty() {
                    it.meta.insert("source".into(), src.to_string());
                }
            }
            it
        })
        .collect())
}

/// 读知识库（kb:*）：只取**引用级别**的摘要（slug / title / kind），不搬正文。
fn collect_kb(cfg: &crate::config::CliConfig, s: &LearnSource, limit: i64, redact: &[String]) -> Result<Vec<LearnItem>> {
    let token = crate::config::token_opt(cfg);
    let d = crate::api::get(cfg, &format!("/api/kb?size={}", limit.clamp(1, 200)), token.as_deref())?;
    let rows = d["docs"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .map(|k| {
            let slug = k["slug"].as_str().unwrap_or("");
            let title = k["title"].as_str().unwrap_or("");
            let mut it = item(
                &s.id,
                k["kind"].as_str().unwrap_or("doc"),
                slug,
                0,
                &redact_text(&format!("{title}（{slug}）"), redact),
            );
            if let Some(sm) = k["summary"].as_str() {
                if !sm.is_empty() {
                    it.meta.insert("summary".into(), redact_text(sm, redact));
                }
            }
            it
        })
        .collect())
}

/// 读检查点（ckpt:*）：只记"哪一次交接点"，不搬字节。
fn collect_ckpt(cfg: &crate::config::CliConfig, s: &LearnSource, limit: i64, redact: &[String]) -> Result<Vec<LearnItem>> {
    let token = crate::config::token_opt(cfg);
    let d = crate::api::get(cfg, &format!("/api/ckpt?limit={}", limit.clamp(1, 200)), token.as_deref())?;
    let rows = d["checkpoints"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .map(|c| {
            let id = c["id"].as_str().unwrap_or("");
            let name = c["name"].as_str().unwrap_or("");
            let mut it = item(&s.id, "ckpt", id, 0, &redact_text(name, redact));
            if let Some(r) = c["ref"].as_str() {
                if !r.is_empty() {
                    it.meta.insert("ref".into(), r.to_string());
                }
            }
            it
        })
        .collect())
}

/// 按同意声明读一遍（**未声明的来源不读**，并返回"跳过了什么"给人看）。
struct Collected {
    items: Vec<LearnItem>,
    /// 每个来源读到了几条（含读不到的原因）
    report: Vec<(String, String, i64)>,
    /// 明确跳过（没声明 / 目标不支持）
    skipped: Vec<String>,
}

fn collect_all(dir: &Path, c: &LearnConsent) -> Collected {
    let cfg = crate::config::load();
    let caps = target_caps(&cfg);
    let budget = c.max_items.max(1);
    let mut per = (budget / (c.sources.len().max(1) as i64)).max(1);
    let mut out = Collected { items: Vec::new(), report: Vec::new(), skipped: Vec::new() };
    // 来源声明都带 id（L1/L2…），现场补上（文件里可能手写漏了）。
    let mut sources = c.sources.clone();
    for (i, s) in sources.iter_mut().enumerate() {
        if s.id.trim().is_empty() {
            s.id = format!("L{}", i + 1);
        }
    }
    for s in &sources {
        let lim = if s.limit > 0 { s.limit.min(per) } else { per };
        per = per.max(1);
        match s.kind.as_str() {
            "log" => {
                let (items, _) = collect_ledger(dir, lim);
                out.report.push((s.id.clone(), "log:ledger".into(), items.len() as i64));
                out.items.extend(items);
            }
            "feedback" => {
                if !caps.is_empty() && !caps.iter().any(|x| x == "feedback") {
                    out.skipped.push(format!("{}（目标 {} 没声明 feedback 能力）", s.id, cfg.current_name()));
                    continue;
                }
                let items = collect_feedback(&cfg, s, lim, &c.redact);
                out.report.push((s.id.clone(), format!("feedback:{}", s.r#where), items.len() as i64));
                out.items.extend(items);
            }
            "mem" | "kb" | "ckpt" => {
                let need = s.kind.as_str();
                if !caps.is_empty() && !caps.iter().any(|x| x == need) {
                    out.skipped.push(format!("{}（目标 {} 没声明 {} 能力）", s.id, cfg.current_name(), need));
                    continue;
                }
                let r = match s.kind.as_str() {
                    "mem" => collect_mem(&cfg, s, lim, &c.redact),
                    "kb" => collect_kb(&cfg, s, lim, &c.redact),
                    _ => collect_ckpt(&cfg, s, lim, &c.redact),
                };
                match r {
                    Ok(items) => {
                        out.report.push((s.id.clone(), format!("{}:{}", s.kind, s.r#where), items.len() as i64));
                        out.items.extend(items);
                    }
                    Err(e) => {
                        out.report.push((s.id.clone(), format!("{}:{}", s.kind, s.r#where), -1));
                        out.skipped.push(format!("{}（{} 读不到：{e}）", s.id, s.kind));
                    }
                }
            }
            _ => out.skipped.push(format!("{}（不认识的来源种类）", s.id)),
        }
    }
    // 预算封顶（读多了不是好事：学习材料要能被复核）
    if out.items.len() as i64 > budget {
        out.items.truncate(budget as usize);
    }
    out
}

/// 一条提案（**不是行动**：它躺在文件里等人点头）。
#[derive(Serialize, Deserialize, Clone)]
struct Proposal {
    id: String,
    /// pref（写进偏好）/ guard（收紧策略，**不自动写**）/ lesson（记一条教训）
    kind: String,
    title: String,
    /// 建议写成的样子（pref 的人话 / guard 的策略片段）
    suggested: String,
    why: String,
    count: i32,
    evidence: Vec<String>,
    /// 落到哪儿
    target: String,
}

/// 读一遍材料 → 出提案（规则全部可解释：每条都说清"凭什么"）。
fn propose(items: &[LearnItem], why_count: &BTree<String, i32>, dir: &Path) -> Vec<Proposal> {
    let mut out: Vec<Proposal> = Vec::new();
    let mut idx = 0;
    let mut next_id = || {
        idx += 1;
        format!("LP-{idx}")
    };

    // ① 偏好正式化：`mem` 里 kind=preference 的记忆条目
    for it in items.iter().filter(|i| i.kind == "preference") {
        let n = out.len() + 1;
        out.push(Proposal {
            id: next_id(),
            kind: "pref".into(),
            title: format!("把记忆里的偏好正式化：{}", it.text.chars().take(40).collect::<String>()),
            suggested: it.text.clone(),
            why: format!(
                "记忆 {}{} 是 kind=preference（人/Agent 写过的偏好）—— 正式化成 avoid 偏好后，`rsi check` 就会拿它拦人",
                it.meta.get("subject").map(|s| format!("{s}/")).unwrap_or_default(),
                it.r#ref
            ),
            count: 1,
            evidence: vec![format!("mem:{}", it.r#ref)],
            target: "prefs.json".into(),
        });
        let _ = n;
    }

    // ② 策略收紧：账本里"要人点头"被反复要求 → 出提案（**但不自动改策略**）
    //
    // 分两种说法，因为它们的下一步不一样：
    //   · 还不在策略里 → 建议加进 confirmCommands（把它变成"要人确认"）
    //   · 已经在 confirmCommands 里、却还是被反复撞到 → 建议**升到 deny**，
    //     或者确认这条路确实需要每次点头（两种都是人的判断，工具只把事实摆出来）
    let pol = load_policy(dir).unwrap_or(Value::Null);
    let existing: Vec<String> = pol["rules"]["confirmCommands"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    for (pattern, n) in why_count.iter() {
        if *n < 2 {
            continue;
        }
        if existing.iter().any(|e| e == pattern) {
            out.push(Proposal {
                id: next_id(),
                kind: "guard".into(),
                title: format!("「{pattern}」被反复要求人确认（{n} 次）—— 要不要更严？"),
                suggested: format!("{{\"rules\": {{\"denyCommands\": [\"{pattern}\"]}}}}"),
                why: format!(
                    "它已经在 confirmCommands 里，但过去 {n} 次都被撞到 —— 要么把它升到 denyCommands（更严），\
                     要么确认这条路确实每次都要人点头。**两种都得你来定**"
                ),
                count: *n,
                evidence: vec![format!("log:ledger ×{n}")],
                target: "policy.json（手动）".into(),
            });
        } else {
            out.push(Proposal {
                id: next_id(),
                kind: "guard".into(),
                title: format!("把「{pattern}」固化成要人确认（账本里 {n} 次）"),
                suggested: format!("{{\"rules\": {{\"confirmCommands\": [\"{pattern}\"]}}}}"),
                why: format!(
                    "过去这条被要求过 {n} 次人点头 —— 与其每次现场判断，不如写进策略（**但要你自己写**：策略永不自动改）"
                ),
                count: *n,
                evidence: vec![format!("log:ledger ×{n}")],
                target: "policy.json（手动）".into(),
            });
        }
    }

    // ③ 教训：反馈里反复出现的问题 / 纠正
    let mut by_ref: BTree<String, Vec<&LearnItem>> = BTree::new();
    for it in items.iter().filter(|i| i.source.starts_with('L') && i.kind == "report") {
        by_ref.entry(it.r#ref.clone()).or_default().push(it);
    }
    for (aref, rows) in by_ref.iter() {
        if rows.len() < 2 {
            continue;
        }
        out.push(Proposal {
            id: next_id(),
            kind: "lesson".into(),
            title: format!("关于 {aref} 反复出问题（{} 条反馈）", rows.len()),
            suggested: format!(
                "用 {aref} 之前先过一遍这几条反馈：{}",
                rows.iter().map(|r| r.text.chars().take(30).collect::<String>()).collect::<Vec<_>>().join(" / ")
            ),
            why: "同一个东西被反复报告 —— 这不是噪音，是它真的不稳".into(),
            count: rows.len() as i32,
            evidence: rows.iter().map(|r| format!("feedback:{}", r.meta.get("feedbackId").cloned().unwrap_or_default())).collect(),
            target: "lessons.jsonl".into(),
        });
    }
    for it in items.iter().filter(|i| i.kind == "correction") {
        out.push(Proposal {
            id: next_id(),
            kind: "lesson".into(),
            title: format!("纠正：{}", it.text.chars().take(40).collect::<String>()),
            suggested: it.text.clone(),
            why: "有人明确说是「纠正」—— 那就是上一条说法不对，值得记下来".into(),
            count: 1,
            evidence: vec![format!("feedback:{}", it.meta.get("feedbackId").cloned().unwrap_or_default())],
            target: "lessons.jsonl".into(),
        });
    }

    // ④ 知识依据：kb 里 kind=spec 的文档 → 提醒按它做（只在被声明读过时才有）
    for it in items.iter().filter(|i| i.kind == "spec") {
        out.push(Proposal {
            id: next_id(),
            kind: "lesson".into(),
            title: format!("知识库里有一份规范：{}", it.text.chars().take(40).collect::<String>()),
            suggested: format!("动手前先看这份规范：{}", it.r#ref),
            why: "声明让我读知识库，读到一份 kind=spec —— 规范类的东西值得挂在教训里".into(),
            count: 1,
            evidence: vec![format!("kb:{}", it.r#ref)],
            target: "lessons.jsonl".into(),
        });
    }
    out
}

fn learn_cmd(a: &LearnArgs) -> Result<()> {
    match &a.action {
        LearnAction::Consent(x) => learn_consent(x),
        LearnAction::Plan(x) => learn_plan(x),
        LearnAction::Apply(x) => learn_apply(x),
        LearnAction::Digest(x) => learn_digest(x),
        LearnAction::Export(x) => learn_export(x),
    }
}

fn learn_consent(a: &LearnConsentArgs) -> Result<()> {
    match &a.action {
        LearnConsentAction::Show { json: j, dir } => {
            let d = rsi_dir(dir.as_deref());
            match load_consent(&d) {
                None => {
                    if *j {
                        println!("{}", serde_json::to_string_pretty(&json!({
                            "dir": d.to_string_lossy(), "exists": false, "enabled": false,
                            "note": "没有同意声明 = 什么都不学（默认关闭）"
                        }))?);
                    } else {
                        println!("还没有学习同意声明（{}）", learn_file(&d).display());
                        println!("   默认**什么都不学**：没有声明，就没得读");
                        println!("   打开：`ncc rsi learn consent set --source feedback:mine --source log:ledger` 然后 `ncc rsi learn consent on`");
                    }
                    Ok(())
                }
                Some(c) => {
                    if *j {
                        println!("{}", serde_json::to_string_pretty(&json!({
                            "dir": d.to_string_lossy(), "exists": true,
                            "enabled": c.enabled, "expired": consent_expired(&c),
                            "sources": c.sources, "redact": c.redact,
                            "maxItems": c.max_items, "expiresAt": c.expires_unix,
                            "setBy": {"user": c.set_by_user, "agent": c.set_by_agent, "at": c.set_at},
                        }))?);
                        return Ok(());
                    }
                    println!("学习同意声明 {}", learn_file(&d).display());
                    println!("   状态     {}", if c.enabled { "开" } else { "关" });
                    println!(
                        "   谁设的   {}{}  {}",
                        if c.set_by_user.is_empty() { "（没记）" } else { &c.set_by_user },
                        if c.set_by_agent.is_empty() { String::new() } else { format!("（代设 Agent：{}）", c.set_by_agent) },
                        if c.set_at > 0 { fmt_epoch(c.set_at as i64) } else { String::new() }
                    );
                    println!("   读多少   ≤ {} 条", c.max_items);
                    if c.expires_unix > 0 {
                        println!(
                            "   什么时候过期 {}（现在{}）",
                            fmt_epoch(c.expires_unix as i64),
                            if consent_expired(&c) { "**已经过期**" } else { "还没过期" }
                        );
                    }
                    if c.sources.is_empty() {
                        println!("   来源     （一条也没有 —— 就算开着也学不到东西）");
                    } else {
                        println!("   来源     {} 条", c.sources.len());
                        for s in &c.sources {
                            println!(
                                "     {}  {:<9} {}{}",
                                s.id,
                                s.kind,
                                s.r#where,
                                if s.limit > 0 { format!("（≤{}）", s.limit) } else { String::new() }
                            );
                        }
                    }
                    if !c.redact.is_empty() {
                        println!("   脱敏     {}", c.redact.join(" · "));
                    }
                    println!("   ⚠️ 学习**只读**来源；写只写 .ncc-rsi/ 里，而且提案要你点头才生效。");
                    Ok(())
                }
            }
        }
        LearnConsentAction::On { dir } => {
            let d = rsi_dir(dir.as_deref());
            let mut c = load_consent(&d).unwrap_or_default();
            c.enabled = true;
            c.set_by_user = whoami();
            c.set_by_agent = std::env::var("NCC_AGENT").unwrap_or_default();
            c.set_at = now_unix();
            save_consent(&d, &c)?;
            println!("✅ 学习已打开（{}）", learn_file(&d).display());
            if c.sources.is_empty() {
                println!("   ⚠️ 但一条来源都没声明 —— 声明即许可，什么都没声明就什么都读不到。");
                println!("   例：`ncc rsi learn consent set --source feedback:mine --source log:ledger --source mem:@me`");
            } else {
                println!("   会读 {} 条来源（`ncc rsi learn consent show` 看清单）", c.sources.len());
            }
            Ok(())
        }
        LearnConsentAction::Off { dir } => {
            let d = rsi_dir(dir.as_deref());
            let mut c = load_consent(&d).unwrap_or_default();
            c.enabled = false;
            c.set_by_user = whoami();
            c.set_at = now_unix();
            save_consent(&d, &c)?;
            println!("⏸  学习已关闭（声明留着：来源清单还在，下次 `on` 不用重配）");
            Ok(())
        }
        LearnConsentAction::Set { sources, replace, redact, max_items, expires_in_days, by_agent, dir } => {
            let d = rsi_dir(dir.as_deref());
            if sources.is_empty() && !*replace && redact.is_empty() {
                bail!("什么都没改：要给来源（`--source feedback:mine`）或 `--replace` 清空");
            }
            let mut c = load_consent(&d).unwrap_or_default();
            if *replace {
                c.sources.clear();
            }
            for raw in sources {
                let mut s = parse_source(raw)?;
                // 给个稳定 id：L1/L2…（同名来源替换，不重复堆）
                if let Some(pos) = c.sources.iter().position(|x| x.kind == s.kind && x.r#where == s.r#where) {
                    s.id = c.sources[pos].id.clone();
                    c.sources[pos] = s;
                } else {
                    s.id = format!("L{}", c.sources.len() + 1);
                    c.sources.push(s);
                }
            }
            for w in redact {
                if !c.redact.contains(w) {
                    c.redact.push(w.clone());
                }
            }
            c.max_items = (*max_items).max(1);
            if *expires_in_days > 0 {
                c.expires_unix = now_unix() + (*expires_in_days as u64) * 86400;
            }
            c.set_by_user = whoami();
            c.set_by_agent = if by_agent.trim().is_empty() {
                std::env::var("NCC_AGENT").unwrap_or_default()
            } else {
                by_agent.clone()
            };
            c.set_at = now_unix();
            save_consent(&d, &c)?;
            println!("📝 同意声明已更新（{} 条来源，≤{} 条/次）", c.sources.len(), c.max_items);
            for s in &c.sources {
                println!("   {}  {}:{}", s.id, s.kind, s.r#where);
            }
            if !c.enabled {
                println!("   （现在还是关着的：`ncc rsi learn consent on` 才开）");
            }
            Ok(())
        }
    }
}

fn learn_plan(a: &LearnPlanArgs) -> Result<()> {
    let d = rsi_dir(a.dir.as_deref());
    let c = require_consent(&d)?;
    let got = collect_all(&d, &c);
    let (_, why_count) = collect_ledger(&d, c.max_items.max(1));
    let props = propose(&got.items, &why_count, &d);
    write_atomic(
        &d.join(PROPOSALS),
        format!("{}\n", serde_json::to_string_pretty(&json!({
            "at": now_unix(), "sources": c.sources, "items": got.items.len(), "proposals": props
        }))?).as_bytes(),
        0o600,
    )?;
    ledger_append(&d, &json!({
        "at": now_unix(), "act": "learn-plan", "by": whoami(),
        "items": got.items.len(), "proposals": props.len()
    }))?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "dir": d.to_string_lossy(), "items": got.items.len(),
            "read": got.report.iter().map(|(id, w, n)| json!({"source": id, "where": w, "items": n})).collect::<Vec<_>>(),
            "skipped": got.skipped, "proposals": props,
            "note": "这些是**提案**，不是行动：`ncc rsi learn apply <id>` 才生效；策略类永远要你自己改"
        }))?);
        return Ok(());
    }
    println!("📖 读完了（{} 条材料 → {} 条提案）", got.items.len(), props.len());
    for (id, w, n) in &got.report {
        println!("   {id}  {w}  {}", if *n < 0 { "读不到".to_string() } else { format!("{n} 条") });
    }
    for s in &got.skipped {
        println!("   ⏭  跳过 {s}");
    }
    if props.is_empty() {
        println!("   （没有可提的 —— 材料太少或都不够反复，这是好事：**不硬凑提案**）");
        return Ok(());
    }
    println!("\n提案（都要你点头才生效）：");
    for p in &props {
        println!(
            "   {}  [{}] {}（依据 {} 次）",
            p.id, p.kind, p.title, p.count
        );
        println!("        → {}", p.suggested);
        println!("        为什么：{}", p.why);
        println!("        依据：{}   落到：{}", p.evidence.join(" · "), p.target);
    }
    println!("\n应用：`ncc rsi learn apply {}`（或一条条挑）", props[0].id);
    if props.iter().any(|p| p.kind == "guard") {
        println!("⚠️ 其中 guard 类**不会自动写策略** —— 那等于让工具自己给自己松绑。");
    }
    Ok(())
}

fn learn_apply(a: &LearnApplyArgs) -> Result<()> {
    let d = rsi_dir(a.dir.as_deref());
    let raw = fs::read(d.join(PROPOSALS)).map_err(|_| {
        anyhow!("还没有提案（先 `ncc rsi learn plan`）—— apply 只能应用已经出过的东西")
    })?;
    let v: Value = serde_json::from_slice(&raw)?;
    let all: Vec<Proposal> = serde_json::from_value(v["proposals"].clone()).unwrap_or_default();
    let want_all = a.id.trim() == "all";
    let picked: Vec<&Proposal> = if want_all {
        all.iter().collect()
    } else {
        all.iter().filter(|p| p.id == a.id.trim()).collect()
    };
    if picked.is_empty() {
        bail!(
            "没有这条提案（{}）。现有：{}",
            a.id,
            all.iter().map(|p| p.id.clone()).collect::<Vec<_>>().join(" · ")
        );
    }
    let mut applied: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut prefs = load_prefs(&d);
    for p in &picked {
        match p.kind.as_str() {
            "pref" => {
                if prefs.prefs.iter().any(|x| x.statement == p.suggested) {
                    skipped.push(format!("{}（偏好里已经有了）", p.id));
                    continue;
                }
                let id = format!("P-{}", prefs.prefs.len() + 1);
                prefs.prefs.push(Pref {
                    id,
                    statement: p.suggested.clone(),
                    scope: String::new(),
                    kind: "avoid".into(),
                    evidence: p.evidence.clone(),
                    confidence: "inferred".into(),
                    hits: 0,
                    created_unix: now_unix(),
                });
                applied.push(format!("{} → 偏好（inferred，会拦人；复核后可改）", p.id));
            }
            "lesson" => {
                let line = json!({
                    "at": now_unix(), "id": p.id, "statement": p.suggested,
                    "why": p.why, "evidence": p.evidence, "source": "learn",
                    "by": whoami(),
                });
                let mut f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(d.join(LESSONS))
                    .with_context(|| format!("打不开 {}", d.join(LESSONS).display()))?;
                writeln!(f, "{}", serde_json::to_string(&line)?)?;
                applied.push(format!("{} → 教训（{LESSONS}）", p.id));
            }
            // 策略永不自动改：给一段能粘贴的片段，剩下的交给人。
            _ => skipped.push(format!(
                "{}（**策略不自动改**）：把这段并进 policy.json 再 `ncc rsi policy check`\n        {}",
                p.id, p.suggested
            )),
        }
    }
    if !applied.is_empty() {
        // 只在真的有 pref 变动时存（免得空写一次把文件时间戳刷了）
        if applied.iter().any(|l| l.contains("偏好")) {
            save_prefs(&d, &prefs)?;
        }
        ledger_append(&d, &json!({
            "at": now_unix(), "act": "learn-apply", "by": whoami(),
            "applied": applied, "skipped": skipped
        }))?;
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "applied": applied, "skipped": skipped,
            "note": "策略类永远不自动写；偏好类标 inferred，复核后再让它拦人"
        }))?);
        return Ok(());
    }
    for l in &applied {
        println!("✅ 应用了 {l}");
    }
    for l in &skipped {
        println!("⏭  跳过 {l}");
    }
    if applied.is_empty() && skipped.is_empty() {
        println!("（什么都没做）");
    }
    Ok(())
}

/// **给 Agent 面用的学习摘要**（`ncc mcp` 的 `ncc_rsi_learn_digest`）。
///
/// 只读：同意声明 + 待点头的提案数 + 最近的教训。
/// （**没有**"让 Agent 自己 apply 提案"的工具 —— 学习产物要人点头，这是红线。）
pub fn learn_digest_json(dir: Option<&Path>) -> Result<Value> {
    let d = rsi_dir(dir);
    let consent = load_consent(&d);
    let lessons: Vec<Value> = fs::read_to_string(d.join(LESSONS))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let props: Vec<Value> = fs::read_to_string(d.join(PROPOSALS))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v["proposals"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    Ok(json!({
        "dir": d.to_string_lossy(),
        "enabled": consent.as_ref().map(|c| c.enabled).unwrap_or(false),
        "sources": consent.as_ref().map(|c| c.sources.iter().map(|s| format!("{}:{}", s.kind, s.r#where)).collect::<Vec<_>>()).unwrap_or_default(),
        "expired": consent.as_ref().map(consent_expired).unwrap_or(false),
        "proposals": props.iter().map(|p| json!({"id": p["id"], "kind": p["kind"], "title": p["title"]})).collect::<Vec<_>>(),
        "lessons": lessons.iter().rev().take(5).cloned().collect::<Vec<_>>(),
        "note": "默认什么都不学；提案要人点头才生效（apply 不在工具面里），策略永不自动改",
    }))
}

fn learn_digest(a: &LearnDigestArgs) -> Result<()> {
    let d = rsi_dir(a.dir.as_deref());
    let consent = load_consent(&d);
    let lessons: Vec<Value> = fs::read_to_string(d.join(LESSONS))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let props: Vec<Value> = fs::read_to_string(d.join(PROPOSALS))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v["proposals"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "dir": d.to_string_lossy(),
            "enabled": consent.as_ref().map(|c| c.enabled).unwrap_or(false),
            "sources": consent.as_ref().map(|c| c.sources.clone()).unwrap_or_default(),
            "expired": consent.as_ref().map(consent_expired).unwrap_or(false),
            "proposals": props.len(),
            "lessons": lessons.len(),
            "recentLessons": lessons.iter().rev().take(5).cloned().collect::<Vec<_>>(),
            "note": "只读摘要：学习的产物（提案 / 教训）都在 .ncc-rsi/ 里，且都要人点过头才存在"
        }))?);
        return Ok(());
    }
    println!("🧠 学习状态（{}）", d.display());
    match &consent {
        None => println!("   同意声明：没有 —— 默认什么都不学"),
        Some(c) => println!(
            "   同意声明：{}（{} 条来源{}）",
            if c.enabled { "开着" } else { "关着" },
            c.sources.len(),
            if consent_expired(c) { "，**已过期**" } else { "" }
        ),
    }
    println!("   待你点头的提案：{} 条", props.len());
    println!("   已经记下的教训：{} 条", lessons.len());
    for l in lessons.iter().rev().take(3) {
        println!(
            "     · [{}] {}",
            l["id"].as_str().unwrap_or(""),
            l["statement"].as_str().unwrap_or("").chars().take(60).collect::<String>()
        );
    }
    if props.is_empty() && lessons.is_empty() {
        println!("   （还什么都没有：`ncc rsi learn plan` 读一遍已授权的来源）");
    }
    Ok(())
}

fn learn_export(a: &LearnExportArgs) -> Result<()> {
    let d = rsi_dir(a.home.as_deref());
    let c = require_consent(&d)?;
    let got = collect_all(&d, &c);
    let out_dir = PathBuf::from(a.dir.trim());
    let manifest = json!({
        "spec": "ncc-learn-set/v1",
        "at": now_unix(),
        "by": whoami(),
        "agent": std::env::var("NCC_AGENT").unwrap_or_default(),
        "about": "RSI 的学习数据集：从**已授权的**来源读来的引用级摘要（不是原文搬运）",
        "consent": {
            "enabled": c.enabled,
            "sources": c.sources,
            "maxItems": c.max_items,
            "expiresAt": c.expires_unix,
            "setByUser": c.set_by_user,
            "setByAgent": c.set_by_agent,
        },
        "redact": c.redact,
        "counts": got.report.iter().map(|(id, w, n)| json!({"source": id, "where": w, "items": n})).collect::<Vec<_>>(),
        "skipped": got.skipped,
        "items": got.items.len(),
        "note": "这份目录**包含被授权读到的内容摘要**：分享前先自己看一眼（`--dry` 只看不写）",
    });
    if a.dry {
        println!("（--dry）会导出 {} 条到 {}", got.items.len(), out_dir.display());
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }
    fs::create_dir_all(&out_dir).with_context(|| format!("建不了目录 {}", out_dir.display()))?;
    write_atomic(&out_dir.join("manifest.json"), format!("{}\n", serde_json::to_string_pretty(&manifest)?).as_bytes(), 0o600)?;
    let mut lines = String::new();
    for it in &got.items {
        lines.push_str(&serde_json::to_string(it)?);
        lines.push('\n');
    }
    write_atomic(&out_dir.join("items.jsonl"), lines.as_bytes(), 0o600)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "dir": out_dir.to_string_lossy(), "items": got.items.len(), "manifest": manifest
        }))?);
        return Ok(());
    }
    println!("📦 导出 {} 条学习材料 → {}", got.items.len(), out_dir.display());
    println!("   manifest.json（来源 / 时刻 / 同意 / 脱敏）+ items.jsonl（引用级摘要）");
    println!("   注意：目录里是**被授权读到的内容**，分享前自己看一眼");
    Ok(())
}

fn whoami() -> String {
    format!("{}@{}", std::env::var("USER").unwrap_or_else(|_| "nobody".into()), crate::connssh::hostname())
}
