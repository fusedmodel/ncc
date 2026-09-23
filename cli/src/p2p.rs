// NCC P2P：跨局域网直连的控制面客户端 + 打洞条件判断。
//
// 三条子命令，对应 prd/ncc-p2p-data.md §5.4 的三层判断：
//
//	probe  ① 单边预检（**纯本地**，不需要服务器）：UDP 出站 / srflx / NAT 映射行为 / 过滤行为
//	ice    看控制面下发的 STUN/TURN 配置（`probe` 会优先用它）
//	check  ③ 真实建连检查：经服务端信令交换双方映射地址，然后**双向对打 STUN Binding 请求**
//	       —— 这就是 ICE 的 connectivity check（收到回包 = 这条路径真的通），
//	       不传业务字节，因此很快（1~3 秒）。②「双方画像比对」由服务端的票据/节点面承载。
//	ticket 票据：ncc p2p ticket create | list | verify | revoke（连接 ≠ 授权）
//
// 为什么手搓 STUN 而不用现成 crate：STUN Binding 请求只有 20 字节头 + 1~2 个属性，
// 而 CLI 要的只是「映射地址 + OTHER-ADDRESS + CHANGE-REQUEST」这三样。
// 手搓换来零新依赖（ureq/clap 之外不引入 tokio/rustls 生态），交叉编译不受影响。
use crate::api;
use crate::config::{self, CliConfig};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/* ============================== 参数 ============================== */

#[derive(clap::Subcommand)]
pub enum P2pCmd {
    /// 打洞条件预检（纯本地）：本机 NAT 画像 + 结论
    Probe(ProbeArgs),
    /// 看控制面下发的 ICE 配置（STUN / TURN）
    Ice,
    /// 真实建连检查：与对端节点互打 STUN 请求，确认这条直连路径能不能通
    Check(CheckArgs),
    /// 信令调试：send / recv（正常流程由 check 自己用）
    Signal(SignalArgs),
    /// 票据：谁（哪个节点）能从谁那里取哪条资源（连接 ≠ 授权）
    Ticket(TicketArgs),
}

#[derive(clap::Args)]
pub struct ProbeArgs {
    /// 只用这些 STUN（逗号分隔；不给就用控制面配置或内置默认）
    #[arg(long)]
    pub stun: Option<String>,
    /// 不向服务端取配置（纯离线预检）
    #[arg(long)]
    pub offline: bool,
    /// 机器可读输出
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct CheckArgs {
    /// 对端节点：LD-… 或 @命名空间/节点slug（与 --addr 二选一）
    #[arg(required_unless_present = "addr")]
    pub peer: Option<String>,
    /// 直接对一个映射地址对打，**不走云端信令**（例：内网节点 `ncc registry p2p self` 报出的 mapped）
    #[arg(long)]
    pub addr: Option<String>,
    /// 我以哪个节点身份参与（缺省：我名下唯一的 living 节点）
    #[arg(long)]
    pub from: Option<String>,
    /// 等对端出现与收包的秒数
    #[arg(long, default_value_t = 20)]
    pub wait: u64,
    /// 只用这些 STUN（逗号分隔；不给就用控制面配置）
    #[arg(long)]
    pub stun: Option<String>,
    /// 会话名（两端必须一致；缺省由两端节点 id 推导，所以两端不用手工对齐）
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct SignalArgs {
    #[command(subcommand)]
    pub action: SignalAction,
}

#[derive(clap::Subcommand)]
pub enum SignalAction {
    /// 发一条信令
    Send {
        /// 收件节点：LD-… 或 @命名空间/节点slug
        #[arg(long)]
        to: String,
        /// 我以哪个节点身份发（缺省：我名下唯一的 living 节点）
        #[arg(long)]
        from: Option<String>,
        #[arg(long, default_value = "check")]
        kind: String,
        #[arg(long, default_value = "")]
        payload: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// 长轮询取一条信令（取出即消费）
    Recv {
        /// 我的节点：LD-… 或 @命名空间/节点slug（缺省自动）
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 15)]
        wait: u64,
    },
}

#[derive(clap::Args)]
pub struct TicketArgs {
    #[command(subcommand)]
    pub action: TicketAction,
}

#[derive(clap::Subcommand)]
pub enum TicketAction {
    /// 发票据（创建时回显票据串一次，之后取不回来）
    Create {
        /// 发给哪个节点：LD-… 或 @命名空间/节点slug
        #[arg(long)]
        peer: String,
        /// 资源引用，如 @ns/slug@1.0.0
        #[arg(long = "ref")]
        reference: String,
        /// pull（对端来取，默认）/ push（我推过去）
        #[arg(long, default_value = "pull")]
        direction: String,
        /// 字节上限（0 = 不限）
        #[arg(long, default_value_t = 0)]
        max_bytes: i64,
        /// 有效期秒数（默认 300，最长 7 天）
        #[arg(long, default_value_t = 300)]
        expires_in: i64,
        #[arg(long)]
        note: Option<String>,
    },
    /// 我发出的票据
    List,
    /// 对端出示票据（校验可用性；--bytes 同时记用量）
    Verify {
        ticket: String,
        #[arg(long, default_value_t = 0)]
        bytes: i64,
        #[arg(long)]
        peer: Option<String>,
    },
    /// 撤销（立即失效）
    Revoke { id: String },
}

/* ============================== STUN 原语 ============================== */

const MAGIC: u32 = 0x2112_A442;
const BINDING_REQ: u16 = 0x0001;
const BINDING_OK: u16 = 0x0101;
const ATTR_MAPPED: u16 = 0x0001;
const ATTR_CHANGE: u16 = 0x0003;
const ATTR_XOR_MAPPED: u16 = 0x0020;
const ATTR_OTHER: u16 = 0x802c;
const CHANGE_BOTH: u32 = 0x06; // 改 IP + 改端口（RFC 5780 Test II）
const CHANGE_PORT: u32 = 0x02; // 只改端口（RFC 5780 Test III）
const DEFAULT_STUN_PORT: u16 = 3478;

static TXID_SEQ: AtomicU64 = AtomicU64::new(0);

fn rand_bytes(n: usize) -> Vec<u8> {
    // 不引 rand：时间戳（纳秒）+ pid + 自增序号，xorshift 打散。txid 只要不重复。
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ ((std::process::id() as u64) << 32)
        ^ TXID_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.extend_from_slice(&seed.to_be_bytes());
    }
    out.truncate(n);
    out
}

