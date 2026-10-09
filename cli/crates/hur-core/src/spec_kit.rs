//! spec kit：写给 **Agent** 的施工说明（`HUR.md` / `SCAFFOLD.md`）。
//!
//! # 为什么要有这两份 markdown
//!
//! 用户提问（原话）：「是否可以有 `scaffold.md` 或者 `hur.md`，这个 md 用于定义一个 spec kit
//! 协助 agent 去生成 scaffold 或者 hur，当然一般情况下 scaffold 是不是只需要 md 就可以了，
//! 因为 vibe coding 代码速度很快」。
//!
//! 两个判断都成立，但它们的原因不同：
//!
//! * **scaffold 只需要 md**：一份脚手架的价值不在"我给你 37 个文件"，而在"**我知道这类工程
//!   该长什么样**"（结构、约定、验收命令、哪些坑不要踩）。让 Agent 读这份 md 去生成，
//!   生成的是**贴合当前仓库**的代码，而不是把一个三个月前的模板复制进去再改。
//!   所以 `SCAFFOLD.md` **就是**那个制品（`kind=scaffold`），不需要再塞一份代码进去。
//! * **`HUR.md` 是包自己在交代"我是怎么构成的"**：包一旦离开作者的机器（发布、被下载、
//!   被另一个 Agent 接手改），规范知识就不在现场了。`HUR.md` 把"这份包是什么、必填什么、
//!   改完怎么验、签名覆盖到哪一层"写在包旁边 —— 它是**从规范表生成**的，不是手抄的
//!   （手抄的说明一定会漂，而漂掉的说明比没有更坏）。
//!
//! # 这两份 md 与"规则"的关系
//!
//! `HUR.md` / `SCAFFOLD.md` **不发明规则**：它们是 [`RULES`]（`.hur` 的 R1~R13）与
//! [`SCAFFOLD_RULES`]（`.huf`/`SCAFFOLD.md` 的 S1~S8）的人读副本，而规则的执行者始终是代码里的
//! 校验器。副本会漂 —— 所以有两道钉子：`规则表不许漏` 与 `规则表不许编`（见本文件末尾的单测：
//! 从 `spec.rs` / `huf.rs` 里把用到的规则号抓出来与表对照）。

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::spec::{HurPackage, Issue, MANIFEST};

/// 包自己的施工说明（放在包根，**随包一起分发**）。
pub const HUR_MD: &str = "HUR.md";
/// 脚手架的制品本体（一份 md 就是一件制品）。
pub const SCAFFOLD_MD: &str = "SCAFFOLD.md";
/// 脚手架规范号。
pub const SCAFFOLD_SPEC: &str = "ncc-scaffold/v1";

/* ================= 规则表（人读副本 + 漂移钉子） ================= */

/// `.hur` 的规则（R1~R13）。`rule` 必须与 `spec.rs` 里实际用的编号一致。
pub struct Rule {
    pub rule: &'static str,
    pub what: &'static str,
}

pub const RULES: &[Rule] = &[
    Rule { rule: "R1", what: "规范与必填：`spec` 必须是 harness-use-package/v1；**可执行 profile 必须有 entry**（数据类反过来禁止）" },
    Rule { rule: "R2", what: "版本：`version` 是 `x.y.z`，不带前后空格" },
    Rule { rule: "R3", what: "能力面：`capabilities` / 端点 schema 与实际入口对得上" },
    Rule { rule: "R4", what: "依赖：`deps` 里的每一条都能解析（本地路径存在 / 远程在目录里）" },
    Rule { rule: "R5", what: "权限面：源码里出现的外呼域名必须在 `permissions.network` 里声明" },
    Rule { rule: "R6", what: "产物摘要：`.sha256` 侧车与手里这份字节一致（下载后先看这一眼）" },
    Rule { rule: "R7", what: "内容完整：`skills` / `tools` 指向的文件都在包里；system prompt 不超长" },
    Rule { rule: "R8", what: "安全策略声明（`security{}`）：引擎名、执行入口、network 档位、远程 + 签名搭配" },
    Rule { rule: "R9", what: "签名：签的是**规范打包字节**（覆盖 `hur.lock`），未受信公钥只算「有签名」不算「已验证」" },
    Rule { rule: "R10", what: "出口声明（`egress.provides[]`）：https 目标、非空 paths/methods、`inject` 只写头名" },
    Rule { rule: "R11", what: "状态声明（`state{}`）：kb / memory / checkpoints 的引用与 mode 合法" },
    Rule { rule: "R12", what: "profile 的额外要求：skill 禁 entry、plugin 要声明宿主、数据类禁跑代码与出网、state.stores[] 与节点一致" },
    Rule { rule: "R13", what: "施工说明（HUR.md）与清单一致：id / version / profile 变了就重新生成（`ncc hur spec-kit --write`）" },
];

