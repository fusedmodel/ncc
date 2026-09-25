//! `hur mcp` —— 把 hur 暴露成 **MCP server（stdio）**，让任何支持 MCP 的宿主 Agent
//! （Claude Code / Cursor / Cline / 其它）都能通过工具调用来装包、看声明、导出宿主配置。
//!
//! 为什么是这条路径：宿主生态千差万别，但只要支持 MCP，就能零改造地用 hur ——
//! 于是「hur 包」自然变成**跨宿主可插件式管理的 Agent 应用包**。
//!
//! 实现要点：
//! - 不引新依赖：JSON-RPC 2.0 over stdio，**一行一条消息**（MCP stdio 传输）；
//! - 核心是纯函数 [`handle`]，stdio 只是壳 —— 所以可以单测，不依赖进程；
//! - 只做**声明式**能力：包只被读取与导出，**不执行包内代码**（与 PRD §11 红线一致）；
//!   写操作（install / sync / export --write）由宿主侧按它自己的授权模型确认。

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::interop;
use crate::spec::{self, MANIFEST};
use crate::{install, pack};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "hur";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 默认：只读（列出 / 查看 / 校验 / 导出预览）
    ReadOnly,
    /// `--allow-write`：允许 pack / install / sync / export 落盘
    ReadWrite,
}

pub struct Ctx {
    pub role: Role,
    /// 工程根（有 `hur.project.json` 时；`hur mcp` 自动向上找）
    pub project_root: Option<PathBuf>,
}

impl Ctx {
    pub fn detect(cwd: &Path, role: Role) -> Self {
        let root = interop::find_project(cwd).ok().and_then(|p| p.parent().map(|x| x.to_path_buf()));
        Self { role, project_root: root }
    }
    fn writable(&self, tool: &str) -> Result<(), String> {
        if self.role == Role::ReadWrite {
            return Ok(());
        }
        Err(format!(
            "{tool} 需要写权限：请在宿主配置里给 hur 加上 `--allow-write`（默认只读）"
        ))
    }
}

/* ---------------- 工具清单 ---------------- */

fn tools() -> Value {
    let s = |t: &str| json!({ "type": "string", "description": t });
    json!([
        {
            "name": "hur_list_agents",
            "description": "列出本机已装的 hur 包与当前工程里同步的包（含 kind / 版本 / 是否带 agent{} 声明）。任何宿主先调这个，才知道有什么可用。",
            "inputSchema": { "type": "object", "properties": {
                "kind": s("按 kind 过滤：agent / harness / repo"),
                "scope": { "type": "string", "enum": ["all", "installed", "project"], "description": "默认 all" }
            } }
        },
        {
            "name": "hur_inspect_agent",
            "description": "读一个 hur 包的声明（agent{}：system_prompt / persona / tools / skills / guard）。用于「把别的包当 Agent 用」——宿主拿到声明后可直接照着扮演，或写入自己的配置。",
            "inputSchema": { "type": "object", "properties": {
                "id": s("包 id（A-…/H-…/repo slug）或引用 @ns/slug"),
                "registry": s("引用是 @ns/slug 时的 registry 地址（可选）")
            }, "required": ["id"] }
        },
        {
            "name": "hur_read_skills",
            "description": "读包内技能文件正文（agent.skills 与 skills/*）。宿主可据此把技能注入自己的上下文。",
            "inputSchema": { "type": "object", "properties": { "id": s("包 id 或 @ns/slug") }, "required": ["id"] }
        },
        {
            "name": "hur_verify",
            "description": "离线校验一个包目录或 .hur 产物（R1~R10：规范/版本/入口/依赖/权限面/摘要一致性/Agent 声明/制品签名），返回结构化结论。装包或信任一个包之前先跑它。",
            "inputSchema": { "type": "object", "properties": { "path": s("包目录或 .hur 文件") }, "required": ["path"] }
        },
        {
            "name": "hur_export_agent",
            "description": "把 hur 包声明式翻译成宿主原生配置：claude（.claude/agents + .claude/skills）· cursor（.cursor/rules）· cline（.clinerules）· codex（AGENTS.md 标记块）· mcp（.mcp.json）。默认只返回预览，write=true 才落盘。",
            "inputSchema": { "type": "object", "properties": {
                "id": s("包 id（本机已装）"),
                "targets": { "type": "array", "items": { "type": "string" }, "description": "claude/cursor/cline/codex/mcp，缺省=all" },
                "out": s("落点目录（默认当前目录）"),
                "write": { "type": "boolean", "description": "true 才真正写文件（需 --allow-write）" }
            }, "required": ["id"] }
        },
        {
            "name": "hur_install_agent",
            "description": "安装一个 hur 包到本机（@ns/slug 走 registry、./x.hur 走本地产物）。安装后本机所有 Agent 都能 `hur export` 出来用。sha256 不一致会被拒绝。",
            "inputSchema": { "type": "object", "properties": {
                "ref": s("@ns/slug 或 .hur 路径"),
                "registry": s("registry 地址（可选）"),
                "sha256": s("期望摘要（可选，来自 registry 元数据）"),
                "force": { "type": "boolean" }
            }, "required": ["ref"] }
        },
        {
            "name": "hur_sync_project",
            "description": "按 hur.project.json 同步工程依赖（registry / .hur / 本地目录三种来源），落到 .hur/packages/ 并写 .hur/lock.json；export=true 时同时生成宿主配置。团队复现用这个。",
            "inputSchema": { "type": "object", "properties": {
                "dir": s("工程目录（默认当前目录）"),
                "force": { "type": "boolean" },
                "export": { "type": "boolean" },
                "targets": { "type": "array", "items": { "type": "string" } }
            } }
        },
        {
            "name": "hur_pack",
            "description": "把一个 hur 包目录打成 dist/<id>-<version>.hur + .sha256（条目排序 + 固定时间戳 → 同内容同摘要）。发布或离线分发前用。",
            "inputSchema": { "type": "object", "properties": { "path": s("包目录") }, "required": ["path"] }
        }
    ])
}

