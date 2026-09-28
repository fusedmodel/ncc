//! `ncc sign` / `ncc verify` —— 制品加签与核对（**不限 kind**）。
//!
//! 与 `ncc hur sign` 的分工，别在实现里糊掉：
//!
//! * **包**（HUR 包目录 / 现成的 `.hur` 产物）：签的是**规范打包字节**（含
//!   `hur.lock`），语义是"这个包"，走 `ncc hur sign` / `ncc hur attach`（那里要连
//!   包身份、版本、lock 一起核对）。`ncc sign` 碰到目录或 `.hur` 直接**转交**，
//!   不另起一套。
//! * **一份制品字节**（一个 `SKILL.md`、一个 `mcp.json`、任意一个文件）：签的就是
//!   **发布出去的那份文件**，`.minisig` 落在旁边。第三方只要有公钥，
//!   `minisign -V -p ncc.pub -m SKILL.md` 就能独立核对 —— **全程不需要 NCC，
//!   也不需要 ncc-cli**。这才是不把核对权交给平台的意思。
//!
//! 三条边界沿用整个 CLI：
//!
//! 1. **私钥不出设备**：`sign` 全本地；只有 `--attach` 联网，而且**只上传签名文件
//!    本身与公钥**（公开产物），制品字节一个字节都不传。
//! 2. **签名绑定字节**：签名对象里的 `sha256` 必须等于那份字节的摘要 —— 客户端先
//!    挡一次，服务端再加签时还会核一次（`signature_mismatch`）。签名不是标签。
//! 3. **不确定就如实说**：有签名但公钥本机不认识 ⇒ 只能说"有签名"，**不能说"已验证"**；
//!    条目里随签名一起带的公钥是**发布方的声明**，不是信任依据（自己给自己盖章）。
//!    签名**对不上**（改过 / 用错钥匙）则一律非 0 退出 —— 那不是"没签名"。

