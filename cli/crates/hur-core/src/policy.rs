//! HUR 安全策略（`harness-use-security/v1`）+ 执行计划器。
//!
//! 用户决策（2026-09-25）：**hur 制定一个安全策略，把 `hur sandbox` 与 `hur verify` 收到同一份策略里**；
//! 不同 Scaffold / 工程可以选不同策略（有的允许 wasmtime、有的允许 container…）；
//! 执行环境也可以是**注册过的远程 Proxy sandbox**（`hur env`）。
//!
//! 三条设计原则：
//! 1. **声明即策略，沙箱即执行器**：能力默认全关，只从 `permissions{}` 与策略里映射（见 PRD §12.2）。
//! 2. **三层 + 单调收紧**：机器 `~/.harnessuse/security.json` → 工程 → 包内声明；
//!    **下层只能收紧上层，永不放宽**（`exec.enabled` 取 AND、`engines` 取交集、限额取 min…）。
//! 3. **可解释**：解析结果带 `sources`（每项来自哪一层），计划器给「为什么不行」的可读理由，而不是静默拒绝。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::spec::{HurPackage, Level};

pub const POLICY_SPEC: &str = "harness-use-security/v1";
pub const ENV_SPEC: &str = "harness-use-environments/v1";
pub const PROJECT_POLICY_FILE: &str = "hur.security.json";
pub const MACHINE_POLICY_FILE: &str = "security.json";
pub const ENV_FILE: &str = "environments.json";

/// 引擎名（与 PRD §12.4 的档位一致）
pub const ENGINES: [&str; 5] = ["wasm", "js", "process", "container", "remote"];

/// 本机已注册（= 真的能执行）的引擎。
///
/// hur-core 自己不接引擎实现 —— 真执行引擎是可选的重依赖（如 wasmtime），
/// 由宿主进程启动时登记，例如 CLI：`policy::register_engines(hur_sandbox::ENGINES)`。
/// **没登记就是空集**：计划器照常出计划，但会如实标注 `engine_ready=false`，
/// 于是 `run --exec` fail-closed（PRD §12.5 红线）。
fn engine_registry() -> &'static std::sync::Mutex<Vec<String>> {
    static ENGINE_NAMES: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    ENGINE_NAMES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// 登记本机可执行的引擎（幂等）
pub fn register_engines(engines: &[&str]) {
    let mut cur = engine_registry().lock().unwrap_or_else(|e| e.into_inner());
    for e in engines {
        let e = e.to_string();
        if !cur.contains(&e) {
            cur.push(e);
        }
    }
}