fn instructions(ctx: &Ctx) -> String {
    let project = match &ctx.project_root {
        Some(p) => format!("当前工程：{}（hur.project.json）", p.display()),
        None => "当前目录不在 hur 工程内（可以 `hur add <ref>` 新建一个）".to_string(),
    };
    format!(
        "hur 是 Agent 应用包的包管理器（规范 {}）。用法顺序：hur_list_agents 看有什么 → \
         hur_inspect_agent / hur_read_skills 读声明与技能 → hurt_export_agent 生成你这个宿主的原生配置 → \
         hur_verify 做信任检查。包是**声明式**的：hur 只读 hur.json 的 agent{{}} 与 skills/*，\
         不执行包内代码。{project}",
        spec::PKG_SPEC
    )
}

/* ---------------- 包定位 ---------------- */

fn resolve_pkg_dir(ctx: &Ctx, id_or_ref: &str) -> Result<(PathBuf, Option<String>), String> {
    let key = id_or_ref.trim();
    if key.is_empty() {
        return Err("id 不能为空".into());
    }
    // 1) 本机已装
    let installed = crate::cfg::packages_dir().join(key);
    if installed.join(MANIFEST).is_file() {
        return Ok((installed, None));
    }
    // 2) 当前工程
    if let Some(root) = &ctx.project_root {
        let p = root.join(interop::PROJECT_DIR).join("packages").join(key);
        if p.join(MANIFEST).is_file() {
            return Ok((p, None));
        }
    }
    // 3) 本地路径
    let p = PathBuf::from(key);
    if p.join(MANIFEST).is_file() {
        return Ok((p, None));
    }
    // 4) 按名字模糊匹配已装包（宿主常常只知道「名字」）
    let lower = key.to_lowercase();
    if let Ok(rd) = std::fs::read_dir(crate::cfg::packages_dir()) {
        for e in rd.flatten() {
            let dir = e.path();
            if !dir.join(MANIFEST).is_file() {
                continue;
            }
            if let Ok(pkg) = spec::read_pkg(&dir) {
                if pkg.name.to_lowercase() == lower || pkg.short.to_lowercase() == lower {
                    return Ok((dir, None));
                }
            }
        }
    }
    Err(format!("找不到包「{key}」：本机未安装、当前工程内也没有。先 hur_install_agent 或 hur_list_agents 看一眼"))
}

fn files_of_dir(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in spec::content_files(dir) {
        let rel = spec::rel(dir, &f);
        if let Ok(t) = std::fs::read_to_string(&f) {
            out.push((rel, t));
        }
    }
    if let Ok(t) = std::fs::read_to_string(dir.join(MANIFEST)) {
        out.push((MANIFEST.to_string(), t));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }] })
}

