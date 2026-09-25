//! 互操作层：把 hur 包**声明式**翻译成别的 Agent 宿主的原生配置，并支持把包
//! 当成可分发、可同步的工程依赖（`hur.project.json` + `hur sync`）。
//!
//! 两条边界（与 PRD §11 一致）：
//! 1. **只读声明，不执行代码**：翻译只用 `hur.json` 的 `agent{}` 与 `skills/*`，
//!    不加载、不运行包内 JS/TS。所以任何宿主里跑的都是「同一份被声明的行为」。
//! 2. **产物可核对**：`hur export` 默认是**预览**（打印将写什么），`--write` 才落盘；
//!    追加型产物（`AGENTS.md`）用标记块包裹，重复执行结果不变。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::pack;
use crate::registry::Registry;
use crate::spec::{self, HurPackage, MANIFEST};

pub const PROJECT_SPEC: &str = "harness-use-project/v1";
pub const PROJECT_LOCK_SPEC: &str = "harness-use-project-lock/v1";
pub const PROJECT_FILE: &str = "hur.project.json";
pub const PROJECT_DIR: &str = ".hur";
pub const PROJECT_LOCK: &str = "lock.json";

/// 支持的宿主目标（`codex` = 通用 `AGENTS.md`）
pub const TARGETS: [&str; 5] = ["claude", "cursor", "cline", "codex", "mcp"];

pub fn valid_target(t: &str) -> bool {
    TARGETS.contains(&t)
}

pub fn parse_targets(raw: &str) -> Result<Vec<String>> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "all" {
        return Ok(TARGETS.iter().map(|s| s.to_string()).collect());
    }
    let mut out = Vec::new();
    for t in raw.split([',', '，', ' ']).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !valid_target(t) {
            bail!("未知目标「{t}」（可选：{}）", TARGETS.join(" / "));
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    Ok(out)
}

/* ---------------- 产物模型 ---------------- */

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// 整文件覆盖
    Write,
    /// 用标记块追加/替换（`AGENTS.md` 这类共享文件）
    Marked,
    /// 深合并进已有 JSON（`.mcp.json`）
    MergeJson,
}

#[derive(Debug, Clone)]
pub struct Artifact {
    pub target: String,
    /// 相对宿主工程根的路径
    pub path: String,
    pub content: String,
    pub mode: Mode,
    /// 合并用的键路径（仅 `MergeJson`）：如 `["mcpServers", "hur"]`
    pub merge_key: Vec<String>,
}

impl Artifact {
    pub fn write(target: &str, path: impl Into<String>, content: impl Into<String>) -> Self {
        Self { target: target.into(), path: path.into(), content: content.into(), mode: Mode::Write, merge_key: vec![] }
    }
    pub fn marked(target: &str, path: impl Into<String>, content: impl Into<String>) -> Self {
        Self { target: target.into(), path: path.into(), content: content.into(), mode: Mode::Marked, merge_key: vec![] }
    }
    pub fn merge_json(target: &str, path: impl Into<String>, key: Vec<String>, content: impl Into<String>) -> Self {
        Self { target: target.into(), path: path.into(), content: content.into(), mode: Mode::MergeJson, merge_key: key }
    }
    pub fn mode_label(&self) -> &'static str {
        match self.mode {
            Mode::Write => "写入",
            Mode::Marked => "标记块",
            Mode::MergeJson => "合并",
        }
    }
}

/* ---------------- 渲染 ---------------- */

/// 从包内容里取出 `agent{}` 声明；非 agent / 无声明时返回 None（调用方回落 summary）
pub fn agent_of(pkg: &HurPackage) -> Option<&spec::AgentSpec> {
    if pkg.kind != "agent" {
        return None;
    }
    let a = pkg.agent.as_ref()?;
    if a.is_empty() {
        None
    } else {
        Some(a)
    }
}

/// 文件名用 slug（ASCII，中文名回落为 id 派生）
fn file_slug(name: &str, fallback_id: &str) -> String {
    let s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() {
        let tail: String = fallback_id
            .chars()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("hur-{}", if tail.is_empty() { "agent".to_string() } else { tail })
    } else {
        s
    }
}

fn first_line(s: &str) -> String {
    s.trim().lines().next().unwrap_or("").trim().to_string()
}

/// YAML 单行标量（frontmatter 用）：截断 + 双引号转义，避免冒号/引号把文件写坏
fn yaml_line(s: &str) -> String {
    let one = first_line(s).replace(['"', '\\'], " ");
    let cut: String = one.chars().take(120).collect();
    format!("\"{}\"", cut.trim())
}

/// 一句话描述（宿主 UI 里显示的那行）
fn description_of(pkg: &HurPackage) -> String {
    let s = pkg.summary.trim();
    if !s.is_empty() {
        return first_line(s);
    }
    if let Some(a) = agent_of(pkg) {
        let p = first_line(&a.system_prompt);
        if !p.is_empty() {
            return p;
        }
    }
    format!("{} · hur 包（{}）", pkg.name, pkg.id)
}

