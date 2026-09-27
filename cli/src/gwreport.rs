// ncc gateway 的**控制面接入**（F2 / F3 / F5）：绑定、心跳、审计摘要上报、留存查看。
//
//      ncc gateway bind      注册到控制面（拿一次性令牌，写进 ~/.ncc/gateway.json）
//      ncc gateway heartbeat 手动心跳（常驻运行时由 `gateway run` 周期自动发）
//      ncc gateway report    把本地 JSONL 审计聚合成**摘要** → 签名 → 上报
//      ncc gateway audit     看留存摘要（--remote 看控制面那侧）
//      ncc gateway usage     用量摘要（不含计费，口径写清楚）
//      ncc gateway unbind    注销（凭据立即失效；留存摘要保留）
//
// ## 三条红线（与 ncc-platform 的 `internal/model/gateway.go` 是同一套，别在一端放宽）
//
//  1. **控制面只收摘要**：计数 / 字节总量 / **主机名**聚合 / 状态码桶 / 分位。
//     逐条审计（含 `path`、含 caller）**只留本地** —— 路径里常有订单号、用户 id，
//     传上去就等于把业务数据传上去了。本地审计有 `path`，上报时一律丢掉。
//  2. **断线不停工**：控制面不可达时审计照落本地，摘要进**待传队列**（落盘），
//     恢复后补传。网关的可用性永远不取决于控制面。
//  3. **签名说明来源，不说明内容为真**：`HMAC-SHA256(key = sha256(令牌), 规范字节)`。
//     它证明"这份摘要来自持有该令牌的进程、且没被改动"；**不**证明数字是真的
//     （持有令牌的人可以报任何数）。计费/合规要把这个口径写清，别把它当"验过了"。
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::api;
use crate::config::{self, CliConfig};
use crate::gateway;

/* ============================ 常量与规范形 ============================ */

/// 摘要规范名（进 digest 的域分隔；与 Go 侧 `model.GatewayAuditSpec` 一致）。
pub const AUDIT_SPEC: &str = "ncc-gateway-audit/v1";

/// 一次上报里最多几条摘要（与服务端 `len(items) > 200` 的上限一致）。
const BATCH_MAX: usize = 200;

/// 上报/心跳间隔默认值（秒）。心跳默认 30s（PRD F3），上报默认 5 分钟。
pub const DEFAULT_HEARTBEAT_SEC: u64 = 30;
pub const DEFAULT_REPORT_SEC: u64 = 300;

/// 单个窗口最长时长。服务端拒收 > 24h 的窗口，这里默认 1 小时：
/// 一台离线一周的网关恢复后要补传，切成小时窗口比一个大窗口更可读，
/// 也避免"一次上报把留存撑满"。
const DEFAULT_WINDOW_MINUTES: i64 = 60;

/// 本地状态文件（待传队列 + 水位）。**与 gateway.json 分开**：
/// 那份是手写的配置，这份是机器写的账本，混在一起会让人不敢编辑配置。
pub fn state_path() -> PathBuf {
    config::ncc_dir().join("gateway-report.json")
}

/* ============================ HMAC-SHA256 ============================ */

/// HMAC-SHA256（RFC 2104）。用现成的 `sha2` 手写，不引新依赖。
///
/// 为什么敢手写：HMAC 的结构极简（ipad/opad 两轮哈希），且下面有 RFC 4231 官方
/// 测试向量钉住 —— 比自己发明一个"签名格式"安全得多。
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(ih);
    outer.finalize().to_vec()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 摘要的签名密钥材料：`sha256(网关令牌)` 的十六进制**文本**。
///
/// 服务端存的就是这一列（`Gateway.TokenHash`），于是**两边算的是同一个值**，
/// 控制面不需要另存任何可用于签名的密钥 —— 这也是选它而不是"再发一把私钥"的原因。
pub fn sign_key(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

/* ============================ 摘要 ============================ */

/// 一条聚合桶（主机名 / 状态码 / 拒绝原因 / 路由 / 侧）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub name: String,
    pub count: i64,
}

/// 一个上报窗口的摘要（**规范形**，见 `digest_core`）。
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// 控制面那侧的行 id（`GW-…`）。**只进摘要、不进上行 JSON** ——
    /// 控制面从令牌/路径就知道是哪个网关，让它再从 body 里读一遍只会多一处可造假的输入。
    pub gateway_id: String,
    pub seq: i64,
    pub window_start: String,
    pub window_end: String,
    pub requests: i64,
    pub allowed: i64,
    pub denied: i64,
    pub egress_bytes: i64,
    pub ingress_bytes: i64,
    pub latency_p50_ms: i64,
    pub latency_p95_ms: i64,
    pub latency_max_ms: i64,
    pub cache_hits: i64,
    pub top_domains: Vec<Bucket>,
    pub status_buckets: Vec<Bucket>,
    pub deny_reasons: Vec<Bucket>,
    pub routes: Vec<Bucket>,
    pub sides: Vec<Bucket>,
}

impl Summary {
    /// 排序规范形：计数降序、同数按名字升序。
    ///
    /// **必须**是确定性排序：并列时若顺序不定，同一份数据两次算出两个 digest，
    /// 服务端的幂等就失效了（会当成两个不同的窗口收下来）。
    fn normalize(&mut self) {
        fn sort_buckets(v: &mut Vec<Bucket>) {
            v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        }
        sort_buckets(&mut self.top_domains);
        sort_buckets(&mut self.status_buckets);
        sort_buckets(&mut self.deny_reasons);
        sort_buckets(&mut self.routes);
        sort_buckets(&mut self.sides);
        self.requests = self.requests.max(0);
        self.allowed = self.allowed.max(0);
        self.denied = self.denied.max(0);
        self.egress_bytes = self.egress_bytes.max(0);
        self.ingress_bytes = self.ingress_bytes.max(0);
        // 分位单调：p50 ≤ p95 ≤ max。
        self.latency_p95_ms = self.latency_p95_ms.max(self.latency_p50_ms);
        self.latency_max_ms = self.latency_max_ms.max(self.latency_p95_ms);
    }

