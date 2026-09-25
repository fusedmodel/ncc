//! `hur-sandbox` —— hur 的 **L1 执行引擎**（PRD §12.4 / §13.4）。
//!
//! 只做一件事：把策略算出来的限额**如实执行**。它不认识 hur 的包结构，只吃
//! 「入口字节 + 限额 + 宿主能力桥」三样东西 —— 于是它可以被单测（用 WAT 构造恶意 guest）。
//!
//! ## 执行保证（每条都有对应测试）
//! - **指令预算**：`Config::consume_fuel` + `Store::set_fuel` → 死循环必然中断；
//! - **墙钟预算**：`epoch_interruption` + 看门狗 `increment_epoch()` + `set_epoch_deadline`
//!   → guest 无法绕过（`memory.copy` 类整块指令的超时窗口由内存上限兜住）；
//! - **内存上限**：`StoreLimits`（线性内存 / 实例 / 表）→ 内存炸弹是 OOM 而不是拖垮宿主；
//! - **栈上限**：`Config::max_wasm_stack` → 深递归是 trap 而不是打穿宿主栈；
//! - **能力默认全关**：guest **没有** WASI、没有 fd、没有 socket、没有文件系统；
//!   联网只能经 `hur.http_get`，且必须命中 `permissions.network` 白名单、受调用次数上限约束；
//! - **输出上限**：`hur.reply` 按 `max_output_chars` 截断并标记。
//!
//! ## Guest ABI（最小、可手写）
//! 导出：`memory`（必填）· `hur_alloc(len: i32) -> i32`（必填）· `hur_run(ptr: i32, len: i32) -> i32`（必填，0 = 成功）
//! 导入：`hur.log(ptr,len)` · `hur.reply(ptr,len)` · `hur.tool(name_ptr,name_len,arg_ptr,arg_len) -> i32`
//!      · `hur.http_get(url_ptr,url_len) -> i32`（返回响应在 guest 内存里的指针，0 = 空）

mod allow;

use anyhow::{bail, Context, Result};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::{Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder};

/// 本 crate 提供的引擎名（宿主用它填 `policy::set_available_engines`）
pub const ENGINES: &[&str] = &["wasm"];

/// 看门狗 tick（毫秒）：epoch 精度
const TICK_MS: u64 = 10;

/// 来自策略的限额（由 `policy::Effective` 映射而来）
#[derive(Debug, Clone)]
pub struct Limits {
    pub fuel: u64,
    pub memory_mb: u32,
    pub stack_kb: u32,
    pub wall_ms: u32,
    pub max_output_chars: u32,
    pub max_network_calls: u32,
    /// 允许外呼的域名（`permissions.network`，支持 `*.example.com`）
    pub network_allow: Vec<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self { fuel: 50_000_000, memory_mb: 256, stack_kb: 1024, wall_ms: 5000, max_output_chars: 8000, max_network_calls: 8, network_allow: vec![] }
    }
}

/// 一次宿主能力调用（进 trace，可审计）
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostCall {
    pub kind: String,
    pub target: String,
    pub ok: bool,
    pub detail: String,
}

/// 宿主能力桥：引擎调用它，具体实现（HTTP / 知识库 / 任务表）留给上层
pub trait HostBridge {
    /// 本机愿意提供给 guest 的工具名。**不在列表里 = 越权调用**：引擎直接拦下整次运行
    /// （那是"包在要本机没答应的能力"，不是运行期小故障）。
    fn provides(&self, name: &str) -> bool {
        let _ = name;
        true
    }
    /// `kb.search` / `task.create` / `repo_search`
    fn tool(&mut self, name: &str, arg: &str) -> Result<String>;
    /// 直连 HTTP（域名白名单由引擎先判，`url` 已通过）
    fn http_get(&mut self, url: &str) -> Result<String>;
}

/// 可共享的能力桥句柄（`Store<T>` 要求 `T: 'static`，所以桥必须被拥有而不是借用；
/// 共享句柄同时让调用方事后能读回 trace）
pub type SharedBridge = Rc<RefCell<Box<dyn HostBridge>>>;

