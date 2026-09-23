// 节点侧的 P2P：判断「这台 registry 机器打不打得到」、以及可选地让别的节点打进来。
//
// 与 `ncc p2p`（云端控制面视角）的分工：
//
//	ncc p2p probe / check        在**我这台笔记本**上做判断（走云端信令找人）
//	ncc registry p2p self/check  在**目标节点那台机器**上做判断（NAT 出口常常不同）
//	ncc registry p2p serve       让本节点开一个可被打洞的 UDP 入口（只应答 STUN）
use crate::api;
use crate::config::{self, CliConfig};
use anyhow::Result;
use serde_json::{json, Value};

#[derive(clap::Args)]
pub struct P2pArgs {
    #[command(subcommand)]
    pub action: P2pAction,
}

#[derive(clap::Subcommand)]
pub enum P2pAction {
    /// 本节点的 NAT 画像与结论（在节点那台机器上探，不是在我这台）
    Self_,
    /// 从节点侧与一个对端映射地址做真实对打（两端要能互发 UDP）
    Check {
        /// 对端映射地址 ip:port（对端 `p2p self` 或 `p2p serve` 里报出的 mapped）
        #[arg(long)]
        peer: String,
        /// 等待秒数（默认 10，最长 60）
        #[arg(long, default_value_t = 10)]
        wait: u32,
    },
    /// 看/开关「可被打洞入口」（只应答 STUN Binding，不接收业务字节）
    Serve {
        /// 打开；不给 = 只看状态
        #[arg(long)]
        on: bool,
        /// 关闭
        #[arg(long)]
        off: bool,
        /// 反向打洞对端映射地址 ip:port（对端 `p2p self`/`serve` 报出的 mapped），可多个逗号分隔
        #[arg(long)]
        peer: Option<String>,
    },
}

pub fn p2p(cfg: &CliConfig, a: &P2pArgs) -> Result<()> {
    let tok = config::require_token(cfg)?;
    match &a.action {
        P2pAction::Self_ => self_profile(cfg, &tok),
        P2pAction::Check { peer, wait } => check(cfg, &tok, peer, *wait),
        P2pAction::Serve { on, off, peer } => serve(cfg, &tok, *on, *off, peer.as_deref()),
    }
}

