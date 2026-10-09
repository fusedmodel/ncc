//! `ncc help` / 参数报错的中文化。
//!
//! 为什么需要这一层：**clap 4 不做本地化** —— `Usage:` / `Commands:` / `error: unexpected
//! argument 'x' found` / `Print help …` 这些模板写死在 `clap_builder` 里
//!（`output/help_template.rs`、`error/format.rs`、`error/kind.rs`）。本仓库的约定是
//! "用户可见输出用中文"，所以**在边界上换一次**：拿到 clap 渲染好的文本，按它的模板逐条
//! 替换，再自己打印（`clap::Error::exit()` 会直接把英文吐出去，不能再用它）。
//!
//! 三条纪律：
//!
//! 1. **只换 clap 自己的模板**。参数说明、子命令说明、我们自己的校验消息一个字都不动 ——
//!    它们本来就是中文，乱动会破坏既有输出（顺带也意味着这个模块**不碰** stdout 协议流）。
//! 2. **认不出来就原样留着**。漏一句看得见（下一次跑 help 就发现），改错了看不见 ——
//!    所以字典是"精确模板 + 前缀模板"，没有模糊匹配、没有猜。
//! 3. **模板要跟着 clap 版本走**。每条都对着 clap 源码里的一行；单测直接跑**真实的
//!    clap 输出**（`Cli::try_parse_from`），clap 升版换了文案，测试当场红。

use std::io::Write;

/// 固定短语（里面**不含**引号参数）。**长的必须排在短的之前**：逐个 `replace` 时，
/// 若短的先换掉，长的就再也匹配不上了（`Print help` 与 `Print help (see …)` 就是这样）。
///
/// 右侧尽量保留 clap 的原结构（方括号 / 冒号），这样"哪些是它生成的、哪些是我们的"
/// 在终端里一眼分得清 —— 也方便下一个人对着 clap 源码核对。
const PHRASES: &[(&str, &str)] = &[
    // —— 帮助模板里写死的标题（`output/help_template.rs`）——
    ("Usage:", "用法:"),
    ("Commands:", "子命令:"),
    ("Options:", "选项:"),
    ("Arguments:", "参数:"),
    ("Possible values:", "可选值:"),
    ("[possible values: ", "[可选值: "),
    ("[default: ", "[默认: "),
    ("[env: ", "[环境变量: "),
    ("[short aliases: ", "[短别名: "),
    ("[aliases: ", "[别名: "),
    // —— 隐式 `help` 子命令与 `-h/--help/-V/--version` 的说明（clap 生成）——
    ("Print this message or the help of the given subcommand(s)", "打印帮助（或指定子命令的帮助）"),
    ("Print help (see a summary with '-h')", "打印帮助（`-h` 看简版）"),
    ("Print help (see more with '--help')", "打印帮助（`--help` 看详细版）"),
    ("Print help", "打印帮助"),
    ("Print version", "打印版本"),
    // —— 报错的前后缀（`error/format.rs`）——
    ("error:", "错误:"),
    ("tip:", "提示:"),
    ("the following required arguments were not provided:", "缺少必填参数："),
    (" one or more of the other specified arguments", " 或其它已给的参数"),
    ("unknown cause", "原因不明"),
];

/// 带引号参数的模板：左侧是 clap 的原文（`{}` 处原本是**单引号包起来**的一段），
/// 右侧是中文（`{0}` / `{1}` 按抽取顺序填）。只做**整行相等**或**行首前缀**两种匹配。
const TEMPLATES: &[(&str, &str)] = &[
    // —— 报错末尾的"去哪儿看更多"（`error/format.rs` 的 `try_help`）——
    ("For more information, try {}", "更多信息：试 `{0}`"),
    // —— ErrorKind 相关的动态上下文 ——
    ("unrecognized subcommand {}", "没有这个子命令「{0}」"),
    ("unexpected argument {} found", "无法识别的参数「{0}」"),
    ("invalid value {} for {}", "「{1}」的取值不合法：「{0}」"),
    ("a value is required for {} but none was supplied", "「{0}」需要一个值，但没给"),
    ("equal sign is needed when assigning values to {}", "给「{0}」赋值要写成 `--参数=值`"),
    ("the argument {} cannot be used multiple times", "「{0}」不能重复给"),
    ("the argument {} cannot be used with {}", "「{0}」不能和「{1}」一起用"),
    ("the subcommand {} cannot be used with {}", "子命令「{0}」不能和「{1}」一起用"),
    ("unexpected value {} for {} found; no more were expected", "「{1}」多给了一个值：「{0}」"),
    ("{} requires a subcommand but one was not provided", "「{0}」需要一个子命令，但没给"),
    // —— tip（`parser/parser.rs` / `error/mod.rs`）——
    ("to pass {} as a value, use {}", "想把「{0}」当成取值传，请用 `-- {0}`"),
    ("a similar subcommand exists: {}", "你是不是想用这个子命令「{0}」"),
    ("some similar subcommands exist: {}", "是不是这些子命令之一「{0}」"),
    ("subcommand {} exists; to use it, remove the {} before it", "有这个子命令「{0}」；要去掉它前面的 {1} 才认"),
];

