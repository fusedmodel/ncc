//! 包规范 `harness-use-package/v1`：模型、解析、校验。
//!
//! 这是「固化」的核心：`hur.json` 是唯一清单，GUI（创作中心）与 CLI（hur）都读它。
//! 校验规则与前端 `agent/src/agent/hurpack.ts` 保持一致（同一份规则两处实现，规则编号 R1~R7）。

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PKG_SPEC: &str = "harness-use-package/v1";
pub const LOCK_SPEC: &str = "harness-use-lock/v1";
pub const MANIFEST: &str = "hur.json";
pub const LOCK: &str = "hur.lock";
pub const DIST: &str = "dist";

// 对外（内置到宿主程序时）更好读的别名，值与上面一致
pub const HUR_SPEC: &str = PKG_SPEC;
pub const HUR_MANIFEST: &str = MANIFEST;
pub const HUR_DIST: &str = DIST;

/// 包内容目录（打包时按此顺序收集，保证可复现）
pub const CONTENT_DIRS: [&str; 4] = ["src", "skills", "kb", "assets"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Permissions {
    /// 允许访问的域名（支持 `*.example.com` 通配；localhost / 127.0.0.1 始终允许）
    #[serde(default)]
    pub network: Vec<String>,
    /// 允许访问的本机资源：kb / files / packages
    #[serde(default)]
    pub local: Vec<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { network: Vec::new(), local: Vec::new() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Deps {
    #[serde(default)]
    pub harness: Vec<String>,
    #[serde(default)]
    pub agent: Vec<String>,
    #[serde(default)]
    pub skill: Vec<String>,
    #[serde(default)]
    pub kb: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<String>,
}

impl Deps {
    /// 所有依赖（kind, ref）
    pub fn iter(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::new();
        for (k, list) in [
            ("harness", &self.harness),
            ("agent", &self.agent),
            ("skill", &self.skill),
            ("kb", &self.kb),
            ("mcp", &self.mcp),
        ] {
            for r in list {
                out.push((k, r.as_str()));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishInfo {
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default)]
    pub slug: String,
}

/// `kind=agent` 的 **Agent 声明**（PRD §9.2）：把 Agent 定义从代码提升到声明，
/// 于是可静态校验、CLI/GUI/平台共读、**不需要执行第三方代码**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentSpec {
    /// 注入 chatbot 的 system prompt（Agent 的「大脑」）
    #[serde(default)]
    pub system_prompt: String,
    /// 人设约束（叠加在本机 agent.persona 之后）
    #[serde(default)]
    pub persona: String,
    /// 允许调用的本机工具（超出即拒；空 = 不限制，兼容旧包）
    #[serde(default)]
    pub tools: Vec<String>,
    /// 包内技能文件（相对路径，必须存在）
    #[serde(default)]
    pub skills: Vec<String>,
    /// 管线开关/权重（自由结构：intent / discover / rank / …）
    #[serde(default)]
    pub pipeline: Option<serde_json::Value>,
    /// 输出约束（自由结构：max_reply_chars / no_tool_calls / …）
    #[serde(default)]
    pub guard: Option<serde_json::Value>,
    /// 声明「这个包打算在哪些宿主里用」（claude/cursor/cline/codex/mcp）。
    /// 只是意图声明：`hur export` 不依赖它，但写全了 `hur verify` 会更早发现笔误。
    #[serde(default)]
    pub adapters: Vec<String>,
}

impl AgentSpec {
    pub fn is_empty(&self) -> bool {
        self.system_prompt.trim().is_empty()
            && self.persona.trim().is_empty()
            && self.tools.is_empty()
            && self.skills.is_empty()
            && self.pipeline.is_none()
            && self.guard.is_none()
            && self.adapters.is_empty()
    }
}

fn default_visibility() -> String {
    "public".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HurPackage {
    pub spec: String,
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub short: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub deps: Deps,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub publish: PublishInfo,
    /// `kind=agent` 的 Agent 声明（其它 kind 可省略）
    #[serde(default)]
    pub agent: Option<AgentSpec>,
    /// 安全策略声明（R8）：本包希望用哪套策略 + 进一步收紧自己
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<crate::policy::SecurityReq>,
    /// 出口声明（R10）：本包可以充当哪几条「出网通道」。
    /// 网关的 accept 路由必须由它**背书**（配置只能比声明更窄）—— 见 `egress_covers`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<Egress>,
}

/// 出口声明（`egress{}`）：本包声明自己可以充当哪几条「出网通道」。
///
/// ⚠️ **只有声明，没有凭据** —— 包要能被签名、分发、公开检索，密钥永远不进包。
/// `inject` 里只有**请求头名**，值由运行者（网关）的本地配置提供。
///
/// 为什么值得做成清单里的声明：这样「提供出口」不再是一份手写 JSON，而是
/// 一份**可签名、可分发、可被第三方核对**的包声明（S2b，PRD §16.3 的硬约束 3）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Egress {
    #[serde(default)]
    pub provides: Vec<EgressRoute>,
}

impl Egress {
    pub fn is_empty(&self) -> bool {
        self.provides.is_empty()
    }
}

/// 一条出口通道声明。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EgressRoute {
    /// 路由名（出现在网关 URL 里：`/v1/<name>/<路径>`）
    #[serde(default)]
    pub name: String,
    /// 上游基址。必须 `https://`；`http://` 只允许 loopback（本地联调）
    #[serde(default)]
    pub target: String,
    /// 允许的上游路径（**精确匹配**，不带查询串）
    #[serde(default)]
    pub paths: Vec<String>,
    /// 允许的方法（如 POST）
    #[serde(default)]
    pub methods: Vec<String>,
    /// 运行者需要注入的**请求头名**（只有名字，没有值）
    #[serde(default)]
    pub inject: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedDep {
    pub r#ref: String,
    pub resolved: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HurLock {
    pub spec: String,
    pub package: String,
    pub version: String,
    /// kind → 依赖锁定（**不写时间戳**，保证 pack 产物可复现）
    #[serde(default)]
    pub deps: BTreeMap<String, Vec<LockedDep>>,
    /// 包内文件 → sha256（相对路径，排序输出）
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warn,
    /// 「已核对通过」这类**证据**（既不拦人，也不进"警告算不算失败"的账）
    /// —— 需要"某项检查到底做没做"时，用它留痕（如 R9 签名核对）
    Info,
}

/// 一条检查结论（R1~R10 共用）。可序列化：CLI 的 `--json` 与 GUI 都要原样转出去。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub rule: String,
    pub level: Level,
    pub msg: String,
}

impl Issue {
    pub fn err(rule: &str, msg: impl Into<String>) -> Self {
        Self { rule: rule.into(), level: Level::Error, msg: msg.into() }
    }
    pub fn warn(rule: &str, msg: impl Into<String>) -> Self {
        Self { rule: rule.into(), level: Level::Warn, msg: msg.into() }
    }
    pub fn info(rule: &str, msg: impl Into<String>) -> Self {
        Self { rule: rule.into(), level: Level::Info, msg: msg.into() }
    }
    pub fn is_problem(&self) -> bool {
        self.level != Level::Info
    }
}

pub fn parse(text: &str) -> Result<HurPackage> {
    let pkg: HurPackage = serde_json::from_str(text).map_err(|e| anyhow!("hur.json 解析失败：{e}"))?;
    Ok(pkg)
}

pub fn read_pkg(dir: &Path) -> Result<HurPackage> {
    let p = dir.join(MANIFEST);
    if !p.exists() {
        bail!("当前目录没有 {MANIFEST}（用 `hur init` 生成，或 cd 到包目录）");
    }
    parse(&std::fs::read_to_string(&p)?)
}

pub fn read_lock(dir: &Path) -> Option<HurLock> {
    let p = dir.join(LOCK);
    if !p.exists() {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// 合法 kind
pub fn valid_kind(k: &str) -> bool {
    matches!(k, "agent" | "harness" | "repo")
}

pub fn kind_prefix(k: &str) -> &'static str {
    match k {
        "agent" => "A-",
        "harness" => "H-",
        _ => "",
    }
}

/// 语义化版本：`x.y.z` 可带 `-prerelease` / `+build`
pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut nums = [0u64; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums[i] = p.parse().ok()?;
    }
    Some((nums[0], nums[1], nums[2]))
}

pub fn cmp_semver(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (x, y) = (parse_semver(a)?, parse_semver(b)?);
    Some(x.cmp(&y))
}

fn valid_id(kind: &str, id: &str) -> bool {
    if id.is_empty() || id.len() > 96 {
        return false;
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@')) {
        return false;
    }
    let p = kind_prefix(kind);
    if p.is_empty() {
        // repo：id 即 slug
        !id.starts_with('-') && !id.ends_with('-')
    } else {
        id.starts_with(p)
    }
}

/// 收集包内容文件（相对路径，排序）——不含 hur.json/hur.lock（它们单独入包）
pub fn content_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    for d in CONTENT_DIRS {
        let p = dir.join(d);
        if p.is_dir() {
            stack.push(p);
        }
    }
    while let Some(p) = stack.pop() {
        let mut children: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&p) {
            for e in rd.flatten() {
                let path = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    children.push(path);
                }
            }
        }
        out.extend(children);
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                    if matches!(n, "README.md" | "LICENSE" | "LICENSE.md") {
                        out.push(p);
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn rel(dir: &Path, file: &Path) -> String {
    file.strip_prefix(dir).unwrap_or(file).to_string_lossy().replace('\\', "/")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn sha256_file(p: &Path) -> Result<String> {
    Ok(sha256_hex(&std::fs::read(p)?))
}

/// 一个已解析的 HTTP 目标。egress 声明与网关（`src/gateway.rs`）**共用同一份**解析，
/// 免得两边对“什么是合法的 target”理解不一致。
pub struct HttpTarget {
    pub scheme: String,
    /// host[:port]
    pub authority: String,
    pub host: String,
    /// 基路径，无尾斜杠（可能是空串）
    pub base: String,
}

impl HttpTarget {
    /// 拼上一条（已归一的）子路径。
    pub fn join(&self, sub: &str) -> String {
        format!("{}://{}{}/{}", self.scheme, self.authority, self.base, sub)
    }
}

/// 解析 `https://host[:port][/base]`。不接受用户信息、控制字符、空格；scheme 只认 http/https。
///
/// 基路径也走 `normalize_egress_path`（同一份规则）—— 否则 `https://host/a b`、
/// `https://host/a%2fb`、`https://host/../x` 这类写法会从这里溜进白名单比较。
pub fn parse_http_target(raw: &str) -> Option<HttpTarget> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if rest.is_empty() {
        return None;
    }
    let (authority, raw_base) = match rest.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (rest, None),
    };
    if authority.is_empty() || authority.contains(['\r', '\n', ' ', '@']) {
        return None;
    }
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority).to_string();
    if host.is_empty() || host.contains(['\r', '\n', ' ', '@', '/']) {
        return None;
    }
    let base = match raw_base {
        None => String::new(),
        Some(p) => {
            let trimmed = p.trim_matches('/');
            if trimmed.is_empty() {
                String::new()
            } else {
                normalize_egress_path(trimmed).map(|n| format!("/{n}"))?
            }
        }
    };
    Some(HttpTarget { scheme: scheme.to_string(), authority: authority.to_string(), host, base })
}

/// host 是不是本机（localhost / 127.0.0.1 / ::1）。
pub fn is_loopback_host(h: &str) -> bool {
    let h = h.trim_start_matches('[').trim_end_matches(']');
    if h == "localhost" {
        return true;
    }
    h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// egress / 网关路由名：`[a-z0-9._-]`，长度 1~64。
pub fn valid_egress_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// HTTP 头名（用于 `inject`）：字母数字与 `-`/`_`。
pub fn valid_header_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// 归一化一条 egress / 网关路径：去首尾斜杠，拒绝一切可能跑出白名单的写法。
///
/// 与网关共用（`src/gateway.rs` 直接调它）。这里**不做百分号解码**而是直接拒 `%`：
/// 解码正是 `%2e%2e` 绕过的来源，而固定路由代理的上游路径都是字面量。
/// `..` / 反斜杠 / 空段 / `?` / `#` / 空格 / 控制字符一律拒。
pub fn normalize_egress_path(p: &str) -> Option<String> {
    let p = p.trim().trim_matches('/');
    if p.is_empty() || p.len() > 512 {
        return None;
    }
    if p.contains(['%', '\\', '?', '#'])
        || p.chars().any(|c| c.is_control() || c == ' ')
        || p.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return None;
    }
    Some(p.to_string())
}

/// S2b：网关的 accept 路由是否**完全落在**某条 egress 声明的范围内。
///
/// 返回所有「配置比声明更宽」的问题（空 = 合规）。方向只有一个：
/// **配置只能比包声明更窄，不能更宽** —— 包的声明是上限。
pub fn egress_covers(
    decl: &EgressRoute,
    target: &str,
    paths: &[String],
    methods: &[String],
    inject: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    if decl.target.trim() != target.trim() {
        out.push(format!(
            "target 必须与声明一致：「{}」，配置写的是「{}」",
            decl.target.trim(),
            target.trim()
        ));
    }
    for p in paths {
        let Some(n) = normalize_egress_path(p) else {
            out.push(format!("路径「{p}」不合法"));
            continue;
        };
        if !decl.paths.iter().any(|d| normalize_egress_path(d).as_deref() == Some(n.as_str())) {
            out.push(format!(
                "路径「{p}」超出声明的范围（声明里只有：{}）",
                decl.paths.join(", ")
            ));
        }
    }
    for m in methods {
        if !decl.methods.iter().any(|d| d.eq_ignore_ascii_case(m)) {
            out.push(format!(
                "方法「{m}」不在声明里（声明里只有：{}）",
                decl.methods.join(", ")
            ));
        }
    }
    for h in inject {
        if !decl.inject.iter().any(|d| d.eq_ignore_ascii_case(h)) {
            out.push(format!(
                "注入头「{h}」不在声明里（声明里只有：{}）",
                if decl.inject.is_empty() { "（无）".to_string() } else { decl.inject.join(", ") }
            ));
        }
    }
    out
}

/// 从源码/技能文件里粗略抽取外呼域名（用于 R5 权限面核对）
fn hosts_in_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &text[i..];
        let hit = rest.find("https://").map(|o| (o, 8)).or_else(|| rest.find("http://").map(|o| (o, 7)));
        let Some((off, skip)) = hit else { break };
        let start = i + off + skip;
        let mut end = start;
        for (k, ch) in text[start..].char_indices() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '_') {
                end = start + k + ch.len_utf8();
            } else {
                break;
            }
        }
        let host = text[start..end]
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !host.is_empty() {
            out.push(host);
        }
        i = end.max(i + 1);
    }
    out.sort();
    out.dedup();
    out
}

pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return true;
    }
    allow.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        if a.is_empty() {
            return false;
        }
        if let Some(suffix) = a.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == a
        }
    })
}

/// 依赖引用是不是包内本地路径（`./x` / `../x` / `x.md`）
/// 远程制品 id（`H-xxx` / `A-xxx` / `@ns/slug`）不带 `.` → 视为远程
pub fn is_local_ref(r: &str) -> bool {
    let r = r.trim();
    r.starts_with("./") || r.starts_with("../") || (!r.contains(':') && !r.contains('/') && r.contains('.'))
}

/// R1~R5 校验（离线）。
///
/// `allow_unlocked_remote`：`hur build` 阶段为 true —— 此时锁文件还没写出来，
/// 远程依赖“未登记”只能算提醒；pack / verify 阶段为 false（必须先 build）。
pub fn validate(pkg: &HurPackage, dir: &Path, lock: Option<&HurLock>, allow_unlocked_remote: bool) -> Vec<Issue> {
    let mut out = Vec::new();

    // R1 规范与必填
    if pkg.spec != PKG_SPEC {
        out.push(Issue::err("R1", format!("spec 必须是 {PKG_SPEC}，当前是「{}」", pkg.spec)));
    }
    if !valid_kind(&pkg.kind) {
        out.push(Issue::err("R1", format!("kind 必须是 agent|harness|repo，当前是「{}」", pkg.kind)));
    }
    if pkg.name.trim().is_empty() {
        out.push(Issue::err("R1", "name 不能为空"));
    }
    if !valid_id(&pkg.kind, &pkg.id) {
        out.push(Issue::err(
            "R1",
            format!(
                "id「{}」不合法：kind={} 时必须以 `{}` 开头，且只含字母数字与 - _ . / @",
                pkg.id,
                pkg.kind,
                kind_prefix(&pkg.kind)
            ),
        ));
    }
    if pkg.kind != "repo" && pkg.entry.trim().is_empty() {
        out.push(Issue::err("R1", "entry 不能为空（指向包内入口文件，如 src/agent.ts）"));
    }

    // R2 版本
    match parse_semver(&pkg.version) {
        None => out.push(Issue::err("R2", format!("version「{}」不是合法语义化版本（x.y.z）", pkg.version))),
        Some(_) if pkg.version.trim() != pkg.version => {
            out.push(Issue::err("R2", "version 不能有前后空格"));
        }
        Some(_) => {}
    }

    // R3 入口与 kind 契约
    if pkg.kind != "repo" {
        let entry = dir.join(pkg.entry.trim());
        if !entry.exists() {
            out.push(Issue::err("R3", format!("entry「{}」不存在", pkg.entry)));
        } else if std::fs::read(&entry).map(|b| b.is_empty()).unwrap_or(true) {
            out.push(Issue::err("R3", format!("entry「{}」是空文件", pkg.entry)));
        }
    }
    if pkg.kind == "harness" {
        let has_schema = dir.join("src").exists()
            && content_files(dir)
                .iter()
                .any(|f| matches!(f.extension().and_then(|e| e.to_str()), Some("json")));
        if !has_schema {
            out.push(Issue::warn("R3", "kind=harness 建议在 src/ 下带 schema JSON（端点定义）"));
        }
    }

    // R4 依赖可解析
    for (kind, reference) in pkg.deps.iter() {
        let r = reference.trim();
        if r.is_empty() {
            continue;
        }
        let is_local = is_local_ref(r);
        if is_local {
            let p = dir.join(r);
            if !p.exists() {
                out.push(Issue::err("R4", format!("{kind} 依赖「{r}」在包内找不到")));
            }
            continue;
        }
        let entry = lock.and_then(|l| l.deps.get(kind)).and_then(|v| v.iter().find(|d| d.r#ref == r));
        match entry {
            None => {
                if allow_unlocked_remote {
                    out.push(Issue::warn(
                        "R4",
                        format!("{kind} 依赖「{r}」暂未登记 —— build 会写入 hur.lock 并标记未校验"),
                    ));
                } else {
                    out.push(Issue::err(
                        "R4",
                        format!("{kind} 依赖「{r}」没有登记进 hur.lock —— 先跑 `hur build`"),
                    ));
                }
            }
            Some(d) if d.resolved == "unresolved" => out.push(Issue::warn(
                "R4",
                format!("{kind} 依赖「{r}」已登记但未在 registry 校验（离线 build）；发布 / 安装时由 registry 解析"),
            )),
            Some(_) => {}
        }
    }

    // R5 权限面：源码里出现的外呼域名必须在 permissions.network 里声明
    for f in content_files(dir) {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "go" | "rs" | "md" | "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for h in hosts_in_text(&text) {
            if !host_allowed(&h, &pkg.permissions.network) {
                out.push(Issue::err(
                    "R5",
                    format!(
                        "{} 里调用了「{h}」，但 permissions.network 未声明（超范围调用）",
                        rel(dir, &f)
                    ),
                ));
            }
        }
    }

    // R7 Agent 声明（kind=agent）：声明必须能静态落地（PRD §9.2/§9.5）
    if pkg.kind == "agent" {
        match pkg.agent.as_ref() {
            None => out.push(Issue::warn(
                "R7",
                "kind=agent 建议带上 agent{} 声明（system_prompt / tools / skills），否则只能按代码包处理",
            )),
            Some(a) => {
                if a.system_prompt.trim().is_empty() {
                    out.push(Issue::warn("R7", "agent.system_prompt 为空 —— 桌面端会回落到内置 Agent 的提示词"));
                }
                if a.system_prompt.chars().count() > 8000 {
                    out.push(Issue::err("R7", format!("agent.system_prompt 过长（{} 字符，上限 8000）", a.system_prompt.chars().count())));
                }
                for s in &a.skills {
                    let r = s.trim();
                    if r.is_empty() {
                        out.push(Issue::err("R7", "agent.skills 里有空项"));
                    } else if !dir.join(r).exists() {
                        out.push(Issue::err("R7", format!("agent.skills 里的「{r}」在包内不存在")));
                    }
                }
                let known = ["repo_search", "harness.call", "kb.search", "task.create"];
                for t in &a.tools {
                    let tv = t.trim();
                    if tv.is_empty() {
                        out.push(Issue::err("R7", "agent.tools 里有空项"));
                    } else if !known.contains(&tv) {
                        out.push(Issue::warn("R7", format!("agent.tools 里的「{tv}」不是本机已知工具（桌面端会忽略）")));
                    }
                }
                // adapters：只做拼写体检（导出能力不依赖它）
                for ad in &a.adapters {
                    let v = ad.trim();
                    if v.is_empty() {
                        out.push(Issue::err("R7", "agent.adapters 里有空项"));
                    } else if !crate::interop::valid_target(v) {
                        out.push(Issue::warn(
                            "R7",
                            format!(
                                "agent.adapters 里的「{v}」不是已知宿主（可选：{}）",
                                crate::interop::TARGETS.join(" / ")
                            ),
                        ));
                    }
                }
            }
        }
    } else if pkg.agent.as_ref().map(|a| !a.is_empty()).unwrap_or(false) {
        out.push(Issue::warn("R7", "只有 kind=agent 才会读取 agent{} 声明（当前 kind 会忽略它）"));
    }

    // R8 安全策略声明（`security{}`）：引擎名、执行入口、network 档位、远程+签名搭配
    out.extend(crate::policy::validate_security(pkg, dir, None));

    // R10 出口声明（`egress{}`）：声明本包可以充当哪几条出网通道。
    // 抽成独立函数是因为**网关也要用同一份判定**（它拿这个当路由的上限）：
    // 「提供出口」是不是合法，只应该有一个答案。
    out.extend(validate_egress(pkg));

    out
}

/// R10：校验包的出口声明（`egress{}`）。
///
/// 要点不是“声明的对不对”，而是**声明本身要有边界**：target 必须 https（本机除外）、
/// paths/methods 不能为空（空 = 放行一切）、要出网就得在 `permissions.network` 里出现。
/// 网关 `accept` 路由拿它当上限（见 `egress_covers`）。
pub fn validate_egress(pkg: &HurPackage) -> Vec<Issue> {
    let mut out = Vec::new();
    match pkg.egress.as_ref() {
        Some(eg) if !eg.is_empty() => {
            let mut seen: Vec<String> = Vec::new();
            for r in &eg.provides {
                if !valid_egress_name(&r.name) {
                    out.push(Issue::err(
                        "R10",
                        format!("egress.provides 的路由名「{}」不合法（只允许 [a-z0-9._-]，≤64）", r.name),
                    ));
                } else if seen.contains(&r.name) {
                    out.push(Issue::err("R10", format!("egress 路由名「{}」重复", r.name)));
                } else {
                    seen.push(r.name.clone());
                }

                match parse_http_target(&r.target) {
                    None => out.push(Issue::err(
                        "R10",
                        format!("egress「{}」的 target「{}」不合法（要 https://主机[:端口][/基路径]）", r.name, r.target),
                    )),
                    Some(t) => {
                        if t.scheme == "http" && !is_loopback_host(&t.host) {
                            out.push(Issue::err(
                                "R10",
                                format!("egress「{}」的 target 是明文 http 且非本机 —— 出站必须 https", r.name),
                            ));
                        }
                        if pkg.permissions.network.is_empty() {
                            out.push(Issue::err(
                                "R10",
                                format!("egress「{}」要出网，但 permissions.network 是空的 —— 声明出口前先声明权限面", r.name),
                            ));
                        } else if !host_allowed(&t.host, &pkg.permissions.network) {
                            out.push(Issue::warn(
                                "R10",
                                format!("egress「{}」的域名「{}」不在 permissions.network 里（建议补齐，否则与 R5 不一致）", r.name, t.host),
                            ));
                        }
                    }
                }

                if r.paths.is_empty() {
                    out.push(Issue::err(
                        "R10",
                        format!("egress「{}」没声明 paths —— 空白名单会放行该上游下的一切路径", r.name),
                    ));
                }
                for p in &r.paths {
                    if normalize_egress_path(p).is_none() {
                        out.push(Issue::err("R10", format!("egress「{}」的路径「{p}」不合法", r.name)));
                    }
                }
                if r.methods.is_empty() {
                    out.push(Issue::err("R10", format!("egress「{}」没声明 methods（空 = 拒绝一切）", r.name)));
                }
                for h in &r.inject {
                    if !valid_header_name(h) {
                        out.push(Issue::err("R10", format!("egress「{}」的 inject 头名「{h}」不合法", r.name)));
                    }
                }
            }
        }
        Some(_) => out.push(Issue::warn(
            "R10",
            "egress{} 存在但 provides 为空 —— 等于没有声明出口（网关不会从它拿到任何路由）",
        )),
        None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pkg(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hur-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/agent.ts"), "export const x = 1\n").unwrap();
        dir
    }

    fn base(kind: &str, id: &str) -> HurPackage {
        HurPackage {
            egress: None,
            spec: PKG_SPEC.into(),
            kind: kind.into(),
            id: id.into(),
            name: "测试包".into(),
            version: "0.1.0".into(),
            short: "T".into(),
            domain: "hotel".into(),
            summary: String::new(),
            entry: "src/agent.ts".into(),
            runtime: "local-v0".into(),
            capabilities: vec![],
            deps: Deps::default(),
            permissions: Permissions::default(),
            publish: PublishInfo::default(),
            agent: None,
            security: None,
        }
    }

    #[test]
    fn semver_parse_and_cmp() {
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("v1.2.3"), None);
        assert_eq!(cmp_semver("0.2.0", "0.1.9"), Some(std::cmp::Ordering::Greater));
    }

    #[test]
    fn happy_path_has_no_errors() {
        let dir = temp_pkg("ok");
        let pkg = base("agent", "A-hotel-demo-abc123");
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn rejects_bad_kind_id_and_version() {
        let dir = temp_pkg("bad");
        let mut pkg = base("agent", "X-wrong-prefix");
        pkg.version = "0.1".into();
        let issues = validate(&pkg, &dir, None, false);
        let rules: Vec<&str> = issues.iter().map(|i| i.rule.as_str()).collect();
        assert!(rules.contains(&"R1"), "{issues:?}");
        assert!(rules.contains(&"R2"), "{issues:?}");
    }

    #[test]
    fn rejects_missing_entry() {
        let dir = temp_pkg("noentry");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        pkg.entry = "src/nope.ts".into();
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R3"), "{issues:?}");
    }

    #[test]
    fn rejects_undeclared_network_host() {
        let dir = temp_pkg("net");
        std::fs::write(
            dir.join("src/agent.ts"),
            "const u = 'https://api.hotel.example.com/v1/search'\n",
        )
        .unwrap();
        let pkg = base("agent", "A-hotel-demo-abc123");
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R5"), "{issues:?}");

        let mut ok = base("agent", "A-hotel-demo-abc123");
        ok.permissions.network = vec!["*.example.com".into()];
        let issues = validate(&ok, &dir, None, false);
        assert!(!issues.iter().any(|i| i.rule == "R5"), "{issues:?}");
    }

    fn with_agent(pkg: &mut HurPackage, a: AgentSpec) {
        pkg.agent = Some(a);
    }

    #[test]
    fn r7_agent_block_missing_is_only_a_warning() {
        let dir = temp_pkg("r7-none");
        let pkg = base("agent", "A-hotel-demo-abc123");
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
        assert!(issues.iter().any(|i| i.rule == "R7" && i.level == Level::Warn), "{issues:?}");
    }

    #[test]
    fn r7_skills_must_exist_in_package() {
        let dir = temp_pkg("r7-skill");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        with_agent(
            &mut pkg,
            AgentSpec {
                system_prompt: "你是前台".into(),
                skills: vec!["skills/front-desk.md".into()],
                ..Default::default()
            },
        );
        let issues = validate(&pkg, &dir, None, false);
        assert!(
            issues.iter().any(|i| i.rule == "R7" && i.level == Level::Error && i.msg.contains("front-desk")),
            "{issues:?}"
        );

        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(dir.join("skills/front-desk.md"), "# 前台技能\n").unwrap();
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn r7_tools_blank_is_error_and_unknown_is_warning() {
        let dir = temp_pkg("r7-tools");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        with_agent(
            &mut pkg,
            AgentSpec {
                system_prompt: "你是前台".into(),
                tools: vec!["repo_search".into(), "  ".into(), "delete_everything".into()],
                ..Default::default()
            },
        );
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R7" && i.level == Level::Error), "{issues:?}");
        assert!(
            issues.iter().any(|i| i.rule == "R7" && i.level == Level::Warn && i.msg.contains("delete_everything")),
            "{issues:?}"
        );
    }

    #[test]
    fn r7_system_prompt_length_cap() {
        let dir = temp_pkg("r7-long");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        with_agent(
            &mut pkg,
            AgentSpec { system_prompt: "甲".repeat(8001), ..Default::default() },
        );
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R7" && i.level == Level::Error), "{issues:?}");

        // 8000 字符（含多字节）正好在上限内
        let mut ok = base("agent", "A-hotel-demo-abc123");
        with_agent(&mut ok, AgentSpec { system_prompt: "甲".repeat(8000), ..Default::default() });
        let issues = validate(&ok, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn r7_agent_block_ignored_on_non_agent_kind() {
        let dir = temp_pkg("r7-kind");
        let mut pkg = base("harness", "H-hotel-demo-abc123");
        with_agent(&mut pkg, AgentSpec { system_prompt: "不该在这".into(), ..Default::default() });
        let issues = validate(&pkg, &dir, None, false);
        assert!(
            issues.iter().any(|i| i.rule == "R7" && i.level == Level::Warn && i.msg.contains("kind=agent")),
            "{issues:?}"
        );
    }

    /// `hur init --kind agent` 生成的工程必须自带能通过 R7 的 agent{} 声明
    #[test]
    fn r7_init_template_is_valid() {
        use crate::tpl::{build_package, files_for, InitInput};
        let dir = temp_pkg("r7-init");
        let pkg = build_package(&InitInput {
            kind: "agent".into(),
            name: "Front Desk".into(),
            role: "负责住房接待".into(),
            domain: "hotel".into(),
            short: String::new(),
            version: String::new(),
            summary: String::new(),
            registry: String::new(),
            namespace: String::new(),
        });
        let a = pkg.agent.as_ref().expect("kind=agent 必须带 agent{}");
        assert!(a.system_prompt.contains("住房接待"), "{}", a.system_prompt);
        assert_eq!(a.tools.len(), 4);

        // 把模板文件落盘后，R7 的 skills 存在性检查必须通过
        for (rel, body) in files_for(&pkg) {
            let p = dir.join(&rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.rule != "R7" || i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn remote_dep_unlocked_warns_in_build_and_fails_in_verify() {
        let dir = temp_pkg("remote");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        pkg.deps.harness = vec!["H-hotel-booking-1234".into()];
        let build = validate(&pkg, &dir, None, true);
        assert!(build.iter().all(|i| i.level != Level::Error), "{build:?}");
        let verify = validate(&pkg, &dir, None, false);
        assert!(verify.iter().any(|i| i.rule == "R4" && i.level == Level::Error), "{verify:?}");
    }

    #[test]
    fn local_dep_must_exist() {
        let dir = temp_pkg("dep");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        pkg.deps.skill = vec!["./skills/missing.md".into()];
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R4"), "{issues:?}");
    }

    /* ---------------- R10：出口声明 ---------------- */

    fn eg_route(name: &str, target: &str, paths: &[&str], methods: &[&str], inject: &[&str]) -> EgressRoute {
        EgressRoute {
            name: name.into(),
            target: target.into(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            methods: methods.iter().map(|s| s.to_string()).collect(),
            inject: inject.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 跑一遍 R1~R10，只要 R10 的结论。
    fn r10(pkg: &HurPackage, dir: &Path) -> Vec<Issue> {
        validate(pkg, dir, None, false).into_iter().filter(|i| i.rule == "R10").collect()
    }

    #[test]
    fn r10_accepts_a_well_formed_declaration() {
        let dir = temp_pkg("r10-ok");
        let mut p = base("harness", "A-r10-ok-000001");
        p.permissions.network = vec!["api.openai.com".into()];
        p.egress = Some(Egress {
            provides: vec![eg_route(
                "llm",
                "https://api.openai.com/v1",
                &["chat/completions"],
                &["POST"],
                &["Authorization"],
            )],
        });
        let out = r10(&p, &dir);
        assert!(out.is_empty(), "合规声明不该有问题：{out:?}");
    }

    #[test]
    fn r10_absent_egress_is_fine() {
        let dir = temp_pkg("r10-none");
        let p = base("harness", "A-r10-none-000001");
        assert!(r10(&p, &dir).is_empty());
    }

    #[test]
    fn r10_rejects_open_or_plaintext_or_credentialed_declarations() {
        let dir = temp_pkg("r10-bad");
        let cases: Vec<(&str, Vec<EgressRoute>, Vec<String>, bool)> = vec![
            // 空 paths：空白名单会放行该上游下的一切路径
            (
                "空白名单",
                vec![eg_route("llm", "https://api.openai.com/v1", &[], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            // 空 methods
            (
                "空方法",
                vec![eg_route("llm", "https://api.openai.com/v1", &["chat/completions"], &[], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            // 明文 http 且非本机
            (
                "明文出站",
                vec![eg_route("llm", "http://api.example.com/v1", &["x"], &["POST"], &[])],
                vec!["api.example.com".into()],
                true,
            ),
            // 要出网但 permissions.network 空
            (
                "没有权限面",
                vec![eg_route("llm", "https://api.openai.com/v1", &["chat/completions"], &["POST"], &[])],
                vec![],
                true,
            ),
            // 路径穿越 / 百分号编码
            (
                "路径带穿越",
                vec![eg_route("llm", "https://api.openai.com/v1", &["../etc/passwd"], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            (
                "路径带编码",
                vec![eg_route("llm", "https://api.openai.com/v1", &["a%2fb"], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            // 路由名不合法 / 重名
            (
                "名字非法",
                vec![eg_route("LLM Main", "https://api.openai.com/v1", &["x"], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            (
                "重名",
                vec![
                    eg_route("llm", "https://api.openai.com/v1", &["x"], &["POST"], &[]),
                    eg_route("llm", "https://api.openai.com/v1", &["y"], &["POST"], &[]),
                ],
                vec!["api.openai.com".into()],
                true,
            ),
            // 注入头名不合法
            (
                "注入头名非法",
                vec![eg_route("llm", "https://api.openai.com/v1", &["x"], &["POST"], &["Bad Header"])],
                vec!["api.openai.com".into()],
                true,
            ),
        ];
        for (label, provides, network, want_problem) in cases {
            let mut p = base("harness", "A-r10-bad-000001");
            p.permissions.network = network;
            p.egress = Some(Egress { provides });
            let out = r10(&p, &dir);
            assert!(
                out.iter().any(|i| i.is_problem()) == want_problem,
                "「{label}」的 R10 结论不符合预期：{out:?}"
            );
        }
    }

    #[test]
    fn r10_allows_loopback_http_for_local_dev() {
        let dir = temp_pkg("r10-loopback");
        let mut p = base("harness", "A-r10-loop-000001");
        p.permissions.network = vec!["127.0.0.1".into()];
        p.egress = Some(Egress {
            provides: vec![eg_route("llm", "http://127.0.0.1:8899/v1", &["chat/completions"], &["POST"], &[])],
        });
        let out = r10(&p, &dir);
        assert!(out.iter().all(|i| i.level != Level::Error), "本机 http 应当允许：{out:?}");
    }

    #[test]
    fn egress_declares_but_never_carries_credentials() {
        // 结构上就没有“放值”的位置：inject 只存头名。
        // 反证：一个想把密钥写进 inject 的包，会因为头名合法性被拒。
        let dir = temp_pkg("r10-secret");
        let mut p = base("harness", "A-r10-secret-000001");
        p.permissions.network = vec!["api.openai.com".into()];
        p.egress = Some(Egress {
            provides: vec![eg_route(
                "llm",
                "https://api.openai.com/v1",
                &["x"],
                &["POST"],
                &["Authorization: Bearer sk-x"],
            )],
        });
        assert!(r10(&p, &dir).iter().any(|i| i.is_problem()), "把值塞进 inject 必须被拒");
    }

    /* ---------------- S2b：配置只能比声明更窄 ---------------- */

    #[test]
    fn egress_covers_only_narrows_never_widens() {
        let decl = eg_route(
            "llm",
            "https://api.openai.com/v1",
            &["chat/completions", "models"],
            &["POST", "GET"],
            &["Authorization"],
        );

        // 完全一致 → 合规
        assert!(egress_covers(
            &decl,
            "https://api.openai.com/v1",
            &["chat/completions".into()],
            &["POST".into()],
            &["Authorization".into()]
        )
        .is_empty());

        // 更窄（只留一条路径 / 一个方法）→ 合规
        assert!(egress_covers(&decl, "https://api.openai.com/v1", &["models".into()], &["GET".into()], &[]).is_empty());

        // 更宽 → 必须逐条报出来
        let bad = egress_covers(
            &decl,
            "https://api.openai.com/v2",
            &["chat/completions".into(), "files".into()],
            &["POST".into(), "DELETE".into()],
            &["Authorization".into(), "X-Api-Key".into()],
        );
        assert_eq!(bad.len(), 4, "target/路径/方法/注入头 各一条：{bad:?}");
        assert!(bad[0].contains("target"));
        assert!(bad.iter().any(|m| m.contains("files")));
        assert!(bad.iter().any(|m| m.contains("DELETE")));
        assert!(bad.iter().any(|m| m.contains("X-Api-Key")));
    }

    #[test]
    fn egress_covers_compares_paths_after_normalization() {
        let decl = eg_route("llm", "https://api.openai.com/v1", &["chat/completions"], &["POST"], &[]);
        // 前导斜杠 / 尾随斜杠都算同一条
        assert!(egress_covers(&decl, "https://api.openai.com/v1", &["/chat/completions/".into()], &["post".into()], &[]).is_empty());
        // 但穿越写法不因为“看起来像”就放行
        assert!(!egress_covers(&decl, "https://api.openai.com/v1", &["../chat/completions".into()], &["POST".into()], &[]).is_empty());
    }

    #[test]
    fn normalize_egress_path_is_the_shared_rule() {
        assert_eq!(normalize_egress_path("/chat/completions/").as_deref(), Some("chat/completions"));
        for bad in ["", "/", "..", "a/../b", "a//b", "%2e%2e/x", "a\\b", "a?x=1", "a#f", "a b"] {
            assert!(normalize_egress_path(bad).is_none(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn parse_http_target_rejects_the_dangerous_shapes() {
        assert!(parse_http_target("https://api.openai.com/v1").is_some());
        assert!(parse_http_target("http://127.0.0.1:8899").is_some());
        for bad in ["api.openai.com", "ftp://x/y", "https://", "https://user@host/x", "https://host/a b"] {
            assert!(parse_http_target(bad).is_none(), "应拒绝：{bad}");
        }
    }
}
