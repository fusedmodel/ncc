//! 包规范 `harness-use-package/v1`：模型、解析、校验。
//!
//! 这是「固化」的核心：`hur.json` 是唯一清单，GUI（创作中心）与 CLI（hur）都读它。
//! 校验规则与前端 `agent/src/agent/hurpack.ts` 保持一致（同一份规则两处实现，规则编号 R1~R7）。

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PKG_SPEC: &str = "harness-use-package/v1";
pub const LOCK_SPEC: &str = "harness-use-lock/v1";
pub const MANIFEST: &str = "hur.json";
pub const LOCK: &str = "hur.lock";
pub const DIST: &str = "dist";

// 对外（内置到宿主程序时）更好读的别名，值与上面一致
pub const HUR_SPEC: &str = PKG_SPEC;
pub const HUR_MANIFEST: &str = MANIFEST;
pub const HUR_DIST: &str = DIST;

// —— `.huf`：面向用户的**资源包**（Harness-Use Files）——
//
// 与 `.hur` **同容器**（gzip + zip + 确定性字节 + 侧车签名），只有清单名与规范号不同：
// `.hur` 是**运行时的包**（有 `entry`，能跑）；`.huf` 是**给用户的文件**（文档 / 提示词 /
// 模板 / 静态资产 / 技能文本），**没有入口、不能执行**。两条边界都是硬拦（见 `crate::huf`）：
// 资源包里出现 `entry`/`runtime` → 报错"改用 .hur"；反过来 `ncc hur verify` 也不认 `.huf`。
// 这样"这个文件能不能跑"不用打开看内容，看扩展名就知道。
pub const HUF_SPEC: &str = "harness-use-files/v1";
pub const HUF_LOCK_SPEC: &str = "harness-use-files-lock/v1";
pub const HUF_MANIFEST: &str = "huf.json";
pub const HUF_LOCK: &str = "huf.lock";

/// 资源包的内容目录：**没有 `src/`** —— 那是代码的落点，属于 `.hur`。
pub const HUF_CONTENT_DIRS: [&str; 5] = ["docs", "assets", "skills", "kb", "data"];

/// 资源包的 `kind` 只有一个值（与目录里的 registry kind `huf` 对应）
pub const HUF_KIND: &str = "files";

/// 容器格式：**同一套容器，两个清单**。
///
/// 所有与"包长什么样"有关的名字都从这里取（清单名 / 锁名 / 规范号 / 内容目录 / 扩展名），
/// 免得到处写 `if huf {…} else {…}` 那种两套实现 —— 包字节的确定性、防穿越解包、签名的
/// 覆盖面这些**不该有两份**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `.hur` —— 可执行的运行时包
    Hur,
    /// `.huf` —— 面向用户的资源包（不执行）
    Huf,
}

impl Format {
    pub fn manifest(self) -> &'static str {
        match self {
            Format::Hur => MANIFEST,
            Format::Huf => HUF_MANIFEST,
        }
    }

    pub fn lock(self) -> &'static str {
        match self {
            Format::Hur => LOCK,
            Format::Huf => HUF_LOCK,
        }
    }

    pub fn lock_spec(self) -> &'static str {
        match self {
            Format::Hur => LOCK_SPEC,
            Format::Huf => HUF_LOCK_SPEC,
        }
    }

    pub fn pkg_spec(self) -> &'static str {
        match self {
            Format::Hur => PKG_SPEC,
            Format::Huf => HUF_SPEC,
        }
    }

    /// 产物名字里的规范段（`.hur` / `.huf`）
    pub fn spec_ext(self) -> &'static str {
        match self {
            Format::Hur => ARTIFACT_SPEC_EXT,
            Format::Huf => ARTIFACT_HUF_EXT,
        }
    }

    pub fn content_dirs(self) -> &'static [&'static str] {
        match self {
            Format::Hur => &CONTENT_DIRS,
            Format::Huf => &HUF_CONTENT_DIRS,
        }
    }

    /// 人读的名字（报错文案用）
    pub fn label(self) -> &'static str {
        match self {
            Format::Hur => "hur 包",
            Format::Huf => "huf 资源包",
        }
    }
}

/// 包内容目录（打包时按此顺序收集，保证可复现）
///
/// `data/` 是**数据快照**的落点（`profile=kb-seed|mem-seed|ckpt-set|trace-set`）；
/// `kb/` 保持原义：随包走的本地知识库文件（`permissions.local: ["kb"]`）。
/// 两者不是一回事：`kb/` 是能力的一部分，`data/` 是**一次快照**。
pub const CONTENT_DIRS: [&str; 5] = ["src", "skills", "kb", "data", "assets"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Permissions {
    /// 允许访问的域名（支持 `*.example.com` 通配；localhost / 127.0.0.1 始终允许）
    #[serde(default)]
    pub network: Vec<String>,
    /// 允许访问的本机资源：kb / files / packages
    #[serde(default)]
    pub local: Vec<String>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { network: Vec::new(), local: Vec::new() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Deps {
    #[serde(default)]
    pub harness: Vec<String>,
    #[serde(default)]
    pub agent: Vec<String>,
    #[serde(default)]
    pub skill: Vec<String>,
    #[serde(default)]
    pub kb: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<String>,
}

impl Deps {
    /// 所有依赖（kind, ref）
    pub fn iter(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::new();
        for (k, list) in [
            ("harness", &self.harness),
            ("agent", &self.agent),
            ("skill", &self.skill),
            ("kb", &self.kb),
            ("mcp", &self.mcp),
        ] {
            for r in list {
                out.push((k, r.as_str()));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishInfo {
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default)]
    pub slug: String,
}

/// `kind=agent` 的 **Agent 声明**（PRD §9.2）：把 Agent 定义从代码提升到声明，
/// 于是可静态校验、CLI/GUI/平台共读、**不需要执行第三方代码**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentSpec {
    /// 注入 chatbot 的 system prompt（Agent 的「大脑」）
    #[serde(default)]
    pub system_prompt: String,
    /// 人设约束（叠加在本机 agent.persona 之后）
    #[serde(default)]
    pub persona: String,
    /// 允许调用的本机工具（超出即拒；空 = 不限制，兼容旧包）
    #[serde(default)]
    pub tools: Vec<String>,
    /// 包内技能文件（相对路径，必须存在）
    #[serde(default)]
    pub skills: Vec<String>,
    /// 管线开关/权重（自由结构：intent / discover / rank / …）
    #[serde(default)]
    pub pipeline: Option<serde_json::Value>,
    /// 输出约束（自由结构：max_reply_chars / no_tool_calls / …）
    #[serde(default)]
    pub guard: Option<serde_json::Value>,
    /// 声明「这个包打算在哪些宿主里用」（claude/cursor/cline/codex/mcp）。
    /// 只是意图声明：`hur export` 不依赖它，但写全了 `hur verify` 会更早发现笔误。
    #[serde(default)]
    pub adapters: Vec<String>,
}

impl HurPackage {
    /// 这份包的 profile 名（缺省按 `kind` 推导）。
    pub fn profile_name(&self) -> &'static str {
        crate::profile::of(self.profile.as_deref(), &self.kind)
    }

    /// 是不是数据快照包（不可执行、只读）。
    pub fn is_data(&self) -> bool {
        crate::profile::is_data(self.profile_name())
    }
}

/// 产物文件名：`<id>-<version>.<profile>.hur.gz`。
///
/// 为什么把 profile 放进**文件名**：一个 `dist/` 里可能躺着几十个包，
/// "这是能跑的包、还是一份数据快照"应当一眼看得出来 —— 这不是安全问题
/// （真正的身份在清单里，R12 按清单判），而是**别让人把一份轨迹数据当 app 发出去**。
///
/// 为什么末尾还跟一个容器后缀（用户口径：**hur 只是一种规范**，文件本身用通用压缩格式结尾）：
/// `.hur` 回答"这是 HUR 规范产物"，`.gz` 回答"外面这层是什么容器" —— 于是
/// `file`、双击、`gunzip`、编辑器、IM 预览都认得出它，不用先知道 hur 是什么。
/// 侧车（`.minisig` / `.sha256`）继续往完整路径后面追加，所以不受影响。
///
/// ⚠️ 名字**只是线索**：任何解析都必须读清单，不许靠点号切文件名 ——
/// `valid_id` 本来就允许 id 里带 `.`。
///
/// ⚠️ **换容器只改 [`ARTIFACT_CONTAINER_EXT`]**（gzip → 别的时连同 `pack.rs` 一起改）。
pub const ARTIFACT_CONTAINER_EXT: &str = "gz";

/// 产物名字里的规范段（`.hur`）：这是"HUR 规范产物"的标记，容器段跟在它后面。
pub const ARTIFACT_SPEC_EXT: &str = "hur";

/// 资源包的规范段（`.huf`）：与 `.hur` 同一个容器，只是清单与内容约束不同。
pub const ARTIFACT_HUF_EXT: &str = "huf";

pub fn artifact_name(pkg: &HurPackage) -> String {
    format!(
        "{}-{}.{}.{}.{}",
        pkg.id,
        pkg.version,
        pkg.profile_name(),
        ARTIFACT_SPEC_EXT,
        ARTIFACT_CONTAINER_EXT
    )
}

/// 资源包的产物名：`<id>-<version>.huf.gz`。
///
/// 不写 profile 段：`.huf` 只有一种身份（资源包），再叠一层名字只会让人以为还有别的。
/// id 里的 `/` 换成 `_` —— 它同时是文件名，不能让 `@org/docs/x` 变成一个子目录。
pub fn huf_artifact_name(id: &str, version: &str) -> String {
    format!(
        "{}-{}.{}.{}",
        id.replace('/', "_"),
        version,
        ARTIFACT_HUF_EXT,
        ARTIFACT_CONTAINER_EXT
    )
}

/// 这个路径像不像**打包产物**（而不是工程目录）：`….hur` / `….hur.gz` / `….huf` / `….huf.gz`。
///
/// 为什么按名字判而不是"是不是文件"：`ncc hur verify <产物>` 与 `<工程目录>` 是两条路，
/// 判错的代价是拿一份 `SKILL.md` 去当包解（报一句莫名其妙的 zip 错）。
/// 老包（加容器后缀之前打的 `.hur`）一律还算产物 —— 已发出去的字节不该因为改名失宠。
pub fn is_archive_path(p: &Path) -> bool {
    let name = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    is_archive_name(&name)
}

/// 产物名判定（`is_archive_path` 与 `format_of_archive` 共用）
pub fn is_archive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [ARTIFACT_SPEC_EXT, ARTIFACT_HUF_EXT].iter().any(|e| {
        name.ends_with(&format!(".{e}")) || name.ends_with(&format!(".{e}.{ARTIFACT_CONTAINER_EXT}"))
    })
}

/// 从产物名认出它是哪种包（`x.huf.gz` → `Huf`）。认不出来按 `.hur` 算：
/// **老包永远要能装**，而 `.hur` 是绝大多数 —— 猜错的代价只是解包时给出的拒绝理由差一句。
pub fn format_of_archive(p: &Path) -> Format {
    let raw = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let stem = raw.strip_suffix(&format!(".{ARTIFACT_CONTAINER_EXT}")).unwrap_or(&raw);
    if stem.ends_with(&format!(".{ARTIFACT_HUF_EXT}")) {
        Format::Huf
    } else {
        Format::Hur
    }
}

/// 产物的候选文件名：**新名字在前，老名字兜底**。
///
/// 为什么要兜底：`dist/` 里可能还躺着两个时代的产物 —— 加容器后缀之前的
/// `<id>-<version>.<profile>.hur`，以及连 profile 段都还没加的 `<id>-<version>.hur`。
/// 因为"名字格式变了"就说"找不到产物"，那是在惩罚用户什么都没做错的事。
pub fn artifact_candidates(pkg: &HurPackage) -> Vec<String> {
    vec![
        artifact_name(pkg),
        format!("{}-{}.{}.hur", pkg.id, pkg.version, pkg.profile_name()),
        format!("{}-{}.hur", pkg.id, pkg.version),
    ]
}

/// 在 `dir`（一般是 `dist/`）里找这个包的产物：先新名字，再老名字。
pub fn find_artifact(dir: &Path, pkg: &HurPackage) -> Option<PathBuf> {
    artifact_candidates(pkg)
        .into_iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// 从文件名里认出 profile 那一段（`demo.kb-seed.hur.gz` → `kb-seed`）。
///
/// 两个后缀都要先摘掉：容器段（`.gz`）与规范段（`.hur`）都不是 profile 段。
/// 只认**规范里的 profile 名**：老名字（`H-demo-0.1.0.hur`，那一段是版本号）、
/// 人手改的、或者别的工具生成的名字一律返回 `None` —— 认不出来就说"没有线索"，
/// 不要说人家写错了。
pub fn name_profile_token(file: &str) -> Option<String> {
    let base = Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    let stem = base
        .strip_suffix(&format!(".{ARTIFACT_CONTAINER_EXT}"))
        .unwrap_or(base);
    let stem = stem.strip_suffix(&format!(".{ARTIFACT_SPEC_EXT}")).unwrap_or(stem);
    let token = stem.rsplit('.').next()?;
    crate::profile::get(token).map(|p| p.name.to_string())
}

/// 文件名与清单对不上时给一条提醒（**只提醒，不拦**）。
///
/// 为什么不拦：文件会被下载、改名、塞进压缩包、被 IM 转到别的地方 ——
/// 一个名字（而不是内容）就能让包"校验不过"，那才是错的。真正判身份的是清单。
pub fn name_mismatch_note(file: &str, pkg: &HurPackage) -> Option<Issue> {
    let token = name_profile_token(file)?;
    let real = pkg.profile_name();
    if token == real {
        return None;
    }
    Some(Issue::warn(
        "R12",
        format!("文件名叫「{token}」，但清单里是 profile={real} —— 以**清单**为准（重命名文件不会改变它是什么）"),
    ))
}

/// 数据快照声明（`data{}`）。
///
/// 为什么要有它，而不是直接把文件塞进包里：一份快照必须回答三个问题才谈得上可信 ——
/// **从哪儿来的**（`source`）、**什么时候的**（`snapshot_at`）、**能给谁看**（`privacy`）。
/// 少了这三样，包里就是一堆来历不明的字节，跟"随手拷了个目录"没区别。
///
/// ⚠️ **活状态永远不进包**：kb / mem / ckpt 是会被反复写、会持续变大、默认私有的状态，
/// 只有它们的**不可变快照**才适合被打包、签名、分发（见 `profile.rs` 模块头）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct DataDecl {
    /// 来源（`@命名空间/slug`，或 `local` 这类本机来源）
    pub source: String,
    /// 从哪个目标取的（目标名；空 = 本机）—— 出了问题要能追溯到哪台机器
    #[serde(default)]
    pub source_target: String,
    /// 快照时刻（RFC3339）。"什么时候的数据"是这份声明的一半价值。
    pub snapshot_at: String,
    /// 隐私级别：`public` | `internal` | `private`（导入方按它决定能不能进公开档）
    pub privacy: String,
    /// 许可（空 = 未声明。未声明就不该往外发 —— 校验只提醒，不替发布者拍板）
    #[serde(default)]
    pub license: String,
    /// 轨迹类专用：payload 策略（`digest` | `preview` | `full`）。默认 digest。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    #[serde(default)]
    pub note: String,
    /// 逐份文件的元数据。**摘要不写在这里** —— `hur.lock.files` 已经记了每个文件的 sha256，
    /// 两处都记迟早会不一致（单一来源原则）。
    #[serde(default)]
    pub docs: Vec<DataDoc>,
}

/// 快照里的一份文件。字段是各数据 profile 的并集（用不到的就空着）——
/// 宁可字段多一点，也不要一个"按 profile 解释的任意 JSON"（那就没法校验了）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct DataDoc {
    /// 包内相对路径（必须在 `data/` 下）
    pub path: String,
    /// kb：文档 slug
    #[serde(default)]
    pub slug: String,
    /// mem：谁的记忆
    #[serde(default)]
    pub subject: String,
    /// mem：键
    #[serde(default)]
    pub key: String,
    /// kb / trace：标题
    #[serde(default)]
    pub title: String,
    /// ckpt：点的名字
    #[serde(default)]
    pub name: String,
    /// kb / trace：种类（`doc|faq|...` / `hur-run|agent`）
    #[serde(default)]
    pub kind: String,
    /// kb：格式（`markdown|text|json|yaml`）
    #[serde(default)]
    pub format: String,
    /// ckpt：媒体类型
    #[serde(default)]
    pub media_type: String,
    /// ckpt：粒度（`episode|step|run|release|handoff|manual`）
    #[serde(default)]
    pub label: String,
    /// ckpt：序号
    #[serde(default)]
    pub step: i64,
    /// ckpt：血缘（父点 id，源包内名字）
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 导入后的可见性（受 `data.privacy` 约束：包不是 public，就不许导入成 public）
    #[serde(default)]
    pub visibility: String,
    #[serde(default)]
    pub summary: String,
    /// 自由结构（ckpt 的 `meta` 这类；别拿它绕过校验）
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

/// 合法隐私级别。
pub const DATA_PRIVACY: [&str; 3] = ["public", "internal", "private"];
/// 轨迹 payload 策略。
pub const DATA_PAYLOAD: [&str; 3] = ["digest", "preview", "full"];

/// 快照文件必须落在这下面 —— 一旦允许散布在包各处，"数据包"就没法一眼认出来。
pub const DATA_DIR: &str = "data/";

impl AgentSpec {
    pub fn is_empty(&self) -> bool {
        self.system_prompt.trim().is_empty()
            && self.persona.trim().is_empty()
            && self.tools.is_empty()
            && self.skills.is_empty()
            && self.pipeline.is_none()
            && self.guard.is_none()
            && self.adapters.is_empty()
    }
}

fn default_visibility() -> String {
    "public".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HurPackage {
    pub spec: String,
    pub kind: String,
    /// 这份包**是什么**（`crate::profile::PROFILES` 里的名字）。
    ///
    /// 可省：老包没写就按 `kind` 推导（`crate::profile::from_kind`）—— **向后兼容**，
    /// 不加这个字段不会让任何既有包变红。写了就是权威（`ncc hur profile` 读它）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub short: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub deps: Deps,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub publish: PublishInfo,
    /// `kind=agent` 的 Agent 声明（其它 kind 可省略）
    #[serde(default)]
    pub agent: Option<AgentSpec>,
    /// **数据快照声明**（`data{}`）：只有数据类 profile 才有，也只允许它们有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<DataDecl>,
    /// 状态声明（`state{}`）：这个包**需要哪些知识库 / 记忆 / 检查点**。
    ///
    /// 关键区分：**包本身仍然是无状态的** —— kb / mem / ckpt 存在节点上，
    /// 包里只有"我需要它们"这句话。这样"这个 Agent 用哪些数据"是可签名、可分发、
    /// 可核对的一句话，而不是散在文档里。
    ///
    /// ⚠️ 与 `permissions.local` 里的 `kb` **不是一件事**：
    ///   本地 kb = 包目录里随包走的文件（本机读）；
    ///   这里的 kb = **节点托管**的知识库（@命名空间/slug，要联网读写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateDecl>,
    /// 安全策略声明（R8）：本包希望用哪套策略 + 进一步收紧自己
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<crate::policy::SecurityReq>,
    /// 出口声明（R10）：本包可以充当哪几条「出网通道」。
    /// 网关的 accept 路由必须由它**背书**（配置只能比声明更窄）—— 见 `egress_covers`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<Egress>,
    /// **授权包声明**（`auth{}`）：只有 `profile=auth` 才有，也只允许它有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthDecl>,
}

/// 授权包声明（`auth{}`，`profile=auth`）。
///
/// 一份授权包 = **一段可以被 ncc 开锁的密文** + 一眼可见的元数据。设计上只做两件事：
///
///  1. **包里只允许元数据明文**：项目名、条目名、过期时间、密钥包法、迭代次数。
///     值一律在 `auth/vault.enc` 里（AEAD 密文）。
///  2. **密文被清单钉住**：`vault_sha256` 写进 `hur.json`，而签名覆盖 `hur.json` ——
///     于是「换掉密文」必须连清单一起改，签名就挂不住了。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthDecl {
    /// 这份包里有哪几个项目（每个项目一份密钥文件 + 一段独立密文）
    pub projects: Vec<String>,
    /// 解锁材料的包法：`passphrase` / `identity`（可以同时有）
    #[serde(default)]
    pub wrap: Vec<String>,
    /// 允许打开这份包的身份公钥指纹（`SHA256:…`）；空 = 不限制身份（只认口令）
    #[serde(default)]
    pub allow: Vec<String>,
    /// 口令 KDF 的迭代次数（写进包，好让第三方一眼看到强度）
    #[serde(default)]
    pub kdf_iters: u64,
    /// `auth/vault.enc` 的 sha256（把密文钉在清单里）
    #[serde(default)]
    pub vault_sha256: String,
    #[serde(default)]
    pub created_at_unix: u64,
    /// 给谁用的 / 干什么的（人读）
    #[serde(default)]
    pub note: String,
}

/// 出口声明（`egress{}`）：本包声明自己可以充当哪几条「出网通道」。
///
/// ⚠️ **只有声明，没有凭据** —— 包要能被签名、分发、公开检索，密钥永远不进包。
/// `inject` 里只有**请求头名**，值由运行者（网关）的本地配置提供。
///
/// 为什么值得做成清单里的声明：这样「提供出口」不再是一份手写 JSON，而是
/// 一份**可签名、可分发、可被第三方核对**的包声明（S2b，PRD §16.3 的硬约束 3）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Egress {
    #[serde(default)]
    pub provides: Vec<EgressRoute>,
}

impl Egress {
    pub fn is_empty(&self) -> bool {
        self.provides.is_empty()
    }
}

/// 一条出口通道声明。
///
/// 两种写法：
///   · **单供应商**（老写法）：直接写 `target` / `inject`；
///   · **多供应商**：写 `providers[]`，每个 supplier 一个上游与凭据头名，
///     网关可以在一组**已声明**的供应商之间切换（`ncc gateway switch`）。
/// 两种写法**不能同时用**（两份真相），R10 会拦。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EgressRoute {
    /// 路由名（出现在网关 URL 里：`/v1/<name>/<路径>`）
    #[serde(default)]
    pub name: String,
    /// 上游基址（单供应商写法）。必须 `https://`；`http://` 只允许 loopback（本地联调）
    #[serde(default)]
    pub target: String,
    /// 允许的上游路径（**精确匹配**，不带查询串）
    #[serde(default)]
    pub paths: Vec<String>,
    /// 允许的方法（如 POST）
    #[serde(default)]
    pub methods: Vec<String>,
    /// 运行者需要注入的**请求头名**（只有名字，没有值）
    #[serde(default)]
    pub inject: Vec<String>,
    /// 多供应商写法：同一套 paths/methods 下的多个上游，网关可在它们之间切换。
    /// 非空时 `target` 必须为空。
    #[serde(default)]
    pub providers: Vec<EgressProvider>,
}

