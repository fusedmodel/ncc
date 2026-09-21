// NCC MCP：把 NCC Registry 作为 MCP server 暴露给任意 Agent（Claude Desktop / Cursor /
// VS Code / 自研 Agent 等），让 Agent 能 agentic 地检索、取回、发布能力制品。
//
// 传输：stdio，逐行 JSON-RPC 2.0（MCP 的 stdio 约定：一行一条消息，不能有内嵌换行）。
//
// ⚠️ 铁律：stdout 只允许出现协议消息。任何日志/提示都必须写 stderr ——
//    否则客户端会把日志当成协议帧解析而报解析错误。
use crate::api;
use crate::api::urlenc;
use crate::config::{self, CliConfig};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// 本实现基于的 MCP 协议版本；客户端若声明了版本则回显，最大化兼容。
const PROTOCOL_VERSION: &str = "2024-11-05";

/// 单条工具返回的文本上限（避免把整篇内容灌进模型上下文）。
const MAX_TEXT: usize = 24000;

/// initialize 时下发给模型的用法说明（MCP 会把它放进系统上下文）。
const INSTRUCTIONS: &str = "\
NCC Registry 是中立、跨协议的能力制品目录（api / skill / mcp / harness / plugin / scaffold / docker-image / benchmark / living）。
把它当成「能力的 npm」来用：

1. 先查后造：用 ncc_list_kinds 看目录里有什么类型，用 ncc_search_catalog 检索是否已有可复用的能力。
2. 看细节：ncc_get_artifact 取元数据；ncc_fetch_artifact 取回正文（例如 SKILL.md 可直接读进上下文照做）。
3. 发布产出：ncc_publish_artifact 把成果发布成条目（需要先 `ncc login`，或配置 API-Key）。
4. 找人与定位：ncc_find_people 按角色检索人才目录；ncc_list_roles 拿角色 id；ncc_get_profile 看某人的名片（作品集 + 已发布能力）。
5. 人脉（需凭据）：ncc_list_contacts 看通讯录；ncc_region_profile 看人脉的区域/职能分布；ncc_recommend_contacts 按区域与角色要推荐。

边界：节点（ncc_list_nodes / ncc_discover_nodes）与授权（ncc_list_grants）属于用户的私人数据，
只在用户问起时用，不要转发给第三方。
连接节点、授权、发布这类会改变「别人能拿到什么」的动作只有读工具 —— 需要变更时，
让用户自己跑 CLI（ncc nodes link / ncc grant set / ncc publish），并先征得同意。

制品引用统一写成 `@命名空间/slug`，也可用 `R-…` 形式的 id。
检索与取回是公开只读的，无需登录；节点类工具需要凭据（API-Key 需 nodes:read / grants:read）。";

/* ---------------- 入口 ---------------- */

/// `ncc mcp`：在 stdin/stdout 上跑 MCP server，直到 stdin 关闭。
pub fn serve(cfg: &CliConfig) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    eprintln!("[ncc-mcp] ready · base={} · 等待 MCP 客户端握手", cfg.base_url);

    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // 解析不了就没有 id 可回，只能记到 stderr
                eprintln!("[ncc-mcp] 非法 JSON 帧: {e}");
                continue;
            }
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        // 通知（无 id）不需要响应；notifications/* 一律忽略
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        if method.starts_with("notifications/") {
            continue;
        }

        let resp = match method.as_str() {
            "initialize" | "ping" | "tools/list" | "tools/call" => {
                match handle(cfg, &method, req.get("params")) {
                    Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
                    Err(e) => json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32603, "message": format!("{e:#}") }
                    }),
                }
            }
            _ => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("未知方法: {method}") }
            }),
        };
        writeln!(out, "{}", serde_json::to_string(&resp)?)?;
        out.flush()?;
    }
    eprintln!("[ncc-mcp] stdin 关闭，退出");
    Ok(())
}