fn new_txid() -> [u8; 12] {
    let v = rand_bytes(12);
    let mut t = [0u8; 12];
    t.copy_from_slice(&v);
    t
}

fn build_msg(kind: u16, txid: &[u8; 12], attrs: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(20 + attrs.len());
    m.extend_from_slice(&kind.to_be_bytes());
    m.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    m.extend_from_slice(&MAGIC.to_be_bytes());
    m.extend_from_slice(txid);
    m.extend_from_slice(attrs);
    m
}

fn attr(t: u16, val: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(4 + val.len());
    a.extend_from_slice(&t.to_be_bytes());
    a.extend_from_slice(&(val.len() as u16).to_be_bytes());
    a.extend_from_slice(val);
    while a.len() % 4 != 0 {
        a.push(0);
    }
    a
}

fn binding_request(txid: &[u8; 12], change: Option<u32>) -> Vec<u8> {
    let attrs = match change {
        Some(flags) => attr(ATTR_CHANGE, &flags.to_be_bytes()),
        None => Vec::new(),
    };
    build_msg(BINDING_REQ, txid, &attrs)
}

/// 回一个 Binding Success（把观察到的对端源地址回填进 XOR-MAPPED-ADDRESS）。
/// 对端因此在它那一侧也能拿到「被穿透成功」的证据 —— ICE 的双向检查就是这个道理。
fn binding_success(txid: &[u8; 12], observed: SocketAddr) -> Vec<u8> {
    let mut val = Vec::with_capacity(8);
    val.push(0);
    val.push(if observed.is_ipv4() { 0x01 } else { 0x02 });
    let port = observed.port() ^ ((MAGIC >> 16) as u16);
    val.extend_from_slice(&port.to_be_bytes());
    match observed.ip() {
        std::net::IpAddr::V4(v4) => {
            let m = MAGIC.to_be_bytes();
            for (i, b) in v4.octets().iter().enumerate() {
                val.push(b ^ m[i]);
            }
        }
        std::net::IpAddr::V6(v6) => {
            let mut mask = Vec::from(MAGIC.to_be_bytes());
            mask.extend_from_slice(txid);
            for (i, b) in v6.octets().iter().enumerate() {
                val.push(b ^ mask[i]);
            }
        }
    }
    build_msg(BINDING_OK, txid, &attr(ATTR_XOR_MAPPED, &val))
}

fn attrs_of(msg: &[u8]) -> Result<Vec<(u16, Vec<u8>)>> {
    if msg.len() < 20 {
        bail!("STUN 报文太短");
    }
    let len = u16::from_be_bytes([msg[2], msg[3]]) as usize;
    let end = (20 + len).min(msg.len());
    let mut out = Vec::new();
    let mut i = 20;
    while i + 4 <= end {
        let t = u16::from_be_bytes([msg[i], msg[i + 1]]);
        let l = u16::from_be_bytes([msg[i + 2], msg[i + 3]]) as usize;
        if i + 4 + l > end {
            break;
        }
        out.push((t, msg[i + 4..i + 4 + l].to_vec()));
        i += 4 + l + ((4 - (l % 4)) % 4);
    }
    Ok(out)
}

fn decode_xor_addr(v: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ ((MAGIC >> 16) as u16);
    match v[1] {
        0x01 => {
            let m = MAGIC.to_be_bytes();
            let ip = std::net::Ipv4Addr::new(v[4] ^ m[0], v[5] ^ m[1], v[6] ^ m[2], v[7] ^ m[3]);
            Some(SocketAddr::new(ip.into(), port))
        }
        0x02 if v.len() >= 20 => {
            let mut mask = Vec::from(MAGIC.to_be_bytes());
            mask.extend_from_slice(txid);
            let mut o = [0u8; 16];
            for i in 0..16 {
                o[i] = v[4 + i] ^ mask[i];
            }
            Some(SocketAddr::new(std::net::Ipv6Addr::from(o).into(), port))
        }
        _ => None,
    }
}

fn decode_addr(v: &[u8]) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]);
    match v[1] {
        0x01 => Some(SocketAddr::new(
            std::net::Ipv4Addr::new(v[4], v[5], v[6], v[7]).into(),
            port,
        )),
        0x02 if v.len() >= 20 => {
            let mut o = [0u8; 16];
            o.copy_from_slice(&v[4..20]);
            Some(SocketAddr::new(std::net::Ipv6Addr::from(o).into(), port))
        }
        _ => None,
    }
}

/// 解析 `stun:host:port` / `host` / `turn:host:port?transport=udp` 为 socket 地址。
fn resolve_server(s: &str) -> Result<SocketAddr> {
    let raw = s.trim();
    let raw = raw
        .strip_prefix("stun:")
        .or_else(|| raw.strip_prefix("turn:"))
        .or_else(|| raw.strip_prefix("stuns:"))
        .or_else(|| raw.strip_prefix("turns:"))
        .unwrap_or(raw);
    let raw = raw.split('?').next().unwrap_or(raw);
    let with_port = if raw.matches(':').count() == 0 {
        format!("{raw}:{DEFAULT_STUN_PORT}")
    } else {
        raw.to_string()
    };
    with_port
        .to_socket_addrs()
        .with_context(|| format!("STUN 地址解析失败：{s}"))?
        .next()
        .ok_or_else(|| anyhow!("STUN 地址解析为空：{s}"))
}

/// 收一个包的短封装：读超时 = 我们自己的超时（不是错误）。
fn recv_from(sock: &UdpSocket) -> Option<(Vec<u8>, SocketAddr)> {
    let mut buf = [0u8; 2048];
    match sock.recv_from(&mut buf) {
        Ok((n, from)) => Some((buf[..n].to_vec(), from)),
        Err(e)
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) =>
        {
            None
        }
        Err(_) => None,
    }
}