/// 把任意桥包成共享句柄
pub fn shared<B: HostBridge + 'static>(b: B) -> SharedBridge {
    Rc::new(RefCell::new(Box::new(b)))
}

/// 什么都不给的桥（单测 / dry-run 用）
#[derive(Default)]
pub struct NoHost;

impl HostBridge for NoHost {
    fn provides(&self, _name: &str) -> bool {
        false
    }
    fn tool(&mut self, name: &str, _arg: &str) -> Result<String> {
        bail!("宿主未提供工具能力（{name}）")
    }
    fn http_get(&mut self, _url: &str) -> Result<String> {
        bail!("宿主未提供网络能力")
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunOutcome {
    pub ok: bool,
    /// guest 交回的回复（已按 max_output_chars 截断）
    pub reply: String,
    pub truncated: bool,
    pub logs: Vec<String>,
    pub host_calls: Vec<HostCall>,
    pub fuel_used: u64,
    pub wall_ms: u128,
    /// 运行结束时 guest 线性内存占用（字节）——用来肉眼核对内存上限真的生效了
    pub memory_bytes: u64,
    /// 失败原因（trap / 预算耗尽 / 内存超限 …）
    pub error: Option<String>,
    /// 失败类别：`fuel` | `wall` | `memory` | `stack` | `trap` | `abi` | `capability`
    pub error_kind: Option<String>,
}

pub(crate) struct HostState {
    limits: StoreLimits,
    cap_chars: u32,
    net_calls_cap: u32,
    reply: String,
    truncated: bool,
    logs: Vec<String>,
    calls: Vec<HostCall>,
    network_calls: u32,
    network_allow: Vec<String>,
    bridge: SharedBridge,
}

/// 跑一个 wasm 包入口。
/// **被沙箱拦下的失败返回 `Ok(outcome)`**（可读结论，便于落盘审计）；只有调用方错误（模块不合法 / ABI 不满足）才 `Err`。
pub fn run(wasm_bytes: &[u8], limits: &Limits, bridge: SharedBridge) -> Result<RunOutcome> {
    let engine = engine_of(limits)?;
    let module = Module::new(&engine, wasm_bytes).context("载入 wasm 模块失败（入口不是合法 wasm？）")?;

    // 墙钟预算：看门狗按 TICK_MS 推进 epoch，guest 到点必被中断
    let ticks = Arc::new(AtomicU64::new(0));
    let watched = engine.clone();
    let counter = ticks.clone();
    let total_ticks = (limits.wall_ms.max(TICK_MS as u32) as u64 / TICK_MS) + 1;
    let watchdog = std::thread::spawn(move || {
        for _ in 0..total_ticks {
            std::thread::sleep(Duration::from_millis(TICK_MS));
            counter.fetch_add(1, Ordering::SeqCst);
            watched.increment_epoch();
        }
    });

    let state = HostState {
        limits: StoreLimitsBuilder::new()
            .memory_size((limits.memory_mb.max(1) as usize) * 1024 * 1024)
            .instances(4)
            .tables(4)
            .memories(2)
            .build(),
        cap_chars: limits.max_output_chars,
        net_calls_cap: limits.max_network_calls,
        reply: String::new(),
        truncated: false,
        logs: Vec::new(),
        calls: Vec::new(),
        network_calls: 0,
        network_allow: limits.network_allow.clone(),
        bridge,
    };
    let mut store = Store::new(&engine, state);
    store.limiter(|s| &mut s.limits);
    store.set_fuel(limits.fuel).context("设置指令预算失败")?;
    store.set_epoch_deadline(total_ticks);

    let mut linker: Linker<HostState> = Linker::new(&engine);
    link_host(&mut linker)?;

    // 结构性缺陷（模块不合法 / ABI 不满足）在**执行前**就判掉：这是包的问题，不是"被沙箱拦下"
    let instance = linker.instantiate(&mut store, &module).context("实例化失败（ABI 不满足？）")?;
    let run_fn = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "hur_run")
        .map_err(|_| anyhow::anyhow!("guest 必须导出 `hur_run(ptr: i32, len: i32) -> i32`"))?;

    let started = Instant::now();
    let res = (|| -> Result<()> {
        let rc = run_fn.call(&mut store, (0, 0))?;
        if rc != 0 {
            bail!("guest 返回失败码 {rc}（guest 自己判定失败）");
        }
        Ok(())
    })();

    let wall = started.elapsed();
    let memory_bytes = instance
        .get_memory(&mut store, "memory")
        .map(|m| m.data_size(&store) as u64)
        .unwrap_or(0);
    let fuel_left = store.get_fuel().unwrap_or(0);
    let fuel_used = limits.fuel.saturating_sub(fuel_left);
    let (ok, err, kind) = match res {
        Ok(()) => (true, None, None),
        Err(e) => {
            let (kind, msg) = classify(&e);
            (false, Some(msg), Some(kind))
        }
    };
    let d = store.data();
    let out = RunOutcome {
        ok,
        reply: d.reply.clone(),
        truncated: d.truncated,
        logs: d.logs.clone(),
        host_calls: d.calls.clone(),
        fuel_used,
        wall_ms: wall.as_millis(),
        memory_bytes,
        error: err,
        error_kind: kind,
    };
    drop(watchdog);
    Ok(out)
}

fn engine_of(limits: &Limits) -> Result<Engine> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config.max_wasm_stack((limits.stack_kb.max(64) as usize) * 1024);
    config.wasm_backtrace(false);
    Engine::new(&config).context("创建 wasm 引擎失败")
}

