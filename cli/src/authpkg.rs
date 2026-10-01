//! `ncc auth pkg` —— **授权包**（`profile=auth`）：一份可以被 ncc 开锁的凭据库。
//!
//! 一句话：**包里只有元数据是明文，值是密文；开锁只发生在一次调用的那一小段时间里，
//! 调用结束就重新上锁。**
//!
//! 为什么值得做成 HUR 包，而不是一个 `.env`：授权数据要能**被签名、被分发、被第三方核对**，
//! 还要能说清"这里面有哪几个项目、要拿什么开、谁能开" —— 那是清单的事，不是点文件的事。
//!
//! ## 落到磁盘上的形状
//!
//! ```text
//! team-auth/
//!   hur.json                 profile=auth，auth{projects/wrap/allow/kdf_iters/vault_sha256}
//!   auth/index.json          明文元数据：项目、条目名、过期、gen、迭代次数（**没有值**）
//!   auth/vault.enc           密文：主密钥 + 每个项目一段独立密文
//!   auth/keys/<项目>.enc     项目密钥文件（每个项目一把，可单独轮换）
//!   hur.lock                 逐文件摘要（更新一次要重新 build + 重新签）
//! ```
//!
//! 本地状态（**不随包走**）：`~/.harnessuse/auth/<包>/`
//!
//! ```text
//!   lock                     一次只许一个写入者（见下）
//!   audit.jsonl              谁、什么时候、对哪个项目做了什么 —— **绝不记值**
//!   sessions/<pid>-<ts>/     run 窗口里的明文，退出即抹
//! ```
//!
//! ## 红线（改代码前先读）
//!
//!  1. **包里只允许元数据明文**。值只能进 `auth/vault.enc`。手写的 index 里出现
//!     `value`/`secret`/`token` 这类键，`hur verify` 直接判不合规（R12 里那一条）。
//!  2. **密文被清单钉住**：`auth.vault_sha256` 写进 `hur.json`，签名覆盖清单 ——
//!     于是"把密文换成一袋垃圾"必须连清单一起改，签名就挂不住了。开锁前先核这一条。
//!  3. **开锁要有明确的许可**：`auth.allow` 非空时，本机身份公钥指纹不在里面就不开；
//!     项目没在 `auth.projects` 里不开；条目过期就不给用（`get --reveal` / `run` 都拒）。
//!  4. **更新要有锁**：所有会动包/密钥文件的动作都在 `~/.harnessuse/auth/<包>/lock` 里串行
//!     （`O_EXCL` 建锁 + 死锁回收 + 超时），写入一律 tmp → fsync → rename。
//!  5. **用完就抹**：`run` 的明文只活在 `sessions/<pid>-<ts>/`（0700），**退出即删**，
//!     异常路径靠 Drop 兜底；残留的会被下一次命令清掉。`ncc auth pkg lock` 可以手动清。
//!  6. **审计不记值**：只记条目名。值一旦写进日志，加密就白做了。
//!  7. **口令不走 argv**：只收 `--passphrase-file` / `--passphrase-stdin`
//!     （`ps` 里看得见 argv，也看得见环境变量）。
//!  8. **本模块不是密码学创新**：PBKDF2 派生 + ChaCha20-Poly1305（ring 提供），
//!     主密钥随机、每项目一把、AAD 绑定「包@版本 / 项目:gen」防密文张冠李戴。
//!     身份包法用的是本机身份私钥（0600）派生 —— 它保护的是"文件被别人拷走"，
//!     不是"这台机器被人坐上去了"。
use anyhow::{anyhow, bail, Context, Result};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::hkdf::{Salt, HKDF_SHA256};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::auth;

const MAGIC: &[u8] = b"NCCAUTH1";
const DEFAULT_ITERS: u32 = 210_000;
const MIN_ITERS: u32 = 50_000;
/// 锁超过这么久没人动 = 判为死锁（进程没了也算死，见 `holder_dead`）
const LOCK_STALE_SEC: u64 = 900;
/// run 窗口的明文目录超过这么久还留着 = 上次异常退出漏下的，清掉
const SESSION_TTL_SEC: u64 = 3600;

/* ---------------- 小工具 ---------------- */

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn rand_bytes(n: usize) -> Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    SystemRandom::new().fill(&mut b).map_err(|_| anyhow!("取随机数失败"))?;
    Ok(b)
}

fn hostname() -> String {
    crate::connssh::hostname()
}

/// 两个主机名“算不算同一台”：认不出来（unknown）就不比 —— 别让一个查不到的名字
/// 把死锁回收整条路堵死（pid 那条判定仍然在跑）。
fn same_host(a: &str, b: &str) -> bool {
    if a.trim().is_empty() || b.trim().is_empty() || a == "unknown" || b == "unknown" || a == "localhost" || b == "localhost" {
        return true;
    }
    a == b
}

fn whoami() -> String {
    format!("{}@{}", std::env::var("USER").unwrap_or_else(|_| "nobody".into()), hostname())
}

/// 包的 id 在本地状态目录里的安全化形式（`@you/team-auth@0.1.0` → `you-team-auth-0.1.0`）。
fn slug_of(id: &str) -> String {
    let mut s = String::new();
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            s.push(c);
        } else {
            s.push('-');
        }
    }
    s.trim_matches('-').to_string()
}

fn state_dir(pkg_id: &str) -> PathBuf {
    let base = std::env::var("HUR_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".harnessuse")
    });
    base.join("auth").join(slug_of(pkg_id))
}

/* ---------------- 加解密 ---------------- */

fn aead_key(raw: &[u8]) -> Result<LessSafeKey> {
    let k = UnboundKey::new(&CHACHA20_POLY1305, raw).map_err(|_| anyhow!("密钥长度不对"))?;
    Ok(LessSafeKey::new(k))
}

fn seal(raw_key: &[u8], aad: &[u8], plain: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let nonce = rand_bytes(12)?;
    let mut buf = plain.to_vec();
    aead_key(raw_key)?
        .seal_in_place_append_tag(Nonce::try_assume_unique_for_key(&nonce).map_err(|_| anyhow!("nonce 不对"))?, Aad::from(aad), &mut buf)
        .map_err(|_| anyhow!("加密失败"))?;
    Ok((nonce, buf))
}

fn open(raw_key: &[u8], aad: &[u8], nonce: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != 12 {
        bail!("nonce 长度不对（{}）", nonce.len());
    }
    let mut buf = ct.to_vec();
    let plain = aead_key(raw_key)?
        .open_in_place(Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow!("nonce 不对"))?, Aad::from(aad), &mut buf)
        .map_err(|_| anyhow!("解不开（口令/身份不对，或者密文被改过）"))?;
    Ok(plain.to_vec())
}

/// 口令 → 32 字节 KEK（PBKDF2-HMAC-SHA256）。
fn kek_from_passphrase(pass: &str, salt: &[u8], iters: u32) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    let rounds = std::num::NonZeroU32::new(iters.max(1)).ok_or_else(|| anyhow!("迭代次数不能是 0"))?;
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, rounds, salt, pass.as_bytes(), &mut out);
    Ok(out)
}

/// 本机身份私钥 → 32 字节 KEK（HKDF）。**用私钥字节的 sha256 当 ikm**（HKDF 要均匀的 ikm）。
fn kek_from_identity(der: &[u8], salt: &[u8]) -> Result<[u8; 32]> {
    let ikm = Sha256::digest(der);
    let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
    let info: [&[u8]; 1] = [b"ncc-auth-wrap-v1"];
    let okm = prk.expand(&info, HkdfLen32).map_err(|_| anyhow!("HKDF 失败"))?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(|_| anyhow!("HKDF 取字节失败"))?;
    Ok(out)
}

struct HkdfLen32;
impl ring::hkdf::KeyType for HkdfLen32 {
    fn len(&self) -> usize {
        32
    }
}

/// 项目密钥由主密钥派生（换主密钥 = 所有项目密钥一起换）。
fn project_kek(master: &[u8], salt: &[u8], project: &str, gen: u64) -> Result<[u8; 32]> {
    let prk = Salt::new(HKDF_SHA256, salt).extract(master);
    let info = format!("ncc-auth-key:{project}:{gen}");
    let info: [&[u8]; 1] = [info.as_bytes()];
    let okm = prk.expand(&info, HkdfLen32).map_err(|_| anyhow!("HKDF 失败"))?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(|_| anyhow!("HKDF 取字节失败"))?;
    Ok(out)
}