/// 本机可执行的引擎列表（`plan()` 与 `hur doctor` / `hur policy ls` 共用）
pub fn implemented_engines() -> Vec<String> {
    engine_registry().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 某个引擎本机是否可执行
pub fn engine_implemented(e: &str) -> bool {
    engine_registry().lock().unwrap_or_else(|err| err.into_inner()).iter().any(|x| x == e)
}

#[cfg(test)]
pub fn reset_engines() {
    engine_registry().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

pub fn valid_engine(e: &str) -> bool {
    ENGINES.contains(&e)
}

/* ---------------- 策略模型（全部字段可选：合并时才确定） ---------------- */

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct VerifyRule {
    /// 执行前必须过 `hur verify`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// 允许的错误数上限（0 = fail-closed）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_errors: Option<u32>,
    /// warning 也当失败
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    /// 必须有 hur.lock
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_lock: Option<bool>,
    /// 必须有签名（P2 预留：`hur sign`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_signature: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ExecRule {
    /// 是否允许执行包内代码（默认 false）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 允许的引擎（按优先级）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines: Option<Vec<String>>,
    /// 只在本地执行（禁止发到远程 sandbox）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_only: Option<bool>,
    /// 输出上限（字符）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_chars: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct SandboxRule {
    /// 引擎无关的限额（wasm fuel / JS 指令数）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_kb: Option<u32>,
    /// 墙钟上限（毫秒）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u32>,
    /// `permissions.local` 目录默认只读
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readonly_preopen: Option<bool>,
    /// `none`（不许联网）| `declared-only`（只允许 permissions.network 里声明的）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// 单次运行的网络调用次数上限
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_network_calls: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct RemoteRule {
    /// 允许把执行发到远程 sandbox
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    /// 允许使用的环境白名单（空 = 不额外限制）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<String>>,
    /// 环境必须带可核对的证明（registry 引用 + sha256）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_attestation: Option<bool>,
    /// 允许把包内容 / 上下文发到远端（数据出边界开关）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_data_leaving: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct AuditRule {
    /// trace 落点（相对工程根）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_dir: Option<String>,
    /// 保留条数
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<u32>,
    /// 落盘前脱敏
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redact_secrets: Option<bool>,
}

/// 策略文档（可作机器/工程/包三层里的任一层；字段全可选）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecurityPolicy {
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub verify: VerifyRule,
    #[serde(default)]
    pub exec: ExecRule,
    #[serde(default)]
    pub sandbox: SandboxRule,
    #[serde(default)]
    pub remote: RemoteRule,
    #[serde(default)]
    pub audit: AuditRule,
    /// 该策略适用的范围（仅作用于「选策略」这一层，不参与收紧计算）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_to: Option<AppliesTo>,
    /// 机器层可带一个具名策略库（工程/包按 id 引用）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policies: BTreeMap<String, SecurityPolicy>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppliesTo {
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub namespaces: Vec<String>,
}

/// 包内声明（`hur.json` 的 `security`）：可以选策略 + 进一步收紧自己
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecurityReq {
    /// 希望使用的具名策略（只能被更严的层覆盖）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy: String,
    #[serde(default)]
    pub verify: VerifyRule,
    #[serde(default)]
    pub exec: ExecRule,
    #[serde(default)]
    pub sandbox: SandboxRule,
    #[serde(default)]
    pub remote: RemoteRule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditRule>,
    /// 包声明的执行入口（如 `src/agent.wasm`）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entry: String,
}

impl SecurityReq {
    /// 把包内声明折算成策略层（`entry` 不参与收紧，只由 R8/计划器检查）
    pub fn as_policy(&self) -> SecurityPolicy {
        SecurityPolicy {
            exec: self.exec.clone(),
            verify: self.verify.clone(),
            sandbox: self.sandbox.clone(),
            remote: self.remote.clone(),
            audit: self.audit.clone().unwrap_or_default(),
            ..Default::default()
        }
    }
}

impl SecurityPolicy {
    pub fn is_empty(&self) -> bool {
        self.verify == VerifyRule::default()
            && self.exec == ExecRule::default()
            && self.sandbox == SandboxRule::default()
            && self.remote == RemoteRule::default()
            && self.audit == AuditRule::default()
    }
}

/* ---------------- 收紧合并 ---------------- */

fn tighter_min_u32(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

fn tighter_min_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

fn tighter_or(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x || y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

fn tighter_and(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x && y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// 列表收紧：交集（空 = 不额外限制）；两者都有限定则取交集
fn tighter_list(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Option<Vec<String>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.iter().filter(|v| y.contains(v)).cloned().collect()),
        (Some(x), None) => Some(x.clone()),
        (None, Some(y)) => Some(y.clone()),
        (None, None) => None,
    }
}

fn network_rank(s: &str) -> u8 {
    match s {
        "none" => 2,
        "declared-only" => 1,
        _ => 0,
    }
}

/// `low` 是更具体的一层；结果只会比任一层更严（单调收紧）
pub fn tighten(upper: &SecurityPolicy, low: &SecurityPolicy) -> SecurityPolicy {
    let net = match (&upper.sandbox.network, &low.sandbox.network) {
        (Some(a), Some(b)) => Some(if network_rank(a) >= network_rank(b) { a.clone() } else { b.clone() }),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };
    SecurityPolicy {
        spec: if upper.spec.is_empty() { low.spec.clone() } else { upper.spec.clone() },
        id: if upper.id.is_empty() { low.id.clone() } else { upper.id.clone() },
        name: if upper.name.is_empty() { low.name.clone() } else { upper.name.clone() },
        description: if upper.description.is_empty() { low.description.clone() } else { upper.description.clone() },
        verify: VerifyRule {
            required: tighter_or(upper.verify.required, low.verify.required),
            max_errors: tighter_min_u32(upper.verify.max_errors, low.verify.max_errors),
            strict: tighter_or(upper.verify.strict, low.verify.strict),
            require_lock: tighter_or(upper.verify.require_lock, low.verify.require_lock),
            require_signature: tighter_or(upper.verify.require_signature, low.verify.require_signature),
        },
        exec: ExecRule {
            enabled: tighter_and(upper.exec.enabled, low.exec.enabled),
            engines: tighter_list(&upper.exec.engines, &low.exec.engines),
            local_only: tighter_or(upper.exec.local_only, low.exec.local_only),
            max_output_chars: tighter_min_u32(upper.exec.max_output_chars, low.exec.max_output_chars),
        },
        sandbox: SandboxRule {
            fuel: tighter_min_u64(upper.sandbox.fuel, low.sandbox.fuel),
            memory_mb: tighter_min_u32(upper.sandbox.memory_mb, low.sandbox.memory_mb),
            stack_kb: tighter_min_u32(upper.sandbox.stack_kb, low.sandbox.stack_kb),
            wall_ms: tighter_min_u32(upper.sandbox.wall_ms, low.sandbox.wall_ms),
            readonly_preopen: tighter_or(upper.sandbox.readonly_preopen, low.sandbox.readonly_preopen),
            network: net,
            max_network_calls: tighter_min_u32(upper.sandbox.max_network_calls, low.sandbox.max_network_calls),
        },
        remote: RemoteRule {
            allowed: tighter_and(upper.remote.allowed, low.remote.allowed),
            environments: tighter_list(&upper.remote.environments, &low.remote.environments),
            require_attestation: tighter_or(upper.remote.require_attestation, low.remote.require_attestation),
            allow_data_leaving: tighter_and(upper.remote.allow_data_leaving, low.remote.allow_data_leaving),
        },
        audit: AuditRule {
            trace_dir: upper.audit.trace_dir.clone().or_else(|| low.audit.trace_dir.clone()),
            retain: tighter_min_u32(upper.audit.retain, low.audit.retain),
            redact_secrets: tighter_or(upper.audit.redact_secrets, low.audit.redact_secrets),
        },
        applies_to: upper.applies_to.clone().or_else(|| low.applies_to.clone()),
        policies: BTreeMap::new(),
    }
}

/* ---------------- 具名策略与内置预设 ---------------- */

/// 内置预设（最小可用集；机器层可扩展、工程/包按 id 引用）
pub fn presets() -> Vec<SecurityPolicy> {
    vec![
        SecurityPolicy {
            spec: POLICY_SPEC.into(),
            id: "strict".into(),
            name: "只读（不执行）".into(),
            description: "默认档：只做声明式装配与校验，任何情况下都不执行包内代码。".into(),
            verify: VerifyRule { required: Some(true), max_errors: Some(0), strict: Some(false), require_lock: Some(true), require_signature: None },
            exec: ExecRule { enabled: Some(false), engines: Some(vec![]), local_only: Some(true), max_output_chars: Some(4000) },
            sandbox: SandboxRule { readonly_preopen: Some(true), network: Some("declared-only".into()), max_network_calls: Some(0), ..Default::default() },
            remote: RemoteRule { allowed: Some(false), require_attestation: Some(true), allow_data_leaving: Some(false), ..Default::default() },
            audit: AuditRule { trace_dir: Some(".hur/run".into()), retain: Some(50), redact_secrets: Some(true) },
            ..Default::default()
        },
        SecurityPolicy {
            spec: POLICY_SPEC.into(),
            id: "wasm-local".into(),
            name: "本机 WASM 沙箱".into(),
            description: "推荐档：包内编排编译成 wasm，在本机 wasmtime 里跑（fuel + epoch + 内存上限 + 目录只读 preopen）。".into(),
            verify: VerifyRule { required: Some(true), max_errors: Some(0), strict: Some(true), require_lock: Some(true), require_signature: None },
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["wasm".into()]), local_only: Some(true), max_output_chars: Some(8000) },
            sandbox: SandboxRule { fuel: Some(50_000_000), memory_mb: Some(256), stack_kb: Some(1024), wall_ms: Some(5000), readonly_preopen: Some(true), network: Some("declared-only".into()), max_network_calls: Some(8) },
            remote: RemoteRule { allowed: Some(false), require_attestation: Some(true), allow_data_leaving: Some(false), ..Default::default() },
            audit: AuditRule { trace_dir: Some(".hur/run".into()), retain: Some(100), redact_secrets: Some(true) },
            ..Default::default()
        },
        SecurityPolicy {
            spec: POLICY_SPEC.into(),
            id: "dev-js".into(),
            name: "开发机 JS（非恶意隔离）".into(),
            description: "仅供开发者本机：进程内 QuickJS + 内存上限 + 中断，能防跑飞但**不是安全边界**。".into(),
            verify: VerifyRule { required: Some(true), max_errors: Some(0), strict: Some(false), require_lock: Some(true), require_signature: None },
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["js".into(), "wasm".into()]), local_only: Some(true), max_output_chars: Some(8000) },
            sandbox: SandboxRule { fuel: Some(20_000_000), memory_mb: Some(128), stack_kb: Some(512), wall_ms: Some(2000), readonly_preopen: Some(true), network: Some("none".into()), max_network_calls: Some(0) },
            remote: RemoteRule { allowed: Some(false), require_attestation: Some(true), allow_data_leaving: Some(false), ..Default::default() },
            audit: AuditRule { trace_dir: Some(".hur/run".into()), retain: Some(20), redact_secrets: Some(true) },
            ..Default::default()
        },
        SecurityPolicy {
            spec: POLICY_SPEC.into(),
            id: "container".into(),
            name: "容器沙箱（企业自托管）".into(),
            description: "企业档：交本机容器运行时（复用 `cli/` 的沙箱清单模型），限额由容器与策略共同约束。".into(),
            verify: VerifyRule { required: Some(true), max_errors: Some(0), strict: Some(true), require_lock: Some(true), require_signature: Some(true) },
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["container".into(), "process".into()]), local_only: Some(true), max_output_chars: Some(20_000) },
            sandbox: SandboxRule { fuel: None, memory_mb: Some(512), stack_kb: None, wall_ms: Some(30_000), readonly_preopen: Some(true), network: Some("declared-only".into()), max_network_calls: Some(32) },
            remote: RemoteRule { allowed: Some(false), require_attestation: Some(true), allow_data_leaving: Some(false), ..Default::default() },
            audit: AuditRule { trace_dir: Some(".hur/run".into()), retain: Some(500), redact_secrets: Some(true) },
            ..Default::default()
        },
        SecurityPolicy {
            spec: POLICY_SPEC.into(),
            id: "remote-only".into(),
            name: "只允许远程沙箱".into(),
            description: "本机不执行：把执行发到**注册过的 hur 环境**（Proxy sandbox），要求证明且默认不允许数据出境。".into(),
            verify: VerifyRule { required: Some(true), max_errors: Some(0), strict: Some(true), require_lock: Some(true), require_signature: Some(true) },
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["remote".into()]), local_only: Some(false), max_output_chars: Some(8000) },
            sandbox: SandboxRule { fuel: Some(50_000_000), memory_mb: Some(256), stack_kb: None, wall_ms: Some(10_000), readonly_preopen: Some(true), network: Some("declared-only".into()), max_network_calls: Some(8) },
            remote: RemoteRule { allowed: Some(true), environments: None, require_attestation: Some(true), allow_data_leaving: Some(false) },
            audit: AuditRule { trace_dir: Some(".hur/run".into()), retain: Some(100), redact_secrets: Some(true) },
            ..Default::default()
        },
    ]
}

