//! 本地配置与路径
//!
//! **两处根，各管一摊（2026-09-25 拍板）**：
//! - `~/.harnessuse/`（`HUR_HOME` 可覆盖）—— 这台机器上的**工具状态**：
//!   `hur.json` 旧的 CLI 配置 · `config.json` 桌面端配置（本 crate 只改写 `packages[]`）·
//!   `keys/` 签名私钥与公钥 · `trusted-keys.json` 信任列表 · `environments.json` 沙箱登记 ·
//!   `security.json` 机器层策略 · `tasks/` 投递任务。
//! - `~/.ncc/packages/`（`NCC_PACKAGES_DIR` 可覆盖）—— **包落点**（`install` / `ncc hur install`）。
//!   工具链属 NCC，客户端只是消费者之一；包与「客户端自用的状态」分开放，才不会被某一个客户端的
//!   目录习惯锁死。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const ENV_HOME: &str = "HUR_HOME";
pub const ENV_REGISTRY: &str = "HUR_REGISTRY";
pub const ENV_TOKEN: &str = "HUR_TOKEN";
/// NCC 用户根目录（与 `ncc` CLI 的 `config::home_dir` 语义一致：**被当作 HOME 用**）
pub const ENV_NCC_HOME: &str = "NCC_HOME";
/// 包落点直接覆盖（与 `ncc install` 的同一个环境变量）
pub const ENV_NCC_PACKAGES: &str = "NCC_PACKAGES_DIR";
pub const CLI_CFG: &str = "hur.json";
pub const DESKTOP_CFG: &str = "config.json";
pub const PACKAGES_DIR: &str = "packages";

pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var(ENV_HOME) {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join(".harnessuse")
}

pub fn ensure_home() -> Result<PathBuf> {
    let h = home();
    std::fs::create_dir_all(&h).with_context(|| format!("创建 {} 失败", h.display()))?;
    // 包落点已经不在这个根下了（见文件头），所以两个都要确保存在
    std::fs::create_dir_all(packages_dir()).with_context(|| format!("创建 {} 失败", packages_dir().display()))?;
    Ok(h)
}

/// NCC 用户根目录：`NCC_HOME` 优先，其次 `HOME` / `USERPROFILE`
/// （Windows 原生 shell 常常没 `HOME`，只读 `$HOME` 会算成相对路径 `./.ncc`）
pub fn ncc_home() -> PathBuf {
    if let Ok(p) = std::env::var(ENV_NCC_HOME) {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from(".")
}

/// `~/.ncc`
pub fn ncc_dir() -> PathBuf {
    ncc_home().join(".ncc")
}

/// **包落点**：`NCC_PACKAGES_DIR` 或 `~/.ncc/packages`。
/// 与顶层 `ncc install` 同一个位置、同一个环境变量 —— 两个入口装出来的包必须能被彼此看到。
pub fn packages_dir() -> PathBuf {
    if let Ok(p) = std::env::var(ENV_NCC_PACKAGES) {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    ncc_dir().join(PACKAGES_DIR)
}

pub fn desktop_config() -> PathBuf {
    home().join(DESKTOP_CFG)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HurCfg {
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub namespace: String,
}

impl HurCfg {
    pub fn effective_registry(&self) -> String {
        if !self.registry.trim().is_empty() {
            return self.registry.trim().trim_end_matches('/').to_string();
        }
        std::env::var(ENV_REGISTRY).unwrap_or_default().trim().trim_end_matches('/').to_string()
    }

    pub fn effective_token(&self) -> String {
        if !self.token.trim().is_empty() {
            return self.token.trim().to_string();
        }
        std::env::var(ENV_TOKEN).unwrap_or_default()
    }
}

pub fn load_cfg() -> HurCfg {
    let p = home().join(CLI_CFG);
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_cfg(cfg: &HurCfg) -> Result<()> {
    ensure_home()?;
    let p = home().join(CLI_CFG);
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(cfg)?))
        .with_context(|| format!("写入 {} 失败", p.display()))?;
    Ok(())
}

/// 读桌面端 `config.json`（不存在则给空对象）
pub fn read_desktop_config() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(desktop_config())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn write_desktop_config(map: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    ensure_home()?;
    let p = desktop_config();
    let v = serde_json::Value::Object(map.clone());
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&v)?))
        .with_context(|| format!("写入 {} 失败", p.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_honours_env() {
        let prev = std::env::var(ENV_HOME).ok();
        std::env::set_var(ENV_HOME, "/tmp/hur-home-test");
        assert_eq!(home(), PathBuf::from("/tmp/hur-home-test"));
        match prev {
            Some(v) => std::env::set_var(ENV_HOME, v),
            None => std::env::remove_var(ENV_HOME),
        }
    }
}
