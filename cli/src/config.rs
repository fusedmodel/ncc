// 本地配置 ~/.ncc/config.json
//
// v2：**多目标（target）**。同一台机器上通常同时存在两个世界：
//
//	hub      云端公共目录（ncc.ai）
//	<名字>   若干内网 ncc-registry 节点（本机 / 公司内网 / 实验室）
//
// 每个目标各自一份 base_url 与凭据 —— 在本地节点 `login` 不会再把你从云端挤下线，
// `--base` 也不会再悄悄改掉当前目标。
//
// 兼容：读到 v1 的扁平字段（base_url/token/email/name）就迁移成名为 hub 的目标，
// 并且**保存时继续镜像写回**这些字段，旧版 CLI 仍然能用。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

/// 默认目标名：云端。
pub const DEFAULT_TARGET: &str = "hub";

/// 一个目标（一台 ncc 节点 / 一个云端端点）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Target {
    /// `cloud`（云端公共目录）| `registry`（内网节点）。探测不到自述时的兜底。
    #[serde(default)]
    pub kind: String,
    #[serde(default = "default_base")]
    pub base_url: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// 节点管理凭据（只对 kind=registry 有意义，见 `ncc registry admin`）。
    #[serde(default)]
    pub admin_key: Option<String>,
    #[serde(default)]
    pub admin_secret: Option<String>,
    /// 最近一次探测到的自述（离线时也能提示「这个目标声明了哪些能力」）。
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl Default for Target {
    fn default() -> Self {
        Target {
            kind: String::new(),
            base_url: default_base(),
            token: None,
            email: None,
            name: None,
            admin_key: None,
            admin_secret: None,
            product: None,
            capabilities: Vec::new(),
        }
    }
}

impl Target {
    /// 展示用 kind：没探测过就按 base 猜（ncc.ai = 云端）。
    pub fn kind_label(&self) -> &str {
        if !self.kind.is_empty() {
            return &self.kind;
        }
        if self.base_url.contains("ncc.ai") {
            "cloud"
        } else {
            "registry"
        }
    }

    /// 是不是内网 registry 节点（`ncc registry …` 只在这种目标上跑）。
    pub fn is_node(&self) -> bool {
        matches!(self.kind_label(), "registry" | "node")
    }