/// 描述（frontmatter 里用的形态：短、YAML 安全）
fn description_yaml(pkg: &HurPackage) -> String {
    yaml_line(&description_of(pkg))
}

/// 宿主里要用的名字（英文/ID 安全）
pub fn host_name(pkg: &HurPackage) -> String {
    let base = file_slug(&pkg.name, &pkg.id);
    format!("hur-{}", base)
}

/// 技能清单：从 `agent.skills` 与包内 `skills/*` 收集（相对路径 → 正文）
pub fn skills_of(pkg: &HurPackage, files: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |path: &str, body: &str| {
        if out.iter().any(|(p, _)| p == path) {
            return;
        }
        out.push((path.to_string(), body.to_string()));
    };
    let declared = agent_of(pkg).map(|a| a.skills.clone()).unwrap_or_default();
    for rel in declared {
        let rel = rel.trim();
        if rel.is_empty() {
            continue;
        }
        if let Some((p, c)) = files.iter().find(|(p, _)| p == rel) {
            push(p, c);
        }
    }
    // 未声明但包内有的 skills/* 也带上（对宿主更有用）
    for (p, c) in files {
        if p.starts_with("skills/") && (p.ends_with(".md") || p.ends_with(".txt")) {
            push(p, c);
        }
    }
    out
}

/// 技能在宿主里的短名（`skills/front-desk.md` → `front-desk`）
pub fn skill_slug(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = base.rsplit_once('.').map(|(a, _)| a).unwrap_or(base);
    file_slug(stem, stem)
}

struct BodyCtx<'a> {
    pkg: &'a HurPackage,
    agent: Option<&'a spec::AgentSpec>,
    skills: Vec<(String, String)>,
    tools: Vec<String>,
    max_chars: u32,
}

