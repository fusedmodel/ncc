//! registry 客户端（ureq）：探活 / 登录 / 上传 / 发布 / 详情 / 下载。
//!
//! 协议与 NCC Registry 对齐：`POST /api/registry/uploads`（raw body + X-Filename）
//! → `POST /api/registry`（元数据 + storage）→ `GET /api/registry/:ns/:slug[/download]`。
//! 认证：`Authorization: Bearer <jwt 或 ncc_ API-Key>`。

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;

#[derive(Debug, Clone)]
pub struct Registry {
    pub base: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct Uploaded {
    pub storage_url: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
}

fn err_from(status: u16, body: &str) -> anyhow::Error {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let code = v.get("error").and_then(|s| s.as_str()).unwrap_or("");
        let msg = v.get("message").and_then(|s| s.as_str()).unwrap_or(body);
        if !code.is_empty() || !msg.is_empty() {
            return anyhow!("registry 返回 {status}：{msg}{}", if code.is_empty() { String::new() } else { format!("（{code}）") });
        }
    }
    anyhow!("registry 返回 {status}：{}", body.chars().take(200).collect::<String>())
}

impl Registry {
    pub fn new(base: &str, token: &str) -> Result<Self> {
        let base = base.trim().trim_end_matches('/');
        if base.is_empty() {
            bail!("没有 registry 地址：用 `--registry <url>`、`hur login --registry <url>` 或环境变量 HUR_REGISTRY");
        }
        if !base.starts_with("http://") && !base.starts_with("https://") {
            bail!("registry 地址要带协议（如 http://localhost:8181）");
        }
        Ok(Self { base: base.to_string(), token: token.trim().to_string() })
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        let url = format!("{}{}", self.base, path);
        let mut r = match method {
            "POST" => ureq::post(&url),
            "PATCH" => ureq::patch(&url),
            _ => ureq::get(&url),
        };
        if !self.token.is_empty() {
            r = r.set("Authorization", &format!("Bearer {}", self.token));
        }
        r.set("Accept", "application/json")
    }

    fn json_of(&self, res: Result<ureq::Response, ureq::Error>) -> Result<Value> {
        match res {
            Ok(r) => {
                let text = r.into_string().unwrap_or_default();
                if text.trim().is_empty() {
                    Ok(json!({}))
                } else {
                    serde_json::from_str(&text).with_context(|| format!("响应不是合法 JSON：{}", text.chars().take(160).collect::<String>()))
                }
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                Err(err_from(code, &body))
            }
            Err(e) => Err(anyhow!("连不上 {}：{e}", self.base)),
        }
    }

    pub fn meta(&self) -> Result<Value> {
        self.json_of(self.req("GET", "/api/meta").call())
    }

    pub fn me(&self) -> Result<Value> {
        self.json_of(self.req("GET", "/api/auth/me").call())
    }

    /// 邮箱密码登录（不需要 token）
    pub fn login(base: &str, email: &str, password: &str) -> Result<(String, Value)> {
        let reg = Registry::new(base, "")?;
        let body = json!({ "email": email, "password": password });
        let v = reg.json_of(reg.req("POST", "/api/auth/login").send_json(body))?;
        let token = v.get("token").and_then(|t| t.as_str()).unwrap_or_default().to_string();
        if token.is_empty() {
            bail!("登录成功但没拿到 token：{v}");
        }
        Ok((token, v.get("user").cloned().unwrap_or(Value::Null)))
    }

    pub fn upload(&self, filename: &str, bytes: Vec<u8>) -> Result<Uploaded> {
        let url = format!("{}/api/registry/uploads", self.base);
        let mut r = ureq::post(&url).set("X-Filename", filename).set("Content-Type", "application/octet-stream");
        if !self.token.is_empty() {
            r = r.set("Authorization", &format!("Bearer {}", self.token));
        }
        let res = r.send_bytes(&bytes);
        let v = self.json_of(res)?;
        let storage_url = v.get("storageUrl").and_then(|s| s.as_str()).unwrap_or_default().to_string();
        if storage_url.is_empty() {
            bail!("上传成功但没拿到 storageUrl：{v}");
        }
        Ok(Uploaded {
            storage_url,
            filename: v.get("filename").and_then(|s| s.as_str()).unwrap_or(filename).to_string(),
            size: v.get("size").and_then(|n| n.as_u64()).unwrap_or(bytes.len() as u64),
            sha256: v.get("sha256").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
        })
    }

    pub fn create_item(&self, body: Value) -> Result<Value> {
        // 同时注册有无尾斜杠两条路由（gin 对无尾斜杠会 301，部分客户端不跟随 POST）
        self.json_of(self.req("POST", "/api/registry").send_json(body))
    }

    pub fn patch_item(&self, id: &str, body: Value) -> Result<Value> {
        self.json_of(self.req("PATCH", &format!("/api/registry/{id}")).send_json(body))
    }

    /// 按 `ns/slug` 取条目：兼容命名空间 slug 带（`@hur`）与不带（`hur`）两种写法
    fn by_alias(&self, ns: &str, slug: &str, suffix: &str) -> Result<Value> {
        let ns = ns.trim();
        let candidates = if ns.starts_with('@') {
            vec![
                format!("/api/registry/{ns}/{slug}{suffix}"),
                format!("/api/registry/{}/{slug}{suffix}", ns.trim_start_matches('@')),
            ]
        } else {
            vec![
                format!("/api/registry/{ns}/{slug}{suffix}"),
                format!("/api/registry/@{ns}/{slug}{suffix}"),
            ]
        };
        let mut last: Option<anyhow::Error> = None;
        for path in candidates {
            match self.json_of(self.req("GET", &path).call()) {
                Ok(v) => return Ok(v),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("条目不存在")))
    }

    pub fn item(&self, ns: &str, slug: &str) -> Result<Value> {
        self.by_alias(ns, slug, "")
    }

    pub fn download_info(&self, ns: &str, slug: &str) -> Result<Value> {
        self.by_alias(ns, slug, "/download")
    }

    /// 在我的命名空间下按 slug 找条目（用于 --update）
    pub fn find_mine_by_slug(&self, slug: &str) -> Result<Option<Value>> {
        let v = self.json_of(self.req("GET", &format!("/api/registry?mine=1&size=100&q={slug}")).call())?;
        Ok(v.get("items")
            .and_then(|a| a.as_array())
            .and_then(|a| a.iter().find(|it| it.get("slug").and_then(|s| s.as_str()) == Some(slug)))
            .cloned())
    }

    /// 下载字节（若是同源直链则带 token）
    pub fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let absolute = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            format!("{}{}", self.base, if url.starts_with('/') { url.to_string() } else { format!("/{url}") })
        };
        let mut r = ureq::get(&absolute);
        if !self.token.is_empty() && absolute.starts_with(&self.base) {
            r = r.set("Authorization", &format!("Bearer {}", self.token));
        }
        match r.call() {
            Ok(res) => {
                let mut body = Vec::new();
                res.into_reader().read_to_end(&mut body).with_context(|| "读取下载内容失败")?;
                Ok(body)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                Err(err_from(code, &text))
            }
            Err(e) => Err(anyhow!("下载失败：{e}")),
        }
    }
}