/// 把 clap 渲染好的帮助 / 报错文本换成中文（**不打印**，只返回）。
pub fn localize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&one_line(line));
    }
    out
}

/// 打印 clap 的帮助 / 报错并**退出**：帮助与版本走 stdout，报错走 stderr
/// （与 clap `Error::exit()` 的语义一致 —— 退出码也沿用它的）。
pub fn exit_with(err: clap::Error) -> ! {
    let text = localize(&err.to_string());
    // 流与退出码都沿用 clap 的语义（帮助 / 版本走 stdout 且 0；报错走 stderr 且 2）。
    // 写失败（管道关掉之类）不该变成恐慌：与 clap 自己的 `exit` 一样吞掉。
    // `flush` 是必要的：下面直接 `process::exit`，不会走 Rust 的清理路径。
    let _ = if err.use_stderr() {
        let mut h = std::io::stderr().lock();
        h.write_all(text.as_bytes()).and_then(|_| h.flush())
    } else {
        let mut h = std::io::stdout().lock();
        h.write_all(text.as_bytes()).and_then(|_| h.flush())
    };
    std::process::exit(err.exit_code());
}

fn one_line(line: &str) -> String {
    let indent: String = line.chars().take_while(|c| *c == ' ').collect();
    let body = &line[indent.len()..];
    if body.is_empty() {
        return line.to_string();
    }
    // `error: …` / `tip: …`：前缀是固定的两类，先摘掉再匹配 —— 否则模板得为
    // "带前缀"与"不带前缀"各写一份（漏一份就是漏一半）。
    for (en, zh) in [("error:", "错误:"), ("tip:", "提示:")] {
        if let Some(rest) = body.strip_prefix(en) {
            let rest = rest.trim_start();
            let zh_rest = by_template(rest).unwrap_or_else(|| phrases(rest));
            return format!("{indent}{zh} {zh_rest}");
        }
    }
    if let Some(zh) = by_template(body) {
        return format!("{indent}{zh}");
    }
    format!("{indent}{}", phrases(body))
}

/// 固定短语表：逐个替换（表内顺序即优先级，见 `PHRASES` 的注释）。
fn phrases(body: &str) -> String {
    let mut s = body.to_string();
    for (en, zh) in PHRASES {
        s = s.replace(en, zh);
    }
    s
}

/// 按带引号的模板换一行；不匹配返回 `None`（宁可漏，也不猜）。
fn by_template(body: &str) -> Option<String> {
    let (tpl, args) = unquote(body);
    for (en, zh) in TEMPLATES {
        if tpl == *en {
            return Some(fill(zh, &args));
        }
    }
    // 前缀匹配：`invalid value …` 这类后面还会跟我们的中文校验消息（`: 具体原因`），
    // 只换 clap 那一段，余下的原样接上。
    for (en, zh) in TEMPLATES {
        if let Some(rest) = tpl.strip_prefix(en) {
            return Some(format!("{}{rest}", fill(zh, &args)));
        }
    }
    None
}

/// 把 `'…'` 抽成 `{}`（返回模板与抽到的内容）。**不处理嵌套**：clap 的文案里没有嵌套。
fn unquote(body: &str) -> (String, Vec<String>) {
    let mut tpl = String::with_capacity(body.len());
    let mut args = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('\'') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('\'') else { break };
        tpl.push_str(&rest[..start]);
        tpl.push_str("{}");
        args.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    tpl.push_str(rest);
    (tpl, args)
}