/* ---------------- 原子写 ---------------- */

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow!("路径没有父目录：{}", path.display()))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("f"),
        std::process::id(),
        hex(&rand_bytes(4)?)
    ));
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).with_context(|| format!("建临时文件失败：{}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    fs::rename(&tmp, path).with_context(|| format!("落位到 {} 失败", path.display()))?;
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

#[cfg(unix)]
fn ensure_dir_700(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(p)?;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
    Ok(())
}
#[cfg(not(unix))]
fn ensure_dir_700(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

/* ---------------- 锁 ---------------- */

/// 一把文件锁：**同一个包的写入动作一次只许一个**。
///
/// 为什么需要它：更新要同时动 `auth/vault.enc`、`auth/keys/<项目>.enc`、`auth/index.json`、
/// `hur.json` 四处 —— 两份更新叠在一起，就会出现"vault 是新密钥的、密钥文件是旧的"这种
/// 谁也解不开的状态。锁把这一段变成串行。
pub struct VaultLock {
    path: PathBuf,
    token: String,
    /// 是不是抢回了别人留下的死锁（抢回来了要说一声）
    pub reclaimed: bool,
}

impl VaultLock {
    pub fn acquire(state: &Path, op: &str, reason: &str, timeout_sec: u64) -> Result<VaultLock> {
        ensure_dir_700(state)?;
        let path = state.join("lock");
        let token = hex(&rand_bytes(8)?);
        let me = json!({
            "pid": std::process::id(), "host": hostname(), "atUnix": now_unix(),
            "op": op, "reason": reason, "by": whoami(), "token": token,
        });
        let deadline = Instant::now() + Duration::from_secs(timeout_sec);
        let mut reclaimed = false;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    f.write_all(format!("{me}\n").as_bytes())?;
                    f.sync_all()?;
                    return Ok(VaultLock { path, token, reclaimed });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder = fs::read_to_string(&path).unwrap_or_default();
                    if holder_stale(&holder) {
                        // 死锁：改名留档再抢（不直接删，万一有人正查"谁卡住了"）
                        let tag = state.join(format!("lock.stale.{}", now_unix()));
                        let _ = fs::rename(&path, &tag);
                        reclaimed = true;
                        continue;
                    }
                    if Instant::now() >= deadline {
                        let v: Value = serde_json::from_str(holder.trim()).unwrap_or(Value::Null);
                        bail!(
                            "这个包正被另一个动作占着（等满 {timeout_sec}s 也没轮到我）：\n  \
                             谁   {}（pid {} @ {}）\n  干什么 {}（{}）\n  什么写 {} \n  \
                             它没动静了就等一会儿，或 `ncc auth pkg lock <包目录>` 清残留",
                            v["by"].as_str().unwrap_or("?"),
                            v["pid"].as_i64().unwrap_or(-1),
                            v["host"].as_str().unwrap_or("?"),
                            v["op"].as_str().unwrap_or("?"),
                            v["reason"].as_str().unwrap_or(""),
                            v["atUnix"].as_i64().map(fmt_epoch).unwrap_or_default(),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(anyhow!("{e}")),
            }
        }
    }

    pub fn release(&self) {
        // 只删自己的锁：token 对不上说明锁已经换人（或被人回收了），别乱删
        if let Ok(txt) = fs::read_to_string(&self.path) {
            if let Ok(v) = serde_json::from_str::<Value>(txt.trim()) {
                if v["token"].as_str() == Some(self.token.as_str()) {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// 锁是不是"死的"：进程没了，或者太久没动静。
fn holder_stale(txt: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(txt.trim()) else {
        return true; // 读不出内容 = 上一个人写坏了，可以抢
    };
    if !same_host(v["host"].as_str().unwrap_or(""), &hostname()) {
        // 别的机器持的锁（同一份 NFS/HOME）—— 只看时间
        let at = v["atUnix"].as_u64().unwrap_or(0);
        return now_unix().saturating_sub(at) > LOCK_STALE_SEC;
    }
    let pid = v["pid"].as_i64().unwrap_or(-1);
    if pid <= 0 {
        return true;
    }
    if !pid_alive(pid as i32) {
        return true;
    }
    let at = v["atUnix"].as_u64().unwrap_or(0);
    now_unix().saturating_sub(at) > LOCK_STALE_SEC
}

/// 这个进程还在吗？**不引 libc**：Linux 看 `/proc`，其它系统问 `kill -0`。
///
/// ⚠️ 别写成 `Command::new("kill")` 就完事：在 macOS 上 `kill` 常常只是 shell 内建，
/// PATH 里未必有可执行文件 —— spawn 失败时如果按"问不出来就当活着"处理，死锁就
/// 永远回收不了（这里踩过：pid 999999 的锁被当成"有人占着"）。
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if Path::new("/proc").is_dir() {
        return Path::new(&format!("/proc/{pid}")).exists();
    }
    for bin in ["/bin/kill", "/usr/bin/kill", "kill"] {
        let Ok(o) = std::process::Command::new(bin).arg("-0").arg(pid.to_string()).output() else {
            continue;
        };
        if o.status.success() {
            return true;
        }
        // EPERM：进程在，只是不是我的 —— 不能当成"死了"
        return String::from_utf8_lossy(&o.stderr).to_ascii_lowercase().contains("permitted");
    }
    true // 真问不出来才保守当成活着
}

fn fmt_epoch(secs: i64) -> String {
    crate::conn::fmt_epoch(secs)
}

/* ---------------- 包结构 ---------------- */

/// 一个已经打开的包（能访问哪几个项目，就打开哪几个）。
struct Unlocked {
    master: [u8; 32],
    header: Value,
    /// 项目名 → (项目密钥, 该项目明文 JSON)
    projects: BTreeMap<String, ([u8; 32], Value)>,
    index: Value,
}

impl Unlocked {
    fn project(&self, name: &str) -> Result<&Value> {
        self.projects
            .get(name)
            .map(|(_, v)| v)
            .ok_or_else(|| anyhow!("这个项目没被打开：{name}（用 --project {name} 单独打开）"))
    }

    fn entries(&self, name: &str) -> BTreeMap<String, Value> {
        self.project(name)
            .ok()
            .and_then(|v| v["entries"].as_object().cloned())
            .map(|m| m.into_iter().collect())
            .unwrap_or_default()
    }
}

fn read_manifest(dir: &Path) -> Result<hur_core::spec::HurPackage> {
    let p = dir.join(hur_core::spec::MANIFEST);
    let txt = fs::read_to_string(&p).with_context(|| format!("读不了 {}", p.display()))?;
    let pkg: hur_core::spec::HurPackage = serde_json::from_str(&txt).with_context(|| format!("{} 不是合法清单", p.display()))?;
    if pkg.profile_name() != "auth" {
        bail!(
            "这不是授权包（profile={}）—— 授权包要用 `ncc auth pkg init` 建；\n  \
             别的包想装凭据请用环境变量或本机密钥，别塞进包里。",
            pkg.profile_name()
        );
    }
    Ok(pkg)
}

/// 密文文件 = `NCCAUTH1` + 头长度 + 头 JSON。头里装着：KDF 参数、主密钥的每一层包裹、
/// 每个项目一段密文。**没有单独的 body** —— 结构越少，能踩的坑越少。
fn read_header(vault: &[u8]) -> Result<Value> {
    if vault.len() < 12 || &vault[..8] != MAGIC {
        bail!("不是授权包的密文格式（开头不是 NCCAUTH1）");
    }
    let hlen = u32::from_le_bytes([vault[8], vault[9], vault[10], vault[11]]) as usize;
    if vault.len() < 12 + hlen {
        bail!("密文头被截断了");
    }
    serde_json::from_slice(&vault[12..12 + hlen]).map_err(|e| anyhow!("密文头不是 JSON：{e}"))
}

fn pack_vault(header: &Value) -> Result<Vec<u8>> {
    let h = serde_json::to_vec(header)?;
    let mut out = Vec::with_capacity(12 + h.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(h.len() as u32).to_le_bytes());
    out.extend_from_slice(&h);
    Ok(out)
}

fn b64(b: &[u8]) -> String {
    auth::b64url(b)
}

fn unb64(s: &str) -> Vec<u8> {
    auth::b64_decode(s)
}

/* ---------------- 权限检查（"ncc 检查调用权限"） ---------------- */

/// 开锁前的许可判定。**这里是"谁能读"的唯一入口**，别在别处再写一遍。
fn check_permission(dir: &Path, pkg: &hur_core::spec::HurPackage, index: &Value, vault: &[u8], project: Option<&str>) -> Result<()> {
    let Some(a) = pkg.auth.as_ref() else {
        bail!("清单里没有 auth{{}} 声明 —— 这份包不完整（`ncc hur verify {}`）", dir.display());
    };
    // ① 密文被清单钉住（签名覆盖清单，于是这一步等于"签名也盖住了密文"）
    let got = sha256_hex(vault);
    if !a.vault_sha256.trim().is_empty() && a.vault_sha256.trim() != got {
        bail!(
            "密文与清单对不上：\n  清单 {}\n  实际 {got}\n  \
             要么这份包被改过（那就别用它），要么你改了密文却没重新 build + 签（`ncc hur build` → `ncc hur sign`）",
            a.vault_sha256.trim()
        );
    }
    // ② 身份允许列表
    if !a.allow.is_empty() {
        let mine = auth::identity_public()?.map(|(_, fpr)| fpr);
        match mine {
            None => bail!(
                "这份包只允许特定身份打开，而本机没有身份密钥：\n  \
                 先生成一把（`ncc auth key new`），或让发布者把你的指纹加进 auth.allow\n  允许的指纹：{}",
                a.allow.join(" / ")
            ),
            Some(f) if !a.allow.iter().any(|x| x.trim() == f) => bail!(
                "本机身份不在允许列表里：\n  本机 {f}\n  允许 {}\n  \
                 要拿别人的授权包，让发布者把你的指纹加进去（auth.allow），或者用口令解锁。",
                a.allow.join(" / ")
            ),
            Some(_) => {}
        }
    }
    // ③ 项目要真的在包里
    if let Some(p) = project {
        let projects: Vec<&str> = index["projects"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect()).unwrap_or_default();
        if !projects.contains(&p) {
            bail!("这份包里没有项目「{p}」（有的：{}）", if projects.is_empty() { "无".into() } else { projects.join(" / ") });
        }
    }
    Ok(())
}

/// 解锁：先用身份（如果包允许），再用口令。返回主密钥。
fn unwrap_master(pkg: &hur_core::spec::HurPackage, header: &Value, pass: Option<&str>) -> Result<[u8; 32]> {
    let aad = header["aad"].as_str().unwrap_or("").as_bytes().to_vec();
    let salt = unb64(header["kdf"]["salt"].as_str().unwrap_or(""));
    let iters = header["kdf"]["iters"].as_u64().unwrap_or(DEFAULT_ITERS as u64) as u32;
    let wraps = header["wraps"].as_array().cloned().unwrap_or_default();
    let allow: Vec<String> = pkg.auth.as_ref().map(|a| a.allow.clone()).unwrap_or_default();

    for w in &wraps {
        let mode = w["mode"].as_str().unwrap_or("");
        let nonce = unb64(w["nonce"].as_str().unwrap_or(""));
        let ct = unb64(w["ct"].as_str().unwrap_or(""));
        if ct.is_empty() {
            continue;
        }
        match mode {
            "identity" => {
                let Some((der, fpr)) = auth::identity_der()? else { continue };
                // 包允许列表非空时，不认没写在里面的身份（哪怕能解开 —— 那说明包被改过）
                if !allow.is_empty() && !allow.iter().any(|x| x.trim() == fpr) {
                    continue;
                }
                let kek = kek_from_identity(&der, &salt)?;
                if let Ok(m) = open(&kek, &aad, &nonce, &ct) {
                    if m.len() == 32 {
                        let mut out = [0u8; 32];
                        out.copy_from_slice(&m);
                        return Ok(out);
                    }
                }
            }
            "passphrase" => {
                let Some(p) = pass else { continue };
                let kek = kek_from_passphrase(p, &salt, iters)?;
                if let Ok(m) = open(&kek, &aad, &nonce, &ct) {
                    if m.len() == 32 {
                        let mut out = [0u8; 32];
                        out.copy_from_slice(&m);
                        return Ok(out);
                    }
                }
            }
            _ => {}
        }
    }
    let modes: Vec<&str> = wraps.iter().filter_map(|w| w["mode"].as_str()).collect();
    bail!(
        "开不了这份包（试过：{}）。\n  口令不对 / 身份公钥不是这份包认的那把，或者密文被改过。",
        if modes.is_empty() { "没有可用的解锁方式".into() } else { modes.join(" / ") }
    )
}

/// 打开包（可选只打开某几个项目）。
fn unlock(dir: &Path, pass: Option<&str>, want: &[String]) -> Result<Unlocked> {
    let pkg = read_manifest(dir)?;
    let vault = fs::read(dir.join(hur_core::spec::AUTH_VAULT)).with_context(|| "读密文失败")?;
    let index: Value = serde_json::from_slice(&fs::read(dir.join(hur_core::spec::AUTH_INDEX)).with_context(|| "读元数据失败")?)
        .map_err(|e| anyhow!("{} 不是合法 JSON：{e}", hur_core::spec::AUTH_INDEX))?;
    check_permission(dir, &pkg, &index, &vault, want.first().map(String::as_str))?;
    let header = read_header(&vault)?;
    let master = unwrap_master(&pkg, &header, pass)?;
    let salt = unb64(header["kdf"]["salt"].as_str().unwrap_or(""));

    // 项目密文一段一段放在 header.projects[].ct 里（一个项目一段，可单独轮换）
    let pkg_id = pkg_id_of(&pkg);
    let mut projects = BTreeMap::new();
    for p in header["projects"].as_array().cloned().unwrap_or_default() {
        let name = p["name"].as_str().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if !want.is_empty() && !want.iter().any(|w| w == &name) {
            continue;
        }
        let gen = p["gen"].as_u64().unwrap_or(1);
        let kek = project_kek(&master, &salt, &name, gen)?;
        // 项目密钥文件（每个项目一把，可单独轮换）
        let kf = dir.join(hur_core::spec::auth_key_file(&name));
        let kj: Value = serde_json::from_slice(&fs::read(&kf).with_context(|| format!("读项目密钥文件失败：{}", kf.display()))?)
            .map_err(|e| anyhow!("{} 不是合法 JSON：{e}", kf.display()))?;
        if kj["gen"].as_u64().unwrap_or(0) != gen {
            bail!("项目「{name}」的密钥文件与密文代数对不上（文件 gen={}，密文 gen={gen}）—— 包被改过？", kj["gen"].as_u64().unwrap_or(0));
        }
        let pkey = open(&kek, format!("{pkg_id}/key/{name}:{gen}").as_bytes(), &unb64(kj["nonce"].as_str().unwrap_or("")), &unb64(kj["ct"].as_str().unwrap_or("")))?;
        if pkey.len() != 32 {
            bail!("项目「{name}」的密钥长度不对");
        }
        let plain = open(&pkey, format!("{pkg_id}/{name}:{gen}").as_bytes(), &unb64(p["nonce"].as_str().unwrap_or("")), &unb64(p["ct"].as_str().unwrap_or("")))?;
        let v: Value = serde_json::from_slice(&plain).map_err(|e| anyhow!("项目「{name}」的明文不是 JSON：{e}"))?;
        let mut k = [0u8; 32];
        k.copy_from_slice(&pkey);
        projects.insert(name, (k, v));
    }
    Ok(Unlocked { master, header, projects, index })
}

fn pkg_id_of(pkg: &hur_core::spec::HurPackage) -> String {
    format!("{}@{}", pkg.id, pkg.version)
}

/* ---------------- 写回（拿锁、原子、同步清单与锁文件） ---------------- */

/// 把「新的密文 + 新的 index + hur.json 的 auth{} + hur.lock」一次性落盘。
///
/// ⚠️ 顺序要紧：先写密文与密钥文件，再改清单（清单里钉着密文摘要），最后重算 hur.lock。
/// 中途挂了会留下一份"清单与密文对不上"的包 —— 那是**能查出来的**坏（R12 会红），
/// 比"看起来没事其实解不开"强。
fn commit(dir: &Path, pkg: &mut hur_core::spec::HurPackage, header: &Value, index: &Value, extra_keys: &[(String, Vec<u8>)]) -> Result<String> {
    for (name, bytes) in extra_keys {
        write_atomic(&dir.join(hur_core::spec::auth_key_file(name)), bytes, 0o600)?;
    }
    let vault = pack_vault(header)?;
    let digest = sha256_hex(&vault);
    write_atomic(&dir.join(hur_core::spec::AUTH_VAULT), &vault, 0o600)?;
    write_atomic(&dir.join(hur_core::spec::AUTH_INDEX), format!("{}\n", serde_json::to_string_pretty(index)?).as_bytes(), 0o600)?;
    if let Some(a) = pkg.auth.as_mut() {
        a.vault_sha256 = digest.clone();
    }
    write_atomic(&dir.join(hur_core::spec::MANIFEST), format!("{}\n", serde_json::to_string_pretty(pkg)?).as_bytes(), 0o644)?;
    hur_core::pack::build_lock(dir).with_context(|| "重算 hur.lock 失败")?;
    Ok(digest)
}

/* ---------------- 口令输入 ---------------- */

fn read_passphrase(file: Option<&PathBuf>, from_stdin: bool) -> Result<Option<String>> {
    if let Some(f) = file {
        let t = fs::read_to_string(f).with_context(|| format!("读口令文件失败：{}", f.display()))?;
        let s = t.trim_end_matches(['\n', '\r']).to_string();
        if s.is_empty() {
            bail!("口令文件是空的：{}", f.display());
        }
        return Ok(Some(s));
    }
    if from_stdin {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        let s = s.trim_end_matches(['\n', '\r']).to_string();
        if s.is_empty() {
            bail!("stdin 里没有口令");
        }
        return Ok(Some(s));
    }
    Ok(None)
}

/* ---------------- 审计 ---------------- */

fn audit(state: &Path, act: &str, project: &str, entries: &[String], reason: &str, result: &str) {
    let line = json!({
        "at": now_unix(), "act": act, "by": whoami(), "project": project,
        "entries": entries, "reason": reason, "result": result,
        // 红线：**只记条目名，绝不记值**
    });
    let p = state.join("audit.jsonl");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{line}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
    }
}

fn audit_tail(state: &Path, n: usize) -> Vec<Value> {
    let Ok(t) = fs::read_to_string(state.join("audit.jsonl")) else { return Vec::new() };
    let mut v: Vec<Value> = t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if v.len() > n {
        v = v.split_off(v.len() - n);
    }
    v
}

/// 清掉上次异常退出留下的 session（明文只该活在 run 那一段时间里）。
fn sweep_sessions(state: &Path) -> usize {
    let dir = state.join("sessions");
    let Ok(rd) = fs::read_dir(&dir) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| now_unix().saturating_sub(d.as_secs()) > SESSION_TTL_SEC)
            .unwrap_or(false);
        if old && fs::remove_dir_all(&p).is_ok() {
            n += 1;
        }
    }
    n
}

/* ---------------- 子命令 ---------------- */

#[derive(clap::Args)]
pub struct InitArgs {
    /// 包目录（不存在就建）
    pub dir: PathBuf,
    /// 包 id（形如 `@you/team-auth`；不写就按目录名生成 `H-auth-<名>-<随机>`）
    #[arg(long)]
    pub id: Option<String>,
    /// 项目名（可重复）—— 每个项目一份密钥文件、一段独立密文
    #[arg(long = "project", required = true)]
    pub projects: Vec<String>,
    /// 给谁用的 / 干什么的
    #[arg(long, default_value = "")]
    pub note: String,
    /// 口令文件（0600；**故意不支持 --passphrase 直接写在 argv 里**，ps 看得见）
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    /// 从 stdin 读口令（一行）
    #[arg(long)]
    pub passphrase_stdin: bool,
    /// 用本机身份密钥解锁（不加口令）
    #[arg(long)]
    pub identity: bool,
    /// 只允许这些身份公钥指纹打开（可重复，形如 SHA256:abcd…）
    #[arg(long = "allow", requires = "identity")]
    pub allow: Vec<String>,
    /// 口令 KDF 迭代次数
    #[arg(long, default_value_t = DEFAULT_ITERS)]
    pub iters: u32,
}

#[derive(clap::Args)]
pub struct ShowArgs {
    pub dir: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct SetArgs {
    pub dir: PathBuf,
    #[arg(long)]
    pub project: String,
    /// 条目名（大写字母数字下划线）
    #[arg(long)]
    pub entry: String,
    /// 值从 stdin 读（**不提供 --value**：argv 在 ps 里看得见）
    #[arg(long)]
    pub value_stdin: bool,
    /// 值从一个文件读（读到的内容去掉末尾换行）
    #[arg(long)]
    pub value_file: Option<PathBuf>,
    /// secret / token / grant / config
    #[arg(long, default_value = "secret")]
    pub kind: String,
    #[arg(long, default_value = "")]
    pub note: String,
    /// 过期时间：`30d` / `12h` / `0`（不过期）
    #[arg(long, default_value = "0")]
    pub expires: String,
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub passphrase_stdin: bool,
    /// 为什么改（必填）
    #[arg(long)]
    pub reason: String,
    /// 等锁多久（秒；0 = 不等，立刻失败 —— 并发脚本用）
    #[arg(long, default_value_t = 10)]
    pub lock_timeout: u64,
}

#[derive(clap::Args)]
pub struct GetArgs {
    pub dir: PathBuf,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub entry: String,
    /// 打印明文（不打码）。不给就只显示掩码与元数据
    #[arg(long)]
    pub reveal: bool,
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub passphrase_stdin: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct RmArgs {
    pub dir: PathBuf,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub entry: String,
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub passphrase_stdin: bool,
    #[arg(long)]
    pub reason: String,
    /// 等锁多久（秒；0 = 不等，立刻失败）
    #[arg(long, default_value_t = 10)]
    pub lock_timeout: u64,
}

#[derive(clap::Args)]
pub struct RotateArgs {
    pub dir: PathBuf,
    /// 只轮换这个项目的密钥（不给就轮换主密钥 —— 那等于换口令/换身份）
    #[arg(long)]
    pub project: Option<String>,
    /// **当前**口令（解锁用；轮换主密钥时也用它先把包打开）
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub passphrase_stdin: bool,
    /// 轮换主密钥时给**新**口令（不给 = 用同一个口令重新包裹，只换主密钥与盐）
    #[arg(long)]
    pub new_passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub new_passphrase_stdin: bool,
    /// 新的允许身份列表（轮换主密钥时用；不给则沿用）
    #[arg(long = "allow")]
    pub allow: Vec<String>,
    /// 额外加一种身份解锁（原本没有的话）—— 不加就**保持原有的解锁方式**
    #[arg(long)]
    pub with_identity: bool,
    #[arg(long)]
    pub reason: String,
    /// 等锁多久（秒；0 = 不等，立刻失败）
    #[arg(long, default_value_t = 10)]
    pub lock_timeout: u64,
}

#[derive(clap::Args)]
pub struct RunArgs {
    pub dir: PathBuf,
    #[arg(long)]
    pub project: String,
    /// 解锁口令（不给就试本机身份密钥）
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub passphrase_stdin: bool,
    /// 为什么开这一次（必填）
    #[arg(long)]
    pub reason: String,
    /// 要跑的命令（`-- cmd arg…`）
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
    /// 等锁多久（秒；0 = 不等，立刻失败）
    #[arg(long, default_value_t = 30)]
    pub lock_timeout: u64,
}

#[derive(clap::Args)]
pub struct LockArgs {
    pub dir: PathBuf,
    /// 连锁文件一起清（只在确认没有人在用时用）
    #[arg(long)]
    pub force: bool,
}

#[derive(clap::Args)]
pub struct StatusArgs {
    pub dir: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Subcommand)]
pub enum PkgAction {
    /// 建一份授权包：定项目、定解锁方式（口令 / 本机身份）
    Init(InitArgs),
    /// 看元数据（项目 / 条目名 / 过期 / 代数）—— **不需要口令，也看不到值**
    Show(ShowArgs),
    /// 写一条（值从 stdin 或文件读）
    Set(SetArgs),
    /// 读一条（默认打码）
    Get(GetArgs),
    /// 删一条
    Rm(RmArgs),
    /// 轮换密钥：--project 换项目密钥；不给就是换主密钥（口令/身份）
    Rotate(RotateArgs),
    /// 开锁 → 跑一条命令（值从 env 与一个 0600 文件给）→ 抹掉明文 → 上锁
    Run(RunArgs),
    /// 清残留（session / 死锁）；--force 连锁文件一起清
    Lock(LockArgs),
    /// 本地状态：锁、审计尾巴、session 残留
    Status(StatusArgs),
}

pub fn cmd(action: &PkgAction) -> Result<()> {
    match action {
        PkgAction::Init(a) => init(a),
        PkgAction::Show(a) => show(a),
        PkgAction::Set(a) => set(a),
        PkgAction::Get(a) => get(a),
        PkgAction::Rm(a) => rm(a),
        PkgAction::Rotate(a) => rotate(a),
        PkgAction::Run(a) => run(a),
        PkgAction::Lock(a) => lock_cmd(a),
        PkgAction::Status(a) => status(a),
    }
}

fn init(a: &InitArgs) -> Result<()> {
    let dir = &a.dir;
    if dir.join(hur_core::spec::AUTH_VAULT).exists() {
        bail!("{} 里已经有一份授权包了（要改项目用 `ncc auth pkg rotate`，要看用 `show`）", dir.display());
    }
    for p in &a.projects {
        if !hur_core::spec::auth_project_ok(p) {
            bail!("项目名「{p}」不合法（只允许小写字母 / 数字 / - _，且要当文件名用）");
        }
    }
    let pass = read_passphrase(a.passphrase_file.as_ref(), a.passphrase_stdin)?;
    let use_identity = a.identity || (pass.is_none() && a.allow.is_empty());
    if pass.is_none() && !use_identity {
        bail!(
            "没说怎么开锁：给 `--passphrase-file <文件>` / `--passphrase-stdin`，或 `--identity`（用本机身份密钥）。\n  \
             （故意不支持把口令写在 argv 里 —— ps 里看得见。）"
        );
    }
    let iters = a.iters.max(MIN_ITERS);

    let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("team-auth");
    // 包 id 有规范：kind=harness 时必须以 `H-` 开头（R1）。用户想写 `@me/team-auth` 也合理
    // —— 那就把它当**对外引用**（publish.namespace + slug），id 按规范生成，两边都说清楚。
    let want = a.id.clone().unwrap_or_else(|| name.to_string());
    let tail = want.rsplit('/').next().unwrap_or(&want).trim_start_matches('@').to_string();
    let slug: String = tail
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let slug = if slug.is_empty() { "team-auth".to_string() } else { slug };
    let (ns, id) = if want.starts_with("H-") {
        (String::new(), want.clone())
    } else {
        let ns = if want.starts_with('@') { want.split('/').next().unwrap_or("").to_string() } else { String::new() };
        (ns, format!("H-{slug}-{}", ncc_rand6()))
    };
    let pkg_name = slug.clone();

    // 先建目录骨架
    ensure_dir_700(dir)?;
    ensure_dir_700(&dir.join("auth/keys"))?;
    ensure_dir_700(&dir.join("src"))?;
    // R3 要 kind=harness 带一份面面：授权包的“面”就是它的授权契约（给第三方读的）
    let schema = json!({
        "auth": {
            "profile": "auth",
            "projects": a.projects,
            "unlock": if use_identity && pass.is_some() { "passphrase+identity" } else if use_identity { "identity" } else { "passphrase" },
            "entries": "每个项目下是 条目名 → {value,kind,note,expiresUnix}；值只在 auth/vault.enc 里",
            "read": "ncc auth pkg get <包> --project <项目> --entry <条目> [--reveal]",
            "use": "ncc auth pkg run <包> --project <项目> --reason \"…\" -- <命令>",
        }
    });
    write_atomic(&dir.join("src/auth.schema.json"), format!("{}\n", serde_json::to_string_pretty(&schema)?).as_bytes(), 0o644)?;
    let pkg = hur_core::spec::HurPackage {
        spec: hur_core::spec::PKG_SPEC.to_string(),
        kind: "harness".into(),
        profile: Some("auth".into()),
        id: id.clone(),
        name: pkg_name.clone(),
        version: "0.1.0".into(),
        short: "AUTH".into(),
        domain: "auth".into(),
        summary: a.note.clone(),
        entry: String::new(),
        runtime: "local-v0".into(),
        capabilities: vec![],
        deps: Default::default(),
        permissions: Default::default(),
        publish: hur_core::spec::PublishInfo {
            registry: String::new(),
            namespace: ns,
            visibility: "private".into(),
            slug: slug.clone(),
        },
        agent: None,
        data: None,
        state: None,
        security: None,
        egress: None,
        auth: Some(hur_core::spec::AuthDecl {
            projects: a.projects.clone(),
            wrap: {
                let mut w = Vec::new();
                if pass.is_some() {
                    w.push("passphrase".into());
                }
                if use_identity {
                    w.push("identity".into());
                }
                w
            },
            allow: a.allow.clone(),
            kdf_iters: iters as u64,
            vault_sha256: String::new(),
            created_at_unix: now_unix(),
            note: a.note.clone(),
        }),
    };

    // 主密钥 + 每个项目一把密钥
    let salt = rand_bytes(16)?;
    let master = rand_bytes(32)?;
    // ⚠️ AAD 必须与 unlock 那边逐字节一致（都用 `包 id@版本`）—— 这里曾经写成只用 id，
    // 结果建得起来、开不开（AEAD 的 tag 直接把不匹配变成“解不开”，不告诉你是哪一段）。
    let aad = pkg_id_of(&pkg);
    let mut wraps = Vec::new();
    if let Some(p) = &pass {
        let kek = kek_from_passphrase(p, &salt, iters)?;
        let (nonce, ct) = seal(&kek, aad.as_bytes(), &master)?;
        wraps.push(json!({"mode":"passphrase","nonce":b64(&nonce),"ct":b64(&ct)}));
    }
    if use_identity {
        let (der, fpr) = auth::identity_der()?.ok_or_else(|| {
            anyhow!("要用身份解锁，但本机还没有身份密钥 —— 先生成一把：`ncc auth key new`")
        })?;
        let kek = kek_from_identity(&der, &salt)?;
        let (nonce, ct) = seal(&kek, aad.as_bytes(), &master)?;
        wraps.push(json!({"mode":"identity","fpr":fpr,"nonce":b64(&nonce),"ct":b64(&ct)}));
    }
    let header = json!({
        "v": 1,
        "cipher": "chacha20poly1305",
        "aad": aad,
        "kdf": {"algo": "pbkdf2-hmac-sha256", "iters": iters, "salt": b64(&salt)},
        "wraps": wraps,
        "projects": [],
    });
    let mut header = header;
    let mut index_projects = Vec::new();
    let mut keys: Vec<(String, Vec<u8>)> = Vec::new();
    let mut projs = Vec::new();
    for p in &a.projects {
        let pkey = rand_bytes(32)?;
        let gen = 1u64;
        let kek = project_kek(&master, &salt, p, gen)?;
        let (knonce, kct) = seal(&kek, format!("{aad}/key/{p}:{gen}").as_bytes(), &pkey)?;
        keys.push((
            p.clone(),
            serde_json::to_vec(&json!({"v":1,"project":p,"gen":gen,"nonce":b64(&knonce),"ct":b64(&kct)}))?,
        ));
        let empty = json!({"v":1,"project":p,"entries":{}});
        let (nonce, ct) = seal(&pkey, format!("{aad}/{p}:{gen}").as_bytes(), &serde_json::to_vec(&empty)?)?;
        projs.push(json!({"name":p,"gen":gen,"nonce":b64(&nonce),"ct":b64(&ct)}));
        index_projects.push(json!({
            "name": p, "gen": gen, "revision": 0, "updatedAtUnix": now_unix(), "bytes": 0, "entries": [],
        }));
    }
    header["projects"] = Value::Array(projs);
    let index = json!({
        "v": 1, "package": pkg_id_of(&pkg), "createdAtUnix": now_unix(), "updatedAtUnix": now_unix(),
        "revision": 0, "cipher": "chacha20poly1305",
        "kdf": {"algo":"pbkdf2-hmac-sha256","iters":iters},
        "wrap": pkg.auth.as_ref().map(|x| x.wrap.clone()).unwrap_or_default(),
        "allow": a.allow,
        "projects": index_projects,
        "note": "这份文件是**明文元数据**：项目、条目名、过期与代数。值全部在 auth/vault.enc 里。",
    });
    let mut pkg_mut = pkg;
    let digest = commit(dir, &mut pkg_mut, &header, &index, &keys)?;
    println!("✅ 授权包已建好 {}", dir.display());
    println!("   包 id    {}", pkg_mut.id);
    if !pkg_mut.publish.namespace.is_empty() || !pkg_mut.publish.slug.is_empty() {
        println!("   对外引用 {}/{}", pkg_mut.publish.namespace, pkg_mut.publish.slug);
    }
    println!("   项目     {}", a.projects.join(" · "));
    println!(
        "   开锁     {}{}",
        pkg_mut.auth.as_ref().map(|x| x.wrap.join(" + ")).unwrap_or_default(),
        if pkg_mut.auth.as_ref().map(|x| x.allow.is_empty()).unwrap_or(true) { "" } else { "（限定身份）" }
    );
    println!("   密文     {}…（{} 字节，sha256 {}…）", hur_core::spec::AUTH_VAULT, fs::metadata(dir.join(hur_core::spec::AUTH_VAULT))?.len(), &digest[..12]);
    println!("\n下一步");
    println!("   ncc auth pkg set {} --project {} --entry DB_URL --value-stdin --reason \"加一条\"", dir.display(), a.projects[0]);
    println!("   ncc auth pkg run {} --project {} --reason \"发版\" -- ./deploy.sh", dir.display(), a.projects[0]);
    println!("   （改了包就别忘了重新 build + 签：`ncc hur build {0}` → `ncc hur sign {0}`）", dir.display());
    Ok(())
}

fn ncc_rand6() -> String {
    rand_bytes(3).map(|b| hex(&b)).unwrap_or_else(|_| "000000".into())
}

fn show(a: &ShowArgs) -> Result<()> {
    let pkg = read_manifest(&a.dir)?;
    let index: Value = serde_json::from_slice(&fs::read(a.dir.join(hur_core::spec::AUTH_INDEX))?)?;
    let state = state_dir(&pkg_id_of(&pkg));
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "package": pkg_id_of(&pkg), "auth": pkg.auth, "index": index,
                "state": state.to_string_lossy(),
                "note": "show 只读元数据：不解密、不拿锁、不碰值。",
            }))?
        );
        return Ok(());
    }
    println!("{}  （profile=auth）", pkg_id_of(&pkg));
    if let Some(n) = pkg.auth.as_ref().map(|x| x.note.clone()).filter(|s| !s.is_empty()) {
        println!("   用途     {n}");
    }
    println!(
        "   开锁     {}   KDF 迭代 {}",
        pkg.auth.as_ref().map(|x| x.wrap.join(" + ")).unwrap_or_default(),
        pkg.auth.as_ref().map(|x| x.kdf_iters).unwrap_or(0)
    );
    let allow = pkg.auth.as_ref().map(|x| x.allow.clone()).unwrap_or_default();
    println!(
        "   允许身份 {}",
        if allow.is_empty() { "不限（只认口令）".to_string() } else { allow.join(" · ") }
    );
    println!("   密文     {}   本地状态 {}", hur_core::spec::AUTH_VAULT, state.display());
    println!("\n   项目（明文元数据，值不在这里）");
    for p in index["projects"].as_array().cloned().unwrap_or_default() {
        let name = p["name"].as_str().unwrap_or("?");
        let entries = index["projects"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(name)))
            .and_then(|x| x["entries"].as_array().cloned())
            .unwrap_or_default();
        println!("     {name:<16} gen {} · rev {} · 更新 {}", p["gen"].as_u64().unwrap_or(0), p["revision"].as_u64().unwrap_or(0), p["updatedAtUnix"].as_u64().map(|t| fmt_epoch(t as i64)).unwrap_or_default());
        for e in entries {
            let exp = e["expiresUnix"].as_u64().unwrap_or(0);
            let tag = if exp == 0 {
                String::new()
            } else if exp <= now_unix() {
                format!("  ⚠ 已过期（{}）", fmt_epoch(exp as i64))
            } else {
                format!("  到 {}", fmt_epoch(exp as i64))
            };
            println!(
                "       {:<28} {:<8} {}{}",
                e["name"].as_str().unwrap_or(""),
                e["kind"].as_str().unwrap_or(""),
                e["bytes"].as_u64().map(|b| format!("{b} 字节 ")).unwrap_or_default(),
                tag
            );
        }
    }
    println!("\n   值要开锁才看得到：`ncc auth pkg get {} --project <项目> --entry <条目> --reveal`", a.dir.display());
    Ok(())
}

