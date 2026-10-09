//! `ncc scaffold` —— **脚手架规范**（`SCAFFOLD.md`）工具链。
//!
//! 为什么脚手架是**一份 markdown**，不是一份压缩包/模板目录（用户原话：「一般情况下
//! scaffold 是不是只需要 md 就可以了，因为 vibe coding 代码速度很快」）：
//!
//! * 模板的时效性由"生成它的那一刻"决定。一个三个月前的目录树复制进今天的仓库，
//!   带来的往往是过时的依赖版本与不再成立的约定 —— 而 Agent 每次读一遍规范、
//!   **贴着当前仓库**生成，产出的是当下正确的东西。
//! * 所以这份 md 的价值不在"我给你几十个文件"，而在**"我知道这类工程该长什么样"**：
//!   结构、约定、交付清单（`outputs`）、算力前置（`requires`）、以及**怎么算生成成功**
//!   （「验收」段里的可执行命令）。少最后一条，脚手架就退化成一段散文。
//!
//! 与包工具链的关系：`SCAFFOLD.md` 可以单发（`ncc publish --kind scaffold --file …`），
//! 也可以作为 `profile=scaffold` 的 hur 包的本体（`ncc hur pack`，模板放 `assets/template/`）。
//! 本模块管**前面那条线**：规范、生成、校验、适配。
//!
//! **离线铁律**：spec / init / verify 全程本地；只有 `fit --remote` 会问平台一句
//! "谁满足这份需求"（判定权始终在本机：服务端只粗筛）。

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

use hur_core::{need, spec, spec_kit, tpl};

use crate::config::{self, CliConfig};

#[derive(Subcommand)]
pub enum ScaffoldCmd {
    /// 规范总览：frontmatter 字段 / 规则 S1~S8 / 骨架模板（`--json` 给 SDK 与工具用）
    Spec {
        #[arg(long)]
        json: bool,
    },
    /// 生成一份 `SCAFFOLD.md`（技术栈从目录里认；认不出要显式 `--stack`）
    Init(ScaffoldInitArgs),
    /// 校验 `SCAFFOLD.md`（S1~S8，全程离线）；`--run` 真跑「验收」里的命令
    Verify(ScaffoldVerifyArgs),
    /// 适配：这份脚手架要的算力（`requires`），本机 / 集群满不满足
    Fit(ScaffoldFitArgs),
}