    /// 规范字节：`key=len:value\n` 长度前缀（与 Go 侧 `GatewayAuditDigestCore` 逐字节一致）。
    ///
    /// 不用 JSON：键顺序、空格、浮点表示在两套语言里都不一样，摘要对不上就会被
    /// 服务端当成"被人改了"拒收。这条在 `ncc trace` 上已经吃过一次教训。
    pub fn digest_core(&self) -> String {
        // 长度前缀写入（`key=len:value\n`）。用自由函数而不是闭包：两个闭包会同时
        // 可变借用同一个 String，编译器不允许（而这里也没必要用闭包）。
        fn seg(out: &mut String, k: &str, v: &str) {
            out.push_str(k);
            out.push('=');
            out.push_str(&v.len().to_string());
            out.push(':');
            out.push_str(v);
            out.push('\n');
        }
        fn segi(out: &mut String, k: &str, v: i64) {
            seg(out, k, &v.to_string());
        }

        let mut s = String::new();
        seg(&mut s, "spec", AUDIT_SPEC);
        seg(&mut s, "gateway", &self.gateway_id);
        seg(&mut s, "windowStart", &self.window_start);
        seg(&mut s, "windowEnd", &self.window_end);
        segi(&mut s, "seq", self.seq);
        segi(&mut s, "requests", self.requests);
        segi(&mut s, "allowed", self.allowed);
        segi(&mut s, "denied", self.denied);
        segi(&mut s, "egressBytes", self.egress_bytes);
        segi(&mut s, "ingressBytes", self.ingress_bytes);
        segi(&mut s, "latencyP50Ms", self.latency_p50_ms);
        segi(&mut s, "latencyP95Ms", self.latency_p95_ms);
        segi(&mut s, "latencyMaxMs", self.latency_max_ms);
        segi(&mut s, "cacheHits", self.cache_hits);
        for (prefix, list) in [
            ("domains", &self.top_domains),
            ("status", &self.status_buckets),
            ("deny", &self.deny_reasons),
            ("routes", &self.routes),
            ("sides", &self.sides),
        ] {
            segi(&mut s, &format!("{prefix}.n"), list.len() as i64);
            for b in list {
                seg(&mut s, &format!("{prefix}.name"), &b.name);
                segi(&mut s, &format!("{prefix}.count"), b.count);
            }
        }
        s
    }

    pub fn digest(&self) -> String {
        format!("sha256:{}", to_hex(&Sha256::digest(self.digest_core().as_bytes())))
    }

    pub fn sign(&self, token: &str) -> String {
        to_hex(&hmac_sha256(sign_key(token).as_bytes(), self.digest_core().as_bytes()))
    }

    /// 上行 JSON（字段名与服务端 `GwAuditIngest` 对齐）。
    pub fn to_json(&self) -> Value {
        let buckets = |v: &[Bucket]| -> Value {
            Value::Array(
                v.iter()
                    .map(|b| json!({ "name": b.name, "count": b.count }))
                    .collect(),
            )
        };
        json!({
            "seq": self.seq,
            "windowStart": self.window_start,
            "windowEnd": self.window_end,
            "requests": self.requests,
            "allowed": self.allowed,
            "denied": self.denied,
            "egressBytes": self.egress_bytes,
            "ingressBytes": self.ingress_bytes,
            "latencyP50Ms": self.latency_p50_ms,
            "latencyP95Ms": self.latency_p95_ms,
            "latencyMaxMs": self.latency_max_ms,
            "cacheHits": self.cache_hits,
            "topDomains": buckets(&self.top_domains),
            "statusBuckets": buckets(&self.status_buckets),
            "denyReasons": buckets(&self.deny_reasons),
            "routes": buckets(&self.routes),
            "sides": buckets(&self.sides),
        })
    }

