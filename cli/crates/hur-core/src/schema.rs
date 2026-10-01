//! `hur.json` 的 **JSON Schema**：机器可读的「形状」契约。
//!
//! 谁需要它：
//!
//! 1. **编辑器** —— `ncc hur init` / `ncc hur schema --write` 会落一份
//!    `hur.schema.json` 并把 `.vscode/settings.json` 指过去，于是写清单时字段名写错、
//!    枚举写错、必填漏掉，在编辑器里当场就能看见（不用等 `verify`）。
//! 2. **任何语言的 SDK / 工具** —— 有了 schema 就不必读 Rust 源码、也不必手抄一遍字段表：
//!    生成器、表单、校验器都可以从它出发。
//! 3. **我们自己** —— `tpl.rs` 生成的清单必须过它（有测试兜着）。
//!
//! ⚠️ **这里不复制规则**：R1~R12 的权威实现永远在 [`crate::spec::validate`]。
//! schema 只描述**形状**（字段名 / 类型 / 枚举 / 数据类必填项）与**profile 表**，
//! 一条规则文案都不抄 —— 两份说法迟早会漂，而漂的那一天没人知道该信谁。
//! 所以 schema 里带的是指针：`x-hur-profiles` 告诉你"这个 profile 要什么"，
//! 具体判定用 `ncc hur verify`。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::profile;
use crate::spec::{DATA_PAYLOAD, DATA_PRIVACY, PKG_SPEC};

/// 落盘的 schema 文件名（包根目录，**不进包**：它不是 src/skills/kb/data/assets 里的东西）。
pub const SCHEMA_FILE: &str = "hur.schema.json";
/// 编辑器接线所在目录。
pub const EDITOR_DIR: &str = ".vscode";
/// 编辑器接线文件。
pub const EDITOR_SETTINGS: &str = ".vscode/settings.json";

