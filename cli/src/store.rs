//! `ncc store` —— 通用记录仓的手（集合是声明，记录是数据）。
//!
//! 为什么要有这一层，而不是给 issue / log 各加一个子命令：
//! 知识库 / 记忆 / 检查点 / 轨迹是四类内容，机械部分却是同一件事（归属命名空间、按 key 取、
//! 分页、标签、可见性、版本、上限、过期、软删）。各写一遍就是抄四遍；再加一类内容再抄一遍。
//! 于是把**声明**（集合）与**数据**（记录）分开 —— 新增一类内容只需要声明一个集合。
//!
//! 三条边界（与节点侧的实现同源，这里只是把话说在客户端）：
//!   1. **动态 ≠ 无模式**：写之前先读集合声明，本地就按声明**类型化 + 校验**，
//!      不合规的请求不发出去（省一次往返，也把错误说在离人近的地方）。
//!   2. **不可变就是不可变**：`mutable=false` / `append_only=true` 的集合没有改这条路，
//!      本地就拒，不去试探服务端。
//!   3. **CRUD ≠ 授权**：能写自己的集合不等于能读别人的；这里只负责把凭据带上，
//!      判断在服务端（与其它状态同一套授权）。
//!
//! 字段过滤写成 `--where 字段=值`，线上是 `?f.字段=值` —— 加前缀是为了**不与保留参数撞车**：
//! 一个集合完全可以声明一个叫 `status` 的字段，而 `?status=` 又是记录状态。

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

use crate::api;
use crate::config::{self, CliConfig};

#[derive(Subcommand)]
pub enum StoreCmd {
    /// 这台节点上有哪些集合（一类内容 = 一个集合）
    Ls(StoreScopeArgs),
    /// 声明（或更新）一个集合 —— **新增一类内容不改服务端**
    Declare(StoreDeclareArgs),
    /// 列记录
    List(StoreListArgs),
    /// 取一条记录
    Get(StoreGetArgs),
    /// 写一条（新 key 建、旧 key 改；不可变集合会拒）
    Put(StorePutArgs),
    /// 归档一条记录（`--hard` 才真删）
    Rm(StoreRmArgs),
    /// 一条记录的改动历史（只记元数据）
    History(StoreHistoryArgs),
    /// 把集合声明导出来（一个文件一件，可进 git、可再 apply）
    Export(StoreExportArgs),
    /// 字段类型白名单 / 上限 / 三条不变量（离线可读）
    Kinds,
}

impl StoreCmd {
    /// 命令 → 它需要的能力。集合的声明在节点上，所以读写都要 `store`。
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            // 词表是本地常量，离线也能看
            StoreCmd::Kinds => None,
            _ => Some("store"),
        }
    }
}