#[derive(Args, Clone)]
pub struct ScaffoldInitArgs {
    /// 写到哪个目录（默认当前目录；目录不存在就新建）
    #[arg(default_value = ".")]
    pub dir: PathBuf,
    /// 脚手架名（ascii slug；默认取目录名）
    #[arg(long, default_value = "")]
    pub name: String,
    /// 一句话说清它生成什么（默认「<名> 工程骨架」）
    #[arg(long, default_value = "")]
    pub desc: String,
    /// 技术栈，可重复或逗号分隔：`--stack rust,cargo`
    #[arg(long = "stack", value_delimiter = ',')]
    pub stack: Vec<String>,
    /// 交付清单（要产出哪些文件），逗号分隔
    #[arg(long = "outputs", alias = "output", value_delimiter = ',')]
    pub outputs: Vec<String>,
    /// 算力前置（`tool:cargo` / `mem>=2G` …），逗号分隔
    #[arg(long = "requires", alias = "require", value_delimiter = ',')]
    pub requires: Vec<String>,
    /// 「验收」里的命令，可重复：`--accept "cargo build"`
    #[arg(long = "accept", value_delimiter = ',')]
    pub accept: Vec<String>,
    /// 已存在就覆盖
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct ScaffoldVerifyArgs {
    /// `SCAFFOLD.md` 或它所在的目录（默认当前目录）
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 真跑「验收」段里的命令（在 md 所在目录里跑；失败就是没完成）
    #[arg(long)]
    pub run: bool,
    /// 提醒也算不通过（退出码 1）
    #[arg(long)]
    pub strict: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct ScaffoldFitArgs {
    /// `SCAFFOLD.md` 或它所在的目录（默认当前目录）
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// 额外需求（如 `--need "gpu>=1"`），与 md 里的 `requires` 合并
    #[arg(long, default_value = "")]
    pub need: String,
    /// 读哪一份算力画像（默认 `~/.ncc/compute-profile.json`）
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// 本机没有画像时先采集一次（几秒）
    #[arg(long)]
    pub scan: bool,
    /// 问平台：谁满足这份需求（需要目标声明 compute 能力）
    #[arg(long)]
    pub remote: bool,
    /// `--remote`：只看自己上报的画像
    #[arg(long)]
    pub mine: bool,
    /// `--remote`：把候选画像拉回本机复核（服务端只粗筛）
    #[arg(long)]
    pub strict: bool,
    /// 不满足就退出码 1（给 CI / 生成前的前置检查用）
    #[arg(long)]
    pub check: bool,
    #[arg(long)]
    pub json: bool,
}

impl ScaffoldCmd {
    /// 这条命令要不要服务端能力：`spec / init / verify` 都在本地，
    /// `fit` 默认看本机画像 —— `--remote` 那条线由实现里 `ensure` 一次（那里能给
    /// "这个目标没有 compute 能力，去哪个目标问"的具体下一步）。
    pub fn capability(&self) -> Option<&'static str> {
        None
    }

    /// 这条子命令带 `--json` 吗（决定"提示走 stderr 还是 stdout"，见 `main::wants_json_stdout`）。
    pub fn json_stdout(&self) -> bool {
        match self {
            ScaffoldCmd::Spec { json } => *json,
            ScaffoldCmd::Init(a) => a.json,
            ScaffoldCmd::Verify(a) => a.json,
            ScaffoldCmd::Fit(a) => a.json,
        }
    }
}

pub fn run(cfg: &CliConfig, cmd: &ScaffoldCmd) -> Result<()> {
    match cmd {
        ScaffoldCmd::Spec { json } => spec_cmd(*json),
        ScaffoldCmd::Init(a) => init(a.clone()),
        ScaffoldCmd::Verify(a) => verify(a.clone()),
        ScaffoldCmd::Fit(a) => fit(cfg, a.clone()),
    }
}

/* ================= spec ================= */

/// frontmatter 的字段表 —— `--json` 与默认输出共用一份（写两份必然漂）。
const FIELDS: [(&str, &str); 8] = [
    ("spec", "规范号，必须是 ncc-scaffold/v1"),
    ("name", "ascii slug（小写字母 / 数字 / - _）"),
    ("description", "一句话：它生成什么"),
    ("stack", "技术栈：`[rust, cargo]`（生成者据此选工具与约定）"),
    ("requires", "算力前置：`[tool:cargo, mem>=2G]`（与 `ncc profile node fit` 同一套语法）"),
    ("outputs", "交付清单：要有哪些文件 —— 这是验收表，不是建议"),
    ("when", "什么时候该用它"),
    ("whenNot", "什么时候**不该**用它（比 when 更值钱：用错地方会毁掉已有工程）"),
];

fn spec_cmd(json_out: bool) -> Result<()> {
    let rules: Vec<serde_json::Value> = spec_kit::SCAFFOLD_RULES
        .iter()
        .map(|r| json!({ "rule": r.rule, "what": r.what }))
        .collect();
    let fields: Vec<serde_json::Value> = FIELDS.iter().map(|(k, v)| json!({ "field": k, "what": v })).collect();
    let template = spec_kit::scaffold_template(
        "my-scaffold",
        "在这里用一句话说清它生成什么",
        &["rust".into(), "cargo".into()],
        &spec_kit::default_outputs("my-scaffold", &["rust".into()]),
        &spec_kit::default_requires(&["rust".into()]),
    );
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "spec": spec_kit::SCAFFOLD_SPEC,
                "file": spec_kit::SCAFFOLD_MD,
                "fields": fields,
                "rules": rules,
                "template": template,
                "commands": {
                    "init": "ncc scaffold init . --stack rust,cargo",
                    "verify": "ncc scaffold verify . [--run]",
                    "fit": "ncc scaffold fit . [--scan] [--remote]",
                    "publish": "ncc publish --kind scaffold --name X --file SCAFFOLD.md",
                },
                "needSyntax": "cpu>=4,mem>=16G,disk>=100G,gpu>=1,gpumem>=8G,tool:docker,service>=1,tag:gpu",
            }))?
        );
        return Ok(());
    }
    println!("SCAFFOLD.md 规范 {} —— **一份 md 就是一件制品**（脚手架 kind=scaffold）\n", spec_kit::SCAFFOLD_SPEC);
    println!("为什么不要模板压缩包：模板的时效性停在生成它的那一刻；而「这类工程该长什么样」");
    println!("（结构 / 约定 / 交付清单 / 前置算力 / **怎么算生成成功**）写成 md，Agent 每次都能贴着你");
    println!("当前的仓库生成 —— 代码写得快，错的是方向。\n");
    println!("frontmatter（其余正文是给生成者的施工指令）：");
    for (k, v) in FIELDS {
        println!("  {k:<12} {v}");
    }
    println!("\n正文至少要两段（规则 S5 / S6 查的就是它们）：");
    println!("  ## 目标结构   一个 ```text 目录树 —— 没有结构就没有交付物");
    println!("  ## 验收       一个 ```bash 代码块，里面是**真能跑**的命令 —— 只描述不验证的不算完成");
    println!("\n规则（校验器 `ncc scaffold verify` 逐条查，全程离线）：");
    for r in spec_kit::SCAFFOLD_RULES {
        println!("  {:<4} {}", r.rule, r.what);
    }
    println!("\n骨架模板（`ncc scaffold init` 会按你目录里的栈填好再落盘）：\n");
    println!("{template}");
    println!("下一步  ncc scaffold init . --stack <你的栈>   →   ncc scaffold verify . --run   →   ncc scaffold fit .");
    println!("发布    ncc publish --kind scaffold --name X --file SCAFFOLD.md（或打成 profile=scaffold 的 hur 包）");
    println!("机器可读  ncc scaffold spec --json");
    Ok(())
}

