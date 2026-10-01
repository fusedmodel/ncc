//! `ncc conn --ssh`：**把系统 `ssh` 当成运输层** —— 通道走 SSH，字节不经 NCC。
//!
//! 用户说「我已经设好了一条 SSH 通道」时，真正设好的是 `~/.ssh/config`、key、agent、
//! 跳板机、`known_hosts` —— 那些在**这台机器上**本来就能用。所以这里只当搬运工：
//! 复用你已有的信任，而不是再造一套（引一个 SSH 库就要自己管 key、host key 校验、
//! ProxyJump……全是重复劳动）。
//!
//! 与 HTTP 通道（`/api/conn/*`，要目标端 `NCCR_CONN_ALLOW=1`）的分工：
//!   · **HTTP 通道**：目标端跑着 `ncc-registry`，门禁与账本在**服务端**执行。
//!   · **SSH 通道**：目标端只要有 `sshd`，**零服务端改动**；权限、认证、加密都是 SSH 的。
//!
//! 红线（与 `prd/ncc-conn.md` 一致，别在这里加权限模型）：
//!  1. **权限就是 SSH 的权限**：能免密 ssh 进那台机器的人本来就能干任何事。ncc 不假装
//!     有比 SSH 更高的门禁 —— 所以 SSH 通道**没有** `NCCR_CONN_ALLOW` 那种开关，也不该有。
//!  2. **相对路径检查是防手滑，不是安全边界**：HTTP 那份 `safeJoin` 由服务端执行，才是真
//!     边界；这里客户端说什么就是什么 —— 得诚实说出来，别把客户端的礼貌当门禁。
//!  3. **账本跟着通道走**：`<目录>/.ncc-ledger.jsonl`，谁、什么时候、要了什么、结果如何。
//!     不经云端（数据面不过托管云）。但要诚实：**它是自报的，不是防篡改的** ——
//!     要服务端记账就得多花一台节点的钱走 HTTP 通道。
//!  4. **TTL 是软的**：SSH 不会替我们拦；到点后 CLI 自己拒绝（元数据里也写着到期时间）。
//!  5. **`push` 在目标端核 sha256**：字节传完就问目标端要指纹，对不上就说对不上
//!     （与 HTTP 通道「字节即事实」同一条规矩）。
//!  6. **`close --purge` 只删「认得出来的」目录**：目录里必须有我们写的
//!     `.ncc-channel.json`；没有就拒绝删，让用户自己动手（别把客户端的错删当能力）。
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `ssh` 可执行文件（测试用替身就靠它替换）。
pub fn ssh_bin() -> String {
    std::env::var("NCC_SSH_BIN").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "ssh".into())
}

const META: &str = ".ncc-channel.json";
const LEDGER: &str = ".ncc-ledger.jsonl";
const TAIL_CAP: usize = 32 << 10; // 与 HTTP 通道的日志尾巴同一个上限

/// 目标机（怎么连、连到哪个目录）。
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SshSpec {
    pub host: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub identity: String,
    /// 通道目录（相对 home 或绝对；默认 `.ncc/conn/<名字>`）
    pub dir: String,
    /// 强制 `BatchMode=yes`（CI：拿不到密钥就直接失败，不挂在那儿等密码）
    #[serde(default)]
    pub batch: bool,
}

impl SshSpec {
    /// `[user@]host[:port]` —— 只认这一种写法（跳板机、别名交给 `~/.ssh/config`）。
    pub fn parse(s: &str) -> Result<SshSpec> {
        let s = s.trim();
        if s.is_empty() {
            bail!("--ssh 后面给个目标：[user@]host[:port]");
        }
        let (user, rest) = match s.split_once('@') {
            Some((u, r)) => (u.to_string(), r),
            None => (String::new(), s),
        };
        // IPv6 写成 [::1]:22 —— 顺手认一下，别让用户去猜
        let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
            match inner.split_once(']') {
                Some((h, tail)) => (h.to_string(), tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(0)),
                None => bail!("IPv6 要写成 [::1]:22 的样子"),
            }
        } else {
            match rest.rsplit_once(':') {
                Some((h, p)) => match p.parse::<u16>() {
                    Ok(n) => (h.to_string(), n),
                    Err(_) => (rest.to_string(), 0), // 冒号后面不是端口 → 当成主机名
                },
                None => (rest.to_string(), 0),
            }
        };
        if host.trim().is_empty() {
            bail!("--ssh 没给出主机名");
        }
        Ok(SshSpec { host, user, port, identity: String::new(), dir: String::new(), batch: false })
    }

    pub fn target(&self) -> String {
        if self.user.trim().is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        }
    }

    pub fn label(&self) -> String {
        if self.port > 0 {
            format!("{}:{}", self.target(), self.port)
        } else {
            self.target()
        }
    }

    fn base_args(&self) -> Vec<String> {
        let mut a = vec!["-o".into(), "ConnectTimeout=10".into()];
        if self.batch {
            a.push("-o".into());
            a.push("BatchMode=yes".into());
        }
        if self.port > 0 {
            a.push("-p".into());
            a.push(self.port.to_string());
        }
        if !self.identity.trim().is_empty() {
            a.push("-i".into());
            a.push(self.identity.trim().to_string());
        }
        a
    }
}

