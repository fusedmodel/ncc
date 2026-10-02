//! NCC Gateway（S2a）：**固定路由的白名单代理**。
//!
//! 权威设计：`ncc-platform/prd/ncc-gateway-prd.md` §16。本模块只实现 §16.6 的 **S2**
//! ——「B 侧固定代理」，不含 P2P 打洞、远程执行服务与计量上报（S3/S4）。
//!
//! ## 它在做什么
//!
//! 一个**人配置过的转发 + 强制执行点**。调用方**不能指定目标地址**，只能给
//! 「路由名 + 该路由内的一条路径」；路由到哪个上游、带什么凭据，全部写在 B 的配置里。
//!
//! ```text
//! A 网里的 Agent
//!   → A 侧网关（mode=forward）  本地白名单 + 审计
//!   → 直连 peer（配对的 B）
//!   → B 侧网关（mode=accept）   鉴权 + 路径/方法白名单 + 配额 + 注入本机凭据 + 审计
//!   → 写死的上游（如 LLM API）
//! ```
//!
//! ## 红线（写代码时别破）
//!
//! 1. **数据面不经 NCC**：本模块只做 A↔B 直连，不向任何 NCC 服务端上报载荷。
//! 2. **默认拒绝**：路径 / 方法 / 配额 / 令牌任一不过 → **本地拒绝并记审计**，请求不发出去。
//! 3. **审计只记元数据**：不落请求体、不落响应体、不落注入的凭据。
//! 4. **`accept` 路由必须有令牌**：没有令牌的出口等于把出口开放给所有人，启动时直接拒。
//! 5. **不跟随重定向**：跟随就等于绕过了白名单（3xx 可以把请求带去别的域）。
//!
//! ## 依赖立场
//!
//! 不引新依赖：HTTP 服务端用 `std::net`（约 200 行，只支持定长 body 的 HTTP/1.1），
//! 出站用已有的 `ureq`，审计/配置用 `serde_json`，请求 id 用已有的 `sha2`。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use crate::httpsrv::{read_request, write_resp, ReadError, Req, Resp};
use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config;

/// 单条请求的 body 上限（默认 1 MiB）。超了直接 413，不转发。
const DEFAULT_MAX_BODY: u64 = 1024 * 1024;
/// 请求头总量上限。超过就当恶意/异常处理。
/// 配额窗口：每分钟。
const QUOTA_WINDOW: Duration = Duration::from_secs(60);

/* ============================ 配置 ============================ */

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// 监听地址。默认 127.0.0.1:9810。
    #[serde(default)]
    pub listen: String,
    /// 审计目录。默认在 ncc 目录下的 gateway-audit/。
    #[serde(default)]
    pub audit_dir: String,
    /// 显式允许监听非 loopback 地址（**没有 TLS 就别对外**）。
    #[serde(default)]
    pub allow_plaintext: bool,
    /// 请求体上限（字节）。
    #[serde(default)]
    pub max_body_bytes: u64,
    #[serde(default)]
    pub routes: Vec<Route>,

    // ---- 控制面接入（F2/F3/F5；`ncc gateway bind` 自动写，也可手工填）----
    /// 控制面地址（ncc.ai 或某个内网节点）。空 = 未绑定（审计只落本地）。
    #[serde(default)]
    pub api_server: String,
    /// 注册时下发的网关 id（`GW-…`）。
    #[serde(default)]
    pub gateway_id: String,
    /// 网关令牌（`ncc_gw_…`）：心跳与上报的凭据，**也是摘要签名的密钥材料**
    /// （签名密钥 = sha256(令牌)）—— 它等价于这个网关的口令，别外传。
    #[serde(default)]
    pub gateway_token: String,
    /// 绑定的命名空间 slug（人看的）。
    #[serde(default)]
    pub namespace: String,
    /// 网关名（控制面列表里显示）。
    #[serde(default)]
    pub name: String,
    /// 心跳间隔（秒，0 = 不心跳）。默认 30s（PRD F3）。
    #[serde(default)]
    pub heartbeat_sec: u64,
    /// 摘要上报间隔（秒，0 = 不自动上报，只用 `ncc gateway report` 手动）。
    #[serde(default)]
    pub report_sec: u64,
}

/// 一条路由。`mode` 决定它是「提供出口」还是「借用出口」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Route {
    /// 路由名，出现在 URL 里：`POST /v1/<name>/<路径>`。只允许 [a-z0-9._-]。
    pub name: String,
    /// `accept` = 我这台机器提供出口（B 侧）；`forward` = 把请求转给配对的 peer（A 侧）。
    #[serde(default)]
    pub mode: String,

    // ---- accept（B 侧）----
    /// 上游基址，**写死在配置里**，调用方无法更改。允许 `https://…`；`http://` 仅限 loopback（本地联调）。
    #[serde(default)]
    pub target: String,
    /// 允许转发的上游路径（**精确匹配**，不带查询串）。空 = 拒绝一切（fail-closed）。
    #[serde(default)]
    pub paths: Vec<String>,
    /// 允许的方法（大写）。空 = 拒绝（fail-closed）。
    #[serde(default)]
    pub methods: Vec<String>,
    /// 转发时**注入**到上游的请求头（如 B 自己的 LLM key）。调用方的同名头一律被覆盖。
    #[serde(default)]
    pub inject: BTreeMap<String, String>,
    /// 每分钟配额（0 = 不限）。
    #[serde(default)]
    pub quota_per_min: u32,

    // ---- forward（A 侧）----
    /// 配对的 peer 地址（B 的网关根地址，如 `http://127.0.0.1:9811`）。
    #[serde(default)]
    pub peer_url: String,

    // ---- 两侧共用 ----
    /// `accept`：来客必须带的令牌（**必填**）。
    /// `forward`：本机调用方必须带的令牌（空 = 仅当监听在 loopback 时允许）。
    #[serde(default)]
    pub token: String,
    /// `forward` 用来访问 peer 的令牌（= B 那条 accept 路由的 token）。
    #[serde(default)]
    pub peer_token: String,

    // ---- accept 专用：出口的“出处” ----
    /// 背书本路由的 HUR 包：写包目录，或直接写 hur.json。
    ///
    /// 「提供出口」必须是一份**可签名、可分发的包声明**（`hur.json` 的 `egress{}`），
    /// 不能只写在这份本地 JSON 里 —— 否则没人能核对它、没人能给签名、
    /// 也没法把“这个出口能干什么”发给另一个人。
    #[serde(default)]
    pub hur: String,

    // ---- 多供应商（可选）：同一套 paths/methods 下的多个上游，可切换 ----
    /// `{ "openai": { "target": …, "inject": {…}, "rewrite": {…} }, … }`
    /// 非空时不再用扁平的 `target`/`inject`（两份真相）。
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderCfg>,
    /// 当前生效的供应商（`ncc gateway switch` 改它；空 = 取 providers 里的第一个）。
    #[serde(default)]
    pub active: String,
}

/// 一个供应商的本地配置：上游地址 + 要注入的凭据（**值在这里，不在包里**）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderCfg {
    #[serde(default)]
    pub target: String,
    /// 注入到上游的请求头（如这家自己的 key）
    #[serde(default)]
    pub inject: BTreeMap<String, String>,
    /// 路径重写：路由内路径 → 这一家的实际路径（只映射路径，不碰 body）
    #[serde(default)]
    pub rewrite: BTreeMap<String, String>,
}

impl Route {
    fn is_accept(&self) -> bool {
        self.mode == "accept"
    }
}

/// 启动时校验后的配置（把「配置错误」挡在启动前，而不是运行中）。
#[derive(Debug)]
pub struct Ready {
    pub cfg: GatewayConfig,
    pub listen: String,
    pub audit_dir: PathBuf,
    pub max_body: u64,
    /// 每条 accept 路由背后那份包声明（`check` 会打出来，便于人工核对）
    pub backing: Vec<Backing>,
}

fn ncc_dir() -> PathBuf {
    config::ncc_dir()
}

pub fn config_path() -> PathBuf {
    ncc_dir().join("gateway.json")
}

fn load_config() -> Result<GatewayConfig> {
    let p = config_path();
    let raw = std::fs::read_to_string(&p)
        .with_context(|| format!("读不到配置 {}（先跑 ncc gateway init）", p.display()))?;
    let cfg: GatewayConfig =
        serde_json::from_str(&raw).with_context(|| format!("配置不是合法 JSON：{}", p.display()))?;
    Ok(cfg)
}

/// 给 `ncc gateway bind/heartbeat/report` 用（它们要读写同一份配置）。
pub fn load_config_pub() -> Result<GatewayConfig> { load_config() }

