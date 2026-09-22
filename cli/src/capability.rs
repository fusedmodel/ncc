// 能力（capability）：CLI 与「目标节点」之间的契约。
//
// 每个节点都会在 `GET /api/meta` 自述它是谁、**声明支持哪些能力**：
//
//	ncc-platform（kind=hub）  registry · nodes · grants · living · services · profile · share · billing · admin
//	ncc-registry（kind=node） registry · config · share · nodes · grants · access · cluster · admin
//
// 命令面按**能力**放行，而不是按「云端/本地」硬编码：
// 将来本地节点也声明 services / profile 时，同一个 `ncc services match` 在那台节点上直接可用。
//
// 两条判定原则：
//  1. **声明了就放行**（不查是不是"该在那边"）；
//  2. **探测不到就放行**（老版本服务端没有 /api/meta，不能因此把功能锁死）；
//     只有「明确声明了这份清单、且清单里没有这个能力」才拒绝，并给出下一步。
use crate::api;
use crate::config::{self, CliConfig};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 一个节点自述的能力清单（探测结果）。
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub product: String,
    pub kind: String, // hub | node
    pub version: String,
    pub about: String,
    /// 服务端**明确声明**的能力；老服务端为空 = 未知（放行）。
    pub capabilities: Vec<String>,
    /// 探测失败的原因（离线 / 老服务端），仅用于展示。
    pub note: Option<String>,
}

impl Meta {
    /// 能力是否可用：`None` = 未知（老服务端没声明，放行）。
    pub fn has(&self, cap: &str) -> Option<bool> {
        if self.capabilities.is_empty() {
            return None;
        }
        Some(self.capabilities.iter().any(|c| c == cap))
    }

