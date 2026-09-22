// 轻量 HTTP 客户端（ureq），支持 JSON 与 raw 上传。
use crate::config::CliConfig;
use anyhow::{bail, Context};
use serde_json::Value;

fn agent() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60));
    b = b.redirects(10);
    b.build()
}

fn err_of(status: u16, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
            let code = v.pointer("/error/code").and_then(|m| m.as_str()).unwrap_or("request_failed");
            return format!("[{code}] {msg}");
        }
    }
    format!("HTTP {status}: {}", truncate(body, 200))
}

fn truncate(s: &str, n: usize) -> String {
    let mut chars = s.chars();
    let mut out = String::new();
    for _ in 0..n {
        match chars.next() {
            Some(c) => out.push(c),
            None => break,
        }
    }
    out
}

/// 发请求。json_body 与 raw 二选一。
pub fn request(
    cfg: &CliConfig,
    method: &str,
    path: &str,
    token: Option<&str>,
    json_body: Option<&Value>,
    raw: Option<&[u8]>,
    extra_headers: &[(&str, &str)],
) -> anyhow::Result<Value> {
    let url = format!("{}{}", cfg.base_url().trim_end_matches('/'), path);
    let mut req = agent().request(method, &url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    for (k, v) in extra_headers {
        req = req.set(k, v);
    }

    let resp = if let Some(j) = json_body {
        req.set("Content-Type", "application/json")
            .send_string(&j.to_string())
    } else if let Some(b) = raw {
        req.send_bytes(b)
    } else {
        req.call()
    };

    match resp {
        Ok(r) => {
            let status = r.status();
            let body = r.into_string().unwrap_or_default();
            if status >= 400 {
                bail!("{}", err_of(status, &body));
            }
            if body.trim().is_empty() {
                Ok(Value::Null)
            } else {
                serde_json::from_str(&body)
                    .with_context(|| format!("响应解析失败: {}", truncate(&body, 200)))
            }
        }
        Err(ureq::Error::Status(status, r)) => {
            let body = r.into_string().unwrap_or_default();
            bail!("{}", err_of(status, &body))
        }
        Err(e) => bail!("网络错误: {e}"),
    }
}

pub fn get(cfg: &CliConfig, path: &str, token: Option<&str>) -> anyhow::Result<Value> {
    request(cfg, "GET", path, token, None, None, &[])
}

/// 查询参数转义（中文区域名必须转义，否则服务端解析 URL 失败）。
pub fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
pub fn post_json(cfg: &CliConfig, path: &str, token: Option<&str>, body: &Value) -> anyhow::Result<Value> {
    request(cfg, "POST", path, token, Some(body), None, &[])
}
pub fn del(cfg: &CliConfig, path: &str, token: Option<&str>) -> anyhow::Result<Value> {
    request(cfg, "DELETE", path, token, None, None, &[])
}