/// 写回配置（只动绑定字段；路由那块由人维护）。
///
/// **权限 0600**：里面有网关令牌，等同于口令。
pub fn save_config_pub(cfg: &GatewayConfig) -> Result<()> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(&p, serde_json::to_string_pretty(cfg)?)
        .with_context(|| format!("写不了配置 {}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 审计目录（`report` 聚合本地 JSONL 用同一个判定）。
pub fn audit_dir_of(cfg: &GatewayConfig) -> PathBuf {
    if cfg.audit_dir.trim().is_empty() {
        ncc_dir().join("gateway-audit")
    } else {
        PathBuf::from(cfg.audit_dir.trim())
    }
}

fn is_loopback_addr(addr: &str) -> bool {
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "localhost" {
        return true;
    }
    host.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// 启动前校验：把「能启动但不安全」的配置全部拒掉。
///
/// 这里每条都是 fail-closed 的：宁可起不来，也不要起一个能被别人借走的出口。
pub fn prepare(cfg: GatewayConfig) -> Result<Ready> {
    let listen = if cfg.listen.trim().is_empty() {
        "127.0.0.1:9810".to_string()
    } else {
        cfg.listen.trim().to_string()
    };
    // 没有 TLS 就别监听公网 —— 要么前面放反代终止 TLS，要么显式承担风险。
    if !is_loopback_addr(&listen) && !cfg.allow_plaintext {
        bail!(
            "listen = {listen} 不是回环地址，而本实现不做 TLS。\n\
             要么只监听 127.0.0.1（前面放反代/P2P 隧道终止 TLS），\n\
             要么在配置里显式写 \"allow_plaintext\": true 承担风险。"
        );
    }

    let audit_dir = if cfg.audit_dir.trim().is_empty() {
        ncc_dir().join("gateway-audit")
    } else {
        PathBuf::from(cfg.audit_dir.trim())
    };
    std::fs::create_dir_all(&audit_dir)
        .with_context(|| format!("建不了审计目录 {}", audit_dir.display()))?;

    if cfg.routes.is_empty() {
        bail!("配置里没有任何 routes —— 没有路由的网关没有意义，也不会启动。");
    }
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut backing: Vec<Backing> = Vec::new();
    for r in &cfg.routes {
        if !valid_route_name(&r.name) {
            bail!("路由名 {:?} 不合法（只允许 [a-z0-9._-]）", r.name);
        }
        let key = (r.mode.clone(), r.name.clone());
        if seen.contains(&key) {
            bail!("路由 {} (mode={}) 重复", r.name, r.mode);
        }
        seen.push(key);

        for (k, v) in &r.inject {
            if !valid_header_name(k) || v.contains(['\r', '\n']) {
                bail!("路由 {} 的 inject 头 {k:?} 不合法", r.name);
            }
        }

        match r.mode.as_str() {
            "accept" => {
                if r.token.trim().len() < 16 {
                    bail!(
                        "accept 路由 {} 必须配一个 ≥16 字符的 token —— 没有令牌的出口等于把出口开放给所有人",
                        r.name
                    );
                }
                // 单供应商写法：地址写在扁平 target 里（多供应商时 target 必须留空，见下）
                if r.providers.is_empty() {
                    let t = parse_target(&r.target).with_context(|| format!("路由 {} 的 target", r.name))?;
                    if t.scheme == "http" && !is_loopback_host(&t.host) {
                        bail!(
                            "路由 {} 的 target 是 http 且主机不是 loopback —— 出站必须 https（本地联调除外）",
                            r.name
                        );
                    }
                }
                if r.paths.is_empty() {
                    bail!("accept 路由 {} 没配 paths —— 空白名单会放行该上游下的一切路径", r.name);
                }
                for p in &r.paths {
                    if normalize_sub_path(p).is_none() {
                        bail!("accept 路由 {} 的 path {p:?} 不合法", r.name);
                    }
                }
                if r.methods.is_empty() {
                    bail!("accept 路由 {} 没配 methods（空 = 拒绝一切，等于不可用）", r.name);
                }
                // 多供应商：本地配置只能从**包声明的范围里**挑，而且 ``active`` 必须真在本地表里
                if !r.providers.is_empty() {
                    if !r.target.trim().is_empty() {
                        bail!(
                            "accept 路由 {} 同时写了 target 与 providers —— 两份真相：\n多供应商时把地址写进各 provider，target 留空；单供应商就别写 providers。",
                            r.name
                        );
                    }
                    for (name, p) in &r.providers {
                        if !valid_route_name(name) {
                            bail!("路由 {} 的供应商名 {name:?} 不合法（只允许 [a-z0-9._-]）", r.name);
                        }
                        if p.target.trim().is_empty() {
                            bail!("路由 {} 的供应商 {} 没写 target", r.name, name);
                        }
                        let t = parse_target(&p.target)
                            .with_context(|| format!("路由 {} 供应商 {} 的 target", r.name, name))?;
                        if t.scheme == "http" && !is_loopback_host(&t.host) {
                            bail!("路由 {} 供应商 {} 的 target 是 http 且非 loopback —— 出站必须 https", r.name, name);
                        }
                        for (k, v) in &p.inject {
                            if !valid_header_name(k) || v.contains(['\r', '\n']) {
                                bail!("路由 {} 供应商 {} 的 inject 头 {k:?} 不合法", r.name, name);
                            }
                        }
                        for (from, to) in &p.rewrite {
                            if normalize_sub_path(from).is_none() || normalize_sub_path(to).is_none() {
                                bail!("路由 {} 供应商 {} 的 rewrite {from:?} -> {to:?} 不合法", r.name, name);
                            }
                        }
                    }
                    let eff = effective_provider_name(r);
                    if !r.providers.contains_key(&eff) {
                        let names: Vec<&str> = r.providers.keys().map(|s| s.as_str()).collect();
                        bail!(
                            "路由 {} 的 active「{}」不在 providers 里（可选：{}）",
                            r.name,
                            eff,
                            names.join(", ")
                        );
                    }
                }
                // S2b：出口的定义来自包，不来自这份本地 JSON。
                if r.hur.trim().is_empty() {
                    bail!(
                        "accept 路由 {} 没绑 HUR 包（缺 \"hur\"）——「提供出口」必须由一份可签名、可分发的包声明背书。\n\n在路由里加：\"hur\": \"/包目录或/hur.json\"，并在那个包的 hur.json 里声明：\n  \"egress\": {{ \"provides\": [ {{ \"name\": \"{}\", \"target\": \"{}\", \"paths\": [...], \"methods\": [...] }} ] }}",
                        r.name,
                        r.name,
                        r.target
                    );
                }
                backing.push(bind_hur(r)?);
            }
            "forward" => {
                if r.peer_url.trim().is_empty() {
                    bail!("forward 路由 {} 没配 peer_url", r.name);
                }
                let t = parse_target(&r.peer_url).with_context(|| format!("路由 {} 的 peer_url", r.name))?;
                if t.scheme == "http" && !is_loopback_host(&t.host) {
                    // 跨网那一跳的机密性由部署负责（P2P / WireGuard / 反代）。这里只警告。
                    eprintln!(
                        "⚠ 路由 {} 的 peer_url 是明文 http 且不在本机：A↔B 那一跳的机密性要靠隧道（P2P/WireGuard/反代）保证。",
                        r.name
                    );
                }
                if r.token.trim().is_empty() && !is_loopback_addr(&listen) {
                    bail!(
                        "forward 路由 {} 没有 token，而 listen 不是 loopback —— 本机调用方必须有令牌",
                        r.name
                    );
                }
                if r.peer_token.trim().is_empty() {
                    bail!("forward 路由 {} 没配 peer_token（拿什么去访问 peer？）", r.name);
                }
                for p in &r.paths {
                    if normalize_sub_path(p).is_none() {
                        bail!("forward 路由 {} 的 path {p:?} 不合法", r.name);
                    }
                }
            }
            other => bail!("路由 {} 的 mode {other:?} 不认识（只能是 accept / forward）", r.name),
        }
    }

    Ok(Ready {
        max_body: if cfg.max_body_bytes == 0 { DEFAULT_MAX_BODY } else { cfg.max_body_bytes },
        cfg,
        listen,
        audit_dir,
        backing,
    })
}

/// 路由名：**同一份规则**（hur-core 的 `valid_egress_name`）—— 路由名就是 URL 里的那段，
/// 而 egress 声明里的 `name` 也是同一段，两边不能有两套合法性。
fn valid_route_name(s: &str) -> bool {
    hur_core::spec::valid_egress_name(s)
}

fn valid_header_name(s: &str) -> bool {
    hur_core::spec::valid_header_name(s)
}

/// 主机是不是本机：同一份规则（hur-core 的 `is_loopback_host`）。
fn is_loopback_host(h: &str) -> bool {
    hur_core::spec::is_loopback_host(h)
}

/* ============================ 小工具 ============================ */

/// Unix 秒 → `YYYY-MM-DDTHH:MM:SSZ`（UTC）。
///
/// 自己算而不是引 chrono：只需要一个方向，且要好测。
pub(crate) fn iso_from_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 days→civil（1970-01-01 起的天数 → 年月日）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

pub(crate) fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_from_secs(secs)
}

fn to_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(b);
    d.iter().map(|x| format!("{x:02x}")).collect()
}

/// 请求 id：不引 rand，用「时间 + pid + 计数」哈希成 16 位十六进制。
/// 它只是**关联日志**用的，不需要不可预测。
fn req_id(n: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}-{}-{}", std::process::id(), nanos, n);
    to_hex(seed.as_bytes())[..16].to_string()
}

