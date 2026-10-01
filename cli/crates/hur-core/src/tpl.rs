//! `hur init` 的模板：按 kind 生成一个合规工程目录。
//!
//! 生成物与创作中心（GUI）产出的文件集**同构**：同一份 `hur.json` 规范，
//! GUI 打开 CLI 生成的目录、CLI 打开 GUI 导出的目录，都必须成立。

use crate::spec::{AgentSpec, Deps, HurPackage, Permissions, PublishInfo, PKG_SPEC};

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    // 中文等非 ASCII 名称会被清空 → 兑底为可读的 ASCII 片段（id 必须 ASCII）
    if out.is_empty() {
        return format!("pkg{}", rand_hex(4));
    }
    out
}

pub fn rand_hex(n: usize) -> String {
    // 不引 rand：用系统时间 + 进程 id 混合，够用（id 只要求唯一可读）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut x = now ^ ((std::process::id() as u128) << 32);
    let chars = b"abcdef0123456789";
    let mut s = String::new();
    for _ in 0..n {
        let i = (x % 16) as usize;
        s.push(chars[i] as char);
        x /= 16;
        x ^= x >> 7;
    }
    s
}

pub fn acronym(name: &str) -> String {
    let mut out = String::new();
    for w in name.split(|c: char| c.is_whitespace() || matches!(c, '-' | '_')).filter(|w| !w.is_empty()) {
        if let Some(c) = w.chars().next() {
            out.push(c.to_ascii_uppercase());
        }
    }
    let ascii: String = out.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if ascii.is_empty() {
        "PKG".to_string()
    } else {
        ascii.to_uppercase().chars().take(4).collect()
    }
}

pub struct InitInput {
    pub kind: String,
    /// 这份包**是什么**（`crate::profile::PROFILES` 里的名字）。`None` = 按 kind 推导。
    pub profile: Option<String>,
    pub name: String,
    /// kind=agent：角色/职责（写进 agent.system_prompt）
    pub role: String,
    pub domain: String,
    pub short: String,
    pub version: String,
    pub summary: String,
    pub registry: String,
    pub namespace: String,
}