fn fill(zh: &str, args: &[String]) -> String {
    let mut s = zh.to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

/* ================= 单测 ================= */

#[cfg(test)]
mod tests {
    use clap::Parser;

    /// 单测只关心"clap 生成的文案"，所以复刻一棵最小的命令树：真正的 `Cli` 在 `main.rs`
    /// 里（那是 bin crate 的私有类型，这里拿不到）。用最小树的好处是：clap 换了任何一条
    /// 模板文案，这里都会红，且失败信息很短。
    #[derive(Parser)]
    #[command(name = "ncc", version, about = "最小命令树（测中文帮助与报错）")]
    struct T {
        /// 服务地址
        #[arg(long, global = true)]
        base: Option<String>,
        #[command(subcommand)]
        cmd: C,
    }

    #[derive(clap::Subcommand)]
    enum C {
        /// 检索
        Search {
            /// 关键词
            query: Option<String>,
            /// 只看某个类型
            #[arg(long, value_parser = ["api", "skill"])]
            kind: Option<String>,
        },
        /// 发布
        Publish {
            /// 类型
            #[arg(long)]
            kind: String,
            /// 名字
            #[arg(long)]
            name: String,
        },
    }

    fn render(argv: &[&str]) -> (String, bool) {
        match T::try_parse_from(argv) {
            Ok(_) => panic!("这条本该解析失败：{argv:?}"),
            Err(e) => (super::localize(&e.to_string()), e.use_stderr()),
        }
    }

    /// 帮助：标题与隐式 `help` 参数都该是中文。
    #[test]
    fn 帮助全中文() {
        for argv in [vec!["ncc", "help"], vec!["ncc", "help", "search"], vec!["ncc", "-h"], vec!["ncc", "search", "--help"]] {
            let (text, stderr) = render(&argv);
            assert!(!stderr, "帮助走 stdout：{argv:?}");
            for en in ["Usage:", "Commands:", "Options:", "Print help", "Print this message"] {
                assert!(!text.contains(en), "帮助里还剩英文「{en}」：\n{text}");
            }
            assert!(text.contains("用法:"), "{argv:?} 没有「用法:」：\n{text}");
        }
    }

    /// 报错：`error:` / `tip:` / `Usage:` / 「更多信息」都该是中文，且**参数名原样保留**。
    #[test]
    fn 报错全中文且保留参数名() {
        let (text, stderr) = render(&["ncc", "nope"]);
        assert!(stderr, "报错走 stderr");
        assert!(text.contains("错误: 没有这个子命令「nope」"), "{text}");
        assert!(text.contains("用法: ncc"), "{text}");

        let (text, _) = render(&["ncc", "search", "--nope"]);
        assert!(text.contains("错误: 无法识别的参数「--nope」"), "{text}");
        assert!(text.contains("提示: 想把「--nope」当成取值传"), "{text}");

        let (text, _) = render(&["ncc", "search", "--kind", "nope"]);
        assert!(text.contains("「--kind <KIND>」的取值不合法：「nope」"), "{text}");
        assert!(text.contains("[可选值: api, skill]"), "{text}");

        let (text, _) = render(&["ncc", "publish"]);
        assert!(text.contains("缺少必填参数："), "{text}");
        assert!(text.contains("--kind <KIND>"), "参数名要原样留着：{text}");
        assert!(text.contains("更多信息：试 `--help`"), "{text}");
    }

    /// 认不出来的英文**原样留着**：不能把我们的中文说明或别的文案改坏。
    #[test]
    fn 只换_clap_的模板() {
        let s = "  --name <NAME>  这份包**是什么**（profile）。不给就按 kind 推导 [默认: \"\"]";
        assert_eq!(super::localize(s), s);
        // 我们自己写的报错（任何命令行）不该被动
        assert_eq!(super::localize("✗ 找不到目标 hub（用 ncc target list 看全部）"), "✗ 找不到目标 hub（用 ncc target list 看全部）");
    }

    /// 短语表**不许自相遮挡**：排在后面的短语不能包含前面的（否则永远换不到）。
    #[test]
    fn 短语表没有自遮挡() {
        for (i, (a, _)) in super::PHRASES.iter().enumerate() {
            for (b, _) in super::PHRASES.iter().skip(i + 1) {
                assert!(!b.contains(a), "「{a}」排在了「{b}」前面，后者永远换不到");
            }
        }
    }

    /// 模板表：`{}` 的个数要与右侧用到的 `{n}` 对得上（写错的模板比不写更坏）。
    #[test]
    fn 模板占位符自洽() {
        for (en, zh) in super::TEMPLATES {
            let holes = en.matches("{}").count();
            let mut used: Vec<usize> = zh
                .match_indices('{')
                .filter_map(|(i, _)| zh[i..].chars().nth(1)?.to_digit(10))
                .map(|d| d as usize)
                .collect();
            used.dedup();
            for u in &used {
                assert!(*u < holes, "「{zh}」用了 {{{u}}}，但英文模板只有 {holes} 个引号位");
            }
        }
    }
}
