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

pub fn config_path() -> PathBuf {
    if let Ok(p) = env::var("NCC_CONFIG") {
        return PathBuf::from(p);
    }
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".ncc").join("config.json")
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
