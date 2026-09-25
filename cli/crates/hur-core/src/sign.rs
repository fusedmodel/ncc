//! 制品签名（`harness-use-signature/v1`）：让策略里的 `verify.require_signature` **真的可满足**。
//!
//! ## 为什么是 Minisign 格式（而不是自造信封）
//! 签名的价值在于**别人也能独立核对**。用 Minisign（ed25519）格式，第三方不必装 hur：
//!
//! ```text
//! minisign -V -p hur.pub -m dist/A-xxx-0.1.0.hur      # 官方 CLI
//! rsign2 -V -p hur.pub -m dist/A-xxx-0.1.0.hur        # 纯 Rust 实现
//! ```
//!
//! 我们自己也不解释密码学：`minisign` crate 负责 ed25519 / 预哈希 / 口令加密，
//! hur 只负责**签什么、放哪、谁的公钥算受信、策略要不要它**。
//!
//! ## 三层模型
//! | 层 | 落点 | 说明 |
//! |---|---|---|
//! | 私钥（签名用） | `~/.harnessuse/keys/hur.key`（0600） | 默认不加密；`--password`/`HUR_KEY_PASSWORD` 才加密。**不做交互式口令提示**（GUI sidecar 没有 TTY，提示会挂住） |
//! | 公钥（自己的） | `~/.harnessuse/keys/hur.pub` | 可给人核对；`hur key pub` 导出 |
//! | 受信公钥（别人的） | `~/.harnessuse/trusted-keys.json` | **显式信任列表**：没在列表里的公钥 = "有签名但不可核对"，不是"已验证" |
//!
//! ## 签什么
//! **永远签"规范打包字节"**（`pack::archive_bytes`）。于是目录与产物两条路都能核：
//! - 产物：直接核 `dist/<id>-<version>.hur` 的字节；
//! - 工程目录：把目录重算成同样的规范字节再核（不落盘、无副作用）。
//!
//! 签名覆盖 `hur.lock`，所以锁没定下来就拒绝签/核（避免"签了一份自己都不确定的字节"）。
//!
//! ## 签名文件里写什么
//! Minisign 的 **trusted comment 是被签名的**（篡改必被发现），我们在里面放包身份与摘要：
//! `hur <id> <version> sha256=<hex>`；于是"签名对得上"同时意味着"身份声明没被改过"。

use anyhow::{bail, Context, Result};
use minisign::{KeyPair, PublicKey, PublicKeyBox, SecretKey, SecretKeyBox, SignatureBox};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::spec::{HurPackage, Issue, Level};

pub const TRUSTED_SPEC: &str = "harness-use-trusted-keys/v1";
pub const KEYS_DIR: &str = "keys";
pub const SECRET_FILE: &str = "hur.key";
pub const PUBLIC_FILE: &str = "hur.pub";
pub const TRUSTED_FILE: &str = "trusted-keys.json";
/// 签名文件后缀（Minisign 惯例；`minisign -V` 默认就找 `<file>.minisig`）
pub const SIG_SUFFIX: &str = ".minisig";
/// 也接受这个后缀（宽容读取）
pub const ALT_SIG_SUFFIX: &str = ".sig";
pub const ENV_PASSWORD: &str = "HUR_KEY_PASSWORD";
pub const ENV_KEY: &str = "HUR_KEY";

fn perr(what: &str, e: minisign::PError) -> anyhow::Error {
    anyhow::anyhow!("{what}：{e}")
}

/* ---------------- 密钥与信任的落点 ---------------- */

/// 密钥/信任的存储位置。可注入（单测不需要动 `HUR_HOME` 这种进程级环境变量）。
#[derive(Debug, Clone)]
pub struct Store {
    pub keys_dir: PathBuf,
    pub trusted_file: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::at(&crate::cfg::home())
    }
}

/// 默认存储（`~/.harnessuse/`）
pub fn store() -> Store {
    Store::default()
}

impl Store {
    pub fn at(home: &Path) -> Self {
        Self { keys_dir: home.join(KEYS_DIR), trusted_file: home.join(TRUSTED_FILE) }
    }
    pub fn secret_path(&self) -> PathBuf {
        self.keys_dir.join(SECRET_FILE)
    }
    pub fn public_path(&self) -> PathBuf {
        self.keys_dir.join(PUBLIC_FILE)
    }