fn fail(s: impl Into<String>) -> Value {
    let mut v = text(s);
    v["isError"] = json!(true);
    v
}

/* ---------------- 工具实现 ---------------- */

fn call_tool(ctx: &Ctx, name: &str, args: &Value) -> Value {
    let str_arg = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let bool_arg = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let targets_arg = |k: &str| -> Result<Vec<String>, String> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(interop::TARGETS.iter().map(|s| s.to_string()).collect()),
            Some(Value::Array(a)) => {
                let raw: Vec<String> = a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect();
                interop::parse_targets(&raw.join(",")).map_err(|e| e.to_string())
            }
            Some(Value::String(s)) => interop::parse_targets(s).map_err(|e| e.to_string()),
            _ => Err("targets 必须是字符串数组".into()),
        }
    };

    match name {
        "hur_list_agents" => {
            let kind = str_arg("kind");
            let scope = {
                let s = str_arg("scope");
                if s.is_empty() { "all".to_string() } else { s }
            };
            let mut rows: Vec<Value> = Vec::new();
            if scope != "project" {
                for r in install::list_installed() {
                    if !kind.is_empty() && r.kind != kind {
                        continue;
                    }
                    rows.push(json!({
                        "id": r.id, "name": r.name, "kind": r.kind, "version": r.version,
                        "declarative": crate::cfg::packages_dir().join(&r.id).join(MANIFEST).is_file(),
                        "enabled": r.enabled, "scope": "installed", "path": r.path,
                    }));
                }
            }
            if scope != "installed" {
                if let Some(root) = &ctx.project_root {
                    for (e, dir) in interop::project_agents(root) {
                        if !kind.is_empty() {
                            let k = spec::read_pkg(&dir).map(|p| p.kind).unwrap_or_default();
                            if k != kind {
                                continue;
                            }
                        }
                        rows.push(json!({
                            "id": e.id, "name": e.name, "version": e.version, "source": e.source,
                            "declarative": dir.join(MANIFEST).is_file(), "scope": "project",
                            "path": dir.to_string_lossy(),
                        }));
                    }
                }
            }
            text(format!("{}\n", serde_json::to_string_pretty(&rows).unwrap_or_default()))
        }
        "hur_inspect_agent" => match resolve_pkg_dir(ctx, &str_arg("id")) {
            Ok((dir, _)) => match spec::read_pkg(&dir) {
                Ok(pkg) => {
                    let files = files_of_dir(&dir);
                    let skills = interop::skills_of(&pkg, &files).into_iter().map(|(p, _)| p).collect::<Vec<_>>();
                    let decl = match interop::agent_of(&pkg) {
                        Some(a) => json!({
                            "system_prompt": a.system_prompt,
                            "persona": a.persona,
                            "tools": a.tools,
                            "skills": a.skills,
                            "guard": a.guard,
                            "pipeline": a.pipeline,
                        }),
                        None => Value::Null,
                    };
                    text(format!(
                        "{}\n",
                        serde_json::to_string_pretty(&json!({
                            "id": pkg.id, "name": pkg.name, "kind": pkg.kind, "version": pkg.version,
                            "summary": pkg.summary, "domain": pkg.domain,
                            "permissions_network": pkg.permissions.network,
                            "declared": decl, "available_skills": skills,
                            "path": dir.to_string_lossy(),
                            "note": "声明式：hur 不执行包内代码；宿主可按此声明扮演或写进自己的配置",
                        }))
                        .unwrap_or_default()
                    ))
                }
                Err(e) => fail(e.to_string()),
            },
            Err(e) => fail(e),
        },
        "hur_read_skills" => match resolve_pkg_dir(ctx, &str_arg("id")) {
            Ok((dir, _)) => match spec::read_pkg(&dir) {
                Ok(pkg) => {
                    let files = files_of_dir(&dir);
                    let skills = interop::skills_of(&pkg, &files);
                    if skills.is_empty() {
                        return text("（这个包没有技能文件）");
                    }
                    let mut s = String::new();
                    for (p, c) in &skills {
                        s.push_str(&format!("## {p}\n\n{}\n\n", c.trim_end()));
                    }
                    text(s)
                }
                Err(e) => fail(e.to_string()),
            },
            Err(e) => fail(e),
        },
        "hur_verify" => {
            let p = PathBuf::from(str_arg("path"));
            if !p.exists() {
                return fail(format!("{} 不存在", p.display()));
            }
            let (dir, is_archive) = if p.is_dir() {
                (p.clone(), false)
            } else {
                let tmp = std::env::temp_dir().join(format!("hur-mcp-verify-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&tmp);
                match pack::unpack(&p, &tmp) {
                    Ok(_) => (tmp, true),
                    Err(e) => return fail(e.to_string()),
                }
            };
            match spec::read_pkg(&dir) {
                Ok(pkg) => {
                    let lock = spec::read_lock(&dir);
                    let issues = spec::validate(&pkg, &dir, lock.as_ref(), false);
                    let errors = issues.iter().filter(|i| i.level == spec::Level::Error).count();
                    let warns = issues.iter().filter(|i| i.level == spec::Level::Warn).count();
                    text(format!(
                        "{}\n",
                        serde_json::to_string_pretty(&json!({
                            "ok": errors == 0, "errors": errors, "warnings": warns,
                            "id": pkg.id, "version": pkg.version, "archive": is_archive,
                            "issues": issues.iter().map(|i| json!({"rule": i.rule, "level": format!("{:?}", i.level), "msg": i.msg})).collect::<Vec<_>>(),
                        }))
                        .unwrap_or_default()
                    ))
                }
                Err(e) => fail(e.to_string()),
            }
        }
        "hur_export_agent" => {
            let id = str_arg("id");
            let targets = match targets_arg("targets") {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let write = bool_arg("write");
            if write {
                if let Err(e) = ctx.writable("hur_export_agent") {
                    return fail(e);
                }
            }
            match resolve_pkg_dir(ctx, &id) {
                Ok((dir, _)) => match spec::read_pkg(&dir) {
                    Ok(pkg) => {
                        let files = files_of_dir(&dir);
                        match interop::render(&pkg, &files, &targets) {
                            Ok(arts) => {
                                let out = {
                                    let o = str_arg("out");
                                    if o.is_empty() { PathBuf::from(".") } else { PathBuf::from(o) }
                                };
                                let mut plan = interop::render_plan(&pkg, &arts, &out.to_string_lossy());
                                if write {
                                    match interop::apply(&out, &arts) {
                                        Ok(paths) => {
                                            plan.push_str("\n已写入：\n");
                                            for p in paths {
                                                plan.push_str(&format!("  {}\n", p.display()));
                                            }
                                        }
                                        Err(e) => return fail(e.to_string()),
                                    }
                                } else {
                                    plan.push_str("\n（预览：确认无误后用 write=true 落盘）\n");
                                }
                                plan.push_str("\n--- 产物内容 ---\n");
                                for a in &arts {
                                    plan.push_str(&format!("\n### {} [{}]\n\n{}\n", a.path, a.target, a.content.trim_end()));
                                }
                                text(plan)
                            }
                            Err(e) => fail(e.to_string()),
                        }
                    }
                    Err(e) => fail(e.to_string()),
                },
                Err(e) => fail(e),
            }
        }
        "hur_install_agent" => {
            if let Err(e) = ctx.writable("hur_install_agent") {
                return fail(e);
            }
            let ref_ = str_arg("ref");
            let registry = str_arg("registry");
            let sha = str_arg("sha256");
            let force = bool_arg("force");
            let local = PathBuf::from(&ref_);
            let res = if local.is_file() && ref_.ends_with(".hur") {
                install::install_archive(&local, if sha.is_empty() { None } else { Some(&sha) }, None, force)
            } else {
                let cfg = crate::cfg::load_cfg();
                let base = if registry.is_empty() { cfg.effective_registry() } else { registry };
                if base.trim().is_empty() {
                    return fail("需要 registry 地址：给 registry 参数，或先 `hur login --registry …`");
                }
                match crate::registry::Registry::new(&base, &cfg.effective_token()) {
                    Ok(reg) => install::install_remote(&reg, &ref_, force),
                    Err(e) => return fail(e.to_string()),
                }
            };
            match res {
                Ok(r) => text(format!(
                    "{}\n",
                    serde_json::to_string_pretty(&json!({
                        "installed": r.id, "name": r.name, "version": r.version,
                        "sha256": r.sha256, "path": r.path,
                        "next": format!("hur_export_agent {{\"id\":\"{}\",\"targets\":[\"claude\"],\"write\":true}}", r.id),
                    }))
                    .unwrap_or_default()
                )),
                Err(e) => fail(e.to_string()),
            }
        }
        "hur_sync_project" => {
            let export = bool_arg("export");
            if export {
                if let Err(e) = ctx.writable("hur_sync_project --export") {
                    return fail(e);
                }
            }
            if ctx.project_root.is_none() {
                return fail("当前目录不在 hur 工程内：先 `hur add <ref>` 生成 hur.project.json");
            }
            let proj_file = ctx
                .project_root
                .as_ref()
                .map(|r| r.join(interop::PROJECT_FILE))
                .unwrap();
            let targets = match targets_arg("targets") {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let targets = if targets.len() == interop::TARGETS.len() { vec![] } else { targets };
            match interop::sync(&proj_file, None, "", bool_arg("force"), export, &targets) {
                Ok(plan) => text(format!(
                    "{}\n",
                    serde_json::to_string_pretty(&json!({
                        "root": plan.root.to_string_lossy(),
                        "lock": plan.lock_path.to_string_lossy(),
                        "targets": plan.targets,
                        "out": plan.out.to_string_lossy(),
                        "packages": plan.agents.iter().map(|a| json!({
                            "id": a.entry.id, "version": a.entry.version, "source": a.entry.source,
                            "sha256": a.entry.sha256,
                            "artifacts": a.artifacts.iter().map(|x| x.path.clone()).collect::<Vec<_>>(),
                            "written": a.written.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    }))
                    .unwrap_or_default()
                )),
                Err(e) => fail(e.to_string()),
            }
        }
        "hur_pack" => {
            if let Err(e) = ctx.writable("hur_pack") {
                return fail(e);
            }
            let dir = PathBuf::from(str_arg("path"));
            match pack::pack(&dir) {
                Ok(o) => text(format!(
                    "{}\n",
                    serde_json::to_string_pretty(&json!({
                        "file": o.file.to_string_lossy(), "sha256": o.sha256,
                        "entries": o.entries.len(), "bytes": o.bytes,
                    }))
                    .unwrap_or_default()
                )),
                Err(e) => fail(e.to_string()),
            }
        }
        _ => fail(format!("未知工具「{name}」（用 tools/list 看可用工具）")),
    }
}

/* ---------------- JSON-RPC ---------------- */

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 处理一条 JSON-RPC 消息；返回 `None` = 通知（无需回复）
pub fn handle(ctx: &Ctx, line: &str) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(rpc_error(Value::Null, -32700, &format!("JSON 解析失败：{e}"))),
    };
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
    let is_notification = req.get("id").is_none();

    let out = match method {
        "initialize" => rpc_result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": SERVER_NAME, "version": crate::VERSION },
                "instructions": instructions(ctx),
            }),
        ),
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            if name.is_empty() {
                rpc_error(id, -32602, "tools/call 缺 name")
            } else {
                rpc_result(id, call_tool(ctx, name, &args))
            }
        }
        "resources/list" => rpc_result(id, json!({ "resources": [] })),
        "prompts/list" => rpc_result(id, json!({ "prompts": [] })),
        "" => rpc_error(id, -32600, "缺 method"),
        other => rpc_error(id, -32601, &format!("不支持的方法「{other}」")),
    };
    if is_notification {
        None
    } else {
        Some(out)
    }
}

