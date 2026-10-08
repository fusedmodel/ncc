//! `ncc huf` —— **资源包**（Harness-Use Files）工具链。
//!
//! `.huf` 与 `.hur` 的分工（一句话）：**`.hur` 装能跑的东西，`.huf` 装给人看的东西**。
//! 文档 / 提示词 / 模板 / 静态资产 / 技能文本 / 知识库片段走这边；有 `entry`、要进沙箱的
//! 走 `ncc hur`。这不是"两套格式"，而是**同一个容器**（`gzip(zip(huf.json, huf.lock, 内容…))`）
//! 的两个身份：清单名不同、规范号不同、内容目录没有 `src/`，其余（确定性字节、防穿越解包、
//! Minisign 侧车签名、`.sha256` 侧车）全同源。
//!
//! 本模块是**薄壳**：规则、打包、验签全在 `hur_core::huf`，这里只做参数映射与输出。
//! 发布不需要新命令：`ncc publish --kind huf --name X --file dist/x.huf.gz`。
//!
//! **离线铁律**：init / build / pack / verify / inspect / ls / unpack / sign 全程本地，
//! 一个字节都不上传（发布是 `ncc publish` 的事）。

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use hur_core::{huf, pack, sign, spec};

#[derive(Subcommand)]
pub enum HufCmd {
    /// 生成一份资源包工程（huf.json + docs/README.md）
    Init(HufInitArgs),
    /// 刷新 huf.lock（逐文件 sha256；签名覆盖它）
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// 产包：dist/<id>-<version>.huf.gz + .sha256（**gzip 容器** · 确定性字节）
    Pack {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 校验包目录或 `.huf` / `.huf.gz` 产物（H1~H9，全程离线）
    Verify(HufVerifyArgs),
    /// 读包：清单 / 面向谁 / 文件清单 / 签名状态
    Inspect {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 列出包内文件（含 `huf.lock` 记录的 sha256）
    Ls {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 解包到目录（防穿越；默认解到产物同名目录）
    Unpack {
        path: PathBuf,
        /// 解到哪个目录（默认：产物名去掉 `.huf.gz` / `.huf`）
        #[arg(short = 'd', long)]
        dir: Option<PathBuf>,
    },
    /// 签名：签的是"规范打包字节"（Minisign/ed25519，第三方可独立核对）
    Sign(HufSignArgs),
    /// 规范总览：内容目录 / 规则 H1~H9 / 与 `.hur` 的差别（`--json` 给工具用）
    Spec {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Clone)]
pub struct HufInitArgs {
    #[arg(default_value = ".")]
    pub dir: PathBuf,
    /// 包 id（默认按目录名生成 `docs/<目录名>`）
    #[arg(long, default_value = "")]
    pub id: String,
    /// 用户看到的名字（默认用 id）
    #[arg(long, default_value = "")]
    pub name: String,
    /// 一句话（目录列表里显示的那行）
    #[arg(long, default_value = "")]
    pub short: String,
    /// 给谁看：user | operator | developer | agent（默认 user）
    #[arg(long, default_value = "user")]
    pub audience: String,
    /// 目录里已有 huf.json 时覆盖
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HufVerifyArgs {
    /// 工程目录 或 `.huf` / `.huf.gz` 产物
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 必须签名（没签就是错误；也可以由 CI 加这个旗标）
    #[arg(long)]
    pub require_signature: bool,
    /// 用指定的公钥核对（不填就用本机信任列表）
    #[arg(long, default_value = "")]
    pub pub_key: String,
    /// 提醒也算不通过（退出码 1）
    #[arg(long)]
    pub strict: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct HufSignArgs {
    /// 工程目录 或 `.huf` / `.huf.gz` 产物
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 私钥文件（默认 `~/.harnessuse/keys/hur.key`）
    #[arg(long, default_value = "")]
    pub key: String,
    /// 私钥口令（不填则读环境变量，别写进命令历史）
    #[arg(long, default_value = "")]
    pub password: String,
    /// 签名写到别处（默认产物旁边 `<产物>.minisig`）
    #[arg(long, default_value = "")]
    pub out: String,
    #[arg(long)]
    pub json: bool,
}

pub fn run(a: &HufCmd) -> Result<()> {
    match a {
        HufCmd::Init(i) => init(i.clone()),
        HufCmd::Build { path } => build(path),
        HufCmd::Pack { path, json } => pack_cmd(path, *json),
        HufCmd::Verify(v) => verify(v.clone()),
        HufCmd::Inspect { path, json } => inspect(path, *json),
        HufCmd::Ls { path, json } => ls(path, *json),
        HufCmd::Unpack { path, dir } => unpack(path, dir.as_deref()),
        HufCmd::Sign(s) => sign_cmd(s.clone()),
        HufCmd::Spec { json } => spec_cmd(*json),
    }
}

/// 打开一个目标（工程目录 / 产物）：产物先解到临时目录，回来时连着清单与清理责任。
struct Target {
    dir: PathBuf,
    archive: Option<PathBuf>,
    tmp: Option<PathBuf>,
    manifest: huf::HufManifest,
}

impl Drop for Target {
    fn drop(&mut self) {
        if let Some(t) = &self.tmp {
            let _ = std::fs::remove_dir_all(t);
        }
    }
}

fn open(path: &Path) -> Result<Target> {
    if !spec::is_archive_path(path) {
        let dir = pack::find_root_of(path, spec::Format::Huf)?;
        let manifest = huf::read(&dir)?;
        return Ok(Target {
            dir,
            archive: None,
            tmp: None,
            manifest,
        });
    }
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = std::env::temp_dir().join(format!("ncc-huf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    huf::unpack(&file, &tmp)?;
    let manifest = huf::read(&tmp)?;
    Ok(Target {
        dir: tmp.clone(),
        tmp: Some(tmp),
        archive: Some(file),
        manifest,
    })
}

fn print_issues(issues: &[spec::Issue]) -> (usize, usize) {
    let mut errs = 0;
    let mut warns = 0;
    for i in issues {
        match i.level {
            spec::Level::Error => {
                errs += 1;
                println!("  [{} 错误] {}", i.rule, i.msg);
            }
            spec::Level::Warn => {
                warns += 1;
                println!("  [{} 提醒] {}", i.rule, i.msg);
            }
            spec::Level::Info => println!("  [{} 提示] {}", i.rule, i.msg),
        }
    }
    (errs, warns)
}

fn init(a: HufInitArgs) -> Result<()> {
    if !huf::AUDIENCES.contains(&a.audience.as_str()) {
        bail!(
            "--audience 只能是 {}（当前「{}」）",
            huf::AUDIENCES.join(" | "),
            a.audience
        );
    }
    let id = if a.id.trim().is_empty() {
        format!(
            "docs/{}",
            a.dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("notes")
        )
    } else {
        a.id.trim().to_string()
    };
    let name = if a.name.trim().is_empty() {
        id.clone()
    } else {
        a.name.trim().to_string()
    };
    let short = a.short.trim().to_string();
    let written = huf::init(&a.dir, &id, &name, &short, a.force)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "root": a.dir, "id": id, "name": name, "audience": a.audience,
                "files": written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    println!("已生成资源包工程 {}", a.dir.display());
    for p in &written {
        println!("  {}", p.display());
    }
    println!();
    println!("下一步");
    println!(
        "  1. 把内容放进 {}（资源包里没有 src/：它不跑代码）",
        spec::HUF_CONTENT_DIRS.join(" / ")
    );
    println!("  2. ncc huf build        # 定下 huf.lock（逐个文件 sha256）");
    println!("  3. ncc huf verify       # H1~H9 本地校验");
    println!("  4. ncc huf pack         # dist/{}", "…huf.gz");
    println!(
        "  5. ncc huf sign && ncc publish --kind huf --name {} --file dist/…",
        name
    );
    Ok(())
}

fn build(path: &Path) -> Result<()> {
    let dir = pack::find_root_of(path, spec::Format::Huf)?;
    let lock = huf::build_lock(&dir)?;
    println!(
        "已写 {}（{} 个文件已锁定）",
        dir.join(spec::Format::Huf.lock()).display(),
        lock.files.len()
    );
    Ok(())
}

fn pack_cmd(path: &Path, json_out: bool) -> Result<()> {
    let dir = pack::find_root_of(path, spec::Format::Huf)?;
    // 产包前先自检：规则不过就**不产** —— 用户拿到的每个字节都该是能发出去的
    let manifest = huf::read(&dir)?;
    let issues = huf::validate(&manifest, &dir, huf::read_lock(&dir).as_ref());
    let errs = issues
        .iter()
        .filter(|i| i.level == spec::Level::Error)
        .count();
    if errs > 0 {
        print_issues(&issues);
        bail!("校验不过（{errs} 个错误），先 `ncc huf verify` 看清楚");
    }
    let out = huf::pack(&dir)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": out.file, "sha256": out.sha256, "bytes": out.bytes, "entries": out.entries,
            }))?
        );
        return Ok(());
    }
    println!("已产包 {}", out.file.display());
    println!("  bytes     {}", out.bytes);
    println!("  sha256    {}", out.sha256);
    println!(
        "  条目      {} 个（{}）",
        out.entries.len(),
        out.entries.join(" · ")
    );
    println!("  侧车      {}.sha256", out.file.display());
    Ok(())
}

fn verify(a: HufVerifyArgs) -> Result<()> {
    let t = open(&a.path)?;
    let mut issues: Vec<spec::Issue> = Vec::new();
    let mut ctx =
        json!({ "id": t.manifest.id, "version": t.manifest.version, "kind": t.manifest.kind });

    if let Some(file) = &t.archive {
        let sha = spec::sha256_file(file)?;
        if let Some(i) = huf::check_sidecar(file) {
            issues.push(i);
        }
        ctx["archive"] = json!(file);
        ctx["sha256"] = json!(sha);
    } else {
        ctx["root"] = json!(t.dir);
    }

    let lock = huf::read_lock(&t.dir);
    issues.extend(huf::validate(&t.manifest, &t.dir, lock.as_ref()));
    if let Some(lock) = &lock {
        ctx["files"] = json!(lock.files.len());
    }

    let pub_override = if a.pub_key.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(a.pub_key.trim()))
    };
    // 签名侧：产物在就核产物那份字节；否则核工程重算出来的规范字节
    let sig = match &t.archive {
        Some(file) => {
            let sig = sign::find_sig(file);
            sign::store().check_with_sig(
                file,
                sig.as_deref(),
                a.require_signature,
                pub_override.as_deref(),
                "产物",
                "H8",
            )
        }
        None => huf::check_signature(&t.dir, &t.manifest, a.require_signature),
    };
    if let Some(i) = &sig.info {
        ctx["signature"] = json!({
            "verified": true, "keynum": i.keynum, "signer": i.signer, "trusted": i.trusted,
            "sha256": i.sha256, "file": i.signature,
        });
    } else if let Some(s) = &sig.signature {
        ctx["signature"] = json!({ "verified": false, "file": s, "required": a.require_signature });
    }
    ctx["signatureRequired"] = json!(a.require_signature);
    issues.extend(sig.issues);

