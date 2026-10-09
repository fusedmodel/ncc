//! 算力画像（compute profile）：**这台机器 / 这个集群能跑什么任务**。
//!
//! 一句话：`ncc profile` 讲的是"人是谁"，本模块讲的是"机器能干什么" —— 两者都挂在
//! `ncc profile` 下（`ncc profile node …`），因为对 Agent 来说它们是同一个问题的两半：
//! **谁**去干、**在哪台机器上**干得动。
//!
//! # 解决什么
//!
//! Agent 拿到一个任务（编译一个 Rust 服务、跑一个前端构建、起一个容器、做一次推理），
//! 第一步不是动手，而是判断**这台机器/这个集群干不干得动**：CPU 几核、内存多少、
//! 磁盘剩多少、有没有对应的工具链、有没有 GPU、能不能访问那个已授权的服务面。
//! 今天这些信息散在 `nproc` / `free` / `which` / `ncc meta` 四处，每换一台机器都要重问一遍。
//!
//! 本模块把它收敛成**一份可采集、可上传、可打包、可评估**的画像：
//!
//! ```text
//! ncc profile node scan    采集（本机探测，落 ~/.ncc/compute-profile.json）
//! ncc profile node show    评估（能跑什么 / 缺什么 / Agent 下一步照做的流程）
//! ncc profile node pack    打包（`.huf` 资源包：可签名、可发布、可发给 Agent）
//! ncc profile node push    上传（平台 `/api/compute/profiles`；没这条能力时降级成 feedback 留痕）
//! ncc profile node fleet   集群统计（平台 `/api/compute/stats`：多少个节点能跑什么）
//! ncc profile node fit     任务适配（平台 `/api/compute/fit?need=…`：谁满足这份需求）
//! ```
//!
//! # 三条边界（别在实现里糊掉）
//!
//! 1. **画像不是盘点资产**：它只为"能不能跑这个任务"服务。收不到的（登录态、内部配额、
//!    许可证）宁可不写，也不要猜一个数字出来 —— 猜出来的容量比没有更危险。
//! 2. **采集是本地动作**：`scan` 一个字节都不上传。上传是显式的 `push`，且默认 **private**。
//! 3. **判据只有一处**：任务档位（[`TASKS`]）与需求表达式（[`parse_need`]）在本模块里定义，
//!    CLI、平台、网页读的是同一份语义 —— 平台只做"通用匹配与统计"，不认识具体任务名。

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{self, require_token, CliConfig};

/// 画像规范号（平台按它校验；不认的一律拒收，别让两个版本的形状混在一张表里）。
pub const SPEC: &str = "ncc-compute-profile/v1";
/// 本地画像文件名（`~/.ncc/compute-profile.json`）。
pub const FILE: &str = "compute-profile.json";

/* ================= 数据模型 ================= */

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub region: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cpu {
    #[serde(default)]
    pub model: String,
    /// 物理核
    #[serde(default)]
    pub cores: i64,
    /// 逻辑核（可并行度：评估时按它算，别按物理核高估）
    #[serde(default)]
    pub threads: i64,
    #[serde(default)]
    pub load1: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mem {
    #[serde(default)]
    pub total_mb: i64,
    /// 采集那一刻的可用内存（**会变**，所以它只是"当时"的快照，评估按 total 算门槛）
    #[serde(default)]
    pub available_mb: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub total_gb: i64,
    #[serde(default)]
    pub free_gb: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub mem_mb: i64,
    #[serde(default)]
    pub driver: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub path: String,
    /// 分类（compiler / container / data / ai / vcs / media），给展示分组用
    #[serde(default)]
    pub kind: String,
}

/// 一个**可达且被授权**的服务面：本机能不能调它、怎么调、要谁给授权。
///
/// 与 `ncc services`（对外发布的业务服务）不是一回事：这里是**从这台机器往外看**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub product: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 这台机器现在**能用哪一种凭据**：session（登录态）| token（API-Key）| none（只能读公开面）
    #[serde(default)]
    pub auth: String,
    /// 要拿到更深的权限该做什么（写成人能照做的一句话）
    #[serde(default)]
    pub how_to: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// 建议的并行任务数（取逻辑核，留一核给调度）
    #[serde(default)]
    pub max_parallel_tasks: i64,
    /// 本机沙箱能力（`ncc hur` 的执行面）
    #[serde(default)]
    pub sandbox: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub spec: String,
    #[serde(default)]
    pub node: NodeRef,
    #[serde(default)]
    pub collected_at: String,
    #[serde(default)]
    pub host: Host,
    #[serde(default)]
    pub cpu: Cpu,
    #[serde(default)]
    pub mem: Mem,
    #[serde(default)]
    pub disk: Vec<Disk>,
    #[serde(default)]
    pub gpu: Vec<Gpu>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub services: Vec<Service>,
    /// 由采集数据**推导**出来的能力标签（不是人写的）：`build-rust` / `gpu` / `mem-32g`…
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub limits: Limits,
    /// 采集者的话（"这台是内网构建机，别拿它跑推理"这类）
    #[serde(default)]
    pub note: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            spec: SPEC.to_string(),
            node: NodeRef::default(),
            collected_at: String::new(),
            host: Host::default(),
            cpu: Cpu::default(),
            mem: Mem::default(),
            disk: Vec::new(),
            gpu: Vec::new(),
            tools: Vec::new(),
            services: Vec::new(),
            tags: Vec::new(),
            limits: Limits::default(),
            note: String::new(),
        }
    }
}

impl Profile {
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tool(name).is_some()
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    /// 主磁盘（评估磁盘门槛时用它；多盘只作展示，不累加 —— 累加会凭空造出容量）。
    pub fn main_disk(&self) -> Option<&Disk> {
        self.disk.first()
    }

    pub fn gpu_count(&self) -> i64 {
        self.gpu.len() as i64
    }
}

/* ================= 任务档位与需求表达式 ================= */

/// 一个任务档位：`needs` 用与 `--need` 完全相同的语法（只有一套判据）。
#[derive(Debug, Clone, Copy)]
pub struct Task {
    pub id: &'static str,
    pub zh: &'static str,
    pub desc: &'static str,
    pub needs: &'static [&'static str],
}

/// 内置任务档位。
///
/// 为什么内置而不是让用户每次现写：**"能不能跑"这件事必须有共同词汇**，否则
/// "机器 A 说能跑" 与 "调度方说 A 不能跑" 会是两句都无法反驳的话。任务名是公共词汇，
/// 具体门槛写在这里，谁都可以对照。
pub const TASKS: &[Task] = &[
    Task {
        id: "build-rust",
        zh: "构建 Rust 项目",
        desc: "cargo build/test（需要 cargo 与至少 2 核，链接期吃内存）",
        needs: &["cpu>=2", "mem>=2G", "tool:cargo"],
    },
    Task {
        id: "build-node",
        zh: "构建前端 / Node 项目",
        desc: "npm/yarn 安装与打包",
        needs: &["cpu>=2", "mem>=2G", "tool:npm"],
    },
    Task {
        id: "build-python",
        zh: "跑 Python 任务",
        desc: "pip 安装与脚本执行",
        needs: &["cpu>=1", "mem>=1G", "tool:python3"],
    },
    Task {
        id: "build-go",
        zh: "构建 Go 项目",
        desc: "go build/test",
        needs: &["cpu>=2", "mem>=2G", "tool:go"],
    },
    Task {
        id: "container",
        zh: "起容器",
        desc: "需要 docker 或 podman（二者之一）",
        needs: &["cpu>=1", "mem>=1G", "tool:docker|podman"],
    },
    Task {
        id: "sandbox-hur",
        zh: "跑 HUR 沙箱包",
        desc: "ncc hur run --exec（wasmtime 内置，不需要额外工具）",
        needs: &["cpu>=1", "mem>=512M"],
    },
    Task {
        id: "inference-gpu",
        zh: "GPU 推理",
        desc: "至少一块 GPU 与 8G 显存",
        needs: &["cpu>=2", "mem>=8G", "gpu>=1", "gpumem>=8G"],
    },
    Task {
        id: "heavy-mem",
        zh: "大内存任务",
        desc: "编译大工程 / 内存型数据库 / 全量索引",
        needs: &["cpu>=4", "mem>=32G"],
    },
    Task {
        id: "disk-heavy",
        zh: "吃磁盘的任务",
        desc: "镜像构建 / 大文件处理 / 数据集导入",
        needs: &["disk>=100G"],
    },
    Task {
        id: "intranet-service",
        zh: "调内网已授权服务",
        desc: "本机能访问某个服务面（能力清单非空）",
        needs: &["service>=1"],
    },
];

pub fn task(id: &str) -> Option<&'static Task> {
    let id = id.trim();
    TASKS.iter().find(|t| t.id == id)
}

/// 一条需求。字段：`cpu` / `mem` / `disk` / `gpu` / `gpumem` / `tool:<名>` /
/// `service:<名>` / `service`（个数）/ `tag:<标签>`。
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub field: String,
    pub op: String,
    pub value: f64,
    /// 原样保留（报错与展示时用原话，别让人看到被改写过的需求）
    pub raw: String,
}