fn handle(cfg: &CliConfig, method: &str, params: Option<&Value>) -> Result<Value> {
    match method {
        "initialize" => {
            let pv = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": pv,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ncc-registry", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(cfg, params),
        _ => unreachable!("serve 已过滤未知方法"),
    }
}

/* ---------------- 工具定义 ---------------- */

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "ncc_list_kinds",
            "description": "列出 NCC 目录里的能力类型（kind）及各类型已发布数量。开始检索前先用它了解目录构成。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_search_catalog",
            "description": "跨命名空间检索能力制品。返回 kind、引用（@命名空间/slug）、版本、摘要、标签与下载量。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "关键词（可选，匹配名称/摘要/slug/描述）" },
                    "kind": { "type": "string", "description": "按类型过滤：api|harness|hur|skill|mcp|plugin|scaffold|docker-image|benchmark|living" },
                    "tag": { "type": "string", "description": "按标签过滤" },
                    "namespace": { "type": "string", "description": "限定命名空间，如 @aya" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_artifact",
            "description": "查看单个制品的完整元数据（含 version、tags、sha256、storage.url、manifest 契约）。",
            "inputSchema": {
                "type": "object",
                "properties": { "target": { "type": "string", "description": "制品引用 @命名空间/slug，或 R-… 形式的 id" } },
                "required": ["target"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_fetch_artifact",
            "description": "取回制品正文。文本类制品（SKILL.md / JSON / YAML / 脚本等）直接返回内容，可直接照做；二进制只返回下载地址与 sha256。",
            "inputSchema": {
                "type": "object",
                "properties": { "target": { "type": "string", "description": "制品引用 @命名空间/slug 或 R-…" } },
                "required": ["target"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_publish_artifact",
            "description": "发布一个能力制品（需要已登录，或配置了 API-Key）。三种字节来源任选其一：content（内联文本）、contentFile（本地文件路径）、url（自带存储直链）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "制品类型，见 ncc_list_kinds" },
                    "name": { "type": "string", "description": "展示名称" },
                    "summary": { "type": "string", "description": "一句话说明" },
                    "slug": { "type": "string", "description": "URL slug；省略则由服务端依据 name 生成" },
                    "version": { "type": "string", "description": "版本，默认 1.0.0" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "标签" },
                    "content": { "type": "string", "description": "制品正文（文本）。会以 filename 作为文件名上传" },
                    "filename": { "type": "string", "description": "配合 content 使用的文件名，如 hotel.SKILL.md" },
                    "contentFile": { "type": "string", "description": "本地文件路径（与 content 二选一）" },
                    "url": { "type": "string", "description": "自带存储：直接发布一个直链，不上传字节" },
                    "manifest": { "type": "object", "description": "封装契约（kind=harness 时用，含 harness.loader / harness.entry）" },
                    "visibility": { "type": "string", "description": "public（默认）| private（需付费套餐）" },
                    "draft": { "type": "boolean", "description": "以 draft 状态创建" }
                },
                "required": ["kind", "name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_whoami",
            "description": "查看当前 CLI 登录的账号与可用的命名空间（发布前用它确认身份与目标命名空间）。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_list_roles",
            "description": "列出 NCC Profile 的工作角色目录（6 组 20 个，含中英标签与描述）。这些 id 用于 ncc_find_people 的 role 过滤与名片设置。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_find_people",
            "description": "按角色 / 技能 / 关键词检索人才目录，找到定位匹配的人（回答「谁做过这类事」）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "角色 id，见 ncc_list_roles，如 fde、aigc-creator" },
                    "skill": { "type": "string", "description": "技能标签，如 RAG" },
                    "query": { "type": "string", "description": "关键词（匹配名字/头衔/技能）" },
                    "availability": { "type": "string", "description": "接洽状态：open|collab|hiring|busy" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_profile",
            "description": "查看某人的 NCC Profile：定位角色、技能、外链、作品集，以及他发布的能力与分享页。省略 username 则看当前登录账号。",
            "inputSchema": {
                "type": "object",
                "properties": { "username": { "type": "string", "description": "用户名（不带 @）；省略则为当前登录账号" } },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_nodes",
            "description": "列出当前账号的 NCC Node：mine = 我自己注册的节点（service 服务 / agent 为人服务的 Agent / assigned 被分配的 Agent，带在线状态与能力），links = 我连接的别人的节点（带我给它的 Name 标签）。这是**节点连接表**，不是通讯录；连接只代表「找得到」，不代表能取对方数据。需要登录/API-Key；属于本人数据，不要向无关第三方转发。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "按类型过滤：service | agent | assigned" },
                    "q": { "type": "string", "description": "关键词（Name 标签 / 节点名 / 用户名）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_discover_nodes",
            "description": "发现**同一个 NCC 实例**上可连接的节点（公开 + 不是我的 + 我还没连）。想连某个能力/服务/Agent 时先用它找到节点引用（@命名空间/节点slug），再让用户跑 `ncc nodes link` 建立连接。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "按类型过滤：service | agent | assigned" },
                    "region": { "type": "string", "description": "按节点归属者所在地筛选，如 杭州" },
                    "limit": { "type": "integer", "description": "返回条数，默认 30，最大 60" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_region_profile",
            "description": "当前账号的**节点覆盖图**：我的节点与连接的节点按归属者所在地聚合，并给出类型分布。用于回答「我的 Agent 网络在哪些区域更密」。区域来自节点归属者的名片所在地。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_recommend_nodes",
            "description": "基于区域密度推荐可连接的**节点**（在服务端已按“同区域已有几个节点”降序，并标注 regionNodes）。比直接 discover 更贴近“该先连谁”。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "region": { "type": "string", "description": "按区域筛选，如 杭州" },
                    "kind": { "type": "string", "description": "按类型筛选：service | agent | assigned" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_grants",
            "description": "查看制品/分享授权关系：outgoing 是我授权给别人的（谁能下载我的私有制品、谁可看我的私有分享页），incoming 是别人给我的。注意：**连接 ≠ 授权** —— 连上一个节点并不代表能取它的数据。",
            "inputSchema": {
                "type": "object",
                "properties": { "direction": { "type": "string", "description": "outgoing（默认）| incoming" } },
                "additionalProperties": false
            }
        }),
    ]
}

/* ---------------- 工具实现 ---------------- */

fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": false })
}

fn tool_err(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": true })
}

fn call_tool(cfg: &CliConfig, params: Option<&Value>) -> Result<Value> {
    let p = params.cloned().unwrap_or(json!({}));
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = p.get("arguments").cloned().unwrap_or(json!({}));
    let sarg = |k: &str| -> Option<String> {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let narg = |k: &str, d: i64, max: i64| -> i64 {
        args.get(k).and_then(|v| v.as_i64()).unwrap_or(d).clamp(1, max)
    };
    let token = config::token_opt(cfg);

    // 工具级失败按 MCP 约定回 isError=true，而不是 JSON-RPC error
    let ok_or_text = |r: Result<Value>| match r {
        Ok(v) => v,
        Err(e) => tool_err(format!("调用失败：{e:#}")),
    };

    Ok(match name {
        "ncc_list_kinds" => ok_or_text((|| {
            let d = api::get(cfg, "/api/registry/kinds", None)?;
            let mut out = String::from("NCC 目录中的能力类型：\n");
            if let Some(ks) = d.get("kinds").and_then(|v| v.as_array()) {
                for k in ks {
                    out.push_str(&format!(
                        "- {}（{}）: {} 个已发布 — {}\n",
                        k.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                        k.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                        k.get("published").and_then(|v| v.as_i64()).unwrap_or(0),
                        k.get("desc").and_then(|v| v.as_str()).unwrap_or(""),
                    ));
                }
            }
            Ok(text(out))
        })()),

        "ncc_search_catalog" => ok_or_text((|| {
            let mut qs: Vec<String> = Vec::new();
            if let Some(q) = sarg("query") {
                qs.push(format!("q={}", urlenc(&q)));
            }
            if let Some(k) = sarg("kind") {
                qs.push(format!("kind={}", urlenc(&k)));
            }
            if let Some(t) = sarg("tag") {
                qs.push(format!("tag={}", urlenc(&t)));
            }
            if let Some(n) = sarg("namespace") {
                qs.push(format!("namespace={}", urlenc(&n)));
            }
            qs.push(format!("size={}", narg("limit", 20, 50)));
            let d = api::get(cfg, &format!("/api/registry?{}", qs.join("&")), token.as_deref())?;
            let total = d.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            let items = d.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if items.is_empty() {
                return Ok(text(format!("没有匹配的条目（共 {total} 条）。换个关键词，或先用 ncc_list_kinds 看看目录构成。")));
            }
            let mut out = format!("共 {total} 条，返回 {} 条：\n", items.len());
            for it in &items {
                let ns = it.pointer("/namespace/slug").and_then(|v| v.as_str()).unwrap_or("");
                let slug = it.get("slug").and_then(|v| v.as_str()).unwrap_or("");
                let tags = it
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- [{}] {}/{slug}@{} — {}{}{}\n",
                    it.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                    ns,
                    it.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                    it.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
                    if tags.is_empty() { String::new() } else { format!("  #{tags}") },
                    format!("  (⬇{})", it.get("downloads").and_then(|v| v.as_i64()).unwrap_or(0)),
                ));
            }
            Ok(text(out))
        })()),

        "ncc_get_artifact" => ok_or_text((|| {
            let target = sarg("target").context("缺少 target")?;
            let d = api::get(cfg, &format!("/api/registry/{}", urlenc(&target)), token.as_deref())?;
            let it = d.get("item").cloned().unwrap_or(d);
            Ok(text(clip(&serde_json::to_string_pretty(&it)?)))
        })()),

        "ncc_fetch_artifact" => ok_or_text((|| {
            let target = sarg("target").context("缺少 target")?;
            let dl = api::get(cfg, &format!("/api/registry/{}/download", urlenc(&target)), token.as_deref())?;
            let url = dl.get("url").and_then(|v| v.as_str()).context("下载响应缺少 url")?;
            let ns = dl.get("namespaceSlug").and_then(|v| v.as_str()).unwrap_or("");
            let slug = dl.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            let ver = dl.get("version").and_then(|v| v.as_str()).unwrap_or("");
            let sha = dl.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
            let head = format!("{ns}/{slug}@{ver}\nurl: {url}\nsha256: {sha}\n\n");

            let agent = ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(60))
                .build();
            let resp = agent.get(url).call().map_err(|e| anyhow::anyhow!("拉取正文失败: {e}"))?;
            let mut buf: Vec<u8> = Vec::new();
            std::io::copy(&mut resp.into_reader(), &mut buf)?;

            match String::from_utf8(buf) {
                Ok(s) if is_texty(url) => Ok(text(format!("{head}{}", clip(&s)))),
                Ok(s) => Ok(text(format!("{head}（非文本类制品，正文 {} 字节，已省略）", s.len()))),
                Err(e) => {
                    let n = e.as_bytes().len();
                    Ok(text(format!("{head}（二进制制品，{n} 字节；请用上面的 url 直接下载）")))
                }
            }
        })()),

        "ncc_publish_artifact" => ok_or_text((|| {
            let token = token.clone().context("发布需要登录：先运行 `ncc login`，或配置 API-Key")?;
            let kind = sarg("kind").context("缺少 kind")?;
            let name = sarg("name").context("缺少 name")?;

            let mut storage_url = sarg("url").unwrap_or_default();
            let mut sha = String::new();
            let mut size: i64 = 0;

            if let Some(content) = args.get("content").and_then(|v| v.as_str()) {
                let fname = sarg("filename").unwrap_or_else(|| format!("{}.txt", kind));
                let up = api::request(
                    cfg, "POST", "/api/registry/uploads", Some(&token), None,
                    Some(content.as_bytes()), &[("X-Filename", fname.as_str())],
                )?;
                storage_url = up.get("storageUrl").and_then(|v| v.as_str()).unwrap_or("").to_string();
                sha = up.get("sha256").and_then(|v| v.as_str()).unwrap_or("").to_string();
                size = up.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
            } else if let Some(path) = sarg("contentFile") {
                let bytes = std::fs::read(&path).with_context(|| format!("读取文件失败: {path}"))?;
                let fname = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("upload.bin")
                    .to_string();
                let up = api::request(
                    cfg, "POST", "/api/registry/uploads", Some(&token), None,
                    Some(&bytes), &[("X-Filename", fname.as_str())],
                )?;
                storage_url = up.get("storageUrl").and_then(|v| v.as_str()).unwrap_or("").to_string();
                sha = up.get("sha256").and_then(|v| v.as_str()).unwrap_or("").to_string();
                size = up.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
            }

            if storage_url.is_empty() {
                anyhow::bail!("需要 content / contentFile / url 之一作为制品字节来源");
            }

            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|t| t.as_str()).map(String::from).collect())
                .unwrap_or_default();
            let mut body = json!({
                "kind": kind, "name": name,
                "slug": sarg("slug"),
                "version": sarg("version").unwrap_or_else(|| "1.0.0".to_string()),
                "summary": sarg("summary").unwrap_or_default(),
                "tags": tags,
                "status": if args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) { "draft" } else { "published" },
                "visibility": sarg("visibility").unwrap_or_else(|| "public".to_string()),
                "storage": {
                    "url": storage_url,
                    "sha256": if sha.is_empty() { Value::Null } else { json!(sha) },
                    "size": if size == 0 { Value::Null } else { json!(size) },
                },
            });
            if let Some(m) = args.get("manifest") {
                if !m.is_null() {
                    body["manifest"] = m.clone();
                }
            }

            let d = api::post_json(cfg, "/api/registry", Some(&token), &body)?;
            let it = d.get("item").cloned().unwrap_or(Value::Null);
            Ok(text(format!(
                "✅ 已发布 [{}] {}/{}@{}  status={}\nid: {}\n存储: {}",
                it.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                it.pointer("/namespace/slug").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("slug").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                it.pointer("/storage/url").and_then(|v| v.as_str()).unwrap_or(""),
            )))
        })()),

        "ncc_whoami" => ok_or_text((|| {
            let token = token.clone().context("未登录：先运行 `ncc login`")?;
            let d = api::get(cfg, "/api/auth/me", Some(&token))?;
            let u = d.get("user").cloned().unwrap_or(Value::Null);
            let mut out = format!(
                "用户 {} <{}>  计划 {}\n",
                u.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                u.get("email").and_then(|v| v.as_str()).unwrap_or(""),
                u.get("plan").and_then(|v| v.as_str()).unwrap_or("free"),
            );
            out.push_str("命名空间：\n");
            if let Some(ns) = d.get("namespaces").and_then(|v| v.as_array()) {
                for n in ns {
                    out.push_str(&format!(
                        "- {} （{}，{}）\n",
                        n.get("slug").and_then(|v| v.as_str()).unwrap_or(""),
                        n.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                        if n.get("owner").and_then(|v| v.as_bool()).unwrap_or(false) { "owner" } else { "member" },
                    ));
                }
            }
            Ok(text(out))
        })()),

        "ncc_list_roles" => ok_or_text((|| {
            let d = api::get(cfg, "/api/profile/roles", None)?;
            let groups = d.get("groups").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let all = d.get("roles").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut out = String::from("NCC Profile 工作角色（id — 名称 — 说明）：\n");
            for g in &groups {
                let gid = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!(
                    "\n【{}】{}\n",
                    g.get("zh").and_then(|v| v.as_str()).unwrap_or(gid),
                    gid
                ));
                for r in all.iter().filter(|r| r.get("group").and_then(|v| v.as_str()) == Some(gid)) {
                    out.push_str(&format!(
                        "- {} — {}：{}\n",
                        r.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        r.get("zh").and_then(|v| v.as_str()).unwrap_or(""),
                        r.get("descZh").and_then(|v| v.as_str()).unwrap_or(""),
                    ));
                }
            }
            Ok(text(out))
        })()),

        "ncc_find_people" => ok_or_text((|| {
            let mut qs: Vec<String> = Vec::new();
            if let Some(r) = sarg("role") {
                qs.push(format!("role={}", urlenc(&r)));
            }
            if let Some(s) = sarg("skill") {
                qs.push(format!("skill={}", urlenc(&s)));
            }
            if let Some(q) = sarg("query") {
                qs.push(format!("q={}", urlenc(&q)));
            }
            if let Some(a) = sarg("availability") {
                qs.push(format!("availability={}", urlenc(&a)));
            }
            qs.push(format!("size={}", narg("limit", 20, 50)));
            let d = api::get(cfg, &format!("/api/profiles?{}", qs.join("&")), token.as_deref())?;
            let total = d.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            let ps = d.get("profiles").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if ps.is_empty() {
                return Ok(text(format!("没有匹配的名片（共 {total} 位）。可用 ncc_list_roles 确认角色 id。")));
            }
            let mut out = format!("共 {total} 位，返回 {} 位：\n", ps.len());
            for p in &ps {
                let roles = p
                    .get("roles")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                let skills = p
                    .get("skills")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- {}（{}）{} — 角色 {}｜技能 {}｜作品 {}｜{}\n",
                    p.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("handle").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("headline").and_then(|v| v.as_str()).unwrap_or(""),
                    roles,
                    skills,
                    p.get("works").and_then(|v| v.as_i64()).unwrap_or(0),
                    p.get("availability").and_then(|v| v.as_str()).unwrap_or(""),
                ));
            }
            Ok(text(out))
        })()),

        "ncc_get_profile" => ok_or_text((|| {
            let path = match sarg("username") {
                Some(u) => format!("/api/profiles/{}", urlenc(u.trim_start_matches('@'))),
                None => "/api/profile/me".to_string(),
            };
            let d = api::get(cfg, &path, token.as_deref())?;
            let p = d.get("profile").cloned().unwrap_or(Value::Null);
            if p.is_null() {
                return Ok(text("还没有名片。可在 ncc.ai 上创建，或用 `ncc profile set` 命令行创建。".to_string()));
            }
            let mut out = format!(
                "{}（{}）\n{}\n",
                p.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                p.get("handle").and_then(|v| v.as_str()).unwrap_or(""),
                p.get("headline").and_then(|v| v.as_str()).unwrap_or(""),
            );
            let list = |v: Option<&Value>| {
                v.and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            };
            let roles = list(p.get("roles"));
            if !roles.is_empty() {
                out.push_str(&format!("定位角色: {roles}\n"));
            }
            let skills = list(p.get("skills"));
            if !skills.is_empty() {
                out.push_str(&format!("技能: {skills}\n"));
            }
            if let Some(bio) = p.get("bio").and_then(|v| v.as_str()) {
                if !bio.is_empty() {
                    out.push_str(&format!("简介: {bio}\n"));
                }
            }
            if let Some(ws) = d.get("works").and_then(|v| v.as_array()) {
                if !ws.is_empty() {
                    out.push_str("\n作品集:\n");
                    for w in ws {
                        out.push_str(&format!(
                            "- {}{} → {}\n",
                            w.get("year").and_then(|v| v.as_str()).map(|y| format!("[{y}] ")).unwrap_or_default(),
                            w.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                            w.get("href").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                }
            }
            if let Some(is) = d.get("items").and_then(|v| v.as_array()) {
                if !is.is_empty() {
                    out.push_str("\n已发布能力:\n");
                    for i in is {
                        out.push_str(&format!(
                            "- [{}] {}/{}@{}\n",
                            i.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                            i.pointer("/namespace/slug").and_then(|v| v.as_str()).unwrap_or(""),
                            i.get("slug").and_then(|v| v.as_str()).unwrap_or(""),
                            i.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                }
            }
            Ok(text(out))
        })()),

        // —— 节点层（只读）：我的节点 / 可连接节点 / 区域聚合 / 推荐 / 授权 ——
        // 只做读。写入类操作（连接节点、授权）一律让用户用 CLI 显式执行：
        // 这些动作会改变「谁能拿到我的东西」或「我的 Agent 会连谁」，不能让 Agent 自作主张。
        "ncc_list_nodes" => ok_or_text((|| {
            let kind = sarg("kind").unwrap_or_default();
            let q = sarg("q").unwrap_or_default();
            let v = if kind.is_empty() && q.is_empty() {
                crate::nodes::fetch_nodes(cfg)?
            } else {
                let mut qs: Vec<String> = Vec::new();
                if !kind.is_empty() {
                    qs.push(format!("kind={}", urlenc(&kind)));
                }
                if !q.is_empty() {
                    qs.push(format!("q={}", urlenc(&q)));
                }
                let path = format!("/api/nodes?{}", qs.join("&"));
                api::get(cfg, &path, config::token_opt(cfg).as_deref())?
            };
            Ok(text(clip(&crate::nodes::render_nodes(&v))))
        })()),

        "ncc_discover_nodes" => ok_or_text((|| {
            let v = crate::nodes::fetch_discover(
                cfg,
                sarg("kind").unwrap_or_default().as_str(),
                narg("limit", 30, 60) as u32,
            )?;
            let region = sarg("region").unwrap_or_default();
            if region.is_empty() {
                Ok(text(clip(&crate::nodes::render_discover(&v))))
            } else {
                // 区域过滤时改走 recommend（服务端在那边才按区域筛 + 排序）
                let r = crate::nodes::fetch_recommend(
                    cfg,
                    &region,
                    sarg("kind").unwrap_or_default().as_str(),
                    narg("limit", 30, 60) as u32,
                )?;
                Ok(text(clip(&crate::nodes::render_recommend(&r))))
            }
        })()),

        "ncc_region_profile" => ok_or_text((|| {
            Ok(text(clip(&crate::nodes::render_region_profile(&crate::nodes::fetch_region_profile(cfg)?))))
        })()),

        "ncc_recommend_nodes" => ok_or_text((|| {
            let v = crate::nodes::fetch_recommend(
                cfg,
                sarg("region").unwrap_or_default().as_str(),
                sarg("kind").unwrap_or_default().as_str(),
                narg("limit", 20, 50) as u32,
            )?;
            Ok(text(clip(&crate::nodes::render_recommend(&v))))
        })()),

        "ncc_list_grants" => ok_or_text((|| {
            let dir = sarg("direction").unwrap_or_else(|| "outgoing".to_string());
            let v = crate::nodes::fetch_grants(cfg, &dir)?;
            Ok(text(clip(&crate::nodes::render_grants(&v, &dir))))
        })()),

        other => tool_err(format!("未知工具: {other}")),
    })
}

/* ---------------- 小工具 ---------------- */

/// 文本类制品的粗略判定（决定是直接把内容给模型，还是只给下载地址）。
fn is_texty(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    const EXTS: [&str; 14] = [
        ".md", ".markdown", ".txt", ".json", ".yaml", ".yml", ".toml", ".sh", ".bash",
        ".py", ".js", ".ts", ".rs", ".go",
    ];
    EXTS.iter().any(|e| path.ends_with(e))
}

/// 按字符（非字节）截断，避免把中文截成半个字。
fn clip(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= MAX_TEXT {
        return s.to_string();
    }
    let head: String = chars[..MAX_TEXT].iter().collect();
    format!("{head}\n…（已截断，原始 {} 字符）", chars.len())
}