    /// 从上行 JSON 读回（待传队列落盘再读）。
    pub fn from_json(v: &Value) -> Result<Self> {
        let gs = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        let gstr = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let gb = |k: &str| -> Vec<Bucket> {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .map(|b| Bucket {
                            name: b.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            count: b.get("count").and_then(|x| x.as_i64()).unwrap_or(0),
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut s = Summary {
            gateway_id: gstr("gatewayId"),
            seq: gs("seq"),
            window_start: gstr("windowStart"),
            window_end: gstr("windowEnd"),
            requests: gs("requests"),
            allowed: gs("allowed"),
            denied: gs("denied"),
            egress_bytes: gs("egressBytes"),
            ingress_bytes: gs("ingressBytes"),
            latency_p50_ms: gs("latencyP50Ms"),
            latency_p95_ms: gs("latencyP95Ms"),
            latency_max_ms: gs("latencyMaxMs"),
            cache_hits: gs("cacheHits"),
            top_domains: gb("topDomains"),
            status_buckets: gb("statusBuckets"),
            deny_reasons: gb("denyReasons"),
            routes: gb("routes"),
            sides: gb("sides"),
            ..Default::default()
        };
        s.normalize();
        Ok(s)
    }

    /// 落盘进待传队列的形态：上行字段 + `gatewayId`（读回来要重建 digest）。
    fn to_state_json(&self) -> Value {
        let mut v = self.to_json();
        v["gatewayId"] = json!(self.gateway_id);
        v
    }

    /// 给人看的一行（`report --dry-run` 与 `--json` 之外的默认输出）。
    fn line(&self) -> String {
        format!(
            "{} → {}  请求 {}（放行 {} / 拒绝 {}）· 出站 {} / 入站 {} 字节 · p50 {}ms p95 {}ms max {}ms",
            &self.window_start[..self.window_start.len().min(19)],
            &self.window_end[..self.window_end.len().min(19)],
            self.requests,
            self.allowed,
            self.denied,
            human_bytes(self.egress_bytes),
            human_bytes(self.ingress_bytes),
            self.latency_p50_ms,
            self.latency_p95_ms,
            self.latency_max_ms,
        )
    }
}

pub fn human_bytes(n: i64) -> String {
    let f = n as f64;
    if f < 1024.0 {
        format!("{n} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", f / (1024.0 * 1024.0 * 1024.0))
    }
}

/* ============================ 本地账本（水位 + 待传队列） ============================ */

/// 本地上报账本。
///
/// `watermark` = 已经聚合进摘要的最大 `ts`（断点就是它，不重复计数）；
/// `pending` = 还没被控制面**收下**的摘要（落盘：控制面挂了也不会丢）。
#[derive(Debug, Clone, Default)]
pub struct ReportState {
    pub seq: i64,
    pub watermark: String,
    pub pending: Vec<Value>,
    /// 最近一次发送失败的原因（给人看，不参与逻辑）。
    pub last_error: String,
}

impl ReportState {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            // 账本坏了不该让网关起不来：从零开始，并且**说清楚**（宁可多报也不能少报）。
            eprintln!("⚠ 上报账本不是合法 JSON（{}）—— 从零开始记", path.display());
            return Self::default();
        };
        ReportState {
            seq: v.get("seq").and_then(|x| x.as_i64()).unwrap_or(0),
            watermark: v.get("watermark").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            pending: v.get("pending").and_then(|x| x.as_array()).cloned().unwrap_or_default(),
            last_error: v.get("lastError").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let v = json!({
            "spec": AUDIT_SPEC,
            "seq": self.seq,
            "watermark": self.watermark,
            "pending": self.pending,
            "lastError": self.last_error,
            "updatedAt": gateway::now_iso(),
        });
        std::fs::write(path, serde_json::to_string_pretty(&v)?)
            .with_context(|| format!("写不了上报账本 {}", path.display()))?;
        Ok(())
    }

    /// 读待传队列。
    ///
    /// `gateway_id` = 当前绑定：队列项一律按它对齐。理由：上行 JSON **故意不带**
    /// `gatewayId`（服务端按令牌判定归属），所以失败重排过的队列项会缺这一格；
    /// 不补回去，重算的 digest 就和服务端不一致 → 每次重传都被判「签名不匹配」，
    /// 队列会被自己毒死（踩过：断线补传全数被拒）。
    pub fn pending_summaries(&self, gateway_id: &str) -> Result<Vec<Summary>> {
        let mut out = Vec::with_capacity(self.pending.len());
        for v in &self.pending {
            let mut s = Summary::from_json(v)?;
            s.gateway_id = gateway_id.to_string();
            out.push(s);
        }
        Ok(out)
    }
}

/* ============================ 聚合 ============================ */

/// 从本地审计目录聚合出摘要。
///
/// 只**读**本地 JSONL（`path` 就在里面，但我们不上报它）。窗口按 `--window-minutes`
/// 切分，避免一个跨越一周的窗口（服务端也拒收 > 24h 的窗口）。
pub struct AggregateOpts {
    /// 从哪个时间点之后开始（不含）。空 = 从最早一条开始。
    pub after: String,
    /// 到哪个时间点为止（含）。空 = 现在。
    pub until: String,
    pub window_minutes: i64,
}

/// 聚合结果：(摘要列表, 新的水位, 读到的样本数, 跳过的坏行数)。
pub struct AggregateResult {
    pub summaries: Vec<Summary>,
    pub watermark: String,
    pub lines: usize,
    pub bad_lines: usize,
}

/// 一个审计样本（只取要用的字段；**故意不解析 path**）。
struct Sample {
    ts: String,
    side: String,
    route: String,
    upstream_host: String,
    decision: String,
    reason: String,
    status: i64,
    req_bytes: i64,
    resp_bytes: i64,
    ms: i64,
}

fn parse_sample(v: &Value) -> Option<Sample> {
    let ts = v.get("ts")?.as_str()?.to_string();
    // `upstream` 可能是完整 URL 或空。**只取主机名** —— 路径里有业务标识。
    let upstream = v.get("upstream").and_then(|x| x.as_str()).unwrap_or("");
    let host = host_of(upstream);
    Some(Sample {
        ts,
        side: v.get("side").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        route: v.get("route").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        upstream_host: host,
        decision: v.get("decision").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        reason: v.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        status: v.get("status").and_then(|x| x.as_i64()).unwrap_or(0),
        req_bytes: v.get("reqBytes").and_then(|x| x.as_i64()).unwrap_or(0),
        resp_bytes: v.get("respBytes").and_then(|x| x.as_i64()).unwrap_or(0),
        ms: v.get("ms").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

/// 从 URL / host:port 里取主机名（去掉 scheme、端口、路径）。
/// 从一串 URL / 上游地址里取**主机名**（只取主机：路径常带业务标识，绝不能上报）。
pub(crate) fn host_of(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    let rest = match s.split_once("://") {
        Some((_, r)) => r,
        None => s,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority);
    host.trim().to_string()
}

/// 最近秩分位（nearest-rank）：`sorted[ceil(p/100 × n) - 1]`。
///
/// 分位在**客户端**算：原始样本（每条多少毫秒）留在本地，上报的只有三个数。
fn percentile(sorted: &[i64], p: i64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len() as i64;
    let rank = (p * n + 99) / 100; // ceil
    let idx = rank.clamp(1, n) as usize - 1;
    sorted[idx]
}

fn bump(map: &mut BTreeMap<String, i64>, key: &str) {
    if key.is_empty() {
        return;
    }
    *map.entry(key.to_string()).or_insert(0) += 1;
}

fn buckets(map: BTreeMap<String, i64>) -> Vec<Bucket> {
    map.into_iter().map(|(name, count)| Bucket { name, count }).collect()
}

/// 把审计目录里的样本聚合成窗口摘要。
pub fn aggregate(
    audit_dir: &Path,
    gateway_id: &str,
    start_seq: i64,
    opts: &AggregateOpts,
) -> Result<AggregateResult> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(audit_dir)
        .with_context(|| format!("读不了审计目录 {}", audit_dir.display()))?
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("gateway-") && n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();

    let mut lines = 0usize;
    let mut bad_lines = 0usize;
    let mut samples: Vec<Sample> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).with_context(|| format!("读不了 {}", f.display()))?;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            lines += 1;
            match serde_json::from_str::<Value>(line) {
                Ok(v) => match parse_sample(&v) {
                    Some(s) => samples.push(s),
                    None => bad_lines += 1,
                },
                Err(_) => bad_lines += 1,
            }
        }
    }

    let after = if opts.after.is_empty() { String::new() } else { opts.after.clone() };
    let until = if opts.until.is_empty() { gateway::now_iso() } else { opts.until.clone() };
    // ts 是规范形（`…Z`，秒级），所以字符串比较就是时间比较 —— 不必引 chrono。
    samples.retain(|s| (after.is_empty() || s.ts.as_str() > after.as_str()) && s.ts.as_str() <= until.as_str());
    samples.sort_by(|a, b| a.ts.cmp(&b.ts));

    let window_secs = opts.window_minutes.max(1) * 60;
    let mut summaries: Vec<Summary> = Vec::new();
    let mut seq = start_seq;
    let mut watermark = after.clone();

    let mut i = 0usize;
    while i < samples.len() {
        let begin_ts = samples[i].ts.clone();
        let begin_secs = parse_iso_secs(&begin_ts).unwrap_or(0);
        let mut j = i;
        // 窗口边界：按时间（不超过 window_minutes），不是按条数。
        while j < samples.len() {
            let secs = parse_iso_secs(&samples[j].ts).unwrap_or(begin_secs);
            if j > i && secs - begin_secs >= window_secs {
                break;
            }
            j += 1;
        }
        let chunk = &samples[i..j];
        // 窗口的 end 取这一段的最后一个样本时间 —— 用"现在"会让同一个窗口
        // 两次算出不同的 digest（幂等就没了）。
        let end_ts = chunk.last().map(|s| s.ts.clone()).unwrap_or(begin_ts.clone());
        seq += 1;
        let mut sum = Summary {
            gateway_id: gateway_id.to_string(),
            seq,
            window_start: begin_ts,
            window_end: end_ts.clone(),
            ..Default::default()
        };
        let mut domains: BTreeMap<String, i64> = BTreeMap::new();
        let mut statuses: BTreeMap<String, i64> = BTreeMap::new();
        let mut reasons: BTreeMap<String, i64> = BTreeMap::new();
        let mut routes: BTreeMap<String, i64> = BTreeMap::new();
        let mut sides: BTreeMap<String, i64> = BTreeMap::new();
        let mut latencies: Vec<i64> = Vec::with_capacity(chunk.len());
        for s in chunk {
            sum.requests += 1;
            if s.decision == "allow" {
                sum.allowed += 1;
            } else {
                sum.denied += 1;
                bump(&mut reasons, &s.reason);
            }
            // 出站 = 我们发出去的（accept：转发给上游；forward：发给 peer）
            // 入站 = 我们收回来的（回给调用方的那份）
            sum.egress_bytes += s.req_bytes.max(0);
            sum.ingress_bytes += s.resp_bytes.max(0);
            bump(&mut domains, &s.upstream_host);
            bump(&mut statuses, &s.status.to_string());
            bump(&mut routes, &s.route);
            bump(&mut sides, &s.side);
            latencies.push(s.ms.max(0));
        }
        latencies.sort_unstable();
        sum.latency_p50_ms = percentile(&latencies, 50);
        sum.latency_p95_ms = percentile(&latencies, 95);
        sum.latency_max_ms = *latencies.last().unwrap_or(&0);
        sum.top_domains = buckets(domains);
        sum.status_buckets = buckets(statuses);
        sum.deny_reasons = buckets(reasons);
        sum.routes = buckets(routes);
        sum.sides = buckets(sides);
        sum.normalize();
        watermark = end_ts;
        summaries.push(sum);
        i = j;
    }

    Ok(AggregateResult { summaries, watermark, lines, bad_lines })
}

/// 解析 `…Z` 到 Unix 秒（聚合只用它比较窗口边界）。
fn parse_iso_secs(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let n = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    // 年月日 → Unix 天（Howard Hinnant 的 civil→days）
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + sec)
}