/// 一次远端调用的结果。
pub struct SshOut {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub ms: u64,
    pub timed_out: bool,
    /// ssh 自己就没跑起来（连不上 / 认证失败）—— 与「远端命令失败」要分开说
    pub ssh_error: bool,
}

impl SshOut {
    pub fn out_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// 单引号包裹（内部 `'` → `'\''`）——远端 shell 拿到的是字面量，不会展开任何东西。
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 相对路径守卫：**防手滑**（不是安全边界，见文件头红线 2）。
fn check_rel(rel: &str) -> Result<()> {
    let r = rel.trim();
    if r.is_empty() {
        bail!("路径不能为空");
    }
    if r.contains('\0') {
        bail!("路径里有非法字符");
    }
    if r.starts_with('/') || r.starts_with('\\') || r.starts_with('~') {
        bail!("只接受相对路径（锁在这条通道的目录里）：{rel}");
    }
    // 逐段看：任何一段是 `..` 就直接拒（比路径规整更好读，也更难绕过）
    for seg in r.split(['/', '\\']) {
        if seg == ".." {
            bail!("路径不许跳出通道目录：{rel}");
        }
    }
    Ok(())
}

/// 日志尾巴（与 HTTP 通道同一个上限）。
pub fn log_tail(b: &[u8]) -> (String, bool) {
    tail_bytes(b, TAIL_CAP)
}

fn tail_bytes(b: &[u8], cap: usize) -> (String, bool) {
    if b.len() <= cap {
        (String::from_utf8_lossy(b).to_string(), false)
    } else {
        (String::from_utf8_lossy(&b[b.len() - cap..]).to_string(), true)
    }
}

/// 跑一次远端命令（`stdin_bytes` 非空就喂给远端 stdin）。
///
/// 输出落到临时文件而不是管道：管道要两个线程去排空，否则输出一大就死锁。
pub fn run(spec: &SshSpec, remote_cmd: &str, stdin_bytes: Option<&[u8]>, timeout_sec: u64) -> Result<SshOut> {
    let bin = ssh_bin();
    let t0 = Instant::now();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let op = std::env::temp_dir().join(format!("ncc-ssh-{stamp}-{}.out", std::process::id()));
    let ep = std::env::temp_dir().join(format!("ncc-ssh-{stamp}-{}.err", std::process::id()));

    let mut cmd = Command::new(&bin);
    cmd.args(spec.base_args()).arg(spec.target()).arg(remote_cmd);
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        // **不接管 stdin**：既不喂给远端命令，也不让远端命令偷走调用方的 stdin
        // （`ncc conn exec x cat` 不该把脚本的标准输入吃掉）。
        // 第一次连的 host key 询问、需要密码时的提示，ssh 会走 /dev/tty，照旧能亲手答。
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::from(std::fs::File::create(&op)?));
    cmd.stderr(Stdio::from(std::fs::File::create(&ep)?));