/// `SCAFFOLD.md` 的规则（S1~S8）。
pub const SCAFFOLD_RULES: &[Rule] = &[
    Rule { rule: "S1", what: "`spec` 必须是 ncc-scaffold/v1（别拿别的规范号来冒充脚手架）" },
    Rule { rule: "S2", what: "`name` 是 ascii slug、`description` 一句话说清「生成什么」" },
    Rule { rule: "S3", what: "`stack` 非空：用什么语言 / 运行时 / 框架" },
    Rule { rule: "S4", what: "`outputs` 非空：要产出哪些文件（这是交付清单，不是建议）" },
    Rule { rule: "S5", what: "必须有「目标结构」段，且里面有一个目录树代码块" },
    Rule { rule: "S6", what: "必须有「验收」段，且里面有**可执行命令** —— 只描述不验证的脚手架不算完成" },
    Rule { rule: "S7", what: "不许出现密钥 / 令牌（AKIA、sk-、BEGIN PRIVATE KEY、token=… 一律拒）" },
    Rule { rule: "S8", what: "`requires` 里每条都是合法的算力需求表达式（能直接喂给 `ncc profile node fit`）" },
];

/* ================= HUR.md ================= */

/// 生成一份 `HUR.md`：**从规范表推导**，不复述人写的散文。
///
/// 内容分四块：这份包是什么 / 必填与禁令 / 命令面（改完怎么验）/ 规则表。
/// 有意**不写**具体业务说明（那是 README 的事）—— 这里只回答"怎么改这份包并且证明改对了"。
pub fn hur_md(pkg: &HurPackage) -> String {
    let prof = crate::profile::get(pkg.profile_name());
    let (prof_sum, executable, requires, forbids) = match prof {
        Some(p) => (p.summary, p.executable, p.requires, p.forbids),
        None => ("（清单里的 profile 不在规范里，`ncc hur profile` 会报错）", false, &[][..], &[][..]),
    };
    let dist = crate::spec::artifact_name(pkg);
    let mut s = String::new();
    s.push_str(&format!("# {HUR_MD} —— 这份包的施工说明（写给 Agent）\n\n"));
    s.push_str(&format!(
        "> 本文件由 `ncc hur spec-kit --write` 从规范生成（**别手改内容**：改了会与清单对不上，\n\
         > `ncc hur verify` 的 R13 会提醒你重新生成）。包的清单是 `{MANIFEST}`，它是唯一的身份来源。\n\n"));
    s.push_str("## 这份包是什么\n\n");
    s.push_str(&format!("| 项 | 值 |\n|---|---|\n"));
    s.push_str(&format!("| 规范 | `{}` |\n", pkg.spec));
    s.push_str(&format!("| id | `{}` |\n", pkg.id));
    s.push_str(&format!("| name / version | {} / `{}` |\n", pkg.name, pkg.version));
    s.push_str(&format!("| kind | `{}` |\n", pkg.kind));
    s.push_str(&format!(
        "| profile | `{}`（{}）|\n",
        pkg.profile_name(),
        if prof_sum.trim().is_empty() { "不许改 profile 而不改必填项" } else { prof_sum }
    ));
    s.push_str(&format!("| 能否执行 | {} |\n", if executable { "**能**（有入口，可进沙箱跑）" } else { "不能（数据 / 文档类）" }));
    if !pkg.entry.trim().is_empty() {
        s.push_str(&format!("| 入口 | `{}` |\n", pkg.entry));
    }
    s.push_str(&format!("| 产物名 | `{}` |\n", dist));
    s.push('\n');

    s.push_str("## 改这份包时要守住的事\n\n");
    if requires.is_empty() && forbids.is_empty() {
        s.push_str("- 这个 profile 没有额外的必填 / 禁令（规则 R1~R13 仍然全部适用）。\n");
    } else {
        for r in requires {
            s.push_str(&format!("- **必须**：{r}\n"));
        }
        for f in forbids {
            s.push_str(&format!("- **禁止**：{f}\n"));
        }
    }
    s.push_str("- 改完**必须**跑 `ncc hur verify`（离线，R1~R13）；它不过就别继续往下走。\n");
    s.push_str("- 改内容后要 `ncc hur build` 刷新 `hur.lock` —— **签名覆盖 lock**，锁没刷新等于签的是旧字节。\n");
    s.push_str("- 别手写 `hur.lock` 里的 sha256（它是内容真值，手写的值会在下次 build 被推翻）。\n");
    s.push('\n');

    s.push_str("## 命令面（从改完到发出去）\n\n");
    s.push_str("```bash\n");
    s.push_str("ncc hur verify .                     # 离线校验（R1~R13）\n");
    s.push_str("ncc hur build .                      # 刷新 hur.lock（内容逐个 sha256）\n");
    s.push_str("ncc hur pack .                       # → dist/…（确定性字节 + .sha256）\n");
    s.push_str("ncc hur sign .                       # 本机签名（私钥不出设备）\n");
    s.push_str("ncc publish --kind hur --file dist/… # 发布（联网）\n");
    if executable {
        s.push_str("ncc hur run . --exec                 # 在本机 wasm 沙箱里真跑（有入口）\n");
    }
    s.push_str("```\n\n");

    s.push_str("## 规则表（校验器执行的就是这些）\n\n");
    s.push_str("| 规则 | 判据 |\n|---|---|\n");
    for r in RULES {
        s.push_str(&format!("| {} | {} |\n", r.rule, r.what));
    }
    s.push('\n');
    s.push_str("> `.huf`（资源包）是另一条线，规则是 H1~H9：见 `ncc huf spec`。\n");
    s
}