use anyhow::{anyhow, bail, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use hur_core::{sign, spec};

use crate::api;
use crate::config::{self, CliConfig};

#[derive(Args, Clone)]
pub struct SignArgs {
    /// 要签的制品：一个文件（SKILL.md / mcp.json / …），或一个 HUR 包目录 / .hur 产物
    #[arg(default_value = ".")]
    pub path: String,
    /// 制品种类（写进签名声明，别人一眼能看出签的是什么）；不给就是 artifact
    #[arg(long, default_value = "")]
    pub kind: String,
    /// 这个制品在目录里的引用（@命名空间/slug）—— 只写进声明，不改任何服务端状态
    #[arg(long, default_value = "")]
    pub reference: String,
    /// 版本号（同样只进声明）
    #[arg(long, default_value = "")]
    pub version: String,
    /// 自定义签名声明（默认自动生成 `ncc <kind> <引用> <版本> sha256=<hex>`）
    #[arg(long, default_value = "")]
    pub comment: String,
    /// 私钥文件（默认 ~/.harnessuse/keys/hur.key）
    #[arg(long, default_value = "")]
    pub key: String,
    /// 私钥口令（更推荐环境变量 HUR_KEY_PASSWORD）
    #[arg(long, default_value = "")]
    pub password: String,
    /// 签名文件落点（默认 <制品>.minisig）
    #[arg(long, default_value = "")]
    pub out: String,
    /// 把这份签名加到**已发布**的条目上（只上传签名文件与公钥，制品字节不传）
    #[arg(long, default_value = "", value_name = "引用")]
    pub attach: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct VerifyArgs {
    /// 本地文件路径，或已发布条目的引用（@命名空间/slug[@版本] / 条目 id）
    ///
    /// ⚠️ 这个位置参数的字段名**不能**叫 `target`：顶层 `--target` 是
    /// `global = true`，两者 arg id 相同时 clap 会把位置参数的值塞进全局项，
    /// 于是 `ncc verify ./SKILL.md` 会报「没有名为 ./SKILL.md 的目标」（已实测的坑，
    /// `Cmd::Info` 的 `reference` 就是这么改的）。
    pub reference: String,
    /// 本地核对时的签名文件（默认找 <制品>.minisig）
    #[arg(long, default_value = "")]
    pub sig: String,
    /// 用指定公钥核对（认谁是你的事，别让发布方替你决定）
    #[arg(long, default_value = "", value_name = "公钥文件")]
    pub pubkey: String,
    /// 没有签名 / 没核对通过就当失败（退出码非 0；适合放进 CI）
    ///
    /// （签名**对不上**的情况不需要这个开关：那是"这份字节被改过"，一律非 0。）
    #[arg(long)]
    pub require_signature: bool,
    #[arg(long)]
    pub json: bool,
}

/* ================================ 加签 ================================ */

pub fn sign(cfg: &CliConfig, a: &SignArgs) -> Result<()> {
    let p = PathBuf::from(a.path.trim());
    let attach = a.attach.trim().to_string();

    // ① 包 → 转交 hur 那条链路（它签的是规范打包字节，语义不同，别硬凑）
    if p.is_dir() || spec::is_archive_path(&p) {
        if !attach.is_empty() {
            bail!(
                "包（目录 / .hur）请用 `ncc hur attach --ref {attach}`：包的加签要连包身份、版本与 hur.lock 一起核对，两套语义别混"
            );
        }
        if !a.kind.trim().is_empty()
            || !a.reference.trim().is_empty()
            || !a.version.trim().is_empty()
            || !a.comment.trim().is_empty()
        {
            bail!("包不支持 --kind/--reference/--version/--comment：声明由 hur.json 自己出（`ncc hur sign .`）");
        }
        return package_sign(a);
    }

    if !p.is_file() {
        bail!(
            "要签的东西不存在（既不是文件，也不是包目录）：{}\n  提示：制品要先落成一份字节（比如 ncc download <引用> --out SKILL.md），签的就是那一份",
            p.display()
        );
    }

    // ② 签这份字节
    let abs = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
    let sha = spec::sha256_file(&abs)?;
    let comment = if a.comment.trim().is_empty() {
        sign::comment_for_artifact(&a.kind, &a.reference, &a.version, &sha)
    } else {
        a.comment.trim().to_string()
    };
    let store = sign::store();
    let password = password_of(&a.password);
    let key = key_of(&a.key);
    let sig_path = store
        .sign_file(&abs, &comment, key.as_deref(), password.as_deref())
        .map_err(|e| anyhow!("{e:#}"))?;
    let sig_path = if a.out.trim().is_empty() {
        sig_path
    } else {
        let want = PathBuf::from(a.out.trim());
        std::fs::rename(&sig_path, &want)
            .with_context(|| format!("把签名挪到 {} 失败", want.display()))?;
        want
    };

    // ③ 自己先核一遍：签出来却核不了（公钥不是一对、口令给错…），别急着拿去用
    let outcome = store.inspect(&abs, Some(&sig_path), None)?;
    let keynum = match &outcome {
        sign::SigOutcome::Verified(i) => i.keynum.clone(),
        other => bail!(
            "签出来了，但本机核不过：{}\n  先修好密钥再签（`ncc hur key gen` / `ncc hur key list`）",
            outcome_line(other)
        ),
    };
    let sig_name = sig_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("artifact.minisig")
        .to_string();
    let mut block = sig_block(&store, &keynum, &sha, &sig_name, &sig_path)?;

    // ④ 加签：只上传签名文件与公钥
    let attached = if attach.is_empty() {
        None
    } else {
        let (full, uploaded) = attach_to_item(cfg, &attach, &sha, &sig_path, &sig_name, &block)?;
        // 上传之后签名对象才带得动 url/sigSha256 —— 输出的就是**服务端实际存的那一份**
        block = uploaded;
        Some(full)
    };

    if a.json {
        let mut out = json!({
            "artifact": abs,
            "sha256": sha,
            "keynum": keynum,
            "comment": comment,
            "signaturePath": sig_path,
            // 这一块可以直接放进发布清单/`PUT /signature` 的 signature 字段
            "signature": block,
        });
        if let Some(full) = &attached {
            out["attached"] = json!(full);
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("✅ 已签名 {}（keynum {keynum}）", display_name(&abs));
    println!("   制品        {}", abs.display());
    println!("   签名        {}", sig_path.display());
    println!("   制品 sha256 {sha}");
    println!("   签名声明    {comment}");
    println!(
        "   第三方核对  minisign -V -p {} -m {}",
        store.public_path().display(),
        abs.display()
    );
    match &attached {
        Some(full) => println!("   已加签      {full}"),
        None => println!("   加到目录里  ncc sign {} --attach @我的命名空间/slug", display_name(&abs)),
    }
    Ok(())
}

/// 包：把参数映射到 hur 那条链路（它就是 `ncc hur sign`，不是第二套实现）。
fn package_sign(a: &SignArgs) -> Result<()> {
    crate::hur::sign_cmd(crate::hur::HurSignArgs {
        path: PathBuf::from(a.path.trim()),
        key: a.key.trim().to_string(),
        password: a.password.trim().to_string(),
        out: a.out.trim().to_string(),
        json: a.json,
    })
}

/// `--attach`：把本机签名补到一个**已发布**的条目上。
///
/// 与 `ncc hur attach` 同一条纪律：只传签名文件（+ 公钥进元数据），制品字节不传。
/// 服务端还会再核一次「签名覆盖的摘要 == 条目产物的摘要」，本机能挡的先在这里挡掉。
fn attach_to_item(
    cfg: &CliConfig,
    reference: &str,
    sha: &str,
    sig_path: &Path,
    sig_name: &str,
    block: &Value,
) -> Result<(String, Value)> {
    let token = config::require_token(cfg)?;
    let it = crate::hur::fetch_item(cfg, Some(&token), reference)?;
    let id = it["id"].as_str().unwrap_or("").to_string();
    if id.is_empty() {
        bail!("没能定位条目 id（引用写错？）");
    }
    let full = format!(
        "{}/{}@{}",
        it["namespace"]["slug"].as_str().unwrap_or(""),
        it["slug"].as_str().unwrap_or(""),
        it["version"].as_str().unwrap_or("")
    );
    let item_sha = it["storage"]["sha256"].as_str().unwrap_or("").trim().to_string();
    if !item_sha.is_empty() && !item_sha.eq_ignore_ascii_case(sha) {
        bail!(
            "这份签名覆盖的是 {sha}，而 {full} 现在的产物是 {item_sha}。\n  加签不是贴标签：把签名贴到别的字节上就是造假。先签对的那份字节（ncc download 下来再签）"
        );
    }

    let sig_bytes = std::fs::read(sig_path).with_context(|| format!("读签名 {} 失败", sig_path.display()))?;
    let up = api::request(
        cfg,
        "POST",
        "/api/registry/uploads",
        Some(&token),
        None,
        Some(&sig_bytes),
        &[("X-Filename", sig_name)],
    )?;
    // 上传成功后签名对象才带得动 url/sigSha256（下载方按它核对签名文件本身）
    let mut sig = block.clone();
    sig["url"] = up["storageUrl"].clone();
    sig["sigSha256"] = up["sha256"].clone();
    sig["sigBytes"] = up["size"].clone();

    let data = api::request(
        cfg,
        "PUT",
        &format!("/api/registry/{}/signature", api::urlenc(&id)),
        Some(&token),
        Some(&json!({ "signature": sig })),
        None,
        &[],
    )?;
    let it = &data["item"];
    let full = format!(
        "{}/{}@{}",
        it["namespace"]["slug"].as_str().unwrap_or(""),
        it["slug"].as_str().unwrap_or(""),
        it["version"].as_str().unwrap_or("")
    );
    // 服务端会把签名归一放在顶层 —— 以它的回执为准（写入失败就不可能走到这里）
    let stored = it
        .get("signature")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(sig);
    Ok((full, stored))
}

/// 签名对象（进清单/接口的那个 `signature{}`）。
///
/// `sig` 是 `.minisig` 的**明文内容**：离线发布（手写 --manifest）时，核对方不必再
/// 去下一个文件；`--attach` 会额外补上 `url` + `sigSha256`（可下载的那份的摘要）。
fn sig_block(
    store: &sign::Store,
    keynum: &str,
    sha: &str,
    sig_name: &str,
    sig_path: &Path,
) -> Result<Value> {
    let text = std::fs::read_to_string(sig_path)
        .with_context(|| format!("读签名 {} 失败", sig_path.display()))?;
    let mut sig = json!({
        "format": "minisign",
        "keynum": keynum,
        "signer": signer_label(store, keynum),
        "trusted": true,
        "sha256": sha,
        "name": sig_name,
        "sig": text,
        "sigBytes": std::fs::metadata(sig_path).map(|m| m.len()).unwrap_or(0),
    });
    // 本机公钥只是**线索**（别人拿到它才谈得上核对），不是信任依据
    if let Ok(pk) = std::fs::read_to_string(store.public_path()) {
        if !pk.trim().is_empty() {
            sig["pubkey"] = json!(pk.trim());
        }
    }
    Ok(sig)
}

/* ================================ 核对 ================================ */

pub fn verify(cfg: &CliConfig, a: &VerifyArgs) -> Result<()> {
    let t = a.reference.trim();
    if t.is_empty() {
        bail!("给个目标：本地文件路径，或条目引用 @命名空间/slug");
    }
    let p = PathBuf::from(t);
    if p.is_file() {
        return verify_local(&p, a);
    }
    if p.exists() && !p.is_file() {
        bail!("{} 不是文件（目录？）：包请用 `ncc hur verify {}`", p.display(), p.display());
    }
    verify_remote(cfg, a)
}

fn verify_local(p: &Path, a: &VerifyArgs) -> Result<()> {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let store = sign::store();
    let sig_override = if a.sig.trim().is_empty() { None } else { Some(PathBuf::from(a.sig.trim())) };
    let pub_override = pub_override(&a.pubkey);
    let outcome = store.inspect(&abs, sig_override.as_deref(), pub_override.as_deref())?;
    let sha = spec::sha256_file(&abs).ok();
    report(&outcome, a, &format!("{}（本地）", abs.display()), sha.as_deref(), None)
}

fn verify_remote(cfg: &CliConfig, a: &VerifyArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let it = crate::hur::fetch_item(cfg, token.as_deref(), a.reference.trim()).map_err(|e| {
        anyhow!(
            "{e:#}\n  （本机也没有叫「{}」的文件；本地文件直接给路径，已发布的给引用）",
            a.reference.trim()
        )
    })?;
    let full = format!(
        "{}/{}@{}",
        it["namespace"]["slug"].as_str().unwrap_or(""),
        it["slug"].as_str().unwrap_or(""),
        it["version"].as_str().unwrap_or("")
    );
    let kind = it["kind"].as_str().unwrap_or("");
    let storage_sha = it["storage"]["sha256"].as_str().unwrap_or("").trim().to_string();
    let url = it["storage"]["url"].as_str().unwrap_or("").trim().to_string();

    let sig = match it.get("signature").filter(|v| !v.is_null()) {
        Some(s) => s.clone(),
        None => {
            let where_ = if full.is_empty() { a.reference.trim().to_string() } else { format!("{full}（{kind}）") };
            if a.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "item": where_, "state": "unsigned", "sha256": storage_sha,
                    }))?
                );
            } else {
                println!("⚪ {where_} 未签名");
                println!("   制品 sha256 {}", if storage_sha.is_empty() { "（条目没有记录摘要）" } else { &storage_sha });
                println!("   要签的话    ncc sign <本地那份字节> --attach {}", a.reference.trim());
            }
            if a.require_signature {
                bail!("{where_} 没有签名（--require-signature 要求必须有）");
            }
            return Ok(());
        }
    };

    // ① 签名对象自洽：签名覆盖的摘要必须就是条目现在的产物摘要
    let sig_sha = sig["sha256"].as_str().unwrap_or("").trim().to_string();
    if !sig_sha.is_empty() && !storage_sha.is_empty() && !sig_sha.eq_ignore_ascii_case(&storage_sha) {
        bail!(
            "签名覆盖的摘要 {sig_sha} 与 {full} 现在的产物 {storage_sha} 不一致 —— 这份签名不是这份产物的（条目被换过？）"
        );
    }
    if url.is_empty() {
        bail!("{full} 没有可下载的产物地址，没法核对（发布时没传字节？）");
    }

    // ② 下字节，先核摘要再验签（避免拿一份"能下但不是登记的那份"去验）
    let tmp = TmpDir::new("ncc-verify")?;
    let art = tmp.path().join("artifact");
    let bytes = api::get_bytes(cfg, &url, token.as_deref())?;
    let got = spec::sha256_hex(&bytes);
    if !storage_sha.is_empty() && !got.eq_ignore_ascii_case(&storage_sha) {
        bail!("下载下来的字节与条目登记的摘要不一致（登记 {storage_sha}，实际 {got}）—— 别签收");
    }
    std::fs::write(&art, &bytes)?;

    // ③ 签名文件：优先按 url 下（可核对 sigSha256），否则用内联内容
    let (sig_text, sig_src) = fetch_sig_text(cfg, token.as_deref(), &sig)?;
    if let Some(expect) = sig["sigSha256"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let real = spec::sha256_hex(sig_text.as_bytes());
        if !real.eq_ignore_ascii_case(expect) {
            bail!("签名文件本身的摘要对不上（元数据 {expect}，实际 {real}）");
        }
    }
    let sig_path = tmp.path().join("artifact.minisig");
    std::fs::write(&sig_path, &sig_text)?;

    // ④ 验签。**只看本地能不能核对**：本机密钥 / 受信列表 / --pubkey
    let store = sign::store();
    let pub_override = pub_override(&a.pubkey);
    let outcome = store.inspect(&art, Some(&sig_path), pub_override.as_deref())?;

    // 有公钥线索却核不了 ⇒ 顺手做一次「自洽性」检查，把两种情况分开说清楚
    let note = match &outcome {
        sign::SigOutcome::UnknownKey { .. } => self_consistency(&store, &art, &sig_path, &sig),
        _ => None,
    };
    report(&outcome, a, &format!("{full}（{kind}）· 签名来自{sig_src}"), Some(&got), note.as_deref())
}

