//! profile：这份包**是什么**。
//!
//! 为什么要有这一层（别把 `kind` 当它用）：`kind: agent|harness|repo` 只有三个值，
//! 而实际要装的东西早就超了 —— 一个可自部署的 AI 应用、一个宿主插件、一份 MCP 声明、
//! 一份知识库快照、一个轨迹数据集，它们**共用同一套封装**（清单 + 锁 + 确定性字节 + 签名），
//! 但**必填项、规则集、能不能执行、怎么接进宿主**完全不同。
//!
//! 所以职责切开：
//!
//! * **pack**（`pack.rs`）与内容无关：确定性字节 + 摘要 + 签名 + 防穿越解包。它不认识 profile。
//! * **profile**（本模块）：决定**校验什么**、**能不能跑**、**怎么集成**、**按什么匹配**。
//!
//! 一句话：**profile 的价值在约束，不在宽容**。数据类 profile 明确**禁止** `entry` 与
//! `permissions.network` —— 一份知识库快照不该能跑代码、也不该自己出网。若把必填项都做成
//! 可选，校验就退化成"文件能不能解开"，那这个格式就什么都没说。
//!
//! ⚠️ 与目录 `kind` 的关系（两套命名，别混）：目录 kind 是**检索用的粗分类**
//! （`api/harness/hur/skill/mcp/plugin/scaffold/...`），profile 是**规范的精确身份**。
//! [`registry_kinds`] 给出确定映射；`--kind` 仍可覆盖，但**清单里的 profile 才是权威**
//! （`manifest.profile`，`ncc hur profile` 读的就是它）。

/// 一份 profile 的规范：它是什么、要什么、能不能执行、怎么接、按什么匹配。
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// 规范名（写进 `hur.json` 的 `profile` 字段）
    pub name: &'static str,
    /// 一句话：这是什么
    pub summary: &'static str,
    /// 能不能执行（`ncc hur run --exec` / 宿主加载）
    pub executable: bool,
    /// 是不是**数据快照**（不可变、不该可执行 —— 见模块头）
    pub data: bool,
    /// 目录会把它记成哪个 registry kind（`--kind` 可覆盖；权威是清单里的 profile）
    pub registry_kinds: &'static [&'static str],
    /// 集成宿主（`ncc hur interop` 的目标；空 = 不渲染宿主产物）
    pub hosts: &'static [&'static str],
    /// 这个 profile **额外**要求什么（人读；机器判定在 `spec::validate` 的 R12）
    pub requires: &'static [&'static str],
    /// 这个 profile **禁止**什么（同上）
    pub forbids: &'static [&'static str],
    /// 按什么维度匹配（`ncc hur match --by`；人读）
    pub match_by: &'static str,
}