    /// 本机公钥（不碰私钥：很多场景只需要"我是谁"）
    pub fn own_key(&self) -> Option<OwnKey> {
        let text = std::fs::read_to_string(self.public_path()).ok()?;
        let pk = PublicKeyBox::from_string(&text).ok()?.into_public_key().ok()?;
        Some(OwnKey {
            keynum: keynum_hex(pk.keynum()),
            public_key: pk.to_base64(),
            encrypted: secret_is_encrypted(&self.secret_path()),
        })
    }

    /// 生成密钥对。`password` 为空 = **不加密**（会打印提醒）；**绝不交互式提示口令**。
    pub fn gen_key(&self, password: Option<&str>, force: bool) -> Result<OwnKey> {
        let sk_path = self.secret_path();
        if sk_path.exists() && !force {
            bail!("私钥已存在：{}（覆盖加 --force —— 旧私钥丢了，之前发出去的签名就再也核不了）", sk_path.display());
        }
        std::fs::create_dir_all(&self.keys_dir).with_context(|| format!("创建 {} 失败", self.keys_dir.display()))?;
        let kp = match password.map(str::trim).filter(|p| !p.is_empty()) {
            Some(pw) => KeyPair::generate_encrypted_keypair(Some(pw.to_string())).map_err(|e| perr("生成加密密钥对失败", e))?,
            None => KeyPair::generate_unencrypted_keypair().map_err(|e| perr("生成密钥对失败", e))?,
        };
        let pk_text = kp.pk.to_box().map_err(|e| perr("序列化公钥失败", e))?.to_string();
        let sk_text = kp
            .sk
            .to_box(Some("hur package signing key"))
            .map_err(|e| perr("序列化私钥失败", e))?
            .to_string();
        write_private(&sk_path, &sk_text)?;
        std::fs::write(self.public_path(), pk_text).with_context(|| format!("写 {} 失败", self.public_path().display()))?;
        Ok(OwnKey { keynum: keynum_hex(kp.pk.keynum()), public_key: kp.pk.to_base64(), encrypted: password.map(str::trim).is_some_and(|p| !p.is_empty()) })
    }