/// 解析 `30d` / `12h` / `45m` / `0`。
fn parse_expires(s: &str) -> Result<u64> {
    let t = s.trim();
    if t.is_empty() || t == "0" {
        return Ok(0);
    }
    let (num, unit) = t.split_at(t.len() - 1);
    let n: u64 = num.trim().parse().map_err(|_| anyhow!("过期时间「{s}」看不懂（写 30d / 12h / 45m / 0）"))?;
    let secs = match unit {
        "d" => n * 86400,
        "h" => n * 3600,
        "m" => n * 60,
        "s" => n,
        _ => bail!("过期时间「{s}」的单位不认识（可选 d / h / m / s；0 = 不过期）"),
    };
    Ok(now_unix() + secs)
}

fn check_entry_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() || n.len() > 64 {
        bail!("条目名要 1~64 个字符");
    }
    if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        bail!("条目名「{n}」只允许字母 / 数字 / _ - .（它要当环境变量名用）");
    }
    Ok(())
}

fn set(a: &SetArgs) -> Result<()> {
    let value = if let Some(f) = &a.value_file {
        fs::read_to_string(f)?.trim_end_matches(['\n', '\r']).to_string()
    } else if a.value_stdin {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s.trim_end_matches(['\n', '\r']).to_string()
    } else {
        bail!("值从哪儿来：`--value-stdin`（管道）或 `--value-file <文件>`（故意不提供 --value：argv 在 ps 里看得见）")
    };
    if value.is_empty() {
        bail!("值是空的（要删一条用 `ncc auth pkg rm`）");
    }
    check_entry_name(&a.entry)?;
    let pass = read_passphrase(a.passphrase_file.as_ref(), a.passphrase_stdin)?;

    let mut pkg = read_manifest(&a.dir)?;
    let state = state_dir(&pkg_id_of(&pkg));
    sweep_sessions(&state);
    let _guard = VaultLock::acquire(&state, "set", &a.reason, a.lock_timeout)?;
    if _guard.reclaimed {
        println!("  ! 抢回了一把没主的锁（上一个动作大概挂了）");
    }
    let want = vec![a.project.clone()];
    let mut un = unlock(&a.dir, pass.as_deref(), &want)?;
    let entries_now = un.entries(&a.project);
    if entries_now.contains_key(a.entry.trim()) {
        // 覆盖写也要留痕：值变了，但审计里只记名字
        audit(&state, "overwrite", &a.project, &[a.entry.clone()], &a.reason, "ok");
    }
    let exp = parse_expires(&a.expires)?;
    let (pkey, mut plain) = un.projects.remove(&a.project).ok_or_else(|| anyhow!("项目「{}」没打开", a.project))?;
    let obj = plain["entries"].as_object_mut().ok_or_else(|| anyhow!("项目明文结构不对"))?;
    obj.insert(
        a.entry.trim().to_string(),
        json!({"value": value, "kind": a.kind.trim(), "note": a.note.trim(), "expiresUnix": exp, "updatedAtUnix": now_unix()}),
    );
    let gen = header_gen(&un.header, &a.project);
    let pkg_id = pkg_id_of(&pkg);
    let (nonce, ct) = seal(&pkey, format!("{pkg_id}/{}:{gen}", a.project).as_bytes(), &serde_json::to_vec(&plain)?)?;

    let mut header = un.header.clone();
    for p in header["projects"].as_array_mut().unwrap() {
        if p["name"].as_str() == Some(a.project.as_str()) {
            p["nonce"] = json!(b64(&nonce));
            p["ct"] = json!(b64(&ct));
        }
    }
    let mut index = un.index.clone();
    let kb = hint(&a.dir, &a.project)?;
    let rev = bump_index(&mut index, &a.project, &plain, &kb, None);
    let digest = commit(&a.dir, &mut pkg, &header, &index, &[])?;
    audit(&state, "set", &a.project, &[a.entry.clone()], &a.reason, "ok");
    println!("✅ 已写入 {}/{}（{} 字节 · 第 {rev} 版 · 密文 {}…）", a.project, a.entry, value.len(), &digest[..12]);
    println!("   提醒：包内容变了 —— 重新 `ncc hur build {}` 再 `ncc hur sign {}`，别人才能核对", a.dir.display(), a.dir.display());
    Ok(())
}

