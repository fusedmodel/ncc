// 轻量 HTTP 客户端（ureq），支持 JSON 与 raw 上传。
use crate::config::CliConfig;
use anyhow::{bail, Context};
use serde_json::Value;
use std::collections::BTreeMap;

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
        // 对外访问令牌自动附持有证明：令牌可能绑定了凭据（cnf.jkt），
        // 那时服务端要求出示私钥签的东西 —— 客户端不配合就是 401。
        if cfg.target().auth_token.as_deref() == Some(t) {
            if let Some(proof) = crate::auth::proof_for(method, path, t) {
                req = req.set("NCC-Proof", &proof);
            }
        }
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
/// 取原始字节。`path_or_url` 既可以是相对本目标的路径，也可以是**绝对地址**
/// （storage 直链 —— 下载产物与签名文件时用得上，那些地址不属于 API）。
pub fn get_bytes(cfg: &CliConfig, path_or_url: &str, token: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let url = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        path_or_url.to_string()
    } else {
        format!("{}{}", cfg.base_url().trim_end_matches('/'), path_or_url)
    };
    let mut req = agent().request("GET", &url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    match req.call() {
        Ok(r) => {
            let mut buf = Vec::new();
            use std::io::Read;
            r.into_reader().read_to_end(&mut buf)?;
            Ok(buf)
        }
        Err(ureq::Error::Status(status, r)) => {
            let body = r.into_string().unwrap_or_default();
            bail!("{}", err_of(status, &body))
        }
        Err(e) => bail!("网络错误: {e}"),
    }
}

pub fn post_json(cfg: &CliConfig, path: &str, token: Option<&str>, body: &Value) -> anyhow::Result<Value> {
    request(cfg, "POST", path, token, Some(body), None, &[])
}

/// 表单 POST（`application/x-www-form-urlencoded`）。
///
/// OAuth / OIDC 的端点（token、device_authorization、introspect、revoke）都只吃表单，
/// 不吃 JSON —— 这是协议规定的，不是偏好。额外头用来带 `NCC-Proof`（持有证明）。
pub fn post_form(
    cfg: &CliConfig,
    path: &str,
    token: Option<&str>,
    form: &[(&str, String)],
    extra_headers: &[(&str, &str)],
) -> anyhow::Result<Value> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", urlenc(k), urlenc(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/x-www-form-urlencoded")];
    headers.extend_from_slice(extra_headers);
    request(cfg, "POST", path, token, None, Some(body.as_bytes()), &headers)
}

/// PUT：**覆盖**语义的写入（如「一人一条」的评价 —— 重复提交是改分，不是新增）。
pub fn put_json(cfg: &CliConfig, path: &str, token: Option<&str>, body: &Value) -> anyhow::Result<Value> {
    request(cfg, "PUT", path, token, Some(body), None, &[])
}

/// 取原始字节**并带上响应头**。
///
/// 导出数据集要它：数据集摘要（`X-NCC-Dataset-Digest`）与条数在响应头里 ——
/// 本地重新序列化一遍得到的字节与服务端的不一样，摘要是算不出来的，
/// 所以**原样落盘 + 原样读头**才是诚实的做法。
pub fn get_bytes_with_headers(
    cfg: &CliConfig,
    path_or_url: &str,
    token: Option<&str>,
) -> anyhow::Result<(Vec<u8>, BTreeMap<String, String>)> {
    let url = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        path_or_url.to_string()
    } else {
        format!("{}{}", cfg.base_url().trim_end_matches('/'), path_or_url)
    };
    let mut req = agent().request("GET", &url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    match req.call() {
        Ok(r) => {
            let mut headers = BTreeMap::new();
            for name in r.headers_names() {
                if let Some(v) = r.header(&name) {
                    headers.insert(name.to_ascii_lowercase(), v.to_string());
                }
            }
            let mut buf = Vec::new();
            use std::io::Read;
            r.into_reader().read_to_end(&mut buf)?;
            Ok((buf, headers))
        }
        Err(ureq::Error::Status(status, r)) => {
            let body = r.into_string().unwrap_or_default();
            bail!("{}", err_of(status, &body))
        }
        Err(e) => bail!("网络错误: {e}"),
    }
}
pub fn del(cfg: &CliConfig, path: &str, token: Option<&str>) -> anyhow::Result<Value> {
    request(cfg, "DELETE", path, token, None, None, &[])
}
