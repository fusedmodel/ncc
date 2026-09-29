// NCC Auth 客户端：凭据（CR-…）+ 设备码登录 + 授权管理。
//
// 设计见 `ncc-platform/prd/ncc-auth.md`。三条边界在这里也必须守住：
//
//  1. **用途密钥分开**：身份持有密钥住在 `~/.ncc/cred/<purpose>.key`，
//     **绝不复用** `~/.harnessuse/keys/hur.key`（那是"发布者签名"身份 ——
//     混用会让"换密钥 = 已发布制品验签全废"）。
//  2. **注册凭据 ≠ 有权用**：`key new` 只完成"这把我证明是自己的"；
//     能不能取东西仍然看 scope 与 `ncc grant`。
//  3. **不外发硬件指纹**：传给服务端的只有公钥；本地也从不采集机器指纹。
use crate::api;
use crate::config::{self, CliConfig};
use anyhow::{bail, Context, Result};
use ring::rand::SystemRandom;
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/* ---------------- 本地密钥 ---------------- */

fn cred_dir() -> PathBuf {
    config::ncc_dir().join("cred")
}

/// 身份持有密钥的落点。`purpose` 分开存是刻意的：用途不同、生命周期不同、撤销时机不同。
fn key_path(purpose: &str) -> PathBuf {
    cred_dir().join(format!("{purpose}.key"))
}