/// `HUR.md` 与清单是否还对得上（规则 **R13**）。
///
/// 判据只看**能一眼认出的身份字段**（id / version / profile）—— 不从 md 里挑错别字，
/// 那样只会制造噪音。对不上就是"该重新生成"，说清怎么重新生成。
pub fn hur_md_check(text: &str, pkg: &HurPackage) -> Option<Issue> {
    let want = format!(
        "| id | `{}` |\n| name / version | {} / `{}` |",
        pkg.id, pkg.name, pkg.version
    );
    let prof_line = format!("| profile | `{}`", pkg.profile_name());
    if !text.contains(&want) || !text.contains(&prof_line) {
        return Some(Issue::warn(
            "R13",
            format!(
                "{HUR_MD} 与清单对不上（id / version / profile 至少有一个变了）：重新生成 —— `ncc hur spec-kit --write`"
            ),
        ));
    }
    Some(Issue::info("R13", format!("{HUR_MD} 与清单一致")))
}

/// 一份缺失 `HUR.md` 时的提示（**只提示，不判错**：老包没有它照样是合法包）。
pub fn hur_md_missing_hint() -> Issue {
    Issue::info(
        "R13",
        format!("没有 {HUR_MD}：`ncc hur spec-kit --write` 生成一份（写给接手改包的 Agent 的施工说明）"),
    )
}

/// 读/写 `HUR.md`（目录版）。
pub fn read_hur_md(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(HUR_MD)).ok()
}

pub fn write_hur_md(dir: &Path, pkg: &HurPackage) -> Result<std::path::PathBuf> {
    let p = dir.join(HUR_MD);
    std::fs::write(&p, hur_md(pkg)).with_context(|| format!("写 {} 失败", p.display()))?;
    Ok(p)
}

/* ================= SCAFFOLD.md ================= */

#[derive(Debug, Clone, Default)]
pub struct Scaffold {
    pub spec: String,
    pub name: String,
    pub description: String,
    pub stack: Vec<String>,
    /// 算力需求（`cpu>=2` / `tool:cargo` …）——与 `ncc profile node fit` 同一套语法
    pub requires: Vec<String>,
    pub outputs: Vec<String>,
    pub when: String,
    pub when_not: String,
    /// frontmatter 之后的正文（原样留着：它是给 Agent 的施工指令）
    pub body: String,
}

impl Scaffold {
    /// 折成算力画像的需求表达式（直接 `ncc profile node fit --need "…"`）。
    pub fn needs_expr(&self) -> String {
        self.requires.join(",")
    }
}