/// 规范里的一等 profile。加一个就要同步 `spec::validate` 的 R12 与 `profile smokes`。
pub const PROFILES: [Profile; 11] = [
    Profile {
        name: "agent",
        summary: "声明式 Agent：system prompt + 工具面 + 技能，装进宿主或本机沙箱",
        executable: true,
        data: false,
        registry_kinds: &["hur"],
        hosts: &["claude", "cursor", "cline", "codex", "mcp"],
        requires: &["entry", "agent（至少 system_prompt 或 persona）"],
        forbids: &[],
        match_by: "capabilities · agent.tools · agent.skills",
    },
    Profile {
        name: "harness",
        summary: "可加载能力包：loader/entry 契约交给 runtime 加载",
        executable: true,
        data: false,
        registry_kinds: &["harness"],
        hosts: &["mcp"],
        requires: &["entry"],
        forbids: &[],
        match_by: "capabilities · loader",
    },
    Profile {
        name: "plugin",
        summary: "宿主插件：声明它接进哪些宿主、提供什么",
        executable: true,
        data: false,
        registry_kinds: &["plugin"],
        hosts: &["claude", "cursor", "cline", "codex", "mcp"],
        requires: &["entry", "两个宿主名（interop 渲染不出来就没法接）"],
        forbids: &[],
        match_by: "hosts · capabilities",
    },
    Profile {
        name: "mcp",
        summary: "MCP server 声明：command/args/env 名（**不含任何值**）",
        executable: true,
        data: false,
        registry_kinds: &["mcp"],
        hosts: &["mcp"],
        requires: &["entry"],
        forbids: &["把凭据写进 args/env 值（凭据不进包）"],
        match_by: "capabilities",
    },
    Profile {
        name: "app",
        summary: "可自部署 AI 应用（NCC 舱）：一条命令起得来，且说清要什么数据、出什么网",
        executable: true,
        data: false,
        registry_kinds: &["hur"],
        hosts: &[],
        requires: &["entry", "state（要哪些数据）", "egress（出什么网）"],
        forbids: &[],
        match_by: "capabilities · state · egress",
    },
    Profile {
        name: "scaffold",
        summary: "工程脚手架：生成一个可用的起点，不是可运行的东西",
        executable: false,
        data: false,
        registry_kinds: &["scaffold"],
        hosts: &[],
        requires: &["至少一个模板文件"],
        forbids: &[],
        match_by: "capabilities · tags",
    },
    Profile {
        name: "skill",
        summary: "一份或多份 SKILL.md：宿主直接读，不含代码入口",
        executable: false,
        data: false,
        registry_kinds: &["skill"],
        hosts: &["claude", "cursor", "cline", "codex"],
        requires: &["skills/ 目录下至少一份技能文件"],
        forbids: &["entry（技能不是程序）"],
        match_by: "技能名 · tags",
    },
    Profile {
        name: "kb-seed",
        summary: "知识库**快照**：不可变、可签名、可灌进节点",
        executable: false,
        data: true,
        registry_kinds: &["hur"],
        hosts: &[],
        requires: &["data.source / snapshotAt / privacy", "data.docs[].path 必须真的在包里"],
        forbids: &["entry", "permissions.network（数据不出网）", "privacy=public 却带 private 文档"],
        match_by: "tags · data.source",
    },
    Profile {
        name: "mem-seed",
        summary: "记忆**快照**：按键导出的一份不可变副本（活记忆永远留在节点）",
        executable: false,
        data: true,
        registry_kinds: &["hur"],
        hosts: &[],
        requires: &["data.source / snapshotAt / privacy", "每条记忆带 subject 与 key"],
        forbids: &["entry", "permissions.network", "privacy=public（记忆默认私密）"],
        match_by: "tags · data.source",
    },
    Profile {
        name: "ckpt-set",
        summary: "检查点集合：字节 + 血缘，天然不可变",
        executable: false,
        data: true,
        registry_kinds: &["hur"],
        hosts: &[],
        requires: &["data.source / snapshotAt / privacy", "每个点带 digest 对应关系"],
        forbids: &["entry", "permissions.network"],
        match_by: "label · tags · data.source",
    },
    Profile {
        name: "trace-set",
        summary: "运行轨迹数据集：**默认只带摘要**，带载荷要显式声明",
        executable: false,
        data: true,
        registry_kinds: &["hur"],
        hosts: &[],
        requires: &["data.source / snapshotAt / privacy", "data.payload 策略（digest|preview|full）"],
        forbids: &["entry", "permissions.network", "payload=full 却把 privacy 写成 public"],
        match_by: "kind · tags · data.source",
    },
];

/// 按名字取 profile。
pub fn get(name: &str) -> Option<&'static Profile> {
    let n = name.trim();
    PROFILES.iter().find(|p| p.name == n)
}

/// 所有 profile 名（顺序稳定，用于 help / 校验）。
pub fn names() -> Vec<&'static str> {
    PROFILES.iter().map(|p| p.name).collect()
}

/// 缺 `profile` 的老包按 `kind` 推导 —— **向后兼容**，老包不会因为没写 profile 就红。
///
/// `repo`（脚手架）过去同时能当"可部署骨架"用，所以推导成 `scaffold` 而不是 `app`：
/// 宁可少说，不要替发布者把它说成"一条命令能起"。
pub fn from_kind(kind: &str) -> &'static str {
    match kind {
        "agent" => "agent",
        "harness" => "harness",
        "repo" => "scaffold",
        _ => "harness",
    }
}