fn b64url(raw: &[u8]) -> String {
    // 与 Rust 生态一致：无填充的 URL-safe base64。
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in raw.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

fn b64_decode(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| *c != '=' && !c.is_whitespace()).collect();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for c in clean.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '-' | '+' => 62,
            '_' | '/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// 装载（必要时生成）身份持有密钥。
///
/// 生成的是 PKCS#8（ring 的标准产物），落盘 0600 —— 私钥不出本机，
/// 服务端永远只见到公钥。
fn load_or_create_key(purpose: &str) -> Result<(Ed25519KeyPair, Vec<u8>, PathBuf)> {
    let path = key_path(purpose);
    if let Ok(raw) = fs::read_to_string(&path) {
        let der = b64_decode(raw.trim());
        if !der.is_empty() {
            let pubkey = public_key_of_pkcs8(&der)?;
            let kp = Ed25519KeyPair::from_pkcs8(&der)
                .map_err(|e| anyhow::anyhow!("私钥文件 {path:?} 无法解析: {e}"))?;
            return Ok((kp, pubkey, path));
        }
    }
    let rng = SystemRandom::new();
    let der = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| anyhow::anyhow!("生成 Ed25519 密钥失败"))?;
    fs::create_dir_all(cred_dir())?;
    fs::write(&path, b64url(der.as_ref()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    println!("  已生成身份持有密钥 {}", path.display());
    let pubkey = public_key_of_pkcs8(der.as_ref())?;
    let kp = Ed25519KeyPair::from_pkcs8(der.as_ref()).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((kp, pubkey, path))
}

/// 只读已存在的身份密钥（**不生成**）：跑 API 请求时顺手签名不该有副作用。
fn read_key(purpose: &str) -> Option<Ed25519KeyPair> {
    let raw = fs::read_to_string(key_path(purpose)).ok()?;
    let der = b64_decode(raw.trim());
    if der.is_empty() {
        return None;
    }
    Ed25519KeyPair::from_pkcs8(&der).ok()
}

/// 持有证明（`NCC-Proof` 头的值）。
///
/// 规范串必须与服务端逐字节一致（含换行）：
///
/// ```text
/// METHOD + "\n" + PATH + "\n" + base64url(sha256(access_token))
/// ```
///
/// 它证明的是「**请求这一刻持有那把私钥**」：令牌被复制到别的机器上也用不了。
/// 与服务端一致地诚实标注差别：没有 `jti` / nonce / 重放窗口。
pub fn proof_for(method: &str, path: &str, token: &str) -> Option<String> {
    let kp = read_key("identity")?;
    Some(sign_proof(&kp, method, path, token))
}

fn sign_proof(kp: &Ed25519KeyPair, method: &str, path: &str, token: &str) -> String {
    b64url(kp.sign(canonical_proof(method, path, token).as_bytes()).as_ref())
}

/// 规范串单独抽出来，好被单测钉死（客户端与服务端两边必须一致）。
fn canonical_proof(method: &str, path: &str, token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    format!("{}\n{}\n{}", method.to_uppercase(), path, b64url(&h.finalize()))
}

/// 从 PKCS#8 里取出 32 字节公钥。
///
/// ring 只给 `sign()`，不暴露公钥 —— 但登记凭据必须报公钥。
/// Ed25519 的 `OneAsymmetricKey` v2（ring 生成的正是这种）把公钥放在文档末尾，
/// 所以取末尾 32 字节；这个假设**由下面的单测钉死**（签一条再用取出的公钥验一条），
/// 不靠“应该是对的”。
fn public_key_of_pkcs8(der: &[u8]) -> Result<Vec<u8>> {
    if der.len() < 32 {
        bail!("私钥结构不完整（{} 字节）", der.len());
    }
    Ok(der[der.len() - 32..].to_vec())
}

/// 公钥指纹（本地算，与服务端同一套：sha256 前 8 字节 hex）。
fn fingerprint(pubkey: &[u8]) -> String {
    let sum = Sha256::digest(pubkey);
    let mut s = String::from("SHA256:");
    for b in sum.iter().take(8) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/* ---------------- 参数 ---------------- */

#[derive(clap::Args)]
pub struct LoginArgs {
    /// 客户端 id（服务端 NCC_AUTH_CLIENTS 里注册的那个）
    #[arg(long)]
    pub client: String,
    /// 客户端密钥（公开客户端留空）；也可用环境变量 NCC_AUTH_CLIENT_SECRET
    #[arg(long, default_value = "")]
    pub secret: String,
    /// 申请的 scope（空格分隔）；缺省只要 openid
    #[arg(long, default_value = "openid")]
    pub scope: String,
    /// 把本地身份密钥绑进这次令牌（cnf）：令牌只能在这台机器上用。
    /// 需要先 `ncc auth key new` 绑过凭据（凭据 id 见 `ncc auth key ls`）。
    #[arg(long, default_value = "")]
    pub credential: String,
    /// 输出 JSON（脚本用）
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ConsentsArgs {
    /// 连已撤销的一起列出来（审计用）
    #[arg(long)]
    pub revoked: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Subcommand)]
pub enum AuthCmd {
    /// 身份持有密钥（CR-…）：生成 / 列出 / 撤销
    Key {
        #[command(subcommand)]
        action: KeyCmd,
    },
    /// 设备码登录（RFC 8628）：终端显示码 → 浏览器确认 → 换令牌
    Login(LoginArgs),
    /// 我授出去的应用（同意记录）
    Consents(ConsentsArgs),
    /// 撤销某个应用的授权（按平台独立：撤一个不影响另一个）
    Revoke { id: String },
    /// 这台机器上有什么凭据（本地密钥 + 服务端登记）
    Status,
}

#[derive(clap::Subcommand)]
pub enum KeyCmd {
    /// 生成身份持有密钥并登记到服务端，拿回 CR-…
    New {
        #[arg(long, default_value = "identity")]
        purpose: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 服务端登记的凭据（CR-…）
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// 撤销凭据（CR-…）：撤销后它签的持有证明不再被接受
    Rm { id: String },
}

/* ---------------- 实现 ---------------- */

pub fn run(cfg: &CliConfig, action: &AuthCmd) -> Result<()> {
    match action {
        AuthCmd::Key { action } => key(cfg, action),
        AuthCmd::Login(a) => login(cfg, a),
        AuthCmd::Consents(a) => consents(cfg, a),
        AuthCmd::Revoke { id } => revoke(cfg, id),
        AuthCmd::Status => status(cfg),
    }
}

fn key(cfg: &CliConfig, action: &KeyCmd) -> Result<()> {
    let token = require_token(cfg)?;
    match action {
        KeyCmd::New { purpose, label, json } => {
            let (kp, pubkey, path) = load_or_create_key(purpose)?;
            let body = json!({
                "publicKey": b64url(&pubkey),
                "purpose": purpose,
                "label": label.clone().unwrap_or_default(),
                "platform": format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
                "version": env!("CARGO_PKG_VERSION"),
            });
            let _ = kp; // 私钥本身不在这一步用；登记只发公钥
            let v = api::post_json(cfg, "/api/auth/credentials", Some(&token), &body)?;
            let id = v.pointer("/credential/id").and_then(Value::as_str).unwrap_or("");
            if *json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            let existing = v.get("existing").and_then(Value::as_bool).unwrap_or(false);
            println!(
                "{} {}",
                if existing { "已有凭据:" } else { "已登记凭据:" },
                id
            );
            println!("  指纹 {}", fingerprint(&pubkey));
            println!("  私钥 {}（0600，永不出本机）", path.display());
            println!("  注：登记凭据 ≠ 有权取东西 —— 那要看 scope 与 `ncc grant`。");
            Ok(())
        }
        KeyCmd::Ls { json } => {
            let v = fetch_credentials(cfg)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            print!("{}", render_credentials(&v));
            Ok(())
        }
        KeyCmd::Rm { id } => {
            api::request(cfg, "DELETE", &format!("/api/auth/credentials/{id}"), Some(&token), None, None, &[])?;
            println!("已撤销 {id}");
            Ok(())
        }
    }
}

/// 设备码登录（RFC 8628）：与 `gh auth login` 同一条路 ——
/// 终端显示一串码，用户在浏览器里确认，CLI 轮询换令牌。
fn login(cfg: &CliConfig, a: &LoginArgs) -> Result<()> {
    let secret = if a.secret.is_empty() {
        std::env::var("NCC_AUTH_CLIENT_SECRET").unwrap_or_default()
    } else {
        a.secret.clone()
    };
    let mut form: Vec<(&str, String)> = vec![
        ("client_id", a.client.clone()),
        ("client_secret", secret.clone()),
        ("scope", a.scope.clone()),
    ];
    if !a.credential.is_empty() {
        form.push(("credential_id", a.credential.clone()));
    }
    let dev = api::post_form(cfg, "/oauth/device_authorization", None, &form, &[])
        .context("设备码请求失败（这个节点开 NCC Auth 了吗？`ncc hub` 的 /api/meta 里要有 auth 能力）")?;
    let device_code = dev.get("device_code").and_then(Value::as_str).unwrap_or("").to_string();
    let user_code = dev.get("user_code").and_then(Value::as_str).unwrap_or("").to_string();
    let verify = dev.get("verification_uri").and_then(Value::as_str).unwrap_or("").to_string();
    let verify_full = dev.get("verification_uri_complete").and_then(Value::as_str).unwrap_or("").to_string();
    let interval = dev.get("interval").and_then(Value::as_u64).unwrap_or(5).max(1);
    let expires = dev.get("expires_in").and_then(Value::as_u64).unwrap_or(900);
    if device_code.is_empty() {
        bail!("服务端没有返回 device_code：{dev}");
    }
    println!("请在浏览器里打开下面这个地址，并确认设备码：");
    println!("  verification_uri: {verify}");
    println!("  user_code: {user_code}");
    println!("  （直达链接：{verify_full}）");

    let deadline = Instant::now() + Duration::from_secs(expires);
    loop {
        if Instant::now() > deadline {
            bail!("设备码已过期，请重新运行 `ncc auth login`");
        }
        std::thread::sleep(Duration::from_secs(interval));
        let res = api::post_form(
            cfg,
            "/oauth/token",
            None,
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code".into()),
                ("client_id", a.client.clone()),
                ("client_secret", secret.clone()),
                ("device_code", device_code.clone()),
            ],
            &[],
        );
        match res {
            Ok(v) => {
                let token = v.get("access_token").and_then(Value::as_str).unwrap_or("");
                if token.is_empty() {
                    bail!("换令牌失败：{v}");
                }
                if a.json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                    return Ok(());
                }
                let mut cfg2 = cfg.clone();
                config::save_auth_token(&mut cfg2, token)?;
                println!("✅ 已拿到对外访问令牌（客户端 {}，已存到当前目标）", a.client);
                println!("   它只能走数据面（读目录/服务/索引/名片）—— 账号面（key、授权）仍用会话。");
                println!("   要管授权就 `ncc login --email … --password …` 后 `ncc auth consents`。");
                return Ok(());
            }
            Err(e) => {
                let msg = e.to_string();
                // 规范要求的两条"还没好"信号：继续等，不算失败。
                if msg.contains("authorization_pending") {
                    continue;
                }
                if msg.contains("slow_down") {
                    std::thread::sleep(Duration::from_secs(interval));
                    continue;
                }
                return Err(e);
            }
        }
    }
}

fn consents(cfg: &CliConfig, a: &ConsentsArgs) -> Result<()> {
    let v = fetch_consents(cfg, a.revoked)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    print!("{}", render_consents(&v));
    Ok(())
}

/* ---------------- 取数据 / 渲染（MCP 与 CLI 共用同一对函数） ---------------- */

/// 服务端登记的凭据（需要会话或带 keys:write 的 key）。
pub fn fetch_credentials(cfg: &CliConfig) -> Result<Value> {
    let token = require_token(cfg)?;
    api::get(cfg, "/api/auth/credentials", Some(&token))
}

pub fn render_credentials(v: &Value) -> String {
    let rows = v
        .get("credentials")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        return "服务端还没有登记任何凭据（`ncc auth key new` 生成一把）\n".to_string();
    }
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!(
            "{}  {:<9} {}  {} {}\n",
            r.get("id").and_then(Value::as_str).unwrap_or(""),
            r.get("purpose").and_then(Value::as_str).unwrap_or(""),
            r.get("fingerprint").and_then(Value::as_str).unwrap_or(""),
            r.get("label").and_then(Value::as_str).unwrap_or(""),
            r.get("platform").and_then(Value::as_str).unwrap_or("")
        ));
    }
    out
}

