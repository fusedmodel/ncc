// `ncc app` —— 把「个人 Agent 舱」这类产品变成一条命令能起停的东西。
//
// 设计见 `prd/ncc-personal-agent.md`。四层分工，谁也别越界：
//
//   产品本体 = 用户的（`app.json` 描述它；界面是用户的事，NCC 不出像素）
//   引擎     = 本模块（`ncc app init|doctor|up|status|export`）
//   应用逻辑 = HUR 包（`hur.json`：声明 `state{}` / `egress{}` / `entry`）
//   互联     = ncc-platform（身份 / 点到点分享 / 网关 / P2P）—— **不新增数据面**
//
// 两条纪律沿用整个 CLI：
//   1. **启动前把话说清**：`doctor` 的同款检查在 `up` 之前先跑一遍，缺什么、下一步跑什么，直接打出来。
//   2. **不确定就如实说不知道**：目标没声明的能力、读不到的接口，都在面板上写"不可用 + 为什么"，
//      而不是给一个空列表让人以为"没有数据"。
use crate::api;
use crate::capability;
use crate::config::{self, CliConfig};
use crate::gateway;
use crate::httpsrv::{self, Req, Resp};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

/// 部署描述的规范版本号。改结构就改它 —— 老舱文件要能被认出来。
pub const APP_SPEC: &str = "ncc-app/v1";
/// 控制台默认端口（loopback）。
pub const DEFAULT_PORT: u16 = 8487;
/// 舱默认声明的出口目标（要换就改 hur.json 的 egress.target，网关配置只能比它更窄）。
const EGRESS_TARGET: &str = "https://api.openai.com/v1";
/// 舱的入口脚本：把 NCC 的 MCP 面交给宿主 Agent（任何 mcp/stdio loader 都能加载）。
const RUN_SH: &str = "#!/bin/sh\n\
# 这个舱的入口（HUR 的 entry）：把 NCC 的 MCP 面交给宿主 Agent。\n\
# 任何实现了 mcp/stdio loader 的 runtime 都能加载它（`--target <名字>` 可指定连哪一侧）。\n\
exec ncc mcp \"$@\"\n";

const TPL_README: &str = include_str!("app_templates/README.md");
const TPL_SKILL: &str = include_str!("app_templates/SKILL.md");
const CONSOLE_HTML: &str = include_str!("app_templates/console.html");

/* ============================ 部署描述 ============================ */

/// 一台机器上的这个舱长什么样（`app.json`）。
///
/// 注意它是**部署描述**，不是包：包是 `hur.json`（应用逻辑）。两者放在同一个目录里，
/// `ncc app export` 就是把这个目录变成可交付物。
#[derive(Debug, Clone)]
pub struct AppManifest {
    pub name: String,
    pub title: String,
    /// 内容落在哪个命名空间（`@me` 这种）。状态是**指向**：换台机器部署同一个 app.json，
    /// 接上同一个命名空间就还是同一份内容。
    pub namespace: String,
    /// `mcp` | `harness` | `proxy` —— 应用逻辑怎么被接上。刻意只这三种。
    pub entry_mode: String,
    /// 入口用哪些包（`@ncc/ncc-for-agents` 这类）。
    pub entry_packages: Vec<String>,
    /// 画布/笔记住哪个库（kb 的 namespace 过滤）。
    pub state_kb_namespace: String,
    /// 记忆读谁的那一份（默认 `self`）。
    pub mem_subject: String,
    /// 检查点默认打什么 label。
    pub ckpt_label: String,
    /// **分享走哪个目标**（默认 `hub` = 云端 ncc.ai）。
    ///
    /// 为什么舱要记两个目标：内容住节点（`ncc-registry`），而点到点分享在云端 ——
    /// "内容住节点、互联走平台"这句话落到配置上就是**两个目标**，一个也不能省。
    pub share_target: String,
    /// 出口用网关的哪条路由（空 = 不声明出口）。
    pub egress_route: String,
    /// 控制台监听地址（默认 loopback）。
    pub listen: String,
}

impl Default for AppManifest {
    fn default() -> Self {
        Self {
            name: "personal-agent".into(),
            title: "我的个人 Agent 舱".into(),
            namespace: String::new(),
            entry_mode: "mcp".into(),
            entry_packages: vec!["@ncc/ncc-for-agents".into()],
            state_kb_namespace: String::new(),
            mem_subject: "self".into(),
            ckpt_label: "handoff".into(),
            share_target: "hub".into(),
            egress_route: "llm".into(),
            listen: format!("127.0.0.1:{DEFAULT_PORT}"),
        }
    }
}