/// 这份包声明的 profile 名（缺省按 kind 推导）。
pub fn of(pkg_profile: Option<&str>, kind: &str) -> &'static str {
    match pkg_profile.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => get(p).map(|x| x.name).unwrap_or("harness"),
        None => from_kind(kind),
    }
}

/// 数据类 profile（快照）：不可执行、只读、带隐私级别。
pub fn is_data(name: &str) -> bool {
    get(name).map(|p| p.data).unwrap_or(false)
}

/// 可执行类 profile。
pub fn is_executable(name: &str) -> bool {
    get(name).map(|p| p.executable).unwrap_or(false)
}

/// 默认映射：profile → 目录 kind。
///
/// 这是**规范的一部分**，不是实现细节 —— 不然目录里会出现"明明是 MCP 声明，检索时
/// 只显示 hur"这种事（过去的 `agent|repo → hur` 就是写死在 `publish.rs` 里的）。
/// 需要另立门户时用 `--kind` 覆盖，但清单里的 `profile` 仍是权威。
pub fn registry_kinds(name: &str) -> &'static [&'static str] {
    get(name).map(|p| p.registry_kinds).unwrap_or(&["hur"])
}

/// 一对人读的清单，`ncc hur profile --list` 用。
pub fn table() -> Vec<(String, String)> {
    PROFILES
        .iter()
        .map(|p| {
            let tag = if p.data {
                "数据快照（不可执行）".to_string()
            } else if p.executable {
                "可执行".to_string()
            } else {
                "只读".to_string()
            };
            (p.name.to_string(), format!("{tag} · {}", p.summary))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_is_self_consistent() {
        for p in PROFILES.iter() {
            assert!(!p.name.is_empty() && p.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "profile 名要用小写与连字符：{}", p.name);
            assert!(!p.summary.is_empty(), "{} 缺 summary", p.name);
            assert!(!p.registry_kinds.is_empty(), "{} 没给目录 kind 映射", p.name);
            // 数据快照**不许**可执行 —— 这是这个格式敢往外发的前提
            if p.data {
                assert!(!p.executable, "{} 是数据快照，不能同时可执行", p.name);
                assert!(
                    p.forbids.iter().any(|f| f.contains("entry")),
                    "{} 必须明确禁止 entry",
                    p.name
                );
            }
        }
    }

    #[test]
    fn data_profiles_forbid_network() {
        for p in PROFILES.iter().filter(|p| p.data) {
            assert!(
                p.forbids.iter().any(|f| f.contains("permissions.network")),
                "{} 必须禁止 permissions.network（数据包不该自己出网）",
                p.name
            );
        }
    }

    #[test]
    fn lookup_and_legacy_derivation() {
        assert_eq!(get("kb-seed").map(|p| p.name), Some("kb-seed"));
        assert!(get("nope").is_none());
        // 老包的 kind 推导：宁少说，不多说
        assert_eq!(of(None, "agent"), "agent");
        assert_eq!(of(None, "harness"), "harness");
        assert_eq!(of(None, "repo"), "scaffold");
        assert_eq!(of(Some("  "), "repo"), "scaffold");
        // 写了 profile 就以它为准（未知值不乱认，退回 harness）
        assert_eq!(of(Some("trace-set"), "agent"), "trace-set");
        assert_eq!(of(Some("bogus"), "agent"), "harness");
        assert!(is_data("kb-seed") && !is_data("agent"));
        assert!(is_executable("plugin") && !is_executable("skill"));
        assert_eq!(registry_kinds("plugin"), ["plugin"]);
        assert_eq!(registry_kinds("ckpt-set"), ["hur"]);
        assert_eq!(names().len(), PROFILES.len());
        assert_eq!(table().len(), PROFILES.len());
    }
}