/// 一次 STUN 往返：发 Binding 请求，等**事务 ID 匹配**的响应（来源不限 —— 改 IP/端口的测试
/// 正是要收到来自另一个地址的响应）。
struct StunReply {
    from: SocketAddr,
    xor_mapped: Option<SocketAddr>,
    other: Option<SocketAddr>,
    rtt: Duration,
}

fn stun_round_trip(
    sock: &UdpSocket,
    server: SocketAddr,
    change: Option<u32>,
    timeout: Duration,
) -> Option<StunReply> {
    let txid = new_txid();
    let req = binding_request(&txid, change);
    let t0 = Instant::now();
    if sock.send_to(&req, server).is_err() {
        return None;
    }
    let deadline = t0 + timeout;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        let _ = sock.set_read_timeout(Some(left.max(Duration::from_millis(50))));
        let (msg, from) = recv_from(sock)?;
        if msg.len() < 20 || msg[8..20] != txid {
            continue; // 不是这个事务的响应（可能是对端打洞的请求），忽略
        }
        let mut xor_mapped = None;
        let mut other = None;
        for (t, v) in attrs_of(&msg).unwrap_or_default() {
            match t {
                ATTR_XOR_MAPPED => xor_mapped = decode_xor_addr(&v, &txid),
                // 有些服务器只回 MAPPED-ADDRESS（非 XOR）——作为兜底。
                ATTR_MAPPED if xor_mapped.is_none() => xor_mapped = decode_addr(&v),
                ATTR_OTHER => other = decode_addr(&v),
                _ => {}
            }
        }
        return Some(StunReply { from, xor_mapped, other, rtt: t0.elapsed() });
    }
    None
}

/* ============================== ① probe ============================== */

const FALLBACK_STUN: &[&str] = &[
    "stun:stun.miwifi.com:3478",
    "stun:stun.qq.com:3478",
    "stun:stun.l.google.com:19302",
];

/// 本机出口 IP（不真的发包，让内核选路）。
fn egress_ip() -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

fn addr_kind(ip: std::net::IpAddr) -> &'static str {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 100 && (64..=127).contains(&o[1]) {
                "cgnat"
            } else if o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
            {
                "private"
            } else {
                "public"
            }
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.segments()[0] & 0xfe00 == 0xfc00 {
                "private"
            } else {
                "public"
            }
        }
    }
}

/// 打洞条件预检结果（字段名与 Go spike / PRD 保持一致，便于两端比对）。
pub struct NatProfile {
    pub local_ip: String,
    pub local_kind: String,
    pub local_port: u16,
    pub samples: Vec<Value>,
    pub reached: usize,
    pub tried: usize,
    pub public_ip: String,
    pub mapping: String,
    pub mapping_method: String,
    pub filtering: String,
    pub filtering_method: String,
    pub rfc5780: bool,
    pub verdict: String,
    pub advice: String,
}