/// 生成 JSON Schema（draft 2020-12）。
pub fn json_schema() -> Value {
    let arr = |items: Value| json!({ "type": "array", "items": items });
    let strs = || arr(json!({ "type": "string" }));

    // profile 表：**从规范里生成**，不手抄 —— SDK 拿这一份就知道"这类包该有什么"。
    let profiles: Vec<Value> = profile::PROFILES
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "summary": p.summary,
                "executable": p.executable,
                "data": p.data,
                "registry_kinds": p.registry_kinds,
                "hosts": p.hosts,
                "default_entry": p.default_entry(),
                "requires": p.requires,
                "forbids": p.forbids,
                "match_by": p.match_by,
                "generatable": p.init_blocker().is_none(),
                "init_blocker": p.init_blocker(),
            })
        })
        .collect();

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://ncc.ai/hur.schema.json",
        "title": "hur.json —— HUR 包清单",
        "description": "包应该提供的内容（不是自述文件）。形状由本 schema 描述；能不能过、该不该红，由 `ncc hur verify` 判（R1~R12）。",
        "type": "object",
        // 多写的字段不报错：老工具/新字段并存是常态，schema 不该比校验器更严
        "additionalProperties": true,
        "required": ["spec", "kind", "id", "name", "version"],
        "properties": {
            "spec": { "const": PKG_SPEC, "description": "格式版本；本 schema 只描述这个版本" },
            "kind": {
                "type": "string",
                "description": "包 kind（agent / harness / repo，或与 profile 同名的 skill / mcp / plugin / app / scaffold）",
            },
            "profile": {
                "type": "string",
                "enum": profile::names(),
                "description": "这份包**是什么** —— 决定必填项、能不能执行、怎么接进宿主。写了就是权威；不写按 kind 推导（老包兼容）",
            },
            "id": { "type": "string", "minLength": 1 },
            "name": { "type": "string", "minLength": 1 },
            "version": { "type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+" },
            "short": { "type": "string" },
            "domain": { "type": "string" },
            "summary": { "type": "string" },
            "entry": {
                "type": "string",
                "description": "包内入口文件。可执行的 profile 必须有；skill 与数据快照**不许有**（R12）",
            },
            "runtime": { "type": "string" },
            "capabilities": strs(),
            "deps": {
                "type": "object",
                "description": "依赖（声明式引用，形如 @命名空间/slug）",
                "properties": {
                    "harness": strs(), "agent": strs(), "skill": strs(), "kb": strs(), "mcp": strs(),
                },
            },
            "permissions": {
                "type": "object",
                "properties": {
                    "network": { "allOf": [strs()], "description": "允许访问的域名（支持 *.example.com；超范围调用 = verify 直接红）" },
                    "local": { "allOf": [strs()], "description": "本机资源：kb / files / packages" },
                },
            },
            "publish": {
                "type": "object",
                "properties": {
                    "registry": { "type": "string" },
                    "namespace": { "type": "string" },
                    "visibility": { "type": "string", "description": "public / private / internal（private 需 Pro）" },
                    "slug": { "type": "string" },
                },
            },
            "agent": {
                "type": "object",
                "description": "Agent 与宿主声明（声明式装配；宿主读它就能工作，不必执行包内代码）",
                "properties": {
                    "system_prompt": { "type": "string" },
                    "persona": { "type": "string" },
                    "tools": strs(),
                    "skills": strs(),
                    "adapters": { "allOf": [strs()], "description": "打算接进哪些宿主（claude / cursor / cline / codex / mcp）；profile=plugin 至少要两个" },
                    "guard": { "type": "object" },
                    "pipeline": { "type": ["object", "null"] },
                },
            },
            "data": {
                "type": "object",
                "description": "数据快照声明。只有 kb-seed / mem-seed / ckpt-set / trace-set 允许，且它们不许有 entry 与 permissions.network",
                "required": ["source", "snapshot_at", "privacy"],
                "properties": {
                    "source": { "type": "string", "description": "从哪儿取的（@命名空间/slug 或 local）" },
                    "snapshot_at": { "type": "string", "description": "什么时候的数据" },
                    "privacy": { "type": "string", "enum": DATA_PRIVACY },
                    "license": { "type": "string" },
                    "payload": { "type": "string", "enum": DATA_PAYLOAD, "description": "带多少载荷；full 不许与 privacy=public 同时出现" },
                    "docs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "必须在 data/ 下，且真的在包里" },
                                "visibility": { "type": "string" },
                                "sha256": { "type": "string" },
                            },
                        },
                    },
                },
            },
            "state": {
                "type": "object",
                "description": "要节点提供什么（kb / memory / checkpoints / stores）。包本身仍然无状态：数据在节点上，包里只有「我需要它」这句话",
                "properties": {
                    "kb": arr(json!({ "type": "object" })),
                    "memory": { "type": "object" },
                    "checkpoints": { "type": "object" },
                    "stores": arr(json!({ "type": "object" })),
                },
            },
            "egress": {
                "type": "object",
                "description": "出网通道声明：只有通道**名字**，没有密钥（钥进 ncc 的 vault，不进包）",
            },
            "auth": {
                "type": "object",
                "description": "授权包声明（profile=auth）。这一类要连密钥一起建，用 `ncc auth pkg init` 生成，不是手写",
            },
            "security": {
                "type": "object",
                "description": "执行策略与沙箱限额声明（profile=agent 等可执行包用）",
                "properties": {
                    "policy": { "type": "string" },
                    "verify": { "type": "object" },
                    "exec": { "type": "object" },
                    "entry": { "type": "string" },
                    "sandbox": { "type": "object" },
                    "remote": { "type": "object" },
                },
            },
        },
        // 扩展位（形状之外的信息，编辑器忽略、SDK 有用）
        "x-hur-profiles": profiles,
        "x-hur-authorable-kinds": profile::AUTHORABLE_KINDS,
        "x-hur-artifact": {
            "name": "<id>-<version>.<profile>.hur[.gz]",
            "note": "规范段（profile）+ 容器段（gz）；文件名是线索，清单才是权威 —— 改名不改变它是什么",
        },
        "x-hur-checks": {
            "how": "ncc hur verify .",
            "authority": "Rust 里的 spec::validate（R1~R12）就是唯一实现，本 schema 不复制规则文案",
        },
    })
}