impl AppManifest {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("app.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读不到 {}（先 `ncc app init`）", path.display()))?;
        let v: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} 不是合法 JSON", path.display()))?;
        Self::from_json(&v)
    }

    pub fn from_json(v: &Value) -> Result<Self> {
        let spec = v.get("spec").and_then(|x| x.as_str()).unwrap_or("");
        if spec != APP_SPEC {
            bail!("app.json 的 spec 必须是 {APP_SPEC}（当前是「{spec}」）");
        }
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let name = s("name");
        if name.is_empty() {
            bail!("app.json 缺 name");
        }
        let entry = v.get("entry").cloned().unwrap_or(json!({}));
        let mode = entry.get("mode").and_then(|x| x.as_str()).unwrap_or("mcp").to_string();
        if !["mcp", "harness", "proxy"].contains(&mode.as_str()) {
            bail!("entry.mode 必须是 mcp | harness | proxy（当前是「{mode}」）");
        }
        let pkgs = entry
            .get("packages")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect())
            .unwrap_or_default();
        let state = v.get("state").cloned().unwrap_or(json!({}));
        let ns = s("namespace");
        let kb_ns = state
            .pointer("/kb/namespace")
            .and_then(|x| x.as_str())
            .map(|x| x.trim().to_string())
            .unwrap_or_else(|| ns.clone());
        Ok(Self {
            name,
            title: {
                let t = s("title");
                if t.is_empty() { "个人 Agent 舱".to_string() } else { t }
            },
            namespace: ns,
            entry_mode: mode,
            entry_packages: pkgs,
            state_kb_namespace: kb_ns,
            mem_subject: state
                .pointer("/memory/subject")
                .and_then(|x| x.as_str())
                .unwrap_or("self")
                .to_string(),
            ckpt_label: state
                .pointer("/checkpoints/label")
                .and_then(|x| x.as_str())
                .unwrap_or("handoff")
                .to_string(),
            share_target: {
                let t = v.pointer("/share/target").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                if t.is_empty() { "hub".to_string() } else { t }
            },
            egress_route: v
                .pointer("/egress/route")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            listen: {
                let l = v
                    .pointer("/console/listen")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if l.is_empty() { format!("127.0.0.1:{DEFAULT_PORT}") } else { l }
            },
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "spec": APP_SPEC,
            "name": self.name,
            "title": self.title,
            "namespace": self.namespace,
            "entry": { "mode": self.entry_mode, "packages": self.entry_packages },
            "state": {
                "kb": { "namespace": self.state_kb_namespace },
                "memory": { "subject": self.mem_subject },
                "checkpoints": { "label": self.ckpt_label },
            },
            "egress": { "route": self.egress_route, "note": "模型调用统一走客户自装网关（ncc gateway）" },
            // 分享在**平台**那一侧（内容在节点这一侧）—— 所以这里是另一个目标名。
            "share": { "enabled": true, "mode": "snapshot", "target": self.share_target },
            "console": { "listen": self.listen },
        })
    }

    /// 应用逻辑的包声明（`hur.json`）。
    ///
    /// 关键点：它必须是**一个合法 HUR 包**（`ncc app doctor` 会用 `hur_core` 的同一份判定校验它）。
    /// 于是"这个产品需要什么状态、要哪条出口、从哪里进来"是**可签名、可分发、可复核**的一句话，
    /// 而不是散在一堆环境变量和 README 里。
    pub fn hur_json(&self) -> Value {
        let kb_ref = if self.state_kb_namespace.trim().is_empty() {
            "*".to_string()
        } else {
            format!("{}/handbook", ns_ref(&self.state_kb_namespace))
        };
        let mut net: Vec<String> = vec!["ncc.ai".to_string()];
        let mut egress = Value::Null;
        if !self.egress_route.is_empty() {
            let host = crate::gwreport::host_of(EGRESS_TARGET);
            if !host.is_empty() {
                net.push(host);
            }
            // 出口声明：只有**头名**，没有凭据 —— 值由运行者（网关那台机器）的本地配置给。
            // 与 S2b① 同一条硬约束：包要能被公开检索，密钥就不能进包。
            egress = json!({ "provides": [{
                "name": self.egress_route,
                "target": EGRESS_TARGET,
                "paths": ["chat/completions"],
                "methods": ["POST"],
                "inject": ["Authorization"],
            }] });
        }
        let mut v = json!({
            "spec": "harness-use-package/v1",
            "kind": "agent",
            "id": format!("A-ncc-app-{}", slug(&self.name)),
            "name": self.title,
            "version": "0.1.0",
            "summary": "个人 Agent 舱（知识库 / 记忆 / 检查点 + 出口走客户自装网关）",
            "entry": "run.sh",
            "agent": {
                "system_prompt": format!(
                    "你是这台机器上的那个人（{}）的个人助理。你读的是他节点上的知识库与记忆，\
                     改动状态要他自己动手，不要替他决定。回答涉及数据在哪时，说清「内容在节点、\
                     控制面只有摘要」。",
                    if self.namespace.is_empty() { "命名空间未指定" } else { &self.namespace }
                ),
                "skills": ["SKILL.md"],
            },
            // 权限面：节点地址是**运行期配置**（app.json 与 CLI 目标决定），包只能声明确定的那几个：
            // 云端控制面（身份 / 分享）+ 出口目标。要收紧就把你的节点域名也加进来。
            "permissions": { "network": net },
            "state": {
                // 引用形态：`@命名空间/slug` 或 `*`（还没定命名空间时用通配 —— R11 只认这两种）。
                "kb": [{ "ref": kb_ref, "mode": "readwrite" }],
                "memory": { "subject": self.mem_subject, "kinds": ["fact", "preference", "summary"], "ttl_days": 365 },
                "checkpoints": { "enabled": true, "label": self.ckpt_label, "keep_local": 5 },
            },
        });
        if !egress.is_null() {
            v["egress"] = egress;
        }
        v
    }

    /// 是不是只监听本机（非 loopback 要显式承担风险 —— 与网关同一套纪律）。
    fn listen_port(&self) -> Result<u16> {
        self.listen
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .with_context(|| format!("console.listen 不合法：{}", self.listen))
    }
    fn listen_is_loopback(&self) -> bool {
        match self.listen.split(':').next().unwrap_or("") {
            "127.0.0.1" | "localhost" | "[::1]" | "::1" => true,
            _ => false,
        }
    }
}

/// 包 id 只允许字母数字与 `-_.`（R1），所以舱名要过一遍。
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches(['-', '.']).to_string();
    if t.is_empty() { "personal-agent".to_string() } else { t }
}

/// `@team` / `team` / 空 → 命名空间引用形态（空 = 服务端按"我的"处理）。
fn ns_ref(s: &str) -> String {
    let s = s.trim().trim_start_matches('@');
    if s.is_empty() { String::new() } else { format!("@{s}") }
}

fn app_dir(explicit: Option<&str>) -> Result<PathBuf> {
    let dir = match explicit {
        Some(d) => PathBuf::from(d),
        None => std::env::current_dir().context("取不到当前目录")?,
    };
    Ok(dir)
}

