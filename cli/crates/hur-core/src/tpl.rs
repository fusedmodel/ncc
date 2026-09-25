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
        spec: PKG_SPEC.to_string(),
        kind: input.kind.clone(),
        id,
        name: input.name.trim().to_string(),
        version: if input.version.trim().is_empty() { "0.1.0".to_string() } else { input.version.trim().to_string() },
        short,
        domain: domain.clone(),
        summary: input.summary.trim().to_string(),
        entry: if input.kind == "repo" { String::new() } else { "src/agent.ts".to_string() },
        runtime: "local-v0".to_string(),
        capabilities: if input.kind == "agent" {
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
        agent: if input.kind == "agent" {
            Some(AgentSpec {
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
            })
        } else {
            None
        },
        // 新工程默认走推荐档：本机 WASM 沙箱（能不能真跑由宿主策略与执行引擎决定）
        security: if input.kind == "agent" {
            Some(crate::policy::SecurityReq {
                policy: "wasm-local".into(),
                // 先只声明"打算用哪个引擎"；真正开执行要等作者提供 src/agent.wasm 再把 enabled 打开
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

/// 生成包内文件（相对路径 → 内容），**不含 hur.json / hur.lock**（由 init/pack 负责）
pub fn files_for(pkg: &HurPackage) -> Vec<(String, String)> {
    let slug_name = slug(&pkg.name);
    let mut out: Vec<(String, String)> = Vec::new();

    match pkg.kind.as_str() {
        "harness" => {
            out.push((
                "src/agent.ts".to_string(),
                format!(
                    r#"// {name} · Harness 能力包入口（由 `hur init --kind harness` 生成）
// 把上游 API 包成 Harness：端点定义见 src/{slug}.schema.json，调用域必须在 hur.json 的 permissions.network 里声明。

const ENDPOINTS = [
  {{ name: 'search', method: 'GET', path: '/search', description: '' }},
]

const BASE = '' // 例：https://api.example.com（记得同步 permissions.network）

export async function call<T = unknown>(path: string, input: Record<string, unknown> = {{}}): Promise<T> {{
  const ep = ENDPOINTS.find((e) => e.path === path)
  if (!ep) throw new Error('unknown endpoint ' + path)
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
        "repo" => {
            out.push((
                "src/index.json".to_string(),
                format!(
                    "{}\n",
                    serde_json::json!({
                        "repo": slug_name,
                        "short": pkg.short,
                        "domain": pkg.domain,
                        "packages": []
                    })
                ),
            ));
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

    out.push((
        "README.md".to_string(),
        format!(
            r#"# {name}

> `{spec}` 包 · 由 `hur init` 生成（创作中心 GUI 产出的目录与本目录同构）。

- id: `{id}`
- kind: `{kind}`
- version: `{version}`
- short: `{short}`
- entry: `{entry}`

## 开发流程

```bash
hur ls                 # 看包结构 / 依赖 / 权限面
hur verify             # 离线校验（R1~R5）
hur build              # 生成 hur.lock（依赖锁定）
hur pack               # 产出 dist/{id}-{version}.hur + sha256
hur publish --registry <url>   # 上传 + 发布到 registry
hur install <ns/slug>          # 装进 ~/.harnessuse/packages/ 供桌面 Agent 使用
```

## 边界

- 对外调用的域名必须写进 `hur.json` 的 `permissions.network`，否则 `hur verify` 直接失败（超范围调用）。
- `build` / `pack` / `verify` 全程离线；只有 `publish` / `install` 需要网络。
"#,
            name = pkg.name,
            spec = pkg.spec,
            id = pkg.id,
            kind = pkg.kind,
            version = pkg.version,
            short = pkg.short,
            entry = pkg.entry
        ),
    ));

    out.push((
        ".gitignore".to_string(),
        "dist/\nnode_modules/\ntarget/\n.DS_Store\n".to_string(),
    ));

    out
}