/// 解析需求表达式：逗号分隔，空白的忽略。单位支持 `512M` / `2G` / `1T`（内存按 MB、磁盘按 GB）。
///
/// 语法故意很小：`字段 操作符 数值`，或 `tool:名`（存在性）。写错的表达**当场报错**，
/// 不"猜一个意思继续跑" —— 猜错的代价是把任务派到跑不动的机器上。
pub fn parse_need(expr: &str) -> Result<Vec<Need>> {
    let mut out = Vec::new();
    for raw in expr.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        out.push(parse_one(raw)?);
    }
    if out.is_empty() {
        bail!("需求表达式是空的（例：cpu>=4,mem>=16G,tool:docker）");
    }
    Ok(out)
}

fn parse_one(raw: &str) -> Result<Need> {
    // 存在性写法：tool:docker / tag:gpu / service:ncc-node
    if let Some((k, v)) = raw.split_once(':') {
        let k = k.trim().to_lowercase();
        let v = v.trim();
        if v.is_empty() {
            bail!("需求「{raw}」少了名字（例：tool:docker）");
        }
        if !matches!(k.as_str(), "tool" | "service" | "tag") {
            bail!("需求「{raw}」的字段不认识（只支持 tool: / service: / tag: 前缀，或 cpu>=N / mem>=2G 这类）");
        }
        return Ok(Need { field: format!("{k}:{v}"), op: ">=".into(), value: 1.0, raw: raw.to_string() });
    }
    for op in [">=", "<=", ">", "<", "="] {
        if let Some((f, v)) = raw.split_once(op) {
            let field = f.trim().to_lowercase();
            if !matches!(field.as_str(), "cpu" | "mem" | "disk" | "gpu" | "gpumem" | "service") {
                bail!(
                    "需求「{raw}」的字段「{field}」不认识（支持 cpu / mem / disk / gpu / gpumem / service，或 tool:名）"
                );
            }
            let value = parse_amount(v.trim(), &field)
                .ok_or_else(|| anyhow!("需求「{raw}」的数值看不懂（例：2 或 16G）"))?;
            return Ok(Need { field, op: op.to_string(), value, raw: raw.to_string() });
        }
    }
    bail!("需求「{raw}」看不懂（例：cpu>=4 / mem>=16G / disk>=100G / tool:docker）")
}

/// `2` / `2G` / `512M` / `1T` → 数值。内存单位统一成 MB，磁盘统一成 GB。
fn parse_amount(s: &str, field: &str) -> Option<f64> {
    let up = s.to_ascii_uppercase();
    let (num, unit) = up
        .strip_suffix('T')
        .map(|n| (n, "T"))
        .or_else(|| up.strip_suffix('G').map(|n| (n, "G")))
        .or_else(|| up.strip_suffix('M').map(|n| (n, "M")))
        .unwrap_or((up.as_str(), ""));
    let n: f64 = num.trim().parse().ok()?;
    if n < 0.0 {
        return None;
    }
    // 内存与显存都按 MB 记，磁盘按 GB 记（与画像里的单位一致，免得换算散在四处）
    let is_mem = matches!(field, "mem" | "gpumem");
    let scale = match (unit, is_mem) {
        ("T", true) => 1024.0 * 1024.0,
        ("G", true) => 1024.0,
        ("", true) => 1.0,
        ("M", true) => 1.0,
        ("T", false) => 1024.0,
        ("G", false) => 1.0,
        ("M", false) => 1.0 / 1024.0,
        _ => 1.0,
    };
    Some(n * scale)
}

/// 一条需求的判定结果。
#[derive(Debug, Clone)]
pub struct Verdict {
    pub need: String,
    pub ok: bool,
    /// 不满足时：现在的值（"这台机器现在是多少"）
    pub actual: String,
    /// 不满足时：差什么（"补什么才够"）
    pub missing: String,
}

/// 评估一份画像对一组需求的满足情况。
pub fn evaluate(p: &Profile, needs: &[Need]) -> Vec<Verdict> {
    needs.iter().map(|n| check_one(p, n)).collect()
}

fn check_one(p: &Profile, n: &Need) -> Verdict {
    let raw = n.raw.clone();
    if let Some(name) = n.field.strip_prefix("tool:") {
        return match tool_match(p, name) {
            Some(t) => Verdict {
                need: raw,
                ok: true,
                actual: format!("{} {}", t.name, t.version).trim().to_string(),
                missing: String::new(),
            },
            None => Verdict {
                need: raw,
                ok: false,
                actual: "未安装".into(),
                missing: format!("装一个 {name}（或用带它的节点）"),
            },
        };
    }
    if let Some(name) = n.field.strip_prefix("tag:") {
        let ok = p.tags.iter().any(|t| t == name);
        return Verdict {
            need: raw,
            ok,
            actual: p.tags.join(" "),
            missing: if ok { String::new() } else { format!("需要标签 {name}") },
        };
    }
    if let Some(name) = n.field.strip_prefix("service:") {
        return match p.service(name) {
            Some(s) => Verdict {
                need: raw,
                ok: true,
                actual: format!("{}（{}）", s.base, s.auth),
                missing: String::new(),
            },
            None => Verdict {
                need: raw,
                ok: false,
                actual: "画像里没有这个服务面".into(),
                missing: format!("确认本机能访问 {name}，再 `ncc profile node scan` 一次"),
            },
        };
    }
    let actual = match n.field.as_str() {
        "cpu" => p.cpu.threads.max(p.cpu.cores) as f64,
        "mem" => p.mem.total_mb as f64,
        "disk" => p.main_disk().map(|d| d.free_gb as f64).unwrap_or(0.0),
        "gpu" => p.gpu_count() as f64,
        "gpumem" => p.gpu.iter().map(|g| g.mem_mb).max().unwrap_or(0) as f64,
        "service" => p.services.len() as f64,
        _ => 0.0,
    };
    let ok = compare(actual, &n.op, n.value);
    let unit = match n.field.as_str() {
        "mem" | "gpumem" => "MB",
        "disk" => "GB",
        _ => "",
    };
    Verdict {
        need: raw,
        ok,
        actual: format!("{}{}", fmt_num(actual), unit),
        missing: if ok {
            String::new()
        } else {
            format!("需要 {} {} {}{}", n.field, n.op, fmt_num(n.value), unit)
        },
    }
}

/// `tool:docker|podman`：竖线表示"二者之一"（同一个需求里给两个选项）。
fn tool_match<'a>(p: &'a Profile, name: &str) -> Option<&'a Tool> {
    for alt in name.split('|').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some(t) = p.tool(alt) {
            return Some(t);
        }
    }
    None
}

fn compare(actual: f64, op: &str, want: f64) -> bool {
    match op {
        ">=" => actual >= want,
        ">" => actual > want,
        "<=" => actual <= want,
        "<" => actual < want,
        "=" => (actual - want).abs() < f64::EPSILON,
        _ => false,
    }
}

fn fmt_num(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// 一个任务档位的评估结果（人读 + 机器读共用）。
pub fn evaluate_task(p: &Profile, t: &Task) -> Vec<Verdict> {
    let needs = t.needs.iter().filter_map(|s| parse_one(s).ok()).collect::<Vec<_>>();
    evaluate(p, &needs)
}

pub fn task_ok(p: &Profile, t: &Task) -> bool {
    evaluate_task(p, t).iter().all(|v| v.ok)
}

/* ================= 采集（本地探测） ================= */

pub struct ScanOpts {
    pub node_id: String,
    pub node_name: String,
    pub node_kind: String,
    pub region: String,
    /// 额外手工登记的服务面：`name=url`（可重复）
    pub services: Vec<String>,
    /// 跳过慢探测（`system_profiler` 这类）
    pub fast: bool,
    pub note: String,
}

impl Default for ScanOpts {
    fn default() -> Self {
        Self {
            node_id: String::new(),
            node_name: String::new(),
            node_kind: String::new(),
            region: String::new(),
            services: Vec::new(),
            fast: false,
            note: String::new(),
        }
    }
}

/// 跑一个外部命令并读它的 stdout（**带超时**：探测卡住不该把命令挂死）。
///
/// 只在超时后 kill；输出走管道读完整（`--version` 的量级不会把管道写满）。
fn probe_cmd(cmd: &str, args: &[&str], timeout_ms: u64) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_string(&mut out);
    }
    let mut err = String::new();
    if let Some(mut se) = child.stderr.take() {
        let _ = se.read_to_string(&mut err);
    }
    let text = if out.trim().is_empty() { err } else { out };
    Some(text.trim().to_string())
}

fn run_one(cmd: &str, args: &[&str]) -> Option<String> {
    probe_cmd(cmd, args, 1500)
}