/* ============================ 检查（doctor / up 共用） ============================ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Err,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub title: String,
    pub detail: String,
    /// 没过的项必须给"下一步跑什么" —— 只说"不行"是没用的。
    pub next: Option<String>,
}

impl Check {
    fn ok(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { level: Level::Ok, title: title.into(), detail: detail.into(), next: None }
    }
    fn warn(title: impl Into<String>, detail: impl Into<String>, next: impl Into<String>) -> Self {
        Self { level: Level::Warn, title: title.into(), detail: detail.into(), next: Some(next.into()) }
    }
    fn err(title: impl Into<String>, detail: impl Into<String>, next: impl Into<String>) -> Self {
        Self { level: Level::Err, title: title.into(), detail: detail.into(), next: Some(next.into()) }
    }
    fn icon(&self) -> &'static str {
        match self.level {
            Level::Ok => "✓",
            Level::Warn => "△",
            Level::Err => "✗",
        }
    }
}

/// 逐项自检。每一项都**真的去问一次**（能发请求就发），不猜。
pub fn checks(cfg: &CliConfig, app: &AppManifest, dir: &Path) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();

    // ① 部署描述本身
    match AppManifest::load(dir) {
        Ok(_) => out.push(Check::ok("部署描述 app.json", format!("{APP_SPEC} · {}", dir.display()))),
        Err(e) => out.push(Check::err(
            "部署描述 app.json",
            format!("{e:#}"),
            format!("在 {} 里跑 `ncc app init`", dir.display()),
        )),
    }
    // ② 监听地址（非 loopback = 把这个舱的界面暴露到网上，必须显式）
    if app.listen_is_loopback() {
        out.push(Check::ok("控制台监听", format!("{}（只对本机）", app.listen)));
    } else {
        out.push(Check::warn(
            "控制台监听",
            format!("{} 不是 loopback —— 你把这个舱的界面暴露给网络了", app.listen),
            "改回 127.0.0.1（在 app.json 里），要给别人看就发**快照链接**：控制台的「生成快照链接」",
        ));
    }
    // ③ 应用逻辑必须先是合法 HUR 包（与 `ncc hur verify` 同一份判定）
    match hur_check(dir) {
        Ok(msg) => out.push(Check::ok("应用逻辑 hur.json", msg)),
        Err((msg, next)) => out.push(Check::err("应用逻辑 hur.json", msg, next)),
    }
    // ④ 凭据
    let token = config::token_opt(cfg);
    match &token {
        Some(_) => out.push(Check::ok("凭据", format!("用 {} 存的身份", config::config_path().display()))),
        None => out.push(Check::warn(
            "凭据",
            "没登录：公开内容与目录能读，写内容 / 发分享都不行".to_string(),
            "ncc login --email … --password …（或配 API-Key）",
        )),
    }
    // ⑤ 目标与能力
    let meta = capability::probe(cfg);
    if meta.product.is_empty() {
        out.push(Check::err(
            "目标可达",
            format!("{} 没有响应 /api/meta", cfg.base_url()),
            "检查地址与网络：`ncc target list` 看当前目标，`ncc --base <URL>` 临时换一个",
        ));
    } else {
        out.push(Check::ok(
            "目标可达",
            format!("{}（{} · {}）· 能力 {}", cfg.base_url(), meta.product, meta.kind, meta.capability_line()),
        ));
    }
    // ⑥ 三样内容：声明了就要能读（**真发一次请求**，不是看能力清单就下结论）
    for (cap, label, path) in [
        ("kb", "画布 / 笔记（kb）", "/api/kb?size=1"),
        ("mem", "记忆（mem）", "/api/mem?limit=1"),
        ("ckpt", "检查点（ckpt）", "/api/ckpt?size=1"),
    ] {
        match meta.has(cap) {
            Some(false) => out.push(Check::err(
                label,
                format!("这个目标没声明 {cap} 能力"),
                format!("内容住 ncc-registry 节点：`ncc target list` 找一个，或 `ncc --base <节点地址>`；本机节点看 ncc-registry 的 README"),
            )),
            _ => match api::get(cfg, path, token.as_deref()) {
                Ok(_) => out.push(Check::ok(label, format!("读得到（{path}）"))),
                Err(e) => out.push(Check::err(label, format!("{e:#}"), "确认登录态与命名空间：`ncc me`")),
            },
        }
    }
    // ⑦ 出口（声明了才查）
    if app.egress_route.is_empty() {
        out.push(Check::warn(
            "出口",
            "没声明 egress.route：模型调用不经过 NCC，也就没有摘要审计".to_string(),
            "起一台网关（`ncc gateway init` → `ncc gateway run`）再在 app.json 里写上路由名",
        ));
    } else {
        match gateway::load_config_pub() {
            Ok(gw) if !gw.gateway_token.trim().is_empty() => out.push(Check::ok(
                "出口",
                format!("路由「{}」· 网关 {}（控制面只收摘要）", app.egress_route, gw.gateway_id),
            )),
            Ok(_) => out.push(Check::warn(
                "出口",
                format!("声明了路由「{}」但本机没有网关凭据", app.egress_route),
                "ncc gateway bind --namespace @你的组织（或先 `ncc gateway init` 起一台）",
            )),
            Err(e) => out.push(Check::warn("出口", format!("读不到网关配置：{e:#}"), "ncc gateway init")),
        }
    }
    // ⑧ 分享那一侧（内容在节点、分享在平台 —— 舱记着两个目标）
    match resolve_share(cfg, app) {
        Err(why) => out.push(Check::warn(
            "分享",
            why,
            "加一个云端目标（用 `ncc --base https://ncc.ai …` 跑一条命令就会建）再把它的名字写进 app.json 的 share.target",
        )),
        Ok((name, share_cfg)) => {
            let sm = capability::probe(&share_cfg);
            match sm.has("share") {
                Some(false) => out.push(Check::warn(
                    "分享",
                    format!("目标「{name}」（{}）没声明 share 能力", share_cfg.base_url()),
                    format!("换一个目标放分享（app.json 的 share.target），或 `ncc --target {name} …` 确认这是不是你要的那台"),
                )),
                _ if sm.kind == "hub" => out.push(Check::ok(
                    "分享",
                    format!("目标「{name}」（云端）· 可以生成只读快照链接（对方不用登录）"),
                )),
                _ => out.push(Check::warn(
                    "分享",
                    format!("分享目标「{name}」是内网节点：它的分享是**制品级**的（要 ref），发不了舱快照"),
                    "把 share.target 指到云端目标（默认 hub）；在节点上分享制品用 `ncc registry share create @ns/slug`",
                )),
            }
        }
    }
    // ⑨ 端口
    match app.listen_port() {
        Ok(port) => match TcpListener::bind(("127.0.0.1", port)) {
            Ok(_) => out.push(Check::ok("控制台端口", format!("{port} 空闲"))),
            Err(e) => out.push(Check::warn(
                "控制台端口",
                format!("{port} 用不了：{e}"),
                format!("`ncc app up --port {other}`（换个端口）", other = port.saturating_add(1)),
            )),
        },
        Err(e) => out.push(Check::err("控制台端口", format!("{e:#}"), "改 app.json 里的 console.listen")),
    }

    out
}

/// 应用逻辑是不是合法 HUR 包 —— 直接调 `hur_core` 的同一份判定，不另写一套规则。
fn hur_check(dir: &Path) -> std::result::Result<String, (String, String)> {
    let path = dir.join("hur.json");
    if !path.exists() {
        return Err((
            format!("{} 不存在", path.display()),
            "在舱目录里跑 `ncc app init`（它会连 hur.json 一起生成）".into(),
        ));
    }
    let pkg = match hur_core::spec::read_pkg(dir) {
        Ok(p) => p,
        Err(e) => return Err((format!("读不了 hur.json：{e:#}"), "`ncc hur verify` 会给出更细的错".into())),
    };
    let issues = hur_core::spec::validate(&pkg, dir, hur_core::spec::read_lock(dir).as_ref(), false);
    let errs: Vec<_> = issues.iter().filter(|i| i.level == hur_core::spec::Level::Error).collect();
    if !errs.is_empty() {
        let msg = errs.iter().map(|i| format!("[{}] {}", i.rule, i.msg)).collect::<Vec<_>>().join("；");
        return Err((msg, "`ncc hur verify --dir <舱目录>` 看完整清单".into()));
    }
    let warns = issues.iter().filter(|i| i.level == hur_core::spec::Level::Warn).count();
    Ok(format!(
        "{} {} · kind={} · 通过校验{}",
        pkg.id,
        pkg.version,
        pkg.kind,
        if warns > 0 { format!("（{warns} 条提醒）") } else { String::new() }
    ))
}

/* ============================ init ============================ */

