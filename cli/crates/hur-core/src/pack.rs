//! 打包：`hur build`（依赖锁定）与 `hur pack`（确定性产包）。
//!
//! 确定性：条目按路径排序、压缩方式 Stored、时间戳固定 → 同样内容必得同样 sha256
//! （验签 / 可复现构建 / 去重都靠它）。

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

use crate::spec::{
    content_files, read_pkg, rel, sha256_file, sha256_hex, HurLock, LockedDep, DIST, LOCK,
    LOCK_SPEC, MANIFEST,
};

/// 从任意子目录向上找包根（含 hur.json 的目录）
pub fn find_root(start: &Path) -> Result<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(MANIFEST).exists() {
            return Ok(cur);
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => bail!("向上找不到 {MANIFEST}（当前不在 hur 包内）"),
        }
    }
}

/// 生成/刷新 hur.lock：本地依赖取 sha256，远程依赖先登记为未解析
pub fn build_lock(dir: &Path) -> Result<HurLock> {
    let pkg = read_pkg(dir)?;
    let mut deps: BTreeMap<String, Vec<LockedDep>> = BTreeMap::new();
    let grouped: [(&str, &Vec<String>); 5] = [
        ("agent", &pkg.deps.agent),
        ("harness", &pkg.deps.harness),
        ("kb", &pkg.deps.kb),
        ("mcp", &pkg.deps.mcp),
        ("skill", &pkg.deps.skill),
    ];
    for (kind, list) in grouped {
        if list.is_empty() {
            continue;
        }
        let mut rows = Vec::new();
        for r in list {
            let r = r.trim();
            if r.is_empty() {
                continue;
            }
            let local = crate::spec::is_local_ref(r);
            if local {
                let p = dir.join(r);
                rows.push(LockedDep {
                    r#ref: r.to_string(),
                    resolved: if p.is_dir() { "local-dir".into() } else { "local".into() },
                    sha256: local_dep_hash(dir, r)?,
                });
            } else {
                rows.push(LockedDep {
                    r#ref: r.to_string(),
                    resolved: "unresolved".into(),
                    sha256: None,
                });
            }
        }
        if !rows.is_empty() {
            deps.insert(kind.to_string(), rows);
        }
    }

    let mut files = BTreeMap::new();
    for f in content_files(dir) {
        files.insert(rel(dir, &f), sha256_file(&f)?);
    }

    let lock = HurLock {
        spec: LOCK_SPEC.to_string(),
        package: pkg.id.clone(),
        version: pkg.version.clone(),
        deps,
        files,
    };
    std::fs::write(dir.join(LOCK), format!("{}\n", serde_json::to_string_pretty(&lock)?))
        .with_context(|| "写入 hur.lock 失败")?;
    Ok(lock)
}

/// 本地依赖的规范哈希（文件=文件 sha256；目录=目录内文件逐个哈希后汇总）。
///
/// **唯一实现**：`build_lock()` 与 `dep::inspect()` 共用 —— 否则"锁里的哈希"与
/// "检查时的哈希"会是两套算法，drift 判断就成了猜。
pub fn local_dep_hash(dir: &Path, reference: &str) -> Result<Option<String>> {
    let p = dir.join(reference.trim());
    if p.is_file() {
        return Ok(Some(sha256_file(&p)?));
    }
    if p.is_dir() {
        let mut acc = String::new();
        for f in content_files(&p) {
            acc.push_str(&rel(dir, &f));
            acc.push(':');
            acc.push_str(&sha256_file(&f)?);
            acc.push('\n');
        }
        return Ok(Some(sha256_hex(acc.as_bytes())));
    }
    Ok(None)
}

pub struct PackOutcome {
    pub file: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub entries: Vec<String>,
}

/// 规范字节：把一个工程目录打成 `.hur` 的**内存字节**（与 `pack()` 落盘的内容逐字节一致）。
///
/// 抽出它是为了让**签名/核对**不产生副作用：核对一个目录时不需要先在 `dist/` 落一个产物，
/// 只要把同样的字节算出来跟签名对账即可。
/// `allow_build_lock = false` 时缺 `hur.lock` 直接报错 —— **签名覆盖 lock**，
/// 锁没定下来就不该签，也不该核。
pub fn archive_bytes(dir: &Path, allow_build_lock: bool) -> Result<(Vec<u8>, String)> {
    let pkg = read_pkg(dir)?;
    if !dir.join(LOCK).exists() {
        if !allow_build_lock {
            bail!("目录里没有 hur.lock：先 `hur build`（签名覆盖 hur.lock，锁没定下来就不能签/核）");
        }
        build_lock(dir)?;
    }
    let lock_text = std::fs::read_to_string(dir.join(LOCK))?;

    let mut entries: Vec<(String, Vec<u8>)> = vec![
        (MANIFEST.to_string(), std::fs::read(dir.join(MANIFEST))?),
        (LOCK.to_string(), lock_text.into_bytes()),
    ];
    for f in content_files(dir) {
        entries.push((rel(dir, &f), std::fs::read(&f)?));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        // 固定时间戳（1980-01-01）+ Stored → 内容一致则字节一致
        let stamp = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(|e| anyhow!("时间戳构造失败：{e}"))?;
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(stamp)
            .unix_permissions(0o644);
        for (path, body) in &entries {
            zw.start_file(path.clone(), opts)?;
            zw.write_all(body)?;
        }
        zw.finish()?;
    }
    Ok((buf.into_inner(), format!("{}-{}.hur", pkg.id, pkg.version)))
}