fn ice_line(v: &Value) -> String {
    let stun = v
        .pointer("/ice/stun")
        .and_then(|s| s.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let turn = v
        .pointer("/ice/turn")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if turn == 0 {
        format!("STUN {stun}（未配 TURN：打洞失败会明确报错，不降级为中心中转）")
    } else {
        format!("STUN {stun} · 已配 {turn} 台自托管 TURN")
    }
}

/// 目标节点那台机器的打洞画像 + 入口状态（MCP 工具 `ncc_p2p_node` 与 CLI 共用同一份）。
pub fn self_json(cfg: &CliConfig, tok: &str) -> Result<Value> {
    api::get(cfg, "/api/p2p/self", Some(tok))
}

fn self_profile(cfg: &CliConfig, tok: &str) -> Result<()> {
    let v = self_json(cfg, tok)?;
    let p = v.get("profile").cloned().unwrap_or(Value::Null);
    let node = v.get("node").cloned().unwrap_or(Value::Null);
    println!(
        "节点 {}（{name} · {role}）",
        node.get("id").and_then(|x| x.as_str()).unwrap_or(""),
        name = node.get("name").and_then(|x| x.as_str()).unwrap_or(""),
        role = node.get("role").and_then(|x| x.as_str()).unwrap_or(""),
    );
    println!(
        "  本机出网 IP {}（{}）· UDP 本地端口 {}",
        p.get("localIpv4").and_then(|x| x.as_str()).unwrap_or("-"),
        p.get("localAddrKind").and_then(|x| x.as_str()).unwrap_or("-"),
        p.get("localPort").and_then(|x| x.as_u64()).unwrap_or(0),
    );
    println!(
        "  公网映射 {}",
        p.get("mapped").and_then(|x| x.as_str()).unwrap_or("（没探到）")
    );
    println!(
        "  STUN 可达 {}/{} · 映射 {}（{}）· 过滤 {}（{}）",
        p.get("serversReached").and_then(|x| x.as_u64()).unwrap_or(0),
        p.get("serversTried").and_then(|x| x.as_u64()).unwrap_or(0),
        p.get("mappingBehavior").and_then(|x| x.as_str()).unwrap_or("-"),
        p.get("mappingMethod").and_then(|x| x.as_str()).unwrap_or("-"),
        p.get("filteringBehavior").and_then(|x| x.as_str()).unwrap_or("-"),
        p.get("filteringMethod").and_then(|x| x.as_str()).unwrap_or("-"),
    );
    println!(
        "  结论 {}",
        p.get("verdict").and_then(|x| x.as_str()).unwrap_or("-")
    );
    println!(
        "  建议：{}",
        p.get("advice").and_then(|x| x.as_str()).unwrap_or("")
    );
    println!("  ICE：{}", ice_line(&v));
    // 可被打洞入口的状态：要让对端打进来，这里必须是 on。
    let serve = v.get("serve").cloned().unwrap_or(Value::Null);
    if serve.get("on").and_then(|x| x.as_bool()) == Some(true) {
        println!(
            "  可被打洞入口：已开（对端发往 {} · 已应答 {} 次 · 对端回包 {} 次）",
            serve.get("mapped").and_then(|x| x.as_str()).unwrap_or("-"),
            serve.get("requestsTaken").and_then(|x| x.as_u64()).unwrap_or(0),
            serve.get("responsesSeen").and_then(|x| x.as_u64()).unwrap_or(0),
        );
    } else {
        println!("  可被打洞入口：关（别人打不进来；要开：ncc registry p2p serve --on --peer <对端 mapped>）");
    }
    Ok(())
}

fn check(cfg: &CliConfig, tok: &str, peer: &str, wait: u32) -> Result<()> {
    let v = api::post_json(
        cfg,
        "/api/p2p/check",
        Some(tok),
        &json!({"peer": peer, "waitSec": wait}),
    )?;
    let r = v.get("result").cloned().unwrap_or(Value::Null);
    println!(
        "  我的映射 {} ↔ 对端 {}",
        r.get("myMapped").and_then(|x| x.as_str()).unwrap_or("-"),
        r.get("peerMapped").and_then(|x| x.as_str()).unwrap_or(peer),
    );
    if r.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        println!(
            "  直连可用 ✓（对端请求 {} 条 / 响应 {} 条{}）",
            r.get("peerRequestsSeen").and_then(|x| x.as_u64()).unwrap_or(0),
            r.get("peerResponsesSeen").and_then(|x| x.as_u64()).unwrap_or(0),
            r.get("rttMs")
                .and_then(|x| x.as_i64())
                .filter(|n| *n > 0)
                .map(|n| format!("，首个往返 {n}ms"))
                .unwrap_or_default(),
        );
    } else {
        println!(
            "  打洞失败 ✗ {}",
            r.get("reason").and_then(|x| x.as_str()).unwrap_or("对端无回包")
        );
        println!(
            "  {}",
            r.get("advice").and_then(|x| x.as_str()).unwrap_or("")
        );
    }
    Ok(())
}

fn serve(cfg: &CliConfig, tok: &str, on: bool, off: bool, peer: Option<&str>) -> Result<()> {
    // 只给 --peer 也当成「开启」：指定对端却又把入口关掉是没意义的。
    let payload_on = if off { false } else { on || peer.is_some() };
    let mut body = json!({"on": payload_on});
    if let Some(p) = peer {
        body["peer"] = json!(p);
    }
    let v = if on || off || peer.is_some() {
        api::post_json(cfg, "/api/p2p/serve", Some(tok), &body)?
    } else {
        api::get(cfg, "/api/p2p/serve", Some(tok))?
    };
    let s = v.get("serve").cloned().unwrap_or(Value::Null);
    if s.get("on").and_then(|x| x.as_bool()) == Some(true) {
        println!(
            "可被打洞入口：已开\n  监听 {}（对端应发往 {}）\n  已应答 {} 次 · 收到对端回包 {} 次",
            s.get("listen").and_then(|x| x.as_str()).unwrap_or("-"),
            s.get("mapped").and_then(|x| x.as_str()).unwrap_or("（没探到映射）"),
            s.get("requestsTaken").and_then(|x| x.as_u64()).unwrap_or(0),
            s.get("responsesSeen").and_then(|x| x.as_u64()).unwrap_or(0),
        );
        let peers = s
            .get("peers")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        if peers.is_empty() {
            println!("  反向打洞对端：无（用 --peer <对端 mapped> 指定，否则对方可能打不进）");
        } else {
            println!("  反向打洞对端：{peers}");
        }
        println!("  只应答 STUN Binding 请求；不接收业务字节（红线：NCC 不中转业务数据）");
        if let Some(n) = s.get("note").and_then(|x| x.as_str()) {
            println!("  提示：{n}");
        }
    } else {
        println!(
            "可被打洞入口：关\n  {}",
            s.get("hint").and_then(|x| x.as_str()).unwrap_or("")
        );
    }
    Ok(())
}