fn link_host(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap("hur", "log", |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<()> {
        let bytes = read_guest(&mut caller, ptr, len)?;
        caller.data_mut().logs.push(String::from_utf8_lossy(&bytes).to_string());
        Ok(())
    })?;

    linker.func_wrap("hur", "reply", |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<()> {
        let bytes = read_guest(&mut caller, ptr, len)?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        let cap = caller.data().cap_chars as usize;
        let already = caller.data().reply.chars().count();
        let room = cap.saturating_sub(already);
        let taken: String = text.chars().take(room).collect();
        let cut = text.chars().count() > room;
        let d = caller.data_mut();
        d.reply.push_str(&taken);
        if cut {
            d.truncated = true;
        }
        Ok(())
    })?;

    linker.func_wrap(
        "hur",
        "tool",
        |mut caller: Caller<'_, HostState>, name_ptr: i32, name_len: i32, arg_ptr: i32, arg_len: i32| -> Result<i32> {
            let name = lossy(read_guest(&mut caller, name_ptr, name_len)?);
            let arg = lossy(read_guest(&mut caller, arg_ptr, arg_len)?);
            // ① 越权工具：本机没答应的能力，一律拦下整次运行
            if !caller.data().bridge.borrow().provides(&name) {
                caller.data_mut().calls.push(HostCall { kind: format!("tool:{name}"), target: arg.clone(), ok: false, detail: "越权：本机未提供给 guest 的工具".into() });
                bail!("越权工具调用：本机没有给 guest 提供工具「{name}」");
            }
            // ② 工具存在但内部报错 → 空响应（guest 自己降级）
            let r = caller.data_mut().bridge.borrow_mut().tool(&name, &arg);
            finish_call(&mut caller, format!("tool:{name}"), arg, r)
        },
    )?;

    linker.func_wrap(
        "hur",
        "http_get",
        |mut caller: Caller<'_, HostState>, url_ptr: i32, url_len: i32| -> Result<i32> {
            let url = lossy(read_guest(&mut caller, url_ptr, url_len)?);
            let host = host_of(&url);
            // ① 域名白名单（声明即策略）：未声明的域名一律拒绝，且不惊动宿主
            let allow = caller.data().network_allow.clone();
            if host.is_empty() || !crate::allow::host_allowed(&host, &allow) {
                let detail = if host.is_empty() { "URL 里没有主机名".to_string() } else { format!("域名「{host}」未在 permissions.network 里声明") };
                caller.data_mut().calls.push(HostCall { kind: "http_get".into(), target: url.clone(), ok: false, detail: detail.clone() });
                bail!("越权外呼：{detail}");
            }
            // ② 调用次数上限
            let (used, cap) = (caller.data().network_calls, caller.data().net_calls_cap);
            if cap == 0 || used >= cap {
                let detail = if cap == 0 { "策略把网络调用上限设为 0".to_string() } else { format!("网络调用次数已达上限（{used}/{cap}）") };
                caller.data_mut().calls.push(HostCall { kind: "http_get".into(), target: url.clone(), ok: false, detail: detail.clone() });
                bail!("{detail}");
            }
            caller.data_mut().network_calls += 1;
            let r = caller.data_mut().bridge.borrow_mut().http_get(&url);
            finish_call(&mut caller, "http_get".into(), url, r)
        },
    )?;
    Ok(())
}