    pub fn is_unknown(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// 能力清单（展示用；未知时给一句说明）。
    pub fn capability_line(&self) -> String {
        if self.is_unknown() {
            "（未声明，按不限制处理）".to_string()
        } else {
            self.capabilities.join(" · ")
        }
    }

    /// 人读的 kind 标签。
    pub fn kind_label(&self) -> &str {
        match self.kind.as_str() {
            "hub" => "云端公共目录",
            "node" => "内网节点",
            "" => "未知",
            other => other,
        }
    }
}

// 进程内缓存：同一次命令里多处探测只发一次请求（CLI 是短命进程，不做落盘缓存）。
fn cache() -> &'static Mutex<HashMap<String, Meta>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Meta>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 探测某个 base 的自述（带进程内缓存）。
pub fn probe_base(base: &str) -> Meta {
    if let Some(m) = cache().lock().unwrap().get(base) {
        return m.clone();
    }
    let m = fetch(base);
    cache().lock().unwrap().insert(base.to_string(), m.clone());
    m
}

/// 探测当前目标。
pub fn probe(cfg: &CliConfig) -> Meta {
    probe_base(&cfg.base_url())
}

fn fetch(base: &str) -> Meta {
    // 走 /api/meta（新旧服务端都有：registry 一直有，platform 2026-09-23 起有）。
    let mut meta = Meta::default();
    match get_json(base, "/api/meta") {
        Ok(v) => {
            meta.product = s(&v, "product");
            meta.kind = s(&v, "kind");
            meta.version = s(&v, "version");
            meta.about = s(&v, "about");
            meta.capabilities = v
                .get("capabilities")
                .and_then(|c| c.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
        }
        Err(e) => {
            meta.note = Some(e);
        }
    }
    // 退一步：/api/health 至少能告诉我们这是谁（老服务端 / 只读代理）。
    if meta.product.is_empty() {
        if let Ok(v) = get_json(base, "/api/health") {
            let service = s(&v, "service");
            meta.product = if service.contains("ncc-registry") {
                "ncc-registry".to_string()
            } else if service.is_empty() {
                String::new()
            } else {
                "ncc-platform".to_string()
            };
            meta.kind = if meta.product == "ncc-registry" { "node".into() } else { "hub".into() };
            if meta.note.is_none() {
                meta.note = Some("服务端未提供能力自述（老版本），按不限制处理".to_string());
            }
        }
    }
    meta
}

/// 把探测结果记进当前目标（下次离线也能提示）。
pub fn remember(cfg: &mut CliConfig, meta: &Meta) {
    config::remember_meta(cfg, &meta.product, &meta.kind, &meta.capabilities);
}

/// 放行判定：目标声明了该能力 → OK；未声明（老服务端）→ OK；声明了但没有 → 报错并给下一步。
pub fn ensure(cfg: &CliConfig, cap: &str) -> Result<Meta> {
    let meta = probe(cfg);
    if meta.has(cap) == Some(false) {
        let name = cfg.current_name();
        let list = if meta.capabilities.is_empty() {
            "（未声明）".to_string()
        } else {
            meta.capabilities.join(" · ")
        };
        let hint = hint_for(cfg, cap);
        anyhow::bail!(
            "目标 {}（{} · {}）没有声明 `{}` 能力\n  它声明的能力：{}\n{}",
            name,
            if meta.product.is_empty() { "未知" } else { &meta.product },
            meta.kind_label(),
            cap,
            list,
            hint
        );
    }
    Ok(meta)
}

/// 能力缺失时给出「该去哪儿做这件事」的提示。
fn hint_for(cfg: &CliConfig, cap: &str) -> String {
    let nodes = cfg.node_names();
    let current = cfg.current_name();
    let others: Vec<String> = cfg
        .names()
        .into_iter()
        .filter(|n| n != &current)
        .collect();
    match cap {
        "config" | "access" | "cluster" | "admin" => format!(
            "  `{}` 目前由内网 registry 节点提供。{}",
            cap,
            if nodes.is_empty() {
                "还没有节点目标：ncc target add office --base http://<内网 IP>:8282".to_string()
            } else {
                format!("切过去：ncc target use {}（ncc target list 看全部）", nodes.join(" / "))
            }
        ),
        "services" | "profile" | "billing" => {
            let others: Vec<String> = others.iter().filter(|n| n.as_str() != "hub").cloned().collect();
            format!(
                "  `{}` 目前由云端（ncc.ai）提供。切过去：ncc target use hub{}",
                cap,
                if others.is_empty() {
                    String::new()
                } else {
                    format!("（云端现在叫 hub；也可用 {}）", others.join(" / "))
                }
            )
        }
        _ => {
            if others.is_empty() {
                "  当前没有别的目标可切（ncc target list / ncc target add）".to_string()
            } else {
                format!("  可切的目标：{}（ncc target use <名字>）", others.join(" / "))
            }
        }
    }
}

/* ---------------- 小工具 ---------------- */

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// 不带 Authorization、短超时的 GET（探测不能拖慢命令）。
fn get_json(base: &str, path: &str) -> std::result::Result<Value, String> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .redirects(3)
        .build();
    match agent.get(&url).set("Accept", "application/json").call() {
        Ok(r) => {
            let body = r.into_string().unwrap_or_default();
            serde_json::from_str(&body).map_err(|e| format!("{url} 响应解析失败: {e}"))
        }
        Err(ureq::Error::Status(code, _)) => Err(format!("{url} → HTTP {code}")),
        Err(e) => Err(format!("{url} 不可达: {e}")),
    }
}

/// `ncc api` 里那套 request 的简化版：这里只发 GET。
pub fn _unused_keep_api_import() -> Result<Value> {
    let _ = api::urlenc("x");
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_of(caps: &[&str]) -> Meta {
        Meta {
            capabilities: caps.iter().map(|c| c.to_string()).collect(),
            ..Meta::default()
        }
    }

    #[test]
    fn declared_capabilities_are_enforced() {
        let m = meta_of(&["registry", "config"]);
        assert_eq!(m.has("config"), Some(true));
        assert_eq!(m.has("services"), Some(false));
    }

    #[test]
    fn unknown_capabilities_fail_open() {
        // 老服务端不声明 capabilities → 不拦（否则会把功能莫名锁死）
        let m = meta_of(&[]);
        assert!(m.is_unknown());
        assert_eq!(m.has("config"), None);
    }
}