/// 出口里的**一个供应商**。
///
/// 为什么要有它："这条出口用哪家 LLM"是运营决定，不是包的一部分 —— 但**可选范围**必须是
/// 声明的一部分（否则网关的本地配置又能凭空多出一个没人能核对的上游）。
/// 于是：包里声明「允许这几家」，网关本地配置只能从这几家里挑一家当 active。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EgressProvider {
    /// 供应商名（网关 `switch` 用它；出现在 URL 里，所以字符集与路由名一样）
    #[serde(default)]
    pub name: String,
    /// 上游基址
    #[serde(default)]
    pub target: String,
    /// 这一家需要注入的头名（并集在路由级 `inject` 之上；仍然只有名字）
    #[serde(default)]
    pub inject: Vec<String>,
    /// 路径重写：`{"chat/completions": "v1/messages"}` —— 路由内路径 → 这一家的实际路径。
    /// **只映射路径，不碰 body**：body 形状是厂商语义，猜错了就是静默发错请求。
    #[serde(default)]
    pub rewrite: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedDep {
    pub r#ref: String,
    pub resolved: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HurLock {
    pub spec: String,
    pub package: String,
    pub version: String,
    /// kind → 依赖锁定（**不写时间戳**，保证 pack 产物可复现）
    #[serde(default)]
    pub deps: BTreeMap<String, Vec<LockedDep>>,
    /// 包内文件 → sha256（相对路径，排序输出）
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warn,
    /// 「已核对通过」这类**证据**（既不拦人，也不进"警告算不算失败"的账）
    /// —— 需要"某项检查到底做没做"时，用它留痕（如 R9 签名核对）
    Info,
}

/// 一条检查结论（R1~R10 共用）。可序列化：CLI 的 `--json` 与 GUI 都要原样转出去。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub rule: String,
    pub level: Level,
    pub msg: String,
}

impl Issue {
    pub fn err(rule: &str, msg: impl Into<String>) -> Self {
        Self { rule: rule.into(), level: Level::Error, msg: msg.into() }
    }
    pub fn warn(rule: &str, msg: impl Into<String>) -> Self {
        Self { rule: rule.into(), level: Level::Warn, msg: msg.into() }
    }
    pub fn info(rule: &str, msg: impl Into<String>) -> Self {
        Self { rule: rule.into(), level: Level::Info, msg: msg.into() }
    }
    pub fn is_problem(&self) -> bool {
        self.level != Level::Info
    }
}

pub fn parse(text: &str) -> Result<HurPackage> {
    let pkg: HurPackage = serde_json::from_str(text).map_err(|e| anyhow!("hur.json 解析失败：{e}"))?;
    Ok(pkg)
}

pub fn read_pkg(dir: &Path) -> Result<HurPackage> {
    let p = dir.join(MANIFEST);
    if !p.exists() {
        bail!("当前目录没有 {MANIFEST}（用 `hur init` 生成，或 cd 到包目录）");
    }
    parse(&std::fs::read_to_string(&p)?)
}

pub fn read_lock(dir: &Path) -> Option<HurLock> {
    let p = dir.join(LOCK);
    if !p.exists() {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// 合法 kind
/// `hur.json` 的 `kind` 是不是已知值。
///
/// 2026-10-02：从「三件套」放宽到 **profile 同名的那几个**（skill / mcp / plugin /
/// app / scaffold）。理由不是想说好听，而是这两件事已经成立很久了：
/// ① 目录（registry）的 kind 词表里早就有 skill / mcp / plugin / scaffold；
/// ② 开发者想的是「我要做一个 skill」，而 `kind=harness` 会把这个包带进 R3（端点
///    schema）这类只对能力包成立的规矩里。
/// **三件套仍是合法值**（老包一个也不会红）；而“这份包是什么”的权威始终是 `profile`
/// —— 新规矩请按 profile 判，别再往 kind 上堆语义。
pub fn valid_kind(k: &str) -> bool {
    crate::profile::is_authorable_kind(k) || matches!(k, "agent" | "harness" | "repo")
}

pub fn kind_prefix(k: &str) -> &'static str {
    match k {
        "agent" => "A-",
        "harness" => "H-",
        _ => "",
    }
}

/// 语义化版本：`x.y.z` 可带 `-prerelease` / `+build`
pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut nums = [0u64; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums[i] = p.parse().ok()?;
    }
    Some((nums[0], nums[1], nums[2]))
}

pub fn cmp_semver(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (x, y) = (parse_semver(a)?, parse_semver(b)?);
    Some(x.cmp(&y))
}

fn valid_id(kind: &str, id: &str) -> bool {
    if id.is_empty() || id.len() > 96 {
        return false;
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@')) {
        return false;
    }
    let p = kind_prefix(kind);
    if p.is_empty() {
        // repo：id 即 slug
        !id.starts_with('-') && !id.ends_with('-')
    } else {
        id.starts_with(p)
    }
}

/// 收集包内容文件（相对路径，排序）——不含 hur.json/hur.lock（它们单独入包）
pub fn content_files(dir: &Path) -> Vec<PathBuf> {
    content_files_in(dir, &CONTENT_DIRS)
}

/// 同 [`content_files`]，但内容目录由调用方给（`.huf` 用的是 [`HUF_CONTENT_DIRS`]）
/// —— **同一套收集规则**，只有"哪些目录算内容"不同。
pub fn content_files_in(dir: &Path, dirs: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let p = dir.join(d);
        if p.is_dir() {
            stack.push(p);
        }
    }
    while let Some(p) = stack.pop() {
        let mut children: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&p) {
            for e in rd.flatten() {
                let path = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    children.push(path);
                }
            }
        }
        out.extend(children);
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                    if matches!(n, "README.md" | "LICENSE" | "LICENSE.md") {
                        out.push(p);
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn rel(dir: &Path, file: &Path) -> String {
    file.strip_prefix(dir).unwrap_or(file).to_string_lossy().replace('\\', "/")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn sha256_file(p: &Path) -> Result<String> {
    Ok(sha256_hex(&std::fs::read(p)?))
}

/// 一个已解析的 HTTP 目标。egress 声明与网关（`src/gateway.rs`）**共用同一份**解析，
/// 免得两边对“什么是合法的 target”理解不一致。
pub struct HttpTarget {
    pub scheme: String,
    /// host[:port]
    pub authority: String,
    pub host: String,
    /// 基路径，无尾斜杠（可能是空串）
    pub base: String,
}

impl HttpTarget {
    /// 拼上一条（已归一的）子路径。
    pub fn join(&self, sub: &str) -> String {
        format!("{}://{}{}/{}", self.scheme, self.authority, self.base, sub)
    }
}

/// 解析 `https://host[:port][/base]`。不接受用户信息、控制字符、空格；scheme 只认 http/https。
///
/// 基路径也走 `normalize_egress_path`（同一份规则）—— 否则 `https://host/a b`、
/// `https://host/a%2fb`、`https://host/../x` 这类写法会从这里溜进白名单比较。
pub fn parse_http_target(raw: &str) -> Option<HttpTarget> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if rest.is_empty() {
        return None;
    }
    let (authority, raw_base) = match rest.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (rest, None),
    };
    if authority.is_empty() || authority.contains(['\r', '\n', ' ', '@']) {
        return None;
    }
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority).to_string();
    if host.is_empty() || host.contains(['\r', '\n', ' ', '@', '/']) {
        return None;
    }
    let base = match raw_base {
        None => String::new(),
        Some(p) => {
            let trimmed = p.trim_matches('/');
            if trimmed.is_empty() {
                String::new()
            } else {
                normalize_egress_path(trimmed).map(|n| format!("/{n}"))?
            }
        }
    };
    Some(HttpTarget { scheme: scheme.to_string(), authority: authority.to_string(), host, base })
}

/// host 是不是本机（localhost / 127.0.0.1 / ::1）。
pub fn is_loopback_host(h: &str) -> bool {
    let h = h.trim_start_matches('[').trim_end_matches(']');
    if h == "localhost" {
        return true;
    }
    h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// egress / 网关路由名：`[a-z0-9._-]`，长度 1~64。
pub fn valid_egress_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// HTTP 头名（用于 `inject`）：字母数字与 `-`/`_`。
pub fn valid_header_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// 归一化一条 egress / 网关路径：去首尾斜杠，拒绝一切可能跑出白名单的写法。
///
/// 与网关共用（`src/gateway.rs` 直接调它）。这里**不做百分号解码**而是直接拒 `%`：
/// 解码正是 `%2e%2e` 绕过的来源，而固定路由代理的上游路径都是字面量。
/// `..` / 反斜杠 / 空段 / `?` / `#` / 空格 / 控制字符一律拒。
pub fn normalize_egress_path(p: &str) -> Option<String> {
    let p = p.trim().trim_matches('/');
    if p.is_empty() || p.len() > 512 {
        return None;
    }
    if p.contains(['%', '\\', '?', '#'])
        || p.chars().any(|c| c.is_control() || c == ' ')
        || p.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return None;
    }
    Some(p.to_string())
}

/// S2b：网关的 accept 路由是否**完全落在**某条 egress 声明的范围内。
///
/// 返回所有「配置比声明更宽」的问题（空 = 合规）。方向只有一个：
/// **配置只能比包声明更窄，不能更宽** —— 包的声明是上限。
pub fn egress_covers(
    decl: &EgressRoute,
    target: &str,
    paths: &[String],
    methods: &[String],
    inject: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    if decl.target.trim() != target.trim() {
        out.push(format!(
            "target 必须与声明一致：「{}」，配置写的是「{}」",
            decl.target.trim(),
            target.trim()
        ));
    }
    for p in paths {
        let Some(n) = normalize_egress_path(p) else {
            out.push(format!("路径「{p}」不合法"));
            continue;
        };
        if !decl.paths.iter().any(|d| normalize_egress_path(d).as_deref() == Some(n.as_str())) {
            out.push(format!(
                "路径「{p}」超出声明的范围（声明里只有：{}）",
                decl.paths.join(", ")
            ));
        }
    }
    for m in methods {
        if !decl.methods.iter().any(|d| d.eq_ignore_ascii_case(m)) {
            out.push(format!(
                "方法「{m}」不在声明里（声明里只有：{}）",
                decl.methods.join(", ")
            ));
        }
    }
    for h in inject {
        if !decl.inject.iter().any(|d| d.eq_ignore_ascii_case(h)) {
            out.push(format!(
                "注入头「{h}」不在声明里（声明里只有：{}）",
                if decl.inject.is_empty() { "（无）".to_string() } else { decl.inject.join(", ") }
            ));
        }
    }
    out
}