fn ctx_of<'a>(pkg: &'a HurPackage, files: &[(String, String)]) -> BodyCtx<'a> {
    let agent = agent_of(pkg);
    let skills = skills_of(pkg, files);
    let tools = agent
        .map(|a| a.tools.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default();
    let max_chars = agent
        .and_then(|a| a.guard.as_ref())
        .and_then(|g| g.get("max_reply_chars"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    BodyCtx { pkg, agent, skills, tools, max_chars }
}

/// 宿主正文：声明 → 人设 → 技能 → 工具与边界（与桌面端装配同一语义）
fn body_of(c: &BodyCtx) -> String {
    let mut out = String::new();
    let prompt = c
        .agent
        .map(|a| a.system_prompt.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("你是「{}」，{}", c.pkg.name, description_of(c.pkg)));
    out.push_str(prompt.trim());
    out.push('\n');

    if let Some(a) = c.agent {
        if !a.persona.trim().is_empty() {
            out.push_str(&format!("\n## 人设要求\n\n{}\n", a.persona.trim()));
        }
    }

    if !c.skills.is_empty() {
        out.push_str("\n## 技能（随包分发，按需查阅）\n");
        for (path, content) in &c.skills {
            out.push_str(&format!("\n### {} \n\n```markdown\n{}\n```\n", path, content.trim_end()));
        }
    }

    out.push_str("\n## 可用手段与边界\n\n");
    if c.tools.is_empty() {
        out.push_str("- 本包**未声明**可调用的本机工具：只依据已有上下文回答，不要臆造调用。\n");
    } else {
        out.push_str(&format!(
            "- 允许的工具（由宿主执行；本包只声明，不越界）：{}\n",
            c.tools.join("、")
        ));
        out.push_str("- 需要外部数据时，优先用 `harness.call` 直连对方 endpoint；**不要在本地拼造返回**。\n");
    }
    if c.max_chars > 0 {
        out.push_str(&format!("- 输出上限 {} 字；超了先删解释、保留结论。\n", c.max_chars));
    }
    let net = &c.pkg.permissions.network;
    if net.is_empty() {
        out.push_str("- 不声明任何外呼域名：**不要联网**取数据。\n");
    } else {
        out.push_str(&format!("- 允许外呼的域名：{}（超出即越权）。\n", net.join("、")));
    }
    out.push_str(&format!(
        "- 来源：hur 包 `{}` v{}（`{}`）；本文件由 `hur export` 生成，**改包不改本文件**。\n",
        c.pkg.id,
        c.pkg.version,
        spec::PKG_SPEC
    ));
    out
}

/// 「怎么再往前一步」——把自己装到别的 Agent 里的可复制提示词（人和 Agent 都能读）
pub fn install_hint(pkg: &HurPackage, target: &str) -> String {
    let ref_ = if pkg.publish.slug.trim().is_empty() {
        format!("./dist/{}-{}.hur", pkg.id, pkg.version)
    } else if pkg.publish.slug.contains('/') {
        format!("@{}", pkg.publish.slug)
    } else {
        pkg.publish.slug.clone()
    };
    match target {
        "mcp" => format!(
            "任何支持 MCP 的宿主：把 `{{\"mcpServers\":{{\"hur\":{{\"command\":\"hur\",\"args\":[\"mcp\"]}}}}}}` 写进宿主配置，\
             之后可用工具 `hur_install_agent` / `hur_export_agent` 让宿主自己装包。包引用：`{ref_}`"
        ),
        _ => format!(
            "命令行安装到本机并生成宿主配置：`hur install {ref_}` → `hur export --package {} --target {} --write`",
            pkg.id,
            if valid_target(target) { target } else { "all" }
        ),
    }
}

/// 渲染单个目标（纯函数，可单测）
pub fn render_target(pkg: &HurPackage, files: &[(String, String)], target: &str) -> Result<Vec<Artifact>> {
    if !valid_target(target) {
        bail!("未知目标「{target}」（可选：{}）", TARGETS.join(" / "));
    }
    let c = ctx_of(pkg, files);
    let slug = host_name(pkg);
    let desc = description_yaml(pkg);
    let body = body_of(&c);
    let mut out: Vec<Artifact> = Vec::new();
    let notice = format!(
        "> 本文件由 `hur export` 从 hur 包 `{}` v{} 生成（声明式，未执行包内代码）。改行为请改包再导出。\n",
        pkg.id, pkg.version
    );

    match target {
        // Claude Code：子 Agent 定义 + Agent Skills
        "claude" => {
            out.push(Artifact::write(
                "claude",
                format!(".claude/agents/{slug}.md"),
                format!(
                    "---\nname: {slug}\ndescription: {desc}\n---\n\n{notice}\n{body}"
                ),
            ));
            for (path, content) in &c.skills {
                let s = skill_slug(path);
                out.push(Artifact::write(
                    "claude",
                    format!(".claude/skills/{s}/SKILL.md"),
                    format!(
                        "---\nname: {s}\ndescription: {}\n---\n\n{}\n",
                        yaml_line(content),
                        content.trim_end()
                    ),
                ));
            }
        }
        // Cursor：规则文件（.mdc）
        "cursor" => {
            out.push(Artifact::write(
                "cursor",
                format!(".cursor/rules/{slug}.mdc"),
                format!("---\ndescription: {desc}\nalwaysApply: false\n---\n\n{notice}\n{body}"),
            ));
        }
        // Cline：.clinerules 目录下的规则文件
        "cline" => {
            out.push(Artifact::write(
                "cline",
                format!(".clinerules/{slug}.md"),
                format!("# {}\n\n{notice}\n{body}", pkg.name),
            ));
        }
        // 通用（Codex CLI / AGENTS.md 生态）：追加标记块，避免覆盖别人写的内容
        "codex" => {
            let block = format!(
                "<!-- hur:begin {id} -->\n## {name}（hur 包 `{id}` v{version}）\n\n{notice}\n{body}\n<!-- hur:end {id} -->\n",
                id = pkg.id,
                name = pkg.name,
                version = pkg.version
            );
            out.push(Artifact::marked("codex", "AGENTS.md", block));
        }
        // MCP：让宿主通过 `hur mcp` 拿到全部 hur 能力（含装包/导出）
        "mcp" => {
            let server = serde_json::json!({
                "command": "hur",
                "args": ["mcp"],
                "env": { "HUR_MCP_AGENT": pkg.id }
            });
            out.push(Artifact::merge_json(
                "mcp",
                ".mcp.json",
                vec!["mcpServers".into(), "hur".into()],
                format!("{}\n", serde_json::to_string_pretty(&server)?),
            ));
        }
        _ => unreachable!(),
    }
    Ok(out)
}

/// 渲染多个目标（`all` = 五个全出）
pub fn render(pkg: &HurPackage, files: &[(String, String)], targets: &[String]) -> Result<Vec<Artifact>> {
    let mut out = Vec::new();
    for t in targets {
        out.extend(render_target(pkg, files, t)?);
    }
    Ok(out)
}

/* ---------------- 落盘（幂等） ---------------- */

const MARK_BEGIN: &str = "<!-- hur:begin ";
const MARK_END: &str = "<!-- hur:end ";

fn marked_id(content: &str) -> Option<&str> {
    let start = content.find(MARK_BEGIN)? + MARK_BEGIN.len();
    let rest = &content[start..];
    let end = rest.find(" -->")?;
    Some(&rest[..end])
}

fn upsert_marked(existing: &str, block: &str) -> String {
    let Some(id) = marked_id(block) else {
        return format!("{}{}", existing.trim_end(), block);
    };
    let begin = format!("{MARK_BEGIN}{id} -->");
    let end = format!("{MARK_END}{id} -->");
    if let (Some(b), Some(e)) = (existing.find(&begin), existing.find(&end)) {
        let mut out = String::new();
        out.push_str(&existing[..b]);
        out.push_str(block.trim_end());
        out.push_str(&existing[e + end.len()..]);
        return out;
    }
    let sep = if existing.trim().is_empty() { "" } else { "\n\n" };
    format!("{}{sep}{}\n", existing.trim_end(), block.trim_end())
}

fn deep_merge(dst: &mut serde_json::Value, path: &[String], src: &serde_json::Value) {
    if path.is_empty() {
        match (dst, src) {
            (serde_json::Value::Object(d), serde_json::Value::Object(s)) => {
                for (k, v) in s {
                    deep_merge(d.get_mut(k).unwrap_or(&mut serde_json::Value::Null), &[], v);
                }
            }
            (d, s) => *d = s.clone(),
        }
        return;
    }
    if !dst.is_object() {
        *dst = serde_json::json!({});
    }
    let obj = dst.as_object_mut().unwrap();
    let key = &path[0];
    let child = obj.entry(key.clone()).or_insert(serde_json::Value::Null);
    deep_merge(child, &path[1..], src);
}

/// 把产物写进宿主工程根（`Marked` 幂等替换、`MergeJson` 深合并）
pub fn apply(root: &Path, artifacts: &[Artifact]) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for a in artifacts {
        let dest = root.join(&a.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("创建 {} 失败", parent.display()))?;
        }
        let final_text = match a.mode {
            Mode::Write => a.content.clone(),
            Mode::Marked => {
                let existing = std::fs::read_to_string(&dest).unwrap_or_default();
                upsert_marked(&existing, &a.content)
            }
            Mode::MergeJson => {
                let mut doc: serde_json::Value = std::fs::read_to_string(&dest)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                let patch: serde_json::Value = serde_json::from_str(&a.content)?;
                deep_merge(&mut doc, &a.merge_key, &patch);
                format!("{}\n", serde_json::to_string_pretty(&doc)?)
            }
        };
        std::fs::write(&dest, final_text).with_context(|| format!("写入 {} 失败", dest.display()))?;
        written.push(dest);
    }
    Ok(written)
}