pub struct InitArgs {
    pub dir: Option<String>,
    pub name: Option<String>,
    pub namespace: Option<String>,
    /// 分享那一侧的目标名（内容那一侧永远是"当前目标"）。
    pub share_target: Option<String>,
    pub port: Option<u16>,
    pub force: bool,
    pub json: bool,
}

pub fn init(cfg: &CliConfig, a: &InitArgs) -> Result<()> {
    let dir = app_dir(a.dir.as_deref())?;
    std::fs::create_dir_all(&dir).with_context(|| format!("建不了目录 {}", dir.display()))?;

    let mut app = AppManifest::default();
    if let Some(n) = &a.name {
        app.name = n.trim().to_string();
    }
    if let Some(p) = a.port {
        app.listen = format!("127.0.0.1:{p}");
    }
    if let Some(t) = &a.share_target {
        app.share_target = t.trim().to_string();
    }
    // 命名空间缺省：跟着当前目标里的自己走（有登录态就用，没登录就留空 = 服务端按"我的"）
    app.namespace = match &a.namespace {
        Some(ns) => ns.trim().to_string(),
        None => current_username(cfg).unwrap_or_default(),
    };
    app.state_kb_namespace = app.namespace.clone();

    let files: [(&str, String); 5] = [
        ("app.json", serde_json::to_string_pretty(&app.to_json())? + "\n"),
        ("hur.json", serde_json::to_string_pretty(&app.hur_json())? + "\n"),
        ("run.sh", RUN_SH.to_string()),
        ("README.md", fill(TPL_README, &app)),
        ("SKILL.md", fill(TPL_SKILL, &app)),
    ];
    let mut wrote: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for (name, body) in files {
        let p = dir.join(name);
        if p.exists() && !a.force {
            kept.push(name.to_string());
            continue;
        }
        std::fs::write(&p, body).with_context(|| format!("写不了 {}", p.display()))?;
        if name.ends_with(".sh") {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
        wrote.push(name.to_string());
    }

    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "dir": dir.display().to_string(),
            "name": app.name,
            "wrote": wrote, "kept": kept,
            "console": app.listen,
        }))?);
        return Ok(());
    }
    println!("✅ 舱已就位：{}", dir.display());
    for f in &wrote {
        println!("   写了 {f}");
    }
    if !kept.is_empty() {
        println!("   留着没动（已存在）：{}（要覆盖加 --force）", kept.join(" / "));
    }
    println!("\n下一步：");
    println!("   ncc app doctor            逐项自检（缺什么、下一步跑什么，都会打出来）");
    println!("   ncc app up                起这个舱（本机控制台 {}）", app.listen);
    println!("\n舱里的东西装在哪：内容住节点（kb / mem / ckpt），互联走平台（分享 / 网关）。");
    println!("删掉这个目录**不会**删掉节点上的内容 —— 那是你的数据。");
    Ok(())
}

fn fill(tpl: &str, app: &AppManifest) -> String {
    tpl.replace("{{name}}", &app.name)
        .replace("{{title}}", &app.title)
        .replace("{{namespace}}", if app.namespace.is_empty() { "（你的个人命名空间）" } else { &app.namespace })
        .replace("{{listen}}", &app.listen)
        .replace("{{route}}", if app.egress_route.is_empty() { "（未声明）" } else { &app.egress_route })
}

/// 当前身份里的用户名（拿不到就 None —— 没登录时不该因为这个失败）。
fn current_username(cfg: &CliConfig) -> Option<String> {
    let tok = config::token_opt(cfg)?;
    let d = api::get(cfg, "/api/auth/me", Some(&tok)).ok()?;
    let u = d.pointer("/user/username").and_then(|x| x.as_str()).or_else(|| {
        d.pointer("/user/email").and_then(|x| x.as_str()).map(|e| e.split('@').next().unwrap_or(e))
    })?;
    Some(format!("@{}", u.trim_start_matches('@')))
}

/* ============================ doctor / status ============================ */

pub struct DoctorArgs {
    pub dir: Option<String>,
    pub json: bool,
}

pub fn doctor(cfg: &CliConfig, a: &DoctorArgs) -> Result<()> {
    let dir = app_dir(a.dir.as_deref())?;
    let app = AppManifest::load(&dir).unwrap_or_else(|_| AppManifest::default());
    let list = checks(cfg, &app, &dir);
    let errs = list.iter().filter(|c| c.level == Level::Err).count();
    let warns = list.iter().filter(|c| c.level == Level::Warn).count();

    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "dir": dir.display().to_string(),
            "ok": errs == 0, "errors": errs, "warnings": warns,
            "checks": list.iter().map(|c| json!({
                "level": format!("{:?}", c.level).to_lowercase(),
                "title": c.title, "detail": c.detail, "next": c.next,
            })).collect::<Vec<_>>(),
        }))?);
    } else {
        println!("舱自检：{}", dir.display());
        for c in &list {
            println!("  {} {} —— {}", c.icon(), c.title, c.detail);
            if let Some(n) = &c.next {
                println!("      下一步：{n}");
            }
        }
        println!(
            "\n{} 通过 · {} 提醒 · {} 不通",
            list.len() - errs - warns,
            warns,
            errs
        );
        if errs > 0 {
            println!("有 ✗ 的项：照上面的「下一步」做完再跑一次 `ncc app doctor`。");
        }
    }
    if errs > 0 {
        std::process::exit(1);
    }
    Ok(())
}

pub struct StatusArgs {
    pub dir: Option<String>,
    pub json: bool,
}