fn header_gen(header: &Value, project: &str) -> u64 {
    header["projects"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["name"].as_str() == Some(project)))
        .and_then(|p| p["gen"].as_u64())
        .unwrap_or(1)
}

/// 项目密钥文件的字节数（index 里显示用）。
fn hint(dir: &Path, project: &str) -> Result<u64> {
    Ok(fs::metadata(dir.join(hur_core::spec::auth_key_file(project))).map(|m| m.len()).unwrap_or(0))
}

/// 更新 index：条目清单 + revision + 时间；`gen` 给了就一起改（轮换用）。
fn bump_index(index: &mut Value, project: &str, plain: &Value, key_bytes: &u64, new_gen: Option<u64>) -> u64 {
    let entries: Vec<Value> = plain["entries"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    json!({
                        "name": k,
                        "kind": v["kind"].as_str().unwrap_or("secret"),
                        "bytes": v["value"].as_str().map(|s| s.len()).unwrap_or(0),
                        "expiresUnix": v["expiresUnix"].as_u64().unwrap_or(0),
                        "updatedAtUnix": v["updatedAtUnix"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut rev = 0;
    if let Some(arr) = index["projects"].as_array_mut() {
        for p in arr.iter_mut() {
            if p["name"].as_str() == Some(project) {
                rev = p["revision"].as_u64().unwrap_or(0) + 1;
                p["revision"] = json!(rev);
                p["updatedAtUnix"] = json!(now_unix());
                p["entries"] = Value::Array(entries.clone());
                p["bytes"] = json!(key_bytes);
                if let Some(g) = new_gen {
                    p["gen"] = json!(g);
                }
            }
        }
    }
    let total = index["revision"].as_u64().unwrap_or(0) + 1;
    index["revision"] = json!(total);
    index["updatedAtUnix"] = json!(now_unix());
    rev
}

fn get(a: &GetArgs) -> Result<()> {
    let pass = read_passphrase(a.passphrase_file.as_ref(), a.passphrase_stdin)?;
    let pkg = read_manifest(&a.dir)?;
    let un = unlock(&a.dir, pass.as_deref(), &[a.project.clone()])?;
    let entries = un.entries(&a.project);
    let Some(e) = entries.get(a.entry.trim()) else {
        bail!(
            "项目「{}」里没有条目「{}」（有的：{}）",
            a.project,
            a.entry,
            if entries.is_empty() { "无".into() } else { entries.keys().cloned().collect::<Vec<_>>().join(" / ") }
        );
    };
    let exp = e["expiresUnix"].as_u64().unwrap_or(0);
    if exp > 0 && exp <= now_unix() {
        bail!("条目「{}」已于 {} 过期 —— 换一条新的，或者让发布者更新这份包", a.entry, fmt_epoch(exp as i64));
    }
    let value = e["value"].as_str().unwrap_or("");
    let state = state_dir(&pkg_id_of(&pkg));
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": a.project, "entry": a.entry, "kind": e["kind"], "note": e["note"],
                "expiresUnix": exp, "bytes": value.len(),
                "value": if a.reveal { json!(value) } else { Value::Null },
            }))?
        );
    } else if a.reveal {
        println!("{value}");
    } else {
        println!(
            "{}（{}）{} · {} 字节 · 更新 {}",
            a.entry,
            e["kind"].as_str().unwrap_or("secret"),
            if e["note"].as_str().unwrap_or("").is_empty() { String::new() } else { format!(" · {}", e["note"].as_str().unwrap_or("")) },
            value.len(),
            e["updatedAtUnix"].as_u64().map(|t| fmt_epoch(t as i64)).unwrap_or_default()
        );
        println!("   值     {}（要看明文加 `--reveal`）", mask(value));
    }
    audit(&state, if a.reveal { "get-reveal" } else { "get" }, &a.project, &[a.entry.clone()], "", "ok");
    Ok(())
}