/// 采集本机画像。**一个字节都不上传**（上传是显式动作，见 `push`）。
pub fn scan(cfg: &CliConfig, o: &ScanOpts) -> Result<Profile> {
    let mut p = Profile::default();
    p.node = NodeRef {
        id: o.node_id.trim().to_string(),
        name: if o.node_name.trim().is_empty() {
            run_one("hostname", &[]).unwrap_or_default()
        } else {
            o.node_name.trim().to_string()
        },
        kind: if o.node_kind.trim().is_empty() { "agent".into() } else { o.node_kind.trim().to_string() },
        region: o.region.trim().to_string(),
    };
    p.collected_at = now_rfc3339();
    p.note = o.note.trim().to_string();
    p.host = probe_host(o.fast);
    p.cpu = probe_cpu();
    p.mem = probe_mem();
    p.disk = probe_disk();
    p.gpu = probe_gpu(o.fast);
    p.tools = probe_tools();
    p.services = probe_services(cfg, &o.services);
    p.limits = Limits {
        max_parallel_tasks: (p.cpu.threads.max(1) - 1).max(1),
        sandbox: if cfg!(feature = "sandbox") { "wasmtime".into() } else { "none".into() },
    };
    p.tags = derive_tags(&p);
    Ok(p)
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // 只到秒的 UTC 时间戳：本机时区偏移对"这份画像是什么时候采的"没有信息量
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 天数（1970-01-01 起）→ 年月日（Howard Hinnant 的 civil_from_days，不引时区依赖）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn probe_host(fast: bool) -> Host {
    let mut h = Host {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        ..Host::default()
    };
    match std::env::consts::OS {
        "macos" => {
            h.os_version = run_one("sw_vers", &["-productVersion"]).unwrap_or_default();
            h.model = run_one("sysctl", &["-n", "hw.model"]).unwrap_or_default();
            if !fast {
                // `system_profiler` 慢（秒级），只用来补型号名，失败也无所谓
                if let Some(out) = probe_cmd("system_profiler", &["SPHardwareDataType"], 4000) {
                    if let Some(line) = out.lines().find(|l| l.contains("Model Name")) {
                        if let Some(v) = line.split(':').nth(1) {
                            h.model = format!("{} {}", v.trim(), h.model).trim().to_string();
                        }
                    }
                }
            }
        }
        "linux" => {
            h.os_version = std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("PRETTY_NAME="))
                        .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
                })
                .unwrap_or_default();
            h.model = std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
        }
        "windows" => {
            h.os_version = run_one("cmd", &["/C", "ver"]).unwrap_or_default();
            h.model = run_one("wmic", &["computersystem", "get", "model"]).unwrap_or_default();
        }
        _ => {}
    }
    h
}

fn probe_cpu() -> Cpu {
    let mut c = Cpu {
        threads: std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1),
        ..Cpu::default()
    };
    match std::env::consts::OS {
        "macos" => {
            c.model = run_one("sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_default();
            c.cores = run_one("sysctl", &["-n", "hw.physicalcpu"])
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(c.threads);
            if c.model.is_empty() {
                c.model = run_one("sysctl", &["-n", "hw.model"]).unwrap_or_default();
            }
        }
        "linux" => {
            if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
                c.model = text
                    .lines()
                    .find_map(|l| l.split_once(':').filter(|(k, _)| k.starts_with("model name")).map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_default();
                c.cores = text
                    .lines()
                    .filter(|l| l.starts_with("processor") && l.contains(": "))
                    .filter_map(|l| l.rsplit(':').next().map(|s| s.trim().to_string()))
                    .count() as i64;
            }
        }
        _ => {}
    }
    if c.cores <= 0 {
        c.cores = c.threads;
    }
    // 负载：拿不到就不写（0.0 会被误读成"很闲"）
    c.load1 = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0);
    c
}

fn probe_mem() -> Mem {
    match std::env::consts::OS {
        "linux" => {
            let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
            let kb = |key: &str| -> i64 {
                text.lines()
                    .find(|l| l.starts_with(key))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0)
            };
            Mem {
                total_mb: kb("MemTotal:") / 1024,
                available_mb: kb("MemAvailable:") / 1024,
            }
        }
        "macos" => {
            let total = run_one("sysctl", &["-n", "hw.memsize"])
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(0)
                / 1024
                / 1024;
            // 可用内存：free + inactive 页（vm_stat 的页大小取 hw.pagesize，取不到按 4K）
            let page = run_one("sysctl", &["-n", "hw.pagesize"])
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(4096);
            let mut avail = 0i64;
            if let Some(vm) = run_one("vm_stat", &[]) {
                for key in ["Pages free", "Pages inactive"] {
                    if let Some(line) = vm.lines().find(|l| l.starts_with(key)) {
                        if let Some(n) = line
                            .split(':')
                            .nth(1)
                            .map(|v| v.trim().trim_end_matches('.').to_string())
                            .and_then(|v| v.parse::<i64>().ok())
                        {
                            avail += n * page / 1024 / 1024;
                        }
                    }
                }
            }
            Mem { total_mb: total, available_mb: avail }
        }
        _ => Mem::default(),
    }
}

fn probe_disk() -> Vec<Disk> {
    let mut out = Vec::new();
    // `-k -P` 两边都支持（POSIX 输出：一行一块设备）
    for path in ["/", "/tmp"] {
        if let Some(text) = run_one("df", &["-k", "-P", path]) {
            if let Some(line) = text.lines().nth(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 4 {
                    let total = cols[1].parse::<i64>().unwrap_or(0) / 1024 / 1024;
                    let free = cols[3].parse::<i64>().unwrap_or(0) / 1024 / 1024;
                    out.push(Disk { path: path.to_string(), total_gb: total, free_gb: free });
                }
            }
        }
    }
    out
}