fn probe_profile(stun_list: &[String]) -> NatProfile {
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            return NatProfile {
                local_ip: String::new(),
                local_kind: "unknown".into(),
                local_port: 0,
                samples: vec![],
                reached: 0,
                tried: stun_list.len(),
                public_ip: String::new(),
                mapping: "unknown".into(),
                mapping_method: "multi-stun".into(),
                filtering: "unknown".into(),
                filtering_method: "unsupported".into(),
                rfc5780: false,
                verdict: "unknown".into(),
                advice: format!("无法创建 UDP socket：{e}"),
            }
        }
    };
    let local_ip = egress_ip().map(|i| i.to_string()).unwrap_or_default();
    let local_kind = egress_ip().map(addr_kind).unwrap_or("unknown").to_string();
    let local_port = sock.local_addr().map(|a| a.port()).unwrap_or(0);

    let timeout = Duration::from_secs(3);
    let mut samples = Vec::new();
    let mut mapped_counts: Vec<(String, usize)> = Vec::new();
    let mut public_ip = String::new();
    let mut reached = 0usize;
    let mut other_of: Option<(String, SocketAddr)> = None;

    for s in stun_list {
        let server = match resolve_server(s) {
            Ok(v) => v,
            Err(e) => {
                samples.push(json!({"server": s, "err": e.to_string()}));
                continue;
            }
        };
        match stun_round_trip(&sock, server, None, timeout) {
            None => samples.push(json!({"server": s, "err": "无响应（超时）"})),
            Some(r) => {
                let mapped = r.xor_mapped.map(|m| m.to_string()).unwrap_or_default();
                if !mapped.is_empty() {
                    reached += 1;
                    if let Some(sa) = r.xor_mapped {
                        if public_ip.is_empty() {
                            public_ip = sa.ip().to_string();
                        }
                    }
                    match mapped_counts.iter_mut().find(|(m, _)| *m == mapped) {
                        Some((_, c)) => *c += 1,
                        None => mapped_counts.push((mapped.clone(), 1)),
                    }
                }
                if let (Some(o), None) = (r.other, other_of.as_ref()) {
                    other_of = Some((s.clone(), o));
                }
                samples.push(json!({
                    "server": s, "rttMs": r.rtt.as_millis() as u64,
                    "mapped": mapped,
                    "from": r.from.to_string(),
                    "otherAddress": r.other.map(|o| o.to_string()).unwrap_or_default(),
                }));
            }
        }
    }

    let direct = !public_ip.is_empty() && public_ip == local_ip && !local_ip.is_empty();
    let mut mapping = "unknown".to_string();
    let mut mapping_method = "multi-stun".to_string();
    let mut filtering = "unknown".to_string();
    let mut filtering_method = "unsupported".to_string();
    let mut rfc5780 = false;

    // RFC 5780 完整分类（需要一台带 OTHER-ADDRESS 的服务器）。
    if !direct {
        if let Some((server_str, other)) = other_of.clone() {
            if let (Ok(primary), Ok(o)) = (resolve_server(&server_str), Ok::<SocketAddr, anyhow::Error>(other)) {
                rfc5780 = true;
                mapping_method = "rfc5780".into();
                filtering_method = "rfc5780".into();
                let m1 = stun_round_trip(&sock, primary, None, timeout).and_then(|r| r.xor_mapped);
                // 换 IP、同端口
                let ip_only = SocketAddr::new(o.ip(), primary.port());
                let m2 = stun_round_trip(&sock, ip_only, None, timeout).and_then(|r| r.xor_mapped);
                mapping = if m1.is_some() && m1 == m2 {
                    "endpoint_independent".into()
                } else {
                    // 换 IP 换端口
                    let m3 = stun_round_trip(&sock, o, None, timeout).and_then(|r| r.xor_mapped);
                    if m3.is_some() && m2 == m3 {
                        "address_dependent".into()
                    } else {
                        "address_and_port_dependent".into()
                    }
                };
                // 过滤：改 IP+端口能收到回包 = 端点无关过滤
                filtering = if stun_round_trip(&sock, primary, Some(CHANGE_BOTH), timeout).is_some() {
                    "endpoint_independent".into()
                } else if stun_round_trip(&sock, primary, Some(CHANGE_PORT), timeout).is_some() {
                    "address_dependent".into()
                } else {
                    "address_and_port_dependent".into()
                };
            }
        }
    }

    // 降级：多 STUN 映射比对（同一本地端口对不同目标是否给出同一映射）。
    if mapping_method != "rfc5780" {
        mapping = if reached == 0 {
            "unknown".into()
        } else if direct {
            "endpoint_independent".into()
        } else if reached < 2 {
            "unknown".into()
        } else if mapped_counts.len() == 1 {
            "endpoint_independent".into()
        } else {
            "address_and_port_dependent".into()
        };
    }

    let (verdict, advice) = match (reached, direct, mapping.as_str()) {
        (0, _, _) => (
            "blocked",
            "所有 STUN 都没响应：UDP 出站可能被封（企业网常见）。直连不可行 —— 要么用 TURN over TCP/TLS，要么走中心搬运（ncc-registry master/worker）。".to_string(),
        ),
        (_, true, _) => (
            "direct",
            "本机就在公网上（映射等于本机 IP）：任何对端都能直连，打洞不必要。".to_string(),
        ),
        (_, _, "unknown") => (
            "unknown",
            "只探到 1 台 STUN 且它不支持 RFC 5780，判不了映射行为：多配几台不同公网 IP 的 STUN 再测。".to_string(),
        ),
        (_, _, "address_and_port_dependent") => (
            "relay_likely",
            "对称 NAT（address-and-port-dependent mapping）：只有对端是锥形且由对端发起时有机会；两端都对称则基本只能 relay。请让控制面下发客户自托管的 TURN，不要指望直连。".to_string(),
        ),
        (_, _, "address_dependent") => (
            "likely_direct",
            "地址相关映射（比锥形差、比对对称好）：通常仍能打洞，但需要双方同时发起。用 `ncc p2p check <节点>` 实测确认。".to_string(),
        ),
        (_, _, _) => (
            "likely_direct",
            format!(
                "锥形 NAT（端点无关映射{filter}）：与同为锥形的对端几乎必成；与对称 NAT 的对端要由你发起才有机会。用 `ncc p2p check <节点>` 实测确认。",
                filter = match filtering.as_str() {
                    "endpoint_independent" => "、端点无关过滤",
                    "address_dependent" => "、地址相关过滤",
                    "address_and_port_dependent" => "、地址端口相关过滤",
                    _ => "",
                }
            ),
        ),
    };

    NatProfile {
        local_ip,
        local_kind,
        local_port,
        samples,
        reached,
        tried: stun_list.len(),
        public_ip,
        mapping,
        mapping_method,
        filtering,
        filtering_method,
        rfc5780,
        verdict: verdict.to_string(),
        advice,
    }
}

/// STUN 列表：优先问控制面（它是权威配置），失败/--offline 时用内置默认。
fn stun_list(cfg: &CliConfig, args_stun: Option<&str>, offline: bool) -> Vec<String> {
    if let Some(s) = args_stun {
        return s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
    }
    if !offline {
        if let Ok(v) = api::get(cfg, "/api/p2p/ice", config::token_opt(cfg).as_deref()) {
            if let Some(arr) = v.pointer("/ice/stun").and_then(|a| a.as_array()) {
                let list: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                if !list.is_empty() {
                    return list;
                }
            }
        }
    }
    FALLBACK_STUN.iter().map(|s| s.to_string()).collect()
}

/// 预检结果的结构化形态：字段名与 Go spike / PRD 保持一致，便于两端比对。
/// CLI `--json` 与 MCP 工具 `ncc_p2p_probe` 共用同一份实现（不各写一套）。
pub fn profile_json(p: &NatProfile) -> Value {
    json!({
        "localIpv4": p.local_ip,
        "localAddrKind": p.local_kind,
        "localPort": p.local_port,
        "serversTried": p.tried,
        "serversReached": p.reached,
        "samples": p.samples,
        "publicIp": p.public_ip,
        "mappingBehavior": p.mapping,
        "mappingMethod": p.mapping_method,
        "filteringBehavior": p.filtering,
        "filteringMethod": p.filtering_method,
        "rfc5780Supported": p.rfc5780,
        "verdict": p.verdict,
        "advice": p.advice,
    })
}