/// 产包：`dist/<id>-<version>.hur` + `.sha256`
pub fn pack(dir: &Path) -> Result<PackOutcome> {
    let (bytes, name) = archive_bytes(dir, true)?;
    let dist = dir.join(DIST);
    std::fs::create_dir_all(&dist)?;
    let out = dist.join(&name);
    std::fs::write(&out, &bytes).with_context(|| format!("写 {} 失败", out.display()))?;

    let sha = sha256_hex(&bytes);
    std::fs::write(
        dist.join(format!("{name}.sha256")),
        format!("{sha}  {name}\n"),
    )?;

    let (_, entries) = archive_entries(dir)?;
    Ok(PackOutcome {
        file: out,
        sha256: sha,
        bytes: bytes.len() as u64,
        entries,
    })
}

/// 打包含哪些条目（人读用；与 `archive_bytes` 同源）
pub fn archive_entries(dir: &Path) -> Result<(usize, Vec<String>)> {
    let mut names: Vec<String> = vec![MANIFEST.to_string(), LOCK.to_string()];
    names.extend(content_files(dir).iter().map(|f| rel(dir, f)));
    names.sort();
    let n = names.len();
    Ok((n, names))
}

/// 解开 `.hur`（拒绝越界路径），返回写入的文件列表
pub fn unpack(hur: &Path, dest: &Path) -> Result<(Vec<String>, String, String)> {
    let f = std::fs::File::open(hur).with_context(|| format!("打开 {} 失败", hur.display()))?;
    let mut zip = zip::ZipArchive::new(f).with_context(|| "不是合法的 .hur（zip）文件")?;
    std::fs::create_dir_all(dest)?;
    let mut written = Vec::new();
    let mut id = String::new();
    let mut version = String::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let raw = e.name().to_string();
        let name = raw.trim_start_matches("./").to_string();
        if name.is_empty() || name.contains("..") || name.starts_with('/') {
            bail!("包内含非法路径「{raw}」");
        }
        if e.is_dir() {
            continue;
        }
        let target = dest.join(&name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut body = Vec::new();
        e.read_to_end(&mut body)?;
        if name == MANIFEST {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
                id = v.get("id").and_then(|s| s.as_str()).unwrap_or_default().to_string();
                version = v.get("version").and_then(|s| s.as_str()).unwrap_or_default().to_string();
            }
        }
        std::fs::write(&target, &body)?;
        written.push(name);
    }
    written.sort();
    if id.is_empty() {
        bail!("包内缺少合法的 {MANIFEST}（没有 id）");
    }
    Ok((written, id, version))
}

pub fn sidecar_sha(hur: &Path) -> Option<String> {
    let p = PathBuf::from(format!("{}.sha256", hur.display()));
    let text = std::fs::read_to_string(p).ok()?;
    text.split_whitespace().next().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tpl::{build_package, files_for, InitInput};

    fn make_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hur-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pkg = build_package(&InitInput {
            kind: "agent".into(),
            name: "Pack Test".into(),
            role: "负责住房事务".into(),
            domain: "hotel".into(),
            short: "PT".into(),
            version: "0.1.0".into(),
            summary: String::new(),
            registry: String::new(),
            namespace: String::new(),
        });
        std::fs::write(dir.join(MANIFEST), serde_json::to_string_pretty(&pkg).unwrap()).unwrap();
        for (rel, body) in files_for(&pkg) {
            let p = dir.join(&rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        dir
    }

    #[test]
    fn lock_lists_files_and_local_deps() {
        let dir = make_project("lock");
        let lock = build_lock(&dir).unwrap();
        assert!(lock.files.contains_key("src/agent.ts"));
        assert_eq!(lock.spec, LOCK_SPEC);
    }

    #[test]
    fn pack_is_deterministic() {
        let dir = make_project("det");
        let a = pack(&dir).unwrap();
        let b = pack(&dir).unwrap();
        assert_eq!(a.sha256, b.sha256, "同样内容两次打包必须同 sha256");
        assert!(a.file.exists());
        assert_eq!(sidecar_sha(&a.file).as_deref(), Some(a.sha256.as_str()));
    }

    #[test]
    fn unpack_rejects_traversal_and_round_trips() {
        let dir = make_project("unpack");
        let out = pack(&dir).unwrap();
        let dest = std::env::temp_dir().join(format!("hur-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let (files, id, version) = unpack(&out.file, &dest).unwrap();
        assert!(files.iter().any(|f| f.as_str() == "hur.json"));
        assert!(id.starts_with("A-hotel-"));
        assert_eq!(version, "0.1.0");
    }
}