    let (errs, warns) = print_issues(&issues);
    let ok = errs == 0 && (!a.strict || warns == 0);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "context": ctx, "ok": ok, "errors": errs, "warnings": warns,
                "issues": issues.iter().map(|i| json!({"rule": i.rule, "level": format!("{:?}", i.level), "msg": i.msg})).collect::<Vec<_>>(),
            }))?
        );
    } else if ok {
        println!(
            "校验通过 ✔  {}",
            if t.archive.is_some() {
                "（产物）"
            } else {
                "（工程）"
            }
        );
        if !sig.summary.is_empty() {
            println!("  签名      {}", sig.summary);
        }
    } else {
        println!("校验结果：{errs} 个错误，{warns} 个提醒");
        if !sig.summary.is_empty() {
            println!("  签名      {}", sig.summary);
        }
    }
    if !ok {
        bail!("校验未通过（H1~H9）");
    }
    Ok(())
}

fn inspect(path: &Path, json_out: bool) -> Result<()> {
    let t = open(path)?;
    let m = &t.manifest;
    let sig = match &t.archive {
        Some(f) => {
            sign::store().check_with_sig(f, sign::find_sig(f).as_deref(), false, None, "产物", "H8")
        }
        None => huf::check_signature(&t.dir, m, false),
    };
    let files: Vec<Value> = file_rows(&t.dir, m);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "root": if t.archive.is_some() { Value::Null } else { json!(t.dir) },
                "archive": t.archive,
                "manifest": m,
                "files": files,
                "signature": { "verified": sig.verified, "summary": sig.summary },
            }))?
        );
        return Ok(());
    }
    println!("{}（{} v{}）", m.name, m.id, m.version);
    println!("  kind      {}（资源包：给人看，不执行）", m.kind);
    println!("  audience  {}", m.audience);
    if !m.headline().is_empty() {
        println!("  说明      {}", m.headline());
    }
    if !m.tags.is_empty() {
        println!("  tags      {}", m.tags.join(" · "));
    }
    let deps = m.deps.iter();
    if !deps.is_empty() {
        println!(
            "  依赖      {}",
            deps.iter()
                .map(|(k, r)| format!("{k}:{r}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    println!("  文件      {} 个", files.len());
    println!(
        "  签名      {}",
        if sig.summary.is_empty() {
            "未签名".into()
        } else {
            sig.summary
        }
    );
    Ok(())
}

fn file_rows(dir: &Path, _m: &huf::HufManifest) -> Vec<Value> {
    let lock = huf::read_lock(dir);
    spec::content_files_in(dir, spec::Format::Huf.content_dirs())
        .iter()
        .map(|f| {
            let rel = spec::rel(dir, f);
            json!({
                "path": rel,
                "sha256": lock.as_ref().and_then(|l| l.files.get(&rel).cloned()),
                "bytes": std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect()
}

fn ls(path: &Path, json_out: bool) -> Result<()> {
    let t = open(path)?;
    let files = file_rows(&t.dir, &t.manifest);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "id": t.manifest.id, "version": t.manifest.version, "files": files,
            }))?
        );
        return Ok(());
    }
    println!(
        "{} v{}（{} 个内容文件）",
        t.manifest.id,
        t.manifest.version,
        files.len()
    );
    for f in &files {
        println!(
            "  {:<40} {}",
            f["path"].as_str().unwrap_or(""),
            f["sha256"].as_str().unwrap_or("（未锁定）")
        );
    }
    Ok(())
}

fn unpack(path: &Path, dir: Option<&Path>) -> Result<()> {
    let dest = match dir {
        Some(d) => d.to_path_buf(),
        None => {
            if !spec::is_archive_path(path) {
                bail!("解包要指向 `.huf` / `.huf.gz` 产物，或者显式给 -d <目录>");
            }
            default_unpack_dir(path)
        }
    };
    let (files, id, version) = huf::unpack(path, &dest)?;
    println!(
        "已解开 {id} v{version} → {}（{} 个文件）",
        dest.display(),
        files.len()
    );
    for f in &files {
        println!("  {f}");
    }
    Ok(())
}

fn default_unpack_dir(archive: &Path) -> PathBuf {
    let name = archive
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("package");
    let stem = name
        .strip_suffix(&format!(".huf.{}", spec::ARTIFACT_CONTAINER_EXT))
        .or_else(|| name.strip_suffix(".huf"))
        .unwrap_or(name);
    archive.with_file_name(stem)
}

fn sign_cmd(a: HufSignArgs) -> Result<()> {
    let s = sign::store();
    // 签的**永远是产物那份字节**：产物给了就签它；给的是工程目录就先把规范字节落到 dist/
    // （与 `ncc huf pack` 的字节逐字节一致），再签。这样"签了"与"发出去的"是同一份。
    let (artifact, id, version) = match open(&a.path) {
        Ok(t) if t.archive.is_some() => {
            let f = t.archive.clone().unwrap();
            (f, t.manifest.id.clone(), t.manifest.version.clone())
        }
        other => {
            let t = other?;
            let (bytes, name) = pack::archive_bytes_for(&t.dir, spec::Format::Huf, false)
                .map_err(|e| anyhow!("先 `ncc huf build` 定下 huf.lock，再签名（{e:#}）"))?;
            let dist = t.dir.join(spec::DIST);
            std::fs::create_dir_all(&dist)?;
            let out = dist.join(&name);
            std::fs::write(&out, &bytes)?;
            let sha = spec::sha256_hex(&bytes);
            std::fs::write(
                dist.join(format!("{name}.sha256")),
                format!("{sha}  {name}\n"),
            )?;
            (out, t.manifest.id.clone(), t.manifest.version.clone())
        }
    };
    let sha = spec::sha256_file(&artifact)?;
    let comment = sign::comment_for_huf(&id, &version, &sha);
    let password = if a.password.trim().is_empty() {
        std::env::var(sign::ENV_PASSWORD)
            .ok()
            .filter(|p| !p.trim().is_empty())
    } else {
        Some(a.password.clone())
    };
    let key = if a.key.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(a.key.trim()))
    };
    let sig = s
        .sign_file(&artifact, &comment, key.as_deref(), password.as_deref())
        .map_err(|e| anyhow!("{e:#}"))?;
    let sig = if a.out.trim().is_empty() {
        sig
    } else {
        let want = PathBuf::from(a.out.trim());
        std::fs::rename(&sig, &want).ok();
        want
    };
    let keynum = s.own_key().map(|k| k.keynum).unwrap_or_default();
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "artifact": artifact, "signature": sig, "sha256": sha, "keynum": keynum,
            }))?
        );
    } else {
        println!("已签名 {id} v{version}（keynum {keynum}）");
        println!("  产物      {}", artifact.display());
        println!("  签名      {}", sig.display());
        println!("  sha256    {sha}");
        println!(
            "  第三方核对  minisign -V -p {} -m {}",
            s.public_path().display(),
            artifact.display()
        );
    }
    Ok(())
}

