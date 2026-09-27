// ncc trace：Agent / HUR 的**运行轨迹**（采集 → 本地暂存 → 上传 → 评测 / 训练数据集）。
//
//      add     采集：把一份/一批轨迹收进本地暂存（原生文档 / HUR 执行留痕 / 事件流）
//      ls      列出本地（--remote 列服务端）轨迹
//      show    看一条的全文
//      push    把本地轨迹上传到当前目标节点
//      stats   聚合结论：成功率 / 耗时 / token / 花费 / 标注分布（能力评估）
//      export  导出数据集（JSONL，带数据集摘要）—— 后训练的输入
//      label   打评测标注（grade / reward / score / task / split / failure / note）
//      kinds   词表与上限
//      rm      删本地（--remote 删服务端）的一条
//      status  本地暂存状态
//
// 三条红线（与 `ncc-registry` 的 model/trace.go 同一套，别在一端悄悄放宽）：
//
//   1. **内容默认不上传**：轨迹天然带提示词、工具入参、模型输出。所以每条轨迹声明
//      `payload` 级别：`digest`（默认，只有哈希与结构）/ `preview`（截断预览，512 字节）/
//      `full`（原文）。级别由**采集方**（也就是这条命令）决定，服务端只如实记录。
//   2. **本地先落盘，上传是显式动作**：`add` 只写本机；`push` 才联网。没有"后台偷偷上报"。
//   3. **摘要跨语言一致**：`trace_digest_core` 与 Go 侧 `model.TraceDigestCore`
//      逐字节相同（有测试钉住），否则服务端会以 `digest_mismatch` 拒收。
//      因此**内容不含浮点**：金额用微美元整数（`costUsdMicros`），得分用千分位整数。
//
// 为什么摘要不覆盖原文：脱敏会改原文、原文可能很大；摘要是"这条轨迹是不是我发的那条"
// 与幂等去重用的。原文级别记在 `payload` 里，谁采集谁负责。
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use hur_core::spec;

use crate::api;
use crate::config::{self, CliConfig};

/* ============================ 规范与词表 ============================ */

pub const TRACE_SPEC: &str = "ncc-trace/v1";
/// HUR 执行留痕（`ncc hur run` 写的那份）：trace 是它的**收敛格式**。
pub const RUN_TRACE_SPEC: &str = "harness-use-run-trace/v1";
pub const DATASET_SPEC: &str = "ncc-trace-dataset/v1";

pub const KINDS: [&str; 2] = ["hur-run", "agent"];
pub const STATUSES: [&str; 3] = ["ok", "error", "cancelled"];
pub const PAYLOADS: [&str; 3] = ["digest", "preview", "full"];
pub const PREVIEW_MAX: usize = 512;
pub const MAX_STEPS: usize = 2000;
pub const BATCH_MAX: usize = 500;
pub const STEP_TYPES: [&str; 5] = ["llm", "tool", "io", "note", "guard"];
pub const GRADES: [&str; 3] = ["pass", "fail", "partial"];
pub const SPLITS: [&str; 3] = ["train", "eval", "holdout"];

/* ============================ 参数 ============================ */

#[derive(clap::Subcommand)]
pub enum TraceCmd {
    /// 采集：把一份/一批轨迹收进本地暂存（原生文档 / HUR 执行留痕 / 事件流）
    Add(AddArgs),
    /// 列出轨迹（默认本地；--remote 列服务端的）
    Ls(LsArgs),
    /// 看一条轨迹的全文（本地或 --remote）
    Show(ShowArgs),
    /// 把本地采集的轨迹上传到当前目标节点（幂等：重复上传只算 duplicates）
    Push(PushArgs),
    /// 聚合结论：成功率 / 耗时 / token / 花费 / 标注分布（能力评估）
    Stats(StatsArgs),
    /// 导出数据集（JSONL）：本地或 --remote（后者带数据集摘要）
    Export(ExportArgs),
    /// 打评测标注（grade / reward / score / task / split / failure / note）
    Label(LabelArgs),
    /// 词表与上限（本地常量；--remote 取服务端的）
    Kinds(KindsArgs),
    /// 删一条轨迹（本地；--remote 删服务端的）
    Rm(RmArgs),
    /// 本地暂存状态：有多少条、传了多少、目录在哪
    Status,
}

impl TraceCmd {
    /// 命令 → 需要的能力。纯本地的子命令返回 None（离线也能用）。
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            TraceCmd::Push(_) => Some("trace"),
            TraceCmd::Ls(a) => a.remote.then_some("trace"),
            TraceCmd::Show(a) => a.remote.then_some("trace"),
            TraceCmd::Stats(a) => a.remote.then_some("trace"),
            TraceCmd::Export(a) => a.remote.then_some("trace"),
            TraceCmd::Label(a) => a.remote.then_some("trace"),
            TraceCmd::Rm(a) => a.remote.then_some("trace"),
            TraceCmd::Kinds(a) => a.remote.then_some("trace"),
            TraceCmd::Add(_) | TraceCmd::Status => None,
        }
    }
}

#[derive(clap::Args, Clone, Default)]
pub struct AddArgs {
    /// 读文件（JSON 或 JSONL）；不给则从 stdin 读
    #[arg(long)]
    pub file: Option<String>,
    /// 把输入当成**事件流**（每行一个 `{type,name,ms,status,in,out}`）包成一条轨迹
    #[arg(long)]
    pub events: bool,
    /// 轨迹种类：hur-run（HUR 执行）| agent（Agent 会话）
    #[arg(long, default_value = "agent", value_parser = ["hur-run", "agent"])]
    pub kind: String,
    /// 这条轨迹是关于谁的：制品引用 @命名空间/slug（评测按它分版本对比）
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// 制品版本（与 --ref 一起，用作评测分组键）
    #[arg(long)]
    pub version: Option<String>,
    /// 跑在哪个 Agent 形态里（如 harness-use / cursor）
    #[arg(long)]
    pub agent: Option<String>,
    /// 哪个节点（默认：本机主机名）
    #[arg(long)]
    pub node: Option<String>,
    /// 归属用户（默认：当前登录邮箱）
    #[arg(long)]
    pub user: Option<String>,
    /// 结论：ok | error | cancelled（默认 ok）
    #[arg(long)]
    pub status: Option<String>,
    /// 耗时（毫秒）
    #[arg(long)]
    pub duration_ms: Option<i64>,
    /// 模型 provider/name（如 openai/gpt-4o-mini）
    #[arg(long)]
    pub model: Option<String>,
    /// token 数 in:out（如 120:45）
    #[arg(long)]
    pub tokens: Option<String>,
    /// 花费（美元；内部换成微美元整数，避免浮点毁掉跨语言摘要）
    #[arg(long)]
    pub cost_usd: Option<f64>,
    /// 采集时的标签，可重复：--label task=book-hotel --label grade=pass
    #[arg(long = "label", value_name = "k=v")]
    pub labels: Vec<String>,
    /// 标签，可重复
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// 内容级别：digest（默认，只有哈希与结构）| preview（截断预览）| full（原文）
    #[arg(long, default_value = "digest", value_parser = ["digest", "preview", "full"])]
    pub payload: String,
    /// 脱敏强度：strict（默认：密钥/邮箱/IP/长随机串）| basic（只密钥）| off
    #[arg(long, default_value = "strict", value_parser = ["strict", "basic", "off"])]
    pub redact: String,
    /// 不算摘要（标签里含非整数浮点等跨语言不确定内容时用；服务端会补上）
    #[arg(long)]
    pub no_digest: bool,
    /// 采集端标识（默认 ncc/<版本>）
    #[arg(long)]
    pub cli: Option<String>,
}

