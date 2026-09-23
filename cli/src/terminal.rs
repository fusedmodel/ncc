// NCC Terminal（P0 · 验证形态）
//
// 官方包标识：@ncc/terminal。P0 = 能力命令台 REPL：
//   - 预置 ncc（publish / search / install / living …）
//   - POSIX 运行时状态/装配（Unix 原生；Windows 检测 WSL2、降级 MSYS2）
//   - 非内置命令交给系统 POSIX shell（Windows 上经 WSL2）执行
// 完整 GUI 渲染与平台安装器属于 P1，见 ncc-platform/prd/ncc-terminal.md。

use crate::config::CliConfig;
use anyhow::Result;
use std::env;
use std::io::{self, BufRead, Write};
use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;

pub const OFFICIAL: &str = "@ncc/terminal";
pub const VERSION: &str = "0.1.0";

/// 命令台已知命令（补全 / help 共用）。
pub const BUILTINS: &[&str] = &[
    "help", "runtime", "runtime status", "runtime setup", "ncc", "exit", "quit",
];
pub const NCC_SUBS: &[&str] = &[
    "register", "login", "logout", "me", "ns", "publish", "search", "info",
    "install", "download", "key", "terminal", "update", "profile", "nodes", "grant", "living", "p2p", "mcp",
];

/// POSIX 运行时一句话摘要。
pub fn runtime_summary() -> String {
    #[cfg(unix)]
    {
        "native POSIX (Unix shell)".to_string()
    }
    #[cfg(windows)]
    {
        if wsl_available() {
            "WSL2".to_string()
        } else if msys_available() {
            "MSYS2 / busybox".to_string()
        } else {
            "missing (run `ncc terminal setup`)".to_string()
        }
    }
}

#[cfg(windows)]
fn wsl_available() -> bool {
    Command::new("wsl")
        .arg("--status")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn msys_available() -> bool {
    // 常见 MSYS2 安装路径下存在 bash.exe 即视为可用（无需管理员）。
    let candidates = [
        r"C:\msys64\usr\bin\bash.exe",
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
    ];
    candidates.iter().any(|p| std::path::Path::new(p).exists())
}

/// 打印环境 / POSIX 运行时状态（`ncc terminal status`）。
pub fn status(cfg: &CliConfig) -> String {
    #[cfg(unix)]
    let os = env::consts::OS.to_string();
    #[cfg(windows)]
    let os = format!("{} (windows)", env::consts::OS);

    let mut out = format!(
        "NCC Terminal  {OFFICIAL} v{VERSION}  (official)\n\
         os            {os}\n\
         base          {}\n\
         posix runtime {}",
        cfg.base_url(),
        runtime_summary()
    );
    #[cfg(windows)]
    {
        out.push_str(&format!("\n  wsl available : {}\n  msys available: {}", wsl_available(), msys_available()));
    }
    #[cfg(unix)]
    {
        let sh = env::var("SHELL").unwrap_or_else(|_| "sh".into());
        out.push_str(&format!("\n  shell         : {sh}"));
    }
    out
}

/// 装配 POSIX 运行时（`ncc terminal setup`）。
pub fn setup(cfg: &CliConfig) -> Result<()> {
    print!("{}", setup_text(cfg));
    println!();
    Ok(())
}

/// setup 的纯文本（REPL / TUI / 子命令共用）。
pub fn setup_text(_cfg: &CliConfig) -> String {
    #[cfg(unix)]
    {
        "Unix 已是原生 POSIX，无需装配。".to_string()
    }
    #[cfg(windows)]
    {
        if wsl_available() {
            "✅ WSL2 已就绪，可直接使用 POSIX 命令。".to_string()
        } else {
            let mut s = String::from("未检测到 WSL2。两种方式任选其一：\n");
            s.push_str("  1) 管理员 PowerShell 运行:  wsl --install   （推荐，随后重启并 ncc terminal setup）\n");
            s.push_str("  2) 安装 MSYS2（无需管理员），ncc terminal 将自动降级使用其 POSIX 命令。\n");
            s.push_str("再次运行 `ncc terminal setup` 完成装配。");
            s
        }
    }
}

fn combine_output(out: &std::process::Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    let e = String::from_utf8_lossy(&out.stderr);
    if !e.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&e);
    }
    s.trim_end().to_string()
}

/// 以当前 ncc 二进制执行子命令并捕获输出（TUI 用）。
pub fn run_ncc_capture(args: &[String]) -> Result<String> {
    let exe = env::current_exe().unwrap_or_else(|_| "ncc".into());
    let out = Command::new(exe).args(args).output()?;
    Ok(combine_output(&out))
}