/// 预检结果的人读文本（CLI 与 MCP 共用，避免两处措辞走偏）。
pub fn render_profile(p: &NatProfile) -> String {
    let mut out = String::from("\n===== 打洞条件预检（单边本地）=====\n");
    out.push_str(&format!(
        "  本机出网 IP {}（{}）· UDP 本地端口 {}\n",
        p.local_ip, p.local_kind, p.local_port
    ));
    for s in &p.samples {
        let server = s.get("server").and_then(|v| v.as_str()).unwrap_or("?");
        if let Some(err) = s.get("err").and_then(|v| v.as_str()) {
            out.push_str(&format!("  {server:<34} ✗ {err}\n"));
            continue;
        }
        let rtt = s.get("rttMs").and_then(|v| v.as_u64()).unwrap_or(0);
        let mapped = s.get("mapped").and_then(|v| v.as_str()).unwrap_or("");
        let other = s.get("otherAddress").and_then(|v| v.as_str()).unwrap_or("");
        let extra = if other.is_empty() {
            String::new()
        } else {
            format!("  other={other}")
        };
        out.push_str(&format!("  {server:<34} {rtt:>4}ms  映射 {mapped}{extra}\n"));
    }
    out.push_str(&format!(
        "  STUN 可达 {}/{} · 映射 {}（{}）· 过滤 {}（{}）\n",
        p.reached, p.tried, p.mapping, p.mapping_method, p.filtering, p.filtering_method
    ));
    out.push_str(&format!("  结论 {}\n", p.verdict));
    out.push_str(&format!("  建议：{}\n", p.advice));
    if !p.rfc5780 {
        out.push_str("  注：本次 STUN 都没返回 OTHER-ADDRESS，NAT 过滤行为判不了（RFC 5780 需要带双 IP 的 STUN）。\n");
    }
    out
}

/// 跑一次预检，返回**结构化结果 + 人读文本**：CLI 与 MCP 共用同一份实现与措辞。
pub fn probe_run(cfg: &CliConfig, stun: Option<&str>, offline: bool) -> (Value, String) {
    let list = stun_list(cfg, stun, offline);
    let p = probe_profile(&list);
    (profile_json(&p), render_profile(&p))
}

pub fn probe(cfg: &CliConfig, a: &ProbeArgs) -> Result<()> {
    let (v, txt) = probe_run(cfg, a.stun.as_deref(), a.offline);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{txt}");
    }
    Ok(())
}

pub fn ice(cfg: &CliConfig) -> Result<()> {
    let tok = config::token_opt(cfg);
    let v = api::get(cfg, "/api/p2p/ice", tok.as_deref())?;
    let ice = v.get("ice").cloned().unwrap_or(Value::Null);
    println!("{}", serde_json::to_string_pretty(&ice)?);
    let turn = ice.get("turn").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0);
    if turn == 0 {
        println!("（未配置 TURN：打洞失败时会明确报错，不会降级为 NCC 中转 —— 这是设计如此）");
    }
    Ok(())
}

/* ============================== 节点与信令 ============================== */

fn my_nodes(cfg: &CliConfig, tok: &str) -> Result<Vec<Value>> {
    let v = api::get(cfg, "/api/namespaces/living", Some(tok))?;
    Ok(v.get("nodes").and_then(|n| n.as_array()).cloned().unwrap_or_default())
}