/// 统一收尾：记 trace + 把响应写回 guest 内存（经 `hur_alloc`），返回指针。
///
/// **能力失败不 trap**：网络不可达 / 上游 404 / 工具内部报错，都是 guest 该自己处理的
/// 运行期状况 —— 返回 0（空响应）并留痕，运行继续。只有**策略违规**才拦下（那是人要看的事）。
fn finish_call(caller: &mut Caller<'_, HostState>, kind: String, target: String, r: Result<String>) -> Result<i32> {
    match r {
        Ok(text) => {
            caller.data_mut().calls.push(HostCall { kind, target, ok: true, detail: format!("{} 字节", text.len()) });
            let ptr = write_via_alloc_caller(caller, text.as_bytes())?;
            Ok(ptr as i32)
        }
        Err(e) => {
            let msg = e.to_string();
            caller.data_mut().calls.push(HostCall { kind, target, ok: false, detail: msg });
            Ok(0) // 空响应：guest 自行决定怎么降级
        }
    }
}

fn lossy(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).to_string()
}

fn memory_of(caller: &mut Caller<'_, HostState>) -> Result<Memory> {
    caller.get_export("memory").and_then(|e| e.into_memory()).ok_or_else(|| anyhow::anyhow!("guest 必须导出 `memory`"))
}