pub fn status(cfg: &CliConfig, a: &StatusArgs) -> Result<()> {
    let dir = app_dir(a.dir.as_deref())?;
    let (app, note) = match AppManifest::load(&dir) {
        Ok(app) => (app, None),
        Err(e) => (AppManifest::default(), Some(format!("{e:#}"))),
    };
    let list = checks(cfg, &app, &dir);
    // status 只报"现状"，不判生死（退出码 0）—— 判生死是 doctor 的事。
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "dir": dir.display().to_string(),
            "app": app.to_json(),
            "manifestError": note,
            "checks": list.iter().map(|c| json!({
                "level": format!("{:?}", c.level).to_lowercase(),
                "title": c.title, "detail": c.detail,
            })).collect::<Vec<_>>(),
            "consoleUrl": format!("http://{}", app.listen),
        }))?);
        return Ok(());
    }
    println!("舱：{}（{}）", app.title, dir.display());
    if let Some(e) = &note {
        println!("   ⚠ {e}");
    }
    println!(
        "   命名空间 {} · 入口 {} · 内容 kb={} mem={} ckpt={} · 出口 {}",
        if app.namespace.is_empty() { "（默认）" } else { &app.namespace },
        app.entry_mode,
        if app.state_kb_namespace.is_empty() { "默认" } else { &app.state_kb_namespace },
        app.mem_subject,
        app.ckpt_label,
        if app.egress_route.is_empty() { "未声明" } else { &app.egress_route },
    );
    println!("   控制台 {}", app.listen);
    let bad: Vec<&Check> = list.iter().filter(|c| c.level != Level::Ok).collect();
    if bad.is_empty() {
        println!("   自检：全过");
    } else {
        println!("   自检 {} 项要处理：", bad.len());
        for c in bad {
            println!("     {} {} —— {}", c.icon(), c.title, c.detail);
            if let Some(n) = &c.next {
                println!("        下一步：{n}");
            }
        }
    }
    Ok(())
}

/* ============================ up：常驻 ============================ */

pub struct UpArgs {
    pub dir: Option<String>,
    pub port: Option<u16>,
}

pub fn up(cfg: &CliConfig, a: &UpArgs) -> Result<()> {
    let dir = app_dir(a.dir.as_deref())?;
    let mut app = AppManifest::load(&dir)?;
    if let Some(p) = a.port {
        app.listen = format!("127.0.0.1:{p}");
    }
    if !app.listen_is_loopback() {
        bail!(
            "控制台只允许监听本机（{} 不是 loopback）。\n\
             把这个舱的界面暴露到网络上没有任何好处：要给别人看，就发一条**快照链接**\
             （控制台里的「生成快照链接」，对方不用登录、还能带访问 key）。",
            app.listen
        );
    }

    // 启动前先把关键项过一遍 —— 起得来但读不到内容的舱，等于骗人。
    let list = checks(cfg, &app, &dir);
    let errs: Vec<&Check> = list.iter().filter(|c| c.level == Level::Err).collect();
    let warns: Vec<&Check> = list.iter().filter(|c| c.level == Level::Warn).collect();
    if !errs.is_empty() {
        println!("起不来：有 {} 项不通 ——", errs.len());
        for c in &errs {
            println!("  ✗ {} —— {}", c.title, c.detail);
            if let Some(n) = &c.next {
                println!("      下一步：{n}");
            }
        }
        bail!("先修好上面的项，或 `ncc app doctor` 看全量清单");
    }

    let token = config::token_opt(cfg);
    let listener = TcpListener::bind(&app.listen).with_context(|| format!("监听不了 {}", app.listen))?;

    println!("NCC 舱已起：{}（{}）", app.title, app.name);
    println!("   控制台  http://{}/", app.listen);
    println!("   内容    kb={} · mem={} · ckpt={}", 
        if app.state_kb_namespace.is_empty() { "默认" } else { &app.state_kb_namespace },
        app.mem_subject, app.ckpt_label);
    println!("   入口    {}（{}）", app.entry_mode, app.entry_packages.join(" / "));
    for c in &warns {
        println!("   △ {} —— {}（下一步：{}）", c.title, c.detail, c.next.clone().unwrap_or_default());
    }
    println!("   边界    内容住节点、互联走平台；控制面只看摘要与授权关系，拿不到业务数据。");

    // 出口绑了才起心跳与上报（没绑也能跑：审计只落本地）。
    match gateway::load_config_pub() {
        Ok(gw) if !gw.gateway_token.trim().is_empty() && !gw.api_server.trim().is_empty() => {
            let hb = if gw.heartbeat_sec == 0 { crate::gwreport::DEFAULT_HEARTBEAT_SEC } else { gw.heartbeat_sec };
            let rp = if gw.report_sec == 0 { crate::gwreport::DEFAULT_REPORT_SEC } else { gw.report_sec };
            println!("   出口    {}（网关 {} · 心跳 {hb}s · 上报 {rp}s）", gw.api_server, gw.gateway_id);
            crate::gwreport::spawn(config::load(), hb, rp);
        }
        _ => println!("   出口    未绑定网关 —— 模型调用不经过 NCC（`ncc gateway bind` 可接入）"),
    }
    println!("   Ctrl+C 停止。**删掉本目录不会删节点上的内容**（那是你的数据）。");

    let share = resolve_share(cfg, &app);
    match &share {
        Ok((name, _)) => println!("   分享    目标「{name}」（内容在节点、分享在平台 —— 舱两侧都记着）"),
        Err(why) => println!("   分享    不可用：{why}"),
    }
    let shared = Console { app, dir, cfg: cfg.clone(), token, share };
    httpsrv::serve(listener, "[ncc-app]", 1 << 20, move |req| handle_console(&shared, req))
}

/* ============================ 控制台（本机 HTTP） ============================ */

struct Console {
    app: AppManifest,
    dir: PathBuf,
    /// 内容这一侧（当前目标：节点）。
    cfg: CliConfig,
    token: Option<String>,
    /// 分享这一侧（`app.json` 的 `share.target`：云端）。取不到就带着原因，面板照实说。
    share: Result<(String, CliConfig), String>,
}

/// 按名字取一份指向该目标的配置（`ncc target list` 里的名字）。
fn cfg_named(cfg: &CliConfig, name: &str) -> Option<CliConfig> {
    let t = cfg.by_name(name)?.clone();
    let mut c = cfg.clone();
    c.current = Some(name.to_string());
    c.targets.insert(name.to_string(), t);
    Some(c)
}