/// 交给系统 POSIX shell 执行并捕获输出（TUI 用）。
pub fn shell_capture(line: &str) -> Result<String> {
    let prefix = shell_prefix();
    let out = Command::new(&prefix[0]).args(&prefix[1..]).arg(line).output()?;
    Ok(combine_output(&out))
}

/// 版本号比较：`a` 是否比 `b` 新。
///
/// ⚠️ **不能用字符串比较**。这里原先写的是 `latest > cur`，字典序在个位数小版本上
/// 碰巧与语义一致，但 `"0.9" > "0.10"` 为**真** —— 于是从 0.9 升到 0.10 时
/// `update` 会报「已是最新」，**用户永远收不到那次更新的提示**。
///
/// 按 `.` 切分逐段做数值比较，段数不同时缺的补 0（`0.2` 与 `0.2.0` 等价）；
/// 数字部分相同则「没有预发布后缀的」更新（`1.0.0` > `1.0.0-rc1`）。
/// 解析不了的段当 0，不 panic —— 这是版本提示，不是校验。
fn version_gt(a: &str, b: &str) -> bool {
    fn split(v: &str) -> (Vec<u64>, bool) {
        let (nums, pre) = match v.split_once('-') {
            Some((n, p)) => (n, !p.is_empty()),
            None => (v, false),
        };
        (
            nums.split('.')
                .map(|x| x.trim().parse::<u64>().unwrap_or(0))
                .collect(),
            pre,
        )
    }

    let (na, pa) = split(a);
    let (nb, pb) = split(b);
    for i in 0..na.len().max(nb.len()) {
        let (x, y) = (
            na.get(i).copied().unwrap_or(0),
            nb.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    // 数字部分相同：没有预发布后缀的那个更新
    pb && !pa
}

/// 检查官方包 / CLI 新版本。给 `ncc upgrade --check` 用，Terminal 命令台的 `update` 也调它。
pub fn update_check() -> String {
    let cur = env!("CARGO_PKG_VERSION");
    let mut s = format!("NCC CLI v{cur} · official package {OFFICIAL} v{VERSION}");
    let base = release_base();
    let api = env::var("NCC_UPDATE_URL")
        .unwrap_or_else(|_| "https://api.github.com/repos/fusedmodel/ncc/releases/latest".into());
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .build();
    match agent
        .get(&api)
        .set("User-Agent", "ncc-cli")
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(resp) => match resp.into_string() {
            Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(v) => {
                    if let Some(tag) = v["tag_name"].as_str() {
                        let latest = tag.trim_start_matches('v');
                        if version_gt(latest, cur) {
                            s.push_str(&format!("\n  → 发现新版本 v{latest}: {base}"));
                        } else {
                            s.push_str("\n  已是最新版本。");
                        }
                    } else {
                        s.push_str("\n  （无法解析远端版本号）");
                    }
                }
                Err(_) => s.push_str("\n  （无法解析远端响应）"),
            },
            Err(_) => s.push_str("\n  （读取远端响应失败）"),
        },
        Err(e) => {
            s.push_str(&format!("\n  检查失败: {e}\n  可访问 {base} 手动更新"));
        }
    }
    s
}

/// 发布基址（`ncc upgrade` 也从这里拼资产 URL，故 pub）。
pub fn release_base() -> String {
    env::var("NCC_RELEASE_BASE")
        .unwrap_or_else(|_| "https://github.com/fusedmodel/ncc/releases/latest".into())
}

/// 挑选 POSIX 命令执行前缀（Windows 经 WSL2，缺省降级 cmd；Unix 用 sh）。
fn shell_prefix() -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["sh".into(), "-c".into()]
    }
    #[cfg(windows)]
    {
        if wsl_available() {
            vec!["wsl".into(), "-e".into(), "sh".into(), "-lc".into()]
        } else {
            vec!["cmd".into(), "/C".into()]
        }
    }
}

fn run_shell(line: &str) -> Result<()> {
    let prefix = shell_prefix();
    let cmd = if prefix[0] == "wsl" {
        // Windows：交给 WSL 内的 POSIX shell
        Command::new(&prefix[0])
            .args(&prefix[1..])
            .arg(line)
            .status()
    } else {
        Command::new(&prefix[0]).args(&prefix[1..]).arg(line).status()
    };
    match cmd {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("shell 执行失败: {e}")),
    }
}

/// 用同一 ncc 二进制执行子命令（预置 ncc：publish/search/install/living…）。
fn run_ncc(args: &[String]) -> Result<()> {
    let exe = env::current_exe().unwrap_or_else(|_| "ncc".into());
    let st = Command::new(exe).args(args).status();
    match st {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("ncc 执行失败: {e}")),
    }
}