/* ================= init ================= */

fn init(a: ScaffoldInitArgs) -> Result<()> {
    let dir = a.dir.clone();
    // 认栈看**目标目录**（已存在的工程），其次当前目录 —— 两边都没有就要人给。
    let mut stack = a.stack.clone();
    if stack.is_empty() {
        stack = spec_kit::detect_stack(&dir);
    }
    if stack.is_empty() {
        stack = spec_kit::detect_stack(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    }
    if stack.is_empty() {
        bail!(
            "认不出技术栈（目录里没有 Cargo.toml / package.json / go.mod / pyproject.toml …）。\n\
             请显式给：`ncc scaffold init . --stack rust,cargo`（脚手架的一半价值在\
             「这类工程该长什么样」，栈不说清，生成的人只能猜）"
        );
    }
    let name = if a.name.trim().is_empty() {
        let base = if dir.as_os_str() == "." {
            std::env::current_dir().ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        } else {
            dir.file_name().map(|s| s.to_string_lossy().to_string())
        };
        tpl::slug(base.as_deref().unwrap_or("my-scaffold"))
    } else {
        a.name.trim().to_string()
    };
    let desc = if a.desc.trim().is_empty() { format!("{name} 工程骨架") } else { a.desc.trim().to_string() };
    let outputs = if a.outputs.is_empty() { spec_kit::default_outputs(&name, &stack) } else { a.outputs.clone() };
    let requires = if a.requires.is_empty() { spec_kit::default_requires(&stack) } else { a.requires.clone() };
    let accept = if a.accept.is_empty() { spec_kit::acceptance_for(&stack) } else { a.accept.clone() };

    std::fs::create_dir_all(&dir).with_context(|| format!("建目录 {} 失败", dir.display()))?;
    let file = dir.join(spec_kit::SCAFFOLD_MD);
    if file.exists() && !a.force {
        bail!("{} 已存在（加 --force 覆盖，或换 --dir）", file.display());
    }
    let text = spec_kit::scaffold_template_with(&name, &desc, &stack, &outputs, &requires, &accept);
    std::fs::write(&file, &text).with_context(|| format!("写 {} 失败", file.display()))?;

    // 生成完顺手校验一次：**不让作者自己发现 S6 没写**（占位命令当场红）。
    let issues = spec_kit::verify_scaffold(&text);
    let (errs, warns) = count(&issues);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file, "root": dir, "name": name, "stack": stack,
                "outputs": outputs, "requires": requires, "acceptance": accept,
                "errors": errs, "warnings": warns,
                "issues": issues.iter().map(issue_json).collect::<Vec<_>>(),
            }))?
        );
        if errs > 0 {
            bail!("生成的脚手架还没写完（{errs} 个错误，见上）");
        }
        return Ok(());
    }
    println!("已生成 {}", file.display());
    println!("  交付清单  {}", outputs.join(" · "));
    println!("  算力前置  {}", if requires.is_empty() { "（没声明 —— 有前置就写上，Agent 才敢接）".into() } else { requires.join(" · ") });
    println!("  验收      {}", accept.join(" ; "));
    for i in &issues {
        match i.level {
            spec::Level::Error => println!("  [{} 错误] {}", i.rule, i.msg),
            spec::Level::Warn => println!("  [{} 提醒] {}", i.rule, i.msg),
            spec::Level::Info => println!("  [{} 提示] {}", i.rule, i.msg),
        }
    }
    println!("  下一步    ncc scaffold verify {} --run   →   ncc scaffold fit {}", dir.display(), dir.display());
    println!("  发布      ncc publish --kind scaffold --name {} --file {}", name, file.display());
    if errs > 0 {
        bail!("生成的脚手架还没写完（{errs} 个错误，见上）");
    }
    Ok(())
}