    let mut child = cmd.spawn().map_err(|e| {
        anyhow!(
            "起不了 `{bin}`：{e}\n  提示：装上 OpenSSH 客户端，或用 NCC_SSH_BIN 指定别的实现"
        )
    })?;
    if let Some(b) = stdin_bytes {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(b);
            // drop 掉 = 关掉远端 stdin（`cat` 才会收工）
        }
    }
    let deadline = if timeout_sec > 0 { Some(Instant::now() + Duration::from_secs(timeout_sec)) } else { None };
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break Some(st),
            None => {
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break None;
                    }
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        }
    };
    let stdout = std::fs::read(&op).unwrap_or_default();
    let stderr = std::fs::read(&ep).unwrap_or_default();
    let _ = std::fs::remove_file(&op);
    let _ = std::fs::remove_file(&ep);

    let code = match status {
        None => 124, // 与 GNU timeout(1) 同一个约定
        Some(st) => st.code().unwrap_or(-1), // -1 = 被信号终止
    };
    let err_text = String::from_utf8_lossy(&stderr).to_string();
    // ssh 自己失败（连不上 / 认证失败）时它固定用 255
    let ssh_error = !timed_out && (code == 255 || (code == -1 && !err_text.is_empty() && stdout.is_empty()));
    Ok(SshOut { code, stdout, stderr: err_text, ms: t0.elapsed().as_millis() as u64, timed_out, ssh_error })
}

/* ---------------- 通道动作 ---------------- */

fn dir_of(spec: &SshSpec) -> String {
    let d = spec.dir.trim();
    if d.is_empty() {
        ".ncc/conn/default".to_string()
    } else {
        d.to_string()
    }
}

/// 在任何动作之前问一句「这条通道还在不在、过期没」——不在就早点说，别让用户以为是网络问题。
fn preflight(spec: &SshSpec, expires_unix: u64) -> Result<()> {
    if expires_unix > 0 && crate::conn::now_unix() >= expires_unix {
        bail!(
            "这条 SSH 通道已经过期（到 {}）——\n  TTL 对 SSH 是**软约束**（sshd 不知道我们在计时），\
             CLI 自己拒绝而已。要接着用：`ncc conn open --ssh {}` 重新开一条（`--ttl <秒>` 给更长）。",
            crate::conn::fmt_epoch(expires_unix as i64),
            spec.label()
        );
    }
    Ok(())
}

/// 开通道：建目录 → 写元数据 → 读回来验一遍。**零服务端改动**（目标端只要有 sshd）。
pub fn open(spec: &SshSpec, name: &str, note: &str, expires_unix: u64) -> Result<()> {
    let dir = dir_of(spec);
    let meta = json!({
        "id": "", "name": name, "note": note,
        "transport": "ssh", "host": spec.host, "user": spec.user, "port": spec.port,
        "dir": dir, "expiresUnix": expires_unix,
        "createdAtUnix": crate::conn::now_unix(),
        "by": format!("{}@{}", whoami(), hostname()),
        "note2": "这条通道由 ncc 建立；账本在同目录的 .ncc-ledger.jsonl（自报，不防篡改）",
    });
    let script = format!(
        "mkdir -p {d} {d}/.tmp || exit 73\nchmod 700 {d} {d}/.tmp 2>/dev/null\ncat > {d}/{meta} || exit 74\ncat {d}/{meta} || exit 75\n",
        d = q(&dir),
        meta = META
    );
    let body = format!("{}\n", serde_json::to_string_pretty(&meta)?);
    let r = run(spec, &script, Some(body.as_bytes()), 30)?;
    if r.ssh_error || r.code == 73 || r.code == 74 || r.code == 75 {
        bail!(
            "在 {} 上建通道目录失败（{}，退出码 {}）：\n  {}\n  检查：ssh {} 能不能直接连上、\
             目录 {dir} 能不能写。",
            spec.label(),
            if r.ssh_error { "ssh 本身就没连上（认证 / 网络 / host key）" } else { "远端命令失败" },
            r.code,
            r.stderr.trim(),
            spec.label()
        );
    }
    let back: Value = serde_json::from_slice(&r.stdout)
        .map_err(|e| anyhow!("写进去的元数据读回来不是 JSON（{e}）—— 目录 {} 可能不对", dir_of(spec)))?;
    if back["name"].as_str() != Some(name) {
        bail!("元数据核对失败：写的是 {name}，读回来是 {}", back["name"]);
    }
    Ok(())
}
/// 在通道上跑一条命令。**退出码就是远端的退出码**（ssh 原样透传）。
pub fn exec(spec: &SshSpec, expires_unix: u64, cmd: &str, cwd: Option<&str>, timeout_sec: u64, reason: &str) -> Result<SshOut> {
    preflight(spec, expires_unix)?;
    let dir = dir_of(spec);
    let mut script = String::new();
    if let Some(w) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        check_rel(w)?;
        script.push_str(&format!("cd {} || {{ echo '子目录不存在：{}' >&2; exit 66; }}\n", q(&format!("{dir}/{w}")), w));
    } else {
        script.push_str(&format!(
            "cd {} 2>/dev/null || {{ echo '通道目录不见了（close --purge 过？）：{}' >&2; exit 66; }}\n",
            q(&dir),
            dir
        ));
    }
    // 原样交给远端 shell：`&&`、管道、重定向都按它自己的规矩来（我们不替它猜）
    script.push_str(cmd);
    script.push('\n');
    let out = run(spec, &script, None, timeout_sec)?;
    // 账本事后补记（第二个往返：退出码只有跑完才知道）。记不上只提醒，不算这次动作失败。
    let entry = json!({
        "at": crate::conn::now_unix(),
        "act": "exec",
        "by": whoami_host(),
        "reason": reason,
        "cmd": cmd,
        "cwd": cwd.unwrap_or(""),
        "exit": out.code,
        "timedOut": out.timed_out,
        "ms": out.ms,
    });
    if let Err(e) = ledger_append(spec, &dir, &entry) {
        eprintln!("  ! 账本没记上（{e}）—— 动作本身已经执行过了");
    }
    Ok(out)
}