/// 解析 `SCAFFOLD.md`：frontmatter（YAML 子集）+ 正文。
///
/// 只支持三种写法（够用且好写好读）：`key: 值`、`key: [a, b]`、`key: a, b`。
/// 多行 / 嵌套一律不支持 —— 需要嵌套时该换成 `.huf` 包或 JSON，而不是把 markdown 当数据库。
pub fn parse_scaffold(text: &str) -> Result<Scaffold> {
    let mut lines = text.lines().peekable();
    let mut sc = Scaffold::default();
    // 允许 BOM 与前置空行
    let first = lines.next().unwrap_or("").trim_start_matches('\u{feff}').trim().to_string();
    if first != "---" {
        bail!("{SCAFFOLD_MD} 必须以 frontmatter 开头（第一行是 `---`）");
    }
    let mut closed = false;
    let mut body_start = 0usize;
    for (i, raw) in text.lines().enumerate().skip(1) {
        let l = raw.trim_end();
        if l.trim() == "---" {
            closed = true;
            body_start = i + 1;
            break;
        }
        let Some((k, v)) = l.split_once(':') else { continue };
        let key = k.trim().to_lowercase();
        let val = v.trim().to_string();
        match key.as_str() {
            "spec" => sc.spec = val,
            "name" => sc.name = val,
            "description" | "summary" | "short" => sc.description = val,
            "stack" => sc.stack = list_of(&val),
            "requires" | "require" => sc.requires = list_of(&val),
            "outputs" | "output" | "files" => sc.outputs = list_of(&val),
            "when" | "use-when" => sc.when = val,
            "when-not" | "whennot" | "not-when" => sc.when_not = val,
            _ => {}
        }
    }
    if !closed {
        bail!("{SCAFFOLD_MD} 的 frontmatter 没闭合（少了第二个 `---`）");
    }
    sc.body = text.lines().skip(body_start).collect::<Vec<_>>().join("\n");
    Ok(sc)
}