/* ============================ 与网关配置对接 ============================ */

/// 拿到网关配置文件里与控制面相关的字段。
pub fn binding(cfg: &gateway::GatewayConfig) -> Result<(String, String)> {
    let token = cfg.gateway_token.trim().to_string();
    let api_server = cfg.api_server.trim().to_string();
    if token.is_empty() || api_server.is_empty() {
        bail!(
            "这个网关还没绑定控制面（{} 里缺 gateway_token / api_server）。\n\
             先跑 `ncc gateway bind --namespace @你的组织` —— 绑定后审计摘要才会上报；\n\
             不绑定也能跑：审计照落本地（只是控制面看不到摘要）。",
            gateway::config_path().display()
        );
    }
    Ok((api_server, token))
}

/// 用显式 base 发请求（不改用户当前 target —— 网关绑定的控制面与"我平时用哪个 ncc"是两件事）。
fn client_at(cfg: &CliConfig, base: &str) -> CliConfig {
    let mut c = cfg.clone();
    c.target_mut().base_url = base.trim_end_matches('/').to_string();
    c
}

/* ============================ 子命令 ============================ */

/// `ncc gateway bind`：注册到控制面并把令牌写进 gateway.json。
pub struct BindArgs {
    pub namespace: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub json: bool,
}

pub fn bind(cfg: &CliConfig, a: &BindArgs) -> Result<()> {
    let tok = config::require_token(cfg)?;
    let mut gwcfg = gateway::load_config_pub()?;
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| {
            // 缺省用主机名：网关是"这台机器上的那个出口"，主机名最直观。
            std::env::var("HOSTNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "gateway".to_string())
        });
    let body = json!({
        "name": name,
        "namespace": a.namespace.clone().unwrap_or_default(),
        "version": a.version.clone().unwrap_or_else(|| format!("ncc/{}", env!("CARGO_PKG_VERSION"))),
    });
    let d = api::post_json(cfg, "/api/gateways", Some(&tok), &body)?;
    let id = d.pointer("/gateway/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let token = d.get("token").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if id.is_empty() || token.is_empty() {
        bail!("控制面没回网关 id 或令牌：{d}");
    }
    let ns = d.pointer("/gateway/namespace").and_then(|x| x.as_str()).unwrap_or("").to_string();

    gwcfg.gateway_token = token.clone();
    gwcfg.gateway_id = id.clone();
    gwcfg.api_server = cfg.base_url();
    gwcfg.namespace = ns.clone();
    gwcfg.name = name.clone();
    gateway::save_config_pub(&gwcfg)?;
    // 换绑 = 换身份：旧队列里那些窗口算的 digest 绑定的是旧网关，留着只会被拒。
    // 清掉水位后本地审计会被重新聚合一遍（内容不变 → 服务端按 digest 判重复，不双计）。
    let _ = std::fs::remove_file(state_path());

    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "gatewayId": id, "namespace": ns, "name": name,
            "apiServer": cfg.base_url(),
            "config": gateway::config_path().display().to_string(),
            "tokenShownOnce": true,
        }))?);
        return Ok(());
    }
    println!("✅ 已绑定控制面：{}  {}", id, name);
    println!("   命名空间 @{ns} · 控制面 {}", cfg.base_url());
    println!("   令牌已写进 {}（库里只存哈希：丢了就 `ncc gateway unbind` 再 bind）",
        gateway::config_path().display());
    println!("   留存：{}", d.pointer("/scope/retention").and_then(|x| x.as_str()).unwrap_or("按计划"));
    println!("   上报账本已清空（换绑就是换身份；本地审计文件没动）");
    println!("\n下一步：`ncc gateway run` 会自动心跳并周期上报摘要；或手动 `ncc gateway report`。");
    Ok(())
}

/// `ncc gateway heartbeat`
pub struct HeartbeatArgs {
    pub status: Option<String>,
    pub json: bool,
}

pub fn heartbeat(cfg: &CliConfig, a: &HeartbeatArgs) -> Result<()> {
    let gwcfg = gateway::load_config_pub()?;
    let (base, token) = binding(&gwcfg)?;
    // 不给 --status 就是最普通的心跳：语义是 **online**（"我还在，能派活"）。
    // 不显式发 online 的话，一次 draining 之后这台网关会永远停在 draining 上。
    let body = json!({
        "status": a.status.clone().unwrap_or_else(|| "online".to_string()),
        "version": format!("ncc/{}", env!("CARGO_PKG_VERSION")),
    });
    let c = client_at(cfg, &base);
    let d = api::post_json(
        &c,
        &format!("/api/gateways/{}/heartbeat", gwcfg.gateway_id),
        Some(&token),
        &body,
    )?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!(
        "✅ 心跳已送达  {}（服务端 {}）",
        gwcfg.gateway_id,
        d.get("serverTime").and_then(|x| x.as_str()).unwrap_or("-")
    );
    println!(
        "   建议间隔 {}s · 留存 {}h · 已收窗口 {}",
        d.get("heartbeatSec").and_then(|x| x.as_i64()).unwrap_or(0),
        d.get("retentionHours").and_then(|x| x.as_i64()).unwrap_or(0),
        d.get("auditSeq").and_then(|x| x.as_i64()).unwrap_or(0),
    );
    if let Some(hint) = d.get("policyHint").and_then(|x| x.as_str()) {
        println!("   策略：{hint}");
    }
    Ok(())
}