fn ledger_append(spec: &SshSpec, dir: &str, entry: &Value) -> Result<()> {
    let line = format!("{}\n", serde_json::to_string(entry)?);
    let script = format!("cat >> {}/{}", q(dir), LEDGER);
    let r = run(spec, &script, Some(line.as_bytes()), 20)?;
    if !r.ok() {
        bail!("{}", r.stderr.trim());
    }
    Ok(())
}

/// 推文件：`cat > 文件` → `chmod` → **问目标端要 sha256** 与本地对照。
pub fn put(spec: &SshSpec, expires_unix: u64, rel: &str, bytes: &[u8], mode: u32, reason: &str) -> Result<String> {
    preflight(spec, expires_unix)?;
    check_rel(rel)?;
    let dir = dir_of(spec);
    let want = crate::sandbox::sha256_hex(bytes);
    // sha256sum（coreutils）与 shasum（macOS/BSD）都试一下，两个都没有就退回只报大小
    let script = format!(
        "cd {d} || exit 66\nmkdir -p \"$(dirname {p})\" || exit 67\ncat > {p} || exit 68\nchmod {m} {p} 2>/dev/null\n{{ sha256sum {p} 2>/dev/null || shasum -a 256 {p} 2>/dev/null; }} | awk '{{print $1}}'\n",
        d = q(&dir),
        p = q(rel),
        m = mode
    );
    let r = run(spec, &script, Some(bytes), 60)?;
    if !r.ok() {
        bail!("推送 {rel} 失败（退出码 {}）：{}", r.code, r.stderr.trim());
    }
    let got = r.out_text().trim().to_string();
    if !got.is_empty() && got != want {
        bail!(
            "推上去的字节和目标端算出来的指纹不一致：\n  本地 {want}\n  远端 {got}\n  \
             （文件已落在 {}:{}/{rel}，请自己核对这条通道的线路）",
            spec.label(),
            dir
        );
    }
    let entry = json!({
        "at": crate::conn::now_unix(), "act": "put", "by": whoami_host(),
        "reason": reason, "path": rel, "bytes": bytes.len(), "mode": format!("{mode:o}"),
        "sha256": want, "verifiedLocally": !got.is_empty(),
    });
    let _ = ledger_append(spec, &dir, &entry);
    Ok(if got.is_empty() { want } else { got })
}

/// 拉文件（字节原样回来）。
pub fn get(spec: &SshSpec, expires_unix: u64, rel: &str, reason: &str) -> Result<Vec<u8>> {
    preflight(spec, expires_unix)?;
    check_rel(rel)?;
    let dir = dir_of(spec);
    let script = format!("cd {d} || exit 66\ncat {p} || exit 69\n", d = q(&dir), p = q(rel));
    let r = run(spec, &script, None, 120)?;
    if !r.ok() {
        bail!("拉取 {rel} 失败（退出码 {}）：{}", r.code, r.stderr.trim());
    }
    let entry = json!({
        "at": crate::conn::now_unix(), "act": "pull", "by": whoami_host(),
        "reason": reason, "path": rel, "bytes": r.stdout.len(),
    });
    let _ = ledger_append(spec, &dir, &entry);
    Ok(r.stdout)
}