fn mask(v: &str) -> String {
    let n = v.chars().count();
    if n <= 4 {
        "•".repeat(n.max(1))
    } else {
        format!("{}…{}", &v.chars().take(2).collect::<String>(), "•".repeat((n - 4).min(12)))
    }
}

fn rm(a: &RmArgs) -> Result<()> {
    let pass = read_passphrase(a.passphrase_file.as_ref(), a.passphrase_stdin)?;
    let mut pkg = read_manifest(&a.dir)?;
    let state = state_dir(&pkg_id_of(&pkg));
    let _guard = VaultLock::acquire(&state, "rm", &a.reason, a.lock_timeout)?;
    let mut un = unlock(&a.dir, pass.as_deref(), &[a.project.clone()])?;
    let (pkey, mut plain) = un.projects.remove(&a.project).ok_or_else(|| anyhow!("项目「{}」没打开", a.project))?;
    let obj = plain["entries"].as_object_mut().ok_or_else(|| anyhow!("项目明文结构不对"))?;
    if obj.remove(a.entry.trim()).is_none() {
        bail!("项目「{}」里没有条目「{}」", a.project, a.entry);
    }
    let gen = header_gen(&un.header, &a.project);
    let pkg_id = pkg_id_of(&pkg);
    let (nonce, ct) = seal(&pkey, format!("{pkg_id}/{}:{gen}", a.project).as_bytes(), &serde_json::to_vec(&plain)?)?;
    let mut header = un.header.clone();
    for p in header["projects"].as_array_mut().unwrap() {
        if p["name"].as_str() == Some(a.project.as_str()) {
            p["nonce"] = json!(b64(&nonce));
            p["ct"] = json!(b64(&ct));
        }
    }
    let mut index = un.index.clone();
    let kb = hint(&a.dir, &a.project)?;
    let rev = bump_index(&mut index, &a.project, &plain, &kb, None);
    commit(&a.dir, &mut pkg, &header, &index, &[])?;
    audit(&state, "rm", &a.project, &[a.entry.clone()], &a.reason, "ok");
    println!("✅ 已删除 {}/{}（第 {rev} 版）", a.project, a.entry);
    println!("   注意：旧值可能还在 hur.lock / 历史副本里 —— 真正的轮换请用 `ncc auth pkg rotate --project {}`", a.project);
    Ok(())
}

