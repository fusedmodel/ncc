//! 安装：把 `.hur` 或 registry 上的制品装进 `~/.ncc/packages/<id>/`，
//! 并在桌面端 `config.json` 的 `packages[]` 里登记（桌面 Agent 据此展示/启用）。
//!
//! 落点 2026-09-25 从 `~/.harnessuse/packages` 搬到 `~/.ncc/packages`（与顶层 `ncc install` 一致）：
//! 工具链属 NCC，客户端只是消费者之一。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::cfg::{self, packages_dir};
use crate::pack;
use crate::registry::Registry;
use crate::spec::{sha256_file, MANIFEST};

pub const RECORD_FILE: &str = "_install.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub version: String,
    #[serde(default)]
    pub short: String,
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub sha256: String,
    pub path: String,
    #[serde(default)]
    pub installed_at_unix: u64,
    #[serde(default = "truth")]
    pub enabled: bool,
}

fn truth() -> bool {
    true
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Source {
    pub registry: String,
    pub slug: String,
}

/// 给**已经落盘**的包目录补登记（GUI「写入本机 / 打包」后用，不重新解包）。
/// 产物若是 `dist/<id>-<version>.hur` 则记录其 sha256，否则留空。
pub fn register_dir(dir: &Path, source: Option<Source>) -> Result<InstallRecord> {
    let manifest_text = std::fs::read_to_string(dir.join(MANIFEST))
        .with_context(|| format!("{} 里没有 {MANIFEST}", dir.display()))?;
    let pkg = crate::spec::parse(&manifest_text)?;
    let dist = dir.join(crate::spec::DIST).join(format!("{}-{}.hur", pkg.id, pkg.version));
    let sha = if dist.is_file() { sha256_file(&dist)? } else { String::new() };
    let rec = InstallRecord {
        id: pkg.id.clone(),
        name: pkg.name.clone(),
        kind: pkg.kind.clone(),
        version: pkg.version.clone(),
        short: pkg.short.clone(),
        registry: source.as_ref().map(|s| s.registry.clone()).unwrap_or_default(),
        slug: source.as_ref().map(|s| s.slug.clone()).unwrap_or_default(),
        sha256: sha,
        path: dir.to_string_lossy().to_string(),
        installed_at_unix: now_unix(),
        enabled: true,
    };
    std::fs::write(dir.join(RECORD_FILE), format!("{}\n", serde_json::to_string_pretty(&rec)?))?;
    upsert_desktop_package(&rec)?;
    Ok(rec)
}

/// 把 `.hur` 落盘安装（`expected_sha` 有值时先验 sha256 —— 验签输入）
pub fn install_archive(hur: &Path, expected_sha: Option<&str>, source: Option<Source>, force: bool) -> Result<InstallRecord> {
    let sha = sha256_file(hur)?;
    if let Some(expect) = expected_sha {
        let expect = expect.trim().to_ascii_lowercase();
        if !expect.is_empty() && expect != sha {
            bail!("sha256 不匹配：登记 {expect}，实际 {sha}（包被改过或下载损坏，已拒绝安装）");
        }
    }

    let root = packages_dir();
    std::fs::create_dir_all(&root)?;
    let tmp = root.join(format!(".unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (files, id, version) = pack::unpack(hur, &tmp)?;

    // 读包内 hur.json 补充元数据
    let manifest_text = std::fs::read_to_string(tmp.join(MANIFEST))?;
    let pkg = crate::spec::parse(&manifest_text)?;

    let dest = root.join(&id);
    if dest.exists() {
        if !force {
            let _ = std::fs::remove_dir_all(&tmp);
            bail!("已安装 {id}（{}）—— 要覆盖请加 --force", dest.display());
        }
        std::fs::remove_dir_all(&dest).with_context(|| format!("清理旧版本 {} 失败", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("移动到 {} 失败", dest.display()))?;

    let rec = InstallRecord {
        id: pkg.id.clone(),
        name: pkg.name.clone(),
        kind: pkg.kind.clone(),
        version: if version.is_empty() { pkg.version.clone() } else { version },
        short: pkg.short.clone(),
        registry: source.as_ref().map(|s| s.registry.clone()).unwrap_or_default(),
        slug: source.as_ref().map(|s| s.slug.clone()).unwrap_or_default(),
        sha256: sha,
        path: dest.to_string_lossy().to_string(),
        installed_at_unix: now_unix(),
        enabled: true,
    };
    std::fs::write(dest.join(RECORD_FILE), format!("{}\n", serde_json::to_string_pretty(&rec)?))?;
    upsert_desktop_package(&rec)?;
    let _ = files;
    Ok(rec)
}

/// 从 registry 安装：`@ns/slug` 或 `ns/slug`
pub fn install_remote(reg: &Registry, spec_ref: &str, force: bool) -> Result<InstallRecord> {
    let (ns, slug) = split_ref(spec_ref)?;
    // 先看元数据：确认这是 hur 制品（不是普通 api/skill 条目）
    let meta = reg.item(ns, slug)?;
    let meta = meta.get("item").cloned().unwrap_or(meta);
    let declared = meta
        .get("manifest")
        .and_then(|m| m.get("spec"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if declared != crate::spec::PKG_SPEC {
        eprintln!(
            "提醒：条目 manifest.spec 是「{}」而不是 {}，将按 hur 包结构解包",
            if declared.is_empty() { "（未声明）" } else { declared },
            crate::spec::PKG_SPEC
        );
    }
    let info = reg.download_info(ns, slug)?;
    let url = info.get("url").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    let sha = info.get("sha256").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    if url.is_empty() {
        bail!("条目没有可下载地址（storage.url 为空）");
    }
    let bytes = reg.fetch_bytes(&url)?;
    if !sha.is_empty() && crate::spec::sha256_hex(&bytes) != sha.to_ascii_lowercase() {
        bail!("下载内容 sha256 与 registry 登记不一致，已拒绝安装");
    }

    let tmp_dir = std::env::temp_dir().join(format!("hur-dl-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;
    let tmp_file = tmp_dir.join(format!("{}.hur", slug));
    std::fs::write(&tmp_file, &bytes)?;
    let rec = install_archive(
        &tmp_file,
        Some(&sha),
        Some(Source { registry: reg.base.clone(), slug: format!("{}/{}", ns.trim_end_matches('/'), slug) }),
        force,
    )?;
    let _ = std::fs::remove_file(&tmp_file);
    Ok(rec)
}

pub fn split_ref(s: &str) -> Result<(&str, &str)> {
    let s = s.trim();
    if let Some((ns, slug)) = s.split_once('/') {
        if ns.is_empty() || slug.is_empty() {
            bail!("引用要写成 `@命名空间/slug`，例如 @me/my-agent");
        }
        return Ok((ns.trim_end_matches('/'), slug));
    }
    bail!("引用要写成 `@命名空间/slug`（也可以直接 `hur install ./dist/x.hur`）")
}

/// 扫描 packages/ 下的安装记录
pub fn list_installed() -> Vec<InstallRecord> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(packages_dir()) {
        for e in rd.flatten() {
            let p = e.path().join(RECORD_FILE);
            if let Ok(text) = std::fs::read_to_string(p) {
                if let Ok(r) = serde_json::from_str::<InstallRecord>(&text) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by(|a, b| b.installed_at_unix.cmp(&a.installed_at_unix));
    out
}

pub fn uninstall(id: &str) -> Result<()> {
    let dest = packages_dir().join(id);
    if !dest.exists() {
        bail!("没有安装 {id}");
    }
    std::fs::remove_dir_all(&dest).with_context(|| format!("删除 {} 失败", dest.display()))?;
    drop_desktop_package(id)?;
    Ok(())
}

/// 在桌面端 config.json 里登记/更新（只动 packages[] 字段）
fn upsert_desktop_package(rec: &InstallRecord) -> Result<()> {
    let mut map = cfg::read_desktop_config();
    let list = map
        .entry("packages".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !list.is_array() {
        *list = serde_json::Value::Array(Vec::new());
    }
    let arr = list.as_array_mut().unwrap();
    arr.retain(|v| v.get("id").and_then(|s| s.as_str()) != Some(rec.id.as_str()));
    arr.insert(0, serde_json::to_value(rec)?);
    cfg::write_desktop_config(&map)
}

fn drop_desktop_package(id: &str) -> Result<()> {
    let mut map = cfg::read_desktop_config();
    if let Some(arr) = map.get_mut("packages").and_then(|v| v.as_array_mut()) {
        arr.retain(|v| v.get("id").and_then(|s| s.as_str()) != Some(id));
    }
    cfg::write_desktop_config(&map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ref_requires_ns_slug() {
        assert!(split_ref("me/agent").is_ok());
        assert!(split_ref("@me/agent").is_ok());
        assert!(split_ref("agent").is_err());
    }

    #[test]
    fn rejects_sha_mismatch() {
        let dir = std::env::temp_dir().join(format!("hur-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.hur");
        std::fs::write(&f, b"not a zip").unwrap();
        let err = install_archive(&f, Some("deadbeef"), None, true).unwrap_err().to_string();
        assert!(err.contains("sha256 不匹配"), "{err}");
    }
}
