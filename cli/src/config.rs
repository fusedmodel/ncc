// 本地配置 ~/.ncc/config.json（与旧 TS 版同路径同结构，无缝迁移）
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct CliConfig {
    #[serde(default = "default_base")]
    pub base_url: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

fn default_base() -> String {
    "http://localhost:8181".to_string()
}

impl Default for CliConfig {
    fn default() -> Self {
        CliConfig {
            base_url: default_base(),
            token: None,
            email: None,
            name: None,
        }
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

pub fn load() -> CliConfig {
    let p = config_path();
    if let Ok(raw) = fs::read_to_string(&p) {
        if let Ok(cfg) = serde_json::from_str::<CliConfig>(&raw) {
            return cfg;
        }
    }
    CliConfig::default()
}

pub fn save(cfg: &CliConfig) -> anyhow::Result<()> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    let raw = serde_json::to_string_pretty(cfg)?;
    fs::write(&p, raw)?;
    Ok(())
}

pub fn require_token(cfg: &CliConfig) -> anyhow::Result<String> {
    cfg.token.clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow::anyhow!("尚未登录，请先运行: ncc login"))
}

/// 已登录则返回 token（可选鉴权：公开资源带上登录态可看到更多，如自己的 unlisted 名片）。
pub fn token_opt(cfg: &CliConfig) -> Option<String> {
    cfg.token.clone().filter(|t| !t.is_empty())
}