#[derive(Args, Clone)]
pub struct StoreScopeArgs {
    /// 哪个命名空间（默认：我自己的）
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StoreDeclareArgs {
    /// 集合名（一类内容的名字：issue / log / note / …）。
    /// 用 `--file` / `--dir` 时可以不写（名字在文件里）
    #[arg(default_value = "")]
    pub kind: String,
    /// 从文件声明（一个 json：单件 / 数组 / `{"stores":[…]}` 都行）——
    /// **声明进 git、评审、再 apply** 才是它该有的样子
    #[arg(long, default_value = "")]
    pub file: String,
    /// 从目录里所有 `*.json` 逐件 apply（`ncc store export --dir` 写出来的那种布局）
    #[arg(long, default_value = "")]
    pub dir: String,
    /// 只看会改什么，**不改**（有差异时退出码=1，可以当 CI 门禁）
    #[arg(long)]
    pub check: bool,
    /// 显示名
    #[arg(long, default_value = "")]
    pub title: String,
    /// 摘要（一类内容一句话说清）
    #[arg(long, default_value = "")]
    pub summary: String,
    /// 为什么要有这一类内容（人看的；节点会存下来，导出时跟声明一起走）
    #[arg(long, default_value = "")]
    pub reason: String,
    /// 字段声明，可多次：`title:string!` · `status:enum:open|closed` · `labels:string[]`
    /// · `body:text?search` · `owner:ref`（`!` = 必填，`?search` = 进搜索文本）
    #[arg(long = "field", value_name = "声明")]
    pub fields: Vec<String>,
    /// 可过滤的字段（必须是已声明的字段；逗号分隔）
    #[arg(long, default_value = "")]
    pub index: String,
    /// 不可变（没有改这条路；审计/快照类内容用）
    #[arg(long)]
    pub immutable: bool,
    /// 只追加（能建、不能改）
    #[arg(long)]
    pub append_only: bool,
    /// 公开档（**默认私有**；要公开就得显式说）
    #[arg(long)]
    pub public: bool,
    /// 单条正文上限（字节；默认 256KB，硬上限 1MB）
    #[arg(long, default_value_t = 0)]
    pub max_bytes: i64,
    /// 同内容重复提交算 duplicate（幂等）：checksum
    #[arg(long, default_value = "")]
    pub dedupe_by: String,
    /// 默认存活天数（0 = 不过期）
    #[arg(long, default_value_t = 0)]
    pub ttl_days: i64,
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StoreListArgs {
    /// 集合名
    pub collection: String,
    /// 关键词匹配（空格分开的几个词都要出现；命中位置决定排序：key 3 / 标签与 ?search 字段 2 / 正文 1）
    /// —— 不是索引检索，也不是向量检索
    #[arg(long, default_value = "")]
    pub q: String,    /// 按声明过的字段过滤，可多次：`--where status=open`
    #[arg(long = "where", value_name = "字段=值")]
    pub where_: Vec<String>,
    /// 按标签过滤（可多次）
    #[arg(long)]
    pub tag: Vec<String>,
    /// key 前缀
    #[arg(long, default_value = "")]
    pub prefix: String,
    #[arg(long, default_value_t = 50)]
    pub size: usize,
    #[arg(long, default_value_t = 1)]
    pub page: usize,
    /// 连归档的一起列
    #[arg(long)]
    pub archived: bool,
    /// 连过期的也列出来（默认按"过期即不存在"）
    #[arg(long)]
    pub expired: bool,
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StoreGetArgs {
    pub collection: String,
    pub key: String,
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StorePutArgs {
    pub collection: String,
    pub key: String,
    /// 正文（也可用 --file / stdin）
    #[arg(long, default_value = "")]
    pub body: String,
    #[arg(long)]
    pub file: Option<String>,
    /// 声明过的字段：`--field status=open`；数组用逗号 `--field labels=bug,backend`
    #[arg(long = "field", value_name = "字段=值")]
    pub fields: Vec<String>,
    /// 自由元数据（**不过滤**，是逃生口不是索引）：`--meta k=v`
    #[arg(long = "meta", value_name = "k=v")]
    pub meta: Vec<String>,
    #[arg(long)]
    pub tag: Vec<String>,
    /// public（集合没声明公开档时会拒 —— 可见性是逐集合的事）
    #[arg(long)]
    pub public: bool,
    /// 存活天数（0 = 用集合默认值；集合也没有就不过期）
    #[arg(long, default_value_t = 0)]
    pub ttl_days: i64,
    /// 乐观并发：带上你看到的版本号，别人先改了就会冲突（不是静默覆盖）
    #[arg(long, default_value_t = 0)]
    pub revision: i64,
    /// 改动备注（进历史）
    #[arg(long, default_value = "")]
    pub note: String,
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StoreRmArgs {
    pub collection: String,
    pub key: String,
    /// 真删（默认只是归档：不列出但还在）
    #[arg(long)]
    pub hard: bool,
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StoreHistoryArgs {
    pub collection: String,
    pub key: String,
    #[arg(long, default_value = "")]
    pub namespace: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct StoreExportArgs {
    /// 写到这个文件（单件时用；多件请用 --dir）
    #[arg(long, default_value = "")]
    pub file: String,
    /// 写到这个目录（一件一个 `<集合>.json` —— 进 git 最好看的那种布局）
    #[arg(long, default_value = "")]
    pub dir: String,
    #[arg(long, default_value = "")]
    pub namespace: String,
    /// 打成一个 JSON（不给 --file/--dir 时，stdout 默认就是这个形态）
    #[arg(long)]
    pub json: bool,
}

/// 一份**集合声明**（节点与包共用同一个形状）。
///
/// 一个形状两种用法：
///   - 节点：`ncc store declare --file stores/issue.json` —— 这台节点**提供**什么；
///   - 包：`state.stores[]` —— 这个包**需要**什么（多一个 `mode`）。
///
/// 共用形状是有意的：不然"我提供什么"与"我需要什么"会长成两种写法，
/// 两边对不上的时候没人知道该信哪份。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionSpec {
    /// 集合名
    pub collection: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// 为什么有它（人看的）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// 字段声明（语法由节点解析 —— 这边不重写一套）
    #[serde(default)]
    pub fields: Vec<String>,
    /// 能过滤的字段（必须是已声明的）
    #[serde(default)]
    pub index: Vec<String>,
    /// `mutable` | `append-only`（不写 = mutable）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shape: String,
    /// `private` | `public`（不写 = private）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub visibility: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_bytes: i64,
    /// 同内容重复提交算 duplicate（幂等）：`checksum`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dedupe_by: String,
    /// 默认存活天数（0 = 不过期）
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ttl_days: i64,
    /// **只在包里**有意义：这个包需要拿它做什么（节点声明里不看它）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

impl CollectionSpec {
    /// 归一化后的形态（空 = mutable）。
    fn shape_norm(&self) -> &str {
        let s = self.shape.trim();
        if s.is_empty() {
            "mutable"
        } else {
            s
        }
    }

    /// 声明里有语义的那几项（用来跟节点上的现状对比算差异）。
    fn comparable(&self) -> Vec<(&'static str, String)> {
        vec![
            ("title", self.title.trim().to_string()),
            ("summary", self.summary.trim().to_string()),
            ("reason", self.reason.trim().to_string()),
            ("fields", self.fields.join(" ")),
            ("index", self.index.join(" ")),
            ("shape", self.shape_norm().to_string()),
            (
                "visibility",
                if self.visibility.trim().is_empty() {
                    "private".to_string()
                } else {
                    self.visibility.trim().to_string()
                },
            ),
            ("max_bytes", self.max_bytes.max(0).to_string()),
            ("dedupe_by", self.dedupe_by.trim().to_string()),
            ("ttl_days", self.ttl_days.max(0).to_string()),
        ]
    }

    /// → 节点声明接口的请求体。
    fn body(&self) -> Value {
        let mut b = json!({
            "kind": self.collection.trim(),
            "title": self.title.trim(),
            "summary": self.summary.trim(),
            "reason": self.reason.trim(),
            "fields": self.fields,
            "index": self.index,
            // 形态：`shape` 是两边共用的说法，接口那边是两个布尔（mutable / append_only）
            "mutable": self.shape_norm() != "append-only",
            "append_only": self.shape_norm() == "append-only",
            "visibility": if self.visibility.trim().is_empty() { "private" } else { self.visibility.trim() },
            "dedupe_by": self.dedupe_by.trim(),
            "default_ttl_days": self.ttl_days.max(0),
        });
        if self.max_bytes > 0 {
            b["max_bytes"] = json!(self.max_bytes);
        }
        b
    }

    /// 从节点上的集合声明反过来建一份（`export` 用）。
    fn from_node(col: &Value) -> CollectionSpec {
        let fields: Vec<String> = col["fields"]
            .as_array()
            .map(|fs| fs.iter().map(field_to_spec_string).collect())
            .unwrap_or_default();
        CollectionSpec {
            collection: col["kind"].as_str().unwrap_or("").to_string(),
            title: col["title"].as_str().unwrap_or("").to_string(),
            summary: col["summary"].as_str().unwrap_or("").to_string(),
            reason: col["reason"].as_str().unwrap_or("").to_string(),
            fields,
            index: col["index"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                .unwrap_or_default(),
            shape: if col["appendOnly"].as_bool().unwrap_or(false) { "append-only".into() } else { "mutable".into() },
            visibility: col["visibility"].as_str().unwrap_or("private").to_string(),
            max_bytes: col["maxBytes"].as_i64().unwrap_or(0),
            dedupe_by: col["dedupeBy"].as_str().unwrap_or("").to_string(),
            ttl_days: col["defaultTtlDays"].as_i64().unwrap_or(0),
            mode: String::new(),
        }
    }
}

/// 节点解析好的字段 → 声明字符串（`title:string!` / `status:enum:open|closed`）。
///
/// 为什么在客户端反着拼一遍：导出的文件要**能再 apply 回去**，而接口给的是解析后的
/// 结构。拼法与节点侧的解析器一一对应（`name:type[!]` / `enum` 带词表 / `string[]`），
/// 不引入第三套语法。保真验证：导出 → 再 apply → 应当"无差异"（冒烟里查这个）。
fn field_to_spec_string(f: &Value) -> String {
    let name = f["name"].as_str().unwrap_or("");
    let typ = f["type"].as_str().unwrap_or("string");
    let mut s = match typ {
        // ⚠️ `enum:` 这个前缀不能省：声明语法是 `status:enum:open|closed`，
        // 少写就成了类型 `open|closed` —— 节点那边会拒（不在类型白名单）。
        // 导出必须能**原样 apply 回去**，而不是“看起来像”。
        "enum" => {
            let vals: Vec<&str> = f["enum"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            format!("{name}:enum:{}", vals.join("|"))
        }
        other => format!("{name}:{other}"),
    };
    if f["search"].as_bool().unwrap_or(false) {
        s.push_str("?search");
    }
    if f["require"].as_bool().unwrap_or(false) {
        s.push('!');
    }
    s
}

/* ============================ 实现 ============================ */

pub fn run(cfg: &CliConfig, cmd: &StoreCmd) -> Result<()> {
    match cmd {
        StoreCmd::Kinds => kinds(),
        StoreCmd::Ls(a) => list_collections(cfg, a),
        StoreCmd::Declare(a) => declare(cfg, a),
        StoreCmd::List(a) => list_records(cfg, a),
        StoreCmd::Get(a) => get_record(cfg, a),
        StoreCmd::Put(a) => put_record(cfg, a),
        StoreCmd::Rm(a) => rm_record(cfg, a),
        StoreCmd::History(a) => history(cfg, a),
        StoreCmd::Export(a) => export(cfg, a),
    }
}

fn ns_qs(namespace: &str) -> String {
    let n = namespace.trim().trim_start_matches('@');
    if n.is_empty() {
        String::new()
    } else {
        format!("?namespace={}", api::urlenc(n))
    }
}

/// 词表与上限（与节点侧 `GET /api/store/kinds` 同一份口径，这里是**离线**那份）。
fn kinds() -> Result<()> {
    println!("字段声明（`--field` 可多次）：");
    println!("  title:string          必填？在类型后加 `!`      → title:string!");
    println!("  status:enum:open|closed|triaged                 （值必须在词表里）");
    println!("  labels:string[]       字符串数组（过滤用 --where labels=bug）");
    println!("  body:text?search      进搜索文本（`?search`），--q 能搜到它");
    println!("  owner:ref             引用：@某人 或 @命名空间/slug");
    println!("  类型白名单：string / text / int / bool / string[] / ref / enum:a|b|c\n");
    println!("检索：`--q` 是**关键词匹配**（空格分开的几个词都要出现），命中位置决定排序");
    println!("      （key 3 / 标签与 ?search 字段 2 / 正文 1）。不是索引检索、更不是向量检索。");
    println!("      排序在最多 500 条候选上做（超出按更新时间截断）——这条取舍是明说的。\n");
    println!("上限：正文默认 256KB、硬上限 1MB（通用仓只存**内联文本**，大对象走制品或检查点的 blob）");
    println!("      字段 ≤32 个、单集合记录 ≤100000 条\n");
    println!("三条不变量（服务端与客户端都按这个来）：");
    println!("  ① 没在 --field 里声明的字段**写不进来**（想放自由结构请用 --meta，但 meta 不可过滤）");
    println!("  ② 没在 --index 里声明的字段**不能当过滤条件**");
    println!("  ③ --immutable / --append-only 的集合**没有改这条路**（PUT 会被拒）");
    println!("  另：集合没声明 --public 时，里面的记录**永远**不会匿名可见（记录自己写 public 也不算）");
    Ok(())
}

fn list_collections(cfg: &CliConfig, a: &StoreScopeArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let d = api::get(cfg, &format!("/api/store{}", ns_qs(&a.namespace)), token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let cols = d["collections"].as_array().cloned().unwrap_or_default();
    println!(
        "{:<14} {:>7} {:<10} {:<10} 字段 / 可过滤",
        "集合", "记录", "形态", "可见性"
    );
    for c in &cols {
        let shape = if c["mutable"].as_bool().unwrap_or(false) { "可变" } else { "不可变" };
        let mut fields: Vec<String> = c["fields"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| x["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        fields.truncate(4);
        println!(
            "{:<14} {:>7} {:<10} {:<10} {}",
            if c["builtin"].as_bool().unwrap_or(false) {
                format!("{}·", c["kind"].as_str().unwrap_or(""))
            } else {
                c["kind"].as_str().unwrap_or("").to_string()
            },
            c["records"].as_i64().unwrap_or(0),
            shape,
            c["visibility"].as_str().unwrap_or(""),
            if fields.is_empty() { "（没声明字段）".to_string() } else { fields.join(",") }
        );
        if let Some(s) = c["summary"].as_str().filter(|s| !s.is_empty()) {
            println!("      {s}");
        }
    }
    // 内置目录：`ls` 要能回答「这台节点上有什么内容」——
    // 四类内置内容在没人用过之前是"还没有行"的，只有列出来的 collections 会漏掉它们。
    let bs = d["builtins"].as_array().cloned().unwrap_or_default();
    let unused: Vec<String> = bs
        .iter()
        .filter(|b| !b["present"].as_bool().unwrap_or(false))
        .map(|b| format!("{}", b["kind"].as_str().unwrap_or("")))
        .collect();
    println!();
    println!(
        "内置（名字保留，节点自己用）：{}",
        bs.iter()
            .filter_map(|b| b["kind"].as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    );
    if !unused.is_empty() {
        println!("   其中 {} 这台节点还没用过 —— 用过之后会出现在上面（取用即声明）", unused.join(" / "));
    }
    if cols.is_empty() {
        println!("   （还没有你自己声明的集合）声明一个：ncc store declare issue --field title:string! --index title");
    }
    Ok(())
}

fn declare(cfg: &CliConfig, a: &StoreDeclareArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    // 声明可以从文件来（一件 / 一组 / 一个目录）——**这才是它该有的样子**：
    // 进 git、被评审、apply 到多台节点。
    let specs = specs_from_args(a)?;
    if specs.is_empty() {
        bail!("没说声明什么：给集合名 + `--field …`，或用 `--file` / `--dir`（见 ncc store kinds）");
    }
    if !a.file.trim().is_empty() || !a.dir.trim().is_empty() {
        let want_mode: Vec<&str> = specs
            .iter()
            .filter(|s| !s.mode.trim().is_empty())
            .map(|s| s.collection.trim())
            .collect();
        if !want_mode.is_empty() {
            println!(
                "注：`{}` 带 `mode` —— 那是**包的需求**（这个包要读还是要写），节点这边不看它；\n     \
                 节点声明的是「提供什么」，包的 `mode` 由 `nur verify` / 模型面去管。\n",
                want_mode.join(" / ")
            );
        }
    }
    let ns = a.namespace.trim().trim_start_matches('@').to_string();
    let existing = node_collections(cfg, &token, &ns)?;
    let (mut created, mut same, mut changed_n) = (0, 0, 0);
    for spec in &specs {
        let kind = spec.collection.trim();
        if kind.is_empty() {
            bail!("声明里没有 collection（每一件都要有名字）");
        }
        // 内置名字保留（与节点侧同一条理由：同一个名字不能有两种内容）。
        // 本地就拦，省一次往返。
        if matches!(kind, "kb" | "mem" | "ckpt" | "trace") {
            bail!(
                "「{kind}」是节点的内置集合名（kb / mem / ckpt / trace 由节点自己在用）—— \
                 另起一个名字，比如 {kind}-mine"
            );
        }
        // 本地先判能离线判的那条：**说了能过滤的字段必须真的声明过**
        // （与包的 R12 同一条口径 —— 到节点上必然拒的事，别等人跑一趟才知道）
        let declared: Vec<&str> = spec.fields.iter().filter_map(|f| f.split(':').next()).map(str::trim).collect();
        let stray: Vec<&str> = spec.index.iter().map(|s| s.trim()).filter(|i| !i.is_empty() && !declared.contains(i)).collect();
        if !stray.is_empty() {
            bail!(
                "集合「{kind}」想按 {} 过滤，但 fields 里没声明 —— 没声明的字段在节点上过滤不了（动态 ≠ 无模式）",
                stray.join(" / ")
            );
        }
        let cur = existing.iter().find(|c| c["kind"].as_str() == Some(kind));
        let diff: Vec<String> = match cur {
            None => Vec::new(),
            Some(c) => changed_keys(spec, c),
        };
        let what = if cur.is_none() {
            created += 1;
            "新建".to_string()
        } else if diff.is_empty() {
            same += 1;
            "不变".to_string()
        } else {
            changed_n += 1;
            format!("更新（{} 变了）", diff.join("、"))
        };
        if a.check {
            println!("  · {kind}  {what}");
            continue;
        }
        let mut body = spec.body();
        if !ns.is_empty() {
            body["namespace"] = json!(format!("@{ns}"));
        }
        let d = api::post_json(cfg, "/api/store", Some(&token), &body)?;
        let verb = if d["created"].as_bool().unwrap_or(false) { "已声明" } else { "已更新" };
        if a.json {
            println!("{}", serde_json::to_string_pretty(&d)?);
        } else if specs.len() == 1 {
            // 单件：把形状与下一步给出来（不然使用者还得再跑一次 ls 才知道是什么样）
            let c = &d["collection"];
            println!(
                "✅ {verb} 集合 {}  · {}  · {}",
                c["ref"].as_str().unwrap_or(kind),
                if c["mutable"].as_bool().unwrap_or(false) { "可变（留历史）" } else { "不可变（没有改这条路）" },
                c["visibility"].as_str().unwrap_or("private")
            );
            let names: Vec<&str> = c["fields"]
                .as_array()
                .map(|fs| fs.iter().filter_map(|f| f["name"].as_str()).collect())
                .unwrap_or_default();
            if !names.is_empty() {
                println!("   字段      {}", names.join(", "));
            }
            let idx: Vec<&str> = c["index"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            if !idx.is_empty() {
                println!("   可过滤    {}", idx.join(", "));
            }
            println!("   写一条    ncc store put {} <key> --body '…' --field …", kind);
        } else {
            println!("✅ {verb} 集合 {} · {what}", d["collection"]["ref"].as_str().unwrap_or(kind));
        }
    }
    if a.check && !a.json {
        println!(
            "\n--check：{} 新建 · {} 更新 · {} 不变（**什么都没改**；有差异退出码 1）",
            created, changed_n, same
        );
    } else if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "applied": specs.len(), "created": created, "updated": changed_n, "unchanged": same,
                "checked": a.check,
            }))?
        );
    }
    if a.check && (created > 0 || changed_n > 0) {
        std::process::exit(1);
    }
    Ok(())
}

/// 从参数（命令行 / 文件 / 目录）凑出声明表。
fn specs_from_args(a: &StoreDeclareArgs) -> Result<Vec<CollectionSpec>> {
    let mut out = Vec::new();
    if !a.file.trim().is_empty() {
        out.extend(specs_from_file(a.file.trim())?);
    }
    if !a.dir.trim().is_empty() {
        let d = std::path::Path::new(a.dir.trim());
        if !d.is_dir() {
            bail!("{} 不是目录", d.display());
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(d)
            .with_context(|| format!("读不了目录 {}", d.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        files.sort();
        if files.is_empty() {
            bail!("{} 里没有 *.json", d.display());
        }
        for f in files {
            out.extend(specs_from_file(&f.to_string_lossy())?);
        }
    }
    if out.is_empty() && !a.kind.trim().is_empty() {
        // 命令行形态（与 --file 可叠加，但一般二选一）
        let index: Vec<String> = a
            .index
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        out.push(CollectionSpec {
            collection: a.kind.trim().to_string(),
            title: a.title.trim().to_string(),
            summary: a.summary.trim().to_string(),
            reason: a.reason.trim().to_string(),
            fields: a.fields.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
            index,
            shape: if a.append_only {
                "append-only".into()
            } else if a.immutable {
                // 不可变 = 只追加（没有"改"这条路）——两个 flag 归一到同一个形状
                "append-only".into()
            } else {
                "mutable".into()
            },
            visibility: if a.public { "public".into() } else { "private".into() },
            max_bytes: a.max_bytes.max(0),
            dedupe_by: a.dedupe_by.trim().to_string(),
            ttl_days: a.ttl_days.max(0),
            mode: String::new(),
        });
    }
    Ok(out)
}

/// 读一个声明文件：单件 / 数组 / `{"stores":[…]}` / `{"collections":[…]}` 都收。
///
/// 收 `stores` 这一种形态是有意的：**包里的 `state.stores[]` 能直接喂进来**
/// （把一个包需要的集合在这台节点上落地），不用人手工翻译一遍。
/// 连 `{"state":{"stores":[…]}}`（也就是 `hur.json` 本身）也收 ——
/// 「把这个包需要的集合落地」应该是 `--file hur.json` 而不是先抄一遍。
fn specs_from_file(path: &str) -> Result<Vec<CollectionSpec>> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("读不了 {path}"))?;
    let v: Value = serde_json::from_str(&raw)
        .with_context(|| format!("{path} 不是合法 JSON（声明文件就是 json）"))?;
    let list_at = |x: &Value| -> Option<Vec<Value>> {
        x.get("stores")
            .or_else(|| x.get("collections"))
            .and_then(|y| y.as_array())
            .cloned()
    };
    let arr: Vec<Value> = if v.is_array() {
        v.as_array().cloned().unwrap_or_default()
    } else if let Some(list) = list_at(&v).or_else(|| v.get("state").and_then(list_at)) {
        list
    } else {
        vec![v]
    };
    let mut out = Vec::new();
    for item in arr {
        let s: CollectionSpec = serde_json::from_value(item)
            .with_context(|| format!("{path} 里有一件声明读不出来（要 `collection` + `fields` 那些字段）"))?;
        out.push(s);
    }
    Ok(out)
}

/// 与节点上现状的差异（只说**哪几项**变了）。
fn changed_keys(want: &CollectionSpec, cur: &Value) -> Vec<String> {
    let now = CollectionSpec::from_node(cur);
    let mut now_map = Map::new();
    for (k, v) in now.comparable() {
        now_map.insert(k.to_string(), json!(v));
    }
    let mut out = Vec::new();
    for (k, v) in want.comparable() {
        let cur_v = now_map.get(k).cloned().unwrap_or(json!(""));
        if cur_v.as_str() != Some(v.as_str()) {
            out.push(k.to_string());
        }
    }
    out
}

/// 取这台节点上的集合声明（拿 token；失败就是空 —— **不猜**）。
fn node_collections(cfg: &CliConfig, token: &str, ns: &str) -> Result<Vec<Value>> {
    let path = if ns.is_empty() {
        "/api/store".to_string()
    } else {
        format!("/api/store?namespace={}", api::urlenc(ns))
    };
    let d = api::get(cfg, &path, Some(token))?;
    Ok(d["collections"].as_array().cloned().unwrap_or_default())
}

/// 把集合声明导出来：一件一个文件（进 git 最好看），或打成一份 JSON。
fn export(cfg: &CliConfig, a: &StoreExportArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    let ns = a.namespace.trim().trim_start_matches('@').to_string();
    let cols = node_collections(cfg, &token, &ns)?;
    // 内置集合不进导出：它们的声明是**代码里的常量**（`model.BuiltinCollections`），
    // 导出成文件然后 apply 回去只会得到一个 400（名字保留）—— 那就是在造一个假动作。
    let user_cols: Vec<Value> = cols
        .iter()
        .filter(|c| !c["builtin"].as_bool().unwrap_or(false))
        .cloned()
        .collect();
    let skipped = cols.len() - user_cols.len();
    let specs: Vec<CollectionSpec> = user_cols.iter().map(CollectionSpec::from_node).collect();
    let mut buf = String::new();
    if !a.file.trim().is_empty() {
        std::fs::write(a.file.trim(), serde_json::to_string_pretty(&specs)?)
            .with_context(|| format!("写不了 {}", a.file.trim()))?;
        buf.push_str(&format!("✅ 已导出 {} 件声明 → {}\n", specs.len(), a.file.trim()));
    } else if !a.dir.trim().is_empty() {
        std::fs::create_dir_all(a.dir.trim()).with_context(|| format!("建不了 {}", a.dir.trim()))?;
        for s in &specs {
            let p = std::path::Path::new(a.dir.trim()).join(format!("{}.json", s.collection));
            let mut one = serde_json::to_string_pretty(s)?;
            one.push('\n');
            std::fs::write(&p, one).with_context(|| format!("写不了 {}", p.display()))?;
        }
        // 一句 README：告诉人这些文件怎么用（它们本身就该进版本控制）
        let readme = "# 集合声明（可进版本控制）\n\n\
            这些文件是这台节点上「提供哪些集合」的声明本身 —— 改了要评审，改完 apply：\n\n\
            ```bash\n\
            ncc store declare --dir . --check   # 先看会改什么（有差异退出码 1，可当 CI 门禁）\n\
            ncc store declare --dir .           # 再改\n\
            ncc store export --dir .            # 把节点上的现状再导回来\n\
            ```\n";
        std::fs::write(std::path::Path::new(a.dir.trim()).join("README.md"), readme)?;
        buf.push_str(&format!("✅ 已导出 {} 件声明 → {}/（一件一个文件 + README.md）\n", specs.len(), a.dir.trim()));
    } else {
        // stdout 是**机器可读**的那份（管道进别的工具），所以人看的说明一律走 stderr ——
        // 之前把说明写在 stdout 后面，等于给 JSON 尾巴上粘一句话，调用方一解析就炸。
        println!("{}", serde_json::to_string_pretty(&specs)?);
        if skipped > 0 {
            eprintln!(
                "（跳过了 {skipped} 个内置集合：kb/mem/ckpt/trace 的声明在节点代码里，不是配置）"
            );
        }
        return Ok(());
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "exported": specs.len(), "file": a.file, "dir": a.dir }))?
        );
    } else {
        print!("{buf}");
        if specs.is_empty() {
            println!("（这台范围里还没有你自己声明的集合）");
        }
        // 说清导的是什么、不是什么 —— 免得有人以为这是备份
        println!("   导的是**声明**（有哪些集合、什么字段、能不能改、给谁看），不是记录本身。");
        if skipped > 0 {
            println!("   跳过了 {skipped} 个**内置集合**（kb/mem/ckpt/trace 那些）—— 它们的声明在节点代码里，不是配置。");
        }
    }
    Ok(())
}

fn list_records(cfg: &CliConfig, a: &StoreListArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let mut qs: Vec<String> = Vec::new();
    let ns = a.namespace.trim().trim_start_matches('@');
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(ns)));
    }
    if !a.q.trim().is_empty() {
        qs.push(format!("q={}", api::urlenc(a.q.trim())));
    }
    for w in &a.where_ {
        let (k, v) = w
            .split_once('=')
            .ok_or_else(|| anyhow!("--where 要写成 字段=值（收到「{w}」）"))?;
        qs.push(format!("f.{}={}", api::urlenc(k.trim()), api::urlenc(v.trim())));
    }
    for t in &a.tag {
        qs.push(format!("tag={}", api::urlenc(t)));
    }
    if !a.prefix.trim().is_empty() {
        qs.push(format!("prefix={}", api::urlenc(a.prefix.trim())));
    }
    if a.archived {
        qs.push("archived=1".into());
    }
    if a.expired {
        qs.push("expired=1".into());
    }
    qs.push(format!("page={}", a.page.max(1)));
    qs.push(format!("size={}", a.size.clamp(1, 200)));
    let d = api::get(
        cfg,
        &format!("/api/store/{}?{}", api::urlenc(a.collection.trim()), qs.join("&")),
        token.as_deref(),
    )?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d["records"].as_array().cloned().unwrap_or_default();
    println!(
        "{} 共 {} 条（列 {} 条）",
        d["collection"].as_str().unwrap_or(""),
        d["total"].as_i64().unwrap_or(0),
        rows.len()
    );
    for r in &rows {
        let expired = r["expiresAt"].as_str().map(|s| format!(" 过期 {s}")).unwrap_or_default();
        println!(
            "  {:<28} rev {:<3} {:>8}  {}{}",
            r["key"].as_str().unwrap_or(""),
            r["revision"].as_i64().unwrap_or(0),
            r["size"].as_i64().unwrap_or(0),
            r["visibility"].as_str().unwrap_or(""),
            expired
        );
        if let Some(f) = r["fields"].as_object().filter(|m| !m.is_empty()) {
            let pairs: Vec<String> = f
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect();
            println!("      {}", pairs.join(" · "));
        }
    }
    Ok(())
}

/// 读集合声明（写之前本地就按它类型化 + 校验 —— 省一次往返，错误也说在人近处）。
fn fetch_collection(cfg: &CliConfig, token: &str, collection: &str, namespace: &str) -> Result<Value> {
    let ns = namespace.trim().trim_start_matches('@');
    let path = if ns.is_empty() {
        "/api/store".to_string()
    } else {
        format!("/api/store?namespace={}", api::urlenc(ns))
    };
    let d = api::get(cfg, &path, Some(token))?;
    let kind = collection.trim();
    d["collections"]
        .as_array()
        .and_then(|v| v.iter().find(|c| c["kind"].as_str() == Some(kind)).cloned())
        .ok_or_else(|| {
            anyhow!(
                "没找到集合「{kind}」的声明（先 ncc store ls 看看，或 ncc store declare {kind} --field …）"
            )
        })
}

/// 按集合声明把一个字符串值类型化成 JSON 值。
fn coerce(field: &Value, raw: &str) -> Result<Value> {
    let typ = field["type"].as_str().unwrap_or("string");
    Ok(match typ {
        "int" => json!(raw.trim().parse::<i64>().map_err(|_| anyhow!("要整数，收到「{raw}」"))?),
        "bool" => json!(match raw.trim() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => bail!("要布尔（true/false），收到「{raw}」"),
        }),
        "string[]" => json!(raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()),
        "enum" => {
            let allowed: Vec<String> = field["enum"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                .unwrap_or_default();
            if !allowed.iter().any(|x| x == raw.trim()) {
                bail!("只能是 {}（收到「{raw}」）", allowed.join(" / "));
            }
            json!(raw.trim())
        }
        _ => json!(raw),
    })
}

fn put_record(cfg: &CliConfig, a: &StorePutArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    let col = fetch_collection(cfg, &token, &a.collection, &a.namespace)?;
    let specs = col["fields"].as_array().cloned().unwrap_or_default();

    // 正文：--file > --body > stdin
    let body = match &a.file {
        Some(p) => std::fs::read_to_string(p).with_context(|| format!("读不了 {p}"))?,
        None if !a.body.is_empty() => a.body.clone(),
        None => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                String::new()
            } else {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).context("读 stdin 失败")?;
                s
            }
        }
    };
    let max = col["maxBytes"].as_i64().unwrap_or(256 << 10);
    if body.len() as i64 > max {
        bail!("正文 {} 字节，超过这个集合的上限 {max}（大对象走制品或检查点的 blob）", body.len());
    }