/// 我授出去的应用（同意记录）。
pub fn fetch_consents(cfg: &CliConfig, include_revoked: bool) -> Result<Value> {
    let token = require_token(cfg)?;
    let path = if include_revoked {
        "/api/auth/consents?revoked=1"
    } else {
        "/api/auth/consents"
    };
    api::get(cfg, path, Some(&token))
}

pub fn render_consents(v: &Value) -> String {
    let rows = v
        .get("consents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        return "还没有授出任何应用（`ncc auth login` 会让某个应用出现在这里）\n".to_string();
    }
    let mut out = String::new();
    for r in rows {
        let scopes = r
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let cred = r.get("credentialId").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!(
            "{}  {} ({}){}\n    scope: {}{}\n",
            r.get("id").and_then(Value::as_str).unwrap_or(""),
            r.get("clientName").and_then(Value::as_str).unwrap_or(""),
            r.get("clientId").and_then(Value::as_str).unwrap_or(""),
            if r.get("revoked").and_then(Value::as_bool).unwrap_or(false) { "  [已撤销]" } else { "" },
            scopes,
            if cred.is_empty() {
                String::new()
            } else {
                format!("\n    绑定凭据: {cred}")
            }
        ));
    }
    out.push_str("\n撤销：ncc auth revoke <id>（按平台独立，撤一个不影响别的）\n");
    out
}