/// 常量时间的字符串比较（避免用 == 比较令牌时的时序侧信道）。
/// 长度不同直接 false（长度本身不是秘密），内容逐字节异或累加。
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/* ============================ 上游地址与路径 ============================ */

/// 上游地址解析**就用 hur-core 的那一份**（`HttpTarget`）。
///
/// 为什么不让两边各写一份：egress 声明与网关必须对「什么算合法 target」有一致理解，
/// 否则「配置只能比声明更窄」根本无从判定。
use hur_core::spec::{parse_http_target, HttpTarget as Target, Level};

fn parse_target(raw: &str) -> Result<Target> {
    parse_http_target(raw)
        .with_context(|| format!("地址不合法（要 http(s)://主机[:端口][/基路径]，不能带用户信息）：{}", raw.trim()))
}

/// 归一化「路由内子路径」：**同一份规则**（hur-core 的 `normalize_egress_path`）。
fn normalize_sub_path(p: &str) -> Option<String> {
    hur_core::spec::normalize_egress_path(p)
}

/// S2b：一条 accept 路由背后那份包声明。
#[derive(Debug)]
pub struct Backing {
    pub route: String,
    /// 包目录（人看的）
    pub dir: String,
    pub id: String,
    pub version: String,
    /// 从包声明里读到的范围（不是本地 JSON 里写的）
    pub target: String,
    pub paths: Vec<String>,
    pub methods: Vec<String>,
    pub inject: Vec<String>,
    /// 包里声明了哪几家（多供应商写法；单供应商时为空）
    pub providers: Vec<String>,
    /// 本地当前选的那家
    pub active: String,
}

/// 把一条 accept 路由绑到包声明上：**包声明是上限，本地配置只能更窄**。
///
/// 这是 S2b 的全部要点：如果「提供出口」只写在这份本地 JSON 里，那么
/// 没人能核对它、没人能给签名、也没法把它发给另一个人。绑到 `hur.json` 的 `egress{}`
/// 之后，出口的定义变成一份**可签名、可分发的包声明**，网关只是它的执行器。
fn bind_hur(r: &Route) -> Result<Backing> {
    let raw = r.hur.trim();
    let p = PathBuf::from(raw);
    // 写包目录，或者直接写 hur.json 文件，都接受。
    let (dir, text) = if p.is_file() {
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("路由 {} 的 hur 文件读不到：{}", r.name, p.display()))?;
        (p.parent().map(|x| x.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")), text)
    } else {
        let f = p.join(hur_core::spec::MANIFEST);
        let text = std::fs::read_to_string(&f).with_context(|| {
            format!(
                "路由 {} 绑的包读不到：{} 既不是文件，里面也没有 {}\n（写包目录，或直接指到 hur.json）",
                r.name,
                p.display(),
                hur_core::spec::MANIFEST
            )
        })?;
        (p, text)
    };
    let pkg = hur_core::spec::parse(&text)
        .with_context(|| format!("路由 {} 绑的包不是合法清单：{}", r.name, dir.display()))?;

    // 声明自己得站得住（R10）——“出口的定义”首先得是一份**有边界**的声明。
    let issues = hur_core::spec::validate_egress(&pkg);
    let errs: Vec<String> = issues
        .iter()
        .filter(|i| i.level == Level::Error)
        .map(|i| format!("[{}] {}", i.rule, i.msg))
        .collect();
    if !errs.is_empty() {
        bail!(
            "路由 {} 绑的包 {} 的出口声明不合法：\n  - {}",
            r.name,
            dir.display(),
            errs.join("\n  - ")
        );
    }
    for w in issues.iter().filter(|i| i.level == Level::Warn) {
        eprintln!("⚠ 路由 {}：{} —— {}", r.name, w.rule, w.msg);
    }

    let names: Vec<String> = pkg
        .egress
        .as_ref()
        .map(|e| e.provides.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
    let decl = pkg
        .egress
        .as_ref()
        .and_then(|e| e.provides.iter().find(|p| p.name == r.name))
        .ok_or_else(|| {
            anyhow!(
                "路由 {} 在包 {} 里找不到同名出口声明：需要 hur.json 的 egress.provides 里有一条 name = {:?}。\n这个包里现有：{}",
                r.name,
                dir.display(),
                r.name,
                if names.is_empty() { "（无）".to_string() } else { names.join(", ") }
            )
        })?;

    // 方向只有一个：配置 ≤ 声明。任何「配置比声明更宽」之处逐条列出。
    //
    // 多供应商时**逐家**判定：网关切到 b，就用 providers[b] 去背书 —— 不能拿 a 的声明
    // 去背书一个 a 从来没用过的目标地址。
    let mut problems: Vec<String> = Vec::new();
    if r.providers.is_empty() {
        let inject_names: Vec<String> = r.inject.keys().cloned().collect();
        problems = hur_core::spec::egress_covers(decl, &r.target, &r.paths, &r.methods, &inject_names);
    } else {
        for (name, p) in &r.providers {
            let mut inject_names: Vec<String> = r.inject.keys().cloned().collect();
            inject_names.extend(p.inject.keys().cloned());
            problems.extend(hur_core::spec::egress_covers_provider(
                decl,
                name,
                &p.target,
                &r.paths,
                &r.methods,
                &inject_names,
            ));
        }
    }
    if !problems.is_empty() {
        bail!(
            "路由 {} 的配置超出了包声明的范围（配置只能比声明更窄）：\n  - {}\n包：{}",
            r.name,
            problems.join("\n  - "),
            dir.display()
        );
    }

    Ok(Backing {
        route: r.name.clone(),
        dir: dir.display().to_string(),
        id: pkg.id.clone(),
        version: pkg.version.clone(),
        target: decl.target.trim().to_string(),
        paths: decl.paths.clone(),
        methods: decl.methods.clone(),
        inject: decl.inject.clone(),
        providers: decl.providers.iter().map(|p| p.name.clone()).collect(),
        active: if r.providers.is_empty() { String::new() } else { effective_provider_name(r) },
    })
}

/// 当前生效的供应商名：写了 `active` 用它；没写就取 providers 里**字典序第一个**
/// （BTreeMap 保证确定性：同一份配置两次启动选出同一家）。
pub fn effective_provider_name(route: &Route) -> String {
    let a = route.active.trim();
    if !a.is_empty() {
        return a.to_string();
    }
    route.providers.keys().next().cloned().unwrap_or_default()
}

/// 路径白名单：**精确匹配**（不做前缀魔法 —— 少一条隐含规则就少一个口子）。
/// 两边都先归一化，所以 `chat/completions` 与 `/chat/completions` 等价；
/// 待查路径归一化失败（含 `..` / `%` 之类）一律视为不命中。
fn path_allowed(list: &[String], sub: &str) -> bool {
    let Some(sub) = normalize_sub_path(sub) else {
        return false;
    };
    list.iter().any(|p| normalize_sub_path(p).as_deref() == Some(sub.as_str()))
}

fn method_allowed(list: &[String], m: &str) -> bool {
    list.iter().any(|x| x.eq_ignore_ascii_case(m))
}

/* ============================ 审计 ============================ */

/// 审计落盘：**只记元数据**。请求体、响应体、注入的凭据一律不落。
pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    fn new(dir: PathBuf) -> Self {
        // 一天一个文件：方便按天保留/清理（清理留给 logrotate 或运维脚本）。
        let path = dir.join(format!("gateway-{}.jsonl", &now_iso()[..10]));
        Audit { path, lock: Mutex::new(()) }
    }

    fn file(&self) -> &PathBuf {
        &self.path
    }

    /// 追加一行。审计失败**不能**改变请求结果：只打到 stderr 提示。
    fn write(&self, rec: &serde_json::Value) {
        let _g = self.lock.lock();
        let line = format!("{rec}\n");
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(line.as_bytes()) {
                    eprintln!("⚠ 审计写入失败（{}）：{e}", self.path.display());
                }
            }
            Err(e) => eprintln!("⚠ 打不开审计文件（{}）：{e}", self.path.display()),
        }
    }
}