/// 快照页里那个占位符：本机控制台每次请求换成一个随机 nonce（这样不必开 `unsafe-inline`），
/// 传到云端的快照里换成空串（分享页由平台的 sandbox 放行脚本）。
const NONCE_PLACEHOLDER: &str = "{{NONCE}}";
/// 快照注入锚点（模板里就写着这一行注释）—— 引擎靠它把数据塞进那份自包含 HTML。
const SNAPSHOT_MARKER: &str = "<!--SNAPSHOT-->";

/// 解析「分享那一侧」的目标（`app.json` 的 `share.target`）。
///
/// 舱一定要记着两个目标：内容住节点（当前目标），分享在云端（这个）。取不到就把原因带出去，
/// 面板照实显示 —— 而不是让「生成快照」按钮点了报错。
fn resolve_share(cfg: &CliConfig, app: &AppManifest) -> std::result::Result<(String, CliConfig), String> {
    let name = app.share_target.trim();
    if name.is_empty() || name == "self" {
        return Ok((cfg.current_name(), cfg.clone()));
    }
    match cfg_named(cfg, name) {
        Some(c) => Ok((name.to_string(), c)),
        None => Err(format!(
            "app.json 里 share.target=「{name}」，但本机没有这个目标（现在有的：{}）。加一个目标：用 `ncc --base <云端地址> …` 跑一条命令，或在 `ncc target list` 里挑一个名字填进 app.json",
            if cfg.targets.is_empty() { "（一个都没有）".to_string() } else { cfg.targets.keys().cloned().collect::<Vec<_>>().join(" / ") }
        )),
    }
}

/// 随机 nonce（不引依赖：优先 /dev/urandom，退化到时间 + pid）。
fn nonce() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut got = false;
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if f.read_exact(&mut buf).is_ok() {
                h.update(buf);
                got = true;
            }
        }
    }
    if !got {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        h.update(format!("{t}/{}", std::process::id()).as_bytes());
    }
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// 读一块内容：**能读就给数据，读不到就说清为什么**（不给空列表让人误以为"没有"）。
fn panel(cfg: &CliConfig, token: Option<&str>, cap: &str, path: &str, key: &str) -> Value {
    let meta = capability::probe(cfg);
    if meta.has(cap) == Some(false) {
        return json!({
            "available": false,
            "reason": format!("这个目标（{}）没声明 `{cap}` 能力 —— 内容住 ncc-registry 节点，切过去就有了", cfg.base_url()),
            "items": [],
        });
    }
    match api::get(cfg, path, token) {
        Ok(d) => {
            let mut items = d.get(key).cloned().unwrap_or(json!([]));
            // 列表接口的字段名各不一样，这里统一成 items（页面只认这一种）
            if key != "items" {
                items = d.get(key).cloned().unwrap_or(json!([]));
            }
            json!({ "available": true, "items": items, "total": d.get("total").cloned().unwrap_or(json!(0)) })
        }
        Err(e) => json!({
            "available": false,
            "reason": format!("读不到：{e:#}"),
            "items": [],
        }),
    }
}

fn handle_console(s: &Console, req: &Req) -> Resp {
    let path = req.path.split('?').next().unwrap_or("/").to_string();
    let method = req.method.to_uppercase();
    if method != "GET" && method != "POST" {
        return Resp::json(405, &json!({ "error": { "code": "method_not_allowed", "message": "只支持 GET/POST" } }));
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => Resp::text(200, "ok"),
        ("GET", "/") | ("GET", "/index.html") => {
            // 这个页面会渲染**用户自己的内容**（画布标题、记忆值），所以不给自己开后门：
            // 内联脚本用一次性 nonce 放行，别的外域一律不许（connect-src 'self'）。
            let n = nonce();
            Resp {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: CONSOLE_HTML.replace(NONCE_PLACEHOLDER, &n).into_bytes(),
                extra: vec![(
                    "content-security-policy".into(),
                    format!(
                        "default-src 'self'; script-src 'nonce-{n}'; style-src 'unsafe-inline'; img-src data:; connect-src 'self'; base-uri 'none'; form-action 'none'"
                    ),
                )],
            }
        }
        ("GET", "/api/status") => Resp::json(200, &console_status(s)),
        ("GET", "/api/canvas") => {
            let ns = if s.app.state_kb_namespace.is_empty() {
                String::new()
            } else {
                format!("&namespace={}", api::urlenc(&s.app.state_kb_namespace))
            };
            let p = panel(&s.cfg, s.token.as_deref(), "kb", &format!("/api/kb?size=50{ns}"), "docs");
            Resp::json(200, &p)
        }
        ("GET", "/api/memory") => {
            let p = panel(
                &s.cfg,
                s.token.as_deref(),
                "mem",
                &format!("/api/mem?limit=50&subject={}", api::urlenc(&s.app.mem_subject)),
                "memories",
            );
            Resp::json(200, &p)
        }
        ("GET", "/api/checkpoints") => {
            let p = panel(&s.cfg, s.token.as_deref(), "ckpt", "/api/ckpt?size=50", "checkpoints");
            Resp::json(200, &p)
        }
        ("GET", "/api/shares") => Resp::json(200, &shares_of(s)),
        ("POST", "/api/share") => share_snapshot(s, req),
        _ => Resp::json(404, &json!({ "error": { "code": "not_found", "message": format!("没有这个面：{path}") } })),
    }
}

fn console_status(s: &Console) -> Value {
    let meta = capability::probe(&s.cfg);
    let gw = gateway::load_config_pub().unwrap_or_else(|_| gateway::GatewayConfig::default());
    let bound = !gw.gateway_token.trim().is_empty() && !gw.api_server.trim().is_empty();
    json!({
        "app": {
            "name": s.app.name, "title": s.app.title, "namespace": s.app.namespace,
            "dir": s.dir.display().to_string(), "spec": APP_SPEC,
            "entry": { "mode": s.app.entry_mode, "packages": s.app.entry_packages },
        },
        "target": {
            "base": s.cfg.base_url(), "name": s.cfg.current_name(),
            "product": meta.product, "kind": meta.kind, "version": meta.version,
            "capabilities": meta.capabilities, "note": meta.note,
        },
        "about": meta.about,
        "credential": {
            "loggedIn": s.token.is_some(),
        },
        "gateway": {
            "bound": bound,
            "route": s.app.egress_route,
            "id": if bound { gw.gateway_id.clone() } else { String::new() },
            "apiServer": if bound { gw.api_server.clone() } else { String::new() },
        },
        "state": {
            "kb": s.app.state_kb_namespace, "memory": s.app.mem_subject, "checkpoints": s.app.ckpt_label,
        },
        "checkedAt": gateway::now_iso(),
        "boundary": "控制面只有摘要与授权关系；内容与逐条审计留在节点/网关本机。",
    })
}