/// 把 schema 与编辑器接线写进工程目录（`ncc hur init` 与 `ncc hur schema --write` 共用）。
///
/// 返回落盘的文件；**已有配置只并、不覆盖**：`json.schemas` 里可能还有别人的 schema，
/// 而 `.vscode/settings.json` 是允许注释的（JSONC），解析不了就原样留着并报一句，
/// 绝不把它冲成一份空文件。
pub fn write_editor_wiring(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let settings = dir.join(EDITOR_SETTINGS);
    let url = format!("./{SCHEMA_FILE}");
    let mut doc = json!({});
    if settings.is_file() {
        let text = std::fs::read_to_string(&settings).map_err(|e| format!("读不了 {}：{e}", EDITOR_SETTINGS))?;
        doc = serde_json::from_str(&text).map_err(|_| {
            format!(
                "{EDITOR_SETTINGS} 不是合法 JSON（VS Code 允许注释，解析器不允许）—— 已跳过，没有覆盖它：\
                 要么手动把它加上 `\"json.schemas\": [{{\"fileMatch\": [\"/hur.json\"], \"url\": \"{url}\"}}]`，要么删掉它再跑一次"
            )
        })?;
    }
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| format!("{EDITOR_SETTINGS} 顶层不是 JSON 对象 —— 已跳过，没有覆盖它"))?;
    let list = obj.entry("json.schemas").or_insert_with(|| json!([]));
    let arr = list
        .as_array_mut()
        .ok_or_else(|| format!("{EDITOR_SETTINGS} 里的 json.schemas 不是数组 —— 已跳过，没有覆盖它"))?;
    if !arr.iter().any(|v| v.get("url").and_then(|u| u.as_str()) == Some(url.as_str())) {
        arr.push(json!({ "fileMatch": ["/hur.json"], "url": url }));
    }

    let mut written = Vec::new();
    let schema_path = dir.join(SCHEMA_FILE);
    let body = serde_json::to_string_pretty(&json_schema()).map_err(|e| e.to_string())?;
    std::fs::write(&schema_path, format!("{body}\n")).map_err(|e| format!("写不了 {SCHEMA_FILE}：{e}"))?;
    written.push(schema_path);
    if settings.parent().map(|p| !p.is_dir()).unwrap_or(false) {
        std::fs::create_dir_all(settings.parent().unwrap()).map_err(|e| format!("建不了 {EDITOR_DIR}：{e}"))?;
    }
    let body = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&settings, format!("{body}\n")).map_err(|e| format!("写不了 {EDITOR_SETTINGS}：{e}"))?;
    written.push(settings);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{AgentSpec, Deps, HurPackage, Permissions, PublishInfo};

    /// 一个**每个字段都填上**的清单（用来发现"schema 漏了字段"）。
    fn full_pkg() -> HurPackage {
        HurPackage {
            spec: PKG_SPEC.to_string(),
            kind: "agent".into(),
            profile: Some("agent".into()),
            id: "A-generic-demo-123456".into(),
            name: "Demo".into(),
            version: "0.1.0".into(),
            short: "D".into(),
            domain: "generic".into(),
            summary: "s".into(),
            entry: "src/agent.ts".into(),
            runtime: "local-v0".into(),
            capabilities: vec!["reply".into()],
            deps: Deps::default(),
            permissions: Permissions::default(),
            publish: PublishInfo::default(),
            agent: Some(AgentSpec::default()),
            data: Some(Default::default()),
            state: Some(Default::default()),
            egress: Some(Default::default()),
            auth: Some(Default::default()),
            security: Some(Default::default()),
        }
    }

    #[test]
    fn schema_covers_every_manifest_field() {
        let schema = json_schema();
        let props = schema["properties"].as_object().expect("要有 properties");
        let manifest = serde_json::to_value(full_pkg()).unwrap();
        for key in manifest.as_object().unwrap().keys() {
            assert!(
                props.contains_key(key),
                "清单字段 `{key}` 不在 schema 里 —— 加了字段就要加 schema，\
                 否则编辑器会把合法清单标红（而 SDK 会以为没这个字段）"
            );
        }
        for key in schema["required"].as_array().unwrap() {
            let k = key.as_str().unwrap();
            assert!(props.contains_key(k), "required 里的 {k} 不在 properties 里");
        }
    }

    #[test]
    fn schema_lists_every_profile_and_authoring_kind() {
        let schema = json_schema();
        let names: Vec<String> = schema["x-hur-profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, profile::names());
        // 可编辑的 kind 与 profile 表对得上（别出现"能选但生成不了"）
        for k in schema["x-hur-authorable-kinds"].as_array().unwrap() {
            let k = k.as_str().unwrap();
            assert!(profile::is_authorable_kind(k));
            assert!(profile::get(profile::from_kind(k)).is_some());
        }
    }

    #[test]
    fn editor_wiring_merges_instead_of_clobbering() {
        let dir = std::env::temp_dir().join(format!("hur-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(EDITOR_DIR)).unwrap();
        // 别人已经配过 schema：要被保留
        std::fs::write(
            dir.join(EDITOR_SETTINGS),
            "{\"json.schemas\":[{\"url\":\"./other.json\"}],\"editor.tabSize\":2}\n",
        )
        .unwrap();
        write_editor_wiring(&dir).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(EDITOR_SETTINGS)).unwrap()).unwrap();
        let arr = doc["json.schemas"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "要并进去，不是覆盖：{doc}");
        assert_eq!(doc["editor.tabSize"], 2, "别人的设置要留着");
        assert!(dir.join(SCHEMA_FILE).is_file());
        // 不是合法 JSON（带注释）时不覆盖，只报错
        std::fs::write(dir.join(EDITOR_SETTINGS), "{\n  // 注释\n}\n").unwrap();
        let before = std::fs::read_to_string(dir.join(EDITOR_SETTINGS)).unwrap();
        assert!(write_editor_wiring(&dir).is_err());
        assert_eq!(std::fs::read_to_string(dir.join(EDITOR_SETTINGS)).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