    // 字段：本地按声明**类型化 + 校验**（没声明的一律不进 fields —— 那是 meta 的位置）
    let mut vals = Map::new();
    for kv in &a.fields {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow!("--field 要写成 字段=值（收到「{kv}」）"))?;
        let k = k.trim();
        let spec = specs
            .iter()
            .find(|f| f["name"].as_str() == Some(k))
            .ok_or_else(|| anyhow!("字段「{k}」没在集合声明里 —— 要么加进声明（ncc store declare），要么放进 --meta（meta 不过滤）"))?;
        vals.insert(k.to_string(), coerce(spec, v)?);
    }
    for spec in &specs {
        let name = spec["name"].as_str().unwrap_or("");
        let require = spec["require"].as_bool().unwrap_or(false);
        if require && !vals.contains_key(name) {
            bail!("字段「{name}」是必填的");
        }
    }
    let mut meta = Map::new();
    for kv in &a.meta {
        if let Some((k, v)) = kv.split_once('=') {
            meta.insert(k.trim().to_string(), json!(v.trim()));
        }
    }

    let mut req = json!({
        "key": a.key.trim(),
        "body": body,
        "fields": Value::Object(vals),
        "tags": a.tag,
        "visibility": if a.public { "public" } else { "private" },
    });
    if !meta.is_empty() {
        req["meta"] = Value::Object(meta);
    }
    if a.ttl_days > 0 {
        req["ttl_days"] = json!(a.ttl_days);
    }
    if a.revision > 0 {
        req["revision"] = json!(a.revision);
    }
    if !a.note.trim().is_empty() {
        req["note"] = json!(a.note.trim());
    }
    let ns = a.namespace.trim().trim_start_matches('@');
    let path = if ns.is_empty() {
        format!("/api/store/{}", api::urlenc(a.collection.trim()))
    } else {
        format!("/api/store/{}?namespace={}", api::urlenc(a.collection.trim()), api::urlenc(ns))
    };
    // 有声明就能选对动词：**可变集合走 PUT**（带 revision 就是防覆盖的乐观并发），
    // **只追加 / 不可变集合只能走 POST** —— 服务端对它们的 PUT 一律拒，别去试探它。
    let mutable = col["mutable"].as_bool().unwrap_or(false);
    let d = if mutable {
        let put_path = if ns.is_empty() {
            format!("/api/store/{}/{}", api::urlenc(a.collection.trim()), api::urlenc(a.key.trim()))
        } else {
            format!(
                "/api/store/{}/{}?namespace={}",
                api::urlenc(a.collection.trim()),
                api::urlenc(a.key.trim()),
                api::urlenc(ns)
            )
        };
        api::request(cfg, "PUT", &put_path, Some(&token), Some(&req), None, &[])?
    } else {
        api::post_json(cfg, &path, Some(&token), &req)?
    };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let r = &d["record"];
    println!(
        "{} {}/{}  rev {}{}",
        if d["created"].as_bool().unwrap_or(false) { "✅ 已建" } else { "✅ 已更新" },
        d["collection"].as_str().unwrap_or(""),
        r["key"].as_str().unwrap_or(""),
        r["revision"].as_i64().unwrap_or(0),
        if d["duplicate"].as_bool().unwrap_or(false) { "（内容没变，未刷版本）" } else { "" }
    );
    if !mutable {
        println!("   这个集合不可变，改不了 —— 要留新内容请用新的 key");
    }
    Ok(())
}

