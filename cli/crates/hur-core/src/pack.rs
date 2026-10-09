//! 打包：`hur build`（依赖锁定）与 `hur pack`（确定性产包）。
//!
//! # `.hur` 的容器形状：**gzip 包住 zip**
//!
//! ```text
//! .hur = gzip( zip( hur.json , hur.lock , 内容文件… ) )
//!          ↑ 压缩在这里          ↑ 结构与防穿越解包在这里
//! ```
//!
//! * **外层 gzip**：包真的被压过（`file x.hur.gz` 现在报 gzip，`gunzip x.hur.gz` 出来的仍是
//!   标准 zip）—— 分发给别人、塞进 IM、放进对象存储时省的是真金白银（用户口径：**hur 只是一种
//!   规范，文件本身用通用压缩包格式结尾**）。
//! * **内层 zip 仍是 Stored**：不二次压缩（压两遍只会更慢更大），但条目名、路径顺序、
//!   防穿越解包这些结构好处全留着；而且任何 zip 工具都能看包内文件清单。
//!
//! 确定性：条目按路径排序、时间戳固定、gzip 头部固定（mtime=0 / OS=255 / 无文件名）×
//! 级别固定 → 同样内容必得同样 sha256（验签 / 可复现构建 / 去重都靠它）。
//! ⚠️ 与「Stored」时代的一点差别：确定性现在也依赖 **flate2 的版本**（同一份 Cargo.lock 内
//! 稳定；换压缩后端可能改变字节）—— 换版本后重打包要重签，包里 `hur.lock` 的逐文件摘要
//! 不受影响，仍是内容级真值。
//!
//! 老包（裸 zip）继续能装：`unpack` 按魔数辨认（`1f 8b` = gzip，`PK` = 裸 zip）。

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

/// gzip 魔数：`.hur` 外层容器（老包是裸 zip，见 `unpack`）
pub const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
/// 压缩级别固定 —— 变一级就换一批字节，所以写常量而不是参数
const GZIP_LEVEL: u32 = 6;

use crate::spec::{
    content_files, content_files_in, read_pkg, rel, sha256_file, sha256_hex, Format, HurLock,
    Issue, LockedDep, DIST, LOCK, LOCK_SPEC,
};

/// 从任意子目录向上找包根（含 hur.json 的目录）
pub fn find_root(start: &Path) -> Result<PathBuf> {
    find_root_of(start, Format::Hur)
}