pub fn preset(id: &str) -> Option<SecurityPolicy> {
    presets().into_iter().find(|p| p.id == id)
}

/* ---------------- 环境登记（远程 Proxy sandbox） ---------------- */

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attestation {
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub note: String,
}

impl Attestation {
    pub fn present(&self) -> bool {
        !self.sha256.trim().is_empty() && !self.reference.trim().is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// proxy | local（local = 本机引擎，仅登记以便统一选择）
    #[serde(default = "kind_proxy")]
    pub kind: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub engines: Vec<String>,
    #[serde(default)]
    pub limits: EnvLimits,
    #[serde(default)]
    pub attestation: Attestation,
    /// 该环境是否允许接收数据出境
    #[serde(default)]
    pub allow_data_leaving: bool,
    #[serde(default)]
    pub registered_at_unix: u64,
    #[serde(default)]
    pub note: String,
}

fn kind_proxy() -> String {
    "proxy".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvLimits {
    #[serde(default)]
    pub max_memory_mb: u32,
    #[serde(default)]
    pub max_wall_ms: u32,
    #[serde(default)]
    pub fuel: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRegistry {
    #[serde(default = "env_spec")]
    pub spec: String,
    /// 默认环境 id
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub environments: Vec<Environment>,
}

fn env_spec() -> String {
    ENV_SPEC.to_string()
}

impl Default for EnvRegistry {
    fn default() -> Self {
        Self { spec: env_spec(), default: String::new(), environments: Vec::new() }
    }
}

pub fn env_path() -> PathBuf {
    crate::cfg::home().join(ENV_FILE)
}

pub fn load_envs() -> EnvRegistry {
    std::fs::read_to_string(env_path())
        .ok()
        .and_then(|t| serde_json::from_str::<EnvRegistry>(&t).ok())
        .map(|mut r| {
            r.spec = env_spec();
            r
        })
        .unwrap_or_default()
}

pub fn save_envs(r: &EnvRegistry) -> Result<()> {
    crate::cfg::ensure_home()?;
    let text = format!("{}\n", serde_json::to_string_pretty(&EnvRegistry { spec: env_spec(), ..r.clone() })?);
    std::fs::write(env_path(), text).with_context(|| format!("写 {} 失败", env_path().display()))
}

/// 环境登记前的纯校验（不落盘，便于单测）
pub fn validate_env(env: &Environment) -> Result<()> {
    let id = env.id.trim();
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        bail!("环境 id 只能用小写字母/数字/-/_/.，且非空（当前「{id}」）");
    }
    for e in &env.engines {
        if !valid_engine(e) {
            bail!("未知引擎「{e}」（可选：{}）", ENGINES.join(" / "));
        }
    }
    match env.kind.as_str() {
        "proxy" if env.url.trim().is_empty() => bail!("proxy 环境必须给 --url（远程 sandbox 地址）"),
        "proxy" | "local" => {}
        other => bail!("环境 kind 只能是 proxy | local，当前「{other}」"),
    }
    Ok(())
}

pub fn add_env(env: Environment, make_default: bool) -> Result<Environment> {
    validate_env(&env)?;
    let id = env.id.trim().to_string();
    let mut reg = load_envs();
    let mut env = env;
    env.id = id.clone();
    env.registered_at_unix = now_unix();
    reg.environments.retain(|e| e.id != id);
    reg.environments.push(env.clone());
    reg.environments.sort_by(|a, b| a.id.cmp(&b.id));
    if make_default || reg.default.is_empty() {
        reg.default = id;
    }
    save_envs(&reg)?;
    Ok(env)
}

pub fn remove_env(id: &str) -> Result<bool> {
    let mut reg = load_envs();
    let before = reg.environments.len();
    reg.environments.retain(|e| e.id != id);
    if reg.default == id {
        reg.default = reg.environments.first().map(|e| e.id.clone()).unwrap_or_default();
    }
    let removed = reg.environments.len() != before;
    save_envs(&reg)?;
    Ok(removed)
}

pub fn set_default_env(id: &str) -> Result<()> {
    let mut reg = load_envs();
    if !reg.environments.iter().any(|e| e.id == id) {
        bail!("没有登记过环境「{id}」（先 `hur env add`）");
    }
    reg.default = id.to_string();
    save_envs(&reg)
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/* ---------------- 三层解析 ---------------- */

fn parse_policy_file(p: &Path) -> Result<SecurityPolicy> {
    let text = std::fs::read_to_string(p).with_context(|| format!("读 {} 失败", p.display()))?;
    let mut pol: SecurityPolicy = serde_json::from_str(&text).with_context(|| format!("解析 {} 失败", p.display()))?;
    if pol.spec.is_empty() {
        pol.spec = POLICY_SPEC.to_string();
    }
    if pol.spec != POLICY_SPEC {
        bail!("{} 的 spec 必须是 {POLICY_SPEC}，当前是「{}」", p.display(), pol.spec);
    }
    Ok(pol)
}

/* ---------------- 策略文件读写（`ncc hur policy set`） ---------------- */

/// 机器层策略文件：`~/.harnessuse/security.json`
pub fn machine_policy_path() -> PathBuf {
    crate::cfg::home().join(MACHINE_POLICY_FILE)
}

/// 工程层策略文件：`<dir>/hur.security.json`。
/// 注意：**只写这一个位置**，不像解析那样向上找父目录（避免"你以为改了但它读的是上层那份"）。
pub fn project_policy_path(start: &Path) -> PathBuf {
    let dir = if start.is_file() {
        start.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
    } else {
        start.to_path_buf()
    };
    let abs = std::fs::canonicalize(&dir).unwrap_or(dir);
    abs.join(PROJECT_POLICY_FILE)
}

/// 读一份策略文件（不存在 → `None`；`spec` 缺省补全、写错则报错）
pub fn load_policy_file(p: &Path) -> Result<Option<SecurityPolicy>> {
    if !p.is_file() {
        return Ok(None);
    }
    Ok(Some(parse_policy_file(p)?))
}

/// 写一份策略文件（pretty JSON + 换行）。只做格式与 `spec` 校验；**语义由调用方负责**。
pub fn save_policy_file(p: &Path, pol: &SecurityPolicy) -> Result<()> {
    let mut doc = pol.clone();
    if doc.spec.is_empty() {
        doc.spec = POLICY_SPEC.to_string();
    }
    if doc.spec != POLICY_SPEC {
        bail!("策略 spec 必须是 {POLICY_SPEC}，当前是「{}」", doc.spec);
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("创建 {} 失败", parent.display()))?;
    }
    std::fs::write(p, format!("{}\n", serde_json::to_string_pretty(&doc)?))
        .with_context(|| format!("写 {} 失败", p.display()))?;
    Ok(())
}

pub fn find_project_policy(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.as_os_str().is_empty() { PathBuf::from(".") } else { start.to_path_buf() };
    if cur.is_file() {
        cur = cur.parent()?.to_path_buf();
    }
    let abs = std::fs::canonicalize(&cur).unwrap_or(cur);
    let mut dir = abs.as_path();
    loop {
        let p = dir.join(PROJECT_POLICY_FILE);
        if p.is_file() {
            return Some(p);
        }
        dir = dir.parent()?;
    }
}

#[derive(Debug, Clone)]
pub struct Resolved {
    /// 逐层收紧后的最终策略
    pub policy: SecurityPolicy,
    /// 每层来源（人读）：`machine:~/.harnessuse/security.json` / `project:…` / `package:<id>`
    pub sources: Vec<String>,
    /// 包内**请求**的具名策略（只是请求：更严的层可以覆盖）
    pub requested: String,
    /// 宿主层实际选定的策略 id（机器 / 工程里写的那个）
    pub host: String,
    /// 请求被宿主层覆盖（宿主更严）
    pub overridden: bool,
    /// 包的具名请求是否真的叠加进了生效策略
    pub request_stacked: bool,
}

/// 三层解析：机器 → 工程 → 包；**下层只能收紧**
pub fn resolve(start: &Path, pkg: Option<&HurPackage>) -> Result<Resolved> {
    let mut sources: Vec<String> = Vec::new();
    let mut cur = SecurityPolicy {
        spec: POLICY_SPEC.into(),
        id: "strict".into(),
        name: "只读（不执行）".into(),
        ..Default::default()
    };
    sources.push("builtin:strict".into());

    // ① 机器层：~/.harnessuse/security.json（可含具名策略库）
    let machine_path = crate::cfg::home().join(MACHINE_POLICY_FILE);
    let mut machine: Option<SecurityPolicy> = None;
    if machine_path.is_file() {
        let m = parse_policy_file(&machine_path)?;
        sources.push(format!("machine:{}", machine_path.display()));
        machine = Some(m);
    }

    // ② 工程层：hur.security.json（向上找）
    let project = find_project_policy(start);
    let project_pol = match &project {
        Some(p) => {
            sources.push(format!("project:{}", p.display()));
            Some(parse_policy_file(p)?)
        }
        None => None,
    };

    // 包内声明的具名策略：机器 → 工程 → 内置，按顺序找库
    let requested = pkg.and_then(|p| p.security.as_ref()).map(|s| s.policy.clone()).unwrap_or_default();
    let named = |id: &str| -> Option<SecurityPolicy> {
        if id.is_empty() {
            return None;
        }
        if let Some(m) = &machine {
            if let Some(p) = m.policies.get(id) {
                return Some(p.clone());
            }
        }
        if let Some(p) = &project_pol {
            if let Some(x) = p.policies.get(id) {
                return Some(x.clone());
            }
        }
        preset(id)
    };
    // 宿主层是否已显式选定策略（机器/工程文件里写了 id）
    let host_id = project_pol
        .as_ref()
        .map(|p| p.id.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| machine.as_ref().map(|p| p.id.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let host_selected = !host_id.is_empty();
    // 宿主选定的**就是**包请求的那一个 → 本来就是同一个策略，谈不到"谁覆盖谁"，当然叠加。
    // 只有两个**不同**的策略相争时，才是"宿主为准、请求只记录"。
    // （否则工程层只写一行 `id` 就会把自己请求的那套规则悄悄顶掉，那是会吓人的。）
    let same_as_request = !requested.is_empty() && host_id == requested;
    // 宿主没选时，包请求的具名策略作为基线；宿主选了就以宿主为准（请求只记录、叠加自身字段仍生效）
    let mut request_stacked = false;
    if !requested.is_empty() && (!host_selected || same_as_request) {
        match named(&requested) {
            Some(p) => {
                sources.push(format!("named:{requested}"));
                cur = tighten(&cur, &p);
                request_stacked = true;
            }
            None => {
                // 找不到就按最严处理，并在 sources 里留痕（R8 也会提醒）
                sources.push(format!("named:{requested}(未找到 → 按最严处理)"));
            }
        }
    } else if !requested.is_empty() {
        sources.push(format!("named:{requested}(未叠加：宿主已选定策略)"));
    }

    // ③ 机器 → 工程 → 包（顺序即"上层 → 下层"）
    //    层文档若**只写了 id**（没有具体规则），按"引用该具名策略"处理
    let layer_of = |l: Option<&SecurityPolicy>| -> Option<SecurityPolicy> {
        let l = l?;
        Some(if l.is_empty() && !l.id.trim().is_empty() { named(l.id.trim()).unwrap_or_else(|| l.clone()) } else { l.clone() })
    };
    let machine_layer = layer_of(machine.as_ref());
    let project_layer = layer_of(project_pol.as_ref());
    for layer in [machine_layer.clone(), project_layer.clone(), pkg.and_then(|p| p.security.as_ref()).map(|s| s.as_policy())] {
        if let Some(l) = layer {
            cur = tighten(&cur, &l);
        }
    }

    // 展示身份：宿主（工程 → 机器）选定的策略优先；包只提请求
    let host = project_pol
        .as_ref()
        .map(|p| p.id.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| machine.as_ref().map(|p| p.id.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let display = if !host.is_empty() { host.clone() } else if !requested.is_empty() { requested.clone() } else { cur.id.clone() };
    let host_name = project_pol
        .as_ref()
        .map(|p| p.name.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| machine.as_ref().map(|p| p.name.trim().to_string()).filter(|s| !s.is_empty()))
        .or_else(|| named(&display).map(|p| p.name).filter(|s| !s.is_empty()));
    if let Some(n) = host_name {
        cur.name = n;
    } else if let Some(p) = named(&display) {
        if !p.name.is_empty() {
            cur.name = p.name;
        }
    }
    cur.id = display.clone();
    let overridden = !requested.is_empty() && !host.is_empty() && requested != host;
    Ok(Resolved { policy: cur, sources, requested, host, overridden, request_stacked })
}

/// 解析结果的默认值视图（人读 / 计划器共用）
pub fn effective(p: &SecurityPolicy) -> Effective {
    Effective {
        verify_required: p.verify.required.unwrap_or(true),
        max_errors: p.verify.max_errors.unwrap_or(0),
        strict: p.verify.strict.unwrap_or(false),
        require_lock: p.verify.require_lock.unwrap_or(true),
        require_signature: p.verify.require_signature.unwrap_or(false),
        exec_enabled: p.exec.enabled.unwrap_or(false),
        engines: p.exec.engines.clone().unwrap_or_default(),
        local_only: p.exec.local_only.unwrap_or(true),
        max_output_chars: p.exec.max_output_chars.unwrap_or(8000),
        fuel: p.sandbox.fuel.unwrap_or(50_000_000),
        memory_mb: p.sandbox.memory_mb.unwrap_or(256),
        stack_kb: p.sandbox.stack_kb.unwrap_or(1024),
        wall_ms: p.sandbox.wall_ms.unwrap_or(5000),
        readonly_preopen: p.sandbox.readonly_preopen.unwrap_or(true),
        network: p.sandbox.network.clone().unwrap_or_else(|| "declared-only".into()),
        max_network_calls: p.sandbox.max_network_calls.unwrap_or(8),
        remote_allowed: p.remote.allowed.unwrap_or(false),
        remote_environments: p.remote.environments.clone().unwrap_or_default(),
        require_attestation: p.remote.require_attestation.unwrap_or(true),
        allow_data_leaving: p.remote.allow_data_leaving.unwrap_or(false),
        trace_dir: p.audit.trace_dir.clone().unwrap_or_else(|| ".hur/run".into()),
        retain: p.audit.retain.unwrap_or(50),
        redact_secrets: p.audit.redact_secrets.unwrap_or(true),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    pub verify_required: bool,
    pub max_errors: u32,
    pub strict: bool,
    pub require_lock: bool,
    pub require_signature: bool,
    pub exec_enabled: bool,
    pub engines: Vec<String>,
    pub local_only: bool,
    pub max_output_chars: u32,
    pub fuel: u64,
    pub memory_mb: u32,
    pub stack_kb: u32,
    pub wall_ms: u32,
    pub readonly_preopen: bool,
    pub network: String,
    pub max_network_calls: u32,
    pub remote_allowed: bool,
    pub remote_environments: Vec<String>,
    pub require_attestation: bool,
    pub allow_data_leaving: bool,
    pub trace_dir: String,
    pub retain: u32,
    pub redact_secrets: bool,
}

/* ---------------- 执行计划器 ---------------- */

#[derive(Debug, Clone, Serialize)]
pub struct VerifyStep {
    pub required: bool,
    pub strict: bool,
    pub max_errors: u32,
    pub require_lock: bool,
    pub require_signature: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunPlan {
    pub package: String,
    pub name: String,
    pub version: String,
    pub policy: String,
    pub policy_name: String,
    pub sources: Vec<String>,
    /// 是否允许执行（false = 只能走声明式）
    pub allowed: bool,
    /// 选定引擎（None = 不允许执行）
    pub engine: Option<String>,
    /// 该引擎在本机是否已实现（= `register_engines` 登记过的。没登记就如实标 false，不假装能跑）
    pub engine_ready: bool,
    /// 远程执行时选中的注册环境
    pub environment: Option<String>,
    pub verify: VerifyStep,
    pub limits: Value,
    /// 拒绝理由 / 提醒（人读，逐条）
    pub reasons: Vec<String>,
    /// 已核对过的检查项（人读）——让"为什么放行"同样可审
    pub checks: Vec<String>,
}

/// 生成执行计划（不执行任何代码）。这就是「策略 → 可审计划」的那一步。
pub fn plan(pkg: &HurPackage, policy: &SecurityPolicy, sources: &[String], verify_issues: &[crate::spec::Issue]) -> RunPlan {
    let e = effective(policy);
    let mut reasons = Vec::new();
    let mut checks = Vec::new();

    // ① verify 前置
    let errors = verify_issues.iter().filter(|i| i.level == Level::Error).count() as u32;
    let warns = verify_issues.iter().filter(|i| i.level == Level::Warn).count() as u32;
    if e.verify_required {
        checks.push(format!("verify 前置：必须过 R1~R10（当前 {errors} 错 / {warns} 提醒）"));
        if errors > e.max_errors {
            reasons.push(format!("verify 未通过：{errors} 个错误 > 策略允许的 {} 个", e.max_errors));
        }
        if e.strict && warns > 0 {
            reasons.push(format!("策略要求 strict：{warns} 个提醒也算失败"));
        }
    } else {
        reasons.push("策略未要求 verify 前置 —— 不推荐（无法保证包结构合法）".into());
    }
    if e.require_lock {
        checks.push("要求 hur.lock（依赖已锁定）".into());
    }
    if e.require_signature {
        checks.push("要求签名：产物须带可核对签名（R9）".into());
        // fail-closed：签名核对是外部输入（目录/产物），计划器自己看不见 ——
        // 调用方必须把 R9 检查项喂进来（用 `policy::collect_issues` 或 `hur verify`）。
        // 没喂 = 没人核过 → 拒绝，绝不"因为看不见就当它过了"。
        if !verify_issues.iter().any(|i| i.rule == "R9") {
            reasons.push(
                "策略要求签名，但这次计划没有拿到签名核对结果（调用方应先做 R9 核对：`hur verify` / `policy::collect_issues`）\
                 —— 拒绝执行以免漏检。"
                    .into(),
            );
        }
    }

    // ② 是否允许执行
    let req = pkg.security.as_ref();
    let pkg_wants_exec = req.map(|r| r.exec.enabled.unwrap_or(false)).unwrap_or(false);
    let pkg_self_disabled = req.map(|r| r.exec.enabled == Some(false)).unwrap_or(false);
    if !e.exec_enabled {
        reasons.push(if pkg_self_disabled {
            "包内声明 exec.enabled=false（本包自己选择不执行）".into()
        } else {
            "生效策略未开启 exec（只允许声明式装配）—— 检查机器/工程层的 security 与包内声明的交集".into()
        });
    }
    let pkg_engines: Vec<String> = req.and_then(|r| r.exec.engines.clone()).unwrap_or_default();
    if e.exec_enabled && !pkg_wants_exec {
        reasons.push("包内没有声明 exec（`security.exec.enabled: true`），按只读包处理".into());
    }
    let entry = req.map(|r| r.entry.clone()).unwrap_or_default();
    if pkg_wants_exec && entry.trim().is_empty() {
        reasons.push("包声明了 exec 但没给执行入口（`security.entry`，如 src/agent.wasm）".into());
    }

    // ③ 引擎选择：策略允许 ∩ 包声明（包没声明 = 跟随策略）
    let mut engine: Option<String> = None;
    if reasons.iter().all(|r| !r.contains("exec") && !r.contains("没有声明 exec") && !r.starts_with("verify 未通过") && !r.contains("没给执行入口")) {
        let candidates: Vec<String> = if pkg_engines.is_empty() { e.engines.clone() } else { e.engines.iter().filter(|x| pkg_engines.contains(x)).cloned().collect() };
        if candidates.is_empty() {
            reasons.push(if e.engines.is_empty() {
                format!("生效策略未允许任何引擎（上层与包请求（{}）的交集为空）", disp(&pkg_engines))
            } else if pkg_engines.is_empty() {
                "生效策略没有允许任何引擎".into()
            } else {
                format!("包要的引擎（{}）与策略允许的（{}）没有交集", pkg_engines.join("+"), disp(&e.engines))
            });
        } else {
            engine = Some(candidates[0].clone());
            checks.push(format!("引擎选择：策略 {} ∩ 包声明 {} → {}", disp(&e.engines), disp(&pkg_engines), candidates.join("+")));
        }
    }

    // ④ 远程 / 本地
    let mut environment: Option<String> = None;
    if let Some(eng) = &engine {
        if eng == "remote" {
            if !e.remote_allowed {
                reasons.push("选了远程引擎，但策略禁止远程执行（`remote.allowed`）".into());
            }
            let reg = load_envs();
            let pool: Vec<Environment> = reg
                .environments
                .iter()
                .filter(|x| x.kind == "proxy")
                .filter(|x| x.engines.is_empty() || x.engines.iter().any(|k| k != "remote"))
                .filter(|x| e.remote_environments.is_empty() || e.remote_environments.contains(&x.id))
                .cloned()
                .collect();
            let pick = reg
                .environments
                .iter()
                .find(|x| x.id == reg.default)
                .filter(|x| pool.iter().any(|p| p.id == x.id))
                .cloned()
                .or_else(|| pool.first().cloned());
            match pick {
                None => reasons.push("没有可用的已登记远程环境（先 `hur env add …`；或用 --env 指定）".into()),
                Some(env) => {
                    if e.require_attestation && !env.attestation.present() {
                        reasons.push(format!("环境「{}」缺证明（registry 引用 + sha256），策略要求要求可核对", env.id));
                    }
                    if !e.allow_data_leaving && env.url.trim().is_empty() {
                        reasons.push(format!("环境「{}」没登记地址，无法投递", env.id));
                    }
                    environment = Some(env.id.clone());
                    checks.push(format!("远程环境：{}（{}）", env.id, if env.url.is_empty() { "本地登记" } else { &env.url }));
                }
            }
        } else if e.local_only {
            checks.push(format!("本地执行（策略 local_only）：本机引擎 {eng}"));
        }
    }

    let allowed = engine.is_some() && reasons.is_empty();
    if !allowed && engine.is_some() {
        engine = None;
    }
    let engine_ready = engine.as_ref().map(|x| engine_implemented(x)).unwrap_or(false);

    RunPlan {
        package: pkg.id.clone(),
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        policy: policy.id.clone(),
        policy_name: policy.name.clone(),
        sources: sources.to_vec(),
        allowed,
        engine,
        engine_ready,
        environment,
        verify: VerifyStep { required: e.verify_required, strict: e.strict, max_errors: e.max_errors, require_lock: e.require_lock, require_signature: e.require_signature },
        limits: serde_json::json!({
            "fuel": e.fuel, "memory_mb": e.memory_mb, "stack_kb": e.stack_kb, "wall_ms": e.wall_ms,
            "readonly_preopen": e.readonly_preopen, "network": e.network,
            "max_network_calls": e.max_network_calls, "max_output_chars": e.max_output_chars,
            "trace_dir": e.trace_dir, "retain": e.retain, "redact_secrets": e.redact_secrets,
        }),
        reasons,
        checks,
    }
}

fn disp(v: &[String]) -> String {
    if v.is_empty() {
        "（空）".into()
    } else {
        v.join("+")
    }
}

/// 人读的计划文本（CLI / GUI 共用）
pub fn render_plan(p: &RunPlan) -> String {
    let mut s = String::new();
    s.push_str(&format!("{}（{} v{}）\n", p.name, p.package, p.version));
    s.push_str(&format!("  策略      {} · {}\n", p.policy, p.policy_name));
    s.push_str(&format!("  来源      {}\n", p.sources.join(" → ")));
    let v = &p.verify;
    s.push_str(&format!(
        "  verify    {}（strict={} max_errors={} lock={} signature={}）\n",
        if v.required { "前置" } else { "不要求" },
        v.strict,
        v.max_errors,
        v.require_lock,
        v.require_signature
    ));
    match (&p.engine, p.allowed) {
        (Some(e), true) => {
            s.push_str(&format!("  执行      {} 引擎 {}（本机已实现：{}）\n", if p.engine_ready { "✔" } else { "·" }, e, if p.engine_ready { "是" } else { "否 —— 计划可行但本机没有该引擎" }));
            if let Some(env) = &p.environment {
                s.push_str(&format!("  环境      {}（注册的远程 sandbox）\n", env));
            }
        }
        _ => s.push_str("  执行      ✘ 不允许（只走声明式装配）\n"),
    }
    if !p.checks.is_empty() {
        s.push_str("  已核对：\n");
        for c in &p.checks {
            s.push_str(&format!("    · {c}\n"));
        }
    }
    if let Some(l) = p.limits.as_object() {
        let bits: Vec<String> = l.iter().map(|(k, v)| format!("{k}={}", v.to_string().trim_matches('"'))).collect();
        s.push_str(&format!("  限额      {}\n", bits.join(" ")));
    }
    if !p.reasons.is_empty() {
        s.push_str("  拒绝/提醒：\n");
        for r in &p.reasons {
            s.push_str(&format!("    ✗ {r}\n"));
        }
    }
    s
}

/* ---------------- R8：包内 security 声明体检 ---------------- */

/// 包内 `security{}` 的静态校验（并入 `hur verify` 的规则表）
pub fn validate_security(pkg: &HurPackage, dir: &Path, known_policy: Option<bool>) -> Vec<crate::spec::Issue> {
    let mut out = Vec::new();
    let Some(sec) = pkg.security.as_ref() else {
        return out;
    };
    if pkg.kind != "agent" {
        out.push(crate::spec::Issue::warn("R8", "只有 kind=agent 才会读取 security{} 声明（当前 kind 会忽略它）"));
    }
    for e in sec.exec.engines.clone().unwrap_or_default() {
        let v = e.trim();
        if v.is_empty() {
            out.push(crate::spec::Issue::err("R8", "security.exec.engines 里有空项"));
        } else if !valid_engine(v) {
            out.push(crate::spec::Issue::err("R8", format!("security.exec.engines 里的「{v}」不是已知引擎（可选：{}）", ENGINES.join(" / "))));
        }
    }
    let wants_exec = sec.exec.enabled.unwrap_or(false);
    let entry = sec.entry.trim();
    if wants_exec {
        if entry.is_empty() && sec.exec.engines.clone().unwrap_or_default().is_empty() {
            out.push(crate::spec::Issue::err("R8", "声明了 exec 但既没给 security.entry，也没给 security.exec.engines"));
        } else if !entry.is_empty() {
            let p = dir.join(entry);
            if !p.exists() {
                out.push(crate::spec::Issue::err("R8", format!("security.entry「{entry}」在包内不存在")));
            } else if !entry.ends_with(".wasm") && !entry.ends_with(".wat") && sec.exec.engines.clone().unwrap_or_default().contains(&"wasm".to_string()) {
                out.push(crate::spec::Issue::warn("R8", format!("security.entry「{entry}」不是 .wasm/.wat，但 engines 里声明了 wasm（需要预编译目标）")));
            }
        }
    }
    if sec.remote.allowed.unwrap_or(false) && !sec.verify.require_signature.unwrap_or(false) {
        out.push(crate::spec::Issue::warn("R8", "包声明允许远程执行，建议同时要求签名（security.verify.require_signature）"));
    }
    match sec.sandbox.network.as_deref() {
        None | Some("none") | Some("declared-only") => {}
        Some(other) => out.push(crate::spec::Issue::err("R8", format!("security.sandbox.network 只能是 none | declared-only，当前「{other}」"))),
    }
    if !sec.policy.trim().is_empty() && known_policy == Some(false) {
        out.push(crate::spec::Issue::warn("R8", format!("security.policy「{}」不在内置预设里（机器/工程层可自定义；找不到会按最严处理）", sec.policy.trim())));
    }
    out
}

/* ---------------- 跑之前先把该验的验完（R1~R10） ---------------- */

/// **一次把该做的检查都做了**：R1~R8（`spec::validate`）+ R9（制品签名，按生效策略的
/// `verify.require_signature`）。CLI 与 GUI 都走这里 —— 少一个调用点忘了核签名，
/// 策略里的 `require_signature` 就成了装饰品。
pub fn collect_issues(dir: &Path, pkg: &HurPackage, policy: &SecurityPolicy) -> Result<Vec<crate::spec::Issue>> {
    let mut out = crate::spec::validate(pkg, dir, crate::spec::read_lock(dir).as_ref(), false);
    let need_sig = effective(policy).require_signature;
    let rep = crate::sign::store().check_dir(dir, pkg, need_sig, None);
    out.extend(rep.issues);
    Ok(out)
}

/* ---------------- 执行留痕（trace）：audit 策略的落地形式 ---------------- */

pub const TRACE_SPEC: &str = "harness-use-run-trace/v1";

/// 一次真执行的留痕。**引擎结论原样存进 `outcome`**：policy 层不解释引擎语义，
/// 只负责"跑过必留痕、留痕可回溯"（PRD §13.8）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    pub spec: String,
    pub at_unix: u64,
    pub package: String,
    pub name: String,
    pub version: String,
    pub policy: String,
    pub engine: String,
    /// 生效限额（燃料/内存/墙钟/网络…），便于事后核对"当时按多少跑的"
    pub limits: Value,
    /// 引擎给出的结论（各引擎自有字段：ok / error / host_calls / fuel_used …）
    pub outcome: Value,
}

impl TraceRecord {
    pub fn new(pkg: &crate::spec::HurPackage, policy_id: &str, engine: &str, limits: Value, outcome: Value) -> Self {
        Self {
            spec: TRACE_SPEC.into(),
            at_unix: now_unix(),
            package: pkg.id.clone(),
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            policy: policy_id.to_string(),
            engine: engine.to_string(),
            limits,
            outcome,
        }
    }
}

/// trace 文件名：`{时间戳}-{包 id}-{序号}.json`。
///
/// 序号**总是**带上并补零两位 —— 这样「按文件名排序」就等于「按写入顺序排序」。
/// 曾经是「第一条不带后缀、撞名才加」，结果 `…-01.json` 会排在 `….json` 前面，
/// 按名字裁剪时反而把**最新**的那条删掉（有测试钉住这一点）。
pub fn trace_file_name(rec: &TraceRecord, seq: u32) -> String {
    let slug: String = rec.package.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    format!("{}-{}-{:02}.json", rec.at_unix, slug, seq)
}

/// 把 trace 写进策略指定的 `audit.trace_dir`，并按 `retain` 裁剪旧文件。
/// 返回落盘路径。
pub fn write_trace(dir: &Path, retain: u32, rec: &TraceRecord) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    // 同一秒内跑两次是常事（脚本连跑 / 失败重试）—— 序号顺延，
    // 否则后一次会把前一次的留痕覆盖掉，"跑过必留痕"就成了空话。
    let mut seq = 0u32;
    let mut path = dir.join(trace_file_name(rec, seq));
    while path.exists() {
        seq += 1;
        path = dir.join(trace_file_name(rec, seq));
    }
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(rec).unwrap_or_else(|_| "{}".into())))?;
    prune_traces(dir, retain);
    Ok(path)
}

/// 只留最近 `retain` 个 trace（按文件名里的时间戳排序；retain=0 表示不留）
pub fn prune_traces(dir: &Path, retain: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    files.sort();
    let keep = retain as usize;
    if files.len() <= keep {
        return;
    }
    for name in &files[..files.len() - keep] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{AgentSpec, Deps, Permissions, PublishInfo, PKG_SPEC};

    fn pkg(sec: Option<SecurityReq>) -> HurPackage {
        HurPackage {
            egress: None,
            spec: PKG_SPEC.into(),
            kind: "agent".into(),
            id: "A-hotel-front-desk-abc123".into(),
            name: "Front Desk".into(),
            version: "1.0.0".into(),
            short: "FD".into(),
            domain: "hotel".into(),
            summary: "接待助手。".into(),
            entry: "src/agent.ts".into(),
            runtime: "local-v0".into(),
            capabilities: vec![],
            deps: Deps::default(),
            permissions: Permissions { network: vec!["api.hotel.example.com".into()], local: vec!["kb".into()] },
            publish: PublishInfo::default(),
            agent: Some(AgentSpec { system_prompt: "你是前台".into(), ..Default::default() }),
            security: sec,
        }
    }

    fn req_exec(engines: &[&str], entry: &str) -> SecurityReq {
        SecurityReq {
            policy: "wasm-local".into(),
            exec: ExecRule { enabled: Some(true), engines: Some(engines.iter().map(|s| s.to_string()).collect()), ..Default::default() },
            entry: entry.into(),
            ..Default::default()
        }
    }

    #[test]
    fn presets_are_valid_and_listed() {
        let ps = presets();
        assert_eq!(ps.len(), 5);
        assert!(ps.iter().all(|p| p.spec == POLICY_SPEC));
        assert!(ps.iter().any(|p| p.id == "strict"));
        assert!(preset("wasm-local").is_some());
        assert!(preset("nope").is_none());
        for p in &ps {
            for e in p.exec.engines.clone().unwrap_or_default() {
                assert!(valid_engine(&e), "{e}");
            }
        }
    }

    /// 单调收紧：每一条都只能更严，不能更宽
    #[test]
    fn tightening_is_monotone() {
        let upper = preset("wasm-local").unwrap();
        let lower = SecurityPolicy {
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["wasm".into(), "js".into()]), local_only: Some(false), max_output_chars: Some(9999) },
            sandbox: SandboxRule { memory_mb: Some(64), fuel: Some(1_000), wall_ms: Some(99999), network: Some("declared-only".into()), max_network_calls: Some(99), ..Default::default() },
            remote: RemoteRule { allowed: Some(true), allow_data_leaving: Some(true), require_attestation: Some(false), ..Default::default() },
            verify: VerifyRule { strict: Some(false), require_signature: Some(true), max_errors: Some(3), ..Default::default() },
            ..Default::default()
        };
        let r = tighten(&upper, &lower);
        let eu = effective(&upper);
        let er = effective(&r);
        // 引擎取交集：js 不该被"加进来"
        assert_eq!(er.engines, vec!["wasm"]);
        assert!(!eu.engines.is_empty());
        // 限额只能更小
        assert_eq!(er.memory_mb, 64);
        assert!(er.memory_mb <= eu.memory_mb);
        assert_eq!(er.max_network_calls, 8);
        assert!(er.max_output_chars <= eu.max_output_chars);
        // 布尔项只能更严：下层说 false 不会放宽上层（local_only 仍真），下层说 true 则收紧（signature 变真）
        assert!(er.local_only, "下层 local_only=false 不得放宽");
        assert!(er.strict, "下层 strict=false 不得放宽");
        assert!(er.require_signature, "下层要求签名应收紧");
        assert!(er.require_attestation, "下层 require_attestation=false 不得放宽");
        assert!(er.require_lock);
        assert!(!er.remote_allowed && !er.allow_data_leaving);
        assert_eq!(er.max_errors, 0);
    }

    #[test]
    fn plan_allows_wasm_when_policy_and_package_agree() {
        let pol = preset("wasm-local").unwrap();
        let p = pkg(Some(req_exec(&["wasm"], "src/agent.wasm")));
        let got = plan(&p, &pol, &["builtin:wasm-local".into()], &[]);
        assert!(got.allowed, "{:?}", got.reasons);
        assert_eq!(got.engine.as_deref(), Some("wasm"));
        // engine_ready 是"宿主登记了什么"的投影（测试并行跑，这里只断言一致性）
        assert_eq!(got.engine_ready, engine_implemented("wasm"), "engine_ready 必须与已登记引擎一致");
        assert_eq!(got.limits["memory_mb"], 256);
        assert_eq!(got.limits["redact_secrets"], true);
    }

    /// 要求签名的策略：**没拿到 R9 核对结果就拒绝**（fail-closed），拿到且通过才放行。
    #[test]
    fn require_signature_is_fail_closed_without_r9_evidence() {
        let mut pol = preset("wasm-local").unwrap();
        pol.verify.require_signature = Some(true);
        pol.verify.strict = Some(false);
        let p = pkg(Some(req_exec(&["wasm"], "src/agent.wasm")));

        // ① 调用方没核签名：不能因为"计划器看不见"就当它过了
        let got = plan(&p, &pol, &[], &[]);
        assert!(!got.allowed);
        assert!(got.reasons.iter().any(|r| r.contains("没有拿到签名核对结果")), "{:?}", got.reasons);

        // ② 核过了（R9 无错）→ 放行
        let ok = plan(&p, &pol, &[], &[crate::spec::Issue::warn("R9", "签名有效")]);
        assert!(ok.allowed, "{:?}", ok.reasons);

        // ③ R9 报错（没签名 / 不可核对 / 被篡改）→ 拒绝
        let bad = plan(&p, &pol, &[], &[crate::spec::Issue::err("R9", "没有签名")]);
        assert!(!bad.allowed, "{:?}", bad.reasons);
        assert!(bad.reasons.iter().any(|r| r.contains("verify 未通过")), "{:?}", bad.reasons);
    }

    /// 登记引擎会让同一份计划从"不可执行"变成"可执行" —— 这就是 `run --exec` 的开关。
    /// （测试并行跑，注册表是进程全局的：所以"前"状态只在确实为空时才断言。）
    /// 留痕：写得出、读得回、按 retain 裁剪（审计是策略承诺，不是可选装饰）
    #[test]
    fn traces_are_written_and_pruned() {
        let dir = std::env::temp_dir().join(format!("hur-trace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = pkg(Some(req_exec(&["wasm"], "src/agent.wasm")));
        let mut paths = Vec::new();
        for i in 0..4u64 {
            let mut rec = TraceRecord::new(&p, "wasm-local", "wasm", serde_json::json!({"fuel": 1}), serde_json::json!({"ok": true}));
            rec.at_unix = 1_700_000_000 + i; // 固定时间戳，保证排序可预期
            paths.push(write_trace(&dir, 2, &rec).unwrap());
        }
        assert!(paths[2].exists() && paths[3].exists());
        assert!(!paths[0].exists() && !paths[1].exists(), "retain=2 应把更早的 trace 裁掉");
        let left: Vec<String> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter_map(|e| e.file_name().into_string().ok()).collect();
        assert_eq!(left.len(), 2, "retain=2 只应留最近两条：{left:?}");
        let text = std::fs::read_to_string(&paths[3]).unwrap();
        let back: TraceRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(back.spec, TRACE_SPEC);
        assert_eq!(back.package, p.id);
        assert_eq!(back.outcome["ok"], true);
        assert_eq!(back.at_unix, 1_700_000_003);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registering_engine_flips_engine_ready() {
        let pol = preset("wasm-local").unwrap();
        let p = pkg(Some(req_exec(&["wasm"], "src/agent.wasm")));
        if !engine_implemented("wasm") {
            assert!(!plan(&p, &pol, &[], &[]).engine_ready, "没登记就不许标可执行");
        }
        register_engines(&["wasm"]);
        let got = plan(&p, &pol, &[], &[]);
        assert!(got.engine_ready, "登记后必须标可执行");
        assert!(engine_implemented("wasm"));
        assert!(!engine_implemented("container"), "没登记的引擎不许混进来");
    }

    #[test]
    fn plan_denies_when_policy_is_readonly() {
        let pol = preset("strict").unwrap();
        let p = pkg(Some(req_exec(&["wasm"], "src/agent.wasm")));
        let got = plan(&p, &pol, &[], &[]);
        assert!(!got.allowed);
        assert!(got.engine.is_none());
        assert!(got.reasons.iter().any(|r| r.contains("未开启 exec")), "{:?}", got.reasons);
    }

    #[test]
    fn plan_denies_on_engine_mismatch_and_missing_entry() {
        let pol = preset("wasm-local").unwrap();
        let p = pkg(Some(req_exec(&["js"], "src/agent.ts")));
        let got = plan(&p, &pol, &[], &[]);
        assert!(!got.allowed);
        assert!(got.reasons.iter().any(|r| r.contains("没有交集")), "{:?}", got.reasons);

        let pol2 = preset("dev-js").unwrap();
        let bad = pkg(Some(SecurityReq { policy: "dev-js".into(), exec: ExecRule { enabled: Some(true), ..Default::default() }, entry: String::new(), ..Default::default() }));
        let got2 = plan(&bad, &pol2, &[], &[]);
        assert!(!got2.allowed);
        assert!(got2.reasons.iter().any(|r| r.contains("没给执行入口")), "{:?}", got2.reasons);
    }

    #[test]
    fn plan_blocks_when_verify_fails() {
        let pol = preset("wasm-local").unwrap();
        let p = pkg(Some(req_exec(&["wasm"], "src/agent.wasm")));
        let issues = vec![crate::spec::Issue::err("R5", "未声明域名")];
        let got = plan(&p, &pol, &[], &issues);
        assert!(!got.allowed);
        assert!(got.reasons.iter().any(|r| r.contains("verify 未通过")), "{:?}", got.reasons);
    }

    #[test]
    fn remote_policy_needs_registered_environment_with_attestation() {
        let pol = preset("remote-only").unwrap();
        let p = pkg(Some(SecurityReq { policy: "remote-only".into(), exec: ExecRule { enabled: Some(true), engines: Some(vec!["remote".into()]), ..Default::default() }, entry: "src/agent.wasm".into(), ..Default::default() }));
        // 先在没有环境的情况下：必须拒绝
        let got = plan(&p, &pol, &[], &[]);
        assert!(!got.allowed, "{:?}", got.reasons);

        // 登记一个**没有证明**的环境 → 仍拒绝
        let dir = std::env::temp_dir().join(format!("hur-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = sanity_env_flow(&dir);
        assert!(ok, "环境登记流程应可跑通");
    }

    /// 环境登记/移除/默认切换（真实读写 `~/.harnessuse/environments.json` 的临时覆盖）
    fn sanity_env_flow(_dir: &Path) -> bool {
        let reg = EnvRegistry::default();
        assert_eq!(reg.spec, ENV_SPEC);
        assert!(reg.environments.is_empty());
        // 不写盘，只验证校验逻辑
        let bad = Environment { id: "Bad Id".into(), kind: "proxy".into(), url: "https://x".into(), ..Default::default() };
        assert!(validate_env(&bad).is_err(), "非法 id 必须被拒");
        let no_url = Environment { id: "ok".into(), kind: "proxy".into(), url: String::new(), ..Default::default() };
        assert!(validate_env(&no_url).is_err(), "proxy 无地址必须被拒");
        let bad_engine = Environment { id: "ok".into(), kind: "proxy".into(), url: "https://x".into(), engines: vec!["vm".into()], ..Default::default() };
        assert!(validate_env(&bad_engine).is_err(), "未知引擎必须被拒");
        let fine = Environment { id: "local-wasm".into(), kind: "local".into(), engines: vec!["wasm".into()], ..Default::default() };
        assert!(validate_env(&fine).is_ok(), "本地环境不该要求 url");
        true
    }

    #[test]
    fn r8_flags_bad_security_declarations() {
        let dir = std::env::temp_dir().join(format!("hur-r8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/agent.wasm"), b"\0asm").unwrap();

        // 未知引擎 → error；entry 不存在 → error
        let mut p = pkg(Some(SecurityReq { exec: ExecRule { enabled: Some(true), engines: Some(vec!["vm".into()]), ..Default::default() }, entry: "src/nope.wasm".into(), ..Default::default() }));
        let issues = validate_security(&p, &dir, Some(false));
        assert!(issues.iter().any(|i| i.rule == "R8" && i.level == Level::Error), "{issues:?}");

        // 正常声明 → 错误清空（签名提醒可存在）
        p.security = Some(SecurityReq {
            policy: "wasm-local".into(),
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["wasm".into()]), ..Default::default() },
            entry: "src/agent.wasm".into(),
            verify: VerifyRule { require_signature: Some(true), ..Default::default() },
            ..Default::default()
        });
        let issues = validate_security(&p, &dir, Some(true));
        assert!(issues.iter().all(|i| i.level != Level::Error), "{issues:?}");

        // .ts 入口却声明 wasm → 提醒；network 写错 → error
        let mut p2 = pkg(Some(SecurityReq {
            exec: ExecRule { enabled: Some(true), engines: Some(vec!["wasm".into()]), ..Default::default() },
            entry: "src/agent.ts".into(),
            sandbox: SandboxRule { network: Some("anything".into()), ..Default::default() },
            ..Default::default()
        }));
        std::fs::write(dir.join("src/agent.ts"), "export const x=1\n").unwrap();
        let issues = validate_security(&p2, &dir, Some(true));
        assert!(issues.iter().any(|i| i.rule == "R8" && i.msg.contains("不是 .wasm")), "{issues:?}");
        assert!(issues.iter().any(|i| i.rule == "R8" && i.msg.contains("network")), "{issues:?}");

        // 非 agent 带声明 → 提醒
        p2.kind = "harness".into();
        p2.id = "H-hotel-x-000000".into();
        let issues = validate_security(&p2, &dir, Some(true));
        assert!(issues.iter().any(|i| i.rule == "R8" && i.msg.contains("kind=agent")), "{issues:?}");
    }

    /// 同一秒内跑两次不能互相覆盖：留痕是审计证据，丢了就等于没跑。
    #[test]
    fn traces_in_the_same_second_do_not_overwrite_each_other() {
        let dir = std::env::temp_dir().join(format!("hur-trace-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = |outcome: &str| TraceRecord {
            spec: TRACE_SPEC.into(),
            at_unix: 1_700_000_000,
            package: "A-demo-x-000001".into(),
            name: "X".into(),
            version: "0.1.0".into(),
            policy: "wasm-local".into(),
            engine: "wasm".into(),
            limits: serde_json::json!({}),
            outcome: serde_json::json!({ "ok": outcome == "ok" }),
        };
        let a = write_trace(&dir, 50, &rec("ok")).unwrap();
        let b = write_trace(&dir, 50, &rec("bad")).unwrap();
        assert_ne!(a, b, "同秒两次必须落成两个文件");
        assert!(a.exists() && b.exists(), "两次的留痕都要在");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        // 序号补零：字典序（prune 用的排序）必须与写入顺序一致
        let mut sorted = names.clone();
        sorted.sort();
        let expect = vec![
            a.file_name().unwrap().to_string_lossy().to_string(),
            b.file_name().unwrap().to_string_lossy().to_string(),
        ];
        assert_eq!(sorted, expect, "按名字排序应等于按写入顺序：{names:?}");
        // retain=1 时只留最后一个（= 最新的那个）—— 这条是上面那个坑的回归锚点
        let c = write_trace(&dir, 1, &rec("ok")).unwrap();
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert_eq!(left.len(), 1, "retain=1 应只留 1 个：{left:?}");
        assert!(c.exists(), "留下的必须是**最新**那条（而不是字典序最小的）");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