fn get_record(cfg: &CliConfig, a: &StoreGetArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let ns = a.namespace.trim().trim_start_matches('@');
    let path = if ns.is_empty() {
        format!("/api/store/{}/{}", api::urlenc(a.collection.trim()), api::urlenc(a.key.trim()))
    } else {
        format!(
            "/api/store/{}/{}?namespace={}",
            api::urlenc(a.collection.trim()),
            api::urlenc(a.key.trim()),
            api::urlenc(ns)
        )
    };
    let d = api::get(cfg, &path, token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d["record"])?);
        return Ok(());
    }
    let r = &d["record"];
    println!("{}/{}  rev {}", d["collection"].as_str().unwrap_or(""), r["key"].as_str().unwrap_or(""), r["revision"].as_i64().unwrap_or(0));
    println!("  {:<10} {}", "可见性", r["visibility"].as_str().unwrap_or(""));
    println!("  {:<10} {}", "摘要", r["checksum"].as_str().unwrap_or(""));
    if let Some(f) = r["fields"].as_object().filter(|m| !m.is_empty()) {
        for (k, v) in f {
            println!("  {k:<10} {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
        }
    }
    if r["expired"].as_bool().unwrap_or(false) {
        println!("  ⚠️ 已过期（按\"过期即不存在\"的口径，它其实读不到了）");
    }
    println!();
    print!("{}", r["body"].as_str().unwrap_or(""));
    if !r["body"].as_str().unwrap_or("").ends_with('\n') {
        println!();
    }
    Ok(())
}