pub fn build_package(input: &InitInput) -> HurPackage {
    let slug_name = slug(&input.name);
    // 先算出**生效的 profile**（写了就用它，没写按 kind 推导）—— 生成什么文件、要不要 entry
    // 都由它决定，不然 `hur init --profile skill` 会生出一份自己校验不过的包。
    let profile_name = crate::profile::of(input.profile.as_deref(), &input.kind);
    let def = crate::profile::get(profile_name);
    let domain = if input.domain.trim().is_empty() { "generic".to_string() } else { slug(&input.domain) };
    let id = match input.kind.as_str() {
        "repo" => slug_name.clone(),
        "harness" => format!("H-{domain}-{slug_name}-{}", rand_hex(6)),
        _ => format!("A-{domain}-{slug_name}-{}", rand_hex(6)),
    };
    let short = if input.short.trim().is_empty() {
        if slug_name == "harness-use" || slug_name == "hur" {
            "HUR".to_string()
        } else {
            acronym(&input.name)
        }
    } else {
        input.short.trim().to_ascii_uppercase()
    };
    HurPackage {
        egress: None,
        state: None,
        // 授权包（profile=auth）不该由通用模板生出来：它要先定密钥与项目，
        // 得走 `ncc auth pkg init`（那里会把 kdf / vault 一起建好）。
        auth: None,
        spec: PKG_SPEC.to_string(),
        kind: input.kind.clone(),
        // 写了就写进清单；没写就不写（老包不带这个字段，照样能被校验）
        profile: input.profile.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        id,
        data: None,
        name: input.name.trim().to_string(),
        version: if input.version.trim().is_empty() { "0.1.0".to_string() } else { input.version.trim().to_string() },
        short,
        domain: domain.clone(),
        summary: input.summary.trim().to_string(),
        // 入口名由 **profile** 决定（`profile.rs::default_entry`）：skill 与数据快照没有代码入口，
        // plugin / mcp / app 各有一个名副其实的入口。
        // 以前这里一律写 `src/agent.ts`，于是"一份技能文档包"里躺着一份 Agent 程序
        // （而 R12 反过来禁止 skill 带 entry）—— 生成物与自己矛盾。
        entry: def.and_then(|p| p.default_entry()).unwrap_or("").to_string(),
        runtime: "local-v0".to_string(),
        capabilities: if profile_name == "agent" {
            vec!["discover".into(), "describe".into(), "rank".into(), "reply".into()]
        } else {
            vec![]
        },
        deps: Deps::default(),
        permissions: Permissions::default(),
        publish: PublishInfo {
            registry: input.registry.trim().to_string(),
            namespace: input.namespace.trim().to_string(),
            visibility: "public".to_string(),
            slug: String::new(),
        },
        // kind=agent：同时给出「声明式 Agent 定义」（PRD §9.2）——
        // 桌面端只读声明即可完成装配，不必执行包内代码。
        //
        // plugin / mcp 也要写这一块，但只写 `adapters`：**声明"我打算接进哪些宿主"**。
        // 不写的话 plugin 包一生成就过不了 R12（"要声明宿主，否则 interop 渲染不出来"），
        // 而 mcp 包的宿主只有一个，写全了比让作者去猜好。
        agent: match profile_name {
            "agent" => Some(AgentSpec {
                system_prompt: {
                    let role = input.role.trim();
                    if role.is_empty() {
                        format!(
                            "你是「{}」，负责 {domain} 域的事务。先给结论，再给可执行下一步；本地决策、数据不出设备。",
                            pkg_name(&input.name)
                        )
                    } else {
                        format!("{role} 先给结论，再给可执行下一步；本地决策、数据不出设备。")
                    }
                },
                persona: String::new(),
                tools: vec!["repo_search".into(), "harness.call".into(), "kb.search".into(), "task.create".into()],
                skills: vec![format!("skills/{}.md", slug_name)],
                pipeline: None,
                guard: Some(serde_json::json!({ "max_reply_chars": 160 })),
                // 默认声明「打算支持哪些宿主」（`hur export` 不依赖它，但写全了更早发现笔误）
                adapters: crate::interop::TARGETS.iter().map(|s| s.to_string()).collect(),
            }),
            "plugin" => Some(AgentSpec {
                adapters: crate::interop::TARGETS.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            "mcp" => Some(AgentSpec {
                adapters: vec!["mcp".to_string()],
                ..Default::default()
            }),
            // skill 是文档：宿主直接读 `skills/`，不需要 agent{} 声明（R12 也不要求）
            _ => None,
        },
        // 新工程默认走推荐档：本机 WASM 沙箱（能不能真跑由宿主策略与执行引擎决定）。
        // **只有可执行的包**才写这一段：给一份技能文档或数据快照声明"执行策略"是空话。
        security: if def.map(|p| p.executable).unwrap_or(false) {
            Some(crate::policy::SecurityReq {
                policy: "wasm-local".into(),
                // 先只声明"打算用哪个引擎"；真正开执行要等作者提供入口产物再把 enabled 打开
                exec: crate::policy::ExecRule { enabled: Some(false), engines: Some(vec!["wasm".into()]), ..Default::default() },
                entry: String::new(),
                sandbox: crate::policy::SandboxRule { network: Some("declared-only".into()), ..Default::default() },
                ..Default::default()
            })
        } else {
            None
        },
    }
}

fn pkg_name(name: &str) -> String {
    let t = name.trim();
    if t.is_empty() { "未命名 Agent".to_string() } else { t.to_string() }
}

/// 生成包内文件（相对路径 → 内容），**不含 hur.json / hur.lock**（由 init/pack 负责）。
///
/// ⚠️ 分支看的是 **profile**，不是 kind：`--kind skill` 与 `--kind agent --profile skill`
/// 是同一件事，文件集必须一致 —— 决定"这类包该长什么样"的从来是 profile。
pub fn files_for(pkg: &HurPackage) -> Vec<(String, String)> {
    let slug_name = slug(&pkg.name);
    let prof = pkg.profile_name();
    let mut out: Vec<(String, String)> = Vec::new();

    match prof {
        "harness" => {
            out.push((
                "src/agent.ts".to_string(),
                format!(
                    r#"// {name} · Harness 能力包入口（由 `ncc hur init --kind harness` 生成）
// 端点定义见 src/{slug}.schema.json。
//
// ⚠️ 调哪个域，就要在 hur.json 的 permissions.network 里声明哪个域 ——
// R5 会逐字扫描本目录里的源码与文档，超范围调用**直接红**。
// 所以这里先留空：填真的地址时，把它一起写进 permissions.network。

const ENDPOINTS = [
  {{ name: 'search', method: 'GET', path: '/search', description: '' }},
]

// 换成你的 API 根地址（形如 https://你的域名）；同时把域名写进 permissions.network
const BASE = ''

export async function call<T = unknown>(path: string, input: Record<string, unknown> = {{}}): Promise<T> {{
  const ep = ENDPOINTS.find((e) => e.path === path)
  if (!ep) throw new Error('unknown endpoint ' + path)
  if (!BASE) throw new Error('先把 BASE 填成你的 API 根地址，并把域名写进 permissions.network')
  const res = await fetch(BASE + ep.path, {{
    method: ep.method,
    body: ep.method === 'GET' ? undefined : JSON.stringify(input),
    headers: {{ 'Content-Type': 'application/json' }},
  }})
  if (!res.ok) throw new Error('call ' + ep.path + ' -> HTTP ' + res.status)
  return (await res.json()) as T
}}

export const endpoints = ENDPOINTS.map((e) => e.path)
export default {{ call, endpoints }}
"#,
                    name = pkg.name,
                    slug = slug_name
                ),
            ));
            out.push((
                format!("src/{slug_name}.schema.json"),
                format!(
                    "{}\n",
                    serde_json::json!({
                        "api_version": "v1",
                        "endpoints": [
                            {"name": "search", "method": "GET", "path": "/search", "description": ""}
                        ]
                    })
                ),
            ));
            out.push((
                format!("skills/{slug_name}.md"),
                format!(
                    "# {} · 调用说明\n\n- 何时用：……\n- 怎么调：`endpoints` 里挑路径，参数按 schema 传\n- 注意：密钥只放本机，不写进包\n",
                    pkg.name
                ),
            ));
        }
        // 脚手架：给别人当起点的模板。模板文件放 `assets/template/` ——
        // 只有 src / skills / kb / data / assets 这五个目录算"包内容"（`spec::CONTENT_DIRS`），
        // 放别处等于没进包（下载的人会拿到一个空目录）。
        "scaffold" => {
            out.push((
                "src/index.json".to_string(),
                format!(
                    "{}\n",
                    serde_json::json!({
                        "repo": slug_name,
                        "short": pkg.short,
                        "domain": pkg.domain,
                        "templates": ["assets/template"],
                        "packages": []
                    })
                ),
            ));
            out.push((
                "assets/template/README.md".to_string(),
                fill(SCAFFOLD_TEMPLATE_README, pkg),
            ));
            out.push((
                "assets/template/src/index.ts".to_string(),
                fill(SCAFFOLD_TEMPLATE_INDEX, pkg),
            ));
        }
        // 技能：一份 SKILL.md，**平铺**在 `skills/` 下（`skills/<名>.md`）。
        // 为什么不平铺不行：interop 拿文件名当宿主里的技能短名，`skills/x/SKILL.md`
        // 会被读成短名 "SKILL"（宿主里看着就是重名）。
        "skill" => {
            out.push((format!("skills/{slug_name}.md"), fill(SKILL_MD, pkg)));
        }
        // MCP server：给一份**能跑起来**的最小 stdio 骨架（不是伪代码）——
        // 协议部分（initialize / tools/list / tools/call）是照抄就能用的，
        // 作者只需要动 TOOLS 与 callTool。
        "mcp" => {
            out.push(("src/server.ts".to_string(), fill(MCP_SERVER_TS, pkg)));
        }
        // 宿主插件：接进哪些宿主是**声明**（hur.json 的 agent.adapters），
        // 这个文件只放"插进宿主之后要做什么"。
        "plugin" => {
            out.push(("src/plugin.ts".to_string(), fill(PLUGIN_TS, pkg)));
        }
        // 可自部署应用（NCC 舱）：入口 + 该声明什么（state{} / egress{}）写在注释里。
        "app" => {
            out.push(("src/app.ts".to_string(), fill(APP_TS, pkg)));
        }
        _ => {
            out.push((
                "src/agent.ts".to_string(),
                format!(
                    r#"// {name} · 自定义 Agent 入口（由 `hur init --kind agent` 生成）
// 本文件是包的「大脑」：角色 / 能力 / 依赖都在 hur.json，这里放提示词与编排。

export const systemPrompt = `你是「{name}」，负责 {domain} 域的事务。
本地意图解析 → 脱敏发现 → 信誉排序 → 权衡表达；数据不出设备。`

export const capabilities = {caps}

/** 编排入口：CLI 的 `hur run` 与桌面 Agent 都从这里进 */
export async function handle(input: {{ text: string }}): Promise<{{ reply: string; notes?: string[] }}> {{
  return {{ reply: `已理解：${{input.text}}`, notes: ['骨架实现，替换为真实编排'] }}
}}

export default {{ systemPrompt, capabilities, handle }}
"#,
                    name = pkg.name,
                    domain = pkg.domain,
                    caps = serde_json::to_string(&pkg.capabilities).unwrap_or_else(|_| "[]".into())
                ),
            ));
            out.push((
                format!("skills/{slug_name}.md"),
                format!("# {} · 技能说明\n\n- 何时用：……\n- 怎么做：……\n", pkg.name),
            ));
        }
    }

    out.push(("README.md".to_string(), readme_for(pkg, prof)));

    out.push((
        ".gitignore".to_string(),
        "dist/\nnode_modules/\ntarget/\n.DS_Store\n".to_string(),
    ));

    out
}

/// 把模板里的占位符换掉。
///
/// 为什么不用 `format!`：骨架里有成堆的 `{` `}`（TS 的对象字面量），每写一次就要
/// `{{` 转义一次，转错一次就是一个编不过或**生成出坏代码**的模板 —— 这类错误在
/// 生成器里最难发现（要真的 init 出来才看得见）。
fn fill(tpl: &str, pkg: &HurPackage) -> String {
    tpl.replace("__NAME__", &pkg.name)
        .replace("__SLUG__", &slug(&pkg.name))
        .replace("__ID__", &pkg.id)
        .replace("__VERSION__", &pkg.version)
        .replace("__SHORT__", &pkg.short)
        .replace("__PROFILE__", pkg.profile_name())
        .replace("__KIND__", &pkg.kind)
        .replace("__SPEC__", &pkg.spec)
        .replace(
            "__ENTRY__",
            if pkg.entry.trim().is_empty() { "（无入口）" } else { pkg.entry.as_str() },
        )
        .replace("__HOSTS__", &hosts_line(pkg))
}

/// 这份包声明接进哪些宿主（没声明就如实说"还没声明"）。
fn hosts_line(pkg: &HurPackage) -> String {
    match pkg.agent.as_ref().map(|a| a.adapters.clone()) {
        Some(a) if !a.is_empty() => a.join(" / "),
        _ => "（还没声明：写在 hur.json 的 agent.adapters）".to_string(),
    }
}

/// 包内 README：**每个 profile 的"这是什么 / 下一步"都不一样**。
///
/// 一份放之四海的 README 等于没写 —— 技能包要的是"怎么渲染到宿主"，MCP 包要的是
/// "怎么先把 server 跑起来"，app 包要的是"先声明要什么数据、出什么网"。
/// 作者拿到工程的第一分钟看的就是这份文件，别让他自己去猜 profile 的规矩。
fn readme_for(pkg: &HurPackage, prof: &str) -> String {
    let (what, next) = readme_notes(prof);
    fill(&README_TPL.replace("__WHAT__", what).replace("__NEXT__", next), pkg)
}

fn readme_notes(prof: &str) -> (&'static str, &'static str) {
    match prof {
        "agent" => (
            "声明式 Agent：system prompt + 工具面 + 技能。宿主只读清单就能装配，不必执行包内代码。",
            "- 看能接进哪些宿主：`ncc hur interop .`（默认只预览，加 `--write` 才落盘）\n\
             - 想在本机沙箱里真跑：先放进入口产物，再把 `security.exec.enabled` 声明为 `true`\n\
             - 角色与工具面在 `hur.json` 的 `agent{}`；细节写进 `skills/<名字>.md`",
        ),
        "harness" => (
            "能力包：把上游 API 包成 harness，由 runtime 按 loader/entry 契约加载。",
            "- 端点写进 `src/<名字>.schema.json`，域名写进 `permissions.network`（否则 verify 直接红）\n\
             - 调用说明写进 `skills/<名字>.md` —— 宿主与人都从这份读\n\
             - 要真跑得先把 `security.exec.enabled` 声明为 `true`",
        ),
        "plugin" => (
            "宿主插件：声明它接进哪些宿主、提供什么。`agent.adapters` 里就是这次选的宿主。",
            "- 改宿主清单：`hur.json` 的 `agent.adapters`（可选 claude / cursor / cline / codex / mcp）\n\
             - 渲染成宿主能直接用的配置：`ncc hur interop . --targets claude --write`\n\
             - 插进宿主之后做什么，写在 `src/plugin.ts`",
        ),
        "mcp" => (
            "MCP server：`src/server.ts` 是一份**能直接跑**的最小 stdio 实现（JSON-RPC 2.0），\
             你在 `TOOLS` 与 `callTool` 里加自己的工具。",
            "- 先本地跑通：`node --experimental-strip-types src/server.ts`（Node ≥ 22.6；≥ 23.6 则可直接 `node src/server.ts`）\n\
             - 宿主配置里 command/args 指向它；**凭据只给名字，值由使用者在本机提供**（不进包）\n\
             - 要在本机沙箱里受限额执行：把 `security.exec.enabled` 声明为 `true`",
        ),
        "app" => (
            "可自部署应用（NCC 舱）：一条命令起得来，并说清要什么数据、出什么网。",
            "- 声明要什么数据：`hur.json` 的 `state{}`（节点托管的 kb / mem / ckpt / 集合）\n\
             - 声明出什么网：`egress{}` 的通道名 + `permissions.network` 的域名\n\
             - 想要一副完整的舱骨架（画布 / 记忆 / 检查点 / 分享）：`ncc app init`，再把逻辑搬进来",
        ),
        "scaffold" => (
            "工程脚手架：给别人当起点的模板，本身不是可运行的东西（所以没有入口）。",
            "- 模板在 `assets/template/` —— 只有 src / skills / kb / data / assets 算包内容，别放别处\n\
             - 别人怎么用：`ncc hur install <包引用>` 装到本机，再从落点把 `assets/template/` 拷出来\n\
             - 发布后 `ncc hur list` 能看到落点",
        ),
        "skill" => (
            "技能：一份或多份 SKILL.md —— 宿主直接读，不含代码入口（`profile=skill` 不允许 entry）。",
            "- 正文写 `skills/<名字>.md`：开头三行 frontmatter（name / description），\
             下面是「何时用 / 怎么做」\n\
             - 渲染到宿主：`ncc hur interop . --targets claude --write`\n\
             - 自己这台机器上用：装到本机 `ncc hur install <包引用>`（桌面端也从同一落点读）",
        ),
        _ => (
            "hur 包。",
            "- `ncc hur profile .` 看这份包「是什么 / 要什么 / 给什么 / 怎么接」",
        ),
    }
}

const README_TPL: &str = r#"# __NAME__

> `__SPEC__` 包 · profile=`__PROFILE__` · kind=`__KIND__` · 由 `ncc hur init` 生成
> （创作中心导出的目录与本目录同构）。

- id: `__ID__`
- version: `__VERSION__`
- short: `__SHORT__`
- entry: `__ENTRY__`
- 宿主: __HOSTS__

## 这是什么

__WHAT__

## 开发流程（前五步全程离线）

```bash
ncc hur verify .        # 校验（R1~R12）：规范 / 入口 / 依赖 / 权限面 / 摘要
ncc hur profile .       # 这份包“是什么 / 要什么 / 给什么 / 怎么接”（带体检）
ncc hur build .         # 锁依赖 → hur.lock
ncc hur pack .          # → dist/__ID__-__VERSION__.__PROFILE__.hur.gz + .sha256
ncc hur sign .          # 签名（私钥只在本机，不出设备）
ncc hur publish . --namespace @you     # 发布（联网）
```

## 下一步

__NEXT__

## 边界

- 对外调用的域名必须写进 `hur.json` 的 `permissions.network`，否则 `ncc hur verify` 直接失败（超范围调用）。
- `verify` / `build` / `pack` / `sign` 全程离线；只有 `publish` 与 `install` 联网。
- 凭据不进包：args / env 里只写**名字**，值由使用者在本机提供。
"#;

const SCAFFOLD_TEMPLATE_README: &str = r#"# __NAME__ · 起点模板

把这份模板拷出来当项目起点，然后**改 `hur.json`**（name / id / profile 都还是脚手架自己的）。

```bash
ncc hur install <包引用>     # 装到本机（`ncc hur list` 看落点）
cp -r <落点>/assets/template ./my-project
cd my-project && ncc hur init --kind agent --name "我的 Agent" --dir .
```
"#;

const SCAFFOLD_TEMPLATE_INDEX: &str = r#"// __NAME__ · 起点模板里的第一行代码
export function main(): void {
  console.log('__NAME__：把这里换成你的东西')
}

main()
"#;

const SKILL_MD: &str = r#"---
name: __SLUG__
description: 一句话说清“什么时候该用它”—— 宿主靠这行决定要不要加载这份技能
---

# __NAME__

## 何时用

- ……（写下触发条件：用户在做什么任务、提到什么关键词时，该走这份技能）

## 怎么做

1. ……
2. ……

## 注意

- 这里只写“怎么做”，不写代码：技能是**文档**（`profile=skill` 不允许带 `entry`）。
- 需要执行的动作交给宿主的工具，或者同目录的 harness 包。
"#;

const MCP_SERVER_TS: &str = r#"// __NAME__ · MCP server（由 `ncc hur init --kind mcp` 生成）
//
// 这是一份**能直接跑**的最小实现：stdio + newline-delimited JSON-RPC 2.0。
// 协议部分不用动，你只需要改 TOOLS 与 callTool。
//
// 本地跑：  node --experimental-strip-types src/server.ts
//          （Node ≥ 22.6；Node ≥ 23.6 可以直接 node src/server.ts；也可以 npx tsx src/server.ts）
// 宿主里：  command/args 指向它；凭据只给**名字**（值由使用者在本机提供，不进包）

type Json = Record<string, unknown>

const TOOLS = [
  {
    name: 'hello',
    description: '示例工具：把入参回显出来',
    inputSchema: {
      type: 'object',
      properties: { text: { type: 'string' } },
      required: ['text'],
    },
  },
]

function send(msg: Json): void {
  process.stdout.write(JSON.stringify(msg) + '\n')
}
function ok(id: unknown, result: unknown): void {
  send({ jsonrpc: '2.0', id, result })
}
function err(id: unknown, code: number, message: string): void {
  send({ jsonrpc: '2.0', id, error: { code, message } })
}

async function callTool(name: unknown, args: Json): Promise<unknown> {
  if (name === 'hello') {
    return { content: [{ type: 'text', text: `hello ${String(args.text ?? '')}` }] }
  }
  throw new Error('unknown tool: ' + String(name))
}

let buf = ''
process.stdin.on('data', (chunk) => {
  buf += chunk.toString()
  void drain()
})
process.stdin.on('end', () => process.exit(0))

async function drain(): Promise<void> {
  let i: number
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i).trim()
    buf = buf.slice(i + 1)
    if (!line) continue
    let msg: any
    try {
      msg = JSON.parse(line)
    } catch {
      continue // 不是 JSON 的行直接跳过（stdio 里混进日志是常事）
    }
    if (msg.id === undefined || msg.id === null) continue // 通知不回包
    switch (msg.method) {
      case 'initialize':
        ok(msg.id, {
          protocolVersion: '2024-11-05',
          capabilities: { tools: {} },
          serverInfo: { name: '__SLUG__', version: '__VERSION__' },
        })
        break
      case 'tools/list':
        ok(msg.id, { tools: TOOLS })
        break
      case 'tools/call':
        try {
          ok(msg.id, await callTool(msg.params?.name, msg.params?.arguments ?? {}))
        } catch (e) {
          err(msg.id, -32603, String((e as Error)?.message ?? e))
        }
        break
      default:
        err(msg.id, -32601, 'method not found: ' + String(msg.method))
    }
  }
}
"#;