/* ============================ 转发 ============================ */

struct Quota {
    hits: Mutex<Vec<Instant>>,
}

impl Quota {
    fn new() -> Self {
        Quota { hits: Mutex::new(Vec::new()) }
    }
    /// 记一次并判断是否超额。返回 (是否放行, 窗口内已用)。
    fn take(&self, limit: u32) -> (bool, usize) {
        if limit == 0 {
            return (true, 0);
        }
        let mut h = self.hits.lock().unwrap();
        let now = Instant::now();
        h.retain(|t| now.duration_since(*t) < QUOTA_WINDOW);
        if h.len() >= limit as usize {
            return (false, h.len());
        }
        h.push(now);
        (true, h.len())
    }
}

struct Shared {
    ready: Ready,
    quota: Mutex<BTreeMap<String, Arc<Quota>>>,
    audit: Audit,
    agent: ureq::Agent,
    counter: Mutex<u64>,
    /// 路由 → 当前生效的供应商（`ncc gateway switch` 改配置文件后由 watcher 热应用）。
    /// 缺条目 = 单供应商写法（用 route 的扁平字段）。
    active: RwLock<BTreeMap<String, ActiveProvider>>,
}

/// 运行时生效的上游：名字 + 地址 + 凭据 + 路径映射。
///
/// 每个请求都从 RwLock 里取一份**克隆**（而不是持锁转发）—— 转发要 100ms 级，
/// 拿读锁跨转发会把切档卡住。
#[derive(Debug, Clone, Default)]
struct ActiveProvider {
    name: String,
    target: String,
    inject: BTreeMap<String, String>,
    rewrite: BTreeMap<String, String>,
}

impl Shared {
    /// 当前该走哪个上游：
    /// · 配了 providers → 取 active 对应的那份（watcher 会随文件更新）；
    /// · 没配 → 单供应商写法（扁平 target/inject）。
    fn active_of(&self, route: &Route) -> ActiveProvider {
        if route.providers.is_empty() {
            return ActiveProvider {
                name: String::new(),
                target: route.target.clone(),
                inject: route.inject.clone(),
                rewrite: BTreeMap::new(),
            };
        }
        let m = self.active.read().unwrap();
        if let Some(a) = m.get(&route.name) {
            return a.clone();
        }
        drop(m);
        // 理论上不会发生（prepare 会把初始值填好）；真发生就按 active 现算一次，
        // 宁可算一次也不默默走错家。
        let name = effective_provider_name(route);
        let p = route.providers.get(&name).cloned().unwrap_or_default();
        ActiveProvider { name, target: p.target, inject: p.inject, rewrite: p.rewrite }
    }

    /// 把 active 表整体换成新的一份（`prepare` 之后调用）。
    fn set_active(&self, routes: &[Route]) {
        let mut m = self.active.write().unwrap();
        m.clear();
        for r in routes {
            if r.providers.is_empty() {
                continue;
            }
            let name = effective_provider_name(r);
            let p = r.providers.get(&name).cloned().unwrap_or_default();
            m.insert(r.name.clone(), ActiveProvider { name, target: p.target, inject: p.inject, rewrite: p.rewrite });
        }
    }
    fn quota_of(&self, name: &str) -> Arc<Quota> {
        let mut m = self.quota.lock().unwrap();
        m.entry(name.to_string()).or_insert_with(|| Arc::new(Quota::new())).clone()
    }

    fn next_id(&self) -> (u64, String) {
        let mut c = self.counter.lock().unwrap();
        *c += 1;
        (*c, req_id(*c))
    }

    /// 记一条审计。`extra` 里塞入 route/method/status 等，**永远不含载荷**。
    fn audit(&self, rec: AuditRec<'_>) {
        let v = serde_json::json!({
            "ts": now_iso(),
            "reqId": rec.id,
            "side": rec.side,
            "route": rec.route,
            "mode": rec.mode,
            "peer": rec.peer,
            "method": rec.method,
            "path": rec.path,
            "upstream": rec.upstream,
            "decision": rec.decision,
            "reason": rec.reason,
            "status": rec.status,
            "reqBytes": rec.req_bytes,
            "respBytes": rec.resp_bytes,
            "ms": rec.ms,
            "provider": rec.provider,
        });
        self.audit.write(&v);
    }
}

struct AuditRec<'a> {
    id: &'a str,
    side: &'a str,
    route: &'a str,
    mode: &'a str,
    peer: &'a str,
    method: &'a str,
    path: &'a str,
    upstream: &'a str,
    decision: &'a str,
    reason: &'a str,
    status: u16,
    req_bytes: usize,
    resp_bytes: usize,
    ms: u128,
    /// 这次走的是哪个供应商（单供应商写法时是空串）。
    /// **必须记**：切换后要能解释“这几笔请求到底走了哪家”，否则账单没有依据。
    provider: &'a str,
}

/// 处理一个请求。所有拒绝都走这里，保证「拒绝也留痕」。
fn handle(shared: &Shared, req: &Req) -> Resp {
    let t0 = Instant::now();
    let (_, id) = shared.next_id();
    let method = req.method.to_uppercase();

    // 健康检查：不鉴权、不审计（否则探活会把审计刷满）。
    if req.path == "/healthz" {
        let names: Vec<&str> = shared.ready.cfg.routes.iter().map(|r| r.name.as_str()).collect();
        return Resp::json(
            200,
            &serde_json::json!({
                "ok": true,
                "pid": std::process::id(),
                "routes": names,
                "config": config_path().display().to_string(),
            }),
        );
    }

    let deny = |status: u16, reason: &str, route: &str, mode: &str, path: &str| -> Resp {
        shared.audit(AuditRec {
            id: &id,
            side: mode,
            route,
            mode,
            peer: "",
            method: &method,
            path,
            upstream: "",
            decision: "deny",
            reason,
            status,
            provider: "",   // 统一的拒绝路径没有“走哪家”这回事
            req_bytes: req.body.len(),
            resp_bytes: 0,
            ms: t0.elapsed().as_millis(),
        });
        Resp::json(status, &serde_json::json!({ "error": { "code": "denied", "message": reason } }))
    };

    // 路由形状：/v1/<route>/<上游路径>
    let rest = match req.path.strip_prefix("/v1/") {
        Some(r) => r,
        None => return deny(404, "只接受 /v1/<路由名>/<路径>", "", "", &req.path),
    };
    let (name, sub_raw) = match rest.split_once('/') {
        Some((n, p)) => (n, p),
        None => (rest, ""),
    };
    let route = match shared.ready.cfg.routes.iter().find(|r| r.name == name) {
        Some(r) => r,
        None => return deny(403, "没有这条路由", name, "", &req.path),
    };

    let sub = match normalize_sub_path(sub_raw) {
        Some(s) => s,
        None => return deny(403, "路径不合法（不许 .. / % / 反斜杠 / 空段）", name, &route.mode, &req.path),
    };

    // 鉴权：两种模式都要令牌（forward 在 loopback 上可免，见 prepare 的校验）。
    if !route.token.trim().is_empty() {
        match req.bearer() {
            Some(t) if ct_eq(t, route.token.trim()) => {}
            _ => return deny(401, "令牌不对或缺失", name, &route.mode, &req.path),
        }
    }

    if req.body.len() as u64 > shared.ready.max_body {
        return deny(413, "请求体超过上限", name, &route.mode, &req.path);
    }

    // A 侧本地白名单（可选）：早一步拒掉明显跑错的请求，最终仍由 B 说了算。
    if !route.is_accept() && !route.paths.is_empty() && !path_allowed(&route.paths, &sub) {
        return deny(403, "路径不在本地白名单里", name, &route.mode, &req.path);
    }

    if route.is_accept() {
        if !method_allowed(&route.methods, &method) {
            return deny(405, format!("方法 {method} 不在白名单里").as_str(), name, &route.mode, &req.path);
        }
        if !path_allowed(&route.paths, &sub) {
            return deny(403, "路径不在白名单里（请求没有发出）", name, &route.mode, &req.path);
        }
        let q = shared.quota_of(&route.name);
        let (ok, used) = q.take(route.quota_per_min);
        if !ok {
            return deny(429, format!("超过配额（{} 次/分钟，已用 {used}）", route.quota_per_min).as_str(), name, &route.mode, &req.path);
        }
        handle_accept(shared, req, route, &sub, &method, &id, t0)
    } else {
        handle_forward(shared, req, route, &sub, &method, &id, t0)
    }
}