/// 「给别人看」在这两个世界里**语义不同**，所以这里做适配而不是硬套：
///
/// - **云端（hub）**：`POST /api/share` 上传一张自包含 HTML → 拿到一条**带 key 的链接**
///   （对方匿名打开，看到只读快照）。这是舱能直接用的那条。
/// - **内网节点（node）**：`POST /api/shares` 是把**某条已发布的制品**临时放行（限次/限时，
///   撤销即失效）—— 它需要 `ref`，舱里没有"制品"这个概念，所以这里只**列**已有的分享，
///   并给出该跑什么命令。
/// 「给别人看」在**平台**那一侧（内容在节点这一侧）。两个世界的分享语义也不同：
///
/// - **云端（hub）**：`POST /api/share` 上传自包含 HTML → 一条**带 key 的链接**（对方匿名打开）。
/// - **内网节点（node）**：`POST /api/shares` 是把**某条已发布制品**临时放行（限次/限时）——
///   它要 `ref`，舱里没有"制品"这个概念，所以这里只列已有的并给出该跑的命令。
fn shares_of(s: &Console) -> Value {
    let (name, cfg) = match &s.share {
        Ok(x) => x,
        Err(why) => {
            return json!({ "available": false, "reason": why.clone(), "items": [], "snapshot": false })
        }
    };
    let meta = capability::probe(cfg);
    if meta.has("share") == Some(false) {
        return json!({
            "available": false,
            "reason": format!("分享目标「{name}」（{}）没声明 `share` 能力", cfg.base_url()),
            "items": [],
            "snapshot": false,
        });
    }
    let hub = meta.kind == "hub";
    let token = config::token_opt(cfg);
    let path = if hub { "/api/share" } else { "/api/shares?mine=1" };
    match api::get(cfg, path, token.as_deref()) {
        Ok(d) => {
            let items: Vec<Value> = d
                .get("shares")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            if hub {
                                json!({
                                    "title": x.get("title").cloned().unwrap_or(json!("")),
                                    "url": x.get("url").cloned().unwrap_or(json!("")),
                                    "hasPassword": x.get("hasPassword").cloned().unwrap_or(json!(false)),
                                    "views": x.get("views").cloned().unwrap_or(json!(0)),
                                    "at": x.get("createdAt").cloned().unwrap_or(json!("")),
                                })
                            } else {
                                let art = x.get("artifact").cloned().unwrap_or(json!({}));
                                json!({
                                    "title": format!(
                                        "{} · {}",
                                        x.get("label").and_then(|v| v.as_str()).unwrap_or("制品分享"),
                                        art.get("ref").and_then(|v| v.as_str()).unwrap_or("")
                                    ),
                                    "url": x.get("link").and_then(|v| v.as_str()).unwrap_or(""),
                                    "hasPassword": false,
                                    "views": x.pointer("/uses/used").and_then(|v| v.as_i64()).unwrap_or(0),
                                    "at": x.get("createdAt").cloned().unwrap_or(json!("")),
                                })
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "available": true,
                "target": name,
                "kind": meta.kind,
                "snapshot": hub,
                "items": items,
                "total": items.len(),
                "note": if hub {
                    "生成的是「只读快照」：对方打开链接看到当前内容的静态拷贝，不需要账号；带 key 的链接要把 key 一起发给他。"
                } else {
                    "分享目标「{name}」是内网节点：它的分享是制品级的（限次/限时、可撤销），要 ref。舱快照请把 app.json 的 share.target 指向云端目标（如 hub），或直接跑 ncc registry share create @ns/slug --uses 1 --expires 7"
                },
            })
        }
        Err(e) => json!({ "available": false, "reason": format!("读不到：{e:#}"), "items": [], "snapshot": false }),
    }
}

/// 生成一张**只读快照**并上传成分享链接。
///
/// 这是控制台里唯一的写动作，而且它只做一件事：把"现在这份内容"变成一张自包含 HTML。
/// 快照与舱完全解耦 —— 对方拿到的是一张静态页，看不到控制台、更碰不到你的节点。
fn share_snapshot(s: &Console, req: &Req) -> Resp {
    let (name, share_cfg) = match &s.share {
        Ok(x) => x.clone(),
        Err(why) => {
            return Resp::json(400, &json!({ "error": { "code": "no_share_target", "message": why } }))
        }
    };
    let meta = capability::probe(&share_cfg);
    if meta.has("share") == Some(false) {
        return Resp::json(400, &json!({ "error": {
            "code": "capability_missing",
            "message": format!("分享目标「{name}」（{}）没声明 share 能力", share_cfg.base_url()),
        }}));
    }
    if meta.kind != "hub" {
        return Resp::json(400, &json!({ "error": {
            "code": "wrong_target",
            "message": format!(
                "分享目标「{name}」是内网节点：它的分享是**制品级**的（要 ref），舱快照要发到云端。\n\
                 · 把 app.json 的 share.target 改成云端目标名（如 hub）\n\
                 · 或在节点上分享制品：ncc registry share create @ns/slug --uses 1 --expires 7"
            ),
        }}));
    }
    let token = match config::token_opt(&share_cfg) {
        Some(t) => t,
        None => {
            return Resp::json(401, &json!({ "error": { "code": "unauthorized", "message": format!("分享目标「{name}」还没登录：`ncc --target {name} login --email … --password …`") } }))
        }
    };
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
    let key = body.get("key").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if !key.is_empty() && (key.len() < 3 || key.len() > 16) {
        return Resp::json(400, &json!({ "error": { "code": "bad_request", "message": "访问 key 长度 3-16 位" } }));
    }

    // 快照的数据与 /api/* 完全同源：这样"分享出去的"和"我在控制台看到的"不会两样。
    let snap = json!({
        "status": console_status(s),
        "canvas": snapshot_panel(s, "kb", &format!("/api/kb?size=50"), "docs"),
        "memory": snapshot_panel(s, "mem", &format!("/api/mem?limit=50&subject={}", api::urlenc(&s.app.mem_subject)), "memories"),
        "checkpoints": snapshot_panel(s, "ckpt", "/api/ckpt?size=50", "checkpoints"),
        "shares": json!({ "available": false, "reason": "快照是只读的：分享链接不随快照走", "items": [] }),
    });
    // 快照是自包含 HTML（它要被传到云端当分享页），所以这里不带 nonce：平台侧用 sandbox 放行脚本。
    //
    // ⚠️ 注入锚点用模板里那个显式标记，**不要**去 replace(`<script>`) —— 主脚本带上
    // nonce 属性之后那个字符串就不存在了，替换会静默失配，最后上传一份"没有数据的快照"
    // （踩过：链接打得开、内容是空的）。锚点找不到就**报错**，不要产出假快照。
    if !CONSOLE_HTML.contains(SNAPSHOT_MARKER) {
        return Resp::json(500, &json!({ "error": {
            "code": "template_broken",
            "message": format!("控制台模板里找不到快照锚点 {SNAPSHOT_MARKER}（引擎与模板版本不匹配）"),
        }}));
    }
    let html = CONSOLE_HTML
        .replace(NONCE_PLACEHOLDER, "")
        .replace(
            SNAPSHOT_MARKER,
            &format!(
                "<script>window.__NCC_SNAPSHOT__ = {};</script>",
                json_escape_in_script(&snap.to_string())
            ),
        );
    let title = format!("{} · 快照 {}", s.app.title, gateway::now_iso());
    let path = format!(
        "/api/share?title={}&visibility=public{}",
        api::urlenc(&title),
        if key.is_empty() { String::new() } else { format!("&key={}", api::urlenc(&key)) }
    );
    let resp = api::request(
        &share_cfg,
        "POST",
        &path,
        Some(&token),
        None,
        Some(html.as_bytes()),
        &[("Content-Type", "text/html; charset=utf-8")],
    );
    match resp {
        Ok(d) => {
            let url = d.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            println!("[ncc-app] 生成快照分享：{url}（{} 字节）", html.len());
            Resp::json(200, &json!({ "ok": true, "url": url, "key": key, "size": html.len() }))
        }
        Err(e) => Resp::json(502, &json!({ "error": { "code": "upstream", "message": format!("{e:#}") } })),
    }
}