/// stdio 主循环：一行一条 JSON-RPC 消息（stdout 只输出协议消息，日志一律走 stderr）
pub fn serve(ctx: &Ctx) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    eprintln!(
        "[hur mcp] {} v{} 已就绪（{}）· 工具 {} 个 · stdout 只走协议",
        SERVER_NAME,
        crate::VERSION,
        if ctx.role == Role::ReadWrite { "可写" } else { "只读" },
        8
    );
    for line in stdin.lock().lines() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(resp) = handle(ctx, t) {
            writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx { role: Role::ReadOnly, project_root: None }
    }

    fn call(line: &str) -> Value {
        handle(&ctx(), line).expect("需要回复")
    }

    #[test]
    fn initialize_reports_tools_capability() {
        let r = call(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(r["result"]["instructions"].as_str().unwrap().contains("不执行包内代码"));
    }

    #[test]
    fn tools_list_has_eight_tools_with_schemas() {
        let r = call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let arr = r["result"]["tools"].as_array().unwrap();
        assert_eq!(arr.len(), 8, "{arr:?}");
        for t in arr {
            assert!(t["name"].as_str().unwrap().starts_with("hur_"));
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(!t["description"].as_str().unwrap().is_empty());
        }
    }

    #[test]
    fn notifications_get_no_reply_and_unknown_method_errors() {
        assert!(handle(&ctx(), r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let r = call(r#"{"jsonrpc":"2.0","id":3,"method":"tools/nope"}"#);
        assert_eq!(r["error"]["code"], -32601);
        let bad = call("not json");
        assert_eq!(bad["error"]["code"], -32700);
    }

    #[test]
    fn read_only_blocks_write_tools() {
        let r = call(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"hur_pack","arguments":{"path":"."}}}"#);
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("--allow-write"));
    }

    #[test]
    fn unknown_tool_is_readable_error() {
        let r = call(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#);
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("未知工具"));
    }

    #[test]
    fn list_agents_returns_json_even_when_empty() {
        let r = call(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"hur_list_agents","arguments":{}}}"#);
        let t = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(t.trim_start().starts_with('['), "{t}");
    }

    /// 端到端（不经进程）：真包目录 → inspect / read_skills / verify / export 预览
    #[test]
    fn inspect_verify_and_export_a_real_package() {
        use crate::spec::{AgentSpec, Deps, HurPackage, Permissions, PublishInfo, PKG_SPEC};
        let dir = std::env::temp_dir().join(format!("hur-mcp-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let pkg = HurPackage {
            egress: None,
            spec: PKG_SPEC.into(),
            kind: "agent".into(),
            id: "A-hotel-front-desk-abc123".into(),
            name: "Front Desk".into(),
            version: "0.2.0".into(),
            short: "FD".into(),
            domain: "hotel".into(),
            summary: "接待助手。".into(),
            entry: "src/agent.ts".into(),
            runtime: "local-v0".into(),
            capabilities: vec![],
            deps: Deps::default(),
            permissions: Permissions::default(),
            publish: PublishInfo::default(),
            security: None,
            agent: Some(AgentSpec {
                system_prompt: "你是前台接待。".into(),
                persona: String::new(),
                tools: vec!["kb.search".into()],
                skills: vec!["skills/front-desk.md".into()],
                pipeline: None,
                guard: None,
                adapters: vec![],
            }),
        };
        std::fs::write(dir.join(MANIFEST), serde_json::to_string_pretty(&pkg).unwrap()).unwrap();
        std::fs::write(dir.join("src/agent.ts"), "export const x = 1\n").unwrap();
        std::fs::write(dir.join("skills/front-desk.md"), "# 房态查询\n\n- 何时用：问房态\n").unwrap();

        let insp = handle(
            &ctx(),
            &format!(
                r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"hur_inspect_agent","arguments":{{"id":"{}"}}}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        let t = insp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(t.contains("你是前台接待"), "{t}");
        assert!(t.contains("kb.search"), "{t}");
        assert!(t.contains("skills/front-desk.md"), "{t}");

        let sk = handle(
            &ctx(),
            &format!(
                r#"{{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{{"name":"hur_read_skills","arguments":{{"id":"{}"}}}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        assert!(sk["result"]["content"][0]["text"].as_str().unwrap().contains("房态查询"));

        let ver = handle(
            &ctx(),
            &format!(
                r#"{{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{{"name":"hur_verify","arguments":{{"path":"{}"}}}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        let vt = ver["result"]["content"][0]["text"].as_str().unwrap();
        assert!(vt.contains("\"ok\": true"), "{vt}");

        let exp = handle(
            &ctx(),
            &format!(
                r#"{{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{{"name":"hur_export_agent","arguments":{{"id":"{}","targets":"claude,cursor"}}}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        let et = exp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(et.contains(".claude/agents/hur-front-desk.md"), "{et}");
        assert!(et.contains(".cursor/rules/hur-front-desk.mdc"), "{et}");
        assert!(et.contains("预览：确认无误后"), "{et}");

        // 模糊名字（宿主常只知道名字）也要能找到
        let by_name = resolve_pkg_dir(&ctx(), &dir.to_string_lossy()).unwrap();
        assert_eq!(by_name.0, dir);
    }
}