fn rotate(a: &RotateArgs) -> Result<()> {
    let mut pkg = read_manifest(&a.dir)?;
    let state = state_dir(&pkg_id_of(&pkg));
    let _guard = VaultLock::acquire(&state, "rotate", &a.reason, a.lock_timeout)?;
    let pass = read_passphrase(a.passphrase_file.as_ref(), a.passphrase_stdin)?;

    match a.project.clone() {
        Some(p) => {
            // —— 项目密钥轮换：换一把项目密钥，重加密该项目的密文（其它项目不动）
            let mut un = unlock(&a.dir, pass.as_deref(), &[p.clone()])?;
            let (_, plain) = un.projects.remove(&p).ok_or_else(|| anyhow!("项目「{p}」没打开"))?;
            let salt = unb64(un.header["kdf"]["salt"].as_str().unwrap_or(""));
            let gen = header_gen(&un.header, &p) + 1;
            let pkey = rand_bytes(32)?;
            let pkg_id = pkg_id_of(&pkg);
            let kek = project_kek(&un.master, &salt, &p, gen)?;
            let (knonce, kct) = seal(&kek, format!("{pkg_id}/key/{p}:{gen}").as_bytes(), &pkey)?;
            let (nonce, ct) = seal(&pkey, format!("{pkg_id}/{p}:{gen}").as_bytes(), &serde_json::to_vec(&plain)?)?;
            let mut header = un.header.clone();
            for q in header["projects"].as_array_mut().unwrap() {
                if q["name"].as_str() == Some(p.as_str()) {
                    q["gen"] = json!(gen);
                    q["nonce"] = json!(b64(&nonce));
                    q["ct"] = json!(b64(&ct));
                }
            }
            let mut index = un.index.clone();
            let keyrev = bump_index(&mut index, &p, &plain, &0, Some(gen));
            let keyfile = serde_json::to_vec(&json!({"v":1,"project":p,"gen":gen,"nonce":b64(&knonce),"ct":b64(&kct)}))?;
            let digest = commit(&a.dir, &mut pkg, &header, &index, &[(p.clone(), keyfile)])?;
            audit(&state, "rotate-project", &p, &[], &a.reason, "ok");
            println!("✅ 项目「{p}」的密钥已轮换到 gen {gen}（第 {keyrev} 版 · 密文 {}…）", &digest[..12]);
            println!("   旧密钥文件已被替换：拿旧文件打不开新密文（这才是轮换）");
        }
        None => {
            // —— 主密钥轮换：换口令 / 换允许身份。所有项目密钥都要重新包一遍（项目密钥本身不变）
            //
            // ⚠️ 两个口令不是一回事：`--passphrase-file` 是**现在**的口令（先把包打开），
            // `--new-passphrase-file` 是**新**口令。一开始只收一个，结果拿新口令去开旧包，
            // 永远解不开（而错误被脚本吞了，看着像“轮换成功但没生效”）。
            let new_pass = read_passphrase(a.new_passphrase_file.as_ref(), a.new_passphrase_stdin)?;
            let un = unlock(&a.dir, pass.as_deref(), &[])?;
            let salt = rand_bytes(16)?;
            let master = rand_bytes(32)?;
            let iters = pkg.auth.as_ref().map(|x| x.kdf_iters.max(MIN_ITERS as u64) as u32).unwrap_or(DEFAULT_ITERS);
            let pkg_id = pkg_id_of(&pkg);
            let aad = pkg_id.clone();
            // 保留**原有的解锁方式**：轮换主密钥不该偷偷多出一种开门方式
            // （一开始只要是本机有身份密钥就顺手加一条 identity wrap ——
            // 结果“换了口令”之后旧口令居然还能开，因为开的是那条身份）。
            // 想加身份就显式 `--with-identity`。
            let mut modes: Vec<String> = pkg.auth.as_ref().map(|x| x.wrap.clone()).unwrap_or_default();
            if a.with_identity && !modes.iter().any(|m| m == "identity") {
                modes.push("identity".into());
            }
            let mut allow = if a.allow.is_empty() { pkg.auth.as_ref().map(|x| x.allow.clone()).unwrap_or_default() } else { a.allow.clone() };
            let mut wraps = Vec::new();
            if modes.iter().any(|m| m == "passphrase") {
                let p = new_pass.clone().or_else(|| pass.clone()).ok_or_else(|| {
                    anyhow!(
                        "这份包原本用口令解锁，轮换主密钥时得告诉我要用哪个口令：\n  \
                         `--new-passphrase-file <新口令>`（不给就用当前口令重新包裹）"
                    )
                })?;
                let kek = kek_from_passphrase(&p, &salt, iters)?;
                let (n1, c1) = seal(&kek, aad.as_bytes(), &master)?;
                wraps.push(json!({"mode":"passphrase","nonce":b64(&n1),"ct":b64(&c1)}));
            }
            if modes.iter().any(|m| m == "identity") {
                match auth::identity_der()? {
                    Some((der, fpr)) if allow.is_empty() || allow.iter().any(|x| x.trim() == fpr) => {
                        let kek2 = kek_from_identity(&der, &salt)?;
                        let (n2, c2) = seal(&kek2, aad.as_bytes(), &master)?;
                        wraps.push(json!({"mode":"identity","fpr":fpr,"nonce":b64(&n2),"ct":b64(&c2)}));
                    }
                    Some(_) => {
                        bail!(
                            "这份包声明了身份解锁，但本机身份不在 auth.allow 里（{}）——\n  \
                             要么在允许那台机器上轮换，要么先 `--allow <本机指纹>`（或者把 identity 从 wrap 里去掉）",
                            allow.join(" / ")
                        );
                    }
                    None => bail!("这份包声明了身份解锁，但本机没有身份密钥 —— 先 `ncc auth key new --offline`"),
                }
            }
            if wraps.is_empty() {
                bail!("轮换之后就没有任何解锁方式了（auth.wrap={:?}）—— 先修清单里的 wrap", modes);
            }
            let _ = &mut allow;
            // 项目密钥重新用新主密钥派生（项目密钥文件与新 KEK 一起写）
            let mut keys = Vec::new();
            let mut projects = Vec::new();
            let mut index_projects = Vec::new();
            let names: Vec<String> = un.header["projects"]
                .as_array()
                .map(|a| a.iter().filter_map(|p| p["name"].as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            for name in &names {
                let plain = un.project(name)?.clone();
                let gen = header_gen(&un.header, name);
                let pkey = rand_bytes(32)?;
                let kk = project_kek(&master, &salt, name, gen)?;
                let (knonce, kct) = seal(&kk, format!("{pkg_id}/key/{name}:{gen}").as_bytes(), &pkey)?;
                keys.push((name.clone(), serde_json::to_vec(&json!({"v":1,"project":name,"gen":gen,"nonce":b64(&knonce),"ct":b64(&kct)}))?));
                let (nonce, ct) = seal(&pkey, format!("{pkg_id}/{name}:{gen}").as_bytes(), &serde_json::to_vec(&plain)?)?;
                projects.push(json!({"name":name,"gen":gen,"nonce":b64(&nonce),"ct":b64(&ct)}));
                index_projects.push(json!({
                    "name": name, "gen": gen,
                    "revision": un.index["projects"].as_array().and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(name.as_str()))).and_then(|x| x["revision"].as_u64()).unwrap_or(0),
                    "updatedAtUnix": now_unix(),
                    "bytes": 0,
                    "entries": un.index["projects"].as_array().and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(name.as_str()))).and_then(|x| x["entries"].as_array().cloned()).map(Value::Array).unwrap_or_else(|| json!([])),
                }));
            }
            let header = json!({
                "v": 1, "cipher": "chacha20poly1305", "aad": aad,
                "kdf": {"algo":"pbkdf2-hmac-sha256","iters":iters,"salt":b64(&salt)},
                "wraps": wraps, "projects": projects,
            });
            if let Some(ad) = pkg.auth.as_mut() {
                ad.wrap = modes.clone();
                ad.allow = allow;
                ad.kdf_iters = iters as u64;
            }
            let index = json!({
                "v": 1, "package": pkg_id, "createdAtUnix": un.index["createdAtUnix"].as_u64().unwrap_or(now_unix()),
                "updatedAtUnix": now_unix(), "revision": un.index["revision"].as_u64().unwrap_or(0) + 1,
                "cipher": "chacha20poly1305", "kdf": {"algo":"pbkdf2-hmac-sha256","iters":iters},
                "wrap": pkg.auth.as_ref().map(|x| x.wrap.clone()).unwrap_or_default(),
                "allow": pkg.auth.as_ref().map(|x| x.allow.clone()).unwrap_or_default(),
                "projects": index_projects,
                "note": "这份文件是**明文元数据**：项目、条目名、过期与代数。值全部在 auth/vault.enc 里。",
            });
            commit(&a.dir, &mut pkg, &header, &index, &keys)?;
            audit(&state, "rotate-master", "", &[], &a.reason, "ok");
            println!("✅ 主密钥已轮换（所有项目密钥重新包裹；值本身没变）");
            println!("   旧口令打不开这份包了 —— 这才是轮换");
        }
    }
    println!("   提醒：`ncc hur build {}` 之后重新签一次", a.dir.display());
    Ok(())
}