/// 多供应商版的 subset 判定：本地配置挑了 `decl.providers` 里的哪一家。
///
/// 规矩与单供应商完全一样（target 精确相等、paths/methods/inject 逐项子集），
/// 只是"声明的范围"换成了**被挑中的那一家**：所以网关切到 `b` 时，配置里写的东西
/// 必须被 `providers[b]` 背书 —— 不能拿 `providers[a]` 的声明去背书一个别人的目标。
///
/// `inject` 取**并集**（路由级 + 该供应商级）：有的头是这条出口共用的（如 `X-Route`），
/// 有的是某一家特有的（如 `anthropic-version`）。
pub fn egress_covers_provider(
    decl: &EgressRoute,
    provider: &str,
    target: &str,
    paths: &[String],
    methods: &[String],
    inject: &[String],
) -> Vec<String> {
    let Some(declared) = decl.providers.iter().find(|p| p.name.eq_ignore_ascii_case(provider.trim())) else {
        let names: Vec<&str> = decl.providers.iter().map(|p| p.name.as_str()).collect();
        return vec![format!(
            "这条出口没声明供应商「{}」（声明里有：{}）",
            provider.trim(),
            if names.is_empty() { "（无 —— 声明里用的是单供应商写法）".to_string() } else { names.join(", ") }
        )];
    };
    let mut out = Vec::new();
    if declared.target.trim() != target.trim() {
        out.push(format!(
            "供应商「{}」的 target 必须与声明一致：「{}」，配置写的是「{}」",
            declared.name,
            declared.target.trim(),
            target.trim()
        ));
    }
    // 路径与方法的上限是**路由级**声明（各家的 paths 子集只能是它的一部分）
    for p in paths {
        let Some(n) = normalize_egress_path(p) else {
            out.push(format!("路径「{p}」不合法"));
            continue;
        };
        if !decl.paths.iter().any(|d| normalize_egress_path(d).as_deref() == Some(n.as_str())) {
            out.push(format!("路径「{p}」超出声明的范围（声明里只有：{}）", decl.paths.join(", ")));
        }
    }
    for m in methods {
        if !decl.methods.iter().any(|d| d.eq_ignore_ascii_case(m)) {
            out.push(format!("方法「{m}」不在声明里（声明里只有：{}）", decl.methods.join(", ")));
        }
    }
    let mut allowed = decl.inject.clone();
    allowed.extend(declared.inject.iter().cloned());
    for h in inject {
        if !allowed.iter().any(|d| d.eq_ignore_ascii_case(h)) {
            out.push(format!(
                "注入头「{h}」不在供应商「{}」的声明里（可用：{}）",
                declared.name,
                if allowed.is_empty() { "（无）".to_string() } else { allowed.join(", ") }
            ));
        }
    }
    out
}

/// 状态声明（`state{}`）：这个包需要哪些**节点托管的状态**。
///
/// 为什么 kb / mem / ckpt 不做成"包"：
///   · 包是**能力**：代码 + 清单 + 确定性字节 + 签名，消费方式是"装上去跑"；
///   · 这三样是**状态**：会被反复写、会持续变大、默认私有、生命周期与包无关。
/// 所以分工是：**状态住节点，包只声明要什么**（与 `egress` 同一个思路）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateDecl {
    /// 需要的知识库（`@命名空间/slug`，或 `*` = 这个节点上我能读的任意库）。
    #[serde(default)]
    pub kb: Vec<KbRequirement>,
    /// 记忆策略（可省 = 不用记忆）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryDecl>,
    /// 检查点策略（可省 = 不打检查点）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoints: Option<CheckpointDecl>,
    /// 需要的**通用记录仓**集合（issue / log / 复盘 / 备注…）。
    ///
    /// 与 `kb` 的分工：知识库是**一类内容**（语料），这里是「还不值得单写一类的那些」——
    /// 集合是声明出来的，所以包里说清楚「我要哪个集合、什么字段、能不能改」。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stores: Vec<StoreRequirement>,
}

impl StateDecl {
    /// 什么都没声明（则不参与校验，也不影响包的任何行为）。
    pub fn is_empty(&self) -> bool {
        self.kb.is_empty()
            && self.memory.is_none()
            && self.checkpoints.is_none()
            && self.stores.is_empty()
    }
}

/// 集合的形态词表（与节点侧的声明同名）。
pub const STORE_MODES: [&str; 3] = ["read", "write", "readwrite"];
/// 集合形态：可变 / 只追加。`""` = 不关心。
pub const STORE_SHAPES: [&str; 2] = ["mutable", "append-only"];
/// 集合可见性词表（与节点侧同名）。`""` = 不关心。
pub const STORE_VISIBILITIES: [&str; 2] = ["private", "public"];

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// 一条集合需求（`state.stores[]` 的一项）。
///
/// 为什么需求放在包里、权限放在节点上：节点管的是**别人能不能**（授权），
/// 包里管的是**这个包需要什么**。两件事分开，读的人才知道
/// 一份包会在什么状态下跑起来、缺什么。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoreRequirement {
    /// 集合名（一类内容的名字：`issue` / `log` / `note` …）。
    pub collection: String,
    /// `read` | `write` | `readwrite`（空 = read）。
    #[serde(default)]
    pub mode: String,
    /// 这个集合要求有哪些字段 —— 语法与节点上同一个（`"title:string!"`
    /// / `"status:enum:open|closed"` / `"labels:string[]"`）。
    /// **写了就是约束**：声明了就得与节点上的对得上，对不上就别跑（见 R12）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// 需要按哪些字段过滤（必须是 `fields` 里声明过的字段）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index: Vec<String>,
    /// `mutable` | `append-only`（不写 = 不关心）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shape: String,
    /// `private` | `public`（不写 = 不关心）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub visibility: String,
    /// 单条上限要求（字节；0 = 不关心）。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub max_bytes: u64,
    /// 为什么要它（授权提示与排障时给人看的一句话）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

impl StoreRequirement {
    /// 归一化后的模式（空 = read）。
    pub fn mode_norm(&self) -> &str {
        let m = self.mode.trim();
        if m.is_empty() {
            "read"
        } else {
            m
        }
    }

    /// 这个声明会不会写。
    pub fn writes(&self) -> bool {
        matches!(self.mode_norm(), "write" | "readwrite")
    }
}

/// 集合名是否合法（与节点侧 `ValidCollectionKind` 同一口径）。
pub fn valid_collection_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 48 {
        return false;
    }
    if !b[0].is_ascii_lowercase() {
        return false;
    }
    b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// 一条知识库要求。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KbRequirement {
    /// `@命名空间/slug` 或 `*`。
    pub r#ref: String,
    /// `read` | `write` | `readwrite`。
    #[serde(default)]
    pub mode: String,
}

/// 记忆策略。`subject` 是"谁的记忆"（`self` 表示整个包共一份；
/// 也可以写具体的会话/角色名，同一个包里多条流水线各记各的）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryDecl {
    #[serde(default)]
    pub subject: String,
    /// 允许写的记忆种类（空 = 不限，但不推荐）。
    #[serde(default)]
    pub kinds: Vec<String>,
    /// 记忆的默认存活天数（0 = 不过期；写进条目的 TTL）。
    #[serde(default)]
    pub ttl_days: u32,
}

/// 检查点策略。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointDecl {
    /// 关掉则说清楚不要检查点（写 false 比不写更明确，所以保留这个字段）。
    #[serde(default)]
    pub enabled: bool,
    /// 打点粒度：`episode` | `step` | `run` | `release` | `handoff` | `manual`。
    #[serde(default)]
    pub label: String,
    /// 本机最多留几份（0 = 不留本地副本，只在节点上）。
    #[serde(default)]
    pub keep_local: u32,
}

/// 知识库读写模式。
pub const STATE_KB_MODES: [&str; 3] = ["read", "write", "readwrite"];
/// 记忆种类词表。
pub const STATE_MEMORY_KINDS: [&str; 5] = ["fact", "preference", "episode", "summary", "pointer"];
/// 检查点粒度词表。
pub const STATE_CKPT_LABELS: [&str; 6] = ["episode", "step", "run", "release", "handoff", "manual"];

/// 知识库引用：`*`（这个节点上能读的任意库）或 `@命名空间/slug`。
pub fn valid_kb_ref(s: &str) -> bool {
    let s = s.trim();
    if s == "*" {
        return true;
    }
    let Some(rest) = s.strip_prefix('@') else { return false };
    let Some((ns, slug)) = rest.split_once('/') else { return false };
    !ns.is_empty()
        && !slug.is_empty()
        && !slug.contains('/')
        && ns.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// 把知识库引用拆成 `(命名空间, slug)`（`*` 返回 `("*", "")`）。
pub fn split_kb_ref(s: &str) -> (String, String) {
    let s = s.trim();
    if s == "*" {
        return ("*".into(), String::new());
    }
    match s.trim_start_matches('@').split_once('/') {
        Some((ns, slug)) => (ns.to_string(), slug.to_string()),
        None => (String::new(), String::new()),
    }
}

/// R11：校验状态声明（`state{}`）。
///
/// 与 R10 同样的取向：不是判断"声明得对不对"，而是**声明本身要有边界** ——
/// 引用必须是个合法引用（否则运行时才知道找不到）、模式/种类必须在词表里（否则两边理解不一致）、
/// 声明了状态就得有网络权限（否则这个包从设计上就拿不到自己的数据）。
pub fn validate_state(pkg: &HurPackage) -> Vec<Issue> {
    let mut out = Vec::new();
    let Some(st) = pkg.state.as_ref() else { return out };
    if st.is_empty() {
        out.push(Issue::warn("R11", "state{} 存在但什么都没声明（kb / memory / checkpoints 全空）"));
        return out;
    }

    // 只在会读它的 profile 上生效：agent 与 app（舱）。
    // 这里原来是 `kind != "agent"` —— 但 app 的 state{} 就是它的主要声明之一，
    // 而 app 包的 kind 从来不是 agent（那时只能写 harness，于是自己的声明被自己警告）。
    let prof = pkg.profile_name();
    if !matches!(prof, "agent" | "app") {
        out.push(Issue::warn(
            "R11",
            format!("只有 profile=agent / app 才会读 state{{}}（当前 profile={prof}，这份声明会被忽略）"),
        ));
    }

    let mut seen: Vec<String> = Vec::new();
    for k in &st.kb {
        let r = k.r#ref.trim();
        if !valid_kb_ref(r) {
            out.push(Issue::err(
                "R11",
                format!("kb 引用「{}」不合法（要 `@命名空间/slug` 或 `*`）", k.r#ref),
            ));
        } else if seen.iter().any(|x| x == r) {
            out.push(Issue::err("R11", format!("kb 引用「{r}」重复")));
        } else {
            seen.push(r.to_string());
        }
        if !STATE_KB_MODES.contains(&k.mode.as_str()) {
            out.push(Issue::err(
                "R11",
                format!("kb「{r}」的 mode「{}」不合法（{}）", k.mode, STATE_KB_MODES.join("|")),
            ));
        }
    }

    if let Some(m) = st.memory.as_ref() {
        if m.subject.trim().is_empty() {
            out.push(Issue::warn("R11", "memory.subject 为空 —— 建议显式写 `self` 或流水线名（否则多条流水线会共享一份记忆）"));
        }
        for k in &m.kinds {
            if !STATE_MEMORY_KINDS.contains(&k.as_str()) {
                out.push(Issue::err(
                    "R11",
                    format!("memory.kinds 里的「{k}」不在词表里（{}）", STATE_MEMORY_KINDS.join("|")),
                ));
            }
        }
        if m.ttl_days > 3650 {
            out.push(Issue::err("R11", format!("memory.ttl_days={} 超过 10 年（0 = 不过期，别用大数字表达“很久”）", m.ttl_days)));
        }
    }

    if let Some(c) = st.checkpoints.as_ref() {
        if c.enabled && !STATE_CKPT_LABELS.contains(&c.label.as_str()) {
            out.push(Issue::err(
                "R11",
                format!("checkpoints.label「{}」不合法（{}）", c.label, STATE_CKPT_LABELS.join("|")),
            ));
        }
        if !c.enabled && c.keep_local > 0 {
            out.push(Issue::warn(
                "R11",
                format!("checkpoints.enabled=false 但 keep_local={} —— 不打点就没什么可留（要么开 enabled，要么把 keep_local 设 0）", c.keep_local),
            ));
        }
        if c.enabled && c.label.trim().is_empty() {
            out.push(Issue::err("R11", "checkpoints.enabled=true 但没写 label（打点粒度得说清）"));
        }
    }

    // 一致性：状态住在**节点**上，要用它就得能联网。
    if pkg.permissions.network.is_empty() {
        out.push(Issue::err(
            "R11",
            "声明了 state{} 但 permissions.network 是空的 —— kb / 记忆 / 检查点都在节点上，拿不到网络就永远读不到自己的数据",
        ));
    }
    out
}

/// 从源码/技能文件里粗略抽取外呼域名（用于 R5 权限面核对）
fn hosts_in_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &text[i..];
        let hit = rest.find("https://").map(|o| (o, 8)).or_else(|| rest.find("http://").map(|o| (o, 7)));
        let Some((off, skip)) = hit else { break };
        let start = i + off + skip;
        let mut end = start;
        for (k, ch) in text[start..].char_indices() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '_') {
                end = start + k + ch.len_utf8();
            } else {
                break;
            }
        }
        let host = text[start..end]
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !host.is_empty() {
            out.push(host);
        }
        i = end.max(i + 1);
    }
    out.sort();
    out.dedup();
    out
}

pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return true;
    }
    allow.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        if a.is_empty() {
            return false;
        }
        if let Some(suffix) = a.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == a
        }
    })
}

/// 依赖引用是不是包内本地路径（`./x` / `../x` / `x.md`）
/// 远程制品 id（`H-xxx` / `A-xxx` / `@ns/slug`）不带 `.` → 视为远程
pub fn is_local_ref(r: &str) -> bool {
    let r = r.trim();
    r.starts_with("./") || r.starts_with("../") || (!r.contains(':') && !r.contains('/') && r.contains('.'))
}