/// 取签名文件正文：先按 `signature.url` 下，拿不到再退回内联的 `signature.sig`。
fn fetch_sig_text(cfg: &CliConfig, token: Option<&str>, sig: &Value) -> Result<(String, String)> {
    let url = sig["url"].as_str().unwrap_or("").trim();
    if !url.is_empty() {
        match api::get_bytes(cfg, url, token) {
            Ok(b) => return Ok((String::from_utf8_lossy(&b).to_string(), "条目里的签名文件".to_string())),
            Err(e) => {
                if let Some(inline) = sig["sig"].as_str().filter(|s| !s.trim().is_empty()) {
                    return Ok((inline.to_string(), format!("条目内联的签名（下载失败：{e:#}）")));
                }
                return Err(e.context("下载签名文件失败，条目里也没有内联签名"));
            }
        }
    }
    match sig["sig"].as_str().filter(|s| !s.trim().is_empty()) {
        Some(s) => Ok((s.to_string(), "条目内联的签名".to_string())),
        None => bail!("条目里既没有签名文件地址也没有内联签名内容（signature.url / signature.sig 都空）"),
    }
}

/// 公钥未知时的补充检查：**条目自带的公钥**与这份签名是否自洽。
///
/// 注意它证明不了信任 —— 公钥和签名是同一个来源给的，通过只能说明"发布方没写错自己的公钥"，
/// 属于自证。要说"已验证"，得是 `ncc verify --pubkey <你自己确认过的公钥>` 或先 `ncc hur key trust`。
fn self_consistency(
    store: &sign::Store,
    art: &Path,
    sig_path: &Path,
    sig: &Value,
) -> Option<String> {
    let pk = sig["pubkey"].as_str().map(str::trim).filter(|s| !s.is_empty())?;
    let t = std::env::temp_dir().join(format!("ncc-verify-pub-{}", std::process::id()));
    if std::fs::write(&t, pk).is_err() {
        return None;
    }
    let res = store.inspect(art, Some(sig_path), Some(&t));
    let _ = std::fs::remove_file(&t);
    Some(match res {
        Ok(sign::SigOutcome::Verified(_)) => "条目自带的公钥与这份签名自洽（签名确实由它对应的私钥产生）—— 但公钥是发布方随签名一起给的，属**自证**；要认它请先核对公钥来源，再 `ncc hur key trust <公钥文件>`，或直接 `ncc verify <引用> --pubkey <你确认过的公钥>`".to_string(),
        _ => "条目自带的公钥与这份签名**对不上**：这份签名连自洽都做不到（元数据里的公钥不是签发这把）".to_string(),
    })
}