fn run(a: &RunArgs) -> Result<()> {
    if a.cmd.is_empty() {
        bail!("要跑什么：`ncc auth pkg run <包目录> --project <项目> --reason \"…\" -- <命令> [参数…]`");
    }
    let pass = read_passphrase(a.passphrase_file.as_ref(), a.passphrase_stdin)?;
    let pkg = read_manifest(&a.dir)?;
    let state = state_dir(&pkg_id_of(&pkg));
    let swept = sweep_sessions(&state);
    if swept > 0 {
        println!("  ! 清掉 {swept} 个上次漏下的明文 session 目录");
    }
    // 打开 → 跑 → 抹掉：整个窗口都在锁里，别的动作进不来
    let guard = VaultLock::acquire(&state, "run", &a.reason, a.lock_timeout)?;
    if guard.reclaimed {
        println!("  ! 抢回了一把没主的锁（上一个动作大概挂了）");
    }
    let un = unlock(&a.dir, pass.as_deref(), &[a.project.clone()])?;
    let entries = un.entries(&a.project);
    let now = now_unix();
    let mut live = BTreeMap::new();
    let mut expired = Vec::new();
    for (k, v) in &entries {
        let exp = v["expiresUnix"].as_u64().unwrap_or(0);
        if exp > 0 && exp <= now {
            expired.push(k.clone());
            continue;
        }
        live.insert(k.clone(), v["value"].as_str().unwrap_or("").to_string());
    }
    if !expired.is_empty() {
        println!("  ! 跳过 {} 条过期条目：{}", expired.len(), expired.join(" / "));
    }
    if live.is_empty() {
        bail!("项目「{}」里没有可用的条目（{} 条已过期）—— 先 `set` 一条新的", a.project, expired.len());
    }

    // 明文只落在一个 0700 的 session 目录里，且进程退出就抹
    let sid = format!("{}-{}", std::process::id(), now);
    let session = state.join("sessions").join(&sid);
    ensure_dir_700(&session)?;
    let bundle = session.join("bundle.json");
    let bundle_json = json!({
        "package": pkg_id_of(&pkg), "project": a.project, "openedAt": now,
        "reason": a.reason, "by": whoami(),
        "entries": live,
    });
    write_atomic(&bundle, format!("{}\n", serde_json::to_string_pretty(&bundle_json)?).as_bytes(), 0o600)?;

    let mut child = std::process::Command::new(&a.cmd[0]);
    child.args(&a.cmd[1..]);
    child.env("NCC_AUTH_PACKAGE", pkg_id_of(&pkg));
    child.env("NCC_AUTH_PROJECT", &a.project);
    child.env("NCC_AUTH_BUNDLE", &bundle);
    child.env("NCC_AUTH_DIR", &state);
    for (k, v) in &live {
        child.env(format!("NCC_AUTH_{}", k.to_ascii_uppercase().replace(['-', '.'], "_")), v);
    }
    println!(
        "🔓 已开锁：{}/{}（{} 条）→ 跑 `{}`\n   （明文只在 session 里：{}，退出即抹；值在 env NCC_AUTH_<条目> 与 NCC_AUTH_BUNDLE）",
        pkg_id_of(&pkg),
        a.project,
        live.len(),
        a.cmd.join(" "),
        session.display()
    );
    let status = child.status();
    // 不管成没成都先抹：这是"调用结束后锁包"这条要求本身
    let wiped = fs::remove_dir_all(&session).is_ok();
    let code = status.as_ref().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    let err = status.err().map(|e| e.to_string()).unwrap_or_default();
    audit(&state, "run", &a.project, &live.keys().cloned().collect::<Vec<_>>(), &a.reason, if code == 0 { "ok" } else { "failed" });
    drop(guard); // 上锁（锁文件删掉，别人可以进来）
    if !wiped {
        bail!("明文 session 没抹干净：{} —— 请手动删掉（`ncc auth pkg lock {} --force`）", session.display(), a.dir.display());
    }
    if !err.is_empty() {
        bail!("起不了命令：{err}");
    }
    println!("🔒 已上锁（明文已抹 · 审计已记条目名，没记值）");
    if code != 0 {
        std::process::exit(if code > 0 { code } else { 1 });
    }
    Ok(())
}