fn spec_cmd(json_out: bool) -> Result<()> {
    let dirs: Vec<&str> = spec::Format::Huf.content_dirs().to_vec();
    let rules: Vec<(&str, &str)> = vec![
        ("H1", "spec 必须是 harness-use-files/v1"),
        ("H2", "kind 必须是 files"),
        (
            "H3",
            "不许出现 entry/runtime/capabilities/agent/permissions… 这些「能跑」的字段",
        ),
        ("H4", "id 只含字母数字与 - _ . / @，不以 - 开头或结尾"),
        ("H5", "name 非空、version 是 x.y.z"),
        (
            "H6",
            "audience ∈ user|operator|developer|agent；short/summary 别都是空的",
        ),
        (
            "H7",
            "至少一个内容文件；huf.lock 与磁盘一致（新增/改动/删除都要重新 build）",
        ),
        (
            "H8",
            "签名（--require-signature 时必须有，且能核对到受信公钥）",
        ),
        ("H9", "产物的 sha256 与侧车 `<产物>.sha256` 登记值一致"),
    ];
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "spec": spec::HUF_SPEC,
                "lockSpec": spec::HUF_LOCK_SPEC,
                "manifest": spec::HUF_MANIFEST,
                "lock": spec::Format::Huf.lock(),
                "kind": spec::HUF_KIND,
                "artifact": "<id>-<version>.huf.gz（id 里的 / 换成 _）",
                "contentDirs": dirs,
                "audiences": huf::AUDIENCES,
                "rules": rules.iter().map(|(r, d)| json!({"rule": r, "desc": d})).collect::<Vec<_>>(),
                "registryKind": "huf",
                "runtime": false,
                "container": "gzip(zip(...)) —— 与 .hur 同容器、同确定性字节规则",
            }))?
        );
        return Ok(());
    }
    println!("`.huf` —— Harness-Use Files（面向用户的资源包）");
    println!("  规范      {}", spec::HUF_SPEC);
    println!(
        "  清单      {}（锁 {})",
        spec::HUF_MANIFEST,
        spec::Format::Huf.lock()
    );
    println!("  kind      {}（唯一身份）", spec::HUF_KIND);
    println!("  产物      <id>-<version>.huf.gz + .sha256（id 里的 / 换成 _）");
    println!("  内容目录  {}", dirs.join(" / "));
    println!("  面向谁    {}", huf::AUDIENCES.join(" | "));
    println!("  目录 kind huf");
    println!();
    println!("与 `.hur` 的差别（只有三处，容器是同一个）");
    println!(
        "  ① 清单名 hur.json → huf.json；② 规范号 harness-use-package/v1 → {}",
        spec::HUF_SPEC
    );
    println!("  ③ 内容目录没有 src/（那是代码的落点）");
    println!("  其余相同：确定性字节、防穿越解包、Minisign 侧车签名、sha256 侧车");
    println!("  **不执行**：没有 entry / runtime，`ncc hur run` 与宿主加载都不认它");
    println!();
    println!("规则");
    for (r, d) in rules {
        println!("  {r}  {d}");
    }
    println!();
    println!("发布（不需要新命令）");
    println!("  ncc huf pack && ncc huf sign && ncc publish --kind huf --name <名字> --file dist/<产物>.huf.gz");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_unpack_dir_strips_known_suffixes() {
        assert_eq!(
            PathBuf::from("/tmp/x/onboarding-0.1.0"),
            default_unpack_dir(Path::new("/tmp/x/onboarding-0.1.0.huf.gz"))
        );
        assert_eq!(
            PathBuf::from("/tmp/x/onboarding-0.1.0"),
            default_unpack_dir(Path::new("/tmp/x/onboarding-0.1.0.huf"))
        );
        // 名字不认得就原样当目录名（不猜）
        assert_eq!(
            PathBuf::from("/tmp/x/裸包"),
            default_unpack_dir(Path::new("/tmp/x/裸包"))
        );
    }

    #[test]
    fn spec_json_is_stable_enough_for_tools() {
        // 规则编号必须与 `huf::validate` 里用的一致（H1~H8）+ 侧车 H9
        let rules: Vec<&str> = vec!["H1", "H2", "H3", "H4", "H5", "H6", "H7", "H8", "H9"];
        let src =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/huf.rs")).unwrap();
        for r in rules {
            assert!(src.contains(&format!("\"{r}\"")), "规则 {r} 在实现里没出现");
        }
    }
}