/// `[a, b]` / `a, b` / `a` → 列表（去掉方括号与引号，空项丢弃）。
fn list_of(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 生成一份 `SCAFFOLD.md` 骨架（`ncc scaffold init`）——「验收」留成 TODO。
///
/// 想在骨架里就写进**真能跑**的命令，用 [`scaffold_template_with`]。
pub fn scaffold_template(name: &str, description: &str, stack: &[String], outputs: &[String], requires: &[String]) -> String {
    scaffold_template_with(name, description, stack, outputs, requires, &["echo \"TODO: 换成真实验收命令\"".to_string()])
}

/// 同上，但「验收」段里的命令由调用方给（`ncc scaffold init` 按认出来的栈填）。
pub fn scaffold_template_with(
    name: &str,
    description: &str,
    stack: &[String],
    outputs: &[String],
    requires: &[String],
    acceptance: &[String],
) -> String {
    // 空列表写成 `[]`（写成空值会留一行 `requires: ` —— 半截 frontmatter 既难读也难判）
    let list = |v: &[String]| format!("[{}]", v.join(", "));
    let acc = acceptance.join("\n");
    format!(
        "---\n\
         spec: {SCAFFOLD_SPEC}\n\
         name: {name}\n\
         description: {description}\n\
         stack: {stack}\n\
         requires: {requires}\n\
         outputs: {outputs}\n\
         when: 需要一个新的这类工程\n\
         whenNot: 已经有工程、只想加一个功能（那就直接改，别重新生成）\n\
         ---\n\n\
         # {name}\n\n\
         ## 目标结构\n\n\
         ```text\n\
         {name}/\n\
         ├── README.md\n\
         └── src/            # 具体文件由生成者按 stack 决定\n\
         ```\n\n\
         ## 步骤（给 Agent 的指令，不是代码）\n\n\
         1. 先看当前目录：**非空就不要覆盖**，改用增量方式新增文件。\n\
         2. 按 `outputs` 逐个生成；每个文件先说清它为什么存在。\n\
         3. 生成完立刻跑「验收」里的命令，把真实输出贴回来。\n\n\
         ## 验收\n\n\
         ```bash\n\
         # 至少一条能真跑的命令；失败就是没完成\n\
         {acc}\n\
         ```\n\n\
         ## 边界\n\n\
         - 不引入未在 `stack` 里声明的框架。\n\
         - 不写入任何密钥 / 令牌（需要凭据时只写名字，值由使用者在本机提供）。\n",
        stack = list(stack),
        requires = list(requires),
        outputs = list(outputs),
    )
}

/// 校验 `SCAFFOLD.md`（S1~S8，全程离线）。
pub fn verify_scaffold(text: &str) -> Vec<Issue> {
    let mut out = Vec::new();
    let sc = match parse_scaffold(text) {
        Ok(sc) => sc,
        Err(e) => {
            out.push(Issue::err("S1", format!("{e:#}")));
            return out;
        }
    };

    // S1 规范号
    if sc.spec != SCAFFOLD_SPEC {
        out.push(Issue::err("S1", format!("spec 必须是「{SCAFFOLD_SPEC}」，当前是「{}」", sc.spec)));
    }
    // S2 name / description
    if sc.name.trim().is_empty() {
        out.push(Issue::err("S2", "name 不能为空"));
    } else if !sc.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        out.push(Issue::err("S2", format!("name「{}」要是 ascii slug（小写字母 / 数字 / - _）", sc.name)));
    }
    if sc.description.trim().is_empty() {
        out.push(Issue::err("S2", "description 不能为空（一句话说清这份脚手架生成什么）"));
    }
    // S3 stack
    if sc.stack.is_empty() {
        out.push(Issue::err("S3", "stack 不能为空（用什么语言 / 运行时 / 框架）"));
    }
    // S4 outputs
    if sc.outputs.is_empty() {
        out.push(Issue::err("S4", "outputs 不能为空 —— 它是交付清单，写清要产出哪些文件"));
    }
    // S5 目标结构
    if !has_section(&sc.body, &["目标结构", "结构", "structure", "layout"]) {
        out.push(Issue::err("S5", "缺「目标结构」段：没有结构就没有交付物"));
    } else if fenced(&sc.body, &["text", "plaintext", ""]).is_empty() {
        out.push(Issue::err("S5", "「目标结构」段里要有一个目录树代码块（```text）"));
    }
    // S6 验收（必须有可执行命令）
    let acc = section(&sc.body, ACCEPTANCE_KEYS);
    match acc {
        None => out.push(Issue::err("S6", "缺「验收」段 —— 只描述不验证的脚手架不算完成")),
        Some(body) => {
            let cmds = acceptance_commands(&body);
            if cmds.is_empty() {
                out.push(Issue::err("S6", "「验收」段里没有可执行命令（代码块里只有注释或没有代码块）"));
            } else if cmds.iter().all(|c| c.contains("TODO")) {
                out.push(Issue::err("S6", "「验收」段还是占位（TODO）—— 换成本工程真实能跑的命令"));
            } else {
                out.push(Issue::info("S6", format!("验收命令 {} 条（`ncc scaffold verify --run` 可真跑）", cmds.len())));
            }
        }
    }
    // S7 秘密
    if let Some(hit) = secret_hit(text) {
        out.push(Issue::err("S7", format!("正文里出现了像密钥 / 令牌的内容（`{hit}`）—— 只写名字，值由使用者在本机提供")));
    }
    // S8 requires 语法（能直接喂给算力画像）
    for r in &sc.requires {
        if crate::need::parse_one(r).is_ok() {
            continue;
        }
        out.push(Issue::err(
            "S8",
            format!("requires 里的「{r}」不是合法的算力需求（例：`cpu>=2` / `mem>=2G` / `tool:cargo`）"),
        ));
    }
    // 提醒：写清"目录非空怎么办"（S8 的软性部分）—— 只提醒，不拦
    let low = sc.body.to_lowercase();
    if !(low.contains("非空") || low.contains("existing") || low.contains("已有") || low.contains("增量")) {
        out.push(Issue::warn(
            "S8",
            "没写「目录非空时怎么办」：Agent 容易直接覆盖用户已有代码 —— 补一句增量策略",
        ));
    }
    out
}