#[derive(clap::Args)]
pub struct LsArgs {
    /// 列服务端的（默认列本地暂存）
    #[arg(long)]
    pub remote: bool,
    /// 只看这个制品引用
    #[arg(long = "ref")]
    pub reference: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub node: Option<String>,
    #[arg(long)]
    pub payload: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    /// 只看打过分的（labeled=1）/ 只看没打分的（labeled=0）
    #[arg(long)]
    pub labeled: Option<u8>,
    /// 关键词
    #[arg(long)]
    pub q: Option<String>,
    /// 起始时间：RFC3339 / 2026-09-26 / unix 秒
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    #[arg(long, default_value_t = 1)]
    pub page: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ShowArgs {
    /// 轨迹 id（本地 traceId，或服务端返回的 TR-… 行 id）
    pub id: String,
    #[arg(long)]
    pub remote: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct PushArgs {
    /// 只推这几条（可重复）；不给则推全部未推过的
    #[arg(long = "id")]
    pub ids: Vec<String>,
    /// 连已经推过的也再推一次（幂等，服务端会算 duplicates）
    #[arg(long)]
    pub recheck: bool,
    /// 推成功后删掉本地副本（默认保留：本地是原始数据，别自己丢）
    #[arg(long)]
    pub prune: bool,
    /// 只显示要推什么，不发请求
    #[arg(long)]
    pub dry_run: bool,
    /// 写到哪个命名空间（默认个人空间）
    #[arg(long = "namespace", value_name = "@slug")]
    pub namespace: Option<String>,
}

#[derive(clap::Args)]
pub struct StatsArgs {
    /// 看服务端的聚合（默认只算本地暂存的）
    #[arg(long)]
    pub remote: bool,
    #[arg(long = "ref")]
    pub reference: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ExportArgs {
    /// 从服务端导（默认从本地暂存导）
    #[arg(long)]
    pub remote: bool,
    /// 输出文件（不给则打到 stdout）
    #[arg(long)]
    pub out: Option<String>,
    #[arg(long = "ref")]
    pub reference: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long)]
    pub grade: Option<String>,
    #[arg(long)]
    pub split: Option<String>,
    #[arg(long)]
    pub labeled: Option<u8>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    /// 最多导多少条（服务端上限 20000）
    #[arg(long, default_value_t = 1000)]
    pub limit: usize,
    /// 打成**数据集快照包**（profile=trace-set）而不是散 JSONL：可签名/可发布/可灌回
    #[arg(long, value_name = "目录")]
    pub as_package: Option<String>,
    #[arg(long, default_value = "internal", value_parser = ["public", "internal", "private"])]
    pub privacy: String,
    #[arg(long, default_value = "")]
    pub license: String,
    /// 这份数据集里**最宽**的载荷级别（digest | preview | full）。
    ///
    /// 为什么默认 `full`：导出的 JSONL 里是暂存区里有的东西，宣称得比实际更严
    /// （说 digest 却在发 payload）是危险的那个方向。要声明 digest，先确认暂存里
    /// 的轨迹确实都是 digest 档。
    #[arg(long, default_value = "full", value_parser = ["digest", "preview", "full"])]
    pub payload: String,
    #[arg(long, default_value = "")]
    pub name: String,
    #[arg(long, default_value = "")]
    pub version: String,
}

#[derive(clap::Args)]
pub struct LabelArgs {
    pub id: String,
    /// 标到服务端（默认只标本地）
    #[arg(long)]
    pub remote: bool,
    /// 结论：pass | fail | partial
    #[arg(long)]
    pub grade: Option<String>,
    /// 奖励（整数，如 1 / 0 / -1）—— 后训练用
    #[arg(long)]
    pub reward: Option<i64>,
    /// 得分（-1..1）—— 评测用
    #[arg(long)]
    pub score: Option<f64>,
    /// 业务任务（如 book-hotel）
    #[arg(long)]
    pub task: Option<String>,
    /// 数据切分：train | eval | holdout
    #[arg(long)]
    pub split: Option<String>,
    /// 失败归类（如 tool_timeout / wrong_answer）
    #[arg(long)]
    pub failure: Option<String>,
    /// 一句说明
    #[arg(long)]
    pub note: Option<String>,
    /// 标注人（默认：当前登录身份）
    #[arg(long)]
    pub by: Option<String>,
}

#[derive(clap::Args)]
pub struct KindsArgs {
    /// 取服务端的词表（默认打印本机 CLI 内置的）
    #[arg(long)]
    pub remote: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct RmArgs {
    pub id: String,
    #[arg(long)]
    pub remote: bool,
    /// 服务端删除要确认（本地删除不用）
    #[arg(long)]
    pub yes: bool,
}

/* ============================ 摘要（与 Go 逐字节一致） ============================ */

/// 摘要的规范文本：`<段名>=<字节长度>:<内容>\n` 逐段拼接。
///
/// **与 Go 侧 `model.TraceDigestCore` 必须逐字节相同**（测试 `digest_matches_go_vector`
/// 钉住）。不用"序列化整个 JSON 再哈希"，是因为浮点/转义/键序在不同语言里不保证一致：
/// Go 会把 `<` 转义成 `\u003c`、Rust 不会；`1.0` 与 `1` 也不同。长度前缀拼接没有这些坑。
pub fn trace_digest_core(d: &Value) -> String {
    let mut b = String::new();
    // 取字符串/整数的两个小工具：缺字段/类型不对一律当空/0（与 Go 的零值一致）。
    let s = |path: &str| -> String {
        d.pointer(path).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let i = |path: &str| -> i64 {
        match d.pointer(path) {
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f as i64))
                .unwrap_or(0),
            _ => 0,
        }
    };
    let seg = |b: &mut String, k: &str, v: &str| {
        b.push_str(k);
        b.push('=');
        b.push_str(&v.len().to_string());
        b.push(':');
        b.push_str(v);
        b.push('\n');
    };
    let segi = |b: &mut String, k: &str, v: i64| seg(b, k, &v.to_string());

    seg(&mut b, "spec", &s("/spec"));
    seg(&mut b, "id", &s("/id"));
    seg(&mut b, "kind", &s("/kind"));
    seg(&mut b, "at", &s("/at"));
    segi(&mut b, "durationMs", i("/durationMs"));
    seg(&mut b, "status", &s("/status"));
    for k in ["node", "host", "cli", "agent", "user", "region"] {
        seg(&mut b, &format!("source.{k}"), &s(&format!("/source/{k}")));
    }
    for k in ["ref", "kind", "version", "digest", "engine", "policy"] {
        seg(&mut b, &format!("subject.{k}"), &s(&format!("/subject/{k}")));
    }
    seg(&mut b, "model.provider", &s("/model/provider"));
    seg(&mut b, "model.name", &s("/model/name"));
    segi(&mut b, "model.calls", i("/model/calls"));
    segi(&mut b, "usage.inputTokens", i("/usage/inputTokens"));
    segi(&mut b, "usage.outputTokens", i("/usage/outputTokens"));
    segi(&mut b, "usage.costUsdMicros", i("/usage/costUsdMicros"));
    seg(&mut b, "payload", &s("/payload"));
    seg(&mut b, "redaction.level", &s("/redaction/level"));

    let steps = d.get("steps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    segi(&mut b, "steps", steps.len() as i64);
    for st in &steps {
        let ss = |k: &str| st.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let si = |k: &str| {
            st.get(k)
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .unwrap_or(0)
        };
        segi(&mut b, "step.i", si("i"));
        seg(&mut b, "step.type", &ss("type"));
        seg(&mut b, "step.name", &ss("name"));
        segi(&mut b, "step.ms", si("ms"));
        seg(&mut b, "step.status", &ss("status"));
        seg(&mut b, "step.inDigest", &ss("inDigest"));
        seg(&mut b, "step.outDigest", &ss("outDigest"));
    }

    // 标签：键**排序**后逐个进摘要（map 迭代顺序随机，不排序摘要就不稳定）。
    let labels = d.get("labels").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    segi(&mut b, "labels", labels.len() as i64);
    let mut keys: Vec<&String> = labels.keys().collect();
    keys.sort();
    for k in keys {
        seg(&mut b, "label.k", k);
        seg(&mut b, "label.v", &label_value(&labels[k]));
    }
    let tags = d
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    segi(&mut b, "tags", tags.len() as i64);
    for t in tags {
        seg(&mut b, "tag", t);
    }
    b
}

/// 标签值的规范文本。
///
/// 整数**一定**写成整数（`1`，不是 `1.0`）——否则 Go 与 Rust 会给出不同摘要。
/// ⚠️ 非整数浮点（如 `0.85`）在不同语言的"最短表示"上不保证一致：轨迹里要存
/// 非整数就用**整数口径**（得分用千分位、金额用微美元），别把浮点塞进标签。
fn label_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return i.to_string();
            }
            match n.as_f64() {
                Some(f) if f.fract() == 0.0 => (f as i64).to_string(),
                Some(f) => format!("{f}"),
                None => n.to_string(),
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `sha256:<hex>`。覆盖身份 + 结构 + 标签，**不覆盖原文**（见文件头注释）。
pub fn trace_digest(d: &Value) -> String {
    let sum = Sha256::digest(trace_digest_core(d).as_bytes());
    format!("sha256:{}", hex(&sum))
}

/// 由内容生成的稳定 id（与服务端 `model.TraceID` 一致）。
pub fn trace_id(d: &Value) -> String {
    let sum = Sha256::digest(trace_digest_core(d).as_bytes());
    format!("TRC-{}", &hex(&sum)[..20])
}

fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 0xf) as usize] as char);
    }
    s
}

/* ============================ 脱敏 ============================ */

/// 一条脱敏规则：命中即替换（手写扫描，不引 regex 依赖）。
struct Rule {
    name: &'static str,
    hit: fn(&str) -> Option<(usize, usize)>,
}

const RULES: &[Rule] = &[
    Rule { name: "api_key", hit: hit_api_key },
    Rule { name: "bearer", hit: hit_bearer },
    Rule { name: "email", hit: hit_email },
    Rule { name: "ipv4", hit: hit_ipv4 },
    Rule { name: "long_secret", hit: hit_long_secret },
];

/// `strict` = 全部规则；`basic` = 只密钥类。`off` 不调这里。
fn rules_for(level: &str) -> Vec<&'static Rule> {
    RULES
        .iter()
        .filter(|r| match level {
            "basic" => matches!(r.name, "api_key" | "bearer" | "long_secret"),
            _ => true,
        })
        .collect()
}

/// 对一段文本脱敏，返回（脱敏后的文本，命中的规则名）。
fn redact_text(s: &str, level: &str) -> (String, Vec<String>) {
    let mut out = s.to_string();
    let mut used: Vec<String> = Vec::new();
    for r in rules_for(level) {
        let mut hits = 0;
        // 反复扫描：一条文本里可能有多个命中（每次替换后长度会变，重新扫）。
        loop {
            match (r.hit)(&out) {
                Some((a, b)) if b > a && b <= out.len() => {
                    let mark = format!("<{}>", r.name);
                    out.replace_range(a..b, &mark);
                    hits += 1;
                    if hits > 64 {
                        break; // 防御：异常输入别把 CPU 吃光
                    }
                }
                _ => break,
            }
        }
        if hits > 0 {
            used.push(r.name.to_string());
        }
    }
    (out, used)
}

/// 密钥前缀：`sk-…` / `AK-…` / `ghp_…` / `xoxb-…`，取到非标识符字符为止。
fn hit_api_key(s: &str) -> Option<(usize, usize)> {
    for pre in ["sk-", "AK-", "ghp_", "xoxb-", "nvapi-"] {
        if let Some(i) = s.find(pre) {
            let start = i;
            let mut end = i + pre.len();
            for (off, c) in s[i + pre.len()..].char_indices() {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    end = i + pre.len() + off + c.len_utf8();
                } else {
                    break;
                }
            }
            // 太短的（如 `sk-x`）不算密钥，避免把普通词误伤。
            if end - start >= pre.len() + 8 {
                return Some((start, end));
            }
        }
    }
    None
}