/// 导出预览（人读）：每个产物一行 + 头部信息
pub fn render_plan(pkg: &HurPackage, artifacts: &[Artifact], out_dir: &str) -> String {
    let mut hosts: Vec<String> = Vec::new();
    for a in artifacts {
        if !hosts.contains(&a.target) {
            hosts.push(a.target.clone());
        }
    }
    let mut s = String::new();
    s.push_str(&format!(
        "{} v{}  →  {} 个文件（目标：{}）\n",
        pkg.name,
        pkg.version,
        artifacts.len(),
        hosts.join("+")
    ));
    s.push_str(&format!("落点：{}\n", out_dir));
    for a in artifacts {
        s.push_str(&format!("  [{}] {}  ({})\n", a.target, a.path, a.mode_label()));
    }
    s.push_str(&format!("\n提示：{}\n", install_hint(pkg, artifacts.first().map(|a| a.target.as_str()).unwrap_or("all"))));
    s
}

/* ---------------- 工程（`hur.project.json`） ---------------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectAgent {
    /// `@ns/slug`（registry）· `./x.hur`（产物）· `/path/to/pkg`（目录）
    pub r#ref: String,
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub sha256: String,
    /// 该包只导出到这些目标（空 = 用工程默认）
    #[serde(default)]
    pub targets: Vec<String>,
    /// 同步时允许覆盖（默认在已有目录上要求 --force）
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub spec: String,
    #[serde(default)]
    pub name: String,
    /// 默认导出目标
    #[serde(default)]
    pub targets: Vec<String>,
    /// 导出落点（相对工程根，默认 `.`）
    #[serde(default)]
    pub out: String,
    #[serde(default)]
    pub agents: Vec<ProjectAgent>,
}

impl Default for Project {
    fn default() -> Self {
        Self { spec: PROJECT_SPEC.to_string(), name: String::new(), targets: Vec::new(), out: String::new(), agents: Vec::new() }
    }
}

pub fn find_project(start: &Path) -> Result<PathBuf> {
    let mut cur = if start.as_os_str().is_empty() { PathBuf::from(".") } else { start.to_path_buf() };
    if cur.is_file() {
        cur = cur.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    }
    let abs = std::fs::canonicalize(&cur).unwrap_or(cur.clone());
    let mut dir = abs.as_path();
    loop {
        let p = dir.join(PROJECT_FILE);
        if p.is_file() {
            return Ok(p);
        }
        match dir.parent() {
            Some(up) => dir = up,
            None => bail!("向上找不到 {PROJECT_FILE}（用 `hur add <ref>` 初始化一个工程）"),
        }
    }
}

pub fn read_project(path: &Path) -> Result<Project> {
    let text = std::fs::read_to_string(path).with_context(|| format!("读 {} 失败", path.display()))?;
    let mut p: Project = serde_json::from_str(&text).with_context(|| format!("{} 解析失败", path.display()))?;
    if p.spec.trim().is_empty() {
        p.spec = PROJECT_SPEC.to_string();
    }
    if p.spec != PROJECT_SPEC {
        bail!("{PROJECT_FILE} 的 spec 必须是 {PROJECT_SPEC}，当前是「{}」", p.spec);
    }
    Ok(p)
}

pub fn write_project(path: &Path, p: &Project) -> Result<()> {
    let text = format!("{}\n", serde_json::to_string_pretty(p)?);
    std::fs::write(path, text).with_context(|| format!("写 {} 失败", path.display()))?;
    Ok(())
}

/// 加一条依赖（同 ref 覆盖）
pub fn add_agent(p: &mut Project, a: ProjectAgent) {
    p.agents.retain(|x| x.r#ref != a.r#ref);
    p.agents.push(a);
}

/* ---------------- 同步 ---------------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    /// registry | archive | dir
    pub source: String,
    /// 来源引用 / 路径
    pub r#ref: String,
    /// 产物 sha256（archive/registry 有；dir 为空）
    #[serde(default)]
    pub sha256: String,
    /// 解出来的包目录（相对工程根）
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLock {
    #[serde(default = "lock_spec")]
    pub spec: String,
    #[serde(default)]
    pub packages: BTreeMap<String, LockEntry>,
    #[serde(default)]
    pub targets: Vec<String>,
}

fn lock_spec() -> String {
    PROJECT_LOCK_SPEC.to_string()
}

impl Default for ProjectLock {
    fn default() -> Self {
        Self { spec: lock_spec(), packages: BTreeMap::new(), targets: Vec::new() }
    }
}

pub fn project_lock_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR).join(PROJECT_LOCK)
}

pub fn read_project_lock(root: &Path) -> Option<ProjectLock> {
    let p = project_lock_path(root);
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

pub fn write_project_lock(root: &Path, lock: &ProjectLock) -> Result<PathBuf> {
    let p = project_lock_path(root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lock = lock.clone();
    lock.spec = PROJECT_LOCK_SPEC.to_string();
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&lock)?))?;
    Ok(p)
}

pub struct Materialized {
    pub entry: LockEntry,
    pub files: Vec<(String, String)>,
    pub pkg: HurPackage,
    pub dir: PathBuf,
}

fn read_tree(dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for f in spec::content_files(dir) {
        let rel = spec::rel(dir, &f);
        if let Ok(text) = std::fs::read_to_string(&f) {
            out.push((rel, text));
        }
    }
    let m = dir.join(MANIFEST);
    if m.is_file() {
        out.push((MANIFEST.to_string(), std::fs::read_to_string(&m)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// 把三种来源（registry / `.hur` 产物 / 本地目录）落到 `<root>/.hur/packages/<id>/`
pub fn materialize(root: &Path, a: &ProjectAgent, force: bool) -> Result<Materialized> {
    let ref_ = a.r#ref.trim();
    let dest_root = root.join(PROJECT_DIR).join("packages");
    std::fs::create_dir_all(&dest_root)?;
    // `./x` / `../x` 相对**工程根**（不是进程 CWD）；`~/x` 展开；其余按原样
    let mut local = if ref_.starts_with("./") || ref_.starts_with("../") {
        root.join(ref_)
    } else {
        expand_home(ref_)
    };
    if !local.exists() {
        let cwd_rel = expand_home(ref_);
        if cwd_rel.exists() {
            local = cwd_rel;
        }
    }
    // 判定来源：`@ns/slug`（或 `ns/slug`）= registry；`.hur` / 路径 = 本地制品
    let looks_like_path = ref_.starts_with('.') || ref_.starts_with('/') || ref_.starts_with('~') || ref_.ends_with(".hur");
    let is_registry = !looks_like_path && (ref_.starts_with('@') || ref_.contains('/'));

    if is_registry {
        let (ns, slug) = crate::install::split_ref(ref_)?;
        let cfg = crate::cfg::load_cfg();
        let base = if a.registry.trim().is_empty() { cfg.effective_registry() } else { a.registry.trim().to_string() };
        if base.trim().is_empty() {
            bail!("同步「{ref_}」需要 registry 地址：加 --registry 或先 `hur login --registry …`");
        }
        let reg = Registry::new(&base, &cfg.effective_token())?;
        let info = reg.download_info(ns, slug)?;
        let url = info.get("url").and_then(|s| s.as_str()).unwrap_or_default().to_string();
        let sha = info.get("sha256").and_then(|s| s.as_str()).unwrap_or_default().to_string();
        if url.is_empty() {
            bail!("条目 {ns}/{slug} 没有可下载地址");
        }
        let bytes = reg.fetch_bytes(&url)?;
        let got = spec::sha256_hex(&bytes);
        if !sha.is_empty() && got != sha.to_ascii_lowercase() {
            bail!("{ref_} 下载内容 sha256 与 registry 登记不一致，已拒绝同步");
        }
        // 工程里锁了 sha256 就必须对上（团队复现的信任锚点）
        if !a.sha256.trim().is_empty() && a.sha256.trim().to_ascii_lowercase() != got {
            bail!(
                "{ref_} 的 sha256 与 hur.project.json 登记不一致（登记 {}，实际 {got}）—— 制品已变更，确认后再改登记值",
                &a.sha256.trim()[..12.min(a.sha256.trim().len())]
            );
        }
        let tmp = std::env::temp_dir().join(format!("hur-sync-{}-{slug}.hur", std::process::id()));
        std::fs::write(&tmp, &bytes)?;
        return unpack_into(&tmp, &dest_root, "registry", ref_, &sha, force || a.force, true);
    }

    if local.is_dir() {
        let src_pkg = spec::read_pkg(&local)?;
        let dest = dest_root.join(&src_pkg.id);
        if !dest.exists() || force || a.force {
            let _ = std::fs::remove_dir_all(&dest);
            copy_tree(&local, &dest)?;
        }
        let pkg = spec::read_pkg(&dest)?;
        let files = read_tree(&dest)?;
        return Ok(Materialized {
            entry: LockEntry {
                id: pkg.id.clone(),
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                source: "dir".into(),
                r#ref: ref_.to_string(),
                sha256: String::new(),
                path: format!("{PROJECT_DIR}/packages/{}", pkg.id),
            },
            files,
            pkg,
            dir: dest,
        });
    }

    if !local.is_file() {
        bail!("找不到来源「{ref_}」（可以用 @ns/slug、./x.hur 或包目录）");
    }
    let sha = spec::sha256_file(&local)?;
    if !a.sha256.trim().is_empty() && a.sha256.trim().to_ascii_lowercase() != sha {
        bail!("{ref_} 的 sha256 与 hur.project.json 登记不一致，已拒绝同步");
    }
    unpack_into(&local, &dest_root, "archive", ref_, &sha, force || a.force, false)
}

/// 解包到 `<dest_root>/<id>/`（`tmp_is_temp` 时删掉临时产物）
fn unpack_into(
    archive: &Path,
    dest_root: &Path,
    source: &str,
    ref_: &str,
    sha: &str,
    force: bool,
    tmp_is_temp: bool,
) -> Result<Materialized> {
    let staging = dest_root.join(format!(".unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let (_, id, version) = pack::unpack(archive, &staging)?;
    let dest = dest_root.join(&id);
    if dest.exists() {
        if !force {
            let _ = std::fs::remove_dir_all(&staging);
            bail!("{id} 已在工程内（{}）—— 要覆盖请加 --force", dest.display());
        }
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::rename(&staging, &dest)?;
    if tmp_is_temp {
        let _ = std::fs::remove_file(archive);
    }
    let pkg = spec::read_pkg(&dest)?;
    Ok(Materialized {
        entry: LockEntry {
            id: pkg.id.clone(),
            name: pkg.name.clone(),
            version: if version.is_empty() { pkg.version.clone() } else { version },
            source: source.to_string(),
            r#ref: ref_.to_string(),
            sha256: sha.to_string(),
            path: format!("{PROJECT_DIR}/packages/{}", pkg.id),
        },
        files: read_tree(&dest)?,
        pkg,
        dir: dest,
    })
}

fn expand_home(p: &str) -> PathBuf {
    let t = p.trim();
    if let Some(rest) = t.strip_prefix("~/") {
        return crate::cfg::home().join(rest);
    }
    PathBuf::from(t)
}

fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for f in spec::content_files(src) {
        let rel = spec::rel(src, &f);
        let to = dest.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&f, &to).with_context(|| format!("复制 {} 失败", rel))?;
    }
    for extra in [MANIFEST, "hur.lock"] {
        let from = src.join(extra);
        if from.is_file() {
            std::fs::copy(&from, dest.join(extra))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SyncedAgent {
    pub entry: LockEntry,
    pub artifacts: Vec<Artifact>,
    pub written: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SyncPlan {
    pub root: PathBuf,
    pub targets: Vec<String>,
    pub out: PathBuf,
    pub lock_path: PathBuf,
    pub agents: Vec<SyncedAgent>,
    /// `--export` 落盘的全部宿主文件（含被 `--out` 改写的那些）
    pub written: Vec<PathBuf>,
}

/// 工程同步：拉包 → 落 `.hur/packages/` → 写 `.hur/lock.json` →（可选）导出宿主配置
pub fn sync(
    project_path: &Path,
    only: Option<&str>,
    registry_override: &str,
    force: bool,
    export: bool,
    targets_override: &[String],
) -> Result<SyncPlan> {
    let root = project_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let mut proj = read_project(project_path)?;
    if !registry_override.trim().is_empty() {
        for a in proj.agents.iter_mut() {
            if a.registry.trim().is_empty() {
                a.registry = registry_override.trim().to_string();
            }
        }
    }
    let targets: Vec<String> = if !targets_override.is_empty() {
        targets_override.to_vec()
    } else if !proj.targets.is_empty() {
        proj.targets.clone()
    } else {
        vec![]
    };
    for t in &targets {
        if !valid_target(t) {
            bail!("未知目标「{t}」（可选：{}）", TARGETS.join(" / "));
        }
    }
    let out = if proj.out.trim().is_empty() { root.clone() } else { root.join(proj.out.trim()) };

    let mut lock = read_project_lock(&root).unwrap_or_default();
    let mut synced: Vec<SyncedAgent> = Vec::new();
    let mut written_all: Vec<PathBuf> = Vec::new();
    for a in proj.agents.clone() {
        if let Some(only) = only {
            if !a.r#ref.contains(only) {
                continue;
            }
        }
        let m = materialize(&root, &a, force)?;
        lock.packages.insert(m.entry.id.clone(), m.entry.clone());
        let mut artifacts = Vec::new();
        if !targets.is_empty() && m.pkg.kind == "agent" {
            let picked: Vec<String> = if a.targets.is_empty() { targets.clone() } else { a.targets.clone() };
            artifacts = render(&m.pkg, &m.files, &picked)?;
        }
        let written = if export && !artifacts.is_empty() { apply(&out, &artifacts)? } else { Vec::new() };
        written_all.extend(written.iter().cloned());
        synced.push(SyncedAgent { entry: m.entry, artifacts, written });
    }
    lock.targets = targets.clone();
    let lock_path = write_project_lock(&root, &lock)?;
    Ok(SyncPlan { root, targets, out, lock_path, agents: synced, written: written_all })
}

/// 从工程锁里列已同步的包（给 `hur mcp` / `hur ls` 用）
pub fn project_agents(root: &Path) -> Vec<(LockEntry, PathBuf)> {
    let Some(lock) = read_project_lock(root) else { return Vec::new() };
    lock.packages
        .into_values()
        .map(|e| {
            let dir = root.join(&e.path);
            (e, dir)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{AgentSpec, Deps, HurPackage, Permissions, PublishInfo, PKG_SPEC};

    fn pkg_with_agent() -> HurPackage {
        HurPackage {
            spec: PKG_SPEC.into(),
            kind: "agent".into(),
            id: "A-hotel-front-desk-abc123".into(),
            name: "Front Desk".into(),
            version: "0.2.0".into(),
            short: "FD".into(),
            domain: "hotel".into(),
            summary: "接待与房态查询助手。".into(),
            entry: "src/agent.ts".into(),
            runtime: "local-v0".into(),
            capabilities: vec!["reply".into()],
            deps: Deps::default(),
            permissions: Permissions { network: vec!["api.hotel.example.com".into()], local: vec![] },
            publish: PublishInfo { registry: "http://localhost:8181".into(), namespace: "@me".into(), visibility: "public".into(), slug: "me/front-desk".into() },
            security: None,
            agent: Some(AgentSpec {
                system_prompt: "你是前台接待。先给结论，再给下一步。".into(),
                persona: "克制、中文。".into(),
                tools: vec!["kb.search".into()],
                skills: vec!["skills/front-desk.md".into()],
                pipeline: None,
                guard: Some(serde_json::json!({ "max_reply_chars": 120 })),
                adapters: vec![],
            }),
        }
    }

    fn files() -> Vec<(String, String)> {
        vec![
            ("src/agent.ts".into(), "export const x = 1\n".into()),
            ("skills/front-desk.md".into(), "# 房态查询\n\n- 何时用：问房态时\n".into()),
        ]
    }

    #[test]
    fn parses_targets_and_rejects_unknown() {
        assert_eq!(parse_targets("all").unwrap().len(), 5);
        assert_eq!(parse_targets("claude, cursor").unwrap(), vec!["claude", "cursor"]);
        assert!(parse_targets("vscode").is_err());
    }

    #[test]
    fn claude_target_writes_agent_and_skill() {
        let a = render_target(&pkg_with_agent(), &files(), "claude").unwrap();
        let paths: Vec<&str> = a.iter().map(|x| x.path.as_str()).collect();
        assert!(paths.contains(&".claude/agents/hur-front-desk.md"), "{paths:?}");
        assert!(paths.contains(&".claude/skills/front-desk/SKILL.md"), "{paths:?}");
        let agent = a.iter().find(|x| x.path.contains("agents/")).unwrap();
        assert!(agent.content.starts_with("---\nname: hur-front-desk"), "{}", agent.content);
        assert!(agent.content.contains("你是前台接待"), "{}", agent.content);
        assert!(agent.content.contains("kb.search"), "{}", agent.content);
        assert!(agent.content.contains("输出上限 120 字"), "{}", agent.content);
        assert!(agent.content.contains("api.hotel.example.com"), "{}", agent.content);
    }

    #[test]
    fn cursor_and_cline_and_codex_targets() {
        let pkg = pkg_with_agent();
        let cur = render_target(&pkg, &files(), "cursor").unwrap();
        assert_eq!(cur[0].path, ".cursor/rules/hur-front-desk.mdc");
        assert!(
            cur[0].content.starts_with("---\ndescription: \"接待与房态查询助手。\""),
            "{}",
            cur[0].content
        );

        let cline = render_target(&pkg, &files(), "cline").unwrap();
        assert_eq!(cline[0].path, ".clinerules/hur-front-desk.md");

        let codex = render_target(&pkg, &files(), "codex").unwrap();
        assert_eq!(codex[0].path, "AGENTS.md");
        assert_eq!(codex[0].mode, Mode::Marked);
        assert!(codex[0].content.contains("<!-- hur:begin A-hotel-front-desk-abc123 -->"));
    }

    #[test]
    fn codex_marked_block_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("hur-marked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# 团队约定\n\n- 提交前跑测试\n").unwrap();
        let arts = render_target(&pkg_with_agent(), &files(), "codex").unwrap();
        apply(&dir, &arts).unwrap();
        apply(&dir, &arts).unwrap(); // 第二次必须替换而不是追加
        let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert_eq!(text.matches("<!-- hur:begin ").count(), 1, "{text}");
        assert!(text.contains("# 团队约定"), "{text}");
        assert!(text.contains("你是前台接待"), "{text}");
    }

    #[test]
    fn mcp_artifact_deep_merges_json() {
        let dir = std::env::temp_dir().join(format!("hur-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"other"}},"keep":1}"#,
        )
        .unwrap();
        let arts = render_target(&pkg_with_agent(), &files(), "mcp").unwrap();
        assert_eq!(arts[0].path, ".mcp.json");
        assert_eq!(arts[0].mode, Mode::MergeJson);
        apply(&dir, &arts).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(doc["mcpServers"]["other"]["command"], "other");
        assert_eq!(doc["mcpServers"]["hur"]["args"][0], "mcp");
        assert_eq!(doc["keep"], 1);
    }

    #[test]
    fn render_all_targets_covers_five_hosts() {
        let arts = render(&pkg_with_agent(), &files(), &parse_targets("all").unwrap()).unwrap();
        let hosts: std::collections::BTreeSet<&str> = arts.iter().map(|a| a.target.as_str()).collect();
        assert_eq!(hosts.iter().count(), 5);
        assert!(arts.iter().any(|a| a.path == "AGENTS.md"));
        assert!(arts.iter().any(|a| a.path == ".mcp.json"));
    }

    #[test]
    fn project_add_and_sync_from_local_dir() {
        let root = std::env::temp_dir().join(format!("hur-proj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // 造一个源包目录
        let src = root.join("src-pkg");
        std::fs::create_dir_all(src.join("skills")).unwrap();
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::write(src.join(MANIFEST), serde_json::to_string_pretty(&pkg_with_agent()).unwrap()).unwrap();
        std::fs::write(src.join("src/agent.ts"), "export const x = 1\n").unwrap();
        std::fs::write(src.join("skills/front-desk.md"), "# 房态查询\n").unwrap();

        let proj_file = root.join(PROJECT_FILE);
        let mut proj = Project { name: "team".into(), targets: parse_targets("claude,codex").unwrap(), ..Default::default() };
        add_agent(&mut proj, ProjectAgent { r#ref: "./src-pkg".into(), registry: String::new(), version: String::new(), sha256: String::new(), targets: vec![], force: false });
        write_project(&proj_file, &proj).unwrap();

        let plan = sync(&proj_file, None, "", false, true, &[]).unwrap();
        assert_eq!(plan.agents.len(), 1);
        assert_eq!(plan.agents[0].entry.source, "dir");
        assert!(plan.root.join(PROJECT_DIR).join("packages/A-hotel-front-desk-abc123").join(MANIFEST).exists());
        assert!(plan.root.join(PROJECT_DIR).join(PROJECT_LOCK).exists());
        assert!(plan.root.join(".claude/agents/hur-front-desk.md").exists());
        assert!(plan.root.join("AGENTS.md").exists());

        // 锁文件里有这条
        let lock = read_project_lock(&plan.root).unwrap();
        assert_eq!(lock.targets, vec!["claude", "codex"]);
        assert!(lock.packages.contains_key("A-hotel-front-desk-abc123"));
    }

    #[test]
    fn sync_missing_source_is_readable() {
        let root = std::env::temp_dir().join(format!("hur-proj-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let proj_file = root.join(PROJECT_FILE);
        let mut proj = Project::default();
        add_agent(&mut proj, ProjectAgent { r#ref: "./nope".into(), registry: String::new(), version: String::new(), sha256: String::new(), targets: vec![], force: false });
        write_project(&proj_file, &proj).unwrap();
        let err = sync(&proj_file, None, "", false, false, &[]).unwrap_err().to_string();
        assert!(err.contains("找不到来源"), "{err}");
    }
}