/// 「验收」段里**真正会跑的命令**（`ncc scaffold verify --run` 跑的就是这几条）。
///
/// 判据只有一份：S6 用它判断"有没有可执行命令"，`--run` 用它决定跑什么 ——
/// 两边各写一份，就会出现"检查说有 3 条、实际一条都不跑"这种自相矛盾。
pub fn acceptance_commands(acc_section: &str) -> Vec<String> {
    fenced(acc_section, &["bash", "sh", "shell", "console", ""])
        .iter()
        .flat_map(|b| b.lines())
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

/* ================= 栈与默认值（init 与 fit 共用） ================= */

/// 从目录里的工程文件**认**出技术栈（`Cargo.toml` → rust…）。认不出来给空 ——
/// 空就让调用方去问人，不猜。
pub fn detect_stack(dir: &Path) -> Vec<String> {
    let has = |n: &str| dir.join(n).exists();
    if has("Cargo.toml") {
        return vec!["rust".into(), "cargo".into()];
    }
    if has("tsconfig.json") {
        return vec!["node".into(), "typescript".into()];
    }
    if has("package.json") {
        return vec!["node".into(), "npm".into()];
    }
    if has("go.mod") {
        return vec!["go".into()];
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        return vec!["python".into()];
    }
    if has("pom.xml") {
        return vec!["java".into(), "maven".into()];
    }
    if has("Gemfile") {
        return vec!["ruby".into()];
    }
    Vec::new()
}

/// 栈 → 交付清单（`outputs`）默认值：**先给一份能对标结构的清单**，再让人改。
pub fn default_outputs(name: &str, stack: &[String]) -> Vec<String> {
    let slug = name.replace('-', "_");
    let has = |k: &str| stack.iter().any(|s| s.eq_ignore_ascii_case(k));
    if has("rust") {
        return vec!["Cargo.toml".into(), format!("src/{slug}.rs"), "README.md".into()];
    }
    if has("typescript") {
        return vec!["package.json".into(), "tsconfig.json".into(), "src/index.ts".into(), "README.md".into()];
    }
    if has("node") {
        return vec!["package.json".into(), "src/index.js".into(), "README.md".into()];
    }
    if has("go") {
        return vec!["go.mod".into(), "main.go".into(), "README.md".into()];
    }
    if has("python") {
        return vec!["pyproject.toml".into(), format!("src/{slug}/__init__.py"), "README.md".into()];
    }
    if has("java") {
        return vec!["pom.xml".into(), "src/main/java/App.java".into(), "README.md".into()];
    }
    if has("ruby") {
        return vec!["Gemfile".into(), format!("lib/{slug}.rb"), "README.md".into()];
    }
    vec!["README.md".into()]
}

/// 栈 → 算力需求（`requires`）默认值：每个栈认一个"必须在场"的工具。
///
/// 认不出来的栈**不给需求**（空列表是合法的：不知道就不编）——
/// 编一条假的，`ncc scaffold fit` 就会拿它去卡机器。
pub fn default_requires(stack: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in stack {
        let tool = match s.to_lowercase().as_str() {
            "rust" | "cargo" => "cargo",
            "node" | "npm" | "typescript" => "node",
            "python" | "python3" => "python3",
            "go" => "go",
            "java" => "java",
            "maven" => "mvn",
            "ruby" => "ruby",
            _ => continue,
        };
        let need = format!("tool:{tool}");
        if !out.contains(&need) {
            out.push(need);
        }
    }
    out
}

/// 栈 → 「验收」里默认的那条命令。认不出栈就留 TODO —— **S6 会报错**，
/// 提醒作者换成真能证明"生成对了"的命令（模板不装作已完成）。
pub fn acceptance_for(stack: &[String]) -> Vec<String> {
    let has = |k: &str| stack.iter().any(|s| s.eq_ignore_ascii_case(k));
    if has("rust") {
        return vec!["cargo build".into()];
    }
    if has("typescript") {
        return vec!["npx tsc --noEmit".into()];
    }
    if has("node") {
        return vec!["npm install --no-audit --no-fund".into()];
    }
    if has("go") {
        return vec!["go build ./...".into()];
    }
    if has("python") {
        return vec!["python3 -m compileall -q src".into()];
    }
    if has("java") {
        return vec!["mvn -q -DskipTests package".into()];
    }
    if has("ruby") {
        return vec!["ruby -c lib/*.rb".into()];
    }
    vec!["echo \"TODO: 换成真实验收命令\"".into()]
}

/// 「验收」段的标题关键词（S6 与 `ncc scaffold verify --run` **共用一份**：
/// 两边各写一份就会出现"检查看到的段"与"真跑的段"不是一个）。
pub const ACCEPTANCE_KEYS: &[&str] = &["验收", "验证", "acceptance", "verify"];

/// 段落正文：从含某个关键词的标题到下一个 `## `。
///
/// ⚠️ 扫标题时必须**跳过围栏代码块**：`# 注释` 是 bash 块里最普通的一行，
/// 而它长得就像 markdown 标题 —— 不跳过去，段落会在这里被截断，
/// 于是"有命令"被读成"没有命令"（S6 会红，`--run` 会什么都不跑）。这是实测踩到的。
pub fn section(body: &str, keys: &[&str]) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let mut start = None;
    let mut fence = false;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence || !t.starts_with('#') {
            continue;
        }
        let title = t.trim_start_matches('#').trim().to_lowercase();
        if keys.iter().any(|k| title.contains(&k.to_lowercase())) {
            start = Some(i + 1);
            break;
        }
    }
    let s = start?;
    let mut end = lines.len();
    let mut fence = false;
    for (i, l) in lines.iter().enumerate().skip(s) {
        let t = l.trim_start();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if !fence && t.starts_with('#') {
            end = i;
            break;
        }
    }
    Some(lines[s..end].join("\n"))
}