fn lock_cmd(a: &LockArgs) -> Result<()> {
    let pkg = read_manifest(&a.dir)?;
    let state = state_dir(&pkg_id_of(&pkg));
    let swept = sweep_sessions(&state);
    let sessions = state.join("sessions");
    let left = fs::read_dir(&sessions).map(|rd| rd.flatten().count()).unwrap_or(0);
    let lockp = state.join("lock");
    let holder = fs::read_to_string(&lockp).unwrap_or_default();
    let stale = !holder.trim().is_empty() && holder_stale(&holder);
    if a.force && (holder.trim().is_empty() || stale) {
        let _ = fs::remove_file(&lockp);
    }
    if a.force {
        let _ = fs::remove_dir_all(&sessions);
    }
    println!("🔒 {}", state.display());
    println!("   锁       {}", if holder.trim().is_empty() { "空着（没人占）".to_string() } else if stale { "死锁（进程没了或太久没动静）".to_string() } else { "有人占着".to_string() });
    if !holder.trim().is_empty() {
        println!("   持有者   {}", holder.trim());
    }
    println!("   明文会话 清掉 {swept} 个，还剩 {left} 个{}", if a.force { "（--force 已全部清除）" } else { "" });
    Ok(())
}

fn status(a: &StatusArgs) -> Result<()> {
    let pkg = read_manifest(&a.dir)?;
    let index: Value = serde_json::from_slice(&fs::read(a.dir.join(hur_core::spec::AUTH_INDEX))?)?;
    let state = state_dir(&pkg_id_of(&pkg));
    let swept = sweep_sessions(&state);
    let holder = fs::read_to_string(state.join("lock")).unwrap_or_default();
    let tail = audit_tail(&state, 10);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "package": pkg_id_of(&pkg), "state": state.to_string_lossy(),
                "locked": !holder.trim().is_empty(),
                "lockHolder": if holder.trim().is_empty() { Value::Null } else { serde_json::from_str(holder.trim()).unwrap_or(Value::Null) },
                "sweptSessions": swept, "audit": tail,
                "projects": index["projects"],
            }))?
        );
        return Ok(());
    }
    println!("{}  （本地状态 {}）", pkg_id_of(&pkg), state.display());
    println!("   锁       {}", if holder.trim().is_empty() { "空着".to_string() } else { format!("有人占着：{}", holder.trim()) });
    if swept > 0 {
        println!("   残留     清掉 {swept} 个过期 session");
    }
    println!("   审计     最近 {} 条（只记条目名，不记值）", tail.len());
    for e in &tail {
        println!(
            "     {} {} {}/{} {} {}",
            e["at"].as_u64().map(|t| fmt_epoch(t as i64)).unwrap_or_default(),
            e["act"].as_str().unwrap_or(""),
            e["project"].as_str().unwrap_or(""),
            e["entries"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(",")).unwrap_or_default(),
            e["result"].as_str().unwrap_or(""),
            e["reason"].as_str().unwrap_or(""),
        );
    }
    Ok(())
}