/* ================= verify ================= */

fn verify(a: ScaffoldVerifyArgs) -> Result<()> {
    let (dir, file) = locate(&a.path)?;
    let text = std::fs::read_to_string(&file).with_context(|| format!("读 {} 失败", file.display()))?;
    let issues = spec_kit::verify_scaffold(&text);
    let (mut errs, warns) = count(&issues);

    // `--run`：跑「验收」段里的命令。**这不是"顺手帮你跑一下"** —— 它是这份脚手架的
    // 定义的一部分（S6）：说得出怎么算成功，才算写完。
    let mut runs: Vec<serde_json::Value> = Vec::new();
    let sc = spec_kit::parse_scaffold(&text).ok();
    let cmds = sc
        .as_ref()
        .map(|s| {
            let sec = acc_section(&s.body);
            spec_kit::acceptance_commands(&sec)
        })
        .unwrap_or_default();
    if a.run {
        if cmds.is_empty() {
            eprintln!("⚠「验收」段里没有可执行的命令，没什么可跑的（S6 已经报了错）");
        }
        for c in &cmds {
            if !a.json {
                println!("  $ {c}");
            }
            let out = Command::new("sh").arg("-c").arg(c).current_dir(&dir).output();
            let (ok, code, tail) = match out {
                Ok(o) => {
                    let ok = o.status.success();
                    let mut tail = String::from_utf8_lossy(&o.stdout).to_string();
                    tail.push_str(&String::from_utf8_lossy(&o.stderr));
                    let tail: String = tail.chars().rev().take(400).collect::<Vec<char>>().into_iter().rev().collect();
                    (ok, o.status.code().unwrap_or(-1), tail.trim().to_string())
                }
                Err(e) => (false, -1, format!("起不了进程：{e}")),
            };
            if !ok {
                errs += 1;
                if !a.json {
                    println!("  ✗ 退出码 {code}；去掉这条命令、或把它改到能过（脚手架不许带着红验收发布）");
                    if !tail.is_empty() {
                        println!("{}", indent(&tail, "      "));
                    }
                }
            } else if !a.json {
                println!("  ✓ 通过");
            }
            runs.push(json!({ "cmd": c, "ok": ok, "code": code, "output": tail }));
        }
    }

    let ok = errs == 0 && (!a.strict || warns == 0);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file, "root": dir, "ok": ok, "errors": errs, "warnings": warns,
                "name": sc.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                "stack": sc.as_ref().map(|s| s.stack.clone()).unwrap_or_default(),
                "requires": sc.as_ref().map(|s| s.requires.clone()).unwrap_or_default(),
                "acceptance": { "commands": cmds, "runs": runs },
                "issues": issues.iter().map(issue_json).collect::<Vec<_>>(),
            }))?
        );
    } else {
        for i in &issues {
            match i.level {
                spec::Level::Error => println!("  [{} 错误] {}", i.rule, i.msg),
                spec::Level::Warn => println!("  [{} 提醒] {}", i.rule, i.msg),
                spec::Level::Info => println!("  [{} 提示] {}", i.rule, i.msg),
            }
        }
        if ok {
            println!("脚手架校验通过 ✔  {}", file.display());
            if !a.run && !cmds.is_empty() {
                println!("  验收      {} 条命令待跑（加 --run 真跑一遍：说的是不是真的）", cmds.len());
            }
        } else {
            println!("脚手架校验结果：{errs} 个错误，{warns} 个提醒");
        }
    }
    if !ok {
        bail!("校验未通过（S1~S8）");
    }
    Ok(())
}