fn has_section(body: &str, keys: &[&str]) -> bool {
    section(body, keys).is_some()
}

/// 取出正文里指定语言的代码块（`langs` 里给空串表示"任意语言"）。
fn fenced(body: &str, langs: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    for l in body.lines() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("```") {
            match cur.take() {
                None => cur = Some((rest.trim().to_lowercase(), Vec::new())),
                Some((lang, lines)) => {
                    let want = langs.iter().any(|w| w.is_empty() || *w == lang);
                    if want {
                        out.push(lines.join("\n"));
                    }
                }
            }
            continue;
        }
        if let Some((_, lines)) = cur.as_mut() {
            lines.push(l.to_string());
        }
    }
    out
}

/// 扫明显像密钥的东西（**宁可误报也不漏报**：误报的代价是改一行文字，漏报的代价是泄密）。
fn secret_hit(text: &str) -> Option<String> {
    for pat in ["AKIA", "sk-", "-----BEGIN", "ghp_", "xoxb-"] {
        if let Some(i) = text.find(pat) {
            let tail: String = text[i..].chars().take(24).collect();
            return Some(tail.replace('\n', " "));
        }
    }
    // `token = "abc…"` / `password: xxx` 这类赋值
    for (i, _) in text.match_indices("token") {
        let tail: String = text[i..].chars().take(48).collect();
        let low = tail.to_lowercase();
        if (low.contains('=') || low.contains(':')) && tail.split(['=', ':']).nth(1).map(|v| v.trim().len() > 8).unwrap_or(false) {
            return Some(tail.replace('\n', " "));
        }
    }
    None
}

