//! `ncc upgrade` —— 自更新：把发布通道里的最新二进制换到 `~/.ncc/bin/ncc[.exe]`。
//!
//! ## 为什么需要这个命令
//!
//! npm 包装（`packages/ncc-cli/bin/install.js` 与 `ncc.js`）**只在自己那份缓存不存在时**
//! 才下载二进制，从不比对版本。于是已经装过的人，`~/.ncc/bin/ncc` 里永远是第一次下载的
//! 那一版 —— 升级 npm 包、`npx` 显示新版本号，跑起来还是旧的。本命令给用户一个明确的
//! 升级入口，不依赖重装。
//!
//! ## 两处不显然的约束
//!
//! 1. **Windows 不允许覆盖或删除正在运行的 exe，但允许改名。** 所以替换不是直接覆盖，而是
//!    「写 `ncc.exe.new` → 把旧的改名成 `ncc.exe.old` → 把新的落位」。落位失败必须把旧的
//!    改回去 —— 宁可没更新，也不能让用户手里没有可执行文件。运行中的旧文件在 Windows 上
//!    删不掉，留给下一次进入时清理。
//! 2. **判定「是否需要更新」靠内容比对，不是版本号。** 发布通道提供的是版本无关的固定资产名
//!    （`releases/latest/download/ncc-<os>-<arch>`），拿不到「当前版本对应哪个构建」。下载完
//!    算 sha256 与现有文件比，相同就什么都不做 —— 好处是「版本号没变但二进制被重建」也不会漏掉。

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::ncc_dir;
use crate::terminal;

/// 比这还小的东西一定不是二进制（实测各平台产物都在 2.7–3.9 MB）。
const MIN_BYTES: u64 = 1024;

#[derive(clap::Args)]
pub struct UpgradeArgs {
    /// 只检查是否有新版本，不下载、不改动任何文件
    #[arg(long)]
    pub check: bool,

    /// 内容一致时也强制重新下载并替换
    #[arg(long)]
    pub force: bool,
}

/// 本地二进制路径 `~/.ncc/bin/ncc[.exe]`。
///
/// ⚠️ **必须与 JS 包装指向同一个文件** —— 否则升级的是另一个副本，用户看到的版本不变。
pub fn bin_path() -> PathBuf {
    ncc_dir().join("bin").join(exe_name())
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ncc.exe"
    } else {
        "ncc"
    }
}

/// 版本标记文件。npm 包装据此判断缓存是否还对应它自己的版本（见 `install.js`）。
pub fn marker_path() -> PathBuf {
    ncc_dir().join("bin").join(".version")
}

/// 发布资产名。
///
/// ⚠️ **命名契约**：这里必须与 `.github/workflows/release.yml` 顶部注释、
/// `packages/ncc-cli/bin/ncc.js` 与 `install.js` 的 `platformFile()` **逐字一致**。
/// 注意 `os` 用的是 `windows`（不是 Node 的 `win32`）、`arch` 用的是 `x86_64`
/// （不是 `x64`）—— 兄弟工程 rsi3d 的约定恰好相反，两边不要互相「统一」。
fn asset_name() -> Result<String> {
    let os = match env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        other => bail!("不支持的平台：{other}"),
    };
    let arch = match env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        other => bail!("不支持的架构：{other}"),
    };
    Ok(format!(
        "ncc-{os}-{arch}{}",
        if cfg!(windows) { ".exe" } else { "" }
    ))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build()
}