/// 解析「我以哪个节点身份参与」：显式 --from 优先，否则要求我名下只有一个 living 节点。
fn pick_my_node(cfg: &CliConfig, tok: &str, from: Option<&str>) -> Result<String> {
    let nodes = my_nodes(cfg, tok)?;
    if let Some(f) = from {
        let hit = nodes.iter().any(|n| {
            n.get("id").and_then(|v| v.as_str()) == Some(f)
                || n.get("slug").and_then(|v| v.as_str()).map(|s| {
                    let ns = n.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
                    format!("{ns}/{s}") == f || f.ends_with(&format!("/{s}")) || s == f
                }) == Some(true)
        });
        if !hit {
            bail!("--from {f} 不是你名下的 living 节点（先 `ncc living --name <名字>` 上报）");
        }
        return Ok(f.to_string());
    }
    match nodes.len() {
        0 => bail!("你还没有 living 节点：先跑 `ncc living --name my-mac` 上报一台，再过来"),
        1 => {
            let n = &nodes[0];
            // 注意：/api/namespaces/living 的 namespace 字段已经带 @（如 "@aya"），
            // 再补一个 @ 会得到 "@@aya/…"（实测踩过）。
            let ns = n
                .get("namespace")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim_start_matches('@');
            let slug = n.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            if ns.is_empty() || slug.is_empty() {
                Ok(n.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string())
            } else {
                Ok(format!("@{ns}/{slug}"))
            }
        }
        _ => {
            let names: Vec<String> = nodes
                .iter()
                .filter_map(|n| n.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect();
            bail!("你有多个 living 节点，请用 --from 指定其中一个：{}", names.join(" / "))
        }
    }
}

/// 两端必须算出**同一个**会话名：先把节点引用解析成节点 id（服务端权威身份），
/// 再排序拼接。这样「一端写 @aya/mac、另一端写 LD-xxx」也不会错开。
fn session_for(cfg: &CliConfig, tok: &str, me_ref: &str, peer_ref: &str) -> Result<String> {
    let mut ids = vec![
        resolve_node_id(cfg, tok, me_ref).unwrap_or_else(|| me_ref.to_string()),
        resolve_node_id(cfg, tok, peer_ref).unwrap_or_else(|| peer_ref.to_string()),
    ];
    ids.sort();
    Ok(format!("p2p-{}-{}", sanitize(&ids[0]), sanitize(&ids[1])))
}

/// 在我能看到的节点里（我的 + 我连接的）按 id / slug / @ns/slug 匹配到节点 id。
fn resolve_node_id(cfg: &CliConfig, tok: &str, reference: &str) -> Option<String> {
    // 服务端是权威：它能把 `@aya/mac` 解析成 LD-…，而对方账号里根本看不到这个节点。
    // 两端都拿同一个 id 才能算出同一个 session 名（自己的写法容易错开）。
    let path = format!("/api/p2p/peer?ref={}", api::urlenc(reference));
    if let Ok(v) = api::get(cfg, &path, Some(tok)) {
        if let Some(id) = v.pointer("/peer/nodeId").and_then(|x| x.as_str()) {
            return Some(id.to_string());
        }
    }
    // 退路：从我的节点 / 我连接的节点里按写法匹配。
    let v = api::get(cfg, "/api/nodes", Some(tok)).ok()?;
    for key in ["mine", "links"] {
        let Some(arr) = v.get(key).and_then(|x| x.as_array()) else {
            continue;
        };
        for n in arr {
            let Some(id) = n.get("id").and_then(|x| x.as_str()) else {
                continue;
            };
            let slug = n.get("slug").and_then(|x| x.as_str()).unwrap_or("");
            let ns = n.get("namespace").and_then(|x| x.as_str()).unwrap_or("");
            let qualified = if ns.is_empty() { String::new() } else { format!("{ns}/{slug}") };
            let plain = format!("@{slug}");
            if reference == id
                || reference == slug
                || reference == qualified
                || reference == plain
                || reference.ends_with(&format!("/{slug}"))
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn signal_send(cfg: &CliConfig, tok: &str, session: &str, from: &str, to: &str, kind: &str, payload: &str) -> Result<Value> {
    api::post_json(
        cfg,
        "/api/p2p/signal",
        Some(tok),
        &json!({"session": session, "from": from, "to": to, "kind": kind, "payload": payload}),
    )
}

fn signal_recv(cfg: &CliConfig, tok: &str, session: &str, node: &str, wait_secs: u64) -> Result<Vec<Value>> {
    let path = format!(
        "/api/p2p/signal?session={}&node={}&waitms={}",
        api::urlenc(session),
        api::urlenc(node),
        wait_secs * 1000
    );
    let v = api::get(cfg, &path, Some(tok))?;
    Ok(v.get("signals").and_then(|s| s.as_array()).cloned().unwrap_or_default())
}

pub fn signal(cfg: &CliConfig, a: &SignalArgs) -> Result<()> {
    let tok = config::require_token(cfg)?;
    match &a.action {
        SignalAction::Send { to, from, kind, payload, session } => {
            let me = pick_my_node(cfg, &tok, from.as_deref())?;
            let sess = match session {
                Some(s) => s.clone(),
                None => session_for(cfg, &tok, &me, to)?,
            };
            signal_send(cfg, &tok, &sess, &me, to, kind, payload)?;
            println!("已投递（session={sess}，kind={kind}）→ {to}");
        }
        SignalAction::Recv { from, session, wait } => {
            let me = pick_my_node(cfg, &tok, from.as_deref())?;
            let sess = session.clone().unwrap_or_else(|| "p2p-debug".to_string());
            let msgs = signal_recv(cfg, &tok, &sess, &me, *wait)?;
            if msgs.is_empty() {
                println!("（{wait}s 内没有信令）");
            } else {
                println!("{}", serde_json::to_string_pretty(&Value::Array(msgs))?);
            }
        }
    }
    Ok(())
}

/* ============================== ③ check ============================== */

pub fn check(cfg: &CliConfig, a: &CheckArgs) -> Result<()> {
    let v = check_flow(cfg, a, a.json)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{}", render_check(&v));
    }
    Ok(())
}

/// 建连检查的**结构化**结果：CLI（`--json` 与人类可读两条路）与 MCP 工具 `ncc_p2p_check` 共用。
/// `quiet=true` 时不打印过程信息 —— MCP 的 stdout 只能出协议帧，一行日志都不能漏。
pub fn check_flow(cfg: &CliConfig, a: &CheckArgs, quiet: bool) -> Result<Value> {
    let direct: Option<SocketAddr> = match &a.addr {
        Some(s) => match s.parse::<SocketAddr>() {
            Ok(sa) => Some(sa),
            Err(_) => bail!("--addr 需要 ip:port 形式（例：--addr 1.2.3.4:5678），收到 {s}"),
        },
        None => None,
    };
    let peer_ref = a.peer.clone().unwrap_or_default();

    // 直连对打（--addr）：不碰信令、不需要节点身份，甚至不需要登录——
    // 典型场景：内网节点 `ncc registry p2p self` 报出自己的 mapped，我这边直接打过去。
    if let Some(peer_addr) = direct {
        let list = stun_list(cfg, a.stun.as_deref(), false);
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        if !quiet {
            println!("直连模式（不走信令）：对端 {peer_addr}；先探我自己的映射…");
        }
        let Some((mine, _)) = probe_mapping(&sock, &list) else {
            bail!("拿不到自己的公网映射（{} 台 STUN 三轮无响应）：先跑 `ncc p2p probe` 看本机结论", list.len());
        };
        if !quiet {
            println!("我的映射 {mine}；开始双向对打…");
        }
        let out = punch(Arc::new(sock), peer_addr, Duration::from_secs(a.wait));
        return Ok(json!({
            "mode": "direct",
            "ok": out.inbound,
            "waitSec": a.wait,
            "myMapped": mine.to_string(),
            "peerMapped": peer_addr.to_string(),
            "peerRequestsSeen": out.requests_seen,
            "peerResponsesSeen": out.replies,
            "rttMs": out.rtt_ms,
            "advice": "对端若是一个内网节点：确认它开着入口（`ncc registry p2p serve --on`）且 `p2p self` 报出的 mapped 就是 peerMapped；打洞必须双方同时发起。",
        }));
    }

    let tok = config::require_token(cfg)?;
    let me = pick_my_node(cfg, &tok, a.from.as_deref())?;
    let session = match &a.session {
        Some(s) => s.clone(),
        None => session_for(cfg, &tok, &me, &peer_ref)?,
    };
    let deadline = Duration::from_secs(a.wait);

    // 0) 先问服务端：这个引用是谁、我够不够得着（够不着就别白跑，直接给原因）。
    let mut peer_info = Value::Null;
    if let Ok(v) = api::get(cfg, &format!("/api/p2p/peer?ref={}", api::urlenc(&peer_ref)), Some(&tok)) {
        let p = v.get("peer").cloned().unwrap_or(Value::Null);
        if p.get("reachable").and_then(|x| x.as_bool()) == Some(false) {
            bail!(
                "对端节点不可达：{reason}\n  · 它是私有节点：让**所有者**显式授权给你（私有节点无法 link，这是设计如此）：\n      ncc grant set --user @你 --kind p2p        # 由对端所有者执行\n  · 它是公开节点却仍报不可达：多半是引用写错或节点已下架：`ncc nodes discover --q <关键词>` 找找它。",
                reason = p.get("reason").and_then(|x| x.as_str()).unwrap_or("未知原因")
            );
        }
        if !quiet {
            println!(
                "对端：{name}（{ns}/{slug} · {kind} · {status}）",
                name = p.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                ns = p.get("namespace").and_then(|x| x.as_str()).unwrap_or(""),
                slug = p.get("slug").and_then(|x| x.as_str()).unwrap_or(""),
                kind = p.get("kind").and_then(|x| x.as_str()).unwrap_or(""),
                status = p.get("status").and_then(|x| x.as_str()).unwrap_or(""),
            );
            if p.get("status").and_then(|x| x.as_str()) == Some("offline") {
                // 心跳过期不影响信令（节点记录还在），但值得提醒一句：
                // 打洞要两端同时跑 check。
                println!("  ⚠️  对端心跳已过期（`ncc living --daemon` 没在跑）：确认它那边真的在执行 check。");
            }
        }
        peer_info = p;
    }

    // 1) 自己的公网映射：打洞要先知道「我站在哪」。
    //    公共 STUN 会限流（实测：两端同时打时一个能通、一个全超时），所以多轮重试。
    let list = stun_list(cfg, a.stun.as_deref(), false);
    let mine_sock = UdpSocket::bind("0.0.0.0:0")?;
    let local_port = mine_sock.local_addr()?.port();
    let Some((my_mapped, stun_used)) = probe_mapping(&mine_sock, &list) else {
        bail!(
            "拿不到自己的公网映射：{} 台 STUN 三轮都没响应。\n  · 可能是 UDP 出站被封（企业网）→ 直连不可行，先跑 `ncc p2p probe` 看结论；\n  · 也可能只是公共 STUN 限流/抖动 → 换一台或稍后重试：`ncc p2p check {} --stun stun:host:3478`",
            list.len(),
            peer_ref
        );
    };
    if !quiet {
        println!("我对外的地址：{my_mapped}（STUN {stun_used}；本地端口 {local_port}）");
    }

    // 2) 信令：把我的候选告诉对端，同时等对端的候选。
    let payload = json!({
        "addr": my_mapped.to_string(),
        "localPort": local_port,
        "kind": "stun",
        "node": me,
    })
    .to_string();
    signal_send(cfg, &tok, &session, &me, &peer_ref, "candidate", &payload)?;
    if !quiet {
        println!("已把候选发给 {peer_ref}（session={session}）；等对端的候选…");
    }

    let t_wait = Instant::now();
    let mut peer_addr: Option<SocketAddr> = None;
    while t_wait.elapsed() < deadline && peer_addr.is_none() {
        for m in signal_recv(cfg, &tok, &session, &me, 3)? {
            let kind = m.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind != "candidate" {
                continue;
            }
            let pl = m.get("payload").and_then(|v| v.as_str()).unwrap_or("");
            if let Ok(v) = serde_json::from_str::<Value>(pl) {
                if let Some(addr) = v.get("addr").and_then(|x| x.as_str()) {
                    if let Ok(sa) = addr.parse::<SocketAddr>() {
                        peer_addr = Some(sa);
                        break;
                    }
                }
            }
        }
    }
    let Some(peer_addr) = peer_addr else {
        bail!(
            "等不到对端的候选（{}s）：对端没在跑 `ncc p2p check {me}`，或它的信令没到（两端都要跑，且都在 1 分钟内）\n  如果对端是内网节点、不方便走云端信令：用 `ncc p2p check --addr <它的 mapped>` 直接对打",
            a.wait
        );
    };
    if !quiet {
        println!("对端候选：{peer_addr} —— 现在开始双向对打（ICE connectivity check）…");
    }

    // 3) 双向对打：我发 Binding 请求 + 回复对端发来的 Binding 请求。
    //    收到**任何**来自对端的 UDP 包即证明「这条路径的 NAT 允许入向」，也就是打洞成功。
    let out = punch(Arc::new(mine_sock), peer_addr, deadline);

    Ok(json!({
        "mode": "signaling",
        "ok": out.inbound,
        // 旧字段名，保留以免脚本依赖断掉（与 ok 同义）。
        "inboundFromPeer": out.inbound,
        "waitSec": a.wait,
        "session": session,
        "myNode": me,
        "peerNode": peer_ref,
        "peer": peer_info,
        "myMapped": my_mapped.to_string(),
        "peerMapped": peer_addr.to_string(),
        "stunUsed": stun_used,
        "localPort": local_port,
        "peerRequestsSeen": out.requests_seen,
        "peerResponsesSeen": out.replies,
        "rttMs": out.rtt_ms,
        "advice": format!("确认对端在同一分钟内跑 `ncc p2p check {me}`；仍失败就让控制面下发客户自托管 TURN（relay 兜底）。"),
    }))
}

/// 结果的人读文本（CLI 与 MCP 共用）。
pub fn render_check(v: &Value) -> String {
    let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let mut out = String::from("\n===== 建连检查结果 =====\n");
    out.push_str(&format!(
        "  我的映射 {} ↔ 对端映射 {}\n",
        g("myMapped"),
        g("peerMapped")
    ));
    if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        out.push_str(&format!(
            "  直连可用 ✓  收到对端回包（请求 {} 条 / 响应 {} 条{}）\n",
            n("peerRequestsSeen"),
            n("peerResponsesSeen"),
            v.get("rttMs")
                .and_then(|x| x.as_i64())
                .filter(|r| *r > 0)
                .map(|r| format!("，首个往返 {r}ms"))
                .unwrap_or_default(),
        ));
        out.push_str("  这条路径的 NAT 允许入向：可以在此之上建数据通道（DTLS/SCTP）。\n");
    } else {
        out.push_str(&format!(
            "  打洞失败 ✗  {}s 内没收到对端任何 UDP 回包\n",
            n("waitSec")
        ));
        out.push_str("  常见原因：①对端没在同时发起（打洞必须双方同时，或对端开着 serve 入口）；②双方 NAT 不兼容（尤其对称 NAT）；\n");
        out.push_str("           ③企业网禁 UDP；④对端进程已退出。\n");
        out.push_str(&format!("  下一步：{}\n", g("advice")));
    }
    out
}

/// 拿自己的公网映射（返回映射与是哪台 STUN 给的）。
/// 公共 STUN 会限流（实测：两端同时打时一个能通、一个全超时），所以三轮重试。
fn probe_mapping(sock: &UdpSocket, list: &[String]) -> Option<(SocketAddr, String)> {
    for attempt in 0..3 {
        for s in list {
            if let Ok(server) = resolve_server(s) {
                if let Some(r) = stun_round_trip(sock, server, None, Duration::from_secs(3)) {
                    if let Some(m) = r.xor_mapped {
                        return Some((m, s.clone()));
                    }
                }
            }
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(600));
        }
    }
    None
}

struct PunchOutcome {
    inbound: bool,
    requests_seen: u32,
    replies: u32,
    rtt_ms: Option<u128>,
}

/// punch 双向对打：一边定时发 Binding 请求，一边回答对端的请求。
/// 收到对端任何 UDP 包 = 这条路径的 NAT 允许入向（即 ICE connectivity check 成功）。
fn punch(sock: Arc<UdpSocket>, peer_addr: SocketAddr, wait: Duration) -> PunchOutcome {
    let stop = Arc::new(AtomicBool::new(false));
    let sender = {
        let sock = sock.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let txid = new_txid();
                let _ = sock.send_to(&binding_request(&txid, None), peer_addr);
                std::thread::sleep(Duration::from_millis(300));
            }
        })
    };
    let t0 = Instant::now();
    let mut out = PunchOutcome { inbound: false, requests_seen: 0, replies: 0, rtt_ms: None };
    let mut last_sent = Instant::now();
    while t0.elapsed() < wait {
        let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
        if let Some((msg, from)) = recv_from(&sock) {
            if from == peer_addr && msg.len() >= 20 {
                out.inbound = true;
                match u16::from_be_bytes([msg[0], msg[1]]) {
                    BINDING_REQ => {
                        out.requests_seen += 1;
                        let mut txid = [0u8; 12];
                        txid.copy_from_slice(&msg[8..20]);
                        let _ = sock.send_to(&binding_success(&txid, from), from);
                    }
                    BINDING_OK => {
                        out.replies += 1;
                        if out.rtt_ms.is_none() {
                            out.rtt_ms = Some(last_sent.elapsed().as_millis());
                        }
                    }
                    _ => {}
                }
            }
        }
        if last_sent.elapsed() >= Duration::from_millis(300) {
            let txid = new_txid();
            let _ = sock.send_to(&binding_request(&txid, None), peer_addr);
            last_sent = Instant::now();
        }
        if out.inbound && out.replies > 0 {
            break; // 已拿到双向证据，不必等满
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = sender.join();
    out
}

/* ============================== 票据 ============================== */

pub fn ticket(cfg: &CliConfig, a: &TicketArgs) -> Result<()> {
    let tok = config::require_token(cfg)?;
    match &a.action {
        TicketAction::Create { peer, reference, direction, max_bytes, expires_in, note } => {
            let v = api::post_json(
                cfg,
                "/api/p2p/tickets",
                Some(&tok),
                &json!({
                    "peer": peer, "ref": reference, "direction": direction,
                    "maxBytes": max_bytes, "expiresIn": expires_in, "note": note,
                }),
            )?;
            let ticket = v.get("ticket").and_then(|x| x.as_str()).unwrap_or("");
            println!("票据（只显示这一次，请发给对端节点）：\n  {ticket}");
            if let Some(info) = v.get("ticketInfo") {
                println!(
                    "  ref={} direction={} 到期 {}",
                    info.get("ref").and_then(|x| x.as_str()).unwrap_or(""),
                    info.get("direction").and_then(|x| x.as_str()).unwrap_or(""),
                    info.get("expiresAt").and_then(|x| x.as_str()).unwrap_or(""),
                );
            }
            println!("  语义：连接 ≠ 授权 —— 对端拿到票据才能取这条资源；撤销即刻失效。");
        }
        TicketAction::List => {
            let v = api::get(cfg, "/api/p2p/tickets", Some(&tok))?;
            let rows = v.get("tickets").and_then(|t| t.as_array()).cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("（还没有发出过票据）");
            }
            for t in rows {
                println!(
                    "  {}  {}  → {}  {}  {}",
                    t.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                    t.get("direction").and_then(|x| x.as_str()).unwrap_or(""),
                    t.get("peerNodeId").and_then(|x| x.as_str()).unwrap_or(""),
                    t.get("ref").and_then(|x| x.as_str()).unwrap_or(""),
                    if t.get("usable").and_then(|x| x.as_bool()).unwrap_or(false) {
                        "可用".to_string()
                    } else {
                        format!("不可用（{}）", t.get("reason").and_then(|x| x.as_str()).unwrap_or(""))
                    }
                );
            }
        }
        TicketAction::Verify { ticket, bytes, peer } => {
            let v = api::post_json(
                cfg,
                "/api/p2p/tickets/verify",
                Some(&tok),
                &json!({"ticket": ticket, "bytes": bytes, "peerNode": peer}),
            )?;
            println!(
                "票据有效 ✓  ref={} direction={} 剩余 {} 字节",
                v.get("ref").and_then(|x| x.as_str()).unwrap_or(""),
                v.get("direction").and_then(|x| x.as_str()).unwrap_or(""),
                v.pointer("/ticketInfo/remainingBytes").and_then(|x| x.as_i64()).unwrap_or(0),
            );
        }
        TicketAction::Revoke { id } => {
            api::del(cfg, &format!("/api/p2p/tickets/{}", api::urlenc(id)), Some(&tok))?;
            println!("已撤销 {id}（立即失效）");
        }
    }
    Ok(())
}