/* ================================ 输出 ================================ */

/// 打印结论。`--require-signature` 时"没核对通过"一律非 0 退出（CI 用）。
fn report(
    outcome: &sign::SigOutcome,
    a: &VerifyArgs,
    what: &str,
    sha: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    let (state, human, detail) = classify(outcome);
    if a.json {
        let mut out = json!({
            "target": what,
            "state": state,
            "verified": state == "verified",
            "sha256": sha,
            "detail": detail,
            "outcome": outcome,
        });
        if let Some(n) = note {
            out["note"] = json!(n);
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("{human}  {what}");
        if let Some(s) = sha {
            println!("   制品 sha256 {s}");
        }
        for line in detail_lines(outcome) {
            println!("   {line}");
        }
        if let Some(n) = note {
            println!("   ⚠️  {n}");
        }
        if let sign::SigOutcome::Verified(i) = outcome {
            println!("   声明        {}", i.comment);
        }
    }
    if a.require_signature && state != "verified" {
        bail!("{what}：{human}（--require-signature 要求签名**可核对**）");
    }
    // 密码学上不通过（改过 / 用错钥匙）**无论如何**都算失败：这不是"没签名"，
    // 是"签名说这份字节不对" —— 脚本/CI 里不能当成功继续往下走。
    if state == "bad" {
        bail!("{what}：{human} —— 这份字节与签名不是一对，别当它可信");
    }
    Ok(())
}

/// (状态, 人读一行, JSON 详情)
fn classify(o: &sign::SigOutcome) -> (&'static str, &'static str, String) {
    match o {
        sign::SigOutcome::Verified(i) => (
            "verified",
            "✔ 已验证",
            format!(
                "签发者 {} · keynum {} · {}{}",
                i.signer,
                i.keynum,
                if i.prehashed { "预哈希格式" } else { "内联格式" },
                match &i.claim_sha256 {
                    // 签名者自己的声明对不上实际字节：签名有效，但"签的是哪份"这句话是假的
                    Some(c) if !c.eq_ignore_ascii_case(&i.sha256) => format!(" ⚠️ 但声明 sha256={c} 与实测 {} 不一致", i.sha256),
                    _ => String::new(),
                }
            ),
        ),
        sign::SigOutcome::UnknownKey { keynum, .. } => (
            "unknown_key",
            "⚠️  有签名，但公钥本机不认识",
            format!("keynum {keynum} —— 只能说「有个签名」，不能说「已验证」。认它就拿到公钥核一遍：ncc hur key trust <公钥文件>"),
        ),
        sign::SigOutcome::Bad { keynum, msg, .. } => (
            "bad",
            "✖ 签名校验不通过",
            format!("keynum {keynum}：{msg} —— 内容被改过，或签名与这份字节不是一对"),
        ),
        sign::SigOutcome::Unsigned { .. } => (
            "unsigned",
            "⚪ 未签名",
            "这份字节旁边没有 .minisig（也没指定 --sig）".to_string(),
        ),
    }
}

fn detail_lines(o: &sign::SigOutcome) -> Vec<String> {
    match o {
        sign::SigOutcome::Verified(i) => vec![
            format!("签发者      {}", i.signer),
            format!("keynum      {}", i.keynum),
            format!("签名文件    {}", i.signature.display()),
            format!("签名声明    {}", i.comment),
            format!(
                "claim 摘要  {}",
                match &i.claim_sha256 {
                    Some(c) if c.eq_ignore_ascii_case(&i.sha256) => format!("{c}（与实测一致）"),
                    Some(c) => format!("{c}（⚠️ 与实测 {} 不一致）", i.sha256),
                    None => "（签名者没写摘要在声明里）".to_string(),
                }
            ),
        ],
        sign::SigOutcome::UnknownKey { signature, keynum, claim, .. } => vec![
            format!("keynum      {keynum}"),
            format!("签名文件    {}", signature.display()),
            match claim.raw.is_empty() {
                true => "签名声明    （没有 trusted comment，看不出是谁说签的什么）".to_string(),
                false => format!("签名声明    {}", claim.raw),
            },
        ],
        sign::SigOutcome::Bad { signature, keynum, msg, .. } => vec![
            format!("keynum      {keynum}"),
            format!("签名文件    {}", signature.display()),
            format!("原因        {msg}"),
        ],
        sign::SigOutcome::Unsigned { artifact } => vec![format!("目标        {}", artifact.display())],
    }
}

fn outcome_line(o: &sign::SigOutcome) -> String {
    let (_, human, detail) = classify(o);
    format!("{human} · {detail}")
}

/* ================================ 小工具 ================================ */

fn key_of(s: &str) -> Option<PathBuf> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(PathBuf::from(t))
    }
}

fn pub_override(s: &str) -> Option<PathBuf> {
    key_of(s)
}

fn password_of(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        std::env::var(sign::ENV_PASSWORD).ok().filter(|p| !p.trim().is_empty())
    } else {
        Some(s.to_string())
    }
}

/// 本机密钥的标签（能说清"是谁签的"就说，说不出来别编）。
fn signer_label(store: &sign::Store, keynum: &str) -> String {
    match store.own_key() {
        Some(k) if k.keynum.eq_ignore_ascii_case(keynum) => format!("本机密钥 {keynum}"),
        _ => keynum.to_string(),
    }
}

fn display_name(p: &Path) -> String {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("artifact").to_string()
}

/// 临时目录（用完删干净：里面是别人的制品字节与签名）。
struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!("{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).with_context(|| format!("建临时目录 {} 失败", d.display()))?;
        Ok(TmpDir(d))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