/// B 侧：转发到**当前生效的那个上游**（多供应商时由 `ncc gateway switch` 决定），
/// 并注入本机凭据。
fn handle_accept(
    shared: &Shared,
    req: &Req,
    route: &Route,
    sub: &str,
    method: &str,
    id: &str,
    t0: Instant,
) -> Resp {
    let up = shared.active_of(route);
    let target = match parse_target(&up.target) {
        Ok(t) => t,
        Err(e) => return Resp::text(500, &format!("配置里的 target 有问题：{e}")),
    };
    // 路径：默认按原样拼；供应商声明了 rewrite 就换成这一家的路径。
    // **只换路径，不碰 body** —— body 形状是厂商语义，猜错了就是静默发错请求。
    let mapped = up
        .rewrite
        .iter()
        .find(|(from, _)| normalize_sub_path(from).as_deref() == Some(sub))
        .map(|(_, to)| to.trim().to_string());
    let url = target.join(mapped.as_deref().unwrap_or(sub));

    let mut r = shared.agent.request(method, &url);
    // 只透传这两个头；调用方的 Authorization **绝不**透传 —— 上游凭据由 inject 决定。
    for h in ["content-type", "accept"] {
        if let Some(v) = req.header(h) {
            r = r.set(h, v);
        }
    }
    for (k, v) in &up.inject {
        r = r.set(k, v);
    }
    r = r.set("x-ncc-gateway-route", &route.name);
    if !up.name.is_empty() {
        r = r.set("x-ncc-gateway-provider", &up.name);
    }

    let out = if req.body.is_empty() { r.call() } else { r.send_bytes(&req.body) };
    let (status, body) = match out {
        Ok(resp) => read_upstream(resp),
        Err(ureq::Error::Status(code, resp)) => {
            // 上游明确回了错误（含 3xx —— 我们不跟随重定向）：如实透传给调用方。
            let (_, b) = read_upstream(resp);
            (code, b)
        }
        Err(e) => {
            shared.audit(AuditRec {
                id,
                side: "accept",
                route: &route.name,
                mode: &route.mode,
                peer: "",
                method,
                path: &req.path,
                upstream: &url,
                decision: "deny",
                reason: "上游不可达",
                status: 502,
                req_bytes: req.body.len(),
                resp_bytes: 0,
                ms: t0.elapsed().as_millis() as u128,
                provider: &up.name,
            });
            return Resp::json(
                502,
                &serde_json::json!({ "error": { "code": "upstream_unreachable", "message": format!("{e}") } }),
            );
        }
    };

    shared.audit(AuditRec {
        id,
        side: "accept",
        route: &route.name,
        mode: &route.mode,
        peer: "",
        method,
        path: &req.path,
        upstream: &url,
        decision: "allow",
        reason: "",
        status,
        req_bytes: req.body.len(),
        resp_bytes: body.len(),
        ms: t0.elapsed().as_millis() as u128,
        provider: &up.name,
    });

    let mut extra = vec![("x-ncc-gateway-route".into(), route.name.clone())];
    if !up.name.is_empty() {
        extra.push(("x-ncc-gateway-provider".into(), up.name.clone()));
    }
    Resp { status, content_type: "application/json", body, extra }
}

/// A 侧：把请求转给配对的 peer（B）。
fn handle_forward(
    shared: &Shared,
    req: &Req,
    route: &Route,
    sub: &str,
    method: &str,
    id: &str,
    t0: Instant,
) -> Resp {
    let peer = match parse_target(&route.peer_url) {
        Ok(t) => t,
        Err(e) => return Resp::text(500, &format!("配置里的 peer_url 有问题：{e}")),
    };
    // 原样把「路由名 + 子路径」交给 B —— 目标由 B 决定，A 不替 B 决定。
    let url = format!("{}://{}{}/v1/{}/{}", peer.scheme, peer.authority, peer.base, route.name, sub);

    let mut r = shared.agent.request(method, &url);
    if let Some(v) = req.header("content-type") {
        r = r.set("content-type", v);
    }
    if let Some(v) = req.header("accept") {
        r = r.set("accept", v);
    }
    r = r.set("authorization", &format!("Bearer {}", route.peer_token.trim()));

    let out = if req.body.is_empty() { r.call() } else { r.send_bytes(&req.body) };
    let (status, body) = match out {
        Ok(resp) => read_upstream(resp),
        Err(ureq::Error::Status(code, resp)) => {
            let (_, b) = read_upstream(resp);
            (code, b)
        }
        Err(e) => {
            // A5：peer 不可达就是**不可用**，绝不降级成任何形式的中转。
            shared.audit(AuditRec {
                id,
                side: "forward",
                route: &route.name,
                mode: &route.mode,
                peer: &peer.authority,
                method,
                path: &req.path,
                upstream: &url,
                decision: "deny",
                reason: "peer 不可达（不降级、不中转）",
                status: 502,
                req_bytes: req.body.len(),
                resp_bytes: 0,
                ms: t0.elapsed().as_millis(),
                provider: "",
            });
            return Resp::json(
                502,
                &serde_json::json!({ "error": { "code": "peer_unreachable", "message": format!("{e}") } }),
            );
        }
    };

    shared.audit(AuditRec {
        id,
        side: "forward",
        route: &route.name,
        mode: &route.mode,
        peer: &peer.authority,
        method,
        path: &req.path,
        upstream: &url,
        decision: "allow",
        reason: "",
        status,
        req_bytes: req.body.len(),
        resp_bytes: body.len(),
        ms: t0.elapsed().as_millis(),
        provider: "",   // 转发侧不选供应商：哪家由对端决定（最终裁决看 B 侧审计）
    });

    Resp {
        status,
        content_type: "application/json",
        body,
        extra: vec![],
    }
}

fn read_upstream(resp: ureq::Response) -> (u16, Vec<u8>) {
    let status = resp.status();
    let mut body = Vec::new();
    // 上游响应上限 8 MiB：再大也不该由这个代理搬。
    let _ = resp.into_reader().take(8 * 1024 * 1024).read_to_end(&mut body);
    (status, body)
}

/* ============================ 子命令 ============================ */

pub fn init(force: bool) -> Result<()> {
    let p = config_path();
    if p.exists() && !force {
        bail!("{} 已存在（要覆盖加 --force）", p.display());
    }
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let sample = serde_json::json!({
        "listen": "127.0.0.1:9810",
        "allow_plaintext": false,
        "max_body_bytes": 1048576,
        "api_server": "",
        "gateway_id": "",
        "gateway_token": "",
        "namespace": "",
        "name": "",
        "heartbeat_sec": 30,
        "report_sec": 300,
        "routes": [
            {
                "name": "llm",
                "mode": "accept",
                "hur": "/path/to/egress-package",
                "target": "https://api.openai.com/v1",
                "paths": ["chat/completions"],
                "methods": ["POST"],
                "inject": { "Authorization": "Bearer sk-把-B-自己的密钥放这里" },
                "token": "换成-B-自己生成的至少16位令牌",
                "quota_per_min": 30
            },
            {
                "name": "llm",
                "mode": "forward",
                "peer_url": "https://b.example.com:9810",
                "peer_token": "与对端 accept 路由的 token 相同",
                "paths": ["chat/completions"],
                "token": "本机调用方要带的令牌（监听在 127.0.0.1 时可留空）"
            }
        ]
    });
    std::fs::write(&p, serde_json::to_string_pretty(&sample)?)?;
    println!("✅ 已写入示例配置：{}", p.display());
    println!("   accept 路由 = 我这台机器提供出口（B 侧）；forward 路由 = 借对端的出口（A 侧）");
    println!("   改完先看一遍：ncc gateway check");
    println!("   注意 accept 路由的 \"hur\"：出口的定义写在那个包的 hur.json（egress{{}}）里，");
    println!("   不在本文件里 —— 这样“这个出口能干什么”是一份可签名、可分发的声明。");
    Ok(())
}