fn read_guest(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Result<Vec<u8>> {
    if ptr < 0 || len < 0 {
        bail!("非法指针/长度：{ptr}/{len}");
    }
    let mem = memory_of(caller)?;
    let (p, l) = (ptr as usize, len as usize);
    let end = p.checked_add(l).ok_or_else(|| anyhow::anyhow!("指针溢出"))?;
    let data = mem.data(&*caller);
    if end > data.len() {
        bail!("越界读写：{p}+{l} > {}", data.len());
    }
    Ok(data[p..end].to_vec())
}

fn write_via_alloc_caller(caller: &mut Caller<'_, HostState>, bytes: &[u8]) -> Result<usize> {
    let alloc = caller.get_export("hur_alloc").and_then(|e| e.into_func()).ok_or_else(|| anyhow::anyhow!("guest 必须导出 `hur_alloc`"))?;
    let alloc = alloc.typed::<i32, i32>(&*caller)?;
    let ptr = alloc.call(&mut *caller, bytes.len() as i32)?;
    if ptr <= 0 && !bytes.is_empty() {
        bail!("guest 分配 {} 字节失败", bytes.len());
    }
    let mem = memory_of(caller)?;
    let (p, l) = (ptr as usize, bytes.len());
    let data = mem.data_mut(&mut *caller);
    if p + l > data.len() {
        bail!("写入越界：{p}+{l} > {}", data.len());
    }
    data[p..p + l].copy_from_slice(bytes);
    Ok(p)
}

/// 从 URL 里取主机名（去端口、去 userinfo）
pub fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// 把 wasmtime 的错误归类成人能看懂的原因（也是 trace 里的 `error_kind`）
pub fn classify(e: &anyhow::Error) -> (String, String) {
    let text = format!("{e:#}");
    let low = text.to_ascii_lowercase();
    if low.contains("fuel") {
        return ("fuel".into(), format!("指令预算耗尽（fuel）—— 可能是死循环：{text}"));
    }
    if low.contains("epoch") || low.contains("interrupt") {
        return ("wall".into(), format!("墙钟超时（epoch deadline）—— 运行过久已被中断：{text}"));
    }
    if low.contains("memory") && (low.contains("limit") || low.contains("out of memory") || low.contains("oom") || low.contains("grow")) {
        return ("memory".into(), format!("超出内存上限：{text}"));
    }
    if low.contains("stack") {
        return ("stack".into(), format!("调用栈超限：{text}"));
    }
    if low.contains("越权") || low.contains("未声明") || low.contains("上限") || low.contains("未提供") || low.contains("策略") {
        return ("capability".into(), text);
    }
    if low.contains("越界") || low.contains("hur_alloc") || low.contains("hur_run") || low.contains("guest 必须导出") || low.contains("分配") {
        return ("abi".into(), format!("guest 用错了宿主 ABI：{text}"));
    }
    if low.contains("guest 返回失败码") {
        return ("trap".into(), text);
    }
    if low.contains("unreachable") || low.contains("out of bounds") || low.contains("integer overflow") || low.contains("trap") {
        return ("trap".into(), text);
    }
    ("trap".into(), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用桥：把每次 http_get 记进一个外部可见的清单（桥本身被 Box 掉了，只能这样读回）
    struct Recording {
        log: Rc<RefCell<Vec<String>>>,
    }
    impl Recording {
        fn new() -> (Self, Rc<RefCell<Vec<String>>>) {
            let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            (Self { log: log.clone() }, log)
        }
    }
    impl HostBridge for Recording {
        fn tool(&mut self, name: &str, arg: &str) -> Result<String> {
            Ok(format!("tool:{name}:{arg}"))
        }
        fn http_get(&mut self, url: &str) -> Result<String> {
            self.log.borrow_mut().push(url.to_string());
            Ok(format!("HTTP200 {url}"))
        }
    }

    /// 只会失败的能力桥（模拟网络不可达 / 上游 5xx）
    struct Failing;
    impl HostBridge for Failing {
        fn tool(&mut self, _n: &str, _a: &str) -> Result<String> {
            bail!("工具内部错误")
        }
        fn http_get(&mut self, _u: &str) -> Result<String> {
            bail!("连接被拒绝")
        }
    }

    fn limits() -> Limits {
        Limits { fuel: 2_000_000, memory_mb: 16, stack_kb: 256, wall_ms: 300, max_output_chars: 40, max_network_calls: 2, network_allow: vec!["api.hotel.example.com".into()] }
    }

    #[test]
    fn host_of_parses_urls() {
        assert_eq!(host_of("https://api.hotel.example.com:8443/rooms?x=1"), "api.hotel.example.com");
        assert_eq!(host_of("http://user:pw@h.example.com/a"), "h.example.com");
        assert_eq!(host_of(""), "");
    }

    /// 死循环必须被中断（fuel 或 wall 任一生效都算过）
    #[test]
    fn infinite_loop_is_interrupted() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "hur_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "hur_run") (param i32 i32) (result i32)
                (loop $l br $l)
                i32.const 0)
        )"#;
        let out = run(wat.as_bytes(), &limits(), shared(NoHost)).unwrap();
        assert!(!out.ok, "死循环必须被拦下");
        let kind = out.error_kind.clone().unwrap();
        assert!(kind == "fuel" || kind == "wall", "kind={kind} err={:?}", out.error);
    }

    /// 内存炸弹：超上限即失败（memory.grow 被拒 → guest 报失败码）
    #[test]
    fn memory_bomb_is_capped() {
        let wat = r#"(module
            (memory (export "memory") 1 100)
            (func (export "hur_alloc") (param i32) (result i32) i32.const 0)
            (func (export "hur_run") (param i32 i32) (result i32)
                (if (i32.eq (memory.grow (i32.const 90)) (i32.const -1)) (then (return (i32.const 7))))
                (if (i32.eq (memory.grow (i32.const 90)) (i32.const -1)) (then (return (i32.const 7))))
                i32.const 0)
        )"#;
        // 8MB 上限：90 页 = 5.6MB，第二次必然被拒
        let mut lim = limits();
        lim.memory_mb = 8;
        let out = run(wat.as_bytes(), &lim, shared(NoHost)).unwrap();
        assert!(!out.ok, "{:?}", out);
        assert!(out.error.as_deref().unwrap().contains('7'), "{:?}", out.error);
    }

    /// 内存上限要能被**看见**：无 max 的 memory.grow 也会被 StoreLimits 挡住，
    /// 且最终线性内存占用必须落在上限内（不是"看起来没事"）。
    #[test]
    fn memory_growth_is_capped_and_visible() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "hur_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "hur_run") (param i32 i32) (result i32)
                (drop (memory.grow (i32.const 5000)))
                i32.const 0)
        )"#;
        let mut lim = limits();
        lim.memory_mb = 8;
        let out = run(wat.as_bytes(), &lim, shared(NoHost)).unwrap();
        // grow 被拒后 guest 选择继续跑：运行"成功"，但内存必须真的没涨上去
        assert!(out.memory_bytes <= 8 * 1024 * 1024, "线性内存必须被压在上限内：{}", out.memory_bytes);
    }

    /// 能力失败 ≠ 策略违规：网络 / 上游出错只是"空响应"，运行继续；
    /// 只有越权（未声明域名 / 次数超限）才拦下整次运行。
    #[test]
    fn host_failure_returns_empty_and_keeps_running() {
        let wat = r#"(module
            (import "hur" "http_get" (func $get (param i32 i32) (result i32)))
            (import "hur" "reply" (func $reply (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "https://api.hotel.example.com/rooms")
            (data (i32.const 64) "degraded")
            (func (export "hur_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "hur_run") (param i32 i32) (result i32)
                (local $p i32)
                (local.set $p (call $get (i32.const 0) (i32.const 35)))
                (if (i32.eqz (local.get $p)) (then (call $reply (i32.const 64) (i32.const 8))))
                i32.const 0)
        )"#;
        let out = run(wat.as_bytes(), &limits(), shared(Failing)).unwrap();
        assert!(out.ok, "能力失败不该 trap：{:?}", out.error);
        assert_eq!(out.reply, "degraded", "空响应应让 guest 走降级分支");
        assert_eq!(out.host_calls.len(), 1);
        assert!(!out.host_calls[0].ok);
        assert!(out.host_calls[0].detail.contains("连接被拒绝"), "{:?}", out.host_calls[0]);
    }

    /// 越权工具调用（本机没答应的能力）→ 拦下整次运行，而不是给个空响应糊过去
    #[test]
    fn tool_outside_host_capabilities_is_denied() {
        let wat = r#"(module
            (import "hur" "tool" (func $tool (param i32 i32 i32 i32) (result i32)))
            (import "hur" "reply" (func $reply (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "shell.exec")
            (data (i32.const 32) "rm -rf /")
            (data (i32.const 64) "should-not-reply")
            (func (export "hur_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "hur_run") (param i32 i32) (result i32)
                (call $reply (i32.const 64) (i32.const 16))
                (drop (call $tool (i32.const 0) (i32.const 10) (i32.const 32) (i32.const 8)))
                i32.const 0)
        )"#;
        let out = run(wat.as_bytes(), &limits(), shared(NoHost)).unwrap();
        assert!(!out.ok, "越权工具必须拦下");
        assert_eq!(out.error_kind.as_deref(), Some("capability"));
        assert!(out.error.as_deref().unwrap().contains("shell.exec"), "{:?}", out.error);
    }

    /// 正常包：log + reply，且 reply 按上限截断
    #[test]
    fn normal_run_logs_and_truncates_reply() {
        let long = "甲".repeat(100);
        let wat = format!(
            r#"(module
            (import "hur" "log" (func $log (param i32 i32)))
            (import "hur" "reply" (func $reply (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "long-reply-body")
            (data (i32.const 64) "{long}")
            (func (export "hur_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "hur_run") (param i32 i32) (result i32)
                (call $log (i32.const 0) (i32.const 6))
                (call $reply (i32.const 64) (i32.const {len}))
                i32.const 0)
        )"#,
            len = long.len()
        );
        let out = run(wat.as_bytes(), &limits(), shared(NoHost)).unwrap();
        assert!(out.ok, "{:?}", out.error);
        assert_eq!(out.logs, vec!["long-r".to_string()]);
        assert_eq!(out.reply.chars().count(), 40, "reply 必须被截断到上限");
        assert!(out.truncated);
    }

    /// 未声明域名 → 越权拒绝（白名单在引擎里判，不惊动宿主）
    #[test]
    fn undeclared_host_is_denied() {
        let wat = r#"(module
            (import "hur" "http_get" (func $get (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "https://evil.example.com/steal")
            (func (export "hur_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "hur_run") (param i32 i32) (result i32)
                (drop (call $get (i32.const 0) (i32.const 30)))
                i32.const 0)
        )"#;
        let (bridge, log) = Recording::new();
        let out = run(wat.as_bytes(), &limits(), shared(bridge)).unwrap();
        assert!(!out.ok, "越权外呼必须失败");
        let err = out.error.clone().unwrap();
        assert!(err.contains("越权") || err.contains("未声明"), "{err}");
        assert!(log.borrow().is_empty(), "宿主不应该真的发出请求");
        assert_eq!(out.host_calls.len(), 1);
        assert!(!out.host_calls[0].ok);
    }

    /// 已声明域名 + 次数上限：前两次放行，第三次被拒
    #[test]
    fn declared_host_allowed_until_call_cap() {
        let wat = r#"(module
            (import "hur" "http_get" (func $get (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "https://api.hotel.example.com/rooms")
            (func (export "hur_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "hur_run") (param i32 i32) (result i32)
                (drop (call $get (i32.const 0) (i32.const 35)))
                (drop (call $get (i32.const 0) (i32.const 35)))
                (drop (call $get (i32.const 0) (i32.const 35)))
                i32.const 0)
        )"#;
        let (bridge, log) = Recording::new();
        let out = run(wat.as_bytes(), &limits(), shared(bridge)).unwrap();
        assert!(!out.ok, "第三次调用应触发上限");
        assert_eq!(log.borrow().len(), 2, "宿主只应被调用两次");
        assert!(out.error.as_deref().unwrap().contains("上限"), "{:?}", out.error);
        assert_eq!(out.host_calls.iter().filter(|c| c.ok).count(), 2);
        assert_eq!(out.host_calls.iter().filter(|c| !c.ok).count(), 1);
    }

    /// ABI 不满足（缺 hur_run）→ 调用方错误（Err），并说清缺什么
    #[test]
    fn missing_abi_is_caller_error() {
        let wat = r#"(module (memory (export "memory") 1))"#;
        let e = run(wat.as_bytes(), &limits(), shared(NoHost)).unwrap_err().to_string();
        assert!(e.contains("hur_run"), "{e}");
    }

    /// host call 的返回值能写回 guest 内存（ABI 闭环）
    #[test]
    fn host_tool_response_is_written_back() {
        let wat = r#"(module
            (import "hur" "tool" (func $tool (param i32 i32 i32 i32) (result i32)))
            (import "hur" "reply" (func $reply (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "kb.search")
            (data (i32.const 32) "hotel")
            (func (export "hur_alloc") (param i32) (result i32) i32.const 8192)
            (func (export "hur_run") (param i32 i32) (result i32)
                (local $p i32)
                (local.set $p (call $tool (i32.const 0) (i32.const 9) (i32.const 32) (i32.const 5)))
                (call $reply (local.get $p) (i32.const 20))
                i32.const 0)
        )"#;
        let (bridge, _log) = Recording::new();
        let out = run(wat.as_bytes(), &limits(), shared(bridge)).unwrap();
        assert!(out.ok, "{:?}", out.error);
        assert_eq!(out.reply, "tool:kb.search:hotel");
        assert_eq!(out.host_calls.len(), 1);
        assert!(out.host_calls[0].ok);
    }
}