    /// 有没有登录会话。
    pub fn logged_in(&self) -> bool {
        self.token.as_deref().map(|t| !t.is_empty()).unwrap_or(false)
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CliConfig {
    /// 当前目标名。
    #[serde(default)]
    pub current: Option<String>,
    /// 全部目标：名字 → 目标。
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,

    // —— v1 兼容镜像：保存时跟随 current 写回，旧版 CLI（只认这些字段）仍可用 ——
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_secret: Option<String>,
}

fn default_base() -> String {
    "http://localhost:8181".to_string()
}

impl CliConfig {
    /// 当前目标（总是有一个；没有就现建）。
    pub fn target(&self) -> &Target {
        let name = self.current_name();
        static EMPTY: std::sync::OnceLock<Target> = std::sync::OnceLock::new();
        self.targets
            .get(&name)
            .unwrap_or_else(|| EMPTY.get_or_init(Target::default))
    }

    pub fn current_name(&self) -> String {
        self.current
            .clone()
            .filter(|c| !c.is_empty())
            .or_else(|| self.targets.keys().next().cloned())
            .unwrap_or_else(|| DEFAULT_TARGET.to_string())
    }

    pub fn target_mut(&mut self) -> &mut Target {
        let name = self.current_name();
        self.targets.entry(name).or_insert_with(Target::default)
    }

    /// 有效 base（`api::request` 用）。
    pub fn base_url(&self) -> String {
        self.target().base_url.clone()
    }

    pub fn by_name(&self, name: &str) -> Option<&Target> {
        self.targets.get(name)
    }

    /// 按 base 找目标名（`--base` 复用同一台机器时用）。
    pub fn name_of_base(&self, base: &str) -> Option<String> {
        let want = base.trim_end_matches('/');
        self.targets
            .iter()
            .find(|(_, t)| t.base_url.trim_end_matches('/') == want)
            .map(|(n, _)| n.clone())
    }

    pub fn names(&self) -> Vec<String> {
        self.targets.keys().cloned().collect()
    }

    /// 所有 kind=registry 的目标名。
    pub fn node_names(&self) -> Vec<String> {
        self.targets
            .iter()
            .filter(|(_, t)| t.is_node())
            .map(|(n, _)| n.clone())
            .collect()
    }
}

/// 用户根目录。
///
/// ⚠️ **必须与 JS 包装算出同一个目录** —— 两边操作的是同一个 `~/.ncc/bin/ncc`。
/// Node 那边用的是 `process.env.HOME || os.homedir()`，而 `os.homedir()` 在 Windows 上
/// 读的是 `USERPROFILE`；如果这里只认 `$HOME`，那么原生 Windows shell（通常没设 HOME）
/// 会算成相对路径 `./.ncc`，于是升级/安装的是**另一份**文件，表现为「升级过了但还是旧版本」。
///
/// `NCC_HOME` 优先级最高：测试时指向临时目录，避免动真实的 `~/.ncc`。
pub fn home_dir() -> PathBuf {
    if let Ok(p) = env::var("NCC_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = env::var(key) {
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from(".")
}

/// `~/.ncc` 目录（配置、二进制、包都在这下面）
pub fn ncc_dir() -> PathBuf {
    home_dir().join(".ncc")
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = env::var("NCC_CONFIG") {
        return PathBuf::from(p);
    }
    ncc_dir().join("config.json")
}

/// 读取配置并做 v1 → v2 迁移。
pub fn load() -> CliConfig {
    let p = config_path();
    let mut cfg = match fs::read_to_string(&p) {
        Ok(raw) => serde_json::from_str::<CliConfig>(&raw).unwrap_or_default(),
        Err(_) => CliConfig::default(),
    };
    migrate(&mut cfg);
    cfg
}

/// v1（扁平字段）→ v2（多目标）。已有 targets 就只补 current。
fn migrate(cfg: &mut CliConfig) {
    if cfg.targets.is_empty() {
        let base = cfg
            .base_url
            .clone()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(default_base);
        let mut t = Target {
            // 默认指向本机 8181 = 平台实例；内网节点由 `ncc target add` 或 `--base` 建。
            kind: "cloud".to_string(),
            base_url: base,
            token: cfg.token.clone().filter(|t| !t.is_empty()),
            email: cfg.email.clone().filter(|e| !e.is_empty()),
            name: cfg.name.clone().filter(|n| !n.is_empty()),
            ..Target::default()
        };
        t.admin_key = cfg.admin_key.clone().filter(|k| !k.is_empty());
        t.admin_secret = cfg.admin_secret.clone().filter(|s| !s.is_empty());
        cfg.targets.insert(DEFAULT_TARGET.to_string(), t);
    }
    if cfg.current.is_none() {
        cfg.current = Some(DEFAULT_TARGET.to_string());
    }
    mirror(cfg);
}

/// 把当前目标镜像回 v1 扁平字段（兼容旧 CLI）。改完目标后应调用它（save 会调）。
pub fn mirror(cfg: &mut CliConfig) {
    let t = cfg.target().clone();
    cfg.base_url = Some(t.base_url);
    cfg.token = t.token;
    cfg.email = t.email;
    cfg.name = t.name;
    cfg.admin_key = t.admin_key;
    cfg.admin_secret = t.admin_secret;
}

pub fn save(cfg: &mut CliConfig) -> anyhow::Result<()> {
    mirror(cfg);
    let p = config_path();
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    let raw = serde_json::to_string_pretty(cfg)?;
    fs::write(&p, raw)?;
    // 这个文件里有 JWT 与（可选的）节点管理 secret：在 Unix 上收紧到 0600。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 切换当前目标（不存在则报错并列出可选）。
pub fn set_current(cfg: &mut CliConfig, name: &str) -> anyhow::Result<()> {
    if !cfg.targets.contains_key(name) {
        let names = cfg.names();
        anyhow::bail!(
            "没有名为 {} 的目标。现有：{}（新建：ncc target add {} --base <URL>）",
            name,
            if names.is_empty() { "（空）".to_string() } else { names.join(" · ") },
            name
        );
    }
    cfg.current = Some(name.to_string());
    Ok(())
}

pub fn require_token(cfg: &CliConfig) -> anyhow::Result<String> {
    cfg.target().token.clone().filter(|t| !t.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "目标 {}（{}）尚未登录，请先运行: ncc login --email <邮箱> --password …\n  或切到已登录的目标：ncc target list / ncc target use <名字>",
            cfg.current_name(),
            cfg.base_url()
        )
    })
}

/// 已登录则返回 token（可选鉴权：公开资源带上登录态可看到更多，如自己的 unlisted 名片）。
pub fn token_opt(cfg: &CliConfig) -> Option<String> {
    cfg.target().token.clone().filter(|t| !t.is_empty())
}

/// 保存登录会话（写进**当前目标**，不影响其它目标）。
pub fn save_session(cfg: &mut CliConfig, token: &str, email: &str, name: &str) -> anyhow::Result<()> {
    {
        let t = cfg.target_mut();
        t.token = Some(token.to_string());
        if !email.is_empty() {
            t.email = Some(email.to_string());
        }
        if !name.is_empty() {
            t.name = Some(name.to_string());
        }
    }
    save(cfg)
}

/// 清掉当前目标的登录态（其它目标不受影响）。
pub fn clear_session(cfg: &mut CliConfig) -> anyhow::Result<()> {
    cfg.target_mut().token = None;
    save(cfg)
}

/// 节点管理凭据（当前目标；可被 `--key/--secret` 或环境变量覆盖）。
pub fn admin_creds(cfg: &CliConfig) -> (Option<String>, Option<String>) {
    let t = cfg.target();
    (
        t.admin_key.clone().filter(|k| !k.trim().is_empty()),
        t.admin_secret.clone().filter(|s| !s.trim().is_empty()),
    )
}

pub fn set_admin_creds(cfg: &mut CliConfig, key: &str, secret: &str) -> anyhow::Result<()> {
    {
        let t = cfg.target_mut();
        t.admin_key = Some(key.to_string());
        t.admin_secret = Some(secret.to_string());
    }
    save(cfg)
}

pub fn clear_admin_creds(cfg: &mut CliConfig) -> anyhow::Result<()> {
    {
        let t = cfg.target_mut();
        t.admin_key = None;
        t.admin_secret = None;
    }
    save(cfg)
}

/// 记下探测到的自述（product / kind / capabilities），离线也能提示。
pub fn remember_meta(cfg: &mut CliConfig, product: &str, kind: &str, caps: &[String]) {
    {
        let t = cfg.target_mut();
        if !product.is_empty() {
            t.product = Some(product.to_string());
        }
        if !kind.is_empty() {
            t.kind = match kind {
                "hub" | "cloud" => "cloud".to_string(),
                _ => "registry".to_string(),
            };
        }
        if !caps.is_empty() {
            t.capabilities = caps.to_vec();
        }
    }
    let _ = save(cfg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_config_migrates_into_a_hub_target() {
        let mut cfg: CliConfig = serde_json::from_str(
            r#"{"base_url":"https://ncc.ai","token":"tok-1","email":"a@b.c","name":"A"}"#,
        )
        .unwrap();
        migrate(&mut cfg);
        assert_eq!(cfg.current_name(), "hub");
        let t = cfg.target();
        assert_eq!(t.base_url, "https://ncc.ai");
        assert_eq!(t.token.as_deref(), Some("tok-1"));
        assert_eq!(t.email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn empty_config_gets_a_hub_target() {
        let mut cfg = CliConfig::default();
        migrate(&mut cfg);
        assert_eq!(cfg.names(), vec!["hub".to_string()]);
        assert_eq!(cfg.base_url(), "http://localhost:8181");
        assert!(!cfg.target().logged_in());
    }

    #[test]
    fn targets_keep_separate_credentials() {
        let mut cfg = CliConfig::default();
        migrate(&mut cfg);
        cfg.targets.insert(
            "office".into(),
            Target { kind: "registry".into(), base_url: "http://10.0.0.5:8282".into(), ..Target::default() },
        );
        set_current(&mut cfg, "office").unwrap();
        save_session(&mut cfg, "node-tok", "me@corp.com", "Me").unwrap();
        assert_eq!(cfg.target().token.as_deref(), Some("node-tok"));
        // 镜像字段跟随当前目标（旧版 CLI 仍能跑）
        assert_eq!(cfg.base_url.as_deref(), Some("http://10.0.0.5:8282"));
        set_current(&mut cfg, "hub").unwrap();
        assert_eq!(cfg.target().token, None, "hub 的会话不应被 office 覆盖");
        assert_eq!(cfg.node_names(), vec!["office".to_string()]);
    }

    #[test]
    fn find_target_by_base_ignores_trailing_slash() {
        let mut cfg = CliConfig::default();
        migrate(&mut cfg);
        cfg.targets.insert(
            "office".into(),
            Target { kind: "registry".into(), base_url: "http://10.0.0.5:8282".into(), ..Target::default() },
        );
        assert_eq!(cfg.name_of_base("http://10.0.0.5:8282/").as_deref(), Some("office"));
        assert_eq!(cfg.name_of_base("http://elsewhere:1").as_deref(), None);
    }
}