/* ================= 单测 ================= */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Level;
    use std::collections::BTreeSet;

    fn sample_pkg() -> HurPackage {
        serde_json::from_str(
            r#"{"spec":"harness-use-package/v1","kind":"agent","id":"A-demo","name":"Demo","version":"0.1.0",
                "entry":"src/agent.ts","short":"x"}"#,
        )
        .unwrap()
    }

    /// `.hur` 这条线的源文件（规则表要与它们对齐）。
    const HUR_SOURCES: [&str; 3] = [include_str!("spec.rs"), include_str!("pack.rs"), include_str!("spec_kit.rs")];

    fn rules_used(src: &str, prefix: char) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for (i, _) in src.match_indices('"') {
            let tail: String = src[i + 1..].chars().take(3).collect();
            let id: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if id.len() >= 2
                && id.starts_with(prefix)
                && id[1..].chars().all(|c| c.is_ascii_digit())
            {
                ids.insert(id);
            }
        }
        ids
    }

    /// 规则表**不许漏**：校验器用到的规则号，必须都在表里有解释
    /// （说明是给人看的，漏了就等于规则不可发现）。
    #[test]
    fn 规则表不许漏() {
        let mut ids: BTreeSet<String> = BTreeSet::new();
        for src in HUR_SOURCES {
            ids.extend(rules_used(src, 'R'));
        }
        let have: BTreeSet<String> = RULES.iter().map(|r| r.rule.to_string()).collect();
        let missing: Vec<String> = ids.difference(&have).cloned().collect();
        assert!(missing.is_empty(), "规则表漏了：{missing:?}（RULES 里补一条）");

        let mut sids: BTreeSet<String> = BTreeSet::new();
        for src in [include_str!("spec_kit.rs"), include_str!("../src/spec_kit.rs")] {
            sids.extend(rules_used(src, 'S'));
        }
        let shave: BTreeSet<String> = SCAFFOLD_RULES.iter().map(|r| r.rule.to_string()).collect();
        let smissing: Vec<String> = sids.difference(&shave).cloned().collect();
        assert!(smissing.is_empty(), "脚手架规则表漏了：{smissing:?}");
    }

    /// 规则表**不许编**：表里写的号必须是校验器真的会用的号（编出来的说明比没有更坏）。
    #[test]
    fn 规则表不许编() {
        let used: BTreeSet<String> = HUR_SOURCES.iter().flat_map(|s| rules_used(s, 'R')).collect();
        for r in RULES {
            assert!(used.contains(r.rule), "RULES 里的 {} 没有任何校验器在用", r.rule);
        }
        let s_used: BTreeSet<String> = rules_used(include_str!("spec_kit.rs"), 'S');
        for r in SCAFFOLD_RULES {
            assert!(s_used.contains(r.rule), "SCAFFOLD_RULES 里的 {} 没被校验器使用", r.rule);
        }
    }

    #[test]
    fn hur_md_生成且能自查一致() {
        let pkg = sample_pkg();
        let md = hur_md(&pkg);
        assert!(md.starts_with("# HUR.md"));
        assert!(md.contains("harness-use-package/v1"), "要写清规范号");
        assert!(md.contains("| id | `A-demo` |"));
        assert!(md.contains("| profile | `agent`"));
        assert!(md.contains("ncc hur verify"), "命令面要在里面");
        assert!(md.contains("R12"), "规则表要在里面");
        assert!(md.contains("system_prompt"), "agent profile 的必填项要写出来：{md}");
        // 与清单一致 → info；改一个版本 → warn
        assert_eq!(hur_md_check(&md, &pkg).unwrap().level, Level::Info);
        let mut other = sample_pkg();
        other.version = "0.2.0".into();
        let i = hur_md_check(&md, &other).unwrap();
        assert_eq!(i.level, Level::Warn, "{i:?}");
        assert!(i.msg.contains("spec-kit"), "要说清怎么重新生成：{}", i.msg);
    }

    #[test]
    fn scaffold_解析与校验() {
        let text = scaffold_template(
            "fastapi-service",
            "Python FastAPI 服务骨架",
            &["python3>=3.11".into(), "fastapi".into()],
            &["pyproject.toml".into(), "app/main.py".into()],
            &["tool:python3".into(), "mem>=1G".into()],
        );
        let sc = parse_scaffold(&text).unwrap();
        assert_eq!(sc.spec, SCAFFOLD_SPEC);
        assert_eq!(sc.name, "fastapi-service");
        assert_eq!(sc.stack.len(), 2);
        assert_eq!(sc.outputs.len(), 2);
        assert_eq!(sc.needs_expr(), "tool:python3,mem>=1G");
        let issues = verify_scaffold(&text);
        // 模板里的验收还是 TODO → S6 必须报错（模板本身不装作"已完成"）
        assert!(issues.iter().any(|i| i.rule == "S6" && i.level == Level::Error), "{issues:?}");
        // 除 S6 与"非空策略"提醒外，不该有别的错
        let errs: Vec<&Issue> = issues.iter().filter(|i| i.level == Level::Error).collect();
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    #[test]
    fn scaffold_逐条规则() {
        let good = "---\nspec: ncc-scaffold/v1\nname: demo\n\
                    description: 生成一个演示工程\nstack: [rust]\nrequires: [tool:cargo]\n\
                    outputs: [Cargo.toml, src/main.rs]\n---\n\n\
                    # demo\n\n## 目标结构\n\n```text\ndemo/\n└── src/main.rs\n```\n\n\
                    ## 步骤\n\n1. 目录非空就增量新增。\n\n\
                    ## 验收\n\n```bash\ncargo build\n```\n";
        let issues = verify_scaffold(good);
        assert!(
            issues.iter().all(|i| i.level != Level::Error),
            "这份该全过：{issues:?}"
        );

        let bad_spec = good.replace("ncc-scaffold/v1", "whatever/v9");
        assert!(verify_scaffold(&bad_spec).iter().any(|i| i.rule == "S1"));
        let no_acc = good.replace("## 验收", "## 说明");
        assert!(verify_scaffold(&no_acc).iter().any(|i| i.rule == "S6" && i.level == Level::Error));
        let no_struct = good.replace("## 目标结构", "## 目录");
        assert!(verify_scaffold(&no_struct).iter().any(|i| i.rule == "S5"));
        let secret = good.replace("cargo build", "export TOKEN=\"sk-abcdefghijklmnop\"");
        assert!(verify_scaffold(&secret).iter().any(|i| i.rule == "S7"));
        let bad_need = good.replace("tool:cargo", "cargo 装好即可");
        assert!(verify_scaffold(&bad_need).iter().any(|i| i.rule == "S8"));
        // frontmatter 没闭合
        assert!(verify_scaffold("# 只有正文").iter().any(|i| i.rule == "S1"));
    }
}