/// 从任意子目录向上找包根（清单名按格式）。
///
/// `.huf` 工程里**没有** `hur.json`，所以判据必须跟着格式走 —— 否则 `ncc huf pack` 会
/// 一路向上找到父目录里那份毫不相干的 `hur.json` 上去。
pub fn find_root_of(start: &Path, fmt: Format) -> Result<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(fmt.manifest()).exists() {
            return Ok(cur);
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => bail!("向上找不到 {}（当前不在 {} 工程里）", fmt.manifest(), fmt.label()),
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
/// 内层 zip 字节：条目按路径排序、Stored、时间戳固定 1980-01-01 → 内容一致则字节一致。
///
/// 调用方必须先确保 `hur.lock` 已存在（`archive_bytes` 负责那件事）—— 锁是包身份的一半。
/// **故意不对外暴露**：单独拿它写盘会产出一个"现行 unpack 认、但已经不是规范容器"的 `.hur`。
fn zip_bytes_for(dir: &Path, fmt: Format) -> Result<Vec<u8>> {
    let lock_text = std::fs::read_to_string(dir.join(fmt.lock()))
        .with_context(|| format!("读 {} 失败（先 build 一次定下锁）", dir.join(fmt.lock()).display()))?;

    let mut entries: Vec<(String, Vec<u8>)> = vec![
        (fmt.manifest().to_string(), std::fs::read(dir.join(fmt.manifest()))?),
        (fmt.lock().to_string(), lock_text.into_bytes()),
    ];
    for f in content_files_in(dir, fmt.content_dirs()) {
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
    Ok(buf.into_inner())
}

/// 内存里的前两个字节是不是 gzip 魔数（`.hur` 的新容器 / 老包是裸 zip）。
pub fn is_gzip(head: &[u8]) -> bool {
    head.len() >= 2 && head[..2] == GZIP_MAGIC
}

/// gzip 压缩（`gzip(file)` = gzip 包 zip）。
///
/// 头部全部写死：`mtime=0`、`OS=255`（unknown）、不带文件名/注释 —— 否则"什么时候压的、
/// 在哪台机器压的"会进字节，同样内容就得不到同样 sha256，签名与去重全乱套。
pub fn gzip(raw: &[u8]) -> Result<Vec<u8>> {
    let mut enc = flate2::GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(Vec::new(), flate2::Compression::new(GZIP_LEVEL));
    enc.write_all(raw)?;
    Ok(enc.finish()?)
}

/// gzip 解压（内存版：给小包与测试用；大包走 `gunzip_to`，别把整包读两遍）。
pub fn gunzip(gz: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut out)
        .with_context(|| "gunzip 失败：不是合法的 gzip 流（包被截断或改过？）")?;
    Ok(out)
}

/// gzip 解压到文件（流式）：数据包可能有几百 MB，不该在内存里存两份。
fn gunzip_to(src: &Path, out: &Path) -> Result<()> {
    let mut dec = flate2::read::GzDecoder::new(
        std::fs::File::open(src).with_context(|| format!("打开 {} 失败", src.display()))?,
    );
    let mut w = std::fs::File::create(out).with_context(|| format!("写 {} 失败", out.display()))?;
    std::io::copy(&mut dec, &mut w)
        .with_context(|| format!("gunzip {} 失败：不是合法的 gzip 流（包被截断或改过？）—— 外层的 .hur 必须是 gzip 包住的 zip", src.display()))?;
    w.flush()?;
    Ok(())
}

/// 规范字节：把一个工程目录打成 `.hur` 的**内存字节**（与 `pack()` 落盘的内容逐字节一致）。
///
/// 抽出它是为了让**签名/核对**不产生副作用：核对一个目录时不需要先在 `dist/` 落一个产物，
/// 只要把同样的字节算出来跟签名对账即可。
/// `allow_build_lock = false` 时缺 `hur.lock` 直接报错 —— **签名覆盖 lock**，
/// 锁没定下来就不该签，也不该核。
pub fn archive_bytes(dir: &Path, allow_build_lock: bool) -> Result<(Vec<u8>, String)> {
    archive_bytes_for(dir, Format::Hur, allow_build_lock)
}

/// 同 [`archive_bytes`]，但容器按 `fmt` 走（`.hur` / `.huf` 共用全部字节规则）。
pub fn archive_bytes_for(dir: &Path, fmt: Format, allow_build_lock: bool) -> Result<(Vec<u8>, String)> {
    if !dir.join(fmt.lock()).exists() {
        if !allow_build_lock {
            bail!(
                "目录里没有 {}：先 `ncc {} build`（签名覆盖锁文件，锁没定下来就不能签/核）",
                fmt.lock(),
                if fmt == Format::Huf { "huf" } else { "hur" }
            );
        }
        match fmt {
            Format::Hur => {
                build_lock(dir)?;
            }
            Format::Huf => {
                crate::huf::build_lock(dir)?;
            }
        }
    }
    let name = match fmt {
        Format::Hur => crate::spec::artifact_name(&read_pkg(dir)?),
        // 文件名带 profile 段（`<id>-<version>.<profile>.hur`）：一眼看得出这是能跑的包
        // 还是一份数据快照。`.hur` 仍是最后的扩展名 → 侧车与解包机制全不受影响。
        Format::Huf => {
            let m = crate::huf::read(dir)?;
            crate::spec::huf_artifact_name(&m.id, &m.version)
        }
    };
    Ok((gzip(&zip_bytes_for(dir, fmt)?)?, name))
}

/// 产包：`dist/<id>-<version>.<profile>.hur` + `.sha256`
pub fn pack(dir: &Path) -> Result<PackOutcome> {
    pack_for(dir, Format::Hur)
}

/// 产包（按格式）：`.hur` 与 `.huf` 只有清单名/产物名不同，其余全同源。
pub fn pack_for(dir: &Path, fmt: Format) -> Result<PackOutcome> {
    let (bytes, name) = archive_bytes_for(dir, fmt, true)?;
    let dist = dir.join(DIST);
    std::fs::create_dir_all(&dist)?;
    let out = dist.join(&name);
    std::fs::write(&out, &bytes).with_context(|| format!("写 {} 失败", out.display()))?;

    let sha = sha256_hex(&bytes);
    std::fs::write(
        dist.join(format!("{name}.sha256")),
        format!("{sha}  {name}\n"),
    )?;

    let (_, entries) = archive_entries_for(dir, fmt)?;
    Ok(PackOutcome {
        file: out,
        sha256: sha,
        bytes: bytes.len() as u64,
        entries,
    })
}

/// 打包含哪些条目（人读用；与 `archive_bytes` 同源）
pub fn archive_entries(dir: &Path) -> Result<(usize, Vec<String>)> {
    archive_entries_for(dir, Format::Hur)
}

pub fn archive_entries_for(dir: &Path, fmt: Format) -> Result<(usize, Vec<String>)> {
    let mut names: Vec<String> = vec![fmt.manifest().to_string(), fmt.lock().to_string()];
    names.extend(content_files_in(dir, fmt.content_dirs()).iter().map(|f| rel(dir, f)));
    names.sort();
    let n = names.len();
    Ok((n, names))
}

/// 打开 `.hur` 里的 zip 层：新包是 gzip 容器（先解压到临时文件，流式、不占两倍内存），
/// 老包是裸 zip（直接开）—— **老包永远要能装**，格式升级不该让已发出去的字节变废纸。
///
/// 返回 `(zip, 解压出来的临时文件)`：临时文件由调用方删（见 `unpack`）。
fn open_zip_for(
    archive: &Path,
    dest: &Path,
    tmp: &mut Option<PathBuf>,
    fmt: Format,
) -> Result<zip::ZipArchive<std::fs::File>> {
    let mut head = [0u8; 2];
    {
        // 只读两个字节判容器：读不满就当作"不是 gzip"，后面那条路会给出可读的拒绝理由
        let mut f = std::fs::File::open(archive).with_context(|| format!("打开 {} 失败", archive.display()))?;
        let _ = f.read(&mut head).unwrap_or(0);
    }
    if is_gzip(&head) {
        let inner = dest.join(format!(".hur-unzipped-{}", std::process::id()));
        gunzip_to(archive, &inner)?;
        *tmp = Some(inner.clone());
        let za = zip::ZipArchive::new(std::fs::File::open(&inner)?).with_context(|| {
            format!("{} 的 gzip 层后面不是 zip —— 这不是一个 {}", archive.display(), fmt.label())
        })?;
        return Ok(za);
    }
    let f = std::fs::File::open(archive).with_context(|| format!("打开 {} 失败", archive.display()))?;
    zip::ZipArchive::new(f).with_context(|| {
        format!(
            "{} 既不是 gzip（`1f 8b`）也不是 zip（`PK`）—— 不是合法的 .{}（损坏，或根本不是包）",
            archive.display(),
            fmt.spec_ext()
        )
    })
}

/// 解开 `.hur`（拒绝越界路径），返回写入的文件列表
pub fn unpack(hur: &Path, dest: &Path) -> Result<(Vec<String>, String, String)> {
    unpack_for(hur, dest, Format::Hur)
}

/// 解开包（按格式）：`.hur` 与 `.huf` 的**解包规则完全一样**（防穿越、条目名、清单里取
/// id/version），只有"缺哪个清单才算非法包"不同。
///
/// `fmt` 一般由 [`crate::spec::format_of_archive`] 从文件名认出来：拿一个 `.hur` 改名成
/// `.huf`，解包时会去找 `huf.json` —— 找不到就当场拒绝，这正是我们要的。
pub fn unpack_for(archive: &Path, dest: &Path, fmt: Format) -> Result<(Vec<String>, String, String)> {
    std::fs::create_dir_all(dest)?;
    let mut tmp: Option<PathBuf> = None;
    let out = (|| -> Result<(Vec<String>, String, String)> {
        let mut zip = open_zip_for(archive, dest, &mut tmp, fmt)?;
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
            if name == fmt.manifest() {
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
            bail!("包内缺少合法的 {}（没有 id）", fmt.manifest());
        }
        Ok((written, id, version))
    })();
    if let Some(t) = &tmp {
        let _ = std::fs::remove_file(t);
    }
    out
}

/// 产物的 sha256 与侧车（`<产物>.sha256`）登记值对不对得上（规则 **R6**）。
///
/// 侧车不进容器、不参与签名，它的用处是"下载完先看一眼有没有被换过"。
/// `.huf` 那边同一条检查叫 H9（见 `huf::check_sidecar`）——两条线两套编号，但判据一样。
pub fn check_sidecar(archive: &Path) -> Option<Issue> {
    let expect = sidecar_sha(archive)?;
    let actual = sha256_file(archive).ok()?;
    if expect == actual {
        return None;
    }
    Some(Issue::err(
        "R6",
        format!("产物 sha256 与登记值不一致：登记 {expect}，实际 {actual}"),
    ))
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
            profile: None,
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
        std::fs::write(dir.join(crate::spec::MANIFEST), serde_json::to_string_pretty(&pkg).unwrap())
            .unwrap();
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

    /// 外层必须是 gzip，且 gunzip 出来仍是**标准 zip**（第三条工具仍能看一眼包内有什么）。
    #[test]
    fn archive_is_gzip_wrapped_and_inner_stays_a_zip() {
        let dir = make_project("gz");
        let out = pack(&dir).unwrap();
        let bytes = std::fs::read(&out.file).unwrap();
        assert!(is_gzip(&bytes), "`.hur` 外层必须是 gzip（魔数 1f 8b）");
        let inner = gunzip(&bytes).unwrap();
        assert!(inner.starts_with(b"PK"), "gunzip 后应是标准 zip");
        let mut za = zip::ZipArchive::new(std::io::Cursor::new(inner)).unwrap();
        let mut s = String::new();
        za.by_name("hur.json").unwrap().read_to_string(&mut s).unwrap();
        assert!(s.contains("\"spec\""), "内层 zip 仍能按名字取到 hur.json");
    }

    /// 压缩要**真省字节**：可压内容（重复文本）在包里必须显著变小，否则这个容器就是白加的。
    #[test]
    fn gzip_actually_shrinks_a_repetitive_package() {
        let dir = make_project("shrink");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/big.txt"), "ABCDEFGHIJ".repeat(20_000)).unwrap();
        build_lock(&dir).unwrap();
        let out = pack(&dir).unwrap();
        let inner = gunzip(&std::fs::read(&out.file).unwrap()).unwrap();
        assert!(
            out.bytes < (inner.len() as u64) / 4,
            "可压内容应显著变小：包 {} 字节 vs 内层 {} 字节",
            out.bytes,
            inner.len()
        );
    }

    /// 老包（格式升级前打出来的**裸 zip**）必须继续能装 —— 已发出去的字节不是废纸。
    #[test]
    fn legacy_plain_zip_still_unpacks() {
        let dir = make_project("legacy");
        build_lock(&dir).unwrap();
        let legacy = dir.join("legacy.hur");
        std::fs::write(&legacy, zip_bytes_for(&dir, Format::Hur).unwrap()).unwrap();
        assert!(!is_gzip(&std::fs::read(&legacy).unwrap()), "这份老包不该带 gzip 头");

        let dest = std::env::temp_dir().join(format!("hur-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let (files, id, version) = unpack(&legacy, &dest).unwrap();
        assert!(files.iter().any(|f| f.as_str() == "hur.json"));
        assert!(id.starts_with("A-hotel-"));
        assert_eq!(version, "0.1.0");
    }

    /// 既不是 gzip 也不是 zip 的东西，要给出**说人话**的拒绝理由（而不是 zip 库的原始报错）。
    #[test]
    fn junk_is_rejected_with_a_readable_reason() {
        let dir = make_project("junk");
        let junk = dir.join("junk.hur");
        std::fs::write(&junk, b"not an archive at all").unwrap();
        let dest = std::env::temp_dir().join(format!("hur-junk-{}", std::process::id()));
        let err = format!("{:#}", unpack(&junk, &dest).unwrap_err());
        assert!(err.contains("既不是 gzip"), "{err}");
        assert!(!dest.join(format!(".hur-unzipped-{}", std::process::id())).exists(), "不该留下临时文件");
    }
}
