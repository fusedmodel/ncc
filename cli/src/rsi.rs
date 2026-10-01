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
use serde_json::{json, Value};
use std::fs;
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

fn whoami() -> String {
    format!("{}@{}", std::env::var("USER").unwrap_or_else(|_| "nobody".into()), crate::connssh::hostname())
}