fn rm_record(cfg: &CliConfig, a: &StoreRmArgs) -> Result<()> {
    let token = config::require_token(cfg)?;
    let ns = a.namespace.trim().trim_start_matches('@');
    let mut path = format!(
        "/api/store/{}/{}",
        api::urlenc(a.collection.trim()),
        api::urlenc(a.key.trim())
    );
    let mut qs: Vec<String> = Vec::new();
    if !ns.is_empty() {
        qs.push(format!("namespace={}", api::urlenc(ns)));
    }
    if a.hard {
        qs.push("hard=1".into());
    }
    if !qs.is_empty() {
        path.push('?');
        path.push_str(&qs.join("&"));
    }
    let d = api::request(cfg, "DELETE", &path, Some(&token), None, None, &[])?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    if d["deleted"].as_bool().unwrap_or(false) {
        println!("✅ 已删除 {}/{}", a.collection.trim(), a.key.trim());
    } else {
        println!("✅ 已归档 {}/{}（默认不列出；?archived=1 还能看）", a.collection.trim(), a.key.trim());
    }
    if let Some(n) = d["note"].as_str() {
        println!("   {n}");
    }
    Ok(())
}

fn history(cfg: &CliConfig, a: &StoreHistoryArgs) -> Result<()> {
    let token = config::token_opt(cfg);
    let ns = a.namespace.trim().trim_start_matches('@');
    let path = if ns.is_empty() {
        format!(
            "/api/store/{}/{}/history",
            api::urlenc(a.collection.trim()),
            api::urlenc(a.key.trim())
        )
    } else {
        format!(
            "/api/store/{}/{}/history?namespace={}",
            api::urlenc(a.collection.trim()),
            api::urlenc(a.key.trim()),
            api::urlenc(ns)
        )
    };
    let d = api::get(cfg, &path, token.as_deref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("{}/{} 当前 rev {}", a.collection.trim(), a.key.trim(), d["currentRevision"].as_i64().unwrap_or(0));
    // 备注**跟着版本走**：每一行说的是"这个版本是带着什么备注写进来的"。
    for h in d["history"].as_array().cloned().unwrap_or_default() {
        println!(
            "  rev {:<4} {:>8}  {:<16} {}  {}",
            h["revision"].as_i64().unwrap_or(0),
            h["size"].as_i64().unwrap_or(0),
            h["changedBy"].as_str().unwrap_or(""),
            h["checksum"].as_str().unwrap_or(""),
            h["note"].as_str().map(|n| format!("（{n}）")).unwrap_or_default()
        );
    }
    if let Some(n) = d["currentNote"].as_str().filter(|s| !s.is_empty()) {
        let cur = d["currentRevision"].as_i64().unwrap_or(0);
        println!("  rev {cur:<4} （当前版本）                            （{n}）");
    }
    if let Some(n) = d["note"].as_str() {
        println!("   {n}");
    }
    Ok(())
}