fn revoke(cfg: &CliConfig, id: &str) -> Result<()> {
    let token = require_token(cfg)?;
    api::request(cfg, "DELETE", &format!("/api/auth/consents/{id}"), Some(&token), None, None, &[])?;
    println!("已撤销授权 {id}（该平台的令牌与刷新凭据立刻失效）");
    Ok(())
}

fn status(cfg: &CliConfig) -> Result<()> {
    let dir = cred_dir();
    let mut local: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().ends_with(".key") {
                local.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    println!("本地密钥目录 {}", dir.display());
    if local.is_empty() {
        println!("  （空 —— `ncc auth key new` 生成一把）");
    } else {
        for f in local.iter() {
            println!("  {f}");
        }
    }
    println!("注意：发布者签名密钥在 ~/.harnessuse/keys（**故意分开** —— 换密钥不该互相牵连）");
    // 服务端那一侧要联网才看得到；没登录就如实说。
    match config::token_opt(cfg) {
        Some(_) => {
            let v = fetch_credentials(cfg)?;
            let n = v.get("count").and_then(Value::as_u64).unwrap_or(0);
            println!("服务端已登记凭据 {n} 把（ncc auth key ls 看详情）");
        }
        None => println!("（未登录，跳过服务端侧检查）"),
    }
    match config::auth_token_opt(cfg) {
        Some(t) => println!("当前目标存有一张对外访问令牌（{}…）—— 它只走数据面", &t[..t.len().min(12)]),
        None => println!("当前目标还没有对外访问令牌（ncc auth login）"),
    }
    Ok(())
}

fn require_token(cfg: &CliConfig) -> Result<String> {
    config::token_opt(cfg).ok_or_else(|| anyhow::anyhow!("需要先登录：ncc login --email … --password …（或 NCC_TOKEN）"))
}

/* ---------------- 单测：把"公钥怎么来"这件事钉死 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{UnparsedPublicKey, ED25519};

    /// 从 PKCS#8 末尾取的 32 字节**真的**是公钥：
    /// 用私钥签一条消息，再用取出来的公钥验签通过 —— 猜错结构这条必红。
    #[test]
    fn public_key_extraction_matches_signing_key() {
        let rng = SystemRandom::new();
        let der = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(der.as_ref()).unwrap();
        let pubkey = public_key_of_pkcs8(der.as_ref()).unwrap();
        assert_eq!(pubkey.len(), 32, "Ed25519 公钥必须是 32 字节");

        let msg = b"ncc-auth-proof";
        let sig = kp.sign(msg);
        UnparsedPublicKey::new(&ED25519, &pubkey)
            .verify(msg, sig.as_ref())
            .expect("取出的公钥必须能验这把私钥的签名");
    }

    /// 指纹形态与进制（服务端也是这套：sha256 前 8 字节 hex）。
    #[test]
    fn fingerprint_shape() {
        let fp = fingerprint(&[7u8; 32]);
        assert!(fp.starts_with("SHA256:"), "{fp}");
        assert_eq!(fp.len(), "SHA256:".len() + 16, "{fp}");
    }

    /// base64url 编解码自洽（无填充）。
    #[test]
    fn b64_roundtrip() {
        for case in [vec![], vec![1u8], vec![1u8, 2], vec![1u8, 2, 3], vec![255u8; 40]] {
            let enc = b64url(&case);
            assert!(!enc.contains('='), "不该带填充：{enc}");
            assert_eq!(b64_decode(&enc), case, "roundtrip 失败：{enc}");
        }
    }

    /// 私钥落点**必须**与发布者签名密钥分开（混用会让"换密钥 = 已发布制品验签全废"）。
    #[test]
    fn key_path_is_separate_from_signing_key() {
        let p = key_path("identity").to_string_lossy().to_string();
        assert!(p.contains("/.ncc/cred/"), "{p}");
        assert!(!p.contains("harnessuse"), "不许复用发布者签名密钥：{p}");
    }

    /// 规范串必须与服务端逐字节一致：`METHOD\nPATH\nb64url(sha256(token))`
    /// （服务端 `authserver.VerifyProof`）。拼错一个字节就是「总是 401」。
    #[test]
    fn proof_canonical_string_matches_server_format() {
        let c = canonical_proof("get", "/api/registry", "ncat_abc");
        let parts: Vec<&str> = c.split('\n').collect();
        assert_eq!(parts.len(), 3, "{c}");
        assert_eq!(parts[0], "GET", "方法要大写");
        assert_eq!(parts[1], "/api/registry");
        let d = Sha256::digest(b"ncat_abc");
        assert_eq!(parts[2], b64url(d.as_slice()));
    }

    /// 签出来的持有证明**能被取出的公钥验过** —— 与服务端拿库里的公钥校验是同一条路。
    #[test]
    fn proof_verifies_with_extracted_public_key() {
        let rng = SystemRandom::new();
        let der = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(der.as_ref()).unwrap();
        let pubkey = public_key_of_pkcs8(der.as_ref()).unwrap();
        let proof = sign_proof(&kp, "GET", "/api/registry", "ncat_x");
        let sig = b64_decode(&proof);
        assert_eq!(sig.len(), 64, "Ed25519 签名是 64 字节");
        UnparsedPublicKey::new(&ED25519, &pubkey)
            .verify(canonical_proof("GET", "/api/registry", "ncat_x").as_bytes(), &sig)
            .expect("持有证明必须能被公钥验证");
        // 换路径就应验不过（证明绑定到具体方法 + 路径 + 令牌）
        assert!(UnparsedPublicKey::new(&ED25519, &pubkey)
            .verify(canonical_proof("GET", "/api/other", "ncat_x").as_bytes(), &sig)
            .is_err());
    }
}