/// `ncc gateway report`
pub struct ReportArgs {
    pub since: Option<String>,
    pub window_minutes: Option<i64>,
    pub dry_run: bool,
    pub drop_rejected: bool,
    pub json: bool,
}

/// 上报结果（常驻模式也用它，所以要能返回数字）。
#[derive(Default)]
pub struct ReportOutcome {
    pub built: usize,
    /// 这次读了多少行本地审计、其中多少行解析不了（坏行要说出来，不许静默跳过）。
    pub lines: usize,
    pub bad_lines: usize,
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
    pub pending: usize,
    pub requests: i64,
    /// 最近一次发送失败的原因：**必须**说出来，否则操作者会以为一切正常。
    pub last_error: String,
}

pub fn report(cfg: &CliConfig, a: &ReportArgs) -> Result<()> {
    let out = report_once(cfg, a)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "built": out.built, "accepted": out.accepted, "duplicates": out.duplicates,
            "rejected": out.rejected, "pending": out.pending, "requests": out.requests,
            "lines": out.lines, "badLines": out.bad_lines,
            "lastError": out.last_error,
        }))?);
        return Ok(());
    }
    if out.bad_lines > 0 {
        println!(
            "⚠ 本地审计里有 {} 行解析不了（共 {} 行）—— 那些行没进摘要，去 `ncc gateway audit` 看看",
            out.bad_lines, out.lines
        );
    }
    if !out.last_error.is_empty() {
        println!("⚠ 上报失败：{}", out.last_error);
        println!(
            "   没丢：{} 个窗口还在待传队列里 —— 控制面恢复后 `ncc gateway report` 补传；\n\
             若这台网关已被吊销/注销，先 `ncc gateway unbind` 再 bind（本地审计与队列都不会丢）",
            out.pending
        );
    }
    if out.built == 0 && out.accepted == 0 && out.duplicates == 0 {
        if out.last_error.is_empty() {
            println!("没有新的审计窗口（本地审计里没有比水位更新的记录）");
        }
    } else {
        println!(
            "✅ 上报完成：新聚合 {} 个窗口 · 收下 {} · 重复 {} · 被拒 {} · 待传 {}",
            out.built, out.accepted, out.duplicates, out.rejected, out.pending
        );
        println!("   窗口内共 {} 次请求", out.requests);
    }
    Ok(())
}