fn download(url: &str) -> Result<Vec<u8>> {
    let resp = agent()
        .get(url)
        .set("User-Agent", "ncc-cli")
        .call()
        .map_err(|e| anyhow!("下载失败 {url}：{e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .context("读取下载内容失败")?;
    Ok(buf)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// 从 release 的 `checksums.txt` 里取本资产那一行的 sha256。
///
/// 取不到（网络问题、文件缺失、没有这一行）返回 `None` —— 调用方据此降级到
/// 「大小 + 魔数」的弱检查，并把这件事如实告诉用户，而不是假装校验过了。
fn expected_sha(base: &str, asset: &str) -> Option<String> {
    let body = String::from_utf8(download(&format!("{base}/download/checksums.txt")).ok()?).ok()?;
    for line in body.lines() {
        let mut it = line.split_whitespace();
        let (hash, name) = (it.next()?, it.next()?);
        // sha256sum 的两种风格：`<hash>  <name>` 与 `<hash> *<name>`
        let name = name.trim_start_matches('*').trim_start_matches("./");
        if name == asset {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// 远端最新版本号（去掉 `v` 前缀）。取不到就返回 None —— 只影响提示文案与版本标记，
/// 不影响升级本身。
fn latest_version() -> Option<String> {
    let api = env::var("NCC_UPDATE_URL")
        .unwrap_or_else(|_| "https://api.github.com/repos/fusedmodel/ncc/releases/latest".into());
    let body = agent()
        .get(&api)
        .set("User-Agent", "ncc-cli")
        .set("Accept", "application/vnd.github+json")
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
}

/// 把 `suffix` 追加到文件名末尾（`ncc.exe` → `ncc.exe.new`）。
/// 不能用 `with_extension` —— 那会把 `.exe` 换成 `.new`，丢掉可执行扩展名。
fn sibling(dest: &Path, suffix: &str) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn install(bytes: &[u8]) -> Result<PathBuf> {
    let dest = bin_path();
    let dir = dest
        .parent()
        .ok_or_else(|| anyhow!("路径异常：{}", dest.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("创建 {} 失败", dir.display()))?;

    let new = sibling(&dest, ".new");
    let old = sibling(&dest, ".old");

    // 清理上一次的残留。运行中的旧文件在 Windows 上删不掉，删不掉就留下，下次再说。
    let _ = fs::remove_file(&new);
    let _ = fs::remove_file(&old);

    fs::write(&new, bytes).with_context(|| format!("写入 {} 失败", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&new, fs::Permissions::from_mode(0o755));
    }

    if dest.exists() {
        fs::rename(&dest, &old).with_context(|| format!("把现有 {} 改名失败", dest.display()))?;
        if let Err(e) = fs::rename(&new, &dest) {
            // ⚠️ 落位失败必须回滚：宁可没升级，也不能让用户手里没有可执行文件。
            let _ = fs::rename(&old, &dest);
            return Err(anyhow!("替换失败，已回滚到原文件：{e}"));
        }
        let _ = fs::remove_file(&old);
    } else {
        fs::rename(&new, &dest).with_context(|| format!("落位到 {} 失败", dest.display()))?;
    }
    Ok(dest)
}

/// 写版本标记，让 npm 包装知道这份缓存对应哪一版（避免它下一轮又重下一遍）。
fn stamp(version: &Option<String>) {
    match version {
        Some(v) => {
            if let Some(dir) = marker_path().parent() {
                let _ = fs::create_dir_all(dir);
            }
            if let Err(e) = fs::write(marker_path(), v) {
                eprintln!("  ⚠️ 写版本标记失败（不影响使用）：{e}");
            }
        }
        None => eprintln!(
            "  ⚠️ 取不到远端版本号，未写版本标记 —— npm 包装下次可能重下一遍二进制"
        ),
    }
}

pub fn run(args: &UpgradeArgs) -> Result<()> {
    if args.check {
        println!("{}", terminal::update_check());
        return Ok(());
    }

    let base = terminal::release_base();
    let asset = asset_name()?;
    let url = format!("{base}/download/{asset}");
    let dest = bin_path();
    let cur = env!("CARGO_PKG_VERSION");
    let ver = latest_version();

    match &ver {
        Some(v) => println!("当前 v{cur}，远端 v{v}"),
        None => println!("当前 v{cur}（取不到远端版本号，直接按内容比对）"),
    }
    println!("下载 {url}");

    let bytes = download(&url)?;

    // 弱检查：大小下限 + Windows 魔数。防的是「把 HTML 错误页 / checksums 当成二进制装进去」。
    if (bytes.len() as u64) < MIN_BYTES {
        bail!(
            "下载到的内容只有 {} 字节，不像是二进制 —— 拒绝安装（现有文件未改动）",
            bytes.len()
        );
    }
    if cfg!(windows) && bytes.len() >= 2 && &bytes[..2] != b"MZ" {
        bail!("下载到的内容不是合法的 Windows 可执行文件（magic 不是 MZ）—— 拒绝安装");
    }
    let got = sha256_hex(&bytes);

    // 强检查：与 release 自带的 checksums.txt 比对。不匹配就拒绝替换、保留旧文件。
    match expected_sha(&base, &asset) {
        Some(want) if want == got => println!("  ✓ 校验通过（sha256 {got}）"),
        Some(want) => bail!("校验不匹配：期望 {want}，实际 {got} —— 拒绝替换，现有二进制未改动"),
        None => println!("  ⚠️ 拿不到 checksums.txt 里这一行，跳过强校验（只做了大小与魔数检查）"),
    }

    // 内容一致就不动文件 —— 这也是「版本号相同但二进制被重建」能兜住的地方。
    if !args.force {
        if let Ok(existing) = fs::read(&dest) {
            if sha256_hex(&existing) == got {
                println!("已是最新（内容与远端一致），未做任何改动。");
                stamp(&ver);
                return Ok(());
            }
        }
    }

    let installed = install(&bytes)?;
    println!("  ✓ 已写入 {}", installed.display());
    stamp(&ver);

    // 当前进程跑的仍是旧二进制；如果压根不是同一个文件，更要说清楚。
    match env::current_exe() {
        Ok(me) if me.canonicalize().ok() != installed.canonicalize().ok() => println!(
            "  ℹ️ 你当前运行的是 {}，不是刚升级的这一份 —— 升级只影响新启动的进程。",
            me.display()
        ),
        _ => println!("  ℹ️ 当前进程仍是 v{cur}，重新启动后生效。"),
    }
    Ok(())
}