/// `ncc gateway check` —— 只校验配置，不启动。
pub fn check() -> Result<()> {
    let cfg = load_config()?;
    let ready = prepare(cfg)?;
    println!("✅ 配置可用：{}", config_path().display());
    println!("   监听 {} · 审计目录 {}", ready.listen, ready.audit_dir.display());
    for r in &ready.cfg.routes {
        if r.is_accept() {
            println!(
                "   [accept ] {:<12} → {} （路径 {} · 方法 {} · 配额 {}/分）",
                r.name,
                r.target,
                r.paths.join(","),
                r.methods.join(","),
                if r.quota_per_min == 0 { "不限".to_string() } else { r.quota_per_min.to_string() }
            );
            // S2b：出口的定义来自包 —— 把「谁背书的」与「声明了多少」一并打出来，
            // 好让运维一眼核对自己配的范围确实比包声明更窄。
            if let Some(b) = ready.backing.iter().find(|b| b.route == r.name) {
                println!("             背书包 {} {}（{}）", b.id, b.version, b.dir);
                if !b.providers.is_empty() {
                    println!(
                        "             供应商 {} · 当前 {}（`ncc gateway switch {} <供应商>` 可切）",
                        b.providers.join(" / "),
                        b.active,
                        r.name
                    );
                }
                println!(
                    "             声明范围 {} · 路径 {} · 方法 {} · 注入 {}",
                    b.target,
                    b.paths.join(","),
                    b.methods.join(","),
                    if b.inject.is_empty() { "（无）".to_string() } else { b.inject.join(",") }
                );
                println!(
                    "             本配置用了声明里的 {}/{} 条路径 · {}/{} 个方法（只能更窄）",
                    r.paths.len(),
                    b.paths.len(),
                    r.methods.len(),
                    b.methods.len()
                );
            }
        } else {
            println!("   [forward] {:<12} → {} （路径 {}）", r.name, r.peer_url, if r.paths.is_empty() { "交给对端决定".into() } else { r.paths.join(",") });
        }
    }
    Ok(())
}

/// `ncc gateway run` —— 常驻。
pub fn run() -> Result<()> {
    let cfg = load_config()?;
    let ready = prepare(cfg)?;
    let listen = ready.listen.clone();
    let audit_dir = ready.audit_dir.clone();
    let route_count = ready.cfg.routes.len();

    // 出站 agent：**不跟随重定向**（跟随 = 白名单可被 3xx 带去别的域），并给足超时。
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(30))
        .redirects(0)
        .user_agent(concat!("ncc-gateway/", env!("CARGO_PKG_VERSION")))
        .build();

    let audit = Audit::new(audit_dir.clone());
    let audit_path = audit.file().display().to_string();
    let shared = Arc::new(Shared {
        ready,
        quota: Mutex::new(BTreeMap::new()),
        audit,
        agent,
        counter: Mutex::new(0),
        active: RwLock::new(BTreeMap::new()),
    });
    shared.set_active(&shared.ready.cfg.routes);

    let listener = TcpListener::bind(&listen).with_context(|| format!("监听不了 {listen}"))?;
    println!("NCC Gateway 已在 {listen} 上运行");
    println!("   {route_count} 条路由 · 审计 {audit_path}");
    // 换供应商是运营动作，不该要求重启：看配置文件 mtime，变了就热应用。
    spawn_config_watcher(shared.clone());
    let multis: Vec<String> = shared
        .ready
        .cfg
        .routes
        .iter()
        .filter(|r| !r.providers.is_empty())
        .map(|r| format!("{}={}", r.name, effective_provider_name(r)))
        .collect();
    if !multis.is_empty() {
        println!("  多供应商路由：{}（`ncc gateway switch <路由> <供应商>` 热切换）", multis.join(" · "));
    }
    println!("   健康检查：curl http://{listen}/healthz");
    println!("   Ctrl+C 停止。本进程**不向任何 NCC 服务端上报载荷**。");

    // 控制面接入（F2/F3/F5）：绑了才起心跳与上报线程。
    // 没绑也能跑 —— 审计照落本地（本地优先，控制面只是"看得到摘要"）。
    let bound = !shared.ready.cfg.gateway_token.trim().is_empty()
        && !shared.ready.cfg.api_server.trim().is_empty();
    if bound {
        let hb = if shared.ready.cfg.heartbeat_sec == 0 {
            crate::gwreport::DEFAULT_HEARTBEAT_SEC
        } else {
            shared.ready.cfg.heartbeat_sec
        };
        let rp = if shared.ready.cfg.report_sec == 0 {
            crate::gwreport::DEFAULT_REPORT_SEC
        } else {
            shared.ready.cfg.report_sec
        };
        println!(
            "   控制面 {}（{}）· 心跳 {hb}s · 摘要上报 {rp}s",
            shared.ready.cfg.api_server,
            shared.ready.cfg.gateway_id
        );
        println!(
            "   ⚠ 上传的是**聚合摘要**（计数 / 字节总量 / 主机名 / 状态码桶 / 分位），\
             逐条审计（含路径）只在本机：{audit_path}"
        );
        // 上报线程要一份 CLI 配置（`load()` 自带兜底，缺文件也能构造出来）；
        // 控制面连不上时线程内部只会打日志 —— 网关的可用性不该因此打折。
        crate::gwreport::spawn(crate::config::load(), hb, rp);
    } else {
        println!(
            "   控制面：未绑定 —— 审计只落本地（`ncc gateway bind --namespace @你的组织` 可接入，\
             接入后控制面只收到**摘要**）"
        );
    }

    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠ 连接异常：{e}");
                continue;
            }
        };
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut s = stream;
            match read_request(&mut s, shared.ready.max_body) {
                Ok(req) => {
                    let resp = handle(&shared, &req);
                    write_resp(&mut s, &resp);
                }
                Err(ReadError::Bad(status, why)) => {
                    write_resp(&mut s, &Resp::json(status, &serde_json::json!({
                        "error": { "code": "bad_request", "message": why }
                    })));
                }
                Err(ReadError::Io(e)) => {
                    // 对端断开 / 超时是常态，不刷屏；要查细节就开 NCC_GATEWAY_DEBUG=1。
                    if std::env::var("NCC_GATEWAY_DEBUG").is_ok() {
                        eprintln!("⚠ 读请求失败：{e}");
                    }
                }
            }
        });
    }
    Ok(())
}

/// `ncc gateway switch <路由> <供应商>` —— 换一家上游。
///
/// 为什么做成"改文件 + 热应用"而不是给运行中的进程发指令：**配置是唯一真相**。
/// 一份可以被 `switch` 直接改写内存的进程，重启后会倒回旧值 —— 那种"切了就忘"的
/// 开关最难排查。所以：`switch` 只改 `gateway.json`，运行中的网关看 mtime 自己跟上。
pub fn switch(route: &str, provider: &str) -> Result<()> {
    let mut cfg = load_config()?;
    let r = cfg
        .routes
        .iter_mut()
        .find(|r| r.name == route.trim())
        .ok_or_else(|| anyhow!("没有这条路由：{}", route.trim()))?;
    if !r.is_accept() {
        bail!("路由 {} 是 {} 模式 —— 供应商切换只对 accept（提供出口）有意义", r.name, r.mode);
    }
    if r.providers.is_empty() {
        bail!(
            "路由 {} 是单供应商写法（没有 providers）。\n要能切，先在包的 egress 声明里写 providers[]，\n再把本地配置的 target/inject 换成 providers 表。",
            r.name
        );
    }
    let name = provider.trim();
    if !r.providers.contains_key(name) {
        let names: Vec<&str> = r.providers.keys().map(|s| s.as_str()).collect();
        bail!("路由 {} 没声明供应商「{name}」（可选：{}）", r.name, names.join(", "));
    }
    let before = effective_provider_name(r);
    if before == name {
        println!("路由 {route} 已经在用「{name}」，没变。");
        return Ok(());
    }
    r.active = name.to_string();
    save_config_pub(&cfg)?;
    println!("已切换 {}：{} → {}", route.trim(), if before.is_empty() { "（单供应商）" } else { before.as_str() }, name);
    println!("  配置 {}", config_path().display());
    println!("  运行中的网关会在 ~1 秒内热应用（不用重启）；`ncc gateway status` 看当前生效的。");
    Ok(())
}