/// `Bearer <token>`：连词一起换掉（保留可读性，比只换 token 更不容易漏）。
fn hit_bearer(s: &str) -> Option<(usize, usize)> {
    let needle = ["Bearer ", "bearer "];
    for n in needle {
        if let Some(i) = s.find(n) {
            let mut end = i + n.len();
            for (off, c) in s[i + n.len()..].char_indices() {
                if !c.is_whitespace() && c != '"' && c != '\'' && c != ',' && c != '}' {
                    end = i + n.len() + off + c.len_utf8();
                } else {
                    break;
                }
            }
            if end > i + n.len() {
                return Some((i, end));
            }
        }
    }
    None
}

/// 邮箱：`本地部分@域名`，按 ASCII 允许的字符粗略扫。
fn hit_email(s: &str) -> Option<(usize, usize)> {
    let at = s.find('@')?;
    let bytes = s.as_bytes();
    let mut start = at;
    while start > 0 {
        let c = bytes[start - 1] as char;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-') {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = at + 1;
    while end < bytes.len() {
        let c = bytes[end] as char;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
            end += 1;
        } else {
            break;
        }
    }
    if start < at && end > at + 1 && s[start..end].contains('.') {
        Some((start, end))
    } else {
        None
    }
}

/// IPv4：四段点分十进制。
fn hit_ipv4(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut end = i;
            let mut dots = 0;
            while end < bytes.len()
                && (bytes[end].is_ascii_digit() || (bytes[end] == b'.' && dots < 3))
            {
                if bytes[end] == b'.' {
                    dots += 1;
                }
                end += 1;
            }
            if dots == 3 && end > start {
                let cand = &s[start..end];
                if cand.split('.').all(|p| !p.is_empty() && p.len() <= 3) {
                    return Some((start, end));
                }
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// 长随机串：长度 ≥ 32 的连续十六进制或 base64 字符（密钥、签名、摘要都长这样）。
fn hit_long_secret(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_') {
            let start = i;
            let mut end = i;
            while end < bytes.len() {
                let c = bytes[end] as char;
                if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_') {
                    end += 1;
                } else {
                    break;
                }
            }
            // 跳过 `sha256:…` 这类**本来就该是摘要**的写法：它们不是秘密。
            let pre = &s[..start];
            let is_digest = pre.ends_with("sha256:") || pre.ends_with("sha512:");
            if end - start >= 32 && !is_digest {
                return Some((start, end));
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/* ============================ 本地暂存 ============================ */

fn traces_dir() -> PathBuf {
    config::ncc_dir().join("traces")
}

fn spool_dir() -> PathBuf {
    traces_dir().join("spool")
}

fn pushed_path() -> PathBuf {
    traces_dir().join("pushed.json")
}

fn labels_path() -> PathBuf {
    traces_dir().join("labels.json")
}

fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(spool_dir()).with_context(|| "建不了本地暂存目录")?;
    Ok(())
}

/// 写文件并尽力收紧权限（轨迹可能含提示词原文，不该是 0644）。
fn write_private(path: &std::path::Path, body: &str) -> Result<()> {
    std::fs::write(path, body).with_context(|| format!("写不了 {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn spool_path(id: &str) -> PathBuf {
    // id 只保留 `[A-Za-z0-9_-]`：任何别的字符（`/`、`.`、`..`、空白…）一律换成 `_`。
    // 轨迹 id 是我们自己生成的（`TRC-<hex>`），不需要别的字符；这样 `--id ../../etc/passwd`
    // 也会变成一个无害的文件名，而不是一次目录穿越。
    let safe: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    spool_dir().join(format!("{safe}.json"))
}

fn load_json_map(path: &std::path::Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_json(path: &std::path::Path, v: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_private(path, &serde_json::to_string_pretty(v)?)
}

fn pushed_map() -> Map<String, Value> {
    load_json_map(&pushed_path())
}

fn labels_map() -> Map<String, Value> {
    load_json_map(&labels_path())
}

/// 本地暂存的全部轨迹（按 at 倒序）。
fn local_traces() -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(spool_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    for e in rd.flatten() {
        if e.path().extension().map(|x| x == "json").unwrap_or(false) {
            if let Ok(s) = std::fs::read_to_string(e.path()) {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    out.push(v);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        let k = |v: &Value| v.get("at").and_then(|x| x.as_str()).unwrap_or("").to_string();
        k(b).cmp(&k(a))
    });
    out
}

fn find_local(id: &str) -> Option<Value> {
    if let Some(v) = std::fs::read_to_string(spool_path(id))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        return Some(v);
    }
    local_traces().into_iter().find(|t| {
        t.get("id").and_then(|x| x.as_str()) == Some(id)
            || t.get("traceId").and_then(|x| x.as_str()) == Some(id)
    })
}

/* ============================ 采集：三种输入 ============================ */

/// 判断一行 JSON 是什么（原生文档 / HUR 执行留痕 / 事件）。
fn classify(v: &Value) -> &'static str {
    match v.get("spec").and_then(|x| x.as_str()) {
        Some(s) if s == TRACE_SPEC => "trace",
        Some(s) if s == RUN_TRACE_SPEC => "run-trace",
        _ if v.get("type").and_then(|x| x.as_str()).is_some() => "event",
        _ => "unknown",
    }
}

/// 把 `ncc hur run` 的执行留痕收敛成一条 trace。
///
/// 留痕里没有提示词与输出（它记的是"按什么限额跑的、结论是什么"），所以转换出来的
/// 轨迹天然是 `payload=digest` —— 这不是保守，是如实的：留痕本来就不含原文。
pub fn from_run_trace(rt: &Value, a: &AddArgs) -> Value {
    let at_unix = rt.get("at_unix").and_then(|x| x.as_i64()).unwrap_or(0);
    let outcome = rt.get("outcome").cloned().unwrap_or(Value::Null);
    let status = match outcome.get("ok").and_then(|x| x.as_bool()) {
        Some(true) => "ok",
        Some(false) => "error",
        None => "ok",
    };
    // 耗时/用量各引擎字段不同，这里只认"常见名字"，认不出就留 0（不编数）。
    let dur = outcome
        .get("ms")
        .or_else(|| outcome.get("wallMs"))
        .or_else(|| outcome.get("durationMs"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let mut doc = json!({
        "spec": TRACE_SPEC,
        "kind": "hur-run",
        "at": crate::gateway::iso_from_secs(at_unix.max(0)),
        "durationMs": dur,
        "status": status,
        "source": { "cli": a.cli.clone().unwrap_or_else(default_cli), "agent": a.agent },
        "subject": {
            "ref": rt.get("package").and_then(|x| x.as_str()).unwrap_or(""),
            "kind": "hur",
            "version": rt.get("version").and_then(|x| x.as_str()).unwrap_or(""),
            "engine": rt.get("engine").and_then(|x| x.as_str()).unwrap_or(""),
            "policy": rt.get("policy").and_then(|x| x.as_str()).unwrap_or(""),
        },
        "payload": "digest",
        "labels": {},
        "tags": [],
    });
    if let Some(o) = doc.as_object_mut() {
        o.insert("limits".into(), rt.get("limits").cloned().unwrap_or(Value::Null));
        o.insert("outcome".into(), outcome);
    }
    doc
}

/// 按内容级别与脱敏强度处理 in/out：digest 级别一律丢掉原文。
pub fn from_events(events: &[Value], a: &AddArgs) -> Value {
    let mut steps: Vec<Value> = Vec::new();
    let mut total_ms: i64 = 0;
    let mut in_tokens: i64 = 0;
    let mut out_tokens: i64 = 0;
    let mut model_provider = String::new();
    let mut model_name = String::new();
    let mut calls: i64 = 0;
    let mut all_rules: Vec<String> = Vec::new();
    for (i, e) in events.iter().enumerate().take(MAX_STEPS) {
        let ty = e.get("type").and_then(|x| x.as_str()).unwrap_or("note");
        let ty = if STEP_TYPES.contains(&ty) { ty } else { "note" };
        let ms = e.get("ms").and_then(|x| x.as_i64()).unwrap_or(0);
        total_ms += ms;
        let inp = e.get("in").map(value_text).unwrap_or_default();
        let outp = e.get("out").map(value_text).unwrap_or_default();
        if ty == "llm" {
            calls += 1;
            if let Some(u) = e.get("usage") {
                in_tokens += u.get("inputTokens").and_then(|x| x.as_i64()).unwrap_or(0);
                out_tokens += u.get("outputTokens").and_then(|x| x.as_i64()).unwrap_or(0);
            }
            if model_name.is_empty() {
                model_name = e.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string();
                model_provider =
                    e.get("provider").and_then(|x| x.as_str()).unwrap_or("").to_string();
            }
        }
        // 摘要**总在**（哪怕级别是 full）：只有摘要时也能核对“这一步跑过什么”。
        let mut st = json!({
            "i": i as i64,
            "type": ty,
            "ms": ms,
            "status": e.get("status").and_then(|x| x.as_str()).unwrap_or("ok"),
            "inDigest": digest_of_text(&inp),
            "outDigest": digest_of_text(&outp),
        });
        if let Some(n) = e.get("name").and_then(|x| x.as_str()) {
            st["name"] = Value::String(n.to_string());
        }
        let (inp2, outp2, rules) = prepare_payload(&inp, &outp, &a.payload, &a.redact);
        all_rules.extend(rules);
        if !inp2.is_empty() {
            st["in"] = Value::String(inp2);
        }
        if !outp2.is_empty() {
            st["out"] = Value::String(outp2);
        }
        if let Some(meta) = e.get("meta") {
            st["meta"] = meta.clone();
        }
        steps.push(st);
    }
    all_rules.sort();
    all_rules.dedup();
    let mut labels = Map::new();
    apply_label_args(&mut labels, a);
    json!({
        "spec": TRACE_SPEC,
        "kind": a.kind,
        "at": now_iso(),
        "durationMs": a.duration_ms.unwrap_or(total_ms),
        "status": a.status.clone().unwrap_or_else(|| "ok".into()),
        "source": {
            "cli": a.cli.clone().unwrap_or_else(default_cli),
            "agent": a.agent,
            "node": a.node.clone().unwrap_or_else(default_node),
            "user": a.user,
        },
        "subject": { "ref": a.reference, "kind": "", "version": a.version },
        "model": { "provider": model_provider, "name": model_name, "calls": calls },
        "usage": { "inputTokens": in_tokens, "outputTokens": out_tokens },
        "steps": steps,
        "labels": Value::Object(labels),
        "tags": a.tags,
        "payload": a.payload,
        // 如实记录被处理成什么样：拿到数据集的人不用猜。
        "redaction": {
            "applied": !all_rules.is_empty(),
            "level": if a.payload == "digest" { "" } else { &a.redact },
            "rules": all_rules,
        },
    })
}

/// 文本摘要（步骤级的 in/out 指纹）：内容是原文时，摘要证明"这一步跑过什么"。
fn digest_of_text(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let sum = Sha256::digest(s.as_bytes());
    format!("sha256:{}", hex(&sum))
}

/// 把任意 JSON 值变成"能进 in/out 的文本"。
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 按内容级别与脱敏强度处理 in/out：digest 级别一律丢掉原文。
fn prepare_payload(inp: &str, outp: &str, payload: &str, redact: &str) -> (String, String, Vec<String>) {
    if payload == "digest" {
        // 只有摘要，没有原文 —— 最保守的默认。
        return (String::new(), String::new(), Vec::new());
    }
    let mut rules: Vec<String> = Vec::new();
    let mut fin = inp.to_string();
    let mut fout = outp.to_string();
    if redact != "off" {
        let (a, r1) = redact_text(&fin, redact);
        let (b, r2) = redact_text(&fout, redact);
        fin = a;
        fout = b;
        rules.extend(r1);
        rules.extend(r2);
    }
    if payload == "preview" {
        let (a, cut1) = truncate_utf8(&fin, PREVIEW_MAX);
        let (b, cut2) = truncate_utf8(&fout, PREVIEW_MAX);
        fin = a;
        fout = b;
        if cut1 || cut2 {
            rules.push("truncate".into());
        }
    }
    rules.sort();
    rules.dedup();
    (fin, fout, rules)
}

/// 按**字符边界**截断（不能把多字节字符劈开）。
fn truncate_utf8(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

fn apply_label_args(labels: &mut Map<String, Value>, a: &AddArgs) {
    for kv in &a.labels {
        if let Some((k, v)) = kv.split_once('=') {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            // 数字就存数字（整数口径）；其它存字符串。
            let val = if let Ok(i) = v.trim().parse::<i64>() {
                Value::from(i)
            } else {
                Value::String(v.trim().to_string())
            };
            labels.insert(k.to_string(), val);
        }
    }
    if let Some(m) = &a.model {
        labels.insert("model".into(), Value::String(m.clone()));
    }
}

fn default_cli() -> String {
    format!("ncc/{}", env!("CARGO_PKG_VERSION"))
}

fn default_node() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

fn now_iso() -> String {
    crate::gateway::now_iso()
}

/// 补齐最小必需字段（id / payload / digest），与服务端 `NormalizeTrace` 同规矩。
pub fn normalize_doc(d: &mut Value, with_digest: bool) {
    if !d.is_object() {
        return;
    }
    if d.pointer("/spec").and_then(|x| x.as_str()).unwrap_or("").is_empty() {
        d["spec"] = Value::String(TRACE_SPEC.into());
    }
    if d.pointer("/payload").and_then(|x| x.as_str()).unwrap_or("").is_empty() {
        // 缺省是最保守的级别：只有摘要与结构，没有原文。
        d["payload"] = Value::String("digest".into());
    }
    let has_id = d
        .get("id")
        .and_then(|x| x.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !has_id {
        let id = trace_id(d);
        d["id"] = Value::String(id);
    }
    if with_digest {
        let dg = trace_digest(d);
        d["digest"] = Value::String(dg);
    }
}

/// 本地上报前的自检（与服务端同一套规矩里"能在本地判"的那些）。
pub fn validate_doc(d: &Value) -> Vec<String> {
    let mut errs: Vec<String> = Vec::new();
    let s = |p: &str| d.pointer(p).and_then(|x| x.as_str()).unwrap_or("").to_string();
    if s("/spec") != TRACE_SPEC {
        errs.push(format!("spec 必须是 {TRACE_SPEC}，当前是「{}」", s("/spec")));
    }
    if s("/id").is_empty() {
        errs.push("缺 id".into());
    }
    if !KINDS.contains(&s("/kind").as_str()) {
        errs.push(format!("kind 必须是 {}，当前是「{}」", KINDS.join("|"), s("/kind")));
    }
    if !STATUSES.contains(&s("/status").as_str()) {
        errs.push(format!("status 必须是 {}，当前是「{}」", STATUSES.join("|"), s("/status")));
    }
    if !PAYLOADS.contains(&s("/payload").as_str()) {
        errs.push(format!("payload 必须是 {}，当前是「{}」", PAYLOADS.join("|"), s("/payload")));
    }
    if s("/at").is_empty() {
        errs.push("缺 at（RFC3339）".into());
    }
    if let Some(steps) = d.get("steps").and_then(|x| x.as_array()) {
        if steps.len() > MAX_STEPS {
            errs.push(format!("步骤太多：{}（上限 {MAX_STEPS}）", steps.len()));
        }
        for (i, st) in steps.iter().enumerate() {
            let ty = st.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if !STEP_TYPES.contains(&ty) {
                errs.push(format!("步骤 {i} 的类型「{ty}」不合法"));
            }
            if s("/payload") == "digest"
                && (st.get("in").is_some() || st.get("out").is_some())
            {
                errs.push(format!("payload=digest 却带步骤 {i} 的原文"));
                break;
            }
        }
    }
    // 摘要自洽（算了就核对）。
    if let Some(dg) = d.get("digest").and_then(|x| x.as_str()) {
        if !dg.is_empty() && dg != trace_digest(d) {
            errs.push(format!("digest 与内容不符（算出来是 {}）", trace_digest(d)));
        }
    }
    errs
}

/* ============================ 命令实现 ============================ */

pub fn run(cfg: &CliConfig, cmd: &TraceCmd) -> Result<()> {
    match cmd {
        TraceCmd::Add(a) => add(cfg, a),
        TraceCmd::Ls(a) => ls(cfg, a),
        TraceCmd::Show(a) => show(cfg, a),
        TraceCmd::Push(a) => push(cfg, a),
        TraceCmd::Stats(a) => stats(cfg, a),
        TraceCmd::Export(a) => export(cfg, a),
        TraceCmd::Label(a) => label(cfg, a),
        TraceCmd::Kinds(a) => kinds(cfg, a),
        TraceCmd::Rm(a) => rm(cfg, a),
        TraceCmd::Status => status(),
    }
}

/// 读输入文本（文件或 stdin）。
fn read_input(file: &Option<String>) -> Result<String> {
    match file {
        Some(p) if p != "-" => std::fs::read_to_string(p).with_context(|| format!("读不了 {p}")),
        _ => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).context("读 stdin 失败")?;
            Ok(s)
        }
    }
}

fn add(cfg: &CliConfig, a: &AddArgs) -> Result<()> {
    ensure_dirs()?;
    // 归属用户：没显式给就用当前登录身份（服务端仍以令牌身份为准，这里只是标注）。
    let mut owned = a.clone();
    if owned.user.as_deref().unwrap_or("").is_empty() {
        if let Some(email) = cfg.target().email.clone() {
            if !email.is_empty() {
                owned.user = Some(email);
            }
        }
    }
    let a = &owned;
    let text = read_input(&a.file)?;
    if text.trim().is_empty() {
        bail!("没有输入（用 --file 或从 stdin 管道进来）");
    }

    // 解析成一个 JSON 值：整个文件是一个 JSON，或是一串 JSONL。
    let parsed: Result<Value> = serde_json::from_str(&text).context("不是合法 JSON");
    let mut docs: Vec<Value> = Vec::new();

    match parsed {
        Ok(v) if v.is_array() => {
            let arr = v.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                bail!("输入是空数组");
            }
            // 数组里是「多份轨迹文档」还是「一串事件」？看第一项。
            if !a.events && classify(&arr[0]) == "trace" {
                docs = arr;
            } else if !a.events && classify(&arr[0]) == "run-trace" {
                docs = arr.iter().map(|rt| from_run_trace(rt, a)).collect();
            } else {
                docs.push(from_events(&arr, a));
            }
        }
        Ok(v) => match classify(&v) {
            "trace" => docs.push(v),
            "run-trace" => docs.push(from_run_trace(&v, a)),
            "event" if !a.events => docs.push(from_events(&[v], a)),
            _ => {
                // 单份对象、但不是我们认识的形状：只有显式 --events 时按"事件"收。
                if a.events {
                    docs.push(from_events(&[v], a));
                } else {
                    bail!(
                        "认不出这份输入：要么是 {TRACE_SPEC} 轨迹文档，要么是 {RUN_TRACE_SPEC} 执行留痕，\
                         要么用 --events 声明它是一串事件（每项 {{type,name,ms,in,out}}）"
                    );
                }
            }
        },
        Err(_) => {
            // JSONL：逐行解析。
            let mut events: Vec<Value> = Vec::new();
            let mut lines = 0;
            for (n, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                lines += 1;
                let v: Value = serde_json::from_str(line)
                    .with_context(|| format!("第 {} 行不是合法 JSON", n + 1))?;
                match classify(&v) {
                    "trace" => docs.push(v),
                    "run-trace" => docs.push(from_run_trace(&v, a)),
                    _ => events.push(v),
                }
            }
            if !events.is_empty() {
                if !a.events && docs.is_empty() {
                    // 全是"不像轨迹"的行：按事件流处理更实用（Agent 直接吐日志）。
                    docs.push(from_events(&events, a));
                } else if !events.is_empty() {
                    bail!(
                        "第 1 行像轨迹、后面 {} 行像事件 —— 两种混在一份输入里。请分开采集。",
                        events.len()
                    );
                }
            }
            if lines == 0 {
                bail!("输入里没有非空行");
            }
        }
    }

    let mut written: Vec<(String, String)> = Vec::new();
    for mut d in docs {
        normalize_doc(&mut d, !a.no_digest);
        let errs = validate_doc(&d);
        if !errs.is_empty() {
            bail!("轨迹不合规，没有落盘：\n  - {}", errs.join("\n  - "));
        }
        let id = d.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let dg = d.get("digest").and_then(|x| x.as_str()).unwrap_or("").to_string();
        write_private(&spool_path(&id), &serde_json::to_string_pretty(&d)?)?;
        written.push((id, dg));
    }

    if written.len() == 1 {
        let (id, dg) = &written[0];
        println!("✅ 已采集 1 条轨迹 → {id}");
        if !dg.is_empty() {
            println!("   摘要 {dg}");
        }
        println!("   本地暂存 {}", spool_dir().display());
        println!("   上传：ncc trace push --id {id}（或 ncc trace push 推全部未上传的）");
    } else {
        println!("✅ 已采集 {} 条轨迹到本地暂存", written.len());
        for (id, _) in &written {
            println!("   {id}");
        }
    }
    Ok(())
}

/// 检索串（本地与服务端同一套参数名）。
fn query_string(pairs: &[(&str, Option<String>)]) -> String {
    let mut qs: Vec<String> = Vec::new();
    for (k, v) in pairs {
        if let Some(x) = v {
            if !x.is_empty() {
                qs.push(format!("{}={}", k, api::urlenc(x)));
            }
        }
    }
    if qs.is_empty() {
        String::new()
    } else {
        format!("?{}", qs.join("&"))
    }
}

fn ls(cfg: &CliConfig, a: &LsArgs) -> Result<()> {
    if a.remote {
        let t = config::require_token(cfg)?;
        let q = query_string(&[
            ("ref", a.reference.clone()),
            ("kind", a.kind.clone()),
            ("status", a.status.clone()),
            ("agent", a.agent.clone()),
            ("node", a.node.clone()),
            ("payload", a.payload.clone()),
            ("tag", a.tag.clone()),
            ("q", a.q.clone()),
            ("since", a.since.clone()),
            ("until", a.until.clone()),
            ("labeled", a.labeled.map(|x| x.to_string())),
            ("page", Some(a.page.to_string())),
            ("size", Some(a.limit.to_string())),
        ]);
        let d = api::get(cfg, &format!("/api/traces{q}"), Some(&t))?;
        if a.json {
            println!("{}", serde_json::to_string_pretty(&d)?);
            return Ok(());
        }
        let rows = d["traces"].as_array().cloned().unwrap_or_default();
        println!(
            "轨迹 {} 条（范围 {}）",
            d["total"].as_i64().unwrap_or(0),
            d["scope"].as_str().unwrap_or("-")
        );
        if rows.is_empty() {
            println!("  没有匹配的轨迹。");
            return Ok(());
        }
        for r in rows {
            print_trace_line(&r);
        }
        return Ok(());
    }
    let local = local_traces();
    let pushed = pushed_map();
    let filtered: Vec<Value> = local
        .into_iter()
        .filter(|t| {
            a.reference.as_deref().map(|r| t.pointer("/subject/ref").and_then(|x| x.as_str()) == Some(r)).unwrap_or(true)
                && a.kind.as_deref().map(|k| t.get("kind").and_then(|x| x.as_str()) == Some(k)).unwrap_or(true)
                && a.status.as_deref().map(|s| t.get("status").and_then(|x| x.as_str()) == Some(s)).unwrap_or(true)
                && a.tag.as_deref().map(|g| {
                    t.get("tags").and_then(|x| x.as_array()).map(|arr| arr.iter().any(|v| v.as_str() == Some(g))).unwrap_or(false)
                }).unwrap_or(true)
        })
        .collect();
    if a.json {
        println!("{}", serde_json::to_string_pretty(&Value::Array(filtered))?);
        return Ok(());
    }
    println!("本地暂存 {} 条（目录 {}）", filtered.len(), spool_dir().display());
    for t in filtered.iter().take(a.limit) {
        let id = t.get("id").and_then(|x| x.as_str()).unwrap_or("-");
        let is_pushed = pushed.contains_key(id);
        println!(
            "  {} {:<24} {:<9} {:<6} {} ms  {} 步  {}",
            if is_pushed { "↑" } else { "•" },
            id,
            t.get("kind").and_then(|x| x.as_str()).unwrap_or("-"),
            t.get("status").and_then(|x| x.as_str()).unwrap_or("-"),
            t.get("durationMs").and_then(|x| x.as_i64()).unwrap_or(0),
            t.get("steps").and_then(|x| x.as_array()).map(|x| x.len()).unwrap_or(0),
            if is_pushed { "已上传" } else { "未上传" }
        );
    }
    Ok(())
}

fn print_trace_line(r: &Value) {
    let ev = r.get("evaluation").cloned().unwrap_or(json!({}));
    let mut marks: Vec<String> = Vec::new();
    if let Some(g) = ev.get("grade").and_then(|x| x.as_str()) {
        marks.push(format!("结论 {g}"));
    }
    if let Some(s) = ev.get("scoreMilli").and_then(|x| x.as_i64()) {
        marks.push(format!("得分 {:.3}", s as f64 / 1000.0));
    }
    if let Some(w) = ev.get("rewardMilli").and_then(|x| x.as_i64()) {
        marks.push(format!("奖励 {}", w / 1000));
    }
    if let Some(sp) = ev.get("split").and_then(|x| x.as_str()) {
        marks.push(format!("切分 {sp}"));
    }
    println!(
        "  {:<24} {:<9} {:<6} {:<28} {} ms  {} 步  {}  {}{}",
        r["id"].as_str().unwrap_or("-"),
        r["kind"].as_str().unwrap_or("-"),
        r["status"].as_str().unwrap_or("-"),
        r.pointer("/subject/ref").and_then(|x| x.as_str()).unwrap_or("-"),
        r["durationMs"].as_i64().unwrap_or(0),
        r["steps"].as_i64().unwrap_or(0),
        r["payload"].as_str().unwrap_or("-"),
        r["at"].as_str().unwrap_or(""),
        if marks.is_empty() { String::new() } else { format!("  [{}]", marks.join(" · ")) },
    );
}

fn show(cfg: &CliConfig, a: &ShowArgs) -> Result<()> {
    if a.remote {
        let t = config::require_token(cfg)?;
        let d = api::get(cfg, &format!("/api/traces/{}", api::urlenc(&a.id)), Some(&t))?;
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let d = find_local(&a.id).with_context(|| {
        format!(
            "本地暂存里没有 {}（目录 {}）；查服务端的要加 --remote",
            a.id,
            spool_dir().display()
        )
    })?;
    println!("{}", serde_json::to_string_pretty(&d)?);
    Ok(())
}

fn push(cfg: &CliConfig, a: &PushArgs) -> Result<()> {
    let t = config::require_token(cfg)?;
    ensure_dirs()?;
    let local = local_traces();
    let mut pushed = pushed_map();
    let picks: Vec<Value> = local
        .into_iter()
        .filter(|d| {
            let id = d.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if !a.ids.is_empty() {
                return a.ids.iter().any(|x| x == id);
            }
            a.recheck || !pushed.contains_key(id)
        })
        .collect();
    if picks.is_empty() {
        println!("没有要上传的轨迹（都已经传过了；要重传加 --recheck）");
        return Ok(());
    }
    if a.dry_run {
        println!("--dry-run：会推 {} 条", picks.len());
        for d in &picks {
            println!(
                "  {}  payload={}  {} 步",
                d.get("id").and_then(|x| x.as_str()).unwrap_or("-"),
                d.get("payload").and_then(|x| x.as_str()).unwrap_or("-"),
                d.get("steps").and_then(|x| x.as_array()).map(|x| x.len()).unwrap_or(0)
            );
        }
        return Ok(());
    }

    let mut acc = 0usize;
    let mut dup = 0usize;
    let mut rej: Vec<String> = Vec::new();
    let mut digest_only = 0usize;
    for chunk in picks.chunks(50) {
        let mut traces: Vec<Value> = Vec::new();
        for d in chunk {
            if d.get("payload").and_then(|x| x.as_str()).unwrap_or("digest") == "digest" {
                digest_only += 1;
            }
            traces.push(d.clone());
        }
        let ns = a.namespace.clone().unwrap_or_default();
        let body = json!({ "traces": traces, "namespace": ns });
        let r = api::post_json(cfg, "/api/traces", Some(&t), &body)?;
        let accepted = r["accepted"].as_i64().unwrap_or(0) as usize;
        let duplicates = r["duplicates"].as_i64().unwrap_or(0) as usize;
        acc += accepted;
        dup += duplicates;
        if let Some(rejects) = r["rejects"].as_array() {
            for x in rejects {
                rej.push(format!(
                    "{}: {} {}",
                    x["id"].as_str().unwrap_or("-"),
                    x["code"].as_str().unwrap_or("rejected"),
                    x["msg"].as_str().unwrap_or("")
                ));
            }
        }
        // 被拒的 id 不标记（下次重推时才不会漏）；收下的与重复的（服务端已有）都记成"已上传"。
        let rejected_ids: Vec<String> = r["rejects"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x["id"].as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let refs = r["refs"].as_array().cloned().unwrap_or_default();
        for d in chunk.iter() {
            let id = d.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if id.is_empty() || rejected_ids.iter().any(|x| x == &id) {
                continue;
            }
            let server_ref = refs
                .iter()
                .find(|x| x["traceId"].as_str() == Some(id.as_str()))
                .and_then(|x| x["id"].as_str())
                .unwrap_or("")
                .to_string();
            pushed.insert(
                id.clone(),
                json!({
                    "at": now_iso(),
                    "server": cfg.base_url(),
                    "ref": server_ref,
                    "digest": d.get("digest").and_then(|x| x.as_str()).unwrap_or(""),
                }),
            );
        }
        save_json(&pushed_path(), &Value::Object(pushed.clone()))?;
        if a.prune {
            for d in chunk {
                let id = d.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if !id.is_empty() {
                    let _ = std::fs::remove_file(spool_path(id));
                }
            }
        }
        // 大批量时给个进度感（一次 50 条）。
        if picks.len() > 50 {
            print!(".");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
    if picks.len() > 50 {
        println!();
    }
    println!(
        "✅ 上传完成：新收 {} 条 · 已存在 {} 条 · 被拒 {} 条 → {}",
        acc,
        dup,
        rej.len(),
        cfg.base_url()
    );
    for m in rej.iter().take(10) {
        println!("   ✗ {m}");
    }
    if digest_only > 0 {
        println!(
            "   ⚠️ 其中 {digest_only} 条只有摘要（payload=digest）：能用于**评测与统计**，\
             但不能直接做后训练语料。要语料请用 `--payload preview|full` 重新采集。"
        );
    }
    if a.prune {
        println!("   （--prune：推成功的本地副本已删除；服务端那份才是权威了）");
    }
    Ok(())
}

fn stats(cfg: &CliConfig, a: &StatsArgs) -> Result<()> {
    if a.remote {
        let t = config::require_token(cfg)?;
        let q = query_string(&[
            ("ref", a.reference.clone()),
            ("kind", a.kind.clone()),
            ("since", a.since.clone()),
        ]);
        let d = api::get(cfg, &format!("/api/traces/stats{q}"), Some(&t))?;
        if a.json {
            println!("{}", serde_json::to_string_pretty(&d)?);
            return Ok(());
        }
        print_stats(&d["stats"]);
        println!(
            "   取样 {} 条{}· 范围 {}",
            d["sampled"].as_i64().unwrap_or(0),
            if d["truncated"].as_bool().unwrap_or(false) { "（被截断）" } else { " " },
            d["scope"].as_str().unwrap_or("-")
        );
        return Ok(());
    }
    // 本地：直接对暂存里的文档算（口径与服务端一致的那几项）。
    let rows: Vec<Value> = local_traces()
        .into_iter()
        .filter(|t| {
            a.reference.as_deref().map(|r| t.pointer("/subject/ref").and_then(|x| x.as_str()) == Some(r)).unwrap_or(true)
                && a.kind.as_deref().map(|k| t.get("kind").and_then(|x| x.as_str()) == Some(k)).unwrap_or(true)
        })
        .collect();
    let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_payload: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_refver: BTreeMap<String, i64> = BTreeMap::new();
    let mut durs: Vec<i64> = Vec::new();
    let mut in_tok = 0i64;
    let mut out_tok = 0i64;
    let mut cost = 0i64;
    let labels = labels_map();
    let mut labeled = 0i64;
    for t in &rows {
        *by_status
            .entry(t.get("status").and_then(|x| x.as_str()).unwrap_or("-").to_string())
            .or_default() += 1;
        *by_kind
            .entry(t.get("kind").and_then(|x| x.as_str()).unwrap_or("-").to_string())
            .or_default() += 1;
        *by_payload
            .entry(t.get("payload").and_then(|x| x.as_str()).unwrap_or("-").to_string())
            .or_default() += 1;
        let refv = format!(
            "{}@{}",
            t.pointer("/subject/ref").and_then(|x| x.as_str()).unwrap_or(""),
            t.pointer("/subject/version").and_then(|x| x.as_str()).unwrap_or("")
        );
        *by_refver.entry(refv).or_default() += 1;
        durs.push(t.get("durationMs").and_then(|x| x.as_i64()).unwrap_or(0));
        in_tok += t.pointer("/usage/inputTokens").and_then(|x| x.as_i64()).unwrap_or(0);
        out_tok += t.pointer("/usage/outputTokens").and_then(|x| x.as_i64()).unwrap_or(0);
        cost += t.pointer("/usage/costUsdMicros").and_then(|x| x.as_i64()).unwrap_or(0);
        let id = t.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if labels.get(id).and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
            labeled += 1;
        }
    }
    durs.sort();
    let st = json!({
        "total": rows.len(),
        "byStatus": by_status,
        "byKind": by_kind,
        "byPayload": by_payload,
        "bySubjectVersion": by_refver,
        "durationMs": {
            "min": durs.first().copied().unwrap_or(0),
            "p50": pct(&durs, 0.50),
            "p90": pct(&durs, 0.90),
            "max": durs.last().copied().unwrap_or(0),
        },
        "inputTokens": in_tok,
        "outputTokens": out_tok,
        "costUsdMicros": cost,
        "labeled": labeled,
        "unlabeled": rows.len() as i64 - labeled,
    });
    if a.json {
        println!("{}", serde_json::to_string_pretty(&st)?);
        return Ok(());
    }
    println!("本地暂存聚合（口径与服务端一致的那几项）");
    print_stats(&st);
    Ok(())
}

fn pct(sorted: &[i64], q: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (q * (sorted.len() - 1) as f64 + 0.5) as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// 打印聚合结论（本地与服务端共用一份渲染，口径不会两处不一致）。
fn print_stats(st: &Value) {
    let total = st["total"].as_i64().unwrap_or(0);
    println!("  轨迹 {total} 条");
    let map_line = |k: &str, m: Option<&Value>| {
        if let Some(o) = m.and_then(|x| x.as_object()) {
            if !o.is_empty() {
                let parts: Vec<String> = o
                    .iter()
                    .map(|(k2, v)| format!("{k2} {}", v.as_i64().unwrap_or(0)))
                    .collect();
                println!("   {k}：{}", parts.join(" · "));
            }
        } else if k.is_empty() {
            return;
        }
    };
    map_line("结论", st.get("byStatus"));
    map_line("种类", st.get("byKind"));
    map_line("内容级别", st.get("byPayload"));
    map_line("按制品版本", st.get("bySubjectVersion"));
    map_line("模型", st.get("byModel"));
    map_line("结论分布", st.get("grades"));
    map_line("失败归类", st.get("failures"));
    map_line("数据切分", st.get("splits"));
    let d = &st["durationMs"];
    if total > 0 {
        println!(
            "   耗时 ms：min {} · p50 {} · p90 {} · max {}",
            d["min"].as_i64().unwrap_or(0),
            d["p50"].as_i64().unwrap_or(0),
            d["p90"].as_i64().unwrap_or(0),
            d["max"].as_i64().unwrap_or(0)
        );
        println!(
            "   用量：输入 {} tokens · 输出 {} tokens · 花费 {:.4} 美元",
            st["inputTokens"].as_i64().unwrap_or(0),
            st["outputTokens"].as_i64().unwrap_or(0),
            st["costUsdMicros"].as_i64().unwrap_or(0) as f64 / 1e6
        );
        let labeled = st["labeled"].as_i64().unwrap_or(0);
        let pctv = if total > 0 { labeled as f64 * 100.0 / total as f64 } else { 0.0 };
        println!(
            "   标注覆盖率：{labeled}/{total}（{pctv:.0}%）{}\n   平均分（只算打过分的）：{}",
            if labeled == 0 { "  ← 还没打分，评测结论先别下" } else { "" },
            st["scoreAvgMilli"]
                .as_i64()
                .map(|x| format!("{:.3}", x as f64 / 1000.0))
                .unwrap_or_else(|| "-".into())
        );
    }
}

fn export(cfg: &CliConfig, a: &ExportArgs) -> Result<()> {
    let body: String;
    let note: String;
    if a.remote {
        let t = config::require_token(cfg)?;
        let q = query_string(&[
            ("ref", a.reference.clone()),
            ("kind", a.kind.clone()),
            ("status", a.status.clone()),
            ("agent", a.agent.clone()),
            ("tag", a.tag.clone()),
            ("grade", a.grade.clone()),
            ("split", a.split.clone()),
            ("labeled", a.labeled.map(|x| x.to_string())),
            ("since", a.since.clone()),
            ("until", a.until.clone()),
            ("limit", Some(a.limit.to_string())),
            ("format", Some("jsonl".to_string())),
        ]);
        let (bytes, headers) = api::get_bytes_with_headers(cfg, &format!("/api/traces/export{q}"), Some(&t))?;
        body = String::from_utf8_lossy(&bytes).to_string();
        note = format!(
            "# 数据集摘要 {} · {} 条{}（服务端算的，本地重新序列化算不出同一个值）",
            headers.get("x-ncc-dataset-digest").cloned().unwrap_or_else(|| "-".into()),
            headers.get("x-ncc-trace-count").cloned().unwrap_or_else(|| "0".into()),
            if headers.contains_key("x-ncc-truncated") { " · 被 limit 截断" } else { "" }
        );
    } else {
        // 本地导出：形状与服务端 JSONL 对齐（`{spec, trace, meta?, evaluation?}`）。
        let labels = labels_map();
        let mut out = String::new();
        let mut n = 0usize;
        for t in local_traces() {
            if let Some(r) = &a.reference {
                if t.pointer("/subject/ref").and_then(|x| x.as_str()) != Some(r.as_str()) {
                    continue;
                }
            }
            if n >= a.limit {
                break;
            }
            let id = t.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let mut row = json!({ "spec": DATASET_SPEC, "trace": t });
            if let Some(ls) = labels.get(&id) {
                row["evaluation"] = ls.clone();
            }
            out.push_str(&row.to_string());
            out.push('\n');
            n += 1;
        }
        let sum = Sha256::digest(out.as_bytes());
        note = format!(
            "# 本地数据集摘要 sha256:{} · {} 条（服务端那份有权威摘要）",
            hex(&sum),
            n
        );
        body = out;
    }
    if body.is_empty() {
        bail!("没有可导出的轨迹（换个过滤条件，或先 ncc trace push 上传）");
    }
    // 打成数据集快照包：轨迹是**数据**不是包 —— 只有快照（按了时间、说清许可与
    // 载荷级别）才适合被签名、被分发。这一层与 kb / mem / ckpt 的快照共用同一个封装。
    if let Some(dir) = &a.as_package {
        let n = body.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).count();
        if n == 0 {
            bail!("导出的数据集里没有轨迹行（只有摘要注释），不成包");
        }
        let name = if a.name.trim().is_empty() {
            format!("轨迹数据集快照（{n} 条）")
        } else {
            a.name.clone()
        };
        let docs = vec![(
            spec::DataDoc {
                path: format!("{}traces.jsonl", hur_core::spec::DATA_DIR),
                title: name.clone(),
                kind: "dataset".into(),
                format: "jsonl".into(),
                ..Default::default()
            },
            body.clone().into_bytes(),
        )];
        return crate::state::write_seed(
            "trace-set",
            dir,
            &name,
            &a.version,
            &format!("运行轨迹数据集（{n} 条；{}）", if a.remote { "取自节点" } else { "取自本机暂存" }),
            &a.reference.clone().unwrap_or_else(|| if a.remote { "remote".into() } else { "local".into() }),
            &a.privacy,
            &a.license,
            Some(&a.payload),
            docs,
            false,
        );
    }
    match &a.out {
        Some(p) => {
            write_private(std::path::Path::new(p), &body)?;
            println!("✅ 已写出 {p}");
            println!("{note}");
        }
        None => {
            eprintln!("{note}");
            print!("{body}");
        }
    }
    Ok(())
}

fn label(cfg: &CliConfig, a: &LabelArgs) -> Result<()> {
    let id = a.id.clone();
    if a.remote {
        let t = config::require_token(cfg)?;
        // 服务端的标注是一条条 append：一个动作一条，历史留得住。
        let mut sent = 0;
        if let Some(g) = &a.grade {
            api::post_json(cfg, &format!("/api/traces/{}/labels", api::urlenc(&id)), Some(&t), &json!({"key":"grade","value":g,"note":a.note,"by":a.by}))?;
            sent += 1;
        }
        if let Some(r) = a.reward {
            api::post_json(cfg, &format!("/api/traces/{}/labels", api::urlenc(&id)), Some(&t), &json!({"reward":r,"note":a.note,"by":a.by}))?;
            sent += 1;
        }
        if let Some(s) = a.score {
            if !(-1.0..=1.0).contains(&s) {
                bail!("--score 要在 -1..1 之间（内部按千分位整数存）");
            }
            api::post_json(cfg, &format!("/api/traces/{}/labels", api::urlenc(&id)), Some(&t), &json!({"score":(s*1000.0).round() as i64,"note":a.note,"by":a.by}))?;
            sent += 1;
        }
        for (k, v) in [("task", &a.task), ("split", &a.split), ("failure", &a.failure)] {
            if let Some(x) = v {
                api::post_json(cfg, &format!("/api/traces/{}/labels", api::urlenc(&id)), Some(&t), &json!({"key":k,"value":x,"note":a.note,"by":a.by}))?;
                sent += 1;
            }
        }
        if sent == 0 {
            bail!("没给任何标注（用 --grade / --reward / --score / --task / --split / --failure）");
        }
        println!("✅ 已标注 {sent} 项 → 服务端的 {id}");
        return Ok(());
    }

    // 本地标注：追加进 labels.json（与服务端的"只追加"一致）。
    if find_local(&id).is_none() {
        bail!("本地暂存里没有 {id}（查服务端的要加 --remote）");
    }
    let mut labels = labels_map();
    let mut arr = labels
        .get(&id)
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut push = |k: &str, v: Value| {
        arr.push(json!({
            "key": k, "value": v, "by": a.by.clone().unwrap_or_default(),
            "note": a.note.clone().unwrap_or_default(), "at": now_iso(),
        }));
    };
    if let Some(g) = &a.grade {
        push("grade", Value::String(g.clone()));
    }
    if let Some(r) = a.reward {
        push("reward", Value::from(r));
    }
    if let Some(s) = a.score {
        push("score", Value::from((s * 1000.0).round() as i64));
    }
    for (k, v) in [("task", &a.task), ("split", &a.split), ("failure", &a.failure)] {
        if let Some(x) = v {
            push(k, Value::String(x.clone()));
        }
    }
    if arr.is_empty() {
        bail!("没给任何标注（用 --grade / --reward / --score / --task / --split / --failure）");
    }
    labels.insert(id.clone(), Value::Array(arr));
    save_json(&labels_path(), &Value::Object(labels))?;
    println!("✅ 已本地标注 {id}（上传标注：ncc trace label {id} --remote …）");
    Ok(())
}

fn kinds(cfg: &CliConfig, a: &KindsArgs) -> Result<()> {
    if a.remote {
        let t = config::token_opt(cfg);
        let d = api::get(cfg, "/api/traces/kinds", t.as_deref())?;
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let out = json!({
        "spec": TRACE_SPEC,
        "runTraceSpec": RUN_TRACE_SPEC,
        "datasetSpec": DATASET_SPEC,
        "kinds": KINDS,
        "statuses": STATUSES,
        "payloads": PAYLOADS,
        "stepTypes": STEP_TYPES,
        "grades": GRADES,
        "splits": SPLITS,
        "limits": { "previewMax": PREVIEW_MAX, "maxSteps": MAX_STEPS, "batchMax": BATCH_MAX },
        "note": "本地内置词表；--remote 取目标节点的（含常用标注键与上限）",
    });
    if a.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("轨迹规范 {TRACE_SPEC}");
    println!("  种类      {}", KINDS.join(" · "));
    println!("  结论      {}（cancelled 与 error 分开算）", STATUSES.join(" · "));
    println!("  内容级别  {}（默认 digest：只有哈希与结构，不含原文）", PAYLOADS.join(" · "));
    println!("  步骤类型  {}", STEP_TYPES.join(" · "));
    println!("  标注建议  grade={} · split={}", GRADES.join("|"), SPLITS.join("|"));
    println!("  上限      预览 {:?} 字节 · 步骤 {} · 单批 {}", PREVIEW_MAX, MAX_STEPS, BATCH_MAX);
    println!("  执行留痕  {RUN_TRACE_SPEC}（ncc hur run 写的，可直接 ncc trace add --file 采集）");
    println!("  数据集    {DATASET_SPEC}（一行一条：{{spec,trace,meta[,evaluation]}}）");
    Ok(())
}

fn rm(cfg: &CliConfig, a: &RmArgs) -> Result<()> {
    if a.remote {
        if !a.yes {
            bail!("删服务端的轨迹要 --yes（本地删除不用）");
        }
        let t = config::require_token(cfg)?;
        let d = api::del(cfg, &format!("/api/traces/{}", api::urlenc(&a.id)), Some(&t))?;
        println!("✅ 已删除服务端的 {}", d["id"].as_str().unwrap_or(&a.id));
        return Ok(());
    }
    let hit = find_local(&a.id).with_context(|| format!("本地暂存里没有 {}", a.id))?;
    let id = hit.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if id.is_empty() {
        bail!("这条本地轨迹没有 id，无法删除");
    }
    let p = spool_path(&id);
    if p.exists() {
        std::fs::remove_file(&p)?;
    } else {
        bail!("找不到文件 {}", p.display());
    }
    let mut pushed = pushed_map();
    pushed.remove(&id);
    let mut labels = labels_map();
    labels.remove(&id);
    save_json(&pushed_path(), &Value::Object(pushed))?;
    save_json(&labels_path(), &Value::Object(labels))?;
    println!("✅ 已删除本地轨迹 {id}");
    Ok(())
}

fn status() -> Result<()> {
    let local = local_traces();
    let pushed = pushed_map();
    let labels = labels_map();
    let n = local.len();
    let np = local
        .iter()
        .filter(|t| pushed.contains_key(t.get("id").and_then(|x| x.as_str()).unwrap_or("")))
        .count();
    println!("本地轨迹暂存  {}", traces_dir().display());
    println!("  轨迹     {n} 条（已上传 {np} · 未上传 {}）", n - np);
    println!("  标注     {} 条轨迹有本地标注", labels.len());
    println!("  上传记录 {}", pushed_path().display());
    if n == 0 {
        println!("\n还没采集过。三种采集方式：");
        println!("  ncc trace add --file <ncc-trace.json|jsonl>     # 原生轨迹文档");
        println!("  ncc trace add --file <run-trace.json>           # ncc hur run 的执行留痕");
        println!("  <你的 Agent> | ncc trace add --events           # 事件流（每行一个事件）");
    }
    Ok(())
}

/// 供 `ncc hur run` 采集用：把执行留痕变成一条轨迹写进本地暂存（**不联网**）。
pub fn capture_run_trace(rt: &Value, payload: &str) -> Result<String> {
    let a = AddArgs {
        file: None,
        events: false,
        kind: "hur-run".into(),
        reference: None,
        version: None,
        agent: None,
        node: None,
        user: None,
        status: None,
        duration_ms: None,
        model: None,
        tokens: None,
        cost_usd: None,
        labels: Vec::new(),
        tags: vec!["hur-run".into()],
        payload: payload.to_string(),
        redact: "strict".into(),
        no_digest: false,
        cli: None,
    };
    let mut doc = from_run_trace(rt, &a);
    normalize_doc(&mut doc, true);
    let errs = validate_doc(&doc);
    if !errs.is_empty() {
        bail!("生成的轨迹不合规：{}", errs.join("; "));
    }
    let id = doc.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    ensure_dirs()?;
    write_private(&spool_path(&id), &serde_json::to_string_pretty(&doc)?)?;
    Ok(id)
}

/* ============================ 测试 ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 Go 侧 `model/trace_test.go::vectorDoc` **逐字段相同** —— 两边的摘要必须一致。
    fn vector_doc() -> Value {
        json!({
            "spec": TRACE_SPEC,
            "id": "TRC-0123456789abcdef0123",
            "kind": "agent",
            "at": "2026-09-26T10:00:00Z",
            "durationMs": 1234,
            "status": "ok",
            "source": {
                "node": "office-master", "host": "mac-1", "cli": "ncc/0.1.3",
                "agent": "harness-use", "user": "@me", "region": "shanghai-intranet"
            },
            "subject": {
                "ref": "@alice/hotel-skill", "kind": "skill", "version": "0.1.0",
                "digest": "sha256:aa11", "engine": "wasm", "policy": "strict"
            },
            "model": { "provider": "openai", "name": "gpt-4o-mini", "calls": 2 },
            "usage": { "inputTokens": 120, "outputTokens": 45, "costUsdMicros": 2100 },
            "steps": [
                { "i": 0, "type": "llm", "name": "plan", "ms": 400, "status": "ok",
                  "inDigest": "sha256:bb22", "outDigest": "sha256:cc33" },
                { "i": 1, "type": "tool", "name": "kb.search", "ms": 30, "status": "ok",
                  "inDigest": "sha256:dd44", "outDigest": "sha256:ee55" }
            ],
            "labels": { "task": "book-hotel", "grade": "pass", "reward": 1 },
            "tags": ["prod", "hotel"],
            "payload": "preview",
            "redaction": { "applied": true, "level": "strict", "rules": ["email", "api_key"] },
            "notes": "示例轨迹"
        })
    }

    /// **跨语言基线**：这个十六进制串与 Go 侧测试里的常量必须一样。
    /// 规则一改（加字段、换分隔符），两边必须同时改 —— 否则上线后所有上报都会被
    /// 服务端以 `digest_mismatch` 拒掉，而且是"本地测试全绿、线上全红"。
    #[test]
    fn digest_matches_go_vector() {
        let d = vector_doc();
        assert_eq!(
            trace_digest(&d),
            "sha256:867eb490e45d2ec56189ee2dd5513eec184465754e3e334ffee71d032be78526"
        );
        assert_eq!(trace_id(&d), "TRC-867eb490e45d2ec56189");
    }

    #[test]
    fn digest_ignores_payload_but_covers_structure_and_labels() {
        let a = vector_doc();
        let mut b = vector_doc();
        b["steps"][0]["in"] = Value::String("原始提示词 a@b.com".into());
        assert_eq!(trace_digest(&a), trace_digest(&b), "原文不该进摘要");

        let mut c = vector_doc();
        c["steps"].as_array_mut().unwrap().push(json!({"i":2,"type":"note","name":"extra"}));
        assert_ne!(trace_digest(&a), trace_digest(&c), "结构必须进摘要");

        let mut d = vector_doc();
        d["labels"]["grade"] = Value::String("fail".into());
        assert_ne!(trace_digest(&a), trace_digest(&d), "标签必须进摘要");
    }

    #[test]
    fn digest_is_order_independent_for_labels() {
        let a = vector_doc();
        let mut b = vector_doc();
        b["labels"] = json!({ "reward": 1, "grade": "pass", "task": "book-hotel" });
        assert_eq!(trace_digest(&a), trace_digest(&b));
    }

    #[test]
    fn redaction_masks_the_things_that_leak() {
        let (out, rules) = redact_text(
            "mail a@corp.com key sk-abcdefgh12345678 ip 10.1.2.3 Bearer tok123abc hash 7f3a9c1d2e4b5a6f7c8d9e0f1a2b3c4d",
            "strict",
        );
        assert!(out.contains("<email>"), "{out}");
        assert!(out.contains("<api_key>"), "{out}");
        assert!(out.contains("<ipv4>"), "{out}");
        assert!(out.contains("<bearer>"), "{out}");
        assert!(out.contains("<long_secret>"), "{out}");
        assert!(rules.contains(&"email".to_string()));
        // 摘要前缀本身不是秘密，别把它当成密钥抹掉
        let (out2, _) = redact_text("digest sha256:867eb490e45d2ec56189ee2dd5513eec184465754e3e334ffee71d032be78526", "strict");
        assert!(out2.contains("sha256:867e"), "{out2}");
    }

    #[test]
    fn preview_is_truncated_on_char_boundary_and_records_the_rule() {
        let long = "中".repeat(400); // 1200 字节
        let (s2, o2, r2) = prepare_payload(&long, "", "preview", "off");
        assert!(s2.len() <= PREVIEW_MAX);
        assert!(s2.chars().all(|c| c == '中'), "不能把多字节字符劈开");
        assert!(o2.is_empty());
        assert!(r2.contains(&"truncate".to_string()));
    }

    #[test]
    fn digest_level_keeps_no_payload_at_all() {
        let (a, b, rules) = prepare_payload("秘密", "输出", "digest", "strict");
        assert!(a.is_empty() && b.is_empty() && rules.is_empty());
    }

    #[test]
    fn run_trace_is_converted_honestly() {
        let rt = json!({
            "spec": RUN_TRACE_SPEC, "at_unix": 1_790_380_800, "package": "A-x-000001",
            "name": "x", "version": "0.1.0", "policy": "strict", "engine": "wasm",
            "limits": { "fuel": 1000 }, "outcome": { "ok": true, "ms": 42 }
        });
        let a = AddArgs {
            file: None, events: false, kind: "hur-run".into(), reference: None,
            version: None, agent: None, node: None, user: None, status: None,
            duration_ms: None, model: None, tokens: None, cost_usd: None,
            labels: vec![], tags: vec![], payload: "digest".into(), redact: "strict".into(),
            no_digest: false, cli: None,
        };
        let d = from_run_trace(&rt, &a);
        assert_eq!(d["kind"], "hur-run");
        assert_eq!(d["status"], "ok");
        assert_eq!(d["durationMs"], 42);
        assert_eq!(d["subject"]["version"], "0.1.0");
        assert_eq!(d["subject"]["engine"], "wasm");
        // 留痕里没有原文 —— 所以转出来的轨迹也如实标成 digest 级别。
        assert_eq!(d["payload"], "digest");
        assert_eq!(d["at"], "2026-09-26T00:00:00Z");
    }

    #[test]
    fn events_are_wrapped_into_steps_with_digests() {
        let events = vec![
            json!({"type":"llm","name":"plan","ms":100,"in":"hi","out":"hello","model":"gpt-4o-mini","usage":{"inputTokens":3,"outputTokens":4}}),
            json!({"type":"tool","name":"kb.search","ms":5,"in":{"q":"x"},"out":"ok"}),
        ];
        let a = AddArgs {
            file: None, events: true, kind: "agent".into(), reference: Some("@a/x".into()),
            version: Some("0.1.0".into()), agent: Some("harness-use".into()), node: None,
            user: None, status: None, duration_ms: None, model: None, tokens: None,
            cost_usd: None, labels: vec!["task=book-hotel".into()], tags: vec!["prod".into()],
            payload: "preview".into(), redact: "strict".into(), no_digest: false, cli: None,
        };
        let d = from_events(&events, &a);
        let steps = d["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["inDigest"].as_str().unwrap().starts_with("sha256:"), true);
        assert_eq!(steps[1]["in"], "{\"q\":\"x\"}", "非字符串入参按 JSON 文本存");
        assert_eq!(d["model"]["calls"], 1);
        assert_eq!(d["usage"]["inputTokens"], 3);
        assert_eq!(d["durationMs"], 105, "没给总耗时就按步骤求和");
        assert_eq!(d["labels"]["task"], "book-hotel");
        let errs = validate_doc(&{
            let mut x = d.clone();
            normalize_doc(&mut x, true);
            x
        });
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn validation_catches_what_the_server_would_reject() {
        let mut d = vector_doc();
        normalize_doc(&mut d, true);
        assert!(validate_doc(&d).is_empty());

        let mut bad = d.clone();
        bad["kind"] = Value::String("chat".into());
        assert!(!validate_doc(&bad).is_empty());

        let mut bad = d.clone();
        bad["payload"] = Value::String("digest".into());
        bad["steps"][0]["in"] = Value::String("原文".into());
        assert!(!validate_doc(&bad).is_empty(), "digest 级别带原文要被拦");

        let mut bad = d.clone();
        bad["digest"] = Value::String("sha256:deadbeef".into());
        assert!(!validate_doc(&bad).is_empty(), "摘要被改过要被拦");
    }

    #[test]
    fn status_labels_are_integers_only() {
        // 关键是"不把浮点写进标签"：那会让两端摘要不一致。
        let mut labels = Map::new();
        let a = AddArgs {
            file: None, events: false, kind: "agent".into(), reference: None, version: None,
            agent: None, node: None, user: None, status: None, duration_ms: None,
            model: Some("openai/gpt-4o-mini".into()), tokens: None, cost_usd: None,
            labels: vec!["reward=1".into(), "task=book-hotel".into(), "bad-no-equals".into()],
            tags: vec![], payload: "digest".into(), redact: "strict".into(),
            no_digest: false, cli: None,
        };
        apply_label_args(&mut labels, &a);
        assert_eq!(labels["reward"], json!(1));
        assert!(labels["reward"].is_i64());
        assert_eq!(labels["task"], json!("book-hotel"));
        assert!(!labels.contains_key("bad-no-equals"));
    }

    #[test]
    fn spool_paths_cannot_escape_the_directory() {
        let p = spool_path("../../../etc/passwd");
        assert!(!p.to_string_lossy().contains(".."), "{p:?}");
        assert_eq!(p.parent(), Some(spool_dir().as_path()));
        assert!(p.file_name().unwrap().to_string_lossy().ends_with(".json"));
    }
}

/// 数据快照包里的轨迹集：**进本机暂存**（与 `ncc trace add` 同一条路，不另写一套）。
///
/// 为什么不做成"直接推节点"：轨迹进暂存是本地动作、上传是另一个动作（`ncc trace push`）。
/// 一个包不该替使用者决定"要不要把别人的数据传上去"。
pub fn import_dataset(rel: &std::path::Path) -> Result<()> {
    // 快照包里的路径是包内相对路径；调用方已经切到包根，这里直接用
    let a = AddArgs {
        file: Some(rel.to_string_lossy().to_string()),
        events: false,
        kind: "agent".into(),
        reference: None,
        version: None,
        agent: None,
        node: None,
        user: None,
        status: None,
        ..AddArgs::default()
    };
    let cfg = config::load();
    add(&cfg, &a)
}