/// R1~R5 校验（离线）。
///
/// `allow_unlocked_remote`：`hur build` 阶段为 true —— 此时锁文件还没写出来，
/// 远程依赖“未登记”只能算提醒；pack / verify 阶段为 false（必须先 build）。
pub fn validate(pkg: &HurPackage, dir: &Path, lock: Option<&HurLock>, allow_unlocked_remote: bool) -> Vec<Issue> {
    let mut out = Vec::new();
    // 生效的 profile（写了用写的，没写按 kind 推导）—— 必填项由它决定，
    // 所以 R1/R3 不再硬编 "除 repo 外 entry 必填"。
    let prof = pkg.profile_name();
    let executable = crate::profile::is_executable(prof);

    // R1 规范与必填
    if pkg.spec != PKG_SPEC {
        out.push(Issue::err("R1", format!("spec 必须是 {PKG_SPEC}，当前是「{}」", pkg.spec)));
    }
    if !valid_kind(&pkg.kind) {
        out.push(Issue::err(
            "R1",
            format!(
                "kind 必须是 agent|harness|repo|skill|mcp|plugin|app|scaffold，当前是「{}」",
                pkg.kind
            ),
        ));
    }
    if pkg.name.trim().is_empty() {
        out.push(Issue::err("R1", "name 不能为空"));
    }
    if !valid_id(&pkg.kind, &pkg.id) {
        out.push(Issue::err(
            "R1",
            format!(
                "id「{}」不合法：kind={} 时必须以 `{}` 开头，且只含字母数字与 - _ . / @",
                pkg.id,
                pkg.kind,
                kind_prefix(&pkg.kind)
            ),
        ));
    }
    // 只有**可执行** profile 才要求入口（数据快照与技能包反过来：禁止有入口，见 R12）
    if executable && pkg.entry.trim().is_empty() {
        out.push(Issue::err(
            "R1",
            format!("profile={prof} 是可执行的，entry 不能为空（指向包内入口文件，如 src/agent.ts）"),
        ));
    }

    // R2 版本
    match parse_semver(&pkg.version) {
        None => out.push(Issue::err("R2", format!("version「{}」不是合法语义化版本（x.y.z）", pkg.version))),
        Some(_) if pkg.version.trim() != pkg.version => {
            out.push(Issue::err("R2", "version 不能有前后空格"));
        }
        Some(_) => {}
    }

    // R3 入口与 kind 契约
    if !pkg.entry.trim().is_empty() {
        let entry = dir.join(pkg.entry.trim());
        if !entry.exists() {
            out.push(Issue::err("R3", format!("entry「{}」不存在", pkg.entry)));
        } else if std::fs::read(&entry).map(|b| b.is_empty()).unwrap_or(true) {
            out.push(Issue::err("R3", format!("entry「{}」是空文件", pkg.entry)));
        }
    }
    // 数据快照包不发代码，别拿"端点 schema"的要求去烦它（R12 已经管它该有什么）
    if prof == "harness" && !pkg.is_data() {
        let has_schema = dir.join("src").exists()
            && content_files(dir)
                .iter()
                .any(|f| matches!(f.extension().and_then(|e| e.to_str()), Some("json")));
        if !has_schema {
            out.push(Issue::warn("R3", "profile=harness 建议在 src/ 下带 schema JSON（端点定义）"));
        }
    }

    // R4 依赖可解析
    for (kind, reference) in pkg.deps.iter() {
        let r = reference.trim();
        if r.is_empty() {
            continue;
        }
        let is_local = is_local_ref(r);
        if is_local {
            let p = dir.join(r);
            if !p.exists() {
                out.push(Issue::err("R4", format!("{kind} 依赖「{r}」在包内找不到")));
            }
            continue;
        }
        let entry = lock.and_then(|l| l.deps.get(kind)).and_then(|v| v.iter().find(|d| d.r#ref == r));
        match entry {
            None => {
                if allow_unlocked_remote {
                    out.push(Issue::warn(
                        "R4",
                        format!("{kind} 依赖「{r}」暂未登记 —— build 会写入 hur.lock 并标记未校验"),
                    ));
                } else {
                    out.push(Issue::err(
                        "R4",
                        format!("{kind} 依赖「{r}」没有登记进 hur.lock —— 先跑 `hur build`"),
                    ));
                }
            }
            Some(d) if d.resolved == "unresolved" => out.push(Issue::warn(
                "R4",
                format!("{kind} 依赖「{r}」已登记但未在 registry 校验（离线 build）；发布 / 安装时由 registry 解析"),
            )),
            Some(_) => {}
        }
    }

    // R5 权限面：源码里出现的外呼域名必须在 permissions.network 里声明
    for f in content_files(dir) {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "go" | "rs" | "md" | "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for h in hosts_in_text(&text) {
            if !host_allowed(&h, &pkg.permissions.network) {
                out.push(Issue::err(
                    "R5",
                    format!(
                        "{} 里调用了「{h}」，但 permissions.network 未声明（超范围调用）",
                        rel(dir, &f)
                    ),
                ));
            }
        }
    }

    // R7 Agent 声明（profile=agent）：声明必须能静态落地（PRD §9.2/§9.5）
    //
    // ⚠️ 按 **profile** 判，不按 kind：`kind=skill / mcp / plugin / app / scaffold`
    // 现在都是合法值，而决定"这份声明会不会被读"的一直是 profile。
    if prof == "agent" {
        match pkg.agent.as_ref() {
            None => out.push(Issue::warn(
                "R7",
                "kind=agent 建议带上 agent{} 声明（system_prompt / tools / skills），否则只能按代码包处理",
            )),
            Some(a) => {
                if a.system_prompt.trim().is_empty() {
                    out.push(Issue::warn("R7", "agent.system_prompt 为空 —— 桌面端会回落到内置 Agent 的提示词"));
                }
                if a.system_prompt.chars().count() > 8000 {
                    out.push(Issue::err("R7", format!("agent.system_prompt 过长（{} 字符，上限 8000）", a.system_prompt.chars().count())));
                }
                for s in &a.skills {
                    let r = s.trim();
                    if r.is_empty() {
                        out.push(Issue::err("R7", "agent.skills 里有空项"));
                    } else if !dir.join(r).exists() {
                        out.push(Issue::err("R7", format!("agent.skills 里的「{r}」在包内不存在")));
                    }
                }
                let known = ["repo_search", "harness.call", "kb.search", "task.create"];
                for t in &a.tools {
                    let tv = t.trim();
                    if tv.is_empty() {
                        out.push(Issue::err("R7", "agent.tools 里有空项"));
                    } else if !known.contains(&tv) {
                        out.push(Issue::warn("R7", format!("agent.tools 里的「{tv}」不是本机已知工具（桌面端会忽略）")));
                    }
                }
                // adapters：只做拼写体检（导出能力不依赖它）
                for ad in &a.adapters {
                    let v = ad.trim();
                    if v.is_empty() {
                        out.push(Issue::err("R7", "agent.adapters 里有空项"));
                    } else if !crate::interop::valid_target(v) {
                        out.push(Issue::warn(
                            "R7",
                            format!(
                                "agent.adapters 里的「{v}」不是已知宿主（可选：{}）",
                                crate::interop::TARGETS.join(" / ")
                            ),
                        ));
                    }
                }
            }
        }
    } else if let Some(a) = pkg.agent.as_ref().filter(|a| !a.is_empty()) {
        // `adapters` 是**通用**的：plugin / mcp 也靠它声明“接进哪些宿主”（R12 对
        // plugin 是硬要求），所以先拼写体检；其余字段才只在 profile=agent 时被读。
        for ad in &a.adapters {
            let v = ad.trim();
            if v.is_empty() {
                out.push(Issue::err("R7", "agent.adapters 里有空项"));
            } else if !crate::interop::valid_target(v) {
                out.push(Issue::warn(
                    "R7",
                    format!(
                        "agent.adapters 里的「{v}」不是已知宿主（可选：{}）",
                        crate::interop::TARGETS.join(" / ")
                    ),
                ));
            }
        }
        let beyond = !a.system_prompt.trim().is_empty()
            || !a.persona.trim().is_empty()
            || !a.tools.is_empty()
            || !a.skills.is_empty()
            || a.pipeline.is_some()
            || a.guard.is_some();
        if beyond {
            out.push(Issue::warn(
                "R7",
                format!(
                    "profile={prof} 只读 agent{{}} 的 adapters，提示词 / 工具面 / 技能不会被读 —— 这部分会白写"
                ),
            ));
        }
    }

    // R8 安全策略声明（`security{}`）：引擎名、执行入口、network 档位、远程+签名搭配
    out.extend(crate::policy::validate_security(pkg, dir, None));

    // R10 出口声明（`egress{}`）：声明本包可以充当哪几条出网通道。
    // 抽成独立函数是因为**网关也要用同一份判定**（它拿这个当路由的上限）：
    // 「提供出口」是不是合法，只应该有一个答案。
    out.extend(validate_egress(pkg));

    // R11 状态声明（`state{}`）：这个包需要哪些知识库 / 记忆 / 检查点。
    // kb / mem / ckpt 是**状态**（住节点），包只声明要什么 —— 见 validate_state 的说明。
    out.extend(validate_state(pkg));

    // R12 profile：**profile 决定必填项**，不是"写了就放过"。
    out.extend(validate_profile(pkg, dir));

    out
}

/// R12：`state.stores[]` 这一层。
///
/// ⚠️ 这里**故意不重新实现字段声明语法**（`title:string!` 那一套）。
/// 那套语法的唯一实现是节点侧（`model.ParseFieldSpec`）——在这里再写一遍就成了
/// "两套说法"：包校验过了、到节点上被拒，或者反过来。所以这里只判**不需要类型语义**
/// 的那些事（集合名、模式、index 是不是声明过的字段、快照包能不能写…），
/// 字段声明本身交给节点（`ncc store declare`）：**报错要发生在真正声明的地方**。
fn validate_stores(pkg: &HurPackage, prof: &str, data_profile: bool, out: &mut Vec<Issue>) {
    let Some(st) = pkg.state.as_ref() else { return };
    let mut seen: Vec<&str> = Vec::new();
    for (i, r) in st.stores.iter().enumerate() {
        let name = r.collection.trim();
        if name.is_empty() {
            out.push(Issue::err("R12", "state.stores[] 里有空 collection"));
            continue;
        }
        if !valid_collection_name(name) {
            out.push(Issue::err(
                "R12",
                format!("集合名「{name}」不合法（小写字母数字与 -_，字母开头，≤48）"),
            ));
        }
        if seen.contains(&name) {
            out.push(Issue::err(
                "R12",
                format!("集合「{name}」在 state.stores[] 里出现了两次 —— 一个集合一份需求，别让它有两种说法"),
            ));
        }
        seen.push(name);

        // 模式：空 = read
        let m = r.mode.trim();
        if !m.is_empty() && !STORE_MODES.contains(&m) {
            out.push(Issue::err(
                "R12",
                format!("state.stores[{i}].mode「{m}」不合法（read / write / readwrite，不写 = read）"),
            ));
        }
        let sh = r.shape.trim();
        if !sh.is_empty() && !STORE_SHAPES.contains(&sh) {
            out.push(Issue::err(
                "R12",
                format!("state.stores[{i}].shape「{sh}」不合法（mutable / append-only）"),
            ));
        }
        let vis = r.visibility.trim();
        if !vis.is_empty() && !STORE_VISIBILITIES.contains(&vis) {
            out.push(Issue::err(
                "R12",
                format!("state.stores[{i}].visibility「{vis}」不合法（private / public）"),
            ));
        }

        // 能过滤的字段必须先在 fields 里声明过 —— 动态 ≠ 无模式。
        // 字段名就是 `:` 前面那一段（取个名字，不算实现语法）。
        let declared: Vec<&str> = r
            .fields
            .iter()
            .filter_map(|f| f.split(':').next())
            .map(str::trim)
            .collect();
        for idx in &r.index {
            let f = idx.trim();
            if f.is_empty() {
                out.push(Issue::err("R12", format!("state.stores[{i}].index 里有空字段名")));
                continue;
            }
            if !declared.contains(&f) {
                out.push(Issue::err(
                    "R12",
                    format!(
                        "集合「{name}」想按「{f}」过滤，但 fields 里没声明它 —— 没声明的字段在节点上过滤不了（动态 ≠ 无模式）"
                    ),
                ));
            }
        }

        // 快照是只读的：数据包不该写集合
        if data_profile && r.writes() {
            out.push(Issue::err(
                "R12",
                format!(
                    "profile={prof} 是数据快照，不该写集合「{name}」（现在是 {}）—— 快照发出去只读",
                    r.mode_norm()
                ),
            ));
        }

        // 写模式在模型面上就是「**模型可以改这些内容**」—— 说清为什么再给。
        // 只写更特殊：写进去的东西**不回头读**（上报/快照类内容的口径）。
        if r.writes() && r.reason.trim().is_empty() {
            out.push(Issue::warn(
                "R12",
                format!(
                    "集合「{name}」声明成了 {}：这等于说「模型可以改这些内容」{}。\
                     建议写清 state.stores[].reason —— 两个月后你会需要它",
                    r.mode_norm(),
                    if r.mode_norm() == "write" {
                        "（只写 = 这个包拿不回自己写的内容）"
                    } else {
                        ""
                    }
                ),
            ));
        }
    }
}

/// R12：按 profile 校验（**这一条是这个格式敢叫"规范"的原因**）。
///
/// 没有它，`profile` 就只是个标签：一份知识库快照既能带 `entry`（可以跑代码）、
/// 又能带 `permissions.network`（可以自己出网），而校验一路绿灯 ——
/// 那么出问题时就没人能说"这份包按规范就不该这样"。
///
/// 判定分成两半：**可执行类必须有入口**（R1 已管）、**数据类与技能包必须没有**；
/// 数据类还额外要求 `data{}` 把"从哪来 / 什么时候 / 能给谁看"说清。
pub fn validate_profile(pkg: &HurPackage, dir: &Path) -> Vec<Issue> {
    let mut out = Vec::new();
    let prof = pkg.profile_name();
    let Some(def) = crate::profile::get(prof) else {
        out.push(Issue::err(
            "R12",
            format!("profile「{prof}」不在规范里（可选：{}）", crate::profile::names().join(" / ")),
        ));
        return out;
    };

    // 清单**自己写的**那个值要认得出 —— 读的时候不认识会退化成 harness（`profile::of`，
    // 只为兼容老包），但"写了就是不认识"是错，不是默认值：
    // 以前 `ncc hur init --profile claude` 会安静地生出一份 harness 包，用户以为自己
    // 拿到的是另一回事 —— 这种静默的替身必须报出来。
    if let Some(declared) = pkg.profile.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if crate::profile::get(declared).is_none() {
            out.push(Issue::err(
                "R12",
                format!(
                    "清单里的 profile「{declared}」不在规范里（可选：{}）—— 未知值不是默认值，它就是写错了；\
                     删掉这个字段则按 kind 推导",
                    crate::profile::names().join(" / ")
                ),
            ));
        }
    }

    // 集合需求（`state.stores[]`）——与 profile 无关的那部分先判
    validate_stores(pkg, prof, def.data, &mut out);

    // 显式写了 profile 就得跟 kind 对得上（目录靠它检索；对不上就是两套说法）
    //
    // ⚠️ 这里**不**比 kind 与 profile 的字面值：包内 kind（agent/harness/repo）与
    // 目录 kind（plugin/mcp/skill/…）本来就是两套命名，对应关系由
    // `profile::registry_kinds` 唯一确定，发布时用它映射（见 `publish::registry_kind`）。
    // 拿两套命名互比只会到处误报。

    if def.data {
        // ① 数据快照**不许可执行**：这是它敢往外发的前提
        if !pkg.entry.trim().is_empty() {
            out.push(Issue::err(
                "R12",
                format!(
                    "profile={prof} 是数据快照，不允许 entry（现在是「{}」）—— 数据不该能跑代码",
                    pkg.entry
                ),
            ));
        }
        if !pkg.permissions.network.is_empty() {
            out.push(Issue::err(
                "R12",
                format!(
                    "profile={prof} 是数据快照，不允许 permissions.network（现在是 {:?}）—— 数据包不该自己出网",
                    pkg.permissions.network
                ),
            ));
        }
        if pkg.egress.as_ref().map(|e| !e.is_empty()).unwrap_or(false) {
            out.push(Issue::err("R12", format!("profile={prof} 是数据快照，不该声明 egress{{}}（那是出网通道）")));
        }
        if pkg.agent.as_ref().map(|a| !a.is_empty()).unwrap_or(false) {
            out.push(Issue::err("R12", format!("profile={prof} 是数据快照，不该带 agent{{}} 声明")));
        }
        // ② 快照必须说清来历与去向
        match pkg.data.as_ref() {
            None => out.push(Issue::err(
                "R12",
                format!("profile={prof} 必须带 data{{}} 声明（source / snapshot_at / privacy）—— 不然包里就是一堆来历不明的字节"),
            )),
            Some(d) => {
                if d.source.trim().is_empty() {
                    out.push(Issue::err("R12", "data.source 不能为空（这份快照是从哪儿取的）"));
                }
                if d.snapshot_at.trim().is_empty() {
                    out.push(Issue::err("R12", "data.snapshot_at 不能为空（「什么时候的数据」是这份声明的一半价值）"));
                }
                if !DATA_PRIVACY.contains(&d.privacy.trim()) {
                    out.push(Issue::err(
                        "R12",
                        format!("data.privacy「{}」不合法（可选：{}）", d.privacy, DATA_PRIVACY.join(" / ")),
                    ));
                }
                if d.license.trim().is_empty() {
                    out.push(Issue::warn("R12", "data.license 未声明 —— 往外发之前先想清楚许可"));
                }
                if let Some(p) = d.payload.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    if !DATA_PAYLOAD.contains(&p) {
                        out.push(Issue::err(
                            "R12",
                            format!("data.payload「{p}」不合法（可选：{}）", DATA_PAYLOAD.join(" / ")),
                        ));
                    }
                    // 带完整载荷却说自己能公开 —— 这是最危险的一种搭配，直接拦
                    if p == "full" && d.privacy.trim() == "public" {
                        out.push(Issue::err("R12", "data.payload=full 与 privacy=public 不能同时出现（完整载荷不该公开）"));
                    }
                }
                if d.docs.is_empty() {
                    out.push(Issue::err("R12", "data.docs 是空的 —— 快照里没有任何文件"));
                }
                for doc in &d.docs {
                    let p = doc.path.trim();
                    if p.is_empty() {
                        out.push(Issue::err("R12", "data.docs 里有空 path"));
                        continue;
                    }
                    if !p.starts_with(DATA_DIR) {
                        out.push(Issue::err(
                            "R12",
                            format!("data.docs 的「{p}」必须在 {DATA_DIR} 下 —— 数据与代码要能一眼分开"),
                        ));
                    }
                    if !dir.join(p).is_file() {
                        out.push(Issue::err("R12", format!("data.docs 的「{p}」在包里不存在")));
                    }
                    // 可见性不能比整包更公开（包说 internal，里面的文件不能声明 public）
                    if !doc.visibility.trim().is_empty()
                        && d.privacy.trim() != "public"
                        && doc.visibility.trim() == "public"
                    {
                        out.push(Issue::err(
                            "R12",
                            format!("data.privacy={} 却把「{p}」声明成 public —— 往外一导入就漏了", d.privacy),
                        ));
                    }
                }
            }
        }
    } else {
        // 反过来：非数据类不该带 data{}（带了说明模板串了）
        if pkg.data.is_some() {
            out.push(Issue::err(
                "R12",
                format!("profile={prof} 不是数据快照，不该带 data{{}} 声明（数据类可选：kb-seed / mem-seed / ckpt-set / trace-set）"),
            ));
        }
    }

    // 技能包：至少一份技能文件（`skills/` 下）；它同样不该有代码入口
    if prof == "skill" {
        let files = content_files(dir);
        let skills: Vec<_> = files
            .iter()
            .filter(|f| rel(dir, f).starts_with("skills/"))
            .collect();
        if skills.is_empty() {
            out.push(Issue::err("R12", "profile=skill 要求 skills/ 下至少一份技能文件"));
        }
        if !pkg.entry.trim().is_empty() {
            out.push(Issue::err("R12", "profile=skill 不该有 entry（技能不是程序）"));
        }
    }

    // 插件类：得说清接进哪个宿主，否则 interop 渲染不出来
    if prof == "plugin" {
        let hosts = pkg.agent.as_ref().map(|a| a.adapters.len()).unwrap_or(0);
        if hosts == 0 {
            out.push(Issue::err(
                "R12",
                format!("profile=plugin 要声明宿主（agent.adapters，可选：{}）", crate::interop::TARGETS.join(" / ")),
            ));
        }
    }

    // 授权包（`auth`）：本仓装的东西里最敏感的一类 —— 它装的是凭据。
    // 判定只认三件事：① 声明齐（`auth{}`）；② 包里只允许元数据明文；
    // ③ **密文被清单钉住**（`auth.vault_sha256` 与 `auth/vault.enc` 对得上）——
    //    签名覆盖 `hur.json`，于是「把密文换掉」必须连清单一起改，签名就挂不住了。
    if prof == "auth" {
        validate_auth(pkg, dir, &mut out);
    }

    out
}