/* ================= fit ================= */

fn fit(cfg: &CliConfig, a: ScaffoldFitArgs) -> Result<()> {
    let (_dir, file) = locate(&a.path)?;
    let text = std::fs::read_to_string(&file).with_context(|| format!("读 {} 失败", file.display()))?;
    let sc = spec_kit::parse_scaffold(&text).with_context(|| format!("{} 解析失败", file.display()))?;
    // 需求 = md 里的 requires + 显式 --need。**两边都过同一套判据**（`hur_core::need`），
    // 所以这里既不用翻译也不用再判一次。
    let mut expr = sc.needs_expr();
    if !a.need.trim().is_empty() {
        let extra = need::parse_need(a.need.trim())?;
        let extra = extra.iter().map(|n| n.raw.clone()).collect::<Vec<_>>().join(",");
        expr = if expr.is_empty() { extra } else { format!("{expr},{extra}") };
    }
    // 空表达式是合法的（`requires: []`）：S8 允许不声明，`fit` 就该如实回答"没前置要求"，
    // 而不是拿 `parse_need` 的空字符串报错去吓人。
    let needs = if expr.trim().is_empty() { Vec::new() } else { need::parse_need(&expr)? };

    if a.remote {
        return fit_remote_cmd(cfg, &a, &file, &sc, &expr, &needs);
    }

    // 本机画像：没有就（--scan 时）采一次，否则如实让他去采集 —— 不猜一个画像继续跑。
    let path = a.file.clone().unwrap_or_else(crate::compute::default_path);
    let mut profile = crate::compute::load_default_or(a.file.as_deref());
    if profile.is_err() && a.scan {
        let opts = crate::compute::ScanOpts::default();
        let p = crate::compute::scan(cfg, &opts)?;
        crate::compute::save(&path, &p)?;
        profile = Ok(p);
    }
    let profile = profile?;
    let verdicts = need::evaluate(&profile, &needs);
    let ok = verdicts.iter().all(|v| v.ok);

    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": file, "profile": path, "what": sc.name, "need": expr, "ok": ok,
                "verdicts": verdicts.iter().map(verdict_json).collect::<Vec<_>>(),
                "node": profile.node.name,
                "hint": "同一份需求问集群：ncc scaffold fit . --remote",
            }))?
        );
    } else {
        print!("{}", crate::compute::render_summary(&profile));
        println!();
        println!("这份脚手架（{}）要什么 —— 需求 {}", sc.name, if expr.is_empty() { "（空）".into() } else { expr.clone() });
        if needs.is_empty() {
            println!("  [S8 提示] 它没声明 requires：任务档位越大的脚手架越该写（`--stack` 认出来的工具会自动写进去）");
        }
        for v in &verdicts {
            if v.ok {
                println!("  ✓ {}", v.need);
            } else {
                println!("  ✗ {} —— 现在 {}，{}", v.need, v.actual, v.missing);
            }
        }
        if ok {
            println!("\n本机能生成 / 跑这份脚手架 ✔");
        } else {
            println!("\n本机不满足（{} 项）：换成满足的机器，或把 requirement 谈下来（`requires` 写宽一点）", verdicts.iter().filter(|v| !v.ok).count());
            println!("  问集群谁满足：ncc scaffold fit . --remote");
        }
    }
    if !ok && a.check {
        bail!("本机不满足这份脚手架的算力前置（{}）", expr);
    }
    Ok(())
}