fn snapshot_panel(s: &Console, cap: &str, path: &str, key: &str) -> Value {
    panel(&s.cfg, s.token.as_deref(), cap, path, key)
}

/// 往 `<script>` 里塞 JSON：转义掉 `</script>` 与行分隔符（HTML 里最经典的两种打断）。
fn json_escape_in_script(s: &str) -> String {
    s.replace("</", "<\\/")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/* ============================ export ============================ */

pub struct ExportArgs {
    pub dir: Option<String>,
    pub out: Option<String>,
    pub json: bool,
}

/// 把舱变成一个**可交付目录**：别人拿到它就能部署一份自己的。
///
/// 交付物里带 sha256 —— 收到的人能核对"我拿到的和作者发的是不是同一份"。
/// 要签名/分发到目录，下一步是 `ncc hur sign` + `ncc publish`（本命令不替你发布）。
pub fn export(cfg: &CliConfig, a: &ExportArgs) -> Result<()> {
    let dir = app_dir(a.dir.as_deref())?;
    let app = AppManifest::load(&dir)?;
    let out = match &a.out {
        Some(o) => PathBuf::from(o),
        None => dir.parent().unwrap_or(&dir).join(format!("{}-export", app.name)),
    };
    if out.exists() {
        bail!("{} 已存在（换个 --out，或先删掉它）", out.display());
    }
    std::fs::create_dir_all(&out).with_context(|| format!("建不了 {}", out.display()))?;

    let names = ["app.json", "hur.json", "run.sh", "README.md", "SKILL.md"];
    let mut files: Vec<Value> = Vec::new();
    for n in names {
        let src = dir.join(n);
        if !src.exists() {
            continue;
        }
        let bytes = std::fs::read(&src).with_context(|| format!("读不了 {}", src.display()))?;
        std::fs::write(out.join(n), &bytes)?;
        files.push(json!({ "name": n, "size": bytes.len(), "sha256": sha256_hex(&bytes) }));
    }
    if files.is_empty() {
        bail!("舱目录里没有可交付的东西（先 `ncc app init`）");
    }
    // 控制台也一起交付：别人部署之后能立刻看到同一个界面（模板来自引擎，不读本地文件）
    let console = CONSOLE_HTML.as_bytes().to_vec();
    std::fs::write(out.join("console.html"), &console)?;
    files.push(json!({ "name": "console.html", "size": console.len(), "sha256": sha256_hex(&console) }));

    let meta = capability::probe(cfg);
    let manifest = json!({
        "spec": "ncc-app-export/v1",
        "app": app.to_json(),
        "exportedAt": gateway::now_iso(),
        "exportedBy": cfg.current_name(),
        "nccVersion": env!("CARGO_PKG_VERSION"),
        "target": { "base": cfg.base_url(), "product": meta.product, "capabilities": meta.capabilities },
        "files": files,
        "next": [
            "在目标机器上：`ncc app doctor --dir <目录>` 逐项自检",
            "`ncc app up --dir <目录>` 起舱",
            "要发布给别人：`ncc hur sign` 后 `ncc publish --kind harness --file ./README.md --manifest ./hur.json`",
        ],
        "boundary": "舱是部署，内容住节点：删舱不会删节点上的 kb / mem / ckpt。",
    });
    std::fs::write(out.join("export.json"), serde_json::to_string_pretty(&manifest)? + "\n")?;
    // 惯例：给一个能 `shasum -c` 的清单
    let sums: String = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| format!("{}  {}\n", f["sha256"].as_str().unwrap_or(""), f["name"].as_str().unwrap_or("")))
        .collect();
    std::fs::write(out.join("checksums.txt"), sums)?;

    if a.json {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }
    println!("✅ 已导出：{}", out.display());
    for f in manifest["files"].as_array().unwrap() {
        println!(
            "   {}  {}  {} 字节",
            &f["sha256"].as_str().unwrap_or("")[..16.min(f["sha256"].as_str().unwrap_or("").len())],
            f["name"].as_str().unwrap_or(""),
            f["size"].as_i64().unwrap_or(0)
        );
    }
    println!("\n别人拿到这个目录就能部署一份自己的：");
    println!("   ncc app doctor --dir {}", out.display());
    println!("   ncc app up     --dir {}", out.display());
    println!("\n要发布到目录（先用 `ncc hur sign` 签）：");
    println!("   ncc publish --kind harness --file ./README.md --manifest ./hur.json");
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