const PLUGIN_TS: &str = r#"// __NAME__ · 宿主插件入口（由 `ncc hur init --kind plugin` 生成）
//
// “接进哪些宿主”是**声明**，写在 hur.json 的 agent.adapters（现在：__HOSTS__）——
// 那是可校验、还能渲染成宿主配置的一份说法；这个文件只放“插进宿主之后要做什么”。

export type Contribution = {
  host: string
  kind: 'tool' | 'prompt' | 'command'
  name: string
}

/** 宿主加载插件时会调用它；返回你要注册的东西。 */
export function register(host: string): Contribution[] {
  return [{ host, kind: 'tool', name: '__SLUG__.hello' }]
}

export default { register }
"#;

const APP_TS: &str = r#"// __NAME__ · 可自部署应用入口（由 `ncc hur init --kind app` 生成）
//
// 这一类包要能“一条命令起得来”，并说清两件事：
//   要什么数据 → hur.json 的 state{}（节点托管的 kb / mem / ckpt / 集合）
//   出什么网   → egress{} 的通道名 + permissions.network 的域名
// 想要一副完整的舱骨架（画布 / 记忆 / 检查点 / 分享）先跑 `ncc app init`，再把逻辑搬进来。

export async function start(): Promise<{ port: number }> {
  // TODO: 起你的服务。默认只监听本机回环（127.0.0.1），对外要单独声明
  return { port: 0 }
}

if (process.argv[1] && /app\.(ts|js)$/.test(process.argv[1])) {
  start()
    .then((s) => console.log('__NAME__ 已启动', s))
    .catch((e) => {
      console.error(e)
      process.exit(1)
    })
}

export default { start }
"#;