fn fit_remote_cmd(
    cfg: &CliConfig,
    a: &ScaffoldFitArgs,
    file: &Path,
    // 远程分支不用目录（候选在自己那台机器上），但签名保留统一的 (md, 目录) 概念

    sc: &spec_kit::Scaffold,
    expr: &str,
    needs: &[need::Need],
) -> Result<()> {
    if expr.trim().is_empty() {
        bail!("这份脚手架没声明 requires（也没什么 --need）：先写清前置算力，再来问集群");
    }
    let token = config::require_token(cfg)?;
    crate::capability::ensure(cfg, "compute")?;
    let mut v = crate::compute::fit_remote(cfg, &token, expr, a.mine, a.strict)?;
    if a.strict {
        v = crate::compute::strict_refine(cfg, &token, v, needs)?;
    }
    if a.json {
        let mut out = v.clone();
        out["file"] = json!(file);
        out["what"] = json!(sc.name);
        out["need"] = json!(expr);
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("这份脚手架（{}）要什么 —— 需求 {}", sc.name, expr);
        let total = v["total"].as_i64().unwrap_or(0);
        let matched = v["matched"].as_i64().unwrap_or(0);
        let pct = if total > 0 { matched as f64 * 100.0 / total as f64 } else { 0.0 };
        println!("  可跑      {matched} / {total}（{pct:.0}%）{}", if a.strict { "（已在本机复核）" } else { "（服务端粗筛，加 --strict 复核）" });
        for n in v["nodes"].as_array().cloned().unwrap_or_default() {
            let ref_ = n["nodeRef"].as_str().unwrap_or("");
            let verdicts = n["verdicts"].as_array().cloned().unwrap_or_default();
            let ok = !verdicts.is_empty() && verdicts.iter().all(|x| x["ok"].as_bool().unwrap_or(false));
            println!("  {} {:<14} {:<24} 采集 {}", if ok { "✓" } else { "✗" }, if ref_.is_empty() { "（本机）" } else { ref_ }, n["name"].as_str().unwrap_or(""), n["updatedAt"].as_str().unwrap_or(""));
            for x in verdicts {
                if !x["ok"].as_bool().unwrap_or(true) {
                    println!("        缺 {}（现在 {}）", x["need"].as_str().unwrap_or(""), x["actual"].as_str().unwrap_or(""));
                }
            }
        }
        if matched == 0 {
            println!("  没有人满足：把 requires 放宽，或先让有能力的机器 `ncc profile node push`");
        }
    }
    if a.check && v["matched"].as_i64().unwrap_or(0) == 0 {
        bail!("集群里没有满足这份脚手架前置算力的机器（{expr}）");
    }
    Ok(())
}

/* ================= 小工具 ================= */