    /// 载入私钥（显式 `--key` 优先），并返回 (私钥, 对应公钥)
    fn secret(&self, key_override: Option<&Path>, password: Option<&str>) -> Result<(SecretKey, PublicKey)> {
        let path = key_override.map(|p| p.to_path_buf()).unwrap_or_else(|| self.secret_path());
        if !path.exists() {
            bail!("没有私钥 {}：先 `hur key gen`（或用 --key 指定别处的私钥）", path.display());
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("读私钥 {} 失败", path.display()))?;
        let sk_box = SecretKeyBox::from_string(&text).map_err(|e| perr("解析私钥失败（不是 Minisign 私钥？）", e))?;
        // 先按"未加密"试：不加密的钥匙不该因为用户没给口令就打不开
        let sk = match sk_box.clone().into_unencrypted_secret_key() {
            Ok(sk) => sk,
            Err(_) => {
                let pw = password
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| p.to_string())
                    .or_else(|| std::env::var(ENV_PASSWORD).ok().filter(|p| !p.trim().is_empty()));
                let Some(pw) = pw else {
                    bail!(
                        "私钥 {} 有口令保护，但没拿到口令：用 --password，或设环境变量 {ENV_PASSWORD}。\
                         （hur 不会交互式提示口令 —— 从 GUI / 脚本调用时那会直接挂住）",
                        path.display()
                    );
                };
                sk_box.into_secret_key(Some(pw)).map_err(|e| perr("口令不对或私钥损坏", e))?
            }
        };
        let pk = PublicKey::from_secret_key(&sk).map_err(|e| perr("从私钥推公钥失败", e))?;
        Ok((sk, pk))
    }

    /* ---------- 签名 ---------- */

    /// 只签一个已经存在的产物文件 → 写 `<file>.minisig`
    pub fn sign_file(&self, artifact: &Path, trusted_comment: &str, key_override: Option<&Path>, password: Option<&str>) -> Result<PathBuf> {
        if !artifact.is_file() {
            bail!("要签的东西不存在：{}", artifact.display());
        }
        let (sk, pk) = self.secret(key_override, password)?;
        // 防呆：本机 hur.pub 若和私钥不是一对，签出来的东西谁都核不了
        if key_override.is_none() {
            if let Some(own) = self.own_key() {
                let mine = keynum_hex(pk.keynum());
                if own.keynum != mine {
                    bail!("本机私钥与 {} 不是一对（私钥 {mine} / 公钥 {}）：先修好再签", self.public_path().display(), own.keynum);
                }
            }
        }
        let f = std::fs::File::open(artifact).with_context(|| format!("打开 {} 失败", artifact.display()))?;
        let sig = minisign::sign(Some(&pk), &sk, f, Some(trusted_comment), Some("hur package signature"))
            .map_err(|e| perr("签名失败", e))?;
        let out = sig_path_for(artifact);
        std::fs::write(&out, sig.to_string()).with_context(|| format!("写 {} 失败", out.display()))?;
        Ok(out)
    }

    /* ---------- 验证 ---------- */

    /// 核对签名。结果是**结论**（不抛错），只有 I/O / 签名文件本身损坏才 `Err`。
    pub fn inspect(&self, artifact: &Path, sig: Option<&Path>, pub_override: Option<&Path>) -> Result<SigOutcome> {
        let Some(sig_path) = sig.map(|p| p.to_path_buf()).or_else(|| find_sig(artifact)) else {
            return Ok(SigOutcome::Unsigned { artifact: artifact.to_path_buf() });
        };
        let text = std::fs::read_to_string(&sig_path).with_context(|| format!("读签名 {} 失败", sig_path.display()))?;
        let sig_box = SignatureBox::from_string(&text).map_err(|e| perr("解析签名失败（不是 Minisign 签名？）", e))?;
        let keynum = keynum_hex(sig_box.keynum());
        let comment = sig_box.trusted_comment().unwrap_or_default();
        let claim = Claim::parse(&comment);

        // 候选公钥：显式 --pub → 本机 → 受信列表；**按 keynum 挑**，不瞎试
        let mut candidates: Vec<(String, PublicKey, bool)> = Vec::new();
        if let Some(p) = pub_override {
            let text = std::fs::read_to_string(p).with_context(|| format!("读公钥 {} 失败", p.display()))?;
            let pk = parse_public(&text).with_context(|| format!("解析公钥 {} 失败", p.display()))?;
            candidates.push((format!("显式 --pub（{}）", p.display()), pk, true));
        }
        if let Some(own) = self.own_key() {
            if let Ok(text) = std::fs::read_to_string(self.public_path()) {
                if let Ok(pk) = parse_public(&text) {
                    candidates.push((format!("本机密钥 {}", own.keynum), pk, true));
                }
            }
        }
        let trusted = self.trusted();
        for k in &trusted.keys {
            if let Ok(pk) = parse_public(&k.public_key) {
                candidates.push((k.label.clone(), pk, true));
            }
        }

        let mut last_err: Option<String> = None;
        for (label, pk, is_trusted) in candidates {
            if keynum_hex(pk.keynum()) != keynum {
                continue;
            }
            let f = std::fs::File::open(artifact).with_context(|| format!("打开 {} 失败", artifact.display()))?;
            match minisign::verify(&pk, &sig_box, f, true, false, false) {
                Ok(()) => {
                    let sha = crate::spec::sha256_file(artifact)?;
                    return Ok(SigOutcome::Verified(SigInfo {
                        artifact: artifact.to_path_buf(),
                        signature: sig_path,
                        keynum,
                        signer: label,
                        trusted: is_trusted,
                        sha256: sha.clone(),
                        claim: claim.clone(),
                        claim_sha256: claim.sha256.clone(),
                        prehashed: sig_box.is_prehashed(),
                        comment,
                    }));
                }
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        Ok(match last_err {
            None => SigOutcome::UnknownKey { artifact: artifact.to_path_buf(), signature: sig_path, keynum, claim },
            Some(msg) => SigOutcome::Bad { artifact: artifact.to_path_buf(), signature: sig_path, keynum, msg },
        })
    }

    /* ---------- 受信公钥 ---------- */

    pub fn trusted(&self) -> TrustedKeys {
        std::fs::read_to_string(&self.trusted_file)
            .ok()
            .and_then(|t| serde_json::from_str::<TrustedKeys>(&t).ok())
            .map(|mut k| {
                k.spec = TRUSTED_SPEC.to_string();
                k
            })
            .unwrap_or_default()
    }

    fn save_trusted(&self, k: &TrustedKeys) -> Result<PathBuf> {
        crate::cfg::ensure_home()?;
        if let Some(dir) = self.trusted_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = format!("{}\n", serde_json::to_string_pretty(&TrustedKeys { spec: TRUSTED_SPEC.to_string(), ..k.clone() })?);
        std::fs::write(&self.trusted_file, text).with_context(|| format!("写 {} 失败", self.trusted_file.display()))?;
        Ok(self.trusted_file.clone())
    }

    /// 信任一个公钥：`src` 可以是 `.pub` 文件路径，也可以是单行 base64
    pub fn trust(&self, src: &str, label: &str, note: &str) -> Result<TrustedKey> {
        let s = src.trim();
        let pk = if Path::new(s).is_file() {
            let text = std::fs::read_to_string(s).with_context(|| format!("读公钥 {s} 失败"))?;
            parse_public(&text)?
        } else {
            parse_public(s).with_context(|| "既不是存在的公钥文件，也不是合法的公钥 base64")?
        };
        let keynum = keynum_hex(pk.keynum());
        let label = if label.trim().is_empty() { format!("keynum {keynum}") } else { label.trim().to_string() };
        let k = TrustedKey {
            keynum: keynum.clone(),
            label,
            public_key: pk.to_base64(),
            added_at_unix: crate::policy::now_unix(),
            note: note.trim().to_string(),
        };
        let mut list = self.trusted();
        list.keys.retain(|x| x.keynum != keynum);
        list.keys.push(k.clone());
        list.keys.sort_by(|a, b| a.keynum.cmp(&b.keynum));
        self.save_trusted(&list)?;
        Ok(k)
    }

    pub fn untrust(&self, keynum: &str) -> Result<bool> {
        let want = keynum.trim().trim_start_matches("0x").to_ascii_uppercase();
        let mut list = self.trusted();
        let before = list.keys.len();
        list.keys.retain(|x| x.keynum.to_ascii_uppercase() != want);
        let removed = list.keys.len() != before;
        self.save_trusted(&list)?;
        Ok(removed)
    }

    /* ---------- 目录 / 产物两条入口 ---------- */

    /// 核一个**工程目录**：重算规范字节（不落盘），再核签名
    pub fn check_dir(&self, dir: &Path, pkg: &HurPackage, required: bool, pub_override: Option<&Path>) -> SigReport {
        let artifact = dir.join(crate::spec::DIST).join(format!("{}-{}.hur", pkg.id, pkg.version));
        let sig = find_sig(&artifact);
        // 产物在就直接核它（下发给别人的也是这一份）；不在就重算规范字节到临时文件核
        if artifact.is_file() {
            return self.report(&artifact, sig, required, pub_override, "产物");
        }
        if sig.is_some() {
            let (bytes, name) = match crate::pack::archive_bytes(dir, false) {
                Ok(v) => v,
                Err(e) => return SigReport { required, ..SigReport::issue(required, Issue::err("R9", format!("无法重算规范字节来核对签名：{e:#}"))) },
            };
            let tmp = std::env::temp_dir().join(format!("hur-sigcheck-{}-{name}", std::process::id()));
            if let Err(e) = std::fs::write(&tmp, &bytes) {
                return SigReport { required, ..SigReport::issue(required, Issue::err("R9", format!("写临时文件失败：{e}"))) };
            }
            let r = self.report(&tmp, sig, required, pub_override, "工程目录（重算的规范字节）");
            let _ = std::fs::remove_file(&tmp);
            return r;
        }
        // 没有签名文件：只有"策略要求"才出声，平时别制造噪音
        self.report(&artifact, None, required, pub_override, "工程目录")
    }

    /// 核一个 `.hur` 产物文件
    pub fn check_archive(&self, archive: &Path, required: bool, pub_override: Option<&Path>) -> SigReport {
        let sig = find_sig(archive);
        self.report(archive, sig, required, pub_override, "产物")
    }

    /// 把 `inspect` 的结论折成「人读摘要 + R9 检查项」
    fn report(&self, artifact: &Path, sig: Option<PathBuf>, required: bool, pub_override: Option<&Path>, what: &str) -> SigReport {
        let outcome = match self.inspect(artifact, sig.as_deref(), pub_override) {
            Ok(o) => o,
            Err(e) => return SigReport { required, ..SigReport::issue(required, Issue::err("R9", format!("{e:#}"))) },
        };
        let mut issues = Vec::new();
        let (verified, info, summary) = match outcome {
            SigOutcome::Verified(info) => {
                if let Some(claim) = &info.claim_sha256 {
                    if *claim != info.sha256 {
                        issues.push(Issue::err("R9", format!("签名声明 sha256={claim}，但产物实际是 {}（签名者自己的声明对不上）", info.sha256)));
                    }
                }
                let s = format!("✔ 签名有效 · 签名者 {} · keynum {} · {}", info.signer, info.keynum, if info.prehashed { "预哈希格式" } else { "内联格式" });
                // 留一条"已核对通过"的证据：计划器的 fail-closed 判据就是"有没有 R9 条目"
                issues.push(Issue::info("R9", format!("签名有效 · 签发者 {} · keynum {}", info.signer, info.keynum)));
                (true, Some(info), s)
            }
            SigOutcome::UnknownKey { keynum, claim, .. } => {
                let msg = match claim.sha256.as_deref().map(|s| s.to_string()) {
                    Some(s) => format!("有签名（keynum {keynum}，声明 sha256={s}），但公钥不在本机受信列表：`hur key trust <公钥>` 之后才算可核对"),
                    None => format!("有签名（keynum {keynum}），但公钥不在本机受信列表：`hur key trust <公钥>` 之后才算可核对"),
                };
                issues.push(if required { Issue::err("R9", msg.clone()) } else { Issue::warn("R9", msg.clone()) });
                (false, None, msg)
            }
            SigOutcome::Bad { keynum, msg, .. } => {
                let m = format!("签名校验失败（keynum {keynum}）：{msg} —— 内容被改过，或签名与这份字节不是一对");
                issues.push(Issue::err("R9", m.clone()));
                (false, None, m)
            }
            SigOutcome::Unsigned { .. } => {
                if required {
                    let m = format!(
                        "策略要求签名（verify.require_signature），但 {what} 找不到签名文件（{}）：先 `hur sign` 签一次",
                        sig_path_for(artifact).display()
                    );
                    issues.push(Issue::err("R9", m.clone()));
                    (false, None, m)
                } else {
                    (false, None, "未签名（策略没要求签名；签名后别人才能独立核对" .to_string() + &format!("：`hur sign {}`）", artifact.display()))
                }
            }
        };
        SigReport { required, artifact: Some(artifact.to_path_buf()), signature: sig, verified, info, summary, issues }
    }
}

/* ---------------- 数据结构 ---------------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKey {
    /// 16 位大写十六进制（Minisign 惯例）
    pub keynum: String,
    #[serde(default)]
    pub label: String,
    /// 单行 base64 公钥（`PublicKey::to_base64`）
    pub public_key: String,
    #[serde(default)]
    pub added_at_unix: u64,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKeys {
    #[serde(default = "trusted_spec_default")]
    pub spec: String,
    #[serde(default)]
    pub keys: Vec<TrustedKey>,
}

fn trusted_spec_default() -> String {
    TRUSTED_SPEC.to_string()
}

impl Default for TrustedKeys {
    fn default() -> Self {
        Self { spec: trusted_spec_default(), keys: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OwnKey {
    pub keynum: String,
    pub public_key: String,
    pub encrypted: bool,
}

/// 签名 trusted comment 里的声明（hur 自己的部分；**该 comment 被签名保护**）
#[derive(Debug, Clone, Default, Serialize)]
pub struct Claim {
    pub package: String,
    pub version: String,
    pub sha256: Option<String>,
    pub raw: String,
}

impl Claim {
    pub fn parse(comment: &str) -> Claim {
        let mut c = Claim { raw: comment.trim().to_string(), ..Default::default() };
        let mut it = comment.split_whitespace();
        if it.next() == Some("hur") {
            c.package = it.next().unwrap_or_default().to_string();
            c.version = it.next().unwrap_or_default().to_string();
            for tok in it {
                if let Some(v) = tok.strip_prefix("sha256=") {
                    c.sha256 = Some(v.to_ascii_lowercase());
                }
            }
        }
        c
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SigInfo {
    pub artifact: PathBuf,
    pub signature: PathBuf,
    pub keynum: String,
    /// 谁签的（本机密钥 / 受信标签 / 显式 --pub）
    pub signer: String,
    /// 公钥是否"可核对"（本机或受信列表）——**没有公钥的签名不算已验证**
    pub trusted: bool,
    pub sha256: String,
    pub claim: Claim,
    pub claim_sha256: Option<String>,
    pub prehashed: bool,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SigOutcome {
    Verified(SigInfo),
    /// 签名在，但公钥不是本机认识的 → 只能说是"有个签名"，不能说"已验证"
    UnknownKey { artifact: PathBuf, signature: PathBuf, keynum: String, claim: Claim },
    /// 密码学上不通过：内容被改过 / 用错钥匙
    Bad { artifact: PathBuf, signature: PathBuf, keynum: String, msg: String },
    Unsigned { artifact: PathBuf },
}

/// 一次签名核对的报告（含 R9 检查项）
#[derive(Debug, Clone, Serialize)]
pub struct SigReport {
    pub required: bool,
    pub artifact: Option<PathBuf>,
    pub signature: Option<PathBuf>,
    pub verified: bool,
    pub info: Option<SigInfo>,
    /// 人读一行
    pub summary: String,
    /// R9 检查项（并入 `hur verify` / 执行计划）
    pub issues: Vec<Issue>,
}

impl Default for SigReport {
    fn default() -> Self {
        Self { required: false, artifact: None, signature: None, verified: false, info: None, summary: String::new(), issues: Vec::new() }
    }
}

impl SigReport {
    fn issue(required: bool, i: Issue) -> SigReport {
        SigReport { required, summary: i.msg.clone(), issues: vec![i], ..Default::default() }
    }
    pub fn has_error(&self) -> bool {
        self.issues.iter().any(|i| i.level == Level::Error)
    }
}

/* ---------------- 小工具 ---------------- */

/// keynum → 16 位大写十六进制（Minisign 的展示惯例）
pub fn keynum_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// 规范签名文件路径：`<artifact>.minisig`
pub fn sig_path_for(artifact: &Path) -> PathBuf {
    PathBuf::from(format!("{}{}", artifact.display(), SIG_SUFFIX))
}

/// 找签名文件：优先 `.minisig`，退回 `.sig`（宽容读）
pub fn find_sig(artifact: &Path) -> Option<PathBuf> {
    let a = sig_path_for(artifact);
    if a.is_file() {
        return Some(a);
    }
    let b = PathBuf::from(format!("{}{}", artifact.display(), ALT_SIG_SUFFIX));
    b.is_file().then_some(b)
}

/// 签名里放的声明：包身份 + 规范字节的 sha256（trusted comment 被签名保护）
pub fn comment_for(pkg: &HurPackage, sha256: &str) -> String {
    format!("hur {} {} sha256={}", pkg.id, pkg.version, sha256)
}

/// 从公钥文件文本（两行 box）或单行 base64 解析公钥
pub fn parse_public(text: &str) -> Result<PublicKey> {
    let t = text.trim();
    if t.contains('\n') {
        let b = PublicKeyBox::from_string(t).map_err(|e| perr("解析公钥 box 失败", e))?;
        return b.into_public_key().map_err(|e| perr("公钥 box 转 key 失败", e));
    }
    PublicKey::from_base64(t).map_err(|e| perr("解析公钥 base64 失败", e))
}

fn secret_is_encrypted(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    match SecretKeyBox::from_string(&text) {
        Ok(b) => b.into_unencrypted_secret_key().is_err(),
        Err(_) => true,
    }
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(text.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("写 {} 失败", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Deps, Permissions, PublishInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("hur-sign-{}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst), tag));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pkg() -> HurPackage {
        HurPackage {
            spec: crate::spec::PKG_SPEC.into(),
            kind: "agent".into(),
            id: "A-test-sign-000001".into(),
            name: "签名测试".into(),
            version: "0.1.0".into(),
            short: "SIG".into(),
            domain: String::new(),
            summary: String::new(),
            entry: "src/agent.ts".into(),
            runtime: String::new(),
            capabilities: vec![],
            deps: Deps::default(),
            permissions: Permissions::default(),
            publish: PublishInfo::default(),
            agent: None,
            security: None,
        }
    }

    /// 造一个最小可打包工程
    fn project(t: &Tmp) -> PathBuf {
        let dir = t.0.join("proj");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("hur.json"), serde_json::to_string_pretty(&pkg()).unwrap()).unwrap();
        std::fs::write(dir.join("src/agent.ts"), "export const x = 1\n").unwrap();
        crate::pack::build_lock(&dir).unwrap();
        dir
    }

    /// 按规范把产物落到 `dir/dist/<id>-<version>.hur`（`check_dir` 找的就是这里）
    fn pack_into_dist(dir: &Path, p: &HurPackage) -> (PathBuf, String) {
        let (bytes, name) = crate::pack::archive_bytes(dir, false).unwrap();
        let dist = dir.join(crate::spec::DIST);
        std::fs::create_dir_all(&dist).unwrap();
        let out = dist.join(&name);
        std::fs::write(&out, &bytes).unwrap();
        let sha = crate::spec::sha256_hex(&bytes);
        assert_eq!(p.id, pkg().id);
        (out, sha)
    }

    #[test]
    fn keygen_sign_verify_roundtrip() {
        let t = Tmp::new("roundtrip");
        let s = Store::at(&t.0);
        let own = s.gen_key(None, false).unwrap();
        assert_eq!(own.keynum.len(), 16);
        assert!(!own.encrypted);
        assert!(s.own_key().is_some(), "公钥应能读回");

        let dir = project(&t);
        let p = pkg();
        let (artifact, sha) = pack_into_dist(&dir, &p);

        let sig = s.sign_file(&artifact, &comment_for(&p, &sha), None, None).unwrap();
        assert!(sig.to_string_lossy().ends_with(SIG_SUFFIX));

        let r = s.check_archive(&artifact, true, None);
        assert!(r.verified, "{}", r.summary);
        assert!(!r.has_error(), "{:?}", r.issues);
        let info = r.info.unwrap();
        assert_eq!(info.keynum, own.keynum);
        assert_eq!(info.claim.package, p.id);
        assert_eq!(info.claim.sha256.as_deref(), Some(sha.as_str()));
        assert!(info.trusted, "本机密钥应算可核对");

        // 目录路径同样能核（产物在就直接核产物，且不改动它）
        let rd = s.check_dir(&dir, &p, true, None);
        assert!(rd.verified, "{}", rd.summary);
        assert_eq!(crate::spec::sha256_file(&artifact).unwrap(), sha, "核对不该改动产物");
    }

    /// 内容被改 → 签名必须失败（这是签名的全部意义）
    #[test]
    fn tampered_bytes_are_rejected() {
        let t = Tmp::new("tamper");
        let s = Store::at(&t.0);
        s.gen_key(None, false).unwrap();
        let dir = project(&t);
        let p = pkg();
        let (artifact, sha) = pack_into_dist(&dir, &p);
        let bytes = std::fs::read(&artifact).unwrap();
        s.sign_file(&artifact, &comment_for(&p, &sha), None, None).unwrap();

        // 改一个字节
        let mut bad = bytes.clone();
        let n = bad.len() / 2;
        bad[n] ^= 0x01;
        std::fs::write(&artifact, &bad).unwrap();

        let r = s.check_archive(&artifact, true, None);
        assert!(!r.verified);
        assert!(r.has_error(), "篡改必须是错误：{:?}", r.issues);
        assert!(r.issues[0].msg.contains("签名校验失败"), "{:?}", r.issues[0].msg);
        assert_eq!(r.issues[0].rule, "R9");
    }

    /// 工程内容变了但签名没重签 → 目录核对同样会失败（签名覆盖规范打包字节）
    #[test]
    fn dir_change_after_signing_is_caught() {
        let t = Tmp::new("dirdrift");
        let s = Store::at(&t.0);
        s.gen_key(None, false).unwrap();
        let dir = project(&t);
        let p = pkg();
        let (artifact, sha) = pack_into_dist(&dir, &p);
        s.sign_file(&artifact, &comment_for(&p, &sha), None, None).unwrap();
        // 把产物挪走 → 强制走"重算目录字节"这条路
        std::fs::remove_file(&artifact).unwrap();

        assert!(s.check_dir(&dir, &p, true, None).verified, "未改动时应核得过");
        assert!(!artifact.exists(), "核对目录不该顺手在 dist/ 重新落一份产物");

        std::fs::write(dir.join("src/agent.ts"), "export const x = 2\n").unwrap();
        let r = s.check_dir(&dir, &p, true, None);
        assert!(!r.verified, "改了内容还能过就说明签名没起作用");
        assert!(r.has_error());
    }

    /// 公钥不在受信列表：**不能说已验证**；策略要求签名时这是错误
    #[test]
    fn unknown_key_is_not_verified() {
        let t = Tmp::new("unknown");
        let a = Store::at(&t.0.join("a"));
        let b = Store::at(&t.0.join("b"));
        a.gen_key(None, false).unwrap();
        b.gen_key(None, false).unwrap();
        let dir = project(&t);
        let p = pkg();
        let (artifact, sha) = pack_into_dist(&dir, &p);
        a.sign_file(&artifact, &comment_for(&p, &sha), None, None).unwrap();

        // b 的钥匙串不认识 a 的签名
        let r = b.check_archive(&artifact, false, None);
        assert!(!r.verified);
        assert!(!r.has_error(), "没要求签名时只是提醒：{:?}", r.issues);
        assert_eq!(r.issues[0].level, Level::Warn);
        assert!(r.issues[0].msg.contains("不在本机受信列表"));

        let strict = b.check_archive(&artifact, true, None);
        assert!(strict.has_error(), "要求签名时，不可核对 = 错误");

        // 把 a 的公钥加进 b 的受信列表 → 可核对
        let pk_line = a.own_key().unwrap().public_key;
        let k = b.trust(&pk_line, "团队 A", "").unwrap();
        assert_eq!(k.keynum.len(), 16);
        let ok = b.check_archive(&artifact, true, None);
        assert!(ok.verified, "{}", ok.summary);
        assert!(ok.info.unwrap().signer.contains("团队 A"));

        // 撤销信任 → 又变成不可核对
        assert!(b.untrust(&k.keynum).unwrap());
        assert!(!b.check_archive(&artifact, true, None).verified);
        assert!(!b.untrust("DEADBEEFDEADBEEF").unwrap());
    }

    /// 没签名 + 策略没要求 → 不出检查项（verify 输出不该被噪音淹没）
    #[test]
    fn unsigned_is_quiet_unless_required() {
        let t = Tmp::new("quiet");
        let s = Store::at(&t.0);
        s.gen_key(None, false).unwrap();
        let dir = project(&t);
        let p = pkg();
        let opt = s.check_dir(&dir, &p, false, None);
        assert!(opt.issues.is_empty(), "{:?}", opt.issues);
        assert!(opt.summary.contains("未签名"));

        let req = s.check_dir(&dir, &p, true, None);
        assert_eq!(req.issues.len(), 1);
        assert_eq!(req.issues[0].level, Level::Error);
        assert!(req.issues[0].msg.contains("hur sign"), "{}", req.issues[0].msg);
    }

    /// 口令保护的私钥：不给口令时报可读错误，**绝不交互式提示**（GUI 会挂住）
    #[test]
    fn encrypted_key_needs_password_and_never_prompts() {
        let t = Tmp::new("enc");
        let s = Store::at(&t.0);
        let own = s.gen_key(Some("hunter2"), false).unwrap();
        assert!(own.encrypted);
        assert!(s.own_key().unwrap().encrypted);
        let dir = project(&t);
        let p = pkg();
        let (artifact, sha) = pack_into_dist(&dir, &p);

        let e = s.sign_file(&artifact, &comment_for(&p, &sha), None, None).unwrap_err().to_string();
        assert!(e.contains("口令"), "{e}");
        assert!(e.contains(ENV_PASSWORD), "{e}");

        let sig = s.sign_file(&artifact, &comment_for(&p, &sha), None, Some("hunter2")).unwrap();
        assert!(sig.exists());
        let wrong = s.sign_file(&artifact, &"hur x y".to_string(), None, Some("nope")).unwrap_err().to_string();
        assert!(wrong.contains("口令不对"), "{wrong}");
    }

    /// 私钥与 hur.pub 不是一对：拒绝签（否则签出来的东西谁都核不了）
    #[test]
    fn mismatched_keypair_is_refused() {
        let t = Tmp::new("mismatch");
        let s = Store::at(&t.0);
        s.gen_key(None, false).unwrap();
        let other = Store::at(&t.0.join("other"));
        other.gen_key(None, false).unwrap();
        let dir = project(&t);
        let p = pkg();
        let (artifact, _sha) = pack_into_dist(&dir, &p);
        // 显式 --key：跳过本机一致性检查（用户明确知道自己在用哪把钥匙），签名应当成功
        assert!(s.sign_file(&artifact, "hur x y", Some(&other.secret_path()), None).is_ok());

        // 把本机公钥换成别人的 → 本机一致性检查必须拦住
        std::fs::copy(other.public_path(), s.public_path()).unwrap();
        let e2 = s.sign_file(&artifact, "hur x y", None, None).unwrap_err().to_string();
        assert!(e2.contains("不是一对"), "{e2}");
    }

    #[test]
    fn claim_parsing_and_keynum_hex() {
        let c = Claim::parse("hur A-x 0.2.0 sha256=ABCdef");
        assert_eq!(c.package, "A-x");
        assert_eq!(c.version, "0.2.0");
        assert_eq!(c.sha256.as_deref(), Some("abcdef"));
        assert!(Claim::parse("别的工具写的注释").sha256.is_none());
        assert_eq!(keynum_hex(&[0x0a, 0xff]), "0AFF");
    }
}