pub const HELP: &str = "\
内置命令：
  help                      显示本帮助
  runtime status            查看 POSIX 运行时 / 环境状态
  runtime setup             装配 POSIX 运行时（Windows: WSL2/MSYS2；Unix: 原生）
  ncc <cmd…>                调用预置 ncc（publish / search / install / living …）
  ! <cmd>                   交给系统 shell 执行
  其它行                    直接交给 POSIX shell 执行（模拟 unix 命令）
  exit / quit               退出命令台";

/// 交互命令台（`ncc terminal`）。
pub fn run(cfg: &CliConfig) -> Result<()> {
    println!();
    println!("NCC Terminal — 能力命令台  ({OFFICIAL} v{VERSION} · official)");
    println!("base: {} · os: {} · posix: {}", cfg.base_url(), env::consts::OS, runtime_summary());
    println!("输入 help 查看内置命令；非内置命令交给系统 shell 执行；exit 退出。\n");

    let stdin = io::stdin();
    let mut sin = stdin.lock();
    let mut input = String::new();
    loop {
        print!("ncc> ");
        io::stdout().flush()?;
        input.clear();
        let n = sin.read_line(&mut input)?;
        if n == 0 {
            println!();
            break; // EOF
        }
        let raw = input.trim_end();
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let first = parts.next().unwrap_or("");
        match first {
            "exit" | "quit" | "q" => {
                println!();
                break;
            }
            "help" | "?" => println!("{HELP}"),
            "runtime" => {
                let sub = parts.next().unwrap_or("status");
                match sub {
                    "status" => println!("{}", status(cfg)),
                    "setup" => setup(cfg)?,
                    _ => println!("用法: runtime status | runtime setup"),
                }
            }
            "ncc" => {
                let args: Vec<String> = parts.map(|s| s.to_string()).collect();
                if args.is_empty() {
                    println!("用法: ncc <子命令>，如 ncc install @org/pkg");
                } else {
                    run_ncc(&args)?;
                }
            }
            "!" => {
                let rest = line[1..].trim();
                if !rest.is_empty() {
                    run_shell(rest)?;
                }
            }
            _ => {
                // 其余一律交给 POSIX shell（命令台 = 命令入口）
                if let Err(e) = run_shell(line) {
                    println!("✗ {e:#}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::version_gt;

    #[test]
    fn newer_patch_and_minor_are_detected() {
        assert!(version_gt("0.1.2", "0.1.1"));
        assert!(version_gt("0.2.0", "0.1.9"));
        assert!(version_gt("1.0.0", "0.9.9"));
        assert!(!version_gt("0.1.1", "0.1.2"));
        assert!(!version_gt("0.1.1", "0.1.1"));
    }

    // 这个用例就是当初漏掉的那格：字典序下 "0.9" > "0.10" 为真，会把升级判成
    // 「已是最新」，用户永远收不到提示。
    #[test]
    fn double_digit_segments_are_numeric_not_lexicographic() {
        assert!("0.9" > "0.10", "前提：字典序确实是这么比的（否则这个用例没意义）");
        assert!(!version_gt("0.9", "0.10"), "0.10 比 0.9 新，不能报「已是最新」");
        assert!(version_gt("0.10", "0.9"));
        assert!(version_gt("0.1.10", "0.1.9"));
        assert!(version_gt("1.10.0", "1.9.0"));
        assert!(version_gt("0.100", "0.99"), "0.100 比 0.99 新（数值比较，不是字典序）");
    }

    #[test]
    fn differing_segment_counts_pad_with_zero() {
        assert!(!version_gt("0.2", "0.2.0"), "0.2 与 0.2.0 等价");
        assert!(!version_gt("0.2.0", "0.2"));
        assert!(version_gt("0.2.1", "0.2"));
        assert!(version_gt("1.0", "0.9.9"));
    }

    #[test]
    fn prerelease_loses_to_the_release() {
        assert!(version_gt("1.0.0", "1.0.0-rc1"));
        assert!(!version_gt("1.0.0-rc1", "1.0.0"));
        // 数字部分更高的预发布仍然更新
        assert!(version_gt("1.0.1-rc1", "1.0.0"));
    }

    // 解析不了的段当 0，不能 panic —— 这是版本提示，不是校验。
    #[test]
    fn garbage_does_not_panic() {
        assert!(!version_gt("abc", "0.1.0"));
        assert!(!version_gt("", ""));
        assert!(version_gt("1.0.0", "x.y.z"));
    }
}