/// 与 HTTP 通道**同形**的执行结果 JSON —— 脚本可以两条路共用一套解析。
///
/// 诚实的一点：两条流是分开收的，这里按「先 stdout 后 stderr」拼（不是真实交错）。
pub fn exec_json(spec: &SshSpec, cmd: &str, out: &SshOut) -> Value {
    let mut merged = out.stdout.clone();
    if !out.stderr.trim().is_empty() {
        if !merged.is_empty() && !merged.ends_with(b"\n") {
            merged.push(b'\n');
        }
        merged.extend_from_slice(out.stderr.as_bytes());
    }
    let (tail, trunc) = log_tail(&merged);
    json!({"exec": {
        "id": "",
        "transport": "ssh",
        "host": spec.label(),
        "cmd": cmd,
        "status": if out.timed_out { "timeout" } else if out.code == 0 { "succeeded" } else { "failed" },
        "exitCode": out.code,
        "durationMs": out.ms,
        "logTail": tail,
        "logTruncated": trunc,
        "sshError": out.ssh_error,
    }})
}

/// 读通道元数据（`status` 用）。
pub fn meta(spec: &SshSpec) -> Result<Value> {
    let dir = dir_of(spec);
    let r = run(spec, &format!("cd {} && cat {}", q(&dir), META), None, 20)?;
    if !r.ok() {
        bail!("读不到通道元数据（{}/{META}，退出码 {}）：{}", dir, r.code, r.stderr.trim());
    }
    let v: Value = serde_json::from_slice(&r.stdout).map_err(|e| anyhow!("元数据不是 JSON：{e}"))?;
    Ok(v)
}

/// 读账本尾巴（最近 n 条）。
pub fn ledger_tail(spec: &SshSpec, n: usize) -> Result<Vec<Value>> {
    let dir = dir_of(spec);
    let r = run(spec, &format!("cd {} && tail -n {} {}", q(&dir), n, LEDGER), None, 20)?;
    if r.code == 69 || !r.ok() {
        return Ok(Vec::new()); // 还没干过活 = 账本文件还不存在，不算错
    }
    Ok(r.out_text()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect())
}

/// 收线：先认一下「这确实是一条通道」（目录里有我们写的元数据），再删；认不出来就拒删。
pub fn close(spec: &SshSpec, purge: bool) -> Result<bool> {
    let dir = dir_of(spec);
    if !purge {
        return Ok(false);
    }
    let script = format!(
        "cd {d} 2>/dev/null || exit 70\ntest -f {m} || exit 71\ncd .. && rm -rf {b}\n",
        d = q(&dir),
        m = META,
        b = q(&basename(&dir))
    );
    let r = run(spec, &script, None, 60)?;
    match r.code {
        70 => bail!("目录不存在（{}），不用删", dir),
        71 => bail!(
            "不肯删 {}：目录里没有 {META}，看起来不是 ncc 建的通道目录。\n  \
             要删就自己动手（我们不替你在别人的目录上 rm -rf）。",
            dir
        ),
        _ => {}
    }
    if !r.ok() {
        bail!("删目录失败（退出码 {}）：{}", r.code, r.stderr.trim());
    }
    Ok(true)
}

fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().unwrap_or(t).to_string()
}

fn whoami() -> String {
    std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "nobody".into())
}

/// 本机名。**别只靠 `Command::new("hostname")`**：macOS 上 `hostname` 不在某些 PATH 里，
/// spawn 失败就会静静退回 "localhost"，而"本机名"是拿来判锁/记账本用的 —— 退化成
/// localhost 会让跨机判重失效（这里踩过：锁里的 host 与本机对不上，死锁回收被跳过）。
pub fn hostname() -> String {
    for k in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    for (bin, args) in [("/bin/hostname", &[][..]), ("/usr/bin/hostname", &[][..]), ("hostname", &[][..]), ("/usr/bin/uname", &["-n"][..])] {
        if let Ok(o) = Command::new(bin).args(args).output() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "unknown".into()
}

fn whoami_host() -> String {
    format!("{}@{}", whoami(), hostname())
}