/// `SCAFFOLD.md` 或它所在的目录 → (目录, 文件)。
fn locate(p: &Path) -> Result<(PathBuf, PathBuf)> {
    if p.is_dir() {
        let f = p.join(spec_kit::SCAFFOLD_MD);
        if !f.exists() {
            bail!(
                "{} 里没有 {}（`ncc scaffold init {}` 生成一份，或指向文件本身）",
                p.display(),
                spec_kit::SCAFFOLD_MD,
                p.display()
            );
        }
        return Ok((p.to_path_buf(), f));
    }
    if !p.exists() {
        bail!("找不到 {}", p.display());
    }
    let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    Ok((dir, p.to_path_buf()))
}

/// 正文里的「验收」段（`--run` 要跑它）。判据**就是** S6 那份（`spec_kit::section`）——
/// 自己再写一遍切分逻辑，就会出现"检查说有 3 条、实际一条都不跑"。
fn acc_section(body: &str) -> String {
    spec_kit::section(body, spec_kit::ACCEPTANCE_KEYS).unwrap_or_default()
}

fn count(issues: &[spec::Issue]) -> (usize, usize) {
    let errs = issues.iter().filter(|i| i.level == spec::Level::Error).count();
    let warns = issues.iter().filter(|i| i.level == spec::Level::Warn).count();
    (errs, warns)
}

fn issue_json(i: &spec::Issue) -> serde_json::Value {
    json!({ "rule": i.rule, "level": format!("{:?}", i.level), "msg": i.msg })
}

fn verdict_json(v: &need::Verdict) -> serde_json::Value {
    json!({ "need": v.need, "ok": v.ok, "actual": v.actual, "missing": v.missing })
}

fn indent(s: &str, prefix: &str) -> String {
    s.lines().map(|l| format!("{prefix}{l}")).collect::<Vec<_>>().join("\n")
}

/* ================= 单测 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_认目录与文件() {
        let t = std::env::temp_dir().join(format!("ncc-scaffold-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        assert!(locate(&t).is_err(), "目录里没有 SCAFFOLD.md 就该报错并给出下一步");
        let f = t.join(spec_kit::SCAFFOLD_MD);
        std::fs::write(&f, "---\n").unwrap();
        assert_eq!(locate(&t).unwrap().1, f, "给目录 → 找目录里的 SCAFFOLD.md");
        assert_eq!(locate(&f).unwrap().0, t, "给文件 → 目录是它的父目录");
        assert!(locate(&t.join("nope.md")).is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn 验收段提取与_run_一致() {
        let body = "# x\n\n## 目标结构\n\n```text\na\n```\n\n## 验收\n\n```bash\ncargo build\n# 注释不算\ncargo test\n```\n\n## 边界\n\n- 无\n";
        let sec = acc_section(body);
        let cmds = spec_kit::acceptance_commands(&sec);
        assert_eq!(cmds, vec!["cargo build".to_string(), "cargo test".to_string()]);
        // 与 S6 用的是同一个函数：段里只有 `#` 注释时两边都该认为"没有命令"
        assert!(spec_kit::acceptance_commands("# 只有注释").is_empty());
        assert!(acc_section("# x\n\n## 别的段\n\n```bash\nls\n```\n").is_empty(), "没有验收段就给空");
    }

    #[test]
    fn 需求表达式合并() {
        let sc = spec_kit::parse_scaffold(
            "---\nspec: ncc-scaffold/v1\nname: d\ndescription: x\nstack: [rust]\nrequires: [tool:cargo, mem>=2G]\noutputs: [README.md]\n---\n\n## 目标结构\n\n```text\nd/\n```\n\n## 验收\n\n```bash\ntrue\n```\n",
        )
        .unwrap();
        assert_eq!(sc.needs_expr(), "tool:cargo,mem>=2G");
        // 与 --need 合并后的表达式仍能被同一套判据解析（不会出现"拼出来的表达式解析不了"）
        let merged = format!("{},{}", sc.needs_expr(), "cpu>=2");
        assert_eq!(need::parse_need(&merged).unwrap().len(), 3);
    }
}