/// 授权包的落点（写死在规范里：改路径等于改格式）。
pub const AUTH_DIR: &str = "auth/";
/// 明文元数据（**不含任何值**）：项目 / 条目名 / 过期 / 密钥包法 / 迭代次数。
pub const AUTH_INDEX: &str = "auth/index.json";
/// 密文（AEAD）：所有项目的值都在这里。
pub const AUTH_VAULT: &str = "auth/vault.enc";

/// 项目密钥文件的路径（`auth/keys/<项目>.enc`）——一份一个文件，可单独轮换。
pub fn auth_key_file(project: &str) -> String {
    format!("{AUTH_DIR}keys/{}.enc", project.trim())
}

/// 项目名的合法字符（它要当文件名，所以窄一点）。
pub fn auth_project_ok(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 64
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// R12 · 授权包（别在别处再写一遍这些规则。客户端与服务端两边都读这一段判定）。
fn validate_auth(pkg: &HurPackage, dir: &Path, out: &mut Vec<Issue>) {
    // ① 职责单一：不跑代码、不出网、不是数据快照
    if !pkg.entry.trim().is_empty() {
        out.push(Issue::err(
            "R12",
            format!("profile=auth 不允许 entry（现在是「{}」）—— 授权包不跑代码，它只被 ncc 打开", pkg.entry),
        ));
    }
    if !pkg.permissions.network.is_empty() {
        out.push(Issue::err("R12", "profile=auth 不允许 permissions.network —— 授权包不该自己出网"));
    }
    if pkg.egress.as_ref().map(|e| !e.is_empty()).unwrap_or(false) {
        out.push(Issue::err("R12", "profile=auth 不该声明 egress{}（那是出网通道）"));
    }
    if pkg.agent.as_ref().map(|a| !a.is_empty()).unwrap_or(false) {
        out.push(Issue::err("R12", "profile=auth 不该带 agent{} 声明"));
    }
    if pkg.data.is_some() {
        out.push(Issue::err("R12", "profile=auth 要用 auth{} 声明，不是 data{}（授权包不是数据快照）"));
    }
    if pkg.state.as_ref().map(|s| !s.stores.is_empty()).unwrap_or(false) {
        out.push(Issue::err("R12", "profile=auth 不该声明 state.stores[]（那是往节点上写集合）"));
    }

    // ② 声明齐
    let Some(a) = pkg.auth.as_ref() else {
        out.push(Issue::err(
            "R12",
            "profile=auth 必须带 auth{} 声明（projects / wrap / vault_sha256）—— 不然包里就是一堆没人知道怎么开、开着什么的字节",
        ));
        return;
    };
    if a.projects.is_empty() {
        out.push(Issue::err("R12", "auth.projects 是空的 —— 一份授权包至少要有一个项目"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &a.projects {
        let n = p.trim();
        if !auth_project_ok(n) {
            out.push(Issue::err(
                "R12",
                format!("auth.projects 的「{n}」不合法（只允许小写字母 / 数字 / - _，且要当文件名用）"),
            ));
            continue;
        }
        if !seen.insert(n.to_string()) {
            out.push(Issue::warn("R12", format!("auth.projects 里「{n}」写了两遍")));
        }
        if !dir.join(auth_key_file(n)).is_file() {
            out.push(Issue::err(
                "R12",
                format!("auth.projects 里的「{n}」没有对应的项目密钥文件 {}（轮换过的旧文件不该留在包里）", auth_key_file(n)),
            ));
        }
    }
    if a.wrap.is_empty() {
        out.push(Issue::err(
            "R12",
            "auth.wrap 是空的 —— 至少写一种解锁方式（passphrase / identity），不然这份包谁也打不开",
        ));
    }
    for w in &a.wrap {
        let v = w.trim();
        if v != "passphrase" && v != "identity" {
            out.push(Issue::err("R12", format!("auth.wrap 里的「{v}」不认识（可选：passphrase / identity）")));
        }
    }
    if a.wrap.iter().any(|w| w.trim() == "identity") && a.allow.is_empty() {
        out.push(Issue::warn(
            "R12",
            "auth.wrap 里有 identity 但 auth.allow 是空的 —— 那台机器上任何身份都能开；要限定就把允许的公钥指纹写进 auth.allow",
        ));
    }
    for f in &a.allow {
        if !f.trim().starts_with("SHA256:") {
            out.push(Issue::err("R12", format!("auth.allow 里的「{f}」不是公钥指纹（应当形如 SHA256:abcd…）")));
        }
    }
    if a.kdf_iters == 0 {
        out.push(Issue::warn("R12", "auth.kdf_iters 没写 —— 第三方没法核对口令强度"));
    } else if a.kdf_iters < 100_000 {
        out.push(Issue::warn("R12", format!("auth.kdf_iters={} 偏低（建议 ≥ 210000）", a.kdf_iters)));
    }

    // ③ 文件真的在，且密文与清单钉得住
    let vault = dir.join(AUTH_VAULT);
    if !vault.is_file() {
        out.push(Issue::err("R12", format!("{AUTH_VAULT} 不在包里 —— 授权包的本体就是它")));
    } else {
        if a.vault_sha256.trim().is_empty() {
            out.push(Issue::err(
                "R12",
                "auth.vault_sha256 是空的 —— 密文必须被清单钉住（否则换掉密文不用改签名）",
            ));
        } else {
            match std::fs::read(&vault).map(|b| sha256_hex(&b)) {
                Ok(got) if got == a.vault_sha256.trim() => {}
                Ok(got) => out.push(Issue::err(
                    "R12",
                    format!("{AUTH_VAULT} 与 auth.vault_sha256 对不上（清单 {}，实际 {got}）", a.vault_sha256.trim()),
                )),
                Err(e) => out.push(Issue::err("R12", format!("读不了 {AUTH_VAULT}：{e}"))),
            }
        }
    }
    let index = dir.join(AUTH_INDEX);
    if !index.is_file() {
        out.push(Issue::err(
            "R12",
            format!("{AUTH_INDEX} 不在包里 —— 没有它，别人看不出这份包里有哪几个项目、要拿什么开"),
        ));
        return;
    }
    match std::fs::read_to_string(&index).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
        None => out.push(Issue::err("R12", format!("{AUTH_INDEX} 不是合法 JSON"))),
        // 元数据文件里出现「值」是最典型的一种手滑 —— 直接拦（这里只扫键名，不看内容）
        Some(v) => {
            if let Some(p) = find_value_like(&v, "") {
                out.push(Issue::err(
                    "R12",
                    format!("{AUTH_INDEX} 的 {p} 看起来带着**值** —— 包里只允许元数据明文，值要进 {AUTH_VAULT}"),
                ));
            }
        }
    }
}

/// 在 JSON 里找形如「值」的字段（`value` / `secret` / `password` / `token` 且非空）。
/// 返回第一个命中的路径，便于把话说清楚。
fn find_value_like(v: &serde_json::Value, path: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                let here = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                let kl = k.to_ascii_lowercase();
                let is_secret_name = matches!(kl.as_str(), "value" | "secret" | "password" | "passphrase" | "token" | "apikey" | "api_key");
                if is_secret_name && !val.is_null() && val.as_str().map(|s| !s.trim().is_empty()).unwrap_or(true) {
                    return Some(format!("「{here}」字段"));
                }
                if let Some(p) = find_value_like(val, &here) {
                    return Some(p);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                if let Some(p) = find_value_like(item, &format!("{path}[{i}]")) {
                    return Some(p);
                }
            }
            None
        }
        _ => None,
    }
}

/// R10：校验包的出口声明（`egress{}`）。
///
/// 要点不是“声明的对不对”，而是**声明本身要有边界**：target 必须 https（本机除外）、
/// paths/methods 不能为空（空 = 放行一切）、要出网就得在 `permissions.network` 里出现。
/// 网关 `accept` 路由拿它当上限（见 `egress_covers`）。
/// 出网域名是否在权限面里（不在就提醒 —— 声明了出口却没声明域名，与 R5 会不一致）。
fn check_egress_host(route: &str, host: &str, pkg: &HurPackage, out: &mut Vec<Issue>) {
    if pkg.permissions.network.is_empty() {
        out.push(Issue::err(
            "R10",
            format!("egress「{route}」要出网，但 permissions.network 是空的 —— 声明出口前先声明权限面"),
        ));
    } else if !host_allowed(host, &pkg.permissions.network) {
        out.push(Issue::warn(
            "R10",
            format!("egress「{route}」的域名「{host}」不在 permissions.network 里（建议补齐，否则与 R5 不一致）"),
        ));
    }
}

pub fn validate_egress(pkg: &HurPackage) -> Vec<Issue> {
    let mut out = Vec::new();
    match pkg.egress.as_ref() {
        Some(eg) if !eg.is_empty() => {
            let mut seen: Vec<String> = Vec::new();
            for r in &eg.provides {
                if !valid_egress_name(&r.name) {
                    out.push(Issue::err(
                        "R10",
                        format!("egress.provides 的路由名「{}」不合法（只允许 [a-z0-9._-]，≤64）", r.name),
                    ));
                } else if seen.contains(&r.name) {
                    out.push(Issue::err("R10", format!("egress 路由名「{}」重复", r.name)));
                } else {
                    seen.push(r.name.clone());
                }

                match parse_http_target(&r.target) {
                    None => {
                        // 多供应商写法下 target 本来就该为空 —— 只有"两种都没写"才是错
                        if r.providers.is_empty() {
                            out.push(Issue::err(
                                "R10",
                                format!("egress「{}」的 target「{}」不合法（要 https://主机[:端口][/基路径]）", r.name, r.target),
                            ));
                        }
                    }
                    Some(t) => {
                        if t.scheme == "http" && !is_loopback_host(&t.host) {
                            out.push(Issue::err(
                                "R10",
                                format!("egress「{}」的 target 是明文 http 且非本机 —— 出站必须 https", r.name),
                            ));
                        }
                        if r.providers.is_empty() {
                            check_egress_host(&r.name, &t.host, pkg, &mut out);
                        }
                    }
                }

                // 多供应商：声明的是"允许在哪几家之间切"
                if !r.providers.is_empty() {
                    if !r.target.trim().is_empty() {
                        out.push(Issue::err(
                            "R10",
                            format!(
                                "egress「{}」同时写了 target 与 providers —— 两份真相：多供应商时 target 必须为空（每一家的地址写在各 provider 里）",
                                r.name
                            ),
                        ));
                    }
                    let mut pseen: Vec<String> = Vec::new();
                    for p in &r.providers {
                        if !valid_egress_name(&p.name) {
                            out.push(Issue::err("R10", format!("egress「{}」的供应商名「{}」不合法（只允许 [a-z0-9._-]，≤64）", r.name, p.name)));
                        } else if pseen.contains(&p.name) {
                            out.push(Issue::err("R10", format!("egress「{}」的供应商名「{}」重复", r.name, p.name)));
                        } else {
                            pseen.push(p.name.clone());
                        }
                        match parse_http_target(&p.target) {
                            None => out.push(Issue::err(
                                "R10",
                                format!("egress「{}」供应商「{}」的 target「{}」不合法", r.name, p.name, p.target),
                            )),
                            Some(t) => {
                                if t.scheme == "http" && !is_loopback_host(&t.host) {
                                    out.push(Issue::err(
                                        "R10",
                                        format!("egress「{}」供应商「{}」的 target 是明文 http 且非本机 —— 出站必须 https", r.name, p.name),
                                    ));
                                }
                                check_egress_host(&r.name, &t.host, pkg, &mut out);
                            }
                        }
                        for h in &p.inject {
                            if !valid_header_name(h) {
                                out.push(Issue::err("R10", format!("egress「{}」供应商「{}」的 inject 头名「{h}」不合法", r.name, p.name)));
                            }
                        }
                        // 重写的**键**必须是这条路由已经声明过的路径：否则等于凭空多出一条
                        // 谁也走不到的映射（路由白名单里没有它）。
                        for (from, to) in &p.rewrite {
                            let ok_from = r
                                .paths
                                .iter()
                                .any(|d| normalize_egress_path(d).as_deref() == normalize_egress_path(from).as_deref());
                            if !ok_from {
                                out.push(Issue::err(
                                    "R10",
                                    format!(
                                        "egress「{}」供应商「{}」的 rewrite 键「{}」不在路由 paths 里（声明里只有：{}）—— 映射一个走不到的路径没有意义",
                                        r.name,
                                        p.name,
                                        from,
                                        r.paths.join(", ")
                                    ),
                                ));
                            }
                            if normalize_egress_path(to).is_none() {
                                out.push(Issue::err(
                                    "R10",
                                    format!("egress「{}」供应商「{}」的 rewrite 目标「{}」不合法", r.name, p.name, to),
                                ));
                            }
                        }
                    }
                }

                if r.paths.is_empty() {
                    out.push(Issue::err(
                        "R10",
                        format!("egress「{}」没声明 paths —— 空白名单会放行该上游下的一切路径", r.name),
                    ));
                }
                for p in &r.paths {
                    if normalize_egress_path(p).is_none() {
                        out.push(Issue::err("R10", format!("egress「{}」的路径「{p}」不合法", r.name)));
                    }
                }
                if r.methods.is_empty() {
                    out.push(Issue::err("R10", format!("egress「{}」没声明 methods（空 = 拒绝一切）", r.name)));
                }
                for h in &r.inject {
                    if !valid_header_name(h) {
                        out.push(Issue::err("R10", format!("egress「{}」的 inject 头名「{h}」不合法", r.name)));
                    }
                }
            }
        }
        Some(_) => out.push(Issue::warn(
            "R10",
            "egress{} 存在但 provides 为空 —— 等于没有声明出口（网关不会从它拿到任何路由）",
        )),
        None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pkg(name: &str) -> PathBuf {
        // ⚠️ 目录名必须**每个测试一份**：测试是并行跑的，同名目录会被另一个测试
        // `remove_dir_all` 掉，于是这个测试在"已经不存在/正在被删"的路径上建文件
        // （症状：`Os { code: 22, kind: InvalidInput }`，看着像权限问题其实是抢目录）。
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("hur-test-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/agent.ts"), "export const x = 1\n").unwrap();
        dir
    }

    fn base(kind: &str, id: &str) -> HurPackage {
        HurPackage {
            egress: None,
            state: None,
            spec: PKG_SPEC.into(),
            kind: kind.into(),
            profile: None,
            data: None,
            id: id.into(),
            name: "测试包".into(),
            version: "0.1.0".into(),
            short: "T".into(),
            domain: "hotel".into(),
            summary: String::new(),
            entry: "src/agent.ts".into(),
            runtime: "local-v0".into(),
            capabilities: vec![],
            deps: Deps::default(),
            permissions: Permissions::default(),
            publish: PublishInfo::default(),
            agent: None,
            security: None,
            auth: None,
        }
    }

    /* ---------------- R12：授权包（profile=auth） ---------------- */

    /// 造一份「像模像样」的授权包：清单 + index + vault + 项目密钥文件。
    fn auth_pkg(name: &str, with_files: bool) -> (PathBuf, HurPackage) {
        let dir = temp_pkg(name);
        std::fs::create_dir_all(dir.join("auth/keys")).unwrap();
        let vault = b"NCCAUTH1-fake-ciphertext".to_vec();
        if with_files {
            std::fs::write(dir.join(AUTH_VAULT), &vault).unwrap();
            std::fs::write(
                dir.join(AUTH_INDEX),
                r#"{"v":1,"projects":[{"name":"web","entries":[{"name":"DB_URL","kind":"secret"}]}]}"#,
            )
            .unwrap();
            std::fs::write(dir.join("auth/keys/web.enc"), b"wrapped").unwrap();
        }
        let mut pkg = base("harness", "H-auth-demo-abc123");
        pkg.profile = Some("auth".into());
        pkg.entry = String::new();
        pkg.auth = Some(AuthDecl {
            projects: vec!["web".into()],
            wrap: vec!["passphrase".into()],
            allow: vec![],
            kdf_iters: 210_000,
            vault_sha256: if with_files { sha256_hex(&vault) } else { String::new() },
            created_at_unix: 0,
            note: "给发版用的".into(),
        });
        (dir, pkg)
    }

    fn auth_msgs(pkg: &HurPackage, dir: &Path) -> Vec<String> {
        validate(pkg, dir, None, false)
            .into_iter()
            .filter(|i| i.rule == "R12")
            .map(|i| format!("{:?}:{}", i.level, i.msg))
            .collect()
    }

    #[test]
    fn r12_auth_happy_path_is_quiet() {
        let (dir, pkg) = auth_pkg("auth-ok", true);
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().all(|m| !m.starts_with("Error")), "{msgs:?}");
    }

    #[test]
    fn r12_auth_requires_the_declaration() {
        let (dir, mut pkg) = auth_pkg("auth-nodecl", true);
        pkg.auth = None;
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().any(|m| m.contains("必须带 auth{}")), "{msgs:?}");
    }

    #[test]
    fn r12_auth_pins_the_ciphertext_to_the_manifest() {
        let (dir, pkg) = auth_pkg("auth-pin", true);
        // 只改密文一个字节：清单里的摘要就与它对不上了（这就是“签名搭得住密文”的那一步）
        std::fs::write(dir.join(AUTH_VAULT), b"NCCAUTH1-fake-ciphertext!").unwrap();
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().any(|m| m.contains("vault_sha256 对不上")), "{msgs:?}");
        // 摘要字段空着也不行：那等于换掉密文不用改签名
        let (dir2, mut pkg2) = auth_pkg("auth-pin2", true);
        if let Some(a) = pkg2.auth.as_mut() {
            a.vault_sha256 = String::new();
        }
        let msgs2 = auth_msgs(&pkg2, &dir2);
        assert!(msgs2.iter().any(|m| m.contains("vault_sha256 是空的")), "{msgs2:?}");
    }

    #[test]
    fn r12_auth_index_must_not_carry_values() {
        let (dir, pkg) = auth_pkg("auth-leak", true);
        std::fs::write(
            dir.join(AUTH_INDEX),
            r#"{"v":1,"projects":[{"name":"web","entries":[{"name":"DB_URL","value":"hunter2"}]}]}"#,
        )
        .unwrap();
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().any(|m| m.contains("带着**值**")), "{msgs:?}");
    }

    #[test]
    fn r12_auth_forbids_entry_network_and_data() {
        let (dir, mut pkg) = auth_pkg("auth-forbid", true);
        pkg.entry = "src/agent.ts".into();
        pkg.permissions.network = vec!["api.example.com".into()];
        pkg.data = Some(DataDecl::default());
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().any(|m| m.contains("不允许 entry")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("不允许 permissions.network")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("要用 auth{} 声明，不是 data{}")), "{msgs:?}");
    }

    #[test]
    fn r12_auth_needs_a_key_file_per_project() {
        let (dir, pkg) = auth_pkg("auth-nokey", true);
        std::fs::remove_file(dir.join("auth/keys/web.enc")).unwrap();
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().any(|m| m.contains("没有对应的项目密钥文件")), "{msgs:?}");
    }

    #[test]
    fn r12_auth_wrap_must_be_a_known_mode() {
        let (dir, mut pkg) = auth_pkg("auth-wrap", true);
        if let Some(a) = pkg.auth.as_mut() {
            a.wrap = vec!["telepathy".into()];
        }
        let msgs = auth_msgs(&pkg, &dir);
        assert!(msgs.iter().any(|m| m.contains("不认识")), "{msgs:?}");
        if let Some(a) = pkg.auth.as_mut() {
            a.wrap = vec![];
        }
        let msgs2 = auth_msgs(&pkg, &dir);
        assert!(msgs2.iter().any(|m| m.contains("auth.wrap 是空的")), "{msgs2:?}");
    }


    #[test]
    fn semver_parse_and_cmp() {
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("v1.2.3"), None);
        assert_eq!(cmp_semver("0.2.0", "0.1.9"), Some(std::cmp::Ordering::Greater));
    }

    #[test]
    fn happy_path_has_no_errors() {
        let dir = temp_pkg("ok");
        let pkg = base("agent", "A-hotel-demo-abc123");
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn rejects_bad_kind_id_and_version() {
        let dir = temp_pkg("bad");
        let mut pkg = base("agent", "X-wrong-prefix");
        pkg.version = "0.1".into();
        let issues = validate(&pkg, &dir, None, false);
        let rules: Vec<&str> = issues.iter().map(|i| i.rule.as_str()).collect();
        assert!(rules.contains(&"R1"), "{issues:?}");
        assert!(rules.contains(&"R2"), "{issues:?}");
    }

    #[test]
    fn rejects_missing_entry() {
        let dir = temp_pkg("noentry");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        pkg.entry = "src/nope.ts".into();
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R3"), "{issues:?}");
    }

    /* ---------------- R12：profile 决定必填项 ---------------- */

    /// 造一份数据快照包：`data/` 下真的放一份文件。
    fn data_pkg(prof: &str, dir: &Path) -> HurPackage {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/sample.md"), "# 快照\n").unwrap();
        let mut p = base("agent", "A-data-demo-abc123");
        p.profile = Some(prof.into());
        p.entry = String::new();
        p.data = Some(DataDecl {
            source: "@me/handbook".into(),
            source_target: String::new(),
            snapshot_at: "2026-09-27T10:00:00Z".into(),
            privacy: "internal".into(),
            license: "CC-BY-4.0".into(),
            payload: None,
            note: String::new(),
            docs: vec![DataDoc { path: "data/sample.md".into(), slug: "sample".into(), ..Default::default() }],
        });
        p
    }

    /// 清单里写了一个不认识的名字 —— 这是**错**，不是"回落到默认值"。
    /// 读的时候不认会退化成 harness（`profile::of`，只为兼容老包），但写错了要报出来：
    /// 以前 `ncc hur init --profile claude` 会安静地产出一份 harness 包。
    #[test]
    fn r12_unknown_profile_name_is_an_error_not_a_fallback() {
        let dir = temp_pkg("r12-profile-typo");
        let mut p = data_pkg("kb-seed", &dir);
        p.profile = Some("claude".into());
        p.data = None; // 只想看 profile 这一条
        p.entry = String::new();
        let out = validate(&p, &dir, None, false);
        assert!(
            out.iter().any(|i| i.rule == "R12" && i.level == Level::Error && i.msg.contains("不在规范里")),
            "{out:?}"
        );
        // 删掉这个字段（老包）则按 kind 推导，不报错
        let mut legacy = data_pkg("kb-seed", &dir);
        legacy.profile = None;
        legacy.data = None;
        legacy.entry = String::new();
        let out2 = validate(&legacy, &dir, None, false);
        assert!(
            out2.iter().all(|i| !(i.level == Level::Error && i.msg.contains("不在规范里"))),
            "{out2:?}"
        );
    }

    #[test]
    fn r12_data_snapshot_may_not_be_executable() {
        let dir = temp_pkg("r12-data-exe");
        let mut p = data_pkg("kb-seed", &dir);
        // 一份数据快照带上入口与网络权限 —— 这是最该拦下来的搭配
        p.entry = "src/agent.ts".into();
        p.permissions.network = vec!["api.example.com".into()];
        let out = validate(&p, &dir, None, false);
        let r12: Vec<&str> = out.iter().filter(|i| i.rule == "R12").map(|i| i.msg.as_str()).collect();
        assert!(r12.iter().any(|m| m.contains("不允许 entry")), "{out:?}");
        assert!(r12.iter().any(|m| m.contains("不允许 permissions.network")), "{out:?}");
        // 清爽的版本不该有任何错
        let clean = data_pkg("kb-seed", &dir);
        let out2 = validate(&clean, &dir, None, false);
        assert!(out2.iter().all(|i| i.level != Level::Error), "{out2:?}");
        // 许可没声明只提醒，不拦
        let mut no_license = data_pkg("kb-seed", &dir);
        no_license.data.as_mut().unwrap().license = String::new();
        let out3 = validate(&no_license, &dir, None, false);
        assert!(out3.iter().any(|i| i.rule == "R12" && i.level == Level::Warn), "{out3:?}");
    }

    #[test]
    fn r12_data_snapshot_must_declare_where_it_came_from() {
        let dir = temp_pkg("r12-data-decl");
        let mut p = data_pkg("trace-set", &dir);
        p.data = None;
        let out = validate(&p, &dir, None, false);
        assert!(
            out.iter().any(|i| i.rule == "R12" && i.msg.contains("必须带 data{}")),
            "{out:?}"
        );

        // 宣告了来源，但文件不在包里 / 跑出了 data/ —— 两种都拦
        let mut bad = data_pkg("trace-set", &dir);
        let d = bad.data.as_mut().unwrap();
        d.source = String::new();
        d.docs = vec![
            DataDoc { path: "data/nope.md".into(), ..Default::default() },
            DataDoc { path: "src/agent.ts".into(), ..Default::default() },
        ];
        let out2 = validate(&bad, &dir, None, false);
        let msgs: Vec<String> = out2.iter().filter(|i| i.rule == "R12").map(|i| i.msg.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("data.source 不能为空")), "{out2:?}");
        assert!(msgs.iter().any(|m| m.contains("不存在")), "{out2:?}");
        assert!(msgs.iter().any(|m| m.contains("必须在 data/ 下")), "{out2:?}");
    }

    /// 完整载荷 + 公开 = 最危险的一种搭配，直接拦死。
    #[test]
    fn r12_full_payload_cannot_be_public() {
        let dir = temp_pkg("r12-payload");
        let mut p = data_pkg("trace-set", &dir);
        let d = p.data.as_mut().unwrap();
        d.payload = Some("full".into());
        d.privacy = "public".into();
        let out = validate(&p, &dir, None, false);
        assert!(
            out.iter().any(|i| i.rule == "R12" && i.msg.contains("不能同时出现")),
            "{out:?}"
        );
    }

    /// 非数据类带了 `data{}`：模板串了，也要说。
    #[test]
    fn r12_only_data_profiles_may_carry_data() {
        let dir = temp_pkg("r12-notdata");
        let mut p = base("agent", "A-hotel-demo-abc123");
        p.data = Some(DataDecl::default());
        let out = validate(&p, &dir, None, false);
        assert!(
            out.iter().any(|i| i.rule == "R12" && i.msg.contains("不是数据快照")),
            "{out:?}"
        );
    }

    /// 技能包与插件包各有各的必填项 —— profile 的价值就是这些区别。
    #[test]
    fn r12_skill_and_plugin_have_their_own_requirements() {
        let dir = temp_pkg("r12-skill");
        let mut skill = base("agent", "A-skill-demo-abc123");
        skill.profile = Some("skill".into());
        skill.entry = String::new();
        let out = validate(&skill, &dir, None, false);
        assert!(
            out.iter().any(|i| i.rule == "R12" && i.msg.contains("skills/ 下至少一份")),
            "{out:?}"
        );
        // 补上一份技能文件就过
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(dir.join("skills/demo.md"), "---\nname: demo\n---\n").unwrap();
        let out2 = validate(&skill, &dir, None, false);
        assert!(out2.iter().all(|i| i.level != Level::Error), "{out2:?}");
        // 技能包再带上 entry 就是走回头路
        let mut with_entry = skill.clone();
        with_entry.entry = "src/agent.ts".into();
        let out3 = validate(&with_entry, &dir, None, false);
        assert!(out3.iter().any(|i| i.rule == "R12" && i.msg.contains("不该有 entry")), "{out3:?}");

        // 插件：不写宿主就没法接
        let mut plugin = base("agent", "A-plugin-demo-abc123");
        plugin.profile = Some("plugin".into());
        let out4 = validate(&plugin, &dir, None, false);
        assert!(out4.iter().any(|i| i.rule == "R12" && i.msg.contains("要声明宿主")), "{out4:?}");
    }

    /// 老包（没写 profile）不许因为这一条变红 —— 向后兼容是硬要求。
    #[test]
    fn r12_is_silent_for_legacy_packages_without_profile() {
        let dir = temp_pkg("r12-legacy");
        for kind in ["agent", "harness", "repo"] {
            let mut p = base(kind, &format!("{}-legacy-abc123", kind_prefix(kind).trim_end_matches('-')));
            if kind == "repo" {
                p.entry = String::new();
                p.id = "legacy-repo".into();
            }
            let out = validate(&p, &dir, None, false);
            assert!(
                out.iter().all(|i| i.rule != "R12"),
                "老包（kind={kind}）不该冒出 R12：{out:?}"
            );
        }
    }

    /* ---------------- 产物文件名里的 profile 段 ---------------- */

    #[test]
    fn artifact_name_carries_the_profile_and_never_breaks_the_sidecars() {
        let mut p = base("agent", "A-hotel-demo-abc123");
        assert_eq!(artifact_name(&p), "A-hotel-demo-abc123-0.1.0.agent.hur.gz");
        p.profile = Some("kb-seed".into());
        assert_eq!(artifact_name(&p), "A-hotel-demo-abc123-0.1.0.kb-seed.hur.gz");
        // `.hur` = 这是 HUR 规范产物；末尾的 `.gz` = 外面这层是 gzip 容器。
        // 侧车（`.minisig` / `.sha256`）往完整路径后面追加，所以容器段换了也不受影响。
        assert!(artifact_name(&p).ends_with(".hur.gz"));
        // 候选名：新名字在前，两个老名字兜底（改命名之前打的包还得能用）
        let c = artifact_candidates(&p);
        assert_eq!(c.len(), 3, "{c:?}");
        assert_eq!(c[1], "A-hotel-demo-abc123-0.1.0.kb-seed.hur");
        assert!(c[2].ends_with("0.1.0.hur") && !c[2].contains("kb-seed"), "{c:?}");
    }

    /// 产物 vs 工程目录按名字分路，两种产物名（最新 / 老）都得认。
    #[test]
    fn archive_paths_are_recognised_with_and_without_the_container_suffix() {
        assert!(is_archive_path(Path::new("dist/x-0.1.0.skill.hur.gz")));
        assert!(is_archive_path(Path::new("dist/x-0.1.0.skill.hur")));
        assert!(is_archive_path(Path::new("/a/b/X-0.1.0.HUR.GZ")), "大小写不该让人白跑一趟");
        // 工程目录与技能正文不是产物（把它们当包解会报一句莫名其妙的 zip 错）
        assert!(!is_archive_path(Path::new(".")));
        assert!(!is_archive_path(Path::new("skills/x/SKILL.md")));
        assert!(!is_archive_path(Path::new("x.hur.bak")));
    }

    #[test]
    fn find_artifact_prefers_the_new_name_but_still_sees_the_old_one() {
        let dir = temp_pkg("find-art");
        let dist = dir.join(DIST);
        std::fs::create_dir_all(&dist).unwrap();
        let mut p = base("agent", "A-hotel-demo-abc123");
        assert!(find_artifact(&dist, &p).is_none(), "什么都没打时不该假装找到");

        // 老名字（改命名之前打的产物）仍然找得到
        let old = dist.join(format!("{}-{}.hur", p.id, p.version));
        std::fs::write(&old, b"old").unwrap();
        assert_eq!(find_artifact(&dist, &p).as_deref(), Some(old.as_path()));

        // 两个都在时以新名字为准
        p.profile = Some("kb-seed".into());
        let new = dist.join(artifact_name(&p));
        std::fs::write(&new, b"new").unwrap();
        assert_eq!(find_artifact(&dist, &p).as_deref(), Some(new.as_path()));
    }

    #[test]
    fn name_token_only_reads_a_real_profile_name() {
        assert_eq!(name_profile_token("demo.kb-seed.hur").as_deref(), Some("kb-seed"));
        assert_eq!(name_profile_token("/a/b/demo.mcp.hur").as_deref(), Some("mcp"));
        // 带容器段的最新名字：两个后缀都要摘掉才看得到 profile 那一段
        assert_eq!(name_profile_token("demo.kb-seed.hur.gz").as_deref(), Some("kb-seed"));
        assert_eq!(name_profile_token("/a/b/demo.plugin.hur.gz").as_deref(), Some("plugin"));
        // 老名字那一段是版本号 → 认不出来，也说不出人家写错了
        assert_eq!(name_profile_token("H-demo-0.1.0.hur"), None);
        assert_eq!(name_profile_token("H-demo-0.1.0.hur.gz"), None);
        assert_eq!(name_profile_token("demo.hur"), None);
        // 短别名不在规范里：不认（要么用规范名，要么就当没线索）
        assert_eq!(name_profile_token("demo.kb.hur"), None);
    }

    #[test]
    fn name_manifest_disagreement_warns_but_never_blocks() {
        let mut p = base("agent", "A-hotel-demo-abc123");
        p.profile = Some("kb-seed".into());
        // 改个文件名不等于换身份：以**清单**为准，而且只提醒
        let note = name_mismatch_note("demo.app.hur", &p).expect("应当提醒");
        assert_eq!(note.level, Level::Warn);
        assert!(note.msg.contains("kb-seed") && note.msg.contains("app"), "{}", note.msg);
        assert!(name_mismatch_note("demo.kb-seed.hur", &p).is_none(), "对得上就不吭声");
        assert!(name_mismatch_note("H-demo-0.1.0.hur", &p).is_none(), "老名字不该冒提醒");
    }

    #[test]
    fn rejects_undeclared_network_host() {        let dir = temp_pkg("net");
        std::fs::write(
            dir.join("src/agent.ts"),
            "const u = 'https://api.hotel.example.com/v1/search'\n",
        )
        .unwrap();
        let pkg = base("agent", "A-hotel-demo-abc123");
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R5"), "{issues:?}");

        let mut ok = base("agent", "A-hotel-demo-abc123");
        ok.permissions.network = vec!["*.example.com".into()];
        let issues = validate(&ok, &dir, None, false);
        assert!(!issues.iter().any(|i| i.rule == "R5"), "{issues:?}");
    }

    fn with_agent(pkg: &mut HurPackage, a: AgentSpec) {
        pkg.agent = Some(a);
    }

    #[test]
    fn r7_agent_block_missing_is_only_a_warning() {
        let dir = temp_pkg("r7-none");
        let pkg = base("agent", "A-hotel-demo-abc123");
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
        assert!(issues.iter().any(|i| i.rule == "R7" && i.level == Level::Warn), "{issues:?}");
    }

    #[test]
    fn r7_skills_must_exist_in_package() {
        let dir = temp_pkg("r7-skill");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        with_agent(
            &mut pkg,
            AgentSpec {
                system_prompt: "你是前台".into(),
                skills: vec!["skills/front-desk.md".into()],
                ..Default::default()
            },
        );
        let issues = validate(&pkg, &dir, None, false);
        assert!(
            issues.iter().any(|i| i.rule == "R7" && i.level == Level::Error && i.msg.contains("front-desk")),
            "{issues:?}"
        );

        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(dir.join("skills/front-desk.md"), "# 前台技能\n").unwrap();
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn r7_tools_blank_is_error_and_unknown_is_warning() {
        let dir = temp_pkg("r7-tools");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        with_agent(
            &mut pkg,
            AgentSpec {
                system_prompt: "你是前台".into(),
                tools: vec!["repo_search".into(), "  ".into(), "delete_everything".into()],
                ..Default::default()
            },
        );
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R7" && i.level == Level::Error), "{issues:?}");
        assert!(
            issues.iter().any(|i| i.rule == "R7" && i.level == Level::Warn && i.msg.contains("delete_everything")),
            "{issues:?}"
        );
    }

    #[test]
    fn r7_system_prompt_length_cap() {
        let dir = temp_pkg("r7-long");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        with_agent(
            &mut pkg,
            AgentSpec { system_prompt: "甲".repeat(8001), ..Default::default() },
        );
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R7" && i.level == Level::Error), "{issues:?}");

        // 8000 字符（含多字节）正好在上限内
        let mut ok = base("agent", "A-hotel-demo-abc123");
        with_agent(&mut ok, AgentSpec { system_prompt: "甲".repeat(8000), ..Default::default() });
        let issues = validate(&ok, &dir, None, false);
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn r7_agent_block_ignored_on_non_agent_profile() {
        let dir = temp_pkg("r7-kind");
        let mut pkg = base("harness", "H-hotel-demo-abc123");
        with_agent(&mut pkg, AgentSpec { system_prompt: "不该在这".into(), ..Default::default() });
        let issues = validate(&pkg, &dir, None, false);
        assert!(
            issues.iter().any(|i| i.rule == "R7" && i.level == Level::Warn && i.msg.contains("只读 agent{}")),
            "{issues:?}"
        );

        // plugin / mcp 的 agent{} 里放的是 adapters（接进哪些宿主）—— 那是**会被读**的，
        // 不能再说“整块会被忽略”；只有 adapters 时也不该冒“白写”那一条。
        let mut plugin = base("plugin", "my-plugin");
        plugin.profile = Some("plugin".into());
        plugin.agent = Some(AgentSpec { adapters: vec!["claude".into(), "cursor".into()], ..Default::default() });
        let issues = validate(&plugin, &dir, None, false);
        assert!(
            issues.iter().all(|i| !i.msg.contains("不会被读")),
            "adapters 是被读的，不该报“白写”：{issues:?}"
        );
        assert!(
            issues.iter().all(|i| !(i.rule == "R7" && i.level == Level::Error)),
            "合法宿主名不该报错：{issues:?}"
        );
    }

    /// `hur init --kind agent` 生成的工程必须自带能通过 R7 的 agent{} 声明
    #[test]
    fn r7_init_template_is_valid() {
        use crate::tpl::{build_package, files_for, InitInput};
        let dir = temp_pkg("r7-init");
        let pkg = build_package(&InitInput {
            profile: None,
            kind: "agent".into(),
            name: "Front Desk".into(),
            role: "负责住房接待".into(),
            domain: "hotel".into(),
            short: String::new(),
            version: String::new(),
            summary: String::new(),
            registry: String::new(),
            namespace: String::new(),
        });
        let a = pkg.agent.as_ref().expect("kind=agent 必须带 agent{}");
        assert!(a.system_prompt.contains("住房接待"), "{}", a.system_prompt);
        assert_eq!(a.tools.len(), 4);

        // 把模板文件落盘后，R7 的 skills 存在性检查必须通过
        for (rel, body) in files_for(&pkg) {
            let p = dir.join(&rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().all(|i| i.rule != "R7" || i.level != Level::Error), "{issues:?}");
    }

    #[test]
    fn remote_dep_unlocked_warns_in_build_and_fails_in_verify() {
        let dir = temp_pkg("remote");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        pkg.deps.harness = vec!["H-hotel-booking-1234".into()];
        let build = validate(&pkg, &dir, None, true);
        assert!(build.iter().all(|i| i.level != Level::Error), "{build:?}");
        let verify = validate(&pkg, &dir, None, false);
        assert!(verify.iter().any(|i| i.rule == "R4" && i.level == Level::Error), "{verify:?}");
    }

    #[test]
    fn local_dep_must_exist() {
        let dir = temp_pkg("dep");
        let mut pkg = base("agent", "A-hotel-demo-abc123");
        pkg.deps.skill = vec!["./skills/missing.md".into()];
        let issues = validate(&pkg, &dir, None, false);
        assert!(issues.iter().any(|i| i.rule == "R4"), "{issues:?}");
    }

    /* ---------------- R11：状态声明（kb / 记忆 / 检查点） ---------------- */

    /// 只挑 R11 的结论。
    fn r11(pkg: &HurPackage, dir: &Path) -> Vec<Issue> {
        validate(pkg, dir, None, false).into_iter().filter(|i| i.rule == "R11").collect()
    }

    fn state_pkg() -> HurPackage {
        let mut p = base("agent", "A-state-demo-000001");
        p.permissions.network = vec!["registry.corp.local".into()];
        p.state = Some(StateDecl {
            kb: vec![
                KbRequirement { r#ref: "@team/handbook".into(), mode: "read".into() },
                KbRequirement { r#ref: "@team/notes".into(), mode: "readwrite".into() },
            ],
            memory: Some(MemoryDecl { subject: "self".into(), kinds: vec!["fact".into()], ttl_days: 90 }),
            checkpoints: Some(CheckpointDecl { enabled: true, label: "episode".into(), keep_local: 2 }),
            stores: Vec::new(),
        });
        p
    }

    #[test]
    fn r11_accepts_a_well_formed_state_declaration() {
        let dir = temp_pkg("r11-ok");
        let out = r11(&state_pkg(), &dir);
        assert!(out.iter().all(|i| i.level != Level::Error), "合规声明不该报错：{out:?}");
    }

    #[test]
    fn r11_absent_or_empty_state_is_reported_honestly() {
        let dir = temp_pkg("r11-none");
        let p = base("agent", "A-state-none-000001");
        assert!(r11(&p, &dir).is_empty(), "没声明就该完全静默");

        let mut p2 = base("agent", "A-state-empty-000001");
        p2.state = Some(StateDecl::default());
        let out = r11(&p2, &dir);
        assert!(out.iter().any(|i| i.level == Level::Warn), "声明了却什么都不写应当提醒：{out:?}");
    }

    /// 状态住在节点上：声明了状态却拿不到网络，等于永远读不到自己的数据。
    #[test]
    fn r11_requires_network_permission_for_state() {
        let dir = temp_pkg("r11-net");
        let mut p = state_pkg();
        p.permissions.network.clear();
        let out = r11(&p, &dir);
        assert!(out.iter().any(|i| i.level == Level::Error && i.msg.contains("permissions.network")), "{out:?}");
    }

    #[test]
    fn r11_rejects_bad_refs_modes_and_kinds() {
        let dir = temp_pkg("r11-bad");
        let cases: Vec<(&str, Box<dyn Fn(&mut StateDecl)>)> = vec![
            ("ref 不是引用", Box::new(|st: &mut StateDecl| st.kb[0].r#ref = "team/handbook".into())),
            ("ref 只有命名空间", Box::new(|st: &mut StateDecl| st.kb[0].r#ref = "@team".into())),
            ("ref 大写", Box::new(|st: &mut StateDecl| st.kb[0].r#ref = "@Team/Handbook".into())),
            ("ref 重复", Box::new(|st: &mut StateDecl| st.kb[1].r#ref = st.kb[0].r#ref.clone())),
            ("mode 不认识", Box::new(|st: &mut StateDecl| st.kb[0].mode = "rw".into())),
            ("mode 空", Box::new(|st: &mut StateDecl| st.kb[0].mode = String::new())),
            ("记忆种类不认识", Box::new(|st: &mut StateDecl| {
                st.memory.as_mut().unwrap().kinds = vec!["feeling".into()]
            })),
            ("ttl 太离谱", Box::new(|st: &mut StateDecl| st.memory.as_mut().unwrap().ttl_days = 99_999)),
            ("打点粒度不认识", Box::new(|st: &mut StateDecl| {
                st.checkpoints.as_mut().unwrap().label = "whenever".into()
            })),
            ("开了打点却没写粒度", Box::new(|st: &mut StateDecl| {
                st.checkpoints.as_mut().unwrap().label = String::new()
            })),
        ];
        for (label, mutator) in cases {
            let mut p = state_pkg();
            mutator(p.state.as_mut().unwrap());
            let out = r11(&p, &dir);
            assert!(out.iter().any(|i| i.level == Level::Error), "「{label}」应当被拒：{out:?}");
        }
    }

    /// 关掉打点却要留本地副本 —— 这是自相矛盾，但只提醒（不至于拦人）。
    #[test]
    fn r11_warns_on_contradictory_checkpoint_settings() {
        let dir = temp_pkg("r11-ckpt");
        let mut p = state_pkg();
        let c = p.state.as_mut().unwrap().checkpoints.as_mut().unwrap();
        c.enabled = false;
        c.keep_local = 3;
        let out = r11(&p, &dir);
        assert!(out.iter().any(|i| i.level == Level::Warn && i.msg.contains("keep_local")), "{out:?}");
        assert!(out.iter().all(|i| i.level != Level::Error), "只该提醒：{out:?}");
    }

    #[test]
    fn r11_notes_that_only_agents_and_apps_read_state() {
        let dir = temp_pkg("r11-kind");
        let mut p = state_pkg();
        p.kind = "harness".into();
        let out = r11(&p, &dir);
        assert!(out.iter().any(|i| i.msg.contains("profile=agent / app")), "{out:?}");
        // app（哨）自己的 state{} 不该被自己的规矩警告 —— 它就是靠这份声明说话的
        let mut a = state_pkg();
        a.kind = "app".into();
        a.profile = Some("app".into());
        let out = r11(&a, &dir);
        assert!(
            out.iter().all(|i| !i.msg.contains("只有 profile=agent / app")),
            "app 自己的 state{{}} 不该被警告：{out:?}"
        );
    }

    #[test]
    fn kb_refs_round_trip() {
        assert!(valid_kb_ref("*"));
        assert!(valid_kb_ref("@team/handbook"));
        assert!(valid_kb_ref("@me/notes-2026"));
        assert!(!valid_kb_ref("team/handbook"));
        assert!(!valid_kb_ref("@team"));
        assert!(!valid_kb_ref("@team/a/b"));
        assert_eq!(split_kb_ref("@team/handbook"), ("team".to_string(), "handbook".to_string()));
        assert_eq!(split_kb_ref("*"), ("*".to_string(), String::new()));
    }

    /* ---------------- R10：出口声明 ---------------- */

    fn eg_route(name: &str, target: &str, paths: &[&str], methods: &[&str], inject: &[&str]) -> EgressRoute {
        EgressRoute {
            name: name.into(),
            target: target.into(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            methods: methods.iter().map(|s| s.to_string()).collect(),
            inject: inject.iter().map(|s| s.to_string()).collect(),
            providers: Vec::new(),
        }
    }

    /// 多供应商写法：同一套 paths/methods，多家上游（每家有地址、头名、可选路径重写）。
    fn eg_multi(name: &str, paths: &[&str], methods: &[&str], inject: &[&str]) -> EgressRoute {
        EgressRoute {
            name: name.into(),
            target: String::new(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            methods: methods.iter().map(|s| s.to_string()).collect(),
            inject: inject.iter().map(|s| s.to_string()).collect(),
            providers: vec![
                EgressProvider {
                    name: "openai".into(),
                    target: "https://api.openai.example/v1".into(),
                    inject: vec!["Authorization".into()],
                    rewrite: BTreeMap::new(),
                },
                EgressProvider {
                    name: "anthropic".into(),
                    target: "https://api.anthropic.example/v1".into(),
                    inject: vec!["x-api-key".into(), "anthropic-version".into()],
                    rewrite: [("chat/completions".to_string(), "messages".to_string())].into_iter().collect(),
                },
            ],
        }
    }

    #[test]
    fn r10_multi_provider_declaration_is_well_formed() {
        let dir = temp_pkg("r10-multi");
        let mut pkg = base("harness", "A-r10-multi-000001");
        // 多家的域名都要在权限面里（不在只会提醒，但这里仍写全，免得测试靠提醒过日子）
        pkg.permissions.network = vec!["api.openai.example".into(), "api.anthropic.example".into()];
        pkg.egress = Some(Egress { provides: vec![eg_multi("llm", &["chat/completions"], &["POST"], &["X-Route"])] });
        let out = r10(&pkg, &dir);
        assert!(
            !out.iter().any(|i| i.level == Level::Error),
            "多供应商声明本身该是合法的：{out:?}"
        );

        // target 与 providers 同时写 = 两份真相
        let mut both = eg_multi("llm", &["chat/completions"], &["POST"], &[]);
        both.target = "https://api.openai.example/v1".into();
        pkg.egress = Some(Egress { provides: vec![both] });
        assert!(r10(&pkg, &dir).iter().any(|i| i.msg.contains("两份真相")), "target 与 providers 不能同时写");

        // 重写一个没声明的路径 = 永远走不到的映射
        let mut bad_rw = eg_multi("llm", &["chat/completions"], &["POST"], &[]);
        bad_rw.providers[1].rewrite = [("models".to_string(), "models".to_string())].into_iter().collect();
        pkg.egress = Some(Egress { provides: vec![bad_rw] });
        assert!(
            r10(&pkg, &dir).iter().any(|i| i.msg.contains("rewrite 键")),
            "rewrite 的键必须在路由 paths 里"
        );

        // 供应商名重复
        let mut dup = eg_multi("llm", &["chat/completions"], &["POST"], &[]);
        dup.providers[1].name = "openai".into();
        pkg.egress = Some(Egress { provides: vec![dup] });
        assert!(r10(&pkg, &dir).iter().any(|i| i.msg.contains("重复")), "供应商名不能重复");
    }

    #[test]
    fn provider_covers_is_per_provider_not_per_route() {
        let decl = eg_multi("llm", &["chat/completions"], &["POST"], &["X-Route"]);
        // 挑 anthropic：地址、头名都要跟**它**对得上
        let ok = egress_covers_provider(
            &decl,
            "anthropic",
            "https://api.anthropic.example/v1",
            &["chat/completions".to_string()],
            &["POST".to_string()],
            &["x-api-key".to_string(), "anthropic-version".to_string(), "X-Route".to_string()],
        );
        assert!(ok.is_empty(), "合规的供应商配置不该有问题：{ok:?}");

        // 拿 openai 的地址去配 anthropic —— 必须被指出来
        let wrong_target = egress_covers_provider(
            &decl,
            "anthropic",
            "https://api.openai.example/v1",
            &["chat/completions".to_string()],
            &["POST".to_string()],
            &["x-api-key".to_string()],
        );
        assert!(wrong_target.iter().any(|m| m.contains("target 必须与声明一致")), "{wrong_target:?}");

        // 头名不在这一家（也不在路由级）的并集里
        let wrong_head = egress_covers_provider(
            &decl,
            "anthropic",
            "https://api.anthropic.example/v1",
            &["chat/completions".to_string()],
            &["POST".to_string()],
            &["Authorization".to_string()],
        );
        assert!(wrong_head.iter().any(|m| m.contains("不在供应商")), "{wrong_head:?}");

        // 没声明过的供应商
        let unknown = egress_covers_provider(
            &decl,
            "gemini",
            "https://api.gemini.example/v1",
            &["chat/completions".to_string()],
            &["POST".to_string()],
            &[],
        );
        assert!(unknown.iter().any(|m| m.contains("没声明供应商")), "{unknown:?}");

        // 单供应商声明下要求切供应商 —— 说清楚"声明里用的是单供应商写法"
        let single = eg_route("llm", "https://api.openai.example/v1", &["chat/completions"], &["POST"], &[]);
        let none = egress_covers_provider(&single, "anthropic", "https://api.openai.example/v1", &[], &["POST".to_string()], &[]);
        assert!(none.iter().any(|m| m.contains("单供应商写法")), "{none:?}");
    }

    /// 跑一遍 R1~R10，只要 R10 的结论。
    fn r10(pkg: &HurPackage, dir: &Path) -> Vec<Issue> {
        validate(pkg, dir, None, false).into_iter().filter(|i| i.rule == "R10").collect()
    }

    #[test]
    fn r10_accepts_a_well_formed_declaration() {
        let dir = temp_pkg("r10-ok");
        let mut p = base("harness", "A-r10-ok-000001");
        p.permissions.network = vec!["api.openai.com".into()];
        p.egress = Some(Egress {
            provides: vec![eg_route(
                "llm",
                "https://api.openai.com/v1",
                &["chat/completions"],
                &["POST"],
                &["Authorization"],
            )],
        });
        let out = r10(&p, &dir);
        assert!(out.is_empty(), "合规声明不该有问题：{out:?}");
    }

    #[test]
    fn r10_absent_egress_is_fine() {
        let dir = temp_pkg("r10-none");
        let p = base("harness", "A-r10-none-000001");
        assert!(r10(&p, &dir).is_empty());
    }

    #[test]
    fn r10_rejects_open_or_plaintext_or_credentialed_declarations() {
        let dir = temp_pkg("r10-bad");
        let cases: Vec<(&str, Vec<EgressRoute>, Vec<String>, bool)> = vec![
            // 空 paths：空白名单会放行该上游下的一切路径
            (
                "空白名单",
                vec![eg_route("llm", "https://api.openai.com/v1", &[], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            // 空 methods
            (
                "空方法",
                vec![eg_route("llm", "https://api.openai.com/v1", &["chat/completions"], &[], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            // 明文 http 且非本机
            (
                "明文出站",
                vec![eg_route("llm", "http://api.example.com/v1", &["x"], &["POST"], &[])],
                vec!["api.example.com".into()],
                true,
            ),
            // 要出网但 permissions.network 空
            (
                "没有权限面",
                vec![eg_route("llm", "https://api.openai.com/v1", &["chat/completions"], &["POST"], &[])],
                vec![],
                true,
            ),
            // 路径穿越 / 百分号编码
            (
                "路径带穿越",
                vec![eg_route("llm", "https://api.openai.com/v1", &["../etc/passwd"], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            (
                "路径带编码",
                vec![eg_route("llm", "https://api.openai.com/v1", &["a%2fb"], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            // 路由名不合法 / 重名
            (
                "名字非法",
                vec![eg_route("LLM Main", "https://api.openai.com/v1", &["x"], &["POST"], &[])],
                vec!["api.openai.com".into()],
                true,
            ),
            (
                "重名",
                vec![
                    eg_route("llm", "https://api.openai.com/v1", &["x"], &["POST"], &[]),
                    eg_route("llm", "https://api.openai.com/v1", &["y"], &["POST"], &[]),
                ],
                vec!["api.openai.com".into()],
                true,
            ),
            // 注入头名不合法
            (
                "注入头名非法",
                vec![eg_route("llm", "https://api.openai.com/v1", &["x"], &["POST"], &["Bad Header"])],
                vec!["api.openai.com".into()],
                true,
            ),
        ];
        for (label, provides, network, want_problem) in cases {
            let mut p = base("harness", "A-r10-bad-000001");
            p.permissions.network = network;
            p.egress = Some(Egress { provides });
            let out = r10(&p, &dir);
            assert!(
                out.iter().any(|i| i.is_problem()) == want_problem,
                "「{label}」的 R10 结论不符合预期：{out:?}"
            );
        }
    }

    #[test]
    fn r10_allows_loopback_http_for_local_dev() {
        let dir = temp_pkg("r10-loopback");
        let mut p = base("harness", "A-r10-loop-000001");
        p.permissions.network = vec!["127.0.0.1".into()];
        p.egress = Some(Egress {
            provides: vec![eg_route("llm", "http://127.0.0.1:8899/v1", &["chat/completions"], &["POST"], &[])],
        });
        let out = r10(&p, &dir);
        assert!(out.iter().all(|i| i.level != Level::Error), "本机 http 应当允许：{out:?}");
    }

    #[test]
    fn egress_declares_but_never_carries_credentials() {
        // 结构上就没有“放值”的位置：inject 只存头名。
        // 反证：一个想把密钥写进 inject 的包，会因为头名合法性被拒。
        let dir = temp_pkg("r10-secret");
        let mut p = base("harness", "A-r10-secret-000001");
        p.permissions.network = vec!["api.openai.com".into()];
        p.egress = Some(Egress {
            provides: vec![eg_route(
                "llm",
                "https://api.openai.com/v1",
                &["x"],
                &["POST"],
                &["Authorization: Bearer sk-x"],
            )],
        });
        assert!(r10(&p, &dir).iter().any(|i| i.is_problem()), "把值塞进 inject 必须被拒");
    }

    /* ---------------- S2b：配置只能比声明更窄 ---------------- */

    #[test]
    fn egress_covers_only_narrows_never_widens() {
        let decl = eg_route(
            "llm",
            "https://api.openai.com/v1",
            &["chat/completions", "models"],
            &["POST", "GET"],
            &["Authorization"],
        );

        // 完全一致 → 合规
        assert!(egress_covers(
            &decl,
            "https://api.openai.com/v1",
            &["chat/completions".into()],
            &["POST".into()],
            &["Authorization".into()]
        )
        .is_empty());

        // 更窄（只留一条路径 / 一个方法）→ 合规
        assert!(egress_covers(&decl, "https://api.openai.com/v1", &["models".into()], &["GET".into()], &[]).is_empty());

        // 更宽 → 必须逐条报出来
        let bad = egress_covers(
            &decl,
            "https://api.openai.com/v2",
            &["chat/completions".into(), "files".into()],
            &["POST".into(), "DELETE".into()],
            &["Authorization".into(), "X-Api-Key".into()],
        );
        assert_eq!(bad.len(), 4, "target/路径/方法/注入头 各一条：{bad:?}");
        assert!(bad[0].contains("target"));
        assert!(bad.iter().any(|m| m.contains("files")));
        assert!(bad.iter().any(|m| m.contains("DELETE")));
        assert!(bad.iter().any(|m| m.contains("X-Api-Key")));
    }

    #[test]
    fn egress_covers_compares_paths_after_normalization() {
        let decl = eg_route("llm", "https://api.openai.com/v1", &["chat/completions"], &["POST"], &[]);
        // 前导斜杠 / 尾随斜杠都算同一条
        assert!(egress_covers(&decl, "https://api.openai.com/v1", &["/chat/completions/".into()], &["post".into()], &[]).is_empty());
        // 但穿越写法不因为“看起来像”就放行
        assert!(!egress_covers(&decl, "https://api.openai.com/v1", &["../chat/completions".into()], &["POST".into()], &[]).is_empty());
    }

    #[test]
    fn normalize_egress_path_is_the_shared_rule() {
        assert_eq!(normalize_egress_path("/chat/completions/").as_deref(), Some("chat/completions"));
        for bad in ["", "/", "..", "a/../b", "a//b", "%2e%2e/x", "a\\b", "a?x=1", "a#f", "a b"] {
            assert!(normalize_egress_path(bad).is_none(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn parse_http_target_rejects_the_dangerous_shapes() {
        assert!(parse_http_target("https://api.openai.com/v1").is_some());
        assert!(parse_http_target("http://127.0.0.1:8899").is_some());
        for bad in ["api.openai.com", "ftp://x/y", "https://", "https://user@host/x", "https://host/a b"] {
            assert!(parse_http_target(bad).is_none(), "应拒绝：{bad}");
        }
    }

    /* ---------------- R12：state.stores[]（集合需求） ---------------- */

    /// 造一份带集合需求的包。
    fn store_pkg(items: Vec<StoreRequirement>) -> HurPackage {
        let mut p = base("agent", "A-issue-demo-abc123");
        p.state = Some(StateDecl { stores: items, ..Default::default() });
        p
    }

    fn req(collection: &str) -> StoreRequirement {
        StoreRequirement {
            collection: collection.into(),
            ..Default::default()
        }
    }

    fn r12_msgs(pkg: &HurPackage) -> (Vec<String>, Vec<String>) {
        let dir = temp_pkg("store");
        let issues = validate(pkg, &dir, None, false);
        let errs: Vec<String> = issues
            .iter()
            .filter(|i| i.rule == "R12" && i.level == Level::Error)
            .map(|i| i.msg.clone())
            .collect();
        let warns: Vec<String> = issues
            .iter()
            .filter(|i| i.rule == "R12" && i.level == Level::Warn)
            .map(|i| i.msg.clone())
            .collect();
        (errs, warns)
    }

    #[test]
    fn store_decl_happy_path_is_silent() {
        let mut r = req("issue");
        r.mode = "readwrite".into();
        r.fields = vec!["title:string!".into(), "status:enum:open|closed".into()];
        r.index = vec!["status".into()];
        r.shape = "mutable".into();
        r.visibility = "private".into();
        // 声明了写就得说清为什么 —— 说清了就不吵（这是那份 warn 想要的形态）
        r.reason = "认领问题单并回写处理进展".into();
        let (errs, warns) = r12_msgs(&store_pkg(vec![r]));
        assert!(errs.is_empty(), "{errs:?}");
        assert!(warns.is_empty(), "{warns:?}");
    }

    #[test]
    fn store_decl_index_requires_declared_field() {
        let mut r = req("issue");
        r.fields = vec!["title:string".into()];
        r.index = vec!["status".into()]; // 没声明就想过滤
        let (errs, _) = r12_msgs(&store_pkg(vec![r]));
        assert!(
            errs.iter().any(|m| m.contains("动态 ≠ 无模式")),
            "该拦：{errs:?}"
        );
    }

    #[test]
    fn store_decl_rejects_bad_names_and_modes() {
        let mut bad_name = req("Issue-Tracker");
        bad_name.mode = "read".into();
        let mut bad_mode = req("log");
        bad_mode.mode = "rw".into();
        let mut bad_shape = req("audit");
        bad_shape.shape = "frozen".into();
        let (errs, _) = r12_msgs(&store_pkg(vec![bad_name, bad_mode, bad_shape]));
        assert!(errs.iter().any(|m| m.contains("集合名")), "{errs:?}");
        assert!(errs.iter().any(|m| m.contains("mode")), "{errs:?}");
        assert!(errs.iter().any(|m| m.contains("shape")), "{errs:?}");
    }

    #[test]
    fn store_decl_duplicate_collection_is_rejected() {
        let a = req("issue");
        let b = req("issue");
        let (errs, _) = r12_msgs(&store_pkg(vec![a, b]));
        assert!(errs.iter().any(|m| m.contains("出现了两次")), "{errs:?}");
    }

    #[test]
    fn store_decl_data_snapshot_cannot_write() {
        // 快照是只读的：一份 kb-seed 包不该声明它能写集合
        let dir = temp_pkg("storewrite");
        let mut p = data_pkg("kb-seed", &dir);
        let mut r = req("issue");
        r.mode = "readwrite".into();
        p.state = Some(StateDecl { stores: vec![r], ..Default::default() });
        let issues = validate(&p, &dir, None, false);
        assert!(
            issues
                .iter()
                .any(|i| i.rule == "R12" && i.msg.contains("不该写集合")),
            "{issues:?}"
        );
        // 只读就没事
        let mut p2 = data_pkg("kb-seed", &dir);
        p2.state = Some(StateDecl { stores: vec![req("issue")], ..Default::default() });
        let issues2 = validate(&p2, &dir, None, false);
        assert!(
            !issues2.iter().any(|i| i.rule == "R12" && i.msg.contains("不该写集合")),
            "{issues2:?}"
        );
    }

    #[test]
    fn store_decl_write_only_warns_but_does_not_block() {
        let mut r = req("audit");
        r.mode = "write".into();
        let (errs, warns) = r12_msgs(&store_pkg(vec![r.clone()]));
        assert!(errs.is_empty(), "{errs:?}");
        assert!(warns.iter().any(|m| m.contains("拿不回自己写的内容")), "{warns:?}");
        // 说清了理由就不吵
        r.reason = "只上报，从不回读".into();
        let (_, warns2) = r12_msgs(&store_pkg(vec![r]));
        assert!(warns2.is_empty(), "{warns2:?}");
    }

    #[test]
    fn store_decl_readwrite_also_warns_about_the_model_face() {
        // readwrite 在模型面上 = 「模型可以改这些内容」，所以也要让人写清理由
        let mut r = req("issue");
        r.mode = "readwrite".into();
        let (errs, warns) = r12_msgs(&store_pkg(vec![r.clone()]));
        assert!(errs.is_empty(), "{errs:?}");
        assert!(warns.iter().any(|m| m.contains("模型可以改这些内容")), "{warns:?}");
        r.reason = "认领问题单并回写进展".into();
        let (_, warns2) = r12_msgs(&store_pkg(vec![r]));
        assert!(warns2.is_empty(), "{warns2:?}");
        // 只读不吵
        let (_, warns3) = r12_msgs(&store_pkg(vec![req("log")]));
        assert!(warns3.is_empty(), "{warns3:?}");
    }

    #[test]
    fn store_decl_json_roundtrip() {
        // hur.json 是别的工具也会写的文件 —— 字段名必须稳定
        let mut r = req("run-log");
        r.mode = "write".into();
        r.fields = vec!["at:string!".into(), "ok:bool".into()];
        r.index = vec!["ok".into()];
        r.shape = "append-only".into();
        r.visibility = "private".into();
        r.max_bytes = 65536;
        r.reason = "运行日志".into();
        let pkg = store_pkg(vec![r]);
        let text = serde_json::to_string(&pkg).unwrap();
        assert!(text.contains("\"stores\""), "{text}");
        assert!(text.contains("append-only"), "{text}");
        let back: HurPackage = serde_json::from_str(&text).unwrap();
        let st = back.state.unwrap();
        assert_eq!(st.stores.len(), 1);
        assert_eq!(st.stores[0].collection, "run-log");
        assert_eq!(st.stores[0].mode_norm(), "write");
        assert!(st.stores[0].writes());
        assert_eq!(st.stores[0].max_bytes, 65536);
        assert_eq!(st.stores[0].index, vec!["ok".to_string()]);
        // 空 state 不该往包里写一堆空数组（省得每份 hur.json 都被撑开）
        let empty = serde_json::to_string(&StateDecl::default()).unwrap();
        assert!(!empty.contains("stores"), "{empty}");
    }
}