fn probe_gpu(fast: bool) -> Vec<Gpu> {
    let mut out = Vec::new();
    // NVIDIA（Linux/Windows 都有 nvidia-smi）：一次问全，字段以逗号分隔
    if let Some(text) = probe_cmd(
        "nvidia-smi",
        &["--query-gpu=name,memory.total,driver_version", "--format=csv,noheader"],
        2000,
    ) {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            let mem = cols
                .get(1)
                .map(|v| v.trim_end_matches(|c: char| !c.is_ascii_digit()).trim().to_string())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            out.push(Gpu {
                vendor: "nvidia".into(),
                model: cols.first().copied().unwrap_or("").to_string(),
                mem_mb: mem,
                driver: cols.get(2).copied().unwrap_or("").to_string(),
            });
        }
    }
    // Apple 芯片：统一内存，不单独报显存（写清"共享"比编一个数字好）
    if out.is_empty() && std::env::consts::OS == "macos" && !fast {
        if let Some(text) = probe_cmd("system_profiler", &["SPDisplaysDataType"], 4000) {
            let chip = text
                .lines()
                .find(|l| l.contains("Chipset Model"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if !chip.is_empty() {
                out.push(Gpu {
                    vendor: "apple".into(),
                    model: chip,
                    mem_mb: 0,
                    driver: "统一内存".into(),
                });
            }
        }
    }
    out
}

/// 要探测的工具链：`(可执行名, 版本参数, 分类)`。
///
/// 这份表是**采集口径**：探到就算"这台机器能访问这个软件"。装了但不在 PATH 里的一律不算
/// —— 那正是"跑不动"的真实原因，替用户补上反而是骗人。
const TOOL_PROBES: &[(&str, &[&str], &str)] = &[
    ("rustc", &["--version"], "compiler"),
    ("cargo", &["--version"], "compiler"),
    ("go", &["version"], "compiler"),
    ("gcc", &["--version"], "compiler"),
    ("clang", &["--version"], "compiler"),
    ("make", &["--version"], "compiler"),
    ("cmake", &["--version"], "compiler"),
    ("node", &["--version"], "runtime"),
    ("npm", &["--version"], "runtime"),
    ("bun", &["--version"], "runtime"),
    ("deno", &["--version"], "runtime"),
    ("python3", &["--version"], "runtime"),
    ("pip3", &["--version"], "runtime"),
    ("java", &["-version"], "runtime"),
    ("dotnet", &["--version"], "runtime"),
    ("docker", &["--version"], "container"),
    ("podman", &["--version"], "container"),
    ("kubectl", &["version", "--client", "--short"], "container"),
    ("sqlite3", &["--version"], "data"),
    ("psql", &["--version"], "data"),
    ("redis-cli", &["--version"], "data"),
    ("ollama", &["--version"], "ai"),
    ("wasmtime", &["--version"], "ai"),
    ("git", &["--version"], "vcs"),
    ("curl", &["--version"], "net"),
    ("jq", &["--version"], "text"),
    ("ffmpeg", &["-version"], "media"),
    ("rsync", &["--version"], "net"),
];

fn probe_tools() -> Vec<Tool> {
    let mut out = Vec::new();
    for (name, args, kind) in TOOL_PROBES {
        // `java -version` 只往 stderr 写版本；run() 已经把 stderr 兜住了
        let Some(text) = probe_cmd(name, args, 1500) else { continue };
        let first = text.lines().next().unwrap_or("").trim().to_string();
        out.push(Tool {
            name: (*name).to_string(),
            version: normalize_version(&first),
            path: which(name),
            kind: (*kind).to_string(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 从版本输出里抠出版本号（不同工具格式差得远，抠不到就留一句照样子的原文）。
fn normalize_version(line: &str) -> String {
    let l = line.trim();
    if l.is_empty() {
        return String::new();
    }
    // 目标形状：`cargo 1.98.0 (…)` / `v22.23.2` / `Python 3.12.4` / `go version go1.24.1 darwin/arm64`
    // / `openjdk version "17.0.11" 2026-08-18` —— 判据是"**数字开头且带点**"，
    // 这样 `go1.24.1` 抠得出 `1.24.1`，而 `2026-08-18`（日期，没有点）不会被当成版本号。
    let mut fallback = String::new();
    for tok in l.split_whitespace() {
        let t: String = tok
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '+' && c != '-')
            .to_string();
        let Some(idx) = t.find(|c: char| c.is_ascii_digit()) else { continue };
        let tail = &t[idx..];
        if tail.contains('.') {
            return tail.to_string();
        }
        if fallback.is_empty() {
            fallback = tail.to_string();
        }
    }
    if !fallback.is_empty() {
        return fallback;
    }
    l.chars().take(60).collect()
}

/// 可执行文件路径（探测口径要写清楚"用的是哪一个"：一台机器上常有两个 python）。
fn which(name: &str) -> String {
    let out = if cfg!(windows) {
        run_one("cmd", &["/C", &format!("where {name}")])
    } else {
        run_one("sh", &["-c", &format!("command -v {name}")])
    };
    out.unwrap_or_default().lines().next().unwrap_or("").trim().to_string()
}

/// 问一次某个 base 的自述（失败给 None：**老服务端照样要记下来**，base 本身是事实）。
fn probe_meta(base: &str) -> Option<crate::capability::Meta> {
    let m = crate::capability::probe_base(base);
    if m.is_unknown() {
        None
    } else {
        Some(m)
    }
}

/// 可达服务面：本地登记的每个 target + 手工加的那些，各问一次 `/api/meta`。
///
/// 这里回答的是"**从这台机器往外看**能不能调它、用哪种凭据"，与 `ncc services`（对外发布）
/// 方向相反。问不到 meta 的老服务端照样记下来（base 是事实，能力列不出来就空着）。
fn probe_services(cfg: &CliConfig, extra: &[String]) -> Vec<Service> {
    let mut out: Vec<Service> = Vec::new();
    for (name, t) in &cfg.targets {
        let token = t.token.clone().unwrap_or_default();
        let meta = probe_meta(&t.base_url);
        out.push(Service {
            name: name.clone(),
            base: t.base_url.clone(),
            product: meta.as_ref().map(|m| m.product.clone()).unwrap_or_default(),
            kind: meta.as_ref().map(|m| m.kind.clone()).unwrap_or_default(),
            capabilities: meta.as_ref().map(|m| m.capabilities.clone()).unwrap_or_default(),
            auth: if token.is_empty() { "none".into() } else { "session".into() },
            how_to: if token.is_empty() {
                format!("`ncc --base {} login` 之后可读私有面", t.base_url)
            } else {
                "要取别人的私有制品：`ncc grant set --user @某人 --kind service`".into()
            },
        });
    }
    for raw in extra {
        let (name, base) = raw.split_once('=').unwrap_or((raw.as_str(), ""));
        let name = name.trim();
        let base = base.trim().trim_end_matches('/');
        if name.is_empty() || base.is_empty() {
            continue;
        }
        let meta = probe_meta(base);
        out.push(Service {
            name: name.to_string(),
            base: base.to_string(),
            product: meta.as_ref().map(|m| m.product.clone()).unwrap_or_default(),
            kind: meta.as_ref().map(|m| m.kind.clone()).unwrap_or_default(),
            capabilities: meta.as_ref().map(|m| m.capabilities.clone()).unwrap_or_default(),
            auth: "none".into(),
            how_to: String::new(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 从采集结果**推导**能力标签。
///
/// 为什么要推导而不是让用户手写：手写的标签会漂（机器换了、工具卸了，标签还挂着），
/// 而调度方信的就是这些标签。推导出来的标签只反映**这份画像采集时的事实**，
/// 谁都能按同一份规则复算 —— 这是它可信的原因。
pub fn derive_tags(p: &Profile) -> Vec<String> {
    let mut t: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !t.contains(&s) {
            t.push(s);
        }
    };
    for tool_tag in [
        ("cargo", "build-rust"),
        ("npm", "build-node"),
        ("node", "run-node"),
        ("python3", "run-python"),
        ("go", "build-go"),
        ("docker", "container"),
        ("podman", "container"),
        ("kubectl", "k8s"),
        ("ollama", "llm-local"),
        ("ffmpeg", "media"),
        ("psql", "postgres"),
        ("sqlite3", "sqlite"),
    ] {
        if p.has_tool(tool_tag.0) {
            push(tool_tag.1.to_string());
        }
    }
    if p.gpu_count() > 0 {
        push("gpu".into());
        let vendor = p.gpu.iter().map(|g| g.vendor.clone()).find(|v| !v.is_empty()).unwrap_or_default();
        if !vendor.is_empty() {
            push(format!("gpu-{vendor}"));
        }
    }
    if p.cpu.threads >= 8 {
        push("cpu-8".into());
    }
    if p.cpu.threads >= 32 {
        push("cpu-32".into());
    }
    let mem = p.mem.total_mb;
    for (mb, tag) in [(8_192, "mem-8g"), (16_384, "mem-16g"), (32_768, "mem-32g"), (131_072, "mem-128g")] {
        if mem >= mb {
            push(tag.into());
        }
    }
    if let Some(d) = p.main_disk() {
        for (gb, tag) in [(100, "disk-100g"), (500, "disk-500g"), (1000, "disk-1t")] {
            if d.free_gb >= gb {
                push(tag.into());
            }
        }
    }
    if !p.services.is_empty() {
        push("service-face".into());
    }
    t
}

/* ================= 落盘 / 读取 ================= */

pub fn default_path() -> PathBuf {
    config::ncc_dir().join(FILE)
}

pub fn save(path: &Path, p: &Profile) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(p)?))
        .with_context(|| format!("写 {} 失败", path.display()))?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Profile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读不到画像 {}（先 `ncc profile node scan`）", path.display()))?;
    let p: Profile = serde_json::from_str(&text)
        .with_context(|| format!("{} 不是合法的算力画像（先看 spec 是不是 {SPEC}）", path.display()))?;
    if p.spec != SPEC {
        bail!("{} 的 spec 是「{}」，本版本只认 {SPEC}", path.display(), p.spec);
    }
    Ok(p)
}

/// 读任意位置：显式 `--file`，否则本地默认路径。
pub fn load_default_or(path: Option<&Path>) -> Result<Profile> {
    load(&path.map(|p| p.to_path_buf()).unwrap_or_else(default_path))
}

/* ================= 上传 / 统计 / 适配（联网部分） ================= */

/// 上传画像到目标（`POST /api/compute/profiles`），返回服务端的回应。
pub fn push_remote(
    cfg: &CliConfig,
    token: &str,
    p: &Profile,
    visibility: &str,
    node_ref: &str,
) -> Result<Value> {
    let body = json!({
        "visibility": visibility,
        "nodeRef": node_ref,
        "profile": p,
    });
    crate::api::post_json(cfg, "/api/compute/profiles", Some(token), &body)
}

/// 平台侧的统计（`GET /api/compute/stats`）。
pub fn stats_remote(cfg: &CliConfig, token: &str, mine: bool) -> Result<Value> {
    let path = if mine { "/api/compute/stats?mine=1" } else { "/api/compute/stats" };
    crate::api::get(cfg, path, Some(token))
}

/// 平台侧的任务适配（`GET /api/compute/fit?need=…`）。
pub fn fit_remote(cfg: &CliConfig, token: &str, need: &str, mine: bool, all: bool) -> Result<Value> {
    let mut path = format!("/api/compute/fit?need={}", crate::api::urlenc(need));
    if mine {
        path.push_str("&mine=1");
    }
    if all {
        // `--strict` 要回答"我的机器差在哪"，那就必须把粗筛没过的也带回来（见平台侧注释）
        path.push_str("&all=1");
    }
    crate::api::get(cfg, &path, Some(token))
}

/// 取一份完整画像（复核用）。
pub fn get_remote(cfg: &CliConfig, token: &str, id: &str) -> Result<Value> {
    crate::api::get(cfg, &format!("/api/compute/profiles/{}", crate::api::urlenc(id)), Some(token))
}

/* ================= 人读渲染 ================= */

/// 画像摘要（`show` / `scan` 共用）。
pub fn render_summary(p: &Profile) -> String {
    let mut s = String::new();
    let who = if p.node.name.is_empty() { "本机".to_string() } else { p.node.name.clone() };
    s.push_str(&format!("{who}（{}）\n", if p.host.arch.is_empty() { "unknown" } else { &p.host.arch }));
    s.push_str(&format!(
        "  系统      {} {}{}\n",
        p.host.os,
        p.host.os_version,
        if p.host.model.is_empty() { String::new() } else { format!(" · {}", p.host.model) }
    ));
    s.push_str(&format!(
        "  CPU       {} 核 / {} 线程{}（可并行 {}）\n",
        p.cpu.cores,
        p.cpu.threads,
        if p.cpu.model.is_empty() { String::new() } else { format!(" · {}", p.cpu.model) },
        p.limits.max_parallel_tasks
    ));
    s.push_str(&format!(
        "  内存      {} MB 总量{}（采集时可用 {} MB）\n",
        p.mem.total_mb,
        if p.mem.total_mb >= 1024 { format!(" · {:.0} GB", p.mem.total_mb as f64 / 1024.0) } else { String::new() },
        p.mem.available_mb
    ));
    if let Some(d) = p.main_disk() {
        s.push_str(&format!("  磁盘      {} 可用 {} GB / {} GB\n", d.path, d.free_gb, d.total_gb));
    }
    if !p.gpu.is_empty() {
        let g: Vec<String> = p
            .gpu
            .iter()
            .map(|g| format!("{} {} {}{}", g.vendor, g.model, if g.mem_mb > 0 { format!("{}MB", g.mem_mb) } else { String::new() }, if g.driver.is_empty() { String::new() } else { format!("（{}）", g.driver) }))
            .collect();
        s.push_str(&format!("  GPU       {}\n", g.join(" · ")));
    } else {
        s.push_str("  GPU       无（推理类任务不在本机跑）\n");
    }
    s.push_str(&format!("  工具链    {} 个可执行\n", p.tools.len()));
    let by_kind = |k: &str| -> Vec<String> {
        p.tools
            .iter()
            .filter(|t| t.kind == k)
            .map(|t| if t.version.is_empty() { t.name.clone() } else { format!("{} {}", t.name, t.version) })
            .collect()
    };
    for (kind, label) in [
        ("compiler", "编译器"),
        ("runtime", "运行时"),
        ("container", "容器"),
        ("data", "数据"),
        ("ai", "AI"),
        ("vcs", "版本控制"),
    ] {
        let list = by_kind(kind);
        if !list.is_empty() {
            s.push_str(&format!("    {label:<6}  {}\n", list.join(" · ")));
        }
    }
    if p.services.is_empty() {
        s.push_str("  服务面    无（本机没有登记任何目标；`ncc --base <url> login` 后重采）\n");
    } else {
        s.push_str(&format!("  服务面    {} 个\n", p.services.len()));
        for sv in &p.services {
            s.push_str(&format!(
                "    {:<12} {} {:<6} 凭据 {} {}\n",
                sv.name,
                sv.base,
                sv.kind,
                sv.auth,
                if sv.capabilities.is_empty() { String::new() } else { format!("[{}]", sv.capabilities.join(" ")) }
            ));
        }
    }
    if !p.tags.is_empty() {
        s.push_str(&format!("  能力标签  {}\n", p.tags.join(" · ")));
    }
    if !p.note.is_empty() {
        s.push_str(&format!("  备注      {}\n", p.note));
    }
    s.push_str(&format!("  采集于    {}\n", p.collected_at));
    s
}

/// 任务适配表（"这台机器能跑什么、缺什么"）。
pub fn render_tasks(p: &Profile) -> String {
    let mut s = String::from("任务执行能力评估（按内置档位；门槛即 `ncc profile node tasks`）\n");
    for t in TASKS {
        let vs = evaluate_task(p, t);
        let ok = vs.iter().all(|v| v.ok);
        let mark = if ok { "✓" } else { "✗" };
        let detail = if ok {
            String::new()
        } else {
            let miss: Vec<String> = vs.iter().filter(|v| !v.ok).map(|v| format!("{} → {}", v.need, v.missing)).collect();
            format!("    缺：{}", miss.join("；"))
        };
        s.push_str(&format!("  {mark} {:<18} {:<22}{detail}\n", t.id, t.zh));
    }
    s
}

/// 给 Agent 的执行流程：**先做什么、后做什么，一条条能照敲**。
///
/// 写成"流程"而不是"一段说明"是有意的：Agent 与脚本读的是同一份东西，
/// 人读也能照着走（`--json` 出去就是同一组命令）。
pub fn flow(has_local: bool) -> Vec<FlowStep> {
    let mut steps = Vec::new();
    if !has_local {
        steps.push(FlowStep {
            step: 1,
            title: "采集本机能力".into(),
            cmd: "ncc profile node scan".into(),
            why: "CPU / 内存 / 磁盘 / 工具链 / 可达服务面，全部本地探测，不上传".into(),
        });
    }
    steps.push(FlowStep {
        step: steps.len() as i64 + 1,
        title: "评估能跑什么".into(),
        cmd: "ncc profile node show".into(),
        why: "按内置档位给出「能跑 / 缺什么」，决定这个任务派不派给本机".into(),
    });
    steps.push(FlowStep {
        step: steps.len() as i64 + 1,
        title: "交给调度方（可选）".into(),
        cmd: "ncc profile node push".into(),
        why: "把画像上传到平台（默认 private），平台据此做任务执行统计与适配；\
              目标没声明 compute 能力时降级成 feedback 留痕".into(),
    });
    steps.push(FlowStep {
        step: steps.len() as i64 + 1,
        title: "找机器（集群/全网）".into(),
        cmd: "ncc profile node fit --task build-rust".into(),
        why: "用同一套需求表达式问平台「谁满足」，拿到候选节点与理由".into(),
    });
    steps.push(FlowStep {
        step: steps.len() as i64 + 1,
        title: "发给别人 / 归档".into(),
        cmd: "ncc profile node pack --out compute.huf".into(),
        why: "画像打成 `.huf` 资源包：可签名、可发布、Agent 可读（给人看的，不执行）".into(),
    });
    steps
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowStep {
    pub step: i64,
    pub title: String,
    pub cmd: String,
    pub why: String,
}

pub fn render_flow(steps: &[FlowStep]) -> String {
    let mut s = String::from("下一步（Agent 与人都能照做）\n");
    for st in steps {
        s.push_str(&format!("  {}. {}  →  {}\n     {}\n", st.step, st.title, st.cmd, st.why));
    }
    s
}

pub fn render_tasks_catalog() -> String {
    let mut s = String::from("内置任务档位（`--task` 的取值；门槛用同一套需求语法）\n");
    for t in TASKS {
        s.push_str(&format!("  {:<18} {:<12} {}\n", t.id, t.zh, t.desc));
        s.push_str(&format!("    {}\n", t.needs.join(" · ")));
    }
    s.push_str("\n也可以自己写需求：`ncc profile node fit --need \"cpu>=8,mem>=32G,tool:docker\"`\n");
    s.push_str("字段：cpu / mem / disk / gpu / gpumem / service；存在性：tool:名 / service:名 / tag:名\n");
    s
}


/* ================= 命令面（`ncc profile node …`） ================= */

/// 算力画像挂在 `ncc profile` 下面：名片说"人是谁"，这里说"机器能干什么"。
///
/// 两者共用入口是有意的 —— Agent 判断一个任务能不能接，要同时回答这两个问题，
/// 不该在两个顶层命令之间来回对照。
#[derive(Subcommand)]
pub enum NodeCmd {
    /// 采集本机算力画像（本地探测，**一个字节都不上传**）
    Scan(ScanArgs),
    /// 展示画像 + 任务执行能力评估 + Agent 下一步流程
    Show(ShowArgs),
    /// 内置任务档位目录（`--task` 的取值来源）
    Tasks {
        #[arg(long)]
        json: bool,
    },
    /// 上传画像到平台（默认 private；目标没声明 compute 能力时降级成 feedback 留痕）
    Push(PushArgs),
    /// 集群 / 全网统计：多少个节点能跑什么（平台 `/api/compute/stats`）
    Fleet(FleetArgs),
    /// 任务适配：谁满足这份需求（平台 `/api/compute/fit`）
    Fit(FitArgs),
    /// 画像打包成 `.huf` 资源包（可签名 / 可发布 / Agent 可读）
    Pack(PackArgs),
}

#[derive(Args, Clone)]
pub struct ScanArgs {
    /// 写到哪儿（默认 `~/.ncc/compute-profile.json`）
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// 本机节点 id（上报用；没有就留空）
    #[arg(long, default_value = "")]
    pub node: String,
    /// 节点名（默认取 hostname）
    #[arg(long, default_value = "")]
    pub name: String,
    /// 节点类型（`ncc registry join --kind` 那一套）
    #[arg(long, default_value = "")]
    pub kind: String,
    /// 区域（内网节点常用，如「上海-内网」）
    #[arg(long, default_value = "")]
    pub region: String,
    /// 额外登记的服务面，可重复：`--service office=http://10.0.0.5:8282`
    #[arg(long = "service", value_name = "NAME=URL")]
    pub services: Vec<String>,
    /// 跳过慢探测（`system_profiler` 这类，秒级）
    #[arg(long)]
    pub fast: bool,
    /// 一句话备注（"内网构建机"这类）
    #[arg(long, default_value = "")]
    pub note: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct ShowArgs {
    /// 读哪一份（默认 `~/.ncc/compute-profile.json`）
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// 只评估一个档位（`ncc profile node tasks` 里的 id）
    #[arg(long, default_value = "")]
    pub task: String,
    /// 用自定义需求评估：`--need "cpu>=8,mem>=32G,tool:docker"`
    #[arg(long, default_value = "")]
    pub need: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct PushArgs {
    /// 上传哪一份（默认 `~/.ncc/compute-profile.json`）
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// 上传前先重新采集一次
    #[arg(long)]
    pub scan: bool,
    /// 可见性：private（默认，只有自己与授权的人看得到）| public（进统计池）
    #[arg(long, default_value = "private", value_parser = ["private", "public"])]
    pub visibility: String,
    /// 关联的节点引用（`ND-…` 或 `@命名空间/节点slug`；留空 = 这台机器）
    #[arg(long, default_value = "")]
    pub node: String,
    /// 怎么送：auto（默认：有 compute 能力走接口，否则降级 feedback）| api | feedback
    #[arg(long, default_value = "auto", value_parser = ["auto", "api", "feedback"])]
    pub via: String,
    /// 备注（随画像一起送）
    #[arg(long, default_value = "")]
    pub note: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct FleetArgs {
    /// 只看我自己上传的那些
    #[arg(long)]
    pub mine: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct FitArgs {
    /// 用内置档位（`ncc profile node tasks` 里的 id）
    #[arg(long, default_value = "")]
    pub task: String,
    /// 或者直接给需求表达式：`--need "cpu>=8,mem>=32G,tool:docker"`
    #[arg(long, default_value = "")]
    pub need: String,
    /// 只看自己上报的画像
    #[arg(long)]
    pub mine: bool,
    /// 拉回候选的**完整画像**在本机复核（服务端只做粗筛，宁可多给）
    #[arg(long)]
    pub strict: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct PackArgs {
    /// 读哪一份（默认 `~/.ncc/compute-profile.json`）
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// 产物路径（默认 `dist/compute-<节点名>-<日期>.huf.gz`）
    #[arg(short = 'o', long)]
    pub out: Option<PathBuf>,
    /// 包 id（默认 `compute/<节点名>`）
    #[arg(long, default_value = "")]
    pub id: String,
    /// 面向谁：agent（默认）| operator | developer | user
    #[arg(long, default_value = "agent", value_parser = ["agent", "operator", "developer", "user"])]
    pub audience: String,
    #[arg(long)]
    pub json: bool,
}

impl NodeCmd {
    /// 这一条要不要服务端能力（与 `cargo` 的能力门禁同一套判定）。
    ///
    /// `scan / show / tasks / pack` 纯本地；`push` 自己决定走接口还是降级 feedback，
    /// 所以不在门禁层拦（拦了降级路径就没了）；`fleet / fit` 由各自实现里 `ensure` 一次
    /// —— 那里能给出"这个目标没有 compute 能力，去哪个目标问"的具体下一步。
    pub fn capability(&self) -> Option<&'static str> {
        None
    }

    /// 这条子命令带 `--json` 吗（决定"提示走 stderr 还是 stdout"，见 `main::wants_json_stdout`）。
    pub fn json_stdout(&self) -> bool {
        match self {
            NodeCmd::Scan(a) => a.json,
            NodeCmd::Show(a) => a.json,
            NodeCmd::Tasks { json } => *json,
            NodeCmd::Push(a) => a.json,
            NodeCmd::Fleet(a) => a.json,
            NodeCmd::Fit(a) => a.json,
            NodeCmd::Pack(a) => a.json,
        }
    }
}

pub fn run(cfg: &CliConfig, cmd: &NodeCmd) -> Result<()> {
    match cmd {
        NodeCmd::Scan(a) => scan_cmd(cfg, a.clone()),
        NodeCmd::Show(a) => show_cmd(a.clone()),
        NodeCmd::Tasks { json } => {
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "spec": SPEC,
                        "tasks": TASKS.iter().map(|t| json!({
                            "id": t.id, "zh": t.zh, "desc": t.desc, "needs": t.needs,
                        })).collect::<Vec<_>>(),
                        "needSyntax": "cpu>=4,mem>=16G,disk>=100G,gpu>=1,gpumem>=8G,tool:docker,service>=1,tag:gpu",
                    }))?
                );
            } else {
                print!("{}", render_tasks_catalog());
            }
            Ok(())
        }
        NodeCmd::Push(a) => push_cmd(cfg, a.clone()),
        NodeCmd::Fleet(a) => fleet_cmd(cfg, a.clone()),
        NodeCmd::Fit(a) => fit_cmd(cfg, a.clone()),
        NodeCmd::Pack(a) => pack_cmd(a.clone()),
    }
}

fn scan_cmd(cfg: &CliConfig, a: ScanArgs) -> Result<()> {
    let o = ScanOpts {
        node_id: a.node.clone(),
        node_name: a.name.clone(),
        node_kind: a.kind.clone(),
        region: a.region.clone(),
        services: a.services.clone(),
        fast: a.fast,
        note: a.note.clone(),
    };
    let p = scan(cfg, &o)?;
    let path = a.out.clone().unwrap_or_else(default_path);
    save(&path, &p)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "file": path, "profile": p }))?);
        return Ok(());
    }
    print!("{}", render_summary(&p));
    println!("  已写入    {}", path.display());
    if p.services.iter().any(|s| s.capabilities.is_empty()) {
        println!(
            "  ⚠ 有的服务面问不到自述（老服务端或没起来）：能力列出来是空的，不影响 base 这件事"
        );
    }
    println!();
    print!("{}", render_tasks(&p));
    println!();
    print!("{}", render_flow(&flow(true)));
    Ok(())
}

fn show_cmd(a: ShowArgs) -> Result<()> {
    let path = a.file.clone().unwrap_or_else(default_path);
    let p = load(&path)?;
    if !a.task.trim().is_empty() && !a.need.trim().is_empty() {
        bail!("--task 与 --need 只能给一个（档位本身就是一组需求）");
    }
    if a.json {
        let mut out = json!({
            "file": path,
            "profile": p,
            "tasks": TASKS.iter().map(|t| json!({
                "id": t.id, "zh": t.zh, "ok": task_ok(&p, t),
                "verdicts": evaluate_task(&p, t).iter().map(|v| json!({
                    "need": v.need, "ok": v.ok, "actual": v.actual, "missing": v.missing,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "flow": flow(true),
        });
        if !a.task.trim().is_empty() {
            let t = task(&a.task).ok_or_else(|| unknown_task(&a.task))?;
            out["task"] = json!({ "id": t.id, "zh": t.zh, "ok": task_ok(&p, t) });
        }
        if !a.need.trim().is_empty() {
            let vs = evaluate(&p, &parse_need(&a.need)?);
            out["need"] = json!({ "expr": a.need, "ok": vs.iter().all(|v| v.ok) });
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    print!("{}", render_summary(&p));
    println!();
    if !a.task.trim().is_empty() {
        let t = task(&a.task).ok_or_else(|| unknown_task(&a.task))?;
        println!("任务 {}（{}）", t.id, t.zh);
        for v in evaluate_task(&p, t) {
            println!(
                "  {} {:<22} 实际 {} {}",
                if v.ok { "✓" } else { "✗" },
                v.need,
                v.actual,
                if v.ok { String::new() } else { format!("→ {}", v.missing) }
            );
        }
        println!();
    } else if !a.need.trim().is_empty() {
        println!("需求 {}", a.need);
        for v in evaluate(&p, &parse_need(&a.need)?) {
            println!(
                "  {} {:<22} 实际 {} {}",
                if v.ok { "✓" } else { "✗" },
                v.need,
                v.actual,
                if v.ok { String::new() } else { format!("→ {}", v.missing) }
            );
        }
        println!();
    } else {
        print!("{}", render_tasks(&p));
        println!();
    }
    print!("{}", render_flow(&flow(true)));
    Ok(())
}

fn unknown_task(id: &str) -> anyhow::Error {
    anyhow!(
        "没有任务档位「{id}」（可用：{}）—— 也可以直接写需求 `--need \"cpu>=8,mem>=32G\"`",
        TASKS.iter().map(|t| t.id).collect::<Vec<_>>().join(" | ")
    )
}

fn push_cmd(cfg: &CliConfig, a: PushArgs) -> Result<()> {
    let p = if a.scan {
        let o = ScanOpts { note: a.note.clone(), ..ScanOpts::default() };
        let p = scan(cfg, &o)?;
        save(&default_path(), &p)?;
        p
    } else {
        load_default_or(a.file.as_deref())?
    };
    let token = require_token(cfg)?;

    // 目标有没有 `compute` 能力，决定走接口还是降级留痕。
    // **不装懂**：能力清单明确写着没有才降级；探测不到（老服务端）按 auto 也降级 ——
    // 老目标上硬发一个它不认识的端点，只会得到一个 404 和一句没人看得懂的错。
    let has_compute = crate::capability::probe(cfg).has("compute");
    let via = match a.via.as_str() {
        "api" => "api",
        "feedback" => "feedback",
        _ => {
            if has_compute == Some(true) {
                "api"
            } else {
                "feedback"
            }
        }
    };
    if via == "api" {
        let v = push_remote(cfg, &token, &p, &a.visibility, &a.node)?;
        let total = v["stats"]["nodes"].as_i64().unwrap_or(0);
        if a.json {
            println!("{}", serde_json::to_string_pretty(&json!({ "via": "api", "response": v }))?);
        } else {
            println!("已上传算力画像（{} · {}）", a.visibility, if a.node.trim().is_empty() { "本机" } else { a.node.trim() });
            println!("  服务端现有画像  {total} 份");
            println!("  看统计          ncc profile node fleet");
            println!("  找人干活        ncc profile node fit --task build-rust");
        }
        return Ok(());
    }
    // 降级：把**摘要**当成一条反馈交上去（老目标 / 内网节点都收得到），
    // 至少让"这台机器是谁、能干什么"留个痕；完整画像仍在本地，随时可 `push --via api` 补上。
    let summary = render_summary(&p);
    let body = format!(
        "算力画像（{}；目标没声明 compute 能力，降级成反馈留痕）\n\n{}",
        SPEC, summary
    );
    // 反馈那边有两条硬规矩（两个服务端都这么判）：`aboutRef` 不能空、tags 上限 8。
    // 画像的"我在说哪台机器"按 节点引用 → 节点 id → 主机名 依次退，别交一条空引用的反馈。
    let about = [a.node.as_str(), p.node.id.as_str(), p.node.name.as_str()]
        .into_iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("本机")
        .to_string();
    let tags: Vec<String> = std::iter::once("compute-profile".to_string())
        .chain(p.tags.iter().cloned())
        .take(8)
        .collect();
    let payload = json!({
        "aboutKind": "node",
        "aboutRef": about,
        "kind": "report",
        "score": 0,
        "body": body,
        "tags": tags,
        "agent": "ncc profile node push",
        "visibility": a.visibility,
    });
    let v = crate::api::post_json(cfg, "/api/feedback", Some(&token), &payload)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "via": "feedback", "response": v }))?);
    } else {
        println!("目标没有声明 compute 能力 → 已按 feedback 上交画像摘要（留痕用）");
        // 两个服务端的反馈创建响应都是 `{feedback: {id: "FB-…"}}`（列表也是这个壳）
        let id = v["feedback"]["id"].as_str().or_else(|| v["id"].as_str()).unwrap_or("");
        println!(
            "  反馈 id        {}",
            if id.is_empty() { "（服务端没回 id，用 `ncc feedback list --mine` 找）" } else { id }
        );
        println!("  完整画像       仍在本地 {}", default_path().display());
        println!("  想进统计池     换一个有 compute 能力的目标再 `ncc profile node push`");
    }
    Ok(())
}

fn fleet_cmd(cfg: &CliConfig, a: FleetArgs) -> Result<()> {
    let token = require_token(cfg)?;
    crate::capability::ensure(cfg, "compute")?;
    let v = stats_remote(cfg, &token, a.mine)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let s = &v["stats"];
    println!(
        "算力画像统计（{}）",
        if a.mine { "只看我上传的" } else { "我可见的全部" }
    );
    println!("  画像份数  {}", s["nodes"].as_i64().unwrap_or(0));
    if let Some(t) = s["updatedAt"].as_str().filter(|t| !t.is_empty()) {
        println!("  最近更新  {t}");
    }
    let hw = &s["hardware"];
    if hw.is_object() {
        println!(
            "  CPU 合计  {} 线程 · 中位 {} · p90 {}",
            hw["cpuThreads"].as_i64().unwrap_or(0),
            hw["cpuThreadsP50"].as_i64().unwrap_or(0),
            hw["cpuThreadsP90"].as_i64().unwrap_or(0)
        );
        println!(
            "  内存合计  {:.0} GB · 中位 {:.0} GB · 最大 {:.0} GB",
            hw["memMb"].as_i64().unwrap_or(0) as f64 / 1024.0,
            hw["memMbP50"].as_i64().unwrap_or(0) as f64 / 1024.0,
            hw["memMbMax"].as_i64().unwrap_or(0) as f64 / 1024.0
        );
        println!(
            "  磁盘可用  合计 {} GB · 中位 {} GB",
            hw["diskFreeGb"].as_i64().unwrap_or(0),
            hw["diskFreeGbP50"].as_i64().unwrap_or(0)
        );
        println!("  GPU 节点  {}", hw["gpuNodes"].as_i64().unwrap_or(0));
    }
    if let Some(tags) = s["tags"].as_array().filter(|a| !a.is_empty()) {
        println!("  能力标签");
        for t in tags {
            println!("    {:<14} {} 个节点", t["tag"].as_str().unwrap_or(""), t["nodes"].as_i64().unwrap_or(0));
        }
    }
    if let Some(tools) = s["tools"].as_array().filter(|a| !a.is_empty()) {
        println!(
            "  工具覆盖  {}",
            tools
                .iter()
                .map(|t| format!("{}×{}", t["name"].as_str().unwrap_or(""), t["nodes"].as_i64().unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    println!("  任务适配  ncc profile node fit --task build-rust");
    Ok(())
}

fn fit_cmd(cfg: &CliConfig, a: FitArgs) -> Result<()> {
    let expr = need_expr(&a.task, &a.need)?;
    let needs = parse_need(&expr)?;
    let token = require_token(cfg)?;
    crate::capability::ensure(cfg, "compute")?;
    let mut v = fit_remote(cfg, &token, &expr, a.mine, a.strict)?;

    // **服务端只粗筛**（宁可多给一个），判定权在本机：`--strict` 把候选的完整画像拉回来
    // 用同一套 `evaluate` 复核，并给出"缺什么"。不拉回来也能用 —— 但那样看到的是服务端的
    // 索引列，不是判定。
    if a.strict {
        let nodes = v["nodes"].as_array().cloned().unwrap_or_default();
        let mut kept: Vec<Value> = Vec::new();
        for mut n in nodes {
            let id = n["id"].as_str().unwrap_or("").to_string();
            let got = get_remote(cfg, &token, &id)?;
            let profile: Profile = match serde_json::from_value(got["facts"].clone()) {
                Ok(p) => p,
                Err(e) => {
                    v["strictNote"] = json!(format!("{id} 的画像读不出来，未复核：{e}"));
                    kept.push(n);
                    continue;
                }
            };
            let verdicts = evaluate(&profile, &needs);
            n["verdicts"] = json!(verdicts
                .iter()
                .map(|x| json!({"need": x.need, "ok": x.ok, "actual": x.actual, "missing": x.missing}))
                .collect::<Vec<_>>());
            // 不满足的候选**也留下**（标 ok=false 并带"缺什么"）：调度最想知道的往往不是
            // "谁行"，而是"我的机器差在哪"，把它们静默丢掉等于把答案扔了。
            kept.push(n);
        }
        let matched = kept
            .iter()
            .filter(|n| {
                n["verdicts"]
                    .as_array()
                    .map(|vs| vs.iter().all(|x| x["ok"].as_bool().unwrap_or(false)))
                    .unwrap_or(false)
            })
            .count();
        let total_listed = kept.len();
        v["nodes"] = json!(kept);
        v["matched"] = json!(matched);
        v["listed"] = json!(total_listed);
        v["strict"] = json!(true);
    }

    if a.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("任务适配（需求 {}）", expr);
    if !a.task.trim().is_empty() {
        if let Some(t) = task(&a.task) {
            println!("  档位      {}（{}）", t.id, t.zh);
        }
    }
    let total = v["total"].as_i64().unwrap_or(0);
    let matched = v["matched"].as_i64().unwrap_or(0);
    let pct = if total > 0 { matched as f64 * 100.0 / total as f64 } else { 0.0 };
    println!(
        "  可跑      {matched} / {total}（{pct:.0}%）{}",
        if a.strict { "（已在本机复核）" } else { "（服务端粗筛，加 --strict 复核）" }
    );
    for n in v["nodes"].as_array().cloned().unwrap_or_default() {
        let node_ref = n["nodeRef"].as_str().unwrap_or("");
        let verdicts = n["verdicts"].as_array().cloned().unwrap_or_default();
        let ok = !verdicts.is_empty() && verdicts.iter().all(|x| x["ok"].as_bool().unwrap_or(false));
        println!(
            "  {} {:<14} {:<24} 采集 {}",
            if ok { "✓" } else { "✗" },
            if node_ref.is_empty() { "（本机）" } else { node_ref },
            n["name"].as_str().unwrap_or(""),
            n["updatedAt"].as_str().unwrap_or("")
        );
        for x in verdicts {
            if !x["ok"].as_bool().unwrap_or(true) {
                println!(
                    "        缺 {}（现在 {}）",
                    x["need"].as_str().unwrap_or(""),
                    x["actual"].as_str().unwrap_or("")
                );
            }
        }
    }
    if matched == 0 {
        println!("  没有人满足：把需求放宽，或先让有能力的机器 `ncc profile node push`");
    }
    Ok(())
}

/// `--task` / `--need` → 一条需求表达式（档位在本地展开，平台只认表达式）。
pub fn need_expr(task_id: &str, need: &str) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    if !task_id.trim().is_empty() {
        let t = task(task_id).ok_or_else(|| unknown_task(task_id))?;
        parts.extend(t.needs.iter().map(|s| s.to_string()));
    }
    if !need.trim().is_empty() {
        parts.extend(parse_need(need)?.iter().map(|n| n.raw.clone()));
    }
    if parts.is_empty() {
        bail!("要么给 --task <档位>，要么给 --need <表达式>（档位见 `ncc profile node tasks`）");
    }
    Ok(parts.join(","))
}

fn pack_cmd(a: PackArgs) -> Result<()> {
    let p = load_default_or(a.file.as_deref())?;
    let slug = if p.node.name.trim().is_empty() {
        "local".to_string()
    } else {
        p.node.name.trim().to_string()
    };
    let id = if a.id.trim().is_empty() {
        format!("compute/{}", slug.replace(' ', "-").to_lowercase())
    } else {
        a.id.trim().to_string()
    };
    let out = a.out.clone().unwrap_or_else(|| {
        let day: String = p.collected_at.chars().take(10).collect();
        PathBuf::from("dist").join(format!(
            "compute-{}-{}.huf.gz",
            slug.replace(' ', "_"),
            if day.is_empty() { "unknown".to_string() } else { day }
        ))
    });

    // 画像打进 `.huf`（**资源包**：给人看，不执行）—— 于是它天然可签名、可发布、
    // 可被 Agent 用 `ncc huf inspect` 读。这一点是有意的：画像是一种"资料"，
    // 不是"程序"，用可执行的 `.hur` 装它反而要求它假装有入口。
    let dir = std::env::temp_dir().join(format!("ncc-compute-pack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("docs"))?;
    std::fs::create_dir_all(dir.join("data"))?;
    let manifest = hur_core::huf::HufManifest {
        spec: hur_core::spec::HUF_SPEC.to_string(),
        kind: hur_core::spec::HUF_KIND.to_string(),
        id: id.clone(),
        name: format!("算力画像 · {slug}"),
        version: "1.0.0".to_string(),
        short: format!(
            "{} 线程 / {} MB 内存 / {} 个工具",
            p.cpu.threads,
            p.mem.total_mb,
            p.tools.len()
        ),
        summary: format!(
            "由 `ncc profile node scan` 采集（{}）。给调度方或 Agent 判断这台机器能干哪些活。",
            SPEC
        ),
        audience: a.audience.clone(),
        tags: p.tags.iter().take(8).cloned().collect(),
        license: String::new(),
        deps: Default::default(),
        publish: Default::default(),
    };
    std::fs::write(
        dir.join(hur_core::spec::HUF_MANIFEST),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    std::fs::write(dir.join("data/compute-profile.json"), format!("{}\n", serde_json::to_string_pretty(&p)?))?;
    std::fs::write(dir.join("docs/README.md"), format!("{}\n", render_summary(&p)))?;

    // 校验不过就不产（与 `ncc huf pack` 同一条规矩：发出去的每个字节都该是干净的）
    let issues = hur_core::huf::validate(&manifest, &dir, None);
    let errs = issues.iter().filter(|i| i.level == hur_core::spec::Level::Error).count();
    if errs > 0 {
        for i in &issues {
            if i.level == hur_core::spec::Level::Error {
                println!("  [{} 错误] {}", i.rule, i.msg);
            }
        }
        bail!("画像包校验不过（{errs} 个错误）");
    }
    hur_core::huf::build_lock(&dir)?;
    let packed = hur_core::huf::pack(&dir)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&packed.file, &out).with_context(|| format!("写 {} 失败", out.display()))?;
    let _ = std::fs::remove_dir_all(&dir);

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": out, "id": id, "sha256": packed.sha256, "entries": packed.entries,
            }))?
        );
        return Ok(());
    }
    println!("已打出画像包 {}", out.display());
    println!("  id        {id}（audience {}）", a.audience);
    println!("  sha256    {}", packed.sha256);
    println!("  发给别人  ncc huf sign {} && ncc publish --kind huf --name 算力画像 --file {}", out.display(), out.display());
    println!("  自己看看  ncc huf inspect {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Profile {
        let mut p = Profile::default();
        p.node = NodeRef { id: "ND-1".into(), name: "build-box".into(), kind: "agent".into(), region: "上海".into() };
        p.cpu = Cpu { model: "M3 Pro".into(), cores: 12, threads: 12, load1: 0.5 };
        p.mem = Mem { total_mb: 36864, available_mb: 20000 };
        p.disk = vec![Disk { path: "/".into(), total_gb: 994, free_gb: 210 }];
        p.tools = vec![
            Tool { name: "cargo".into(), version: "1.98.0".into(), path: "/usr/bin/cargo".into(), kind: "compiler".into() },
            Tool { name: "node".into(), version: "22.0.0".into(), path: "/usr/bin/node".into(), kind: "runtime".into() },
        ];
        p.services = vec![Service {
            name: "office".into(),
            base: "http://10.0.0.5:8282".into(),
            product: "ncc-registry".into(),
            kind: "node".into(),
            capabilities: vec!["registry".into(), "kb".into()],
            auth: "session".into(),
            how_to: String::new(),
        }];
        p.tags = derive_tags(&p);
        p
    }

    #[test]
    fn 需求解析与单位换算() {
        let ns = parse_need("cpu>=4, mem>=16G, disk>=100G").unwrap();
        assert_eq!(ns.len(), 3);
        assert_eq!(ns[0].field, "cpu");
        assert_eq!(ns[0].value, 4.0);
        // 内存按 MB：16G = 16384
        assert_eq!(ns[1].value, 16384.0);
        // 磁盘按 GB：100G = 100
        assert_eq!(ns[2].value, 100.0);
        assert_eq!(parse_need("mem>=512M").unwrap()[0].value, 512.0);
        assert_eq!(parse_need("mem>=1T").unwrap()[0].value, 1024.0 * 1024.0);
    }

    #[test]
    fn 需求写错当场报错不猜() {
        assert!(parse_need("cpu").is_err());
        assert!(parse_need("cores>=4").is_err());
        assert!(parse_need("tool:").is_err());
        assert!(parse_need("  ").is_err());
    }

    #[test]
    fn 工具二选一竖线() {
        let p = sample();
        let n = parse_need("tool:docker|podman").unwrap();
        let v = evaluate(&p, &n);
        assert!(!v[0].ok, "没装 docker/podman 时应当不满足");
        let p2 = {
            let mut p = sample();
            p.tools.push(Tool { name: "podman".into(), version: "5.0".into(), path: String::new(), kind: "container".into() });
            p
        };
        assert!(evaluate(&p2, &n)[0].ok, "装了 podman 就该满足");
    }

    #[test]
    fn 任务档位评估给出缺什么() {
        let p = sample();
        assert!(task_ok(&p, task("build-rust").unwrap()), "12 核 36G 有 cargo：能构建 Rust");
        assert!(!task_ok(&p, task("build-node").unwrap()), "只有 node 没有 npm：构建 Node 项目不该判为可跑");
        let vs = evaluate_task(&p, task("inference-gpu").unwrap());
        let miss: Vec<&str> = vs.iter().filter(|v| !v.ok).map(|v| v.need.as_str()).collect();
        assert!(miss.contains(&"gpu>=1"), "{miss:?}");
    }

    #[test]
    fn 标签是推导出来的() {
        let p = sample();
        for t in ["build-rust", "run-node", "mem-32g", "disk-100g", "service-face", "cpu-8"] {
            assert!(p.tags.iter().any(|x| x == t), "缺标签 {t}: {:?}", p.tags);
        }
        assert!(!p.tags.iter().any(|x| x == "gpu"), "没有 GPU 不该有 gpu 标签");
        assert!(!p.tags.iter().any(|x| x == "container"), "没装容器工具不该有 container 标签");
    }

    #[test]
    fn 画像往返与规范号校验() {
        let d = std::env::temp_dir().join(format!("ncc-compute-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(FILE);
        let p = sample();
        save(&f, &p).unwrap();
        let back = load(&f).unwrap();
        assert_eq!(back.spec, SPEC);
        assert_eq!(back.cpu.threads, 12);
        assert_eq!(back.tools.len(), 2);

        // 规范号不对：拒绝（别让两个版本的形状混进同一张表）
        let mut bad = serde_json::to_value(&p).unwrap();
        bad["spec"] = json!("ncc-compute-profile/v0");
        std::fs::write(&f, bad.to_string()).unwrap();
        assert!(load(&f).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 采集表是自洽的() {
        // 采集口径写在一张静态表里：名字不能空、分类不能空、实参只能是 --version 这类短参
        // （探测带 1.5s 超时，写个会启动服务的参数进来会让 scan 变得不可预期）
        for (name, args, kind) in TOOL_PROBES {
            assert!(!name.trim().is_empty(), "有名字是空的");
            assert!(!kind.trim().is_empty(), "{name} 没有分类");
            assert!(args.len() <= 3, "{name} 的实参太多：{args:?}");
        }
        // 同一张表里不该有重名（重名会让画像里出现两个同名工具，评估时谁先谁后说不清）
        let mut names: Vec<&str> = TOOL_PROBES.iter().map(|(n, _, _)| *n).collect();
        names.sort_unstable();
        let n0 = names.len();
        names.dedup();
        assert_eq!(n0, names.len(), "采集表里有重名");
    }

    #[test]
    fn 版本号归一化() {
        assert_eq!(normalize_version("cargo 1.98.0 (abc 2026-01-01)"), "1.98.0");
        assert_eq!(normalize_version("v20.11.0"), "20.11.0");
        assert_eq!(normalize_version("Python 3.12.4"), "3.12.4");
        // 这两条是真机上踩出来的：`go version go1.24.1 …` 抠不出 `go`，日期也不能当版本
        assert_eq!(normalize_version("go version go1.24.1 darwin/arm64"), "1.24.1");
        assert_eq!(normalize_version("openjdk version \"17.0.11\" 2026-08-18"), "17.0.11");
        assert_eq!(normalize_version(""), "");
    }

    #[test]
    fn 显存单位与内存同口径() {
        // gpumem 也要按 MB 换算：写成 GB 的倍数会被误判成 8MB
        assert_eq!(parse_need("gpumem>=8G").unwrap()[0].value, 8192.0);
        let p = sample();
        let v = evaluate(&p, &parse_need("gpumem>=8G").unwrap());
        assert!(!v[0].ok);
        assert!(v[0].missing.contains("8192MB"), "报错要说清差多少：{}", v[0].missing);
    }

    #[test]
    fn 服务面需求判定() {
        let p = sample();
        assert!(evaluate(&p, &parse_need("service:office").unwrap())[0].ok);
        assert!(!evaluate(&p, &parse_need("service:nowhere").unwrap())[0].ok);
        assert!(evaluate(&p, &parse_need("service>=1").unwrap())[0].ok);
    }
}