/// 配置文件 watcher：`switch` 只改文件，常驻进程靠 mtime 自己跟上。
///
/// **只热应用"换供应商"**；监听地址 / 审计目录 / body 上限 / 路由增删与 mode 变了
/// 都要求重启（那些改动重载一半会让人以为生效了，比不生效更糟）。旧配置继续用，
/// 并把"要重启"打在屏幕上。
fn spawn_config_watcher(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let p = config_path();
        let mtime = || std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let mut last = mtime();
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let cur = mtime();
            if cur == last {
                continue;
            }
            last = cur;
            let cfg = match load_config() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("⚠ 配置变了但读不了（继续用旧的）：{e}");
                    continue;
                }
            };
            let ready = match prepare(cfg) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("⚠ 新配置没通过校验（继续用旧的）：{e}");
                    continue;
                }
            };
            let old = &shared.ready.cfg;
            if ready.listen != old.listen || ready.cfg.max_body_bytes != old.max_body_bytes || ready.audit_dir != old.audit_dir {
                eprintln!("⚠ 监听 / body 上限 / 审计目录变了 —— 这些要**重启**才生效（其余改动已忽略）");
                continue;
            }
            let same_shape = ready.cfg.routes.len() == old.routes.len()
                && ready.cfg.routes.iter().all(|n| old.routes.iter().any(|o| o.name == n.name && o.mode == n.mode));
            if !same_shape {
                eprintln!("⚠ 路由增删或 mode 变了 —— 要**重启**才生效（其余改动已忽略）");
                continue;
            }
            let mut moves: Vec<String> = Vec::new();
            for n in &ready.cfg.routes {
                let Some(o) = old.routes.iter().find(|o| o.name == n.name) else { continue };
                if n.providers.is_empty() && o.providers.is_empty() {
                    continue;
                }
                let (b, a) = (effective_provider_name(o), effective_provider_name(n));
                if b != a {
                    moves.push(format!("{}：{} → {}", n.name, b, a));
                }
            }
            shared.set_active(&ready.cfg.routes);
            for m in &moves {
                println!("⟳ 已热切换 {m}");
            }
            if moves.is_empty() {
                println!("⟳ 配置已重载（供应商没变，只更新了名称/凭据等运行时可变的字段）");
            }
        }
    });
}

/// `ncc gateway status` —— 配置摘要 + 是否在跑（探自己的 /healthz）。
pub fn status() -> Result<()> {
    let cfg = load_config()?;
    let ready = prepare(cfg)?;
    println!("配置 {}", config_path().display());
    println!("  监听 {} · 审计目录 {}", ready.listen, ready.audit_dir.display());
    let live = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(400))
        .timeout_read(Duration::from_millis(800))
        .build()
        .get(&format!("http://{}/healthz", ready.listen))
        .call();
    match live {
        Ok(r) => {
            let body = r.into_string().unwrap_or_default();
            println!("  状态 ✅ 在跑（{body}）");
        }
        Err(_) => println!("  状态 ○ 没在跑（或不在这个地址上）"),
    }
    // 控制面接入状态（F2/F3/F5）。
    if ready.cfg.gateway_token.trim().is_empty() || ready.cfg.api_server.trim().is_empty() {
        println!("  控制面 ○ 未绑定（审计只落本地；`ncc gateway bind` 可接入）");
    } else {
        let hb = if ready.cfg.heartbeat_sec == 0 { 30 } else { ready.cfg.heartbeat_sec };
        let rp = if ready.cfg.report_sec == 0 { 300 } else { ready.cfg.report_sec };
        println!(
            "  控制面 ✅ {} · {} · @{} · 心跳 {}s · 上报 {}s",
            ready.cfg.api_server, ready.cfg.gateway_id, ready.cfg.namespace, hb, rp
        );
        let hint = crate::gwreport::pending_hint();
        if !hint.is_empty() {
            println!("  待传 {hint}");
        }
    }
    for r in &ready.cfg.routes {
        if r.is_accept() && !r.providers.is_empty() {
            let names: Vec<String> = r.providers.keys().cloned().collect();
            println!(
                "  [{:<7}] {:<12} {} · 供应商 {} · 当前 {}",
                r.mode,
                r.name,
                if r.target.trim().is_empty() { "（多供应商）" } else { r.target.as_str() },
                names.join(" / "),
                effective_provider_name(r)
            );
            continue;
        }
        println!(
            "  [{:<7}] {:<12} {}",
            r.mode,
            r.name,
            if r.is_accept() { r.target.as_str() } else { r.peer_url.as_str() }
        );
    }
    Ok(())
}

/// `ncc gateway audit [--tail N] [--json]` —— 看本地审计（只读，永远只看元数据）。
pub fn audit_cmd(tail: usize, json: bool) -> Result<()> {
    let cfg = load_config().unwrap_or_default();
    let dir = if cfg.audit_dir.trim().is_empty() {
        ncc_dir().join("gateway-audit")
    } else {
        PathBuf::from(cfg.audit_dir.trim())
    };
    if !dir.exists() {
        println!("还没有审计目录：{}", dir.display());
        return Ok(());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    files.sort();
    let mut lines: Vec<String> = Vec::new();
    for f in &files {
        if let Ok(s) = std::fs::read_to_string(f) {
            lines.extend(s.lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_string()));
        }
    }
    let total = lines.len();
    let start = total.saturating_sub(tail);
    if json {
        for l in &lines[start..] {
            println!("{l}");
        }
        return Ok(());
    }
    let mut allow = 0;
    let mut deny = 0;
    for l in &lines {
        match serde_json::from_str::<serde_json::Value>(l) {
            Ok(v) if v["decision"] == "allow" => allow += 1,
            Ok(_) => deny += 1,
            Err(_) => {}
        }
    }
    println!("审计 {} 条（放行 {allow} · 拒绝 {deny}）· 目录 {}", total, dir.display());
    println!("{:<24} {:<7} {:<10} {:<6} {:<7} {}", "ts", "side", "route", "状态", "决定", "理由 / 上游");
    for l in &lines[start..] {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
            let reason = v["reason"].as_str().unwrap_or("");
            let upstream = v["upstream"].as_str().unwrap_or("");
            println!(
                "{:<24} {:<7} {:<10} {:<6} {:<7} {}",
                v["ts"].as_str().unwrap_or(""),
                v["side"].as_str().unwrap_or(""),
                v["route"].as_str().unwrap_or(""),
                v["status"].as_u64().unwrap_or(0),
                v["decision"].as_str().unwrap_or(""),
                if reason.is_empty() { upstream } else { reason },
            );
        }
    }
    Ok(())
}