/// 上报一次（给 CLI 与常驻线程共用）。
pub fn report_once(cfg: &CliConfig, a: &ReportArgs) -> Result<ReportOutcome> {
    let gwcfg = gateway::load_config_pub()?;
    let (base, token) = binding(&gwcfg)?;
    let audit_dir = gateway::audit_dir_of(&gwcfg);
    let state_file = state_path();
    let mut st = ReportState::load(&state_file);

    let after = match &a.since {
        Some(s) => s.clone(),
        None => st.watermark.clone(),
    };
    let opts = AggregateOpts {
        after: after.clone(),
        until: String::new(),
        window_minutes: a.window_minutes.unwrap_or(DEFAULT_WINDOW_MINUTES),
    };
    let agg = aggregate(&audit_dir, &gwcfg.gateway_id, st.seq, &opts)?;

    if !agg.summaries.is_empty() {
        if a.dry_run {
            if a.json {
                let list: Vec<Value> = agg.summaries.iter().map(|s| s.to_state_json()).collect();
                println!("{}", serde_json::to_string_pretty(&json!({ "summaries": list }))?);
            } else {
                println!("（--dry-run）会聚合出 {} 个窗口，不发送：", agg.summaries.len());
                for s in &agg.summaries {
                    println!("  {}", s.line());
                }
                println!("  规范摘要 {}", agg.summaries[0].digest());
            }
            return Ok(ReportOutcome {
                built: agg.summaries.len(),
                lines: agg.lines,
                bad_lines: agg.bad_lines,
                ..Default::default()
            });
        }
        // 先落盘再发送：控制面挂了也不会丢（F3：断线不停工，恢复后补传）。
        for s in &agg.summaries {
            st.pending.push(s.to_state_json());
        }
        st.seq = agg.summaries.last().map(|s| s.seq).unwrap_or(st.seq);
        st.watermark = agg.watermark.clone();
        st.save(&state_file)?;
    }

    let mut outcome = ReportOutcome {
        built: agg.summaries.len(),
        lines: agg.lines,
        bad_lines: agg.bad_lines,
        ..Default::default()
    };
    if st.pending.is_empty() {
        return Ok(outcome);
    }

    let pending = st.pending_summaries(&gwcfg.gateway_id)?;
    let c = client_at(cfg, &base);
    // 先确保每一个窗口都签上名（密钥材料 = sha256(令牌)）。
    let signed: Vec<Value> = pending
        .iter()
        .map(|s| {
            let mut v = s.to_json();
            v["sig"] = json!(s.sign(&token));
            v
        })
        .collect();

    let mut keep: Vec<Value> = Vec::new();
    for chunk in signed.chunks(BATCH_MAX) {
        let body = json!({ "summaries": chunk });
        let resp = api::post_json(
            &c,
            &format!("/api/gateways/{}/audit", gwcfg.gateway_id),
            Some(&token),
            &body,
        );
        match resp {
            Ok(d) => {
                outcome.accepted += d.get("accepted").and_then(|x| x.as_i64()).unwrap_or(0) as usize;
                outcome.duplicates += d.get("duplicates").and_then(|x| x.as_i64()).unwrap_or(0) as usize;
                outcome.rejected += d.get("rejected").and_then(|x| x.as_i64()).unwrap_or(0) as usize;
                // 逐条看结果：accepted / duplicate 都算"控制面收下了"，从队列里去掉；
                // rejected **留在队列里**并报出来 —— 不静默丢审计数据。
                let results = d.get("results").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                for (i, chunk_item) in chunk.iter().enumerate() {
                    let status = results
                        .get(i)
                        .and_then(|r| r.get("status"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    if status == "accepted" || status == "duplicate" {
                        continue;
                    }
                    let reason = results
                        .get(i)
                        .and_then(|r| r.get("reason"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("未知原因");
                    if a.drop_rejected {
                        eprintln!("⚠ 丢弃被拒的摘要（--drop-rejected）：{reason}");
                    } else {
                        eprintln!("⚠ 摘要被拒（留在待传队列里）：{reason}");
                        keep.push(chunk_item.clone());
                    }
                }
                st.last_error.clear();
            }
            Err(e) => {
                // 控制面不可达：**什么都不丢**，整批留在队列里。
                st.last_error = format!("{e:#}");
                keep.extend(chunk.iter().cloned());
            }
        }
    }
    // 保留仍然是 JSON（**含 gatewayId**）的形态 —— 队列项必须是自洽的，
    // 读回来直接就能重算 digest（漏了这一格就会"签名不匹配"，见 pending_summaries）。
    let kept: Vec<Value> = keep
        .iter()
        .map(|v| {
            let mut s = Summary::from_json(v).unwrap_or_default();
            if s.gateway_id.is_empty() {
                s.gateway_id = gwcfg.gateway_id.clone();
            }
            s.to_state_json()
        })
        .collect();
    st.pending = kept;
    outcome.pending = st.pending.len();
    st.save(&state_file)?;
    outcome.requests = pending.iter().map(|s| s.requests).sum();
    outcome.last_error = st.last_error.clone();
    Ok(outcome)
}

/// `ncc gateway audit --remote`：看控制面留存的摘要（本地模式见 `gateway::audit_cmd`）。
pub struct RemoteAuditArgs {
    pub since: Option<String>,
    pub limit: i64,
    pub csv: bool,
    pub json: bool,
}

pub fn audit_remote(cfg: &CliConfig, a: &RemoteAuditArgs) -> Result<()> {
    let tok = config::require_token(cfg)?;
    let gwcfg = gateway::load_config_pub()?;
    let id = gateway_id_of(&gwcfg)?;
    let mut qs: Vec<String> = vec![format!("limit={}", a.limit.clamp(1, 5000))];
    if let Some(s) = &a.since {
        qs.push(format!("since={}", api::urlenc(s)));
    }
    if a.csv {
        // 服务端按 `format=csv` 出 CSV（合规导出）；忘了带这个参数就会拿到 JSON。
        qs.push("format=csv".into());
    }
    let path = format!("/api/gateways/{}/audit?{}", id, qs.join("&"));
    if a.csv {
        let (bytes, _) = api::get_bytes_with_headers(cfg, &path, Some(&tok))?;
        print!("{}", String::from_utf8_lossy(&bytes));
        return Ok(());
    }
    let d = api::get(cfg, &path, Some(&tok))?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let rows = d.get("summaries").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("（控制面没有留存的摘要）");
    }
    for r in &rows {
        let gs = |k: &str| r.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        println!(
            "#{}  {} → {}  请求 {}（放行 {} / 拒绝 {}）· 出站 {} / 入站 {}",
            gs("seq"),
            r.get("windowStart").and_then(|x| x.as_str()).unwrap_or(""),
            r.get("windowEnd").and_then(|x| x.as_str()).unwrap_or(""),
            gs("requests"),
            gs("allowed"),
            gs("denied"),
            human_bytes(gs("egressBytes")),
            human_bytes(gs("ingressBytes")),
        );
        let buckets = |k: &str| -> String {
            r.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .map(|b| {
                            format!(
                                "{}={}",
                                b.get("name").and_then(|x| x.as_str()).unwrap_or("?"),
                                b.get("count").and_then(|x| x.as_i64()).unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        for (label, key) in [("目标域", "topDomains"), ("状态码", "statusBuckets"), ("拒绝原因", "denyReasons")] {
            let v = buckets(key);
            if !v.is_empty() {
                println!("    {label} {v}");
            }
        }
        println!(
            "    摘要 {}",
            r.get("digest").and_then(|x| x.as_str()).unwrap_or("")
        );
    }
    println!(
        "\n共 {} 条（留存 {}h）· 本地逐条审计比这里细（含路径）：`ncc gateway audit`",
        d.get("total").and_then(|x| x.as_i64()).unwrap_or(rows.len() as i64),
        d.get("retentionHours").and_then(|x| x.as_i64()).unwrap_or(0)
    );
    Ok(())
}

/// `ncc gateway usage`
pub fn usage(cfg: &CliConfig, json_out: bool) -> Result<()> {
    let tok = config::require_token(cfg)?;
    let gwcfg = gateway::load_config_pub()?;
    let id = gateway_id_of(&gwcfg)?;
    let d = api::get(cfg, &format!("/api/gateways/{id}/usage"), Some(&tok))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    let gs = |k: &str| d.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    println!("用量（{}）", id);
    println!(
        "  窗口 {} 个 · 活跃 {} 天 · 请求 {}（放行 {} / 拒绝 {}）",
        gs("windows"), gs("days"), gs("requests"), gs("allowed"), gs("denied")
    );
    println!("  出站 {}", human_bytes(gs("egressBytes")));
    println!(
        "  范围 {} → {}",
        d.get("firstWindow").and_then(|x| x.as_str()).unwrap_or("-"),
        d.get("lastWindow").and_then(|x| x.as_str()).unwrap_or("-")
    );
    println!("\n口径：{}", d.get("basis").and_then(|x| x.as_str()).unwrap_or(""));
    Ok(())
}

/// `ncc gateway unbind`
pub fn unbind(cfg: &CliConfig, keep_audit: bool, json_out: bool) -> Result<()> {
    let tok = config::require_token(cfg)?;
    let mut gwcfg = gateway::load_config_pub()?;
    let id = gateway_id_of(&gwcfg)?;
    let d = api::del(cfg, &format!("/api/gateways/{id}"), Some(&tok));
    let retained = match &d {
        Ok(v) => v.get("retainedSummaries").and_then(|x| x.as_i64()).unwrap_or(0),
        Err(_) => 0,
    };
    // 本地清掉绑定（审计文件不动 —— 那是客户自己的数据）。
    gwcfg.gateway_token.clear();
    gwcfg.gateway_id.clear();
    gateway::save_config_pub(&gwcfg)?;
    let st_path = state_path();
    if !keep_audit {
        let _ = std::fs::remove_file(&st_path);
    }
    match d {
        Ok(_) => {
            if json_out {
                println!("{}", serde_json::to_string_pretty(&json!({
                    "ok": true, "gatewayId": id, "retainedSummaries": retained,
                    "localAuditKept": true,
                }))?);
            } else {
                println!("✅ 已注销 {id}（凭据立即失效）");
                println!("   控制面留下的摘要 {retained} 条（注销 ≠ 抹掉合规记录）");
                println!("   本地审计文件**没动**：那是你自己的数据");
            }
        }
        Err(e) => {
            // 控制面不可达时也要能解绑本地（否则这台机器再也报不上去也清不掉）。
            eprintln!("⚠ 控制面没删掉（{e:#}）—— 本地绑定已清；恢复后请重试或让管理员吊销");
            println!("✅ 本地已解绑（控制面上的记录可能还在）");
        }
    }
    Ok(())
}

fn gateway_id_of(cfg: &gateway::GatewayConfig) -> Result<String> {
    let id = cfg.gateway_id.trim().to_string();
    if id.is_empty() {
        bail!(
            "这个网关还没绑定控制面（{} 里缺 gateway_id）—— 先 `ncc gateway bind`",
            gateway::config_path().display()
        );
    }
    Ok(id)
}

/* ============================ 常驻上报线程 ============================ */

/// 在 `ncc gateway run` 里起一个后台线程：周期心跳 + 周期上报。
///
/// 返回句柄，调用方可以不管它（进程退出即结束）。**失败只打日志**：
/// 控制面不可达绝不能让本地网关停下来（F3 的断线策略）。
pub fn spawn(cfg: CliConfig, heartbeat_sec: u64, report_sec: u64) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last_report = std::time::Instant::now()
            - std::time::Duration::from_secs(report_sec.max(1)); // 启动先报一次（把积压的补传出去）
        let mut last_beat = std::time::Instant::now()
            - std::time::Duration::from_secs(heartbeat_sec.max(1));
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if heartbeat_sec > 0 && last_beat.elapsed().as_secs() >= heartbeat_sec {
                last_beat = std::time::Instant::now();
                let a = HeartbeatArgs { status: None, json: false };
                if let Err(e) = heartbeat(&cfg, &a) {
                    eprintln!("⚠ 心跳失败（本地不受影响）：{e:#}");
                }
            }
            if report_sec > 0 && last_report.elapsed().as_secs() >= report_sec {
                last_report = std::time::Instant::now();
                let a = ReportArgs {
                    since: None,
                    window_minutes: None,
                    dry_run: false,
                    drop_rejected: false,
                    json: false,
                };
                match report_once(&cfg, &a) {
                    Ok(o) if o.built > 0 || o.accepted > 0 || o.duplicates > 0 => {
                        println!(
                            "📤 审计摘要已上报：新窗口 {} · 收下 {} · 重复 {} · 待传 {}",
                            o.built, o.accepted, o.duplicates, o.pending
                        );
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("⚠ 摘要上报失败（审计照落本地，恢复后补传）：{e:#}"),
                }
            }
        }
    })
}

/// 待传队列里所有窗口涉及的本地审计行数上限提示（给人看的）。
pub fn pending_hint() -> String {
    let st = ReportState::load(&state_path());
    if st.pending.is_empty() && st.last_error.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    if !st.pending.is_empty() {
        s.push_str(&format!("{} 个窗口待传", st.pending.len()));
    }
    if !st.last_error.is_empty() {
        if !s.is_empty() {
            s.push_str(" · ");
        }
        s.push_str(&format!("上次错误：{}", st.last_error));
    }
    s
}

/* ============================ 测试 ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    /// 待传队列是「落盘 → 读回 → 再签名」，所以**读回来必须逐字节等价**：
    /// 否则断线补传的那一批会被服务端当成签名不匹配而全部拒收（踩过）。
    #[test]
    fn pending_queue_round_trip_is_digest_identical() {
        let s = fixture();
        let back = Summary::from_json(&s.to_state_json()).expect("读回待传摘要");
        assert_eq!(s.digest_core(), back.digest_core(), "回读后规范字节变了");
        assert_eq!(s.digest(), back.digest());
        assert_eq!(s.sign("tok"), back.sign("tok"));
    }

    /// 与 Go 侧 `model/gateway_test.go` **同一个固定样本**：两边算出同一个 digest
    /// 与同一个 HMAC。改任何一边的编码规则，两边测试一起红。
    fn fixture() -> Summary {
        let mut s = Summary {
            gateway_id: "GW-test".into(),
            seq: 3,
            window_start: "2026-09-26T10:00:00Z".into(),
            window_end: "2026-09-26T10:05:00Z".into(),
            requests: 12,
            allowed: 10,
            denied: 2,
            egress_bytes: 4096,
            ingress_bytes: 8192,
            latency_p50_ms: 12,
            latency_p95_ms: 40,
            latency_max_ms: 90,
            // 故意乱序 + 带并列计数：钉住"先排序再进摘要"这条规范形。
            top_domains: vec![
                Bucket { name: "example.com".into(), count: 2 },
                Bucket { name: "api.openai.com".into(), count: 7 },
            ],
            status_buckets: vec![
                Bucket { name: "502".into(), count: 2 },
                Bucket { name: "200".into(), count: 8 },
                Bucket { name: "403".into(), count: 2 },
            ],
            deny_reasons: vec![
                Bucket { name: "token_invalid".into(), count: 1 },
                Bucket { name: "path_not_allowed".into(), count: 1 },
            ],
            routes: vec![
                Bucket { name: "docs".into(), count: 3 },
                Bucket { name: "llm".into(), count: 9 },
            ],
            sides: vec![Bucket { name: "accept".into(), count: 12 }],
            ..Default::default()
        };
        s.normalize();
        s
    }

    const FIXTURE_DIGEST: &str =
        "sha256:74fc891c761b76b1ba63e0e8dfdeff69419a7b5422fec7ccadde0095f2b14295";
    const FIXTURE_TOKEN: &str = "ncc_gw_test_token";
    const FIXTURE_KEY: &str = "330760b435a8e91163b1a593d46517a135b44c662df7b74916fb2419bc688ae5";
    const FIXTURE_SIG: &str = "935bbc69c4c6282ff292be35164bbd507b6f4495631d1bf58c722ff8c9ac244d";

    #[test]
    fn digest_matches_go_vector() {
        let s = fixture();
        assert_eq!(s.digest(), FIXTURE_DIGEST);
        // 规范字节本身也钉住：只钉哈希的话，出错时看不出是哪一段变了。
        let core = s.digest_core();
        assert!(core.starts_with("spec=20:ncc-gateway-audit/v1\n"), "{core}");
        assert!(core.contains("gateway=7:GW-test\n"), "{core}");
        assert!(core.contains("status.name=3:403\nstatus.count=1:2\n"), "{core}");
        assert!(
            core.ends_with("sides.name=6:accept\nsides.count=2:12\n"),
            "{core}"
        );
    }

    #[test]
    fn signature_and_key_match_go_vector() {
        let s = fixture();
        // 密钥材料 = sha256(令牌) 的十六进制文本（服务端存的就是这一列）。
        assert_eq!(sign_key(FIXTURE_TOKEN), FIXTURE_KEY);
        assert_eq!(s.sign(FIXTURE_TOKEN), FIXTURE_SIG);
        // 令牌不同 → 签名不同（否则"签名"与身份无关，等于没签）。
        assert_ne!(s.sign("ncc_gw_other_token"), FIXTURE_SIG);
        // 内容变一点（多一次拒绝）→ 签名必须失效。
        let mut t = fixture();
        t.denied = 3;
        t.requests = 13;
        t.normalize();
        assert_ne!(t.sign(FIXTURE_TOKEN), FIXTURE_SIG);
    }

    #[test]
    fn hmac_matches_rfc4231_vectors() {
        // RFC 4231 的四个官方用例（自己手写 HMAC 就得拿标准向量钉住）。
        let k = [0x0bu8; 20];
        assert_eq!(
            to_hex(&hmac_sha256(&k, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            to_hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let k2 = [0xaau8; 20];
        let data = vec![0xddu8; 50];
        assert_eq!(
            to_hex(&hmac_sha256(&k2, &data)),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        // 长密钥（>64 字节）要先哈希：这一支最容易写错。
        let k3 = vec![0xaau8; 131];
        let data3 = b"Test Using Larger Than Block-Size Key - Hash Key First";
        assert_eq!(
            to_hex(&hmac_sha256(&k3, data3)),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn bucket_order_is_deterministic_only_after_normalize() {
        // 同一份数据、两种输入顺序 → normalize 之后必须得到完全一样的 digest
        // （否则服务端幂等失效：同一个窗口会被当成两个收下来）。
        let mut a = fixture();
        let mut b = fixture();
        b.top_domains.reverse();
        b.status_buckets.reverse();
        b.deny_reasons.reverse();
        a.normalize();
        b.normalize();
        assert_eq!(a.digest_core(), b.digest_core());
        // 排序规则：计数降序、同数按名字升序（200 在 403/502 之前）。
        assert_eq!(a.status_buckets[0].name, "200");
        assert_eq!(a.status_buckets[1].name, "403");
        assert_eq!(a.status_buckets[2].name, "502");
        assert_eq!(a.deny_reasons[0].name, "path_not_allowed");
    }

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        // 只上报**主机名**：路径里常有订单号 / 用户 id，传上去等于传业务数据。
        assert_eq!(host_of("https://api.openai.com/v1/chat/completions"), "api.openai.com");
        assert_eq!(host_of("http://127.0.0.1:9811/v1/llm"), "127.0.0.1");
        assert_eq!(host_of("example.com:8443/x?y=1"), "example.com");
        assert_eq!(host_of(""), "");
        assert_eq!(host_of("not a url/with space"), "not a url");
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let v = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&v, 50), 5);
        assert_eq!(percentile(&v, 95), 10);
        assert_eq!(percentile(&v, 100), 10);
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[7], 95), 7);
    }

    #[test]
    fn iso_secs_round_trips_with_iso_from_secs() {
        for secs in [0i64, 1_774_000_000, 1_788_600_000, 2_000_000_000] {
            let iso = crate::gateway::iso_from_secs(secs);
            assert_eq!(parse_iso_secs(&iso), Some(secs), "{iso}");
        }
        assert_eq!(parse_iso_secs("乱写"), None);
    }

    #[test]
    fn aggregate_splits_windows_and_keeps_paths_out() {
        let dir = std::env::temp_dir().join(format!("ncc-gw-agg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gateway-2026-09-26.jsonl");
        let mut lines = String::new();
        // 10:00:05 放行（1 次拒绝、1 次放行），10:00:20 放行；10:05:00 拒绝。
        for (ts, decision, reason, status, side, upstream, ms) in [
            ("2026-09-26T10:00:05Z", "allow", "", 200, "accept", "https://api.openai.com/v1/chat/completions", 30),
            ("2026-09-26T10:00:06Z", "deny", "path_not_allowed", 403, "accept", "", 1),
            ("2026-09-26T10:00:20Z", "allow", "", 200, "accept", "https://api.openai.com/v1/chat/completions", 60),
            ("2026-09-26T10:05:00Z", "deny", "token_invalid", 401, "forward", "", 2),
        ] {
            lines.push_str(&format!(
                "{{\"ts\":\"{ts}\",\"reqId\":\"r\",\"side\":\"{side}\",\"route\":\"llm\",\"mode\":\"{side}\",\"peer\":\"\",\"method\":\"POST\",\"path\":\"/v1/llm/chat/completions\",\"upstream\":\"{upstream}\",\"decision\":\"{decision}\",\"reason\":\"{reason}\",\"status\":{status},\"reqBytes\":100,\"respBytes\":200,\"ms\":{ms}}}\n"
            ));
        }
        // 一行坏数据（审计文件被人手动改过 / 写到一半断电）。
        lines.push_str("{不是 JSON}\n");
        std::fs::write(&file, lines).unwrap();

        let opts = AggregateOpts { after: String::new(), until: String::new(), window_minutes: 1 };
        let r = aggregate(&dir, "GW-test", 0, &opts).unwrap();
        assert_eq!(r.bad_lines, 1, "坏行要数出来（不能静默跳过）");
        assert_eq!(r.summaries.len(), 2, "10:00 与 10:05 应当分成两个窗口");
        let first = &r.summaries[0];
        assert_eq!(first.seq, 1);
        assert_eq!(first.requests, 3);
        assert_eq!(first.allowed, 2);
        assert_eq!(first.denied, 1);
        assert_eq!(first.window_start, "2026-09-26T10:00:05Z");
        assert_eq!(first.window_end, "2026-09-26T10:00:20Z");
        assert_eq!(first.latency_p50_ms, 30);
        assert_eq!(first.latency_max_ms, 60);
        assert_eq!(first.top_domains.len(), 1, "只有主机名，且相同主机合并");
        assert_eq!(first.top_domains[0].name, "api.openai.com");
        assert_eq!(first.top_domains[0].count, 2);
        assert_eq!(first.deny_reasons[0].name, "path_not_allowed");
        assert_eq!(first.sides[0].name, "accept");
        // **路径绝不进摘要**：规范字节里只该有主机名。
        assert!(!first.digest_core().contains("/v1/"), "{}", first.digest_core());
        // 水位推进到最后一个窗口的结束时间。
        assert_eq!(r.watermark, "2026-09-26T10:05:00Z");
        // 再聚合一次（水位之后）：没有新样本 → 不产生窗口（幂等的基础）。
        let opts2 = AggregateOpts {
            after: r.watermark.clone(),
            until: String::new(),
            window_minutes: 1,
        };
        let r2 = aggregate(&dir, "GW-test", r.summaries.len() as i64, &opts2).unwrap();
        assert!(r2.summaries.is_empty(), "水位之后不该重复计数");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn summary_round_trips_through_state_json() {
        let s = fixture();
        let v = s.to_state_json();
        assert_eq!(v["gatewayId"], "GW-test");
        // 上行 JSON **不带** gatewayId（控制面从令牌知道是谁，少一处可造假的输入）。
        assert!(s.to_json().get("gatewayId").is_none());
        let back = Summary::from_json(&v).unwrap();
        assert_eq!(back.gateway_id, s.gateway_id);
        assert_eq!(back.digest_core(), s.digest_core());
        assert_eq!(back.sign(FIXTURE_TOKEN), s.sign(FIXTURE_TOKEN));
    }
}