/* ============================ 测试 ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_time_matches_known_instants() {
        assert_eq!(iso_from_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_secs(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(iso_from_secs(1_700_000_000), "2023-11-14T22:13:20Z");
        // 跨世纪 + 闰年路径：20722 天 = 2026-09-26
        assert_eq!(iso_from_secs(1_790_380_800), "2026-09-26T00:00:00Z");
        // 往前一天、带时分秒
        assert_eq!(iso_from_secs(1_790_332_800), "2026-09-25T10:40:00Z");
    }

    #[test]
    fn sub_path_rejects_traversal_and_encoding() {
        assert_eq!(normalize_sub_path("/chat/completions/").as_deref(), Some("chat/completions"));
        assert_eq!(normalize_sub_path("chat/completions").as_deref(), Some("chat/completions"));
        // 一切都可能跑出白名单的写法都拒
        for bad in ["..", "../x", "a/../b", "a//b", "%2e%2e/x", "a%2fb", "a\\b", "a?x=1", "a#f", "a b", ""] {
            assert!(normalize_sub_path(bad).is_none(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn path_allow_list_is_exact_match() {
        let list = vec!["chat/completions".to_string()];
        assert!(path_allowed(&list, "chat/completions"));
        // 不做前缀魔法：多一段就不算命中
        assert!(!path_allowed(&list, "chat/completions/extra"));
        assert!(!path_allowed(&list, "chat"));
        assert!(!path_allowed(&list, "models"));
        // 归一化后等价
        assert!(path_allowed(&list, "/chat/completions"));
    }

    #[test]
    fn target_parsing_and_joining() {
        let t = parse_target("https://api.openai.com/v1").unwrap();
        assert_eq!(t.host, "api.openai.com");
        assert_eq!(t.join("chat/completions"), "https://api.openai.com/v1/chat/completions");

        let t = parse_target("https://api.openai.com/v1/").unwrap();
        assert_eq!(t.join("models"), "https://api.openai.com/v1/models");

        // 带端口
        let t = parse_target("http://127.0.0.1:8899").unwrap();
        assert_eq!(t.join("x"), "http://127.0.0.1:8899/x");

        for bad in ["api.openai.com", "ftp://x/y", "https://", "https://user@host/x"] {
            assert!(parse_target(bad).is_err(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn ct_eq_works_without_early_exit_bugs() {
        assert!(ct_eq("abcdef", "abcdef"));
        assert!(!ct_eq("abcdef", "abcdeg"));
        assert!(!ct_eq("abcdef", "abcde"));
        assert!(!ct_eq("", ""));
    }

    fn base_cfg() -> GatewayConfig {
        let dir = tmp_pkg(
            "base",
            "llm",
            "https://api.openai.com/v1",
            &["chat/completions"],
            &["POST"],
            &["Authorization"],
        );
        GatewayConfig {
            listen: "127.0.0.1:9810".into(),
            routes: vec![Route {
                name: "llm".into(),
                mode: "accept".into(),
                hur: dir.display().to_string(),
                target: "https://api.openai.com/v1".into(),
                paths: vec!["chat/completions".into()],
                methods: vec!["POST".into()],
                token: "0123456789abcdef".into(),
                quota_per_min: 2,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// 造一个临时包目录，只写 `hur.json` —— 网关切的就是那份出口声明。
    fn tmp_pkg(
        tag: &str,
        route: &str,
        target: &str,
        paths: &[&str],
        methods: &[&str],
        inject: &[&str],
    ) -> PathBuf {
        // ⚠️ 目录必须**每次调用唯一**：测试是并发跑的，两个测试共用同一个夹具目录
        // 会互相把对方的 hur.json 写坏（曾因此出现"单跑绿、一起跑红"的假失败）。
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let uniq = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("ncc-gw-test-{tag}-{}-{uniq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = target.split('/').nth(2).unwrap_or("");
        let pkg = serde_json::json!({
            "spec": "harness-use-package/v1",
            "kind": "harness",
            "id": "A-gw-test-000001",
            "name": "gateway test package",
            "version": "0.1.0",
            "summary": "只为了给网关的 accept 路由背书",
            "entry": "src/agent.ts",
            "permissions": { "network": [host] },
            "egress": { "provides": [ {
                "name": route,
                "target": target,
                "paths": paths,
                "methods": methods,
                "inject": inject
            } ] }
        });
        std::fs::write(dir.join("hur.json"), serde_json::to_string_pretty(&pkg).unwrap()).unwrap();
        dir
    }

    #[test]
    fn accept_route_without_token_is_refused() {
        let mut cfg = base_cfg();
        cfg.routes[0].token = "short".into();
        assert!(prepare(cfg).is_err());
    }

    #[test]
    fn accept_route_without_paths_or_methods_is_refused() {
        let mut cfg = base_cfg();
        cfg.routes[0].paths.clear();
        assert!(prepare(cfg).is_err());

        let mut cfg = base_cfg();
        cfg.routes[0].methods.clear();
        assert!(prepare(cfg).is_err());
    }

    #[test]
    fn non_loopback_listen_requires_explicit_opt_in() {
        let mut cfg = base_cfg();
        cfg.listen = "0.0.0.0:9810".into();
        assert!(prepare(cfg.clone()).is_err());
        cfg.allow_plaintext = true;
        assert!(prepare(cfg).is_ok());
    }

    #[test]
    fn plaintext_target_outside_loopback_is_refused() {
        let mut cfg = base_cfg();
        cfg.routes[0].target = "http://api.example.com/v1".into();
        assert!(prepare(cfg).is_err());
        // loopback 的 http 允许（本地联调）—— 但要换成声明了本机 target 的那份包
        let mut cfg = base_cfg();
        cfg.routes[0].target = "http://127.0.0.1:8899/v1".into();
        cfg.routes[0].hur = tmp_pkg(
            "loopback",
            "llm",
            "http://127.0.0.1:8899/v1",
            &["chat/completions"],
            &["POST"],
            &[],
        )
        .display()
        .to_string();
        assert!(prepare(cfg).is_ok());
    }

    /* ---------------- S2b：accept 路由由包声明背书 ---------------- */

    #[test]
    fn accept_route_without_a_backing_package_is_refused() {
        let mut cfg = base_cfg();
        cfg.routes[0].hur = String::new();
        let msg = format!("{}", prepare(cfg).unwrap_err());
        assert!(msg.contains("没绑 HUR 包"), "{msg}");
    }

    #[test]
    fn accept_route_wider_than_the_package_declaration_is_refused() {
        // 多一条未声明的路径
        let mut cfg = base_cfg();
        cfg.routes[0].paths.push("models".into());
        let msg = format!("{}", prepare(cfg).unwrap_err());
        assert!(msg.contains("超出声明的范围"), "{msg}");
        assert!(msg.contains("models"), "{msg}");

        // 多一个未声明的方法
        let mut cfg = base_cfg();
        cfg.routes[0].methods.push("DELETE".into());
        assert!(prepare(cfg).is_err());

        // 换个域：配置不能把路由指向声明之外的地址
        let mut cfg = base_cfg();
        cfg.routes[0].target = "https://evil.example.com/v1".into();
        assert!(prepare(cfg).is_err());

        // 注入一个声明里没有的头：等于让网关带出一份没被声明过的凭据
        let mut cfg = base_cfg();
        cfg.routes[0].inject.insert("X-Api-Key".into(), "sk-x".into());
        assert!(prepare(cfg).is_err());
    }

    #[test]
    fn accept_route_must_match_a_declared_route_name() {
        let mut cfg = base_cfg();
        cfg.routes[0].hur = tmp_pkg(
            "other-name",
            "other",
            "https://api.openai.com/v1",
            &["chat/completions"],
            &["POST"],
            &[],
        )
        .display()
        .to_string();
        let msg = format!("{}", prepare(cfg).unwrap_err());
        assert!(msg.contains("找不到同名出口声明"), "{msg}");
    }

    #[test]
    fn backing_package_with_a_sloppy_declaration_is_refused() {
        // 包自己就没边界（paths 为空 = 放行该上游下一切路径）→ 绑定时被 R10 拦下
        let mut cfg = base_cfg();
        cfg.routes[0].hur = tmp_pkg("sloppy", "llm", "https://api.openai.com/v1", &[], &["POST"], &[])
            .display()
            .to_string();
        let msg = format!("{}", prepare(cfg).unwrap_err());
        assert!(msg.contains("R10"), "{msg}");
    }

    #[test]
    fn narrower_config_than_the_declaration_is_accepted() {
        // 包声明两条路径 / 两个方法，配置只用其中一条 —— 合规，且声明范围原样带出来
        let dir = tmp_pkg(
            "narrow",
            "llm",
            "https://api.openai.com/v1",
            &["chat/completions", "models"],
            &["POST", "GET"],
            &["Authorization"],
        );
        let mut cfg = base_cfg();
        cfg.routes[0].hur = dir.display().to_string();
        let ready = prepare(cfg).unwrap();
        let b = &ready.backing[0];
        assert_eq!(b.route, "llm");
        assert_eq!(b.paths.len(), 2, "声明范围要原样带出来");
        assert_eq!(b.methods.len(), 2);
        assert_eq!(b.target, "https://api.openai.com/v1");
        assert_eq!(b.inject, vec!["Authorization".to_string()]);
    }

    #[test]
    fn forward_route_needs_peer_and_peer_token() {
        let mut cfg = base_cfg();
        cfg.routes.push(Route {
            name: "llm".into(),
            mode: "forward".into(),
            peer_url: "http://127.0.0.1:9811".into(),
            peer_token: String::new(),
            ..Default::default()
        });
        assert!(prepare(cfg).is_err());

        let mut cfg = base_cfg();
        cfg.routes.push(Route {
            name: "llm".into(),
            mode: "forward".into(),
            peer_url: String::new(),
            peer_token: "0123456789abcdef".into(),
            ..Default::default()
        });
        assert!(prepare(cfg).is_err());
    }

    #[test]
    fn duplicate_route_names_per_mode_are_refused() {
        let mut cfg = base_cfg();
        cfg.routes.push(cfg.routes[0].clone());
        assert!(prepare(cfg).is_err());
    }

    #[test]
    fn quota_allows_up_to_limit_then_denies() {
        let q = Quota::new();
        assert_eq!(q.take(2), (true, 1));
        assert_eq!(q.take(2), (true, 2));
        let (ok, used) = q.take(2);
        assert!(!ok);
        assert_eq!(used, 2);
        // 0 = 不限
        let q = Quota::new();
        for _ in 0..100 {
            assert!(q.take(0).0);
        }
    }

    #[test]
    fn unknown_mode_is_refused() {
        let mut cfg = base_cfg();
        cfg.routes[0].mode = "relay".into();
        assert!(prepare(cfg).is_err());
    }
}
