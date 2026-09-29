// NCC MCP：把 NCC Registry 作为 MCP server 暴露给任意 Agent（Claude Desktop / Cursor /
// VS Code / 自研 Agent 等），让 Agent 能 agentic 地检索、取回、发布能力制品。
//
// 传输：stdio，逐行 JSON-RPC 2.0（MCP 的 stdio 约定：一行一条消息，不能有内嵌换行）。
//
// ⚠️ 铁律：stdout 只允许出现协议消息。任何日志/提示都必须写 stderr ——
//    否则客户端会把日志当成协议帧解析而报解析错误。
use crate::api;
use crate::api::urlenc;
use crate::capability;
use crate::config::{self, CliConfig};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// 本实现基于的 MCP 协议版本；客户端若声明了版本则回显，最大化兼容。
const PROTOCOL_VERSION: &str = "2024-11-05";

/// 单条工具返回的文本上限（避免把整篇内容灌进模型上下文）。
const MAX_TEXT: usize = 24000;

/// initialize 时下发给模型的用法说明（MCP 会把它放进系统上下文）。
const INSTRUCTIONS: &str = "\
NCC Registry 是中立、跨协议的能力制品目录（api / skill / mcp / harness / plugin / scaffold / docker-image / benchmark / living）。
把它当成「能力的 npm」来用：

1. 先查后造：用 ncc_list_kinds 看目录里有什么类型，用 ncc_search_catalog 检索是否已有可复用的能力。
2. 看细节：ncc_get_artifact 取元数据；ncc_fetch_artifact 取回正文（例如 SKILL.md 可直接读进上下文照做）。
3. 发布产出：ncc_publish_artifact 把成果发布成条目（需要先 `ncc login`，或配置 API-Key）。
4. 找人与定位：ncc_find_people 按角色检索人才目录；ncc_list_roles 拿角色 id；ncc_get_profile 看某人的名片（作品集 + 已发布能力）。
5. 找服务：公司 / 连锁集团把多条业务打包声明成**对外服务**。要办具体事时用 ncc_match_services 传意图（如「订杭州的酒店」），
   它会按提供方的匹配策略打分并给出接入步骤（端点 / 节点 / 能力包 / 是否需要授权）；ncc_list_services 浏览目录，ncc_get_service 看细节。
   非公开服务需要提供方授予 service 授权后才会展开接入细节。
6. 团队配置：内网 ncc-registry 上托管了团队的网络 / 基础设施 / Agent 配置。用 ncc_list_configs 看有什么（公开配置无需凭据），
   ncc_get_config 取一份（**内容默认打码**，只有带 reveal=true 才回明文；敏感配置是静态加密的）。
   要改配置（ncc registry config set / rollback）属于写操作，留在 CLI 里由用户执行。
7. 人脉（需凭据）：ncc_list_nodes 看节点连接表；ncc_region_profile 看区域分布；ncc_recommend_nodes 按区域要推荐。
8. 跨局域网直连（P2P，判断面）：ncc_p2p_probe 看**本机**的出网条件（纯本地，结论 direct|likely_direct|relay_likely|blocked）；
   ncc_p2p_node 看**目标内网节点那台机器**的条件与「可被打洞入口」是否开着；ncc_p2p_check 做真实打洞实测（0 字节）。
   三条红线要记住：**发现 ≠ 授权 ≠ 字节通道**（能连不等于能取数据）；**STUN 可由 NCC 托管，TURN 必须客户自托管**；
   **打洞失败就明确报错，绝不降级成 NCC 中转业务字节**。入口开关（`ncc registry p2p serve --on`）、
   票据（`ncc p2p ticket create/revoke`）与授权都属于用户自己拍的动作，不在 MCP 工具里。

9. 三样状态（知识库 / 记忆 / 检查点）：ncc_list_kb 查语料、ncc_get_kb 取正文（检索是**关键词加权**，不是向量检索）；
   ncc_list_mem / ncc_get_mem 读记忆（键值 + TTL + 来源）；ncc_list_ckpt 看不可变快照与血缘。
   **这三样都只有读工具**：写（ncc kb set / mem set / ckpt save）要用户自己跑 —— 让一次工具调用悄悄改写
   Agent 的记忆或知识，出问题没人能复盘是谁改的。知识库与检查点有公开档；**记忆没有公开档**。

10. 网关与合规审计（**只有读**）：客户自装的 `ncc gateway` 把逐请求审计留在**那台机器上**，只把
   窗口摘要上报控制面。ncc_list_gateways 看「我们有哪些网关、在线吗、这段时间用了多少」；
   ncc_gateway_audit 看某个网关留存下来的窗口摘要（计数、主机名、状态桶、延迟分位）；
   ncc_gateway_usage 看用量汇总。三条必须说清的边界：① **在线状态是控制面按心跳推导的**（超时即
   offline），不是网关自报；② 控制面**只有摘要** —— 路径 / 载荷 / 凭据一律不在，所以别指望从这里
   回答「谁调了哪个 URL」；③ 用量是**网关自报的计数**，签名只证明「是持有令牌的那台进程报的、
   没被改过」，**不证明内容为真**。注册 / 吊销 / 让网关开始上报都是用户自己的动作，不在工具里。

11. 索引与匹配（`ncc index` 的读面）：别人把「我能办什么 / 我要什么」**登记进一个频道**
   （自由频道名，如 booking/hotel）。要办事时用 ncc_match_index 传一句需求，它会按相关度
   排序并解释命中理由；ncc_list_index_channels 先看有哪些频道。
   两条边界：① **登记 / 撤回不在工具里**（那是改「别人能搜到我什么」，用户自己跑 `ncc index`）；
   ② 排序含**内部信誉权重**，但**评分不对外显示** —— 工具不会给出任何分数，别去猜，
   也别把「分高」当成承诺：接入仍然照旧要授权（制品要 grant、非公开服务要 service 授权）。

边界：节点（ncc_list_nodes / ncc_discover_nodes）、授权（ncc_list_grants）与你自己声明的服务属于用户的私人数据，
只在用户问起时用，不要转发给第三方。
声明服务、连接节点、授权这类会改变「别人能拿到什么」的动作只有读工具 —— 需要变更时，
让用户自己跑 CLI（ncc services add / ncc nodes link / ncc grant set / ncc index publish），并先征得同意。

制品引用统一写成 `@命名空间/slug`，也可用 `R-…` 形式的 id。
检索与取回是公开只读的，无需登录；节点类工具需要凭据（API-Key 需 nodes:read / grants:read）。";

/* ---------------- 入口 ---------------- */

/// MCP 工具 → 它需要的能力（与服务端的 capabilities 词表一致）。
/// 不在表里的工具（目录检索/取回/我是谁）属于基础能力，两边都声明，不做门禁。
fn tool_capability(tool: &str) -> Option<&'static str> {
    match tool {
        "ncc_match_services"
        | "ncc_list_services"
        | "ncc_get_service"
        | "ncc_service_categories" => Some("services"),
        // 索引与匹配：**只给读**（登记 / 撤回是改「别人能搜到我什么」，留在 CLI）
        "ncc_match_index" | "ncc_list_index_channels" => Some("index"),
        "ncc_list_configs" | "ncc_get_config" => Some("config"),
        "ncc_list_nodes" | "ncc_discover_nodes" | "ncc_region_profile" | "ncc_recommend_nodes" => {
            Some("nodes")
        }
        "ncc_list_grants" => Some("grants"),
        // 轨迹读取属于 trace 能力（节点侧声明了才有：轨迹默认私有，写入口留给 CLI）。
        "ncc_list_traces" | "ncc_trace_stats" => Some("trace"),
        // 三样状态：**只有读工具**（写是"改变 Agent 自己的认知/状态"，交给持有凭据的人）。
        "ncc_list_kb" | "ncc_get_kb" => Some("kb"),
        "ncc_list_mem" | "ncc_get_mem" => Some("mem"),
        "ncc_list_ckpt" => Some("ckpt"),
        // 通用记录仓：默认只给浏览（列集合 / 列记录 / 读一条）；
        // 写口子只由包声明产生（`ncc mcp --package`），见 `StoreScope`。
        "ncc_list_store_collections" | "ncc_list_store_records" | "ncc_get_store_record" => {
            Some("store")
        }
        n if n.starts_with("ncc_store_") => Some("store"),
        // 网关控制面（ncc.ai 声明 gateway；内网节点没有这个面）：**只有读工具** ——
        // 注册/吊销/改网关属于"改变谁能上报什么"，留给持凭据的人跑 CLI。
        "ncc_list_gateways" | "ncc_gateway_audit" | "ncc_gateway_usage" => Some("gateway"),
        // 节点侧的打洞入口状态由 ncc-registry 的 /api/p2p/self 回答（云端没有这个面）。
        // 本机预检与真实打洞都在**本地**算（不依赖目标），因此不做能力门禁。
        "ncc_p2p_node" => Some("p2p"),
        "ncc_list_roles" | "ncc_find_people" | "ncc_get_profile" => Some("profile"),
        _ => None,
    }
}

/// 一个 MCP 工具面（名字 + 说明书 + 工具表）。
/// 把「协议怎么收发」和「你有哪些工具」分开：`ncc mcp`（registry）与 `ncc hur mcp`（hur 治理）
/// 共用同一套收发实现 —— 协议细节只写一遍，才不会两边各错一种。
pub struct Face {
    pub name: &'static str,
    pub instructions: String,
    pub tools: Vec<Value>,
}

/// `ncc mcp` 的启动参数。
#[derive(Default)]
pub struct McpOptions {
    /// 只暴露这个包（目录或 hur.json）`state.stores[]` 声明过的集合
    pub package: String,
}

/// `ncc mcp`：在 stdin/stdout 上跑 MCP server，直到 stdin 关闭。
pub fn serve(cfg: &CliConfig, opts: &McpOptions) -> Result<()> {
    // 启动时探测一次目标：把「你现在连的是谁、它声明了什么」写进日志与 initialize 说明，
    // 避免 Agent 把云端工具打到内网节点（或反过来）。
    let meta = capability::probe(cfg);
    // 模型面 = 声明面：给了包就收窄到它声明的集合（详见 `StoreScope`）。
    let scope = load_store_scope(cfg, &opts.package)?;
    let scoped_note = match &scope {
        Some(s) if s.items.is_empty() => format!(
            "\n【模型面】已指定包 {}，但它的 `state.stores[]` 是空的 —— \
             所以这个面上**没有任何集合工具**（没声明 = 模型连名字都看不到）。\n",
            s.pkg
        ),
        Some(s) => {
            let items: Vec<String> = s
                .items
                .iter()
                .map(|(r, _)| format!("{}（{}）", r.collection.trim(), r.mode_norm()))
                .collect();
            format!(
                "\n【模型面】收窄到包 {} 声明的集合：{}。只读的集合不会给写口子，\
                 只写的集合不会给读口子；不在这个清单里的集合，这里既看不到也调不动。\n",
                s.pkg,
                items.join(" · ")
            )
        }
        None => String::new(),
    };
    let header = format!(
        "当前目标：{}（{} · {}）\n它声明了能力：{}\n不在其中的工具会明确报错，而不是静默失败。\n{}\n",
        cfg.current_name(),
        if meta.product.is_empty() { "未知服务端" } else { &meta.product },
        cfg.base_url(),
        meta.capability_line(),
        scoped_note
    );
    let target_line = format!(
        "【当前目标】{} · {} · {} · 能力：{}",
        cfg.current_name(),
        if meta.product.is_empty() {
            "未知服务端"
        } else {
            &meta.product
        },
        cfg.base_url(),
        meta.capability_line()
    );
    let base = tools();
    let mut face_tools = base.clone();
    face_tools.extend(store_tools(cfg, scope.as_ref()));
    let store_n = face_tools.len() - base.len();
    eprintln!(
        "[ncc-mcp] ready · {target_line} · 工具 {} 个（其中记录仓 {store_n}）· 等待 MCP 客户端握手",
        face_tools.len()
    );
    serve_face(
        Face {
            name: "ncc-registry",
            instructions: format!("{header}{INSTRUCTIONS}"),
            tools: face_tools,
        },
        move |params| call_tool_scoped(cfg, scope.as_ref(), params),
    )
}

/// 跑一个工具面：逐行 JSON-RPC 2.0，直到 stdin 关闭。
/// 通用部分（initialize / ping / tools.list / tools.call / 错误码）都在这儿；
/// `call` 只管「这个名字 + 这些参数 → 结果」。
pub fn serve_face(face: Face, mut call: impl FnMut(Option<&Value>) -> Result<Value>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let tag = face.name;

    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // 解析不了就没有 id 可回，只能记到 stderr
                eprintln!("[ncc-mcp] 非法 JSON 帧: {e}");
                continue;
            }
        };
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        // 通知（无 id）不需要响应；notifications/* 一律忽略
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        if method.starts_with("notifications/") {
            continue;
        }

        let params = req.get("params");
        // 未知方法按 JSON-RPC 规范回 -32601，而不是 -32603（内部错误）
        if !matches!(
            method.as_str(),
            "initialize" | "ping" | "tools/list" | "tools/call"
        ) {
            let resp = json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("未知方法: {method}") }
            });
            writeln!(out, "{}", serde_json::to_string(&resp)?)?;
            out.flush()?;
            continue;
        }

        let res = match method.as_str() {
            "initialize" => {
                let pv = params
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSION);
                Ok(json!({
                    "protocolVersion": pv,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": face.name, "version": env!("CARGO_PKG_VERSION") },
                    "instructions": face.instructions,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": face.tools })),
            "tools/call" => call(params),
            _ => unreachable!("上面已过滤未知方法"),
        };
        let resp = match res {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32603, "message": format!("{e:#}") }
            }),
        };
        writeln!(out, "{}", serde_json::to_string(&resp)?)?;
        out.flush()?;
    }
    eprintln!("[{tag}] stdin 关闭，退出");
    Ok(())
}

/* ---------------- 工具定义 ---------------- */

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "ncc_list_kinds",
            "description": "列出 NCC 目录里的能力类型（kind）及各类型已发布数量。开始检索前先用它了解目录构成。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_search_catalog",
            "description": "跨命名空间检索能力制品。返回 kind、引用（@命名空间/slug）、版本、摘要、标签与下载量。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "关键词（可选，匹配名称/摘要/slug/描述）" },
                    "kind": { "type": "string", "description": "按类型过滤：api|harness|hur|skill|mcp|plugin|scaffold|docker-image|benchmark|living" },
                    "tag": { "type": "string", "description": "按标签过滤" },
                    "namespace": { "type": "string", "description": "限定命名空间，如 @aya" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_artifact",
            "description": "查看单个制品的完整元数据（含 version、tags、sha256、storage.url、manifest 契约、signature 签名）。注意 signature 只说明「谁签了哪份字节」：公钥是否认识要看 keynum，随签名带的 pubkey 只是发布方的声明（自证）—— 别把它当成已验证。",
            "inputSchema": {
                "type": "object",
                "properties": { "ref": { "type": "string", "description": "制品引用 @命名空间/slug，或 R-… 形式的 id（旧名 target 仍兼容）" } },
                "required": ["ref"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_fetch_artifact",
            "description": "取回制品正文。文本类制品（SKILL.md / JSON / YAML / 脚本等）直接返回内容，可直接照做；二进制只返回下载地址与 sha256。",
            "inputSchema": {
                "type": "object",
                "properties": { "ref": { "type": "string", "description": "制品引用 @命名空间/slug 或 R-…（旧名 target 仍兼容）" } },
                "required": ["ref"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_publish_artifact",
            "description": "发布一个能力制品（需要已登录，或配置了 API-Key）。三种字节来源任选其一：content（内联文本）、contentFile（本地文件路径）、url（自带存储直链）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "制品类型，见 ncc_list_kinds" },
                    "name": { "type": "string", "description": "展示名称" },
                    "summary": { "type": "string", "description": "一句话说明" },
                    "slug": { "type": "string", "description": "URL slug；省略则由服务端依据 name 生成" },
                    "version": { "type": "string", "description": "版本，默认 1.0.0" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "标签" },
                    "content": { "type": "string", "description": "制品正文（文本）。会以 filename 作为文件名上传" },
                    "filename": { "type": "string", "description": "配合 content 使用的文件名，如 hotel.SKILL.md" },
                    "contentFile": { "type": "string", "description": "本地文件路径（与 content 二选一）" },
                    "url": { "type": "string", "description": "自带存储：直接发布一个直链，不上传字节" },
                    "manifest": { "type": "object", "description": "封装契约（kind=harness 时用，含 harness.loader / harness.entry）" },
                    "visibility": { "type": "string", "description": "public（默认）| private（需付费套餐）" },
                    "draft": { "type": "boolean", "description": "以 draft 状态创建" }
                },
                "required": ["kind", "name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_whoami",
            "description": "查看当前 CLI 登录的账号与可用的命名空间（发布前用它确认身份与目标命名空间）。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_list_roles",
            "description": "列出 NCC Profile 的工作角色目录（6 组 20 个，含中英标签与描述）。这些 id 用于 ncc_find_people 的 role 过滤与名片设置。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_find_people",
            "description": "按角色 / 技能 / 关键词检索人才目录，找到定位匹配的人（回答「谁做过这类事」）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "角色 id，见 ncc_list_roles，如 fde、aigc-creator" },
                    "skill": { "type": "string", "description": "技能标签，如 RAG" },
                    "query": { "type": "string", "description": "关键词（匹配名字/头衔/技能）" },
                    "availability": { "type": "string", "description": "接洽状态：open|collab|hiring|busy" },
                    "following": { "type": "boolean", "description": "只看我关注的人（需要已登录；未登录会报 401 而不是返回空）" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_profile",
            "description": "查看某人的 NCC Profile：定位角色、技能、外链、作品集，以及他发布的能力与分享页。省略 username 则看当前登录账号。",
            "inputSchema": {
                "type": "object",
                "properties": { "username": { "type": "string", "description": "用户名（不带 @）；省略则为当前登录账号" } },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_match_services",
            "description": "按**意图**匹配对外服务：公司 / 连锁集团把多条业务打包声明成服务，这里传入自然语言意图（如「帮我订杭州的酒店」「门店要巡检」），服务端按提供方的匹配策略打分并解释命中理由，同时给出接入步骤（端点 / MCP 地址 / 能力包 / 执行节点 / 是否需要授权）。要办事时先用它找「该找谁」。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "intent": { "type": "string", "description": "自然语言意图，如：帮我订杭州的酒店" },
                    "category": { "type": "string", "description": "业务分类 id，见 ncc_service_categories，如 booking、inspection" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "能力标签（可选，可多个）" },
                    "region": { "type": "string", "description": "区域，如 杭州（不限区域的服务也算覆盖）" },
                    "limit": { "type": "integer", "description": "返回条数，默认 5，最大 20" }
                },
                "required": ["intent"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_match_index",
            "description": "按**一句需求**在索引里找人：别人（或别人的 Agent）把「我能办什么 / 我要什么」登记进一个频道，这里传自然语言意图（如「帮我订杭州的酒店」），服务端按相关度打分并解释命中理由，同时给出怎么接过去。默认找**能办这件事的人**；want=need 则是找需求（给服务方找活）。排序含内部信誉权重，**评分不对外显示**，工具也不会给出分数。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "intent": { "type": "string", "description": "自然语言需求，如：帮我订杭州的酒店" },
                    "channel": { "type": "string", "description": "频道（可用前缀，如 booking 命中 booking/hotel）；不确定就不传" },
                    "region": { "type": "string", "description": "区域，如 杭州" },
                    "want": { "type": "string", "enum": ["supply", "need"], "description": "supply（默认）= 找能办事的人；need = 找需求" },
                    "limit": { "type": "integer", "description": "返回条数，默认 5，最大 20" }
                },
                "required": ["intent"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_index_channels",
            "description": "列出索引里的**频道**（检索空间）与各频道的条数 / 供给与需求分布 / 关联的业务分类。当你不确定该往哪个频道找时先看它。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prefix": { "type": "string", "description": "只看某个频道前缀，如 booking" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_services",
            "description": "浏览对外服务目录（不传意图，按分类 / 标签 / 区域过滤）。返回服务名称、业务分类、接入方式、授权方式与「何时找我」。想按需求找人办事请用 ncc_match_services。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "category": { "type": "string", "description": "业务分类 id，见 ncc_service_categories" },
                    "tag": { "type": "string", "description": "能力标签" },
                    "region": { "type": "string", "description": "覆盖区域" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_service",
            "description": "看一条对外服务的完整接入信息：端点 / 执行节点 / 能力包 / 接入步骤 / 响应与计费 / 条款。非公开服务在你拿到提供方的 service 授权前只会返回「需要授权」与申请办法。",
            "inputSchema": {
                "type": "object",
                "properties": { "ref": { "type": "string", "description": "服务引用：@提供方/标识（如 @aya/hotel-booking）或 SV-… id（旧名 target 仍兼容）" } },
                "required": ["ref"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_service_categories",
            "description": "列出对外服务的业务分类目录（六组，含中英标签与已有服务数）与接入方式 / 授权方式取值。用于给 ncc_match_services 的 category 参数取值。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_list_configs",
            "description": "列出内网 registry 上托管的**团队配置**（网络 / 网关 / 基础设施 / Agent / CI / 安全…）。公开且生效的配置无需凭据即可看到；自己命名空间的配置传 mine=true；敏感配置（secret）内容静态加密、默认只回校验和。改配置请让用户跑 CLI（ncc registry config set）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": { "type": "string", "description": "限定团队命名空间，如 @team 或 team" },
                    "kind": { "type": "string", "description": "配置类型：network|gateway|infra|registry|agent|ci|observability|security|app|other" },
                    "env": { "type": "string", "description": "环境：any|dev|staging|prod（含 any 通用项）" },
                    "mine": { "type": "boolean", "description": "只看我（owner/成员）命名空间里的配置" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_config",
            "description": "取一份托管配置的元数据与（可选的）内容。**内容默认打码**：要明文必须显式 reveal=true，而且需要配置读取权限（config:read + 命名空间成员身份或 config 授权）。含敏感值的配置在服务端是加密存储的，取明文前先确认用户确实需要。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "配置引用：@命名空间/slug（如 @team/network）或 C-… id（旧名 target 仍兼容）" },
                    "reveal": { "type": "boolean", "description": "是否取明文（默认 false，只回校验和与大小）" },
                    "revision": { "type": "integer", "description": "取历史版本（见版本历史）" }
                },
                "required": ["ref"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_nodes",
            "description": "列出当前账号的 NCC Node：mine = 我自己注册的节点（service 服务 / agent 为人服务的 Agent / assigned 被分配的 Agent，带在线状态与提供能力），links = 我连接的别人的节点（带我给它的 Name 标签）。这是**节点连接表**，不是通讯录；连接只代表「找得到」，不代表能取对方数据。需要登录/API-Key；属于本人数据，不要向无关第三方转发。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "按类型过滤：service | agent | assigned" },
                    "q": { "type": "string", "description": "关键词（Name 标签 / 节点名 / 用户名）" },
                    "can": { "type": "string", "description": "按**提供能力**过滤（逗号分隔，要与全部匹配）。取值：run:wasm, run:js, run:process, run:container, run:remote, egress:llm, egress:internet, serve:http, serve:mcp, artifact, directory, config, share；历史短名 mcp/api/wasm/llm 也认" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_discover_nodes",
            "description": "发现**同一个 NCC 实例**上可连接的节点（公开 + 不是我的 + 我还没连）。想连某个能力/服务/Agent 时先用它找到节点引用（@命名空间/节点slug），再让用户跑 `ncc nodes link` 建立连接。注意：只用 `can` 过滤返回的是**节点**，不代表对方已授权你调用它 —— 取数据仍要对方 grant。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "按类型过滤：service | agent | assigned" },
                    "region": { "type": "string", "description": "按节点归属者所在地筛选，如 杭州" },
                    "can": { "type": "string", "description": "按**提供能力**过滤（逗号分隔，要与全部匹配），例如 run:wasm（能跑沙箱）、egress:llm（有模型 API 出口）。取值见 ncc_list_nodes 的 can 说明" },
                    "limit": { "type": "integer", "description": "返回条数，默认 30，最大 60" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_region_profile",
            "description": "当前账号的**节点覆盖图**：我的节点与连接的节点按归属者所在地聚合，并给出类型分布。用于回答「我的 Agent 网络在哪些区域更密」。区域来自节点归属者的名片所在地。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "ncc_recommend_nodes",
            "description": "基于区域密度推荐可连接的**节点**（在服务端已按“同区域已有几个节点”降序，并标注 regionNodes）。比直接 discover 更贴近“该先连谁”。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "region": { "type": "string", "description": "按区域筛选，如 杭州" },
                    "kind": { "type": "string", "description": "按类型筛选：service | agent | assigned" },
                    "can": { "type": "string", "description": "按**提供能力**过滤（逗号分隔，要与全部匹配），如 run:wasm / egress:llm；取值见 ncc_list_nodes 的 can 说明" },
                    "limit": { "type": "integer", "description": "返回条数，默认 20，最大 50" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_grants",
            "description": "查看制品/分享授权关系：outgoing 是我授权给别人的（谁能下载我的私有制品、谁可看我的私有分享页），incoming 是别人给我的。注意：**连接 ≠ 授权** —— 连上一个节点并不代表能取它的数据。",
            "inputSchema": {
                "type": "object",
                "properties": { "direction": { "type": "string", "description": "outgoing（默认）| incoming" } },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_traces",
            "description": "列出**运行轨迹**（Agent 会话 / HUR 执行）：能力评估与后训练数据集的原料。可按 ref（制品）、kind、status、tag、时间过滤。轨迹默认私有，只能看到你有权的那些；**内容级别** payload=digest 表示这条只有哈希与结构、没有提示词与输出原文（这是默认，也是安全的那一档）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "只看向这个制品（@命名空间/slug）的轨迹 —— 评测改版前后就靠它" },
                    "kind": { "type": "string", "description": "hur-run（HUR 执行）| agent（Agent 会话）" },
                    "status": { "type": "string", "description": "ok | error | cancelled" },
                    "agent": { "type": "string", "description": "按 Agent 形态过滤（如 harness-use）" },
                    "tag": { "type": "string", "description": "按标签过滤" },
                    "since": { "type": "string", "description": "起始时间：RFC3339 / 2026-09-26 / unix 秒" },
                    "limit": { "type": "integer", "description": "最多几条（默认 20，上限 200）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_trace_stats",
            "description": "运行轨迹的**聚合结论**：成功/失败/取消各多少、耗时 p50/p90、token 与花费、按制品版本分组（改版前后对比）、标注覆盖率与结论分布、失败归类。回答「这个 Agent / 这个包的能力怎么样」时用它；注意 score 只对**打过分的**轨迹求平均 —— 没标注就别下结论。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "只看这个制品的轨迹" },
                    "kind": { "type": "string", "description": "hur-run | agent" },
                    "since": { "type": "string", "description": "起始时间（RFC3339 / 2026-09-26 / unix 秒）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_p2p_probe",
            "description": "本机打洞条件预检（**纯本地**：不需要登录、不需要对端）：UDP 出站能不能用、能否拿到公网映射（srflx）、NAT 映射行为（锥形/对称）与过滤行为（RFC 5780）。结论 direct | likely_direct | relay_likely | blocked。回答「这台机器能不能跟别的节点直连」时先用它；结论是 relay_likely / blocked 时不要承诺直连（TURN 由客户自托管，NCC 不中转业务字节）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "stun": { "type": "string", "description": "只用这些 STUN（逗号分隔，如 stun:stun.qq.com:3478）；不给就先用目标下发的 ICE 配置" },
                    "offline": { "type": "boolean", "description": "true = 完全不问服务端（纯离线预检）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_p2p_check",
            "description": "真实打洞实测（**0 字节**，不传业务数据）：与对端互打 STUN Binding 请求，收到回包即证明这条路径允许入向（≈ ICE connectivity check），耗 2～20 秒。两种用法：addr = 直接对一个映射地址 ip:port 对打（不走信令、不需登录，适合内网节点 `ncc registry p2p self` 报出的 mapped）；peer = 对端节点引用（走控制面信令，需登录，且**两端要在同一分钟内各跑一次**）。⚠️ 打洞必须双方同时发起，单边跑一定失败；本机 NAT 过滤是地址/端口相关时（绝大多数家用与企业网），对方即使开着入口也收不到单边包。不要用中心搬运静默兜底。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "addr": { "type": "string", "description": "对端映射地址 ip:port（与 peer 二选一）；通常来自对端 `ncc_p2p_node` 的 mapped" },
                    "peer": { "type": "string", "description": "对端节点引用 LD-… 或 @命名空间/节点slug（与 addr 二选一）" },
                    "from": { "type": "string", "description": "我以哪个节点身份参与（缺省：我名下唯一的 living 节点）" },
                    "wait": { "type": "integer", "description": "等对端与收包的秒数，默认 12，最大 60" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_p2p_node",
            "description": "目标 ncc-registry 节点**那台机器**的 NAT 画像与「可被打洞入口」状态（与 ncc_p2p_probe 的区别：那个算的是跑 MCP 的**本机**；内网节点的出口 NAT 常完全是另一回事）。回答「能不能直连那台节点」时用它看 verdict / mapped / 入口是否开着。提示：入口的 peer（对端映射）必须由信令下发才能长期可用，不是配置项。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        // ---- 三样状态：知识库 / 记忆 / 检查点（**只读**）----
        // 为什么只给读：写状态是"改变 Agent 自己的认知与检查点"，该由持有凭据的人
        // 显式跑 CLI 决定。工具里给一个"改记忆"的口子，等于让一次工具调用悄悄改写
        // Agent 的记忆 —— 出问题没人能复盘是谁改的。
        json!({
            "name": "ncc_list_kb",
            "description": "查**托管知识库**（Agent 的语料）：按命名空间 / 类型 / 标签 / 关键词列出文档。检索是**关键词加权**（标题 3 / 摘要 2 / 正文 1），不是向量检索 —— 换个说法不一定命中；要正文就用 ncc_get_kb。默认只给你可见的：公开的 + 你自己的 + 别人授权给你的。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": { "type": "string", "description": "只看某个命名空间（@team 或 team）" },
                    "kind": { "type": "string", "description": "doc | faq | notes | spec | transcript" },
                    "tag": { "type": "string", "description": "按标签过滤" },
                    "query": { "type": "string", "description": "关键词（走服务端打分检索）" },
                    "limit": { "type": "integer", "description": "最多几条（默认 20，上限 200）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_kb",
            "description": "读一篇**托管知识库文档**的正文（引用写作 @命名空间/slug；也可用 KD-… 或 slug）。拿到的正文与 checksum 就是节点上那份（要核对就用 checksum 自己算一遍）。默认给最新版，要历史版本用 revision。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "@命名空间/slug 或 KD-…" },
                    "revision": { "type": "integer", "description": "取第 N 版（不给 = 最新）" }
                },
                "required": ["ref"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_mem",
            "description": "查**托管记忆**（键值 + TTL + 来源）：跨运行/跨机器记得住的那部分。可按 subject（谁的记忆）、key 前缀、kind、来源过滤。过期的默认不算（读时即视为不存在）。注意：记忆**默认私有**，只有你自己的/被授权的看得到；要写记忆请让用户跑 `ncc mem set`（写不在工具里）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": { "type": "string", "description": "哪个命名空间（默认你的）" },
                    "subject": { "type": "string", "description": "谁的记忆（如 self / planner）" },
                    "prefix": { "type": "string", "description": "键前缀" },
                    "kind": { "type": "string", "description": "fact | preference | episode | summary | pointer" },
                    "source": { "type": "string", "description": "按来源过滤（traceId / ckpt 引用）" },
                    "limit": { "type": "integer", "description": "最多几条（默认 50，上限 1000）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_get_mem",
            "description": "按 key 精确读一条记忆（默认 subject=self）：这是 Agent 读自己记忆的主路径。带 TTL 的会给出过期时间；读不到就是没有（或已过期）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "记忆的键" },
                    "subject": { "type": "string", "description": "谁的记忆（默认 self）" },
                    "namespace": { "type": "string", "description": "哪个命名空间（默认你的）" }
                },
                "required": ["key"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_list_ckpt",
            "description": "查**托管检查点**（不可变快照 + 血缘）：交接与回滚的落点。可按 ref（属于哪个制品）、label（打点粒度）、名字过滤。检查点不可变（没有「改」这个动作），字节取回要走 `ncc ckpt pull`（客户端会核对摘要）—— 工具里只给元数据与摘要，不给字节通道。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "只看属于这个制品的点（@命名空间/slug）" },
                    "label": { "type": "string", "description": "episode | step | run | release | handoff | manual" },
                    "query": { "type": "string", "description": "名字里含这个词" },
                    "limit": { "type": "integer", "description": "最多几条（默认 20，上限 200）" }
                },
                "additionalProperties": false
            }
        }),
        // ---- 网关控制面（**只读**）----
        // 为什么只给读：注册网关 / 吊销 / 改名字都是"改变谁能上报什么"的动作，
        // 属于持凭据的人；工具里给一个"注册一台网关"的口子很容易变成后门。
        json!({
            "name": "ncc_list_gateways",
            "description": "列出**我（或我的组织）名下的 NCC Gateway**：名字、命名空间、在线状态、版本、最近心跳、已收摘要窗口数，以及这段时间的用量（请求/放行/拒绝/出站字节）。回答「我们有几台网关 / 哪台掉线了 / 最近有多少请求被拒」用它。⚠ 两条口径：**在线状态是控制面按心跳超时推导的**（不是网关自报），`statusReported` 才是网关自己说的（online/draining）；用量是**网关自报的计数**。要细节（主机名、状态桶、延迟分位）用 ncc_gateway_audit。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": { "type": "string", "description": "只看某个命名空间（@team 或 team）；不给 = 我名下所有命名空间" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_gateway_audit",
            "description": "读某个网关**留存下来的审计摘要**（一个时间窗一条）：请求/放行/拒绝数、出站入站字节、延迟 p50/p95/max、**主机名聚合**、状态码桶、拒绝理由、路由与方向。合规问题（“这周有多少请求被拒、去的是哪些域名”）靠它回答。⚠ **控制面只有摘要**：路径、请求载荷、凭据一律不在 —— 路径里常带订单号这类业务标识，那是**故意不上报**的；所以别用这个工具回答“谁调了哪个 URL”（那要去**那台网关本机**跑 `ncc gateway audit`）。另：这些计数是网关**自报**的，签名只证明来源与完整，不证明内容为真。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "gateway": { "type": "string", "description": "网关 id（GW-…）或名字；名字不唯一时会列出候选" },
                    "since": { "type": "string", "description": "只看这个时间之后的窗口（RFC3339 / 2026-09-27 / unix 秒）" },
                    "limit": { "type": "integer", "description": "最多几个窗口（默认 20，上限 200）" }
                },
                "required": ["gateway"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "ncc_gateway_usage",
            "description": "某个网关的**用量汇总**（窗口数、活跃天数、请求/放行/拒绝、出站字节、首末窗口时间）。回答「这台网关这个月用了多少」用它。⚠ 计数是**网关自报**的：NCC 不碰业务数据，所以服务端没有独立计量；签名证明的是来源与完整，不是内容为真。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "gateway": { "type": "string", "description": "网关 id（GW-…）或名字" },
                    "since": { "type": "string", "description": "统计起点（默认按留存期）" }
                },
                "required": ["gateway"],
                "additionalProperties": false
            }
        }),
    ]
}

/* ---------------- 通用记录仓：模型面 = 声明面 ---------------- */

/// `ncc mcp --package <包>` 时，模型面收窄到这个包声明过的集合。
///
/// 为什么要有这一层：`ncc mcp` 默认暴露的是「当前凭据能看到的 NCC」，而一个 HUR 包
/// （插件 / harness）需要的是**恰好它能用的那部分** —— 多了是风险（模型看得到不该看的），
/// 少了是幻觉（模型以为自己能调）。于是：
///
///   - **没给 `--package`**：只给「浏览」用的只读工具（列集合 / 列记录 / 读一条），
///     与 kb / mem 同一档：**写要由持有凭据的人显式跑 CLI 决定**；
///   - **给了 `--package`**：只有 `state.stores[]` 里声明过的集合有工具。
///     读工具按 `mode` 给（`read` / `readwrite`），**写工具只有声明了写才给** ——
///     没声明 = 模型连名字都看不到；
///   - `tools/list` 里看不到的，`tools/call` 也**真调不动**（这里拦，不只是不广告）。
///
/// 字段的形状**不问包问节点**：声明语法（`title:string!`）的唯一实现在节点侧，
/// 这边只把节点解析好的 `name/type/enum/require` 翻译成 JSON Schema ——
/// 再写一套解析就会得到「包校验过了、节点上却是另一种读法」。
struct StoreScope {
    /// 包 id + 版本（写进说明书：模型知道自己被限在哪个包的面上）
    pkg: String,
    /// 声明了**且这台节点上真有**的集合：需求 + 节点上的声明（字段形状从这里来）
    items: Vec<(hur_core::spec::StoreRequirement, Value)>,
    /// 包声明了、但这台节点上没有的集合。
    ///
    /// 单独记一笔是为了说清楚：没声明 ≠ 声明了但没落地 —— 前者是「不给你」，
    /// 后者是「你少了一步」。（工具表里两者都不出现，报错时分开说。）
    missing: Vec<String>,
}

/// 读包的 `state.stores[]`，并与节点上的实际声明对一遍（没给 `--package` 就是 None）。
///
/// 为什么在这里就对一遍：模型面的边界必须在**启动时定下来** —— 工具表、门禁、
/// 说明书三处说的是同一件事，模型才不会遇到「广告了却调不动」或反过来。
fn load_store_scope(cfg: &CliConfig, path: &str) -> Result<Option<StoreScope>> {
    let p = path.trim();
    if p.is_empty() {
        return Ok(None);
    }
    let root = hur_core::pack::find_root(std::path::Path::new(p))?;
    let pkg = hur_core::spec::read_pkg(&root)?;
    let declared = pkg
        .state
        .as_ref()
        .map(|s| s.stores.clone())
        .unwrap_or_default();
    let cols = node_collections(cfg);
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for req in declared {
        let kind = req.collection.trim();
        match cols.iter().find(|c| c["kind"].as_str() == Some(kind)) {
            Some(c) => items.push((req, c.clone())),
            None => {
                eprintln!(
                    "[ncc-mcp] 包声明了集合「{kind}」，但这台节点上没有 —— \
                     模型面不给它（先在该节点上 ncc store declare）"
                );
                missing.push(kind.to_string());
            }
        }
    }
    Ok(Some(StoreScope {
        pkg: format!("{}@{}", pkg.id, pkg.version),
        items,
        missing,
    }))
}

/// 从节点取回集合声明（拿不到就当没有 —— **不猜**）。
fn node_collections(cfg: &CliConfig) -> Vec<Value> {
    let token = config::token_opt(cfg);
    match api::get(cfg, "/api/store", token.as_deref()) {
        Ok(v) => v["collections"].as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 声明字段 → JSON Schema（`type` 由节点解析，这边只翻译）。
fn field_schema(f: &Value) -> Value {
    match f["type"].as_str().unwrap_or("string") {
        "int" => json!({ "type": "integer" }),
        "bool" => json!({ "type": "boolean" }),
        "string[]" => {
            json!({ "type": "array", "items": { "type": "string" }, "description": "多个值用逗号分隔" })
        }
        // 词表直接从声明来：模型**看得见**合法取值，就不会去试错
        "enum" => json!({ "type": "string", "enum": f["enum"] }),
        "text" => json!({ "type": "string", "description": "长文本正文" }),
        "ref" => json!({ "type": "string", "description": "引用：@某人 或 @命名空间/slug" }),
        _ => json!({ "type": "string" }),
    }
}

/// 一个集合的字段区（写工具的入参）：类型、词表、必填全来自声明。
fn fields_object(col: &Value) -> (serde_json::Map<String, Value>, Vec<String>) {
    let mut props = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for f in col["fields"].as_array().into_iter().flatten() {
        let name = f["name"].as_str().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let mut sch = field_schema(f);
        if f["require"].as_bool().unwrap_or(false) {
            required.push(name.to_string());
        }
        if f["search"].as_bool().unwrap_or(false) {
            if let Some(o) = sch.as_object_mut() {
                o.insert(
                    "description".into(),
                    json!("这份内容进搜索文本（--q 能搜到）"),
                );
            }
        }
        props.insert(name.to_string(), sch);
    }
    (props, required)
}

/// 列表工具的过滤入参：**只广告能过滤的字段**（`index` 里声明过的）。
/// 广告一个服务端会 400 的过滤条件，等于让模型去撞墙。
fn filters_object(col: &Value) -> serde_json::Map<String, Value> {
    let mut props = serde_json::Map::new();
    let declared: Vec<Value> = col["fields"].as_array().cloned().unwrap_or_default();
    for f in col["index"].as_array().into_iter().flatten() {
        let name = f.as_str().unwrap_or("");
        if name.is_empty() || name == "q" {
            continue;
        }
        let desc = declared
            .iter()
            .find(|d| d["name"].as_str() == Some(name))
            .map(|d| match d["enum"].as_array() {
                Some(v) if !v.is_empty() => format!(
                    "按 {name} 过滤（取值：{}）",
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
                _ => format!("按 {name} 过滤"),
            })
            .unwrap_or_else(|| format!("按 {name} 过滤"));
        props.insert(
            name.to_string(),
            json!({ "type": "string", "description": desc }),
        );
    }
    props
}

/// 字段形状的一句话摘要（写进工具说明）。
fn fields_brief(col: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for f in col["fields"].as_array().into_iter().flatten() {
        let n = f["name"].as_str().unwrap_or("");
        let t = f["type"].as_str().unwrap_or("string");
        let mut s = format!("{n}:{t}");
        if let Some(v) = f["enum"].as_array().filter(|v| !v.is_empty()) {
            // 词表直接写进说明：模型看得见合法取值，就不会拿错值去试
            s = format!(
                "{}:{}",
                n,
                v.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("/")
            );
        } else if t == "enum" {
            s = format!("{n}:enum");
        }
        if f["require"].as_bool().unwrap_or(false) {
            s.push('!');
        }
        parts.push(s);
    }
    if parts.is_empty() {
        "（没声明字段）".to_string()
    } else {
        parts.join(" · ")
    }
}

/// 建模型面工具表。
fn store_tools(cfg: &CliConfig, scope: Option<&StoreScope>) -> Vec<Value> {
    let cols = node_collections(cfg);
    let Some(sc) = scope else {
        // 没给包：只给「浏览」，不给写口子。
        let have: Vec<String> = cols
            .iter()
            .filter_map(|c| c["kind"].as_str().map(str::to_string))
            .collect();
        let line = if have.is_empty() {
            "这台节点上还没有集合（先用 ncc store declare 声明一个）".to_string()
        } else {
            format!("这台节点上的集合：{}", have.join(" / "))
        };
        return vec![
            json!({
                "name": "ncc_list_store_collections",
                "description": format!("列这台节点上的**集合**（一类内容 = 一个集合）：字段、能不能改、可见性、有多少条。{line}"),
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            }),
            json!({
                "name": "ncc_list_store_records",
                "description": "列某个集合里的记录（默认不含归档与过期）。`q` 是**子串**匹配（key / 标签 / 声明为 ?search 的字段 / 正文），不是索引检索；按字段精确过滤用 `filter` 里的字段名。公开档是**逐集合**决定的：集合不公开时，里面的记录也不会因为自己写了 public 而可读。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "collection": { "type": "string", "description": format!("集合名（{line}）") },
                        "q": { "type": "string", "description": "子串搜索" },
                        "filter": { "type": "object", "description": "按声明过的可过滤字段精确匹配（字段名 → 值）", "additionalProperties": { "type": "string" } },
                        "archived": { "type": "boolean", "description": "连归档的一起列" },
                        "limit": { "type": "integer", "description": "最多几条（默认 20，上限 200）" }
                    },
                    "required": ["collection"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "ncc_get_store_record",
                "description": "读一条记录（含正文）。拿到的正文与 checksum 就是节点上那份。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "collection": { "type": "string", "description": "集合名" },
                        "key": { "type": "string", "description": "记录的 key" }
                    },
                    "required": ["collection", "key"],
                    "additionalProperties": false
                }
            }),
        ];
    };

    let mut out: Vec<Value> = Vec::new();
    for (req, col) in &sc.items {
        let kind = req.collection.trim();
        let col = col.clone();
        let mode = req.mode_norm();
        let vis = col["visibility"].as_str().unwrap_or("private");
        let shape = if col["mutable"].as_bool().unwrap_or(false) {
            "可改"
        } else {
            "不可变"
        };
        let brief = fields_brief(&col);
        let (props, required) = fields_object(&col);
        let reads = matches!(mode, "read" | "readwrite");
        let writes = matches!(mode, "write" | "readwrite");

        if reads {
            let mut list_props = serde_json::Map::new();
            // 过滤写成一个**嵌套对象**而不是平铺参数：集合完全可以声明一个叫
            // `archived` / `q` / `limit` 的字段，平铺就撞车了。
            list_props.insert(
                "filter".into(),
                json!({
                    "type": "object",
                    "description": "按声明过的可过滤字段精确匹配（字段名 → 值，值都是字符串）",
                    "properties": Value::Object(filters_object(&col)),
                    "additionalProperties": false
                }),
            );
            list_props.insert("q".into(), json!({ "type": "string", "description": "子串搜索（key / 标签 / ?search 字段 / 正文）" }));
            list_props.insert(
                "archived".into(),
                json!({ "type": "boolean", "description": "连归档的一起列" }),
            );
            list_props.insert(
                "limit".into(),
                json!({ "type": "integer", "description": "最多几条（默认 20）" }),
            );
            out.push(json!({
                "name": format!("ncc_store_list_{kind}"),
                "description": format!(
                    "列「{kind}」（这样内容在这个节点上的名字）。字段：{brief} —— {shape} · {vis}。\n只列**声明过的**过滤字段：没在集合声明里 `index` 的字段这里不能滤（那是全表扫，服务端会拒）。"
                ),
                "inputSchema": { "type": "object", "properties": list_props, "additionalProperties": false }
            }));
            out.push(json!({
                "name": format!("ncc_store_get_{kind}"),
                "description": format!("读「{kind}」的一条记录（含正文）。字段：{brief}"),
                "inputSchema": {
                    "type": "object",
                    "properties": { "key": { "type": "string", "description": "记录的 key" } },
                    "required": ["key"],
                    "additionalProperties": false
                }
            }));
        }
        if writes {
            let mut put_props = serde_json::Map::new();
            put_props.insert("key".into(), json!({ "type": "string", "description": "记录的 key（小写字母数字与 -_.，字母开头）" }));
            put_props.insert(
                "body".into(),
                json!({ "type": "string", "description": "这份内容的正文" }),
            );
            // 声明字段放在 `fields` 里（不是平铺）：字段类型与词表都从声明来，
            // 而且集合完全可以声明一个叫 `body` 的字段 —— 平铺就分不清是哪个了。
            put_props.insert(
                "fields".into(),
                json!({
                    "type": "object",
                    "description": format!("按声明的字段填：{brief}"),
                    "properties": Value::Object(props.clone()),
                    "required": required.clone(),
                    "additionalProperties": false
                }),
            );
            put_props.insert(
                "tags".into(),
                json!({ "type": "array", "items": { "type": "string" } }),
            );
            put_props.insert("note".into(), json!({ "type": "string", "description": "为什么要写这一次 —— 会进历史，两个月后有人会看" }));
            out.push(json!({
                "name": format!("ncc_store_put_{kind}"),
                "description": format!(
                    "写「{kind}」：**这个包声明了写**，所以模型可以写。字段：{brief} —— {shape} · {vis}。{}同 key 再写就是改一条{}。",
                    if col["mutable"].as_bool().unwrap_or(false) { "" } else { "这个集合不可变：同 key 只能写一次。" },
                    "（每次都会记下是谁改的、什么时候、改成了哪个摘要，以及你给的 note）"
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": put_props,
                    "required": ["key"],
                    "additionalProperties": false
                }
            }));
        }
    }
    out
}
/* ---------------- 工具实现 ---------------- */

fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": false })
}

fn tool_err(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": true })
}

/// 带「模型面」的工具分发：通用记录仓的工具在这里先过一次**范围门禁**。
///
/// 为什么门禁要真的拦而不只是不广告：`tools/list` 看不到 ≠ 调不动 ——
/// 一个跑偏的客户端（或被人手工构造的请求）照样能发 `tools/call`。
/// 声明面是给模型划的边界，所以边界得在**执行处**判。
fn call_tool_scoped(
    cfg: &CliConfig,
    scope: Option<&StoreScope>,
    params: Option<&Value>,
) -> Result<Value> {
    let p = params.cloned().unwrap_or(json!({}));
    let name = p
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = p.get("arguments").cloned().unwrap_or(json!({}));
    if is_store_tool(&name) {
        // 能力门禁（节点没有 store 能力时明确报错，而不是丢 404 给 Agent）
        if let Err(e) = capability::ensure(cfg, "store") {
            return Ok(tool_err(format!("{e:#}")));
        }
        return Ok(match store_call(cfg, scope, &name, &args) {
            Ok(v) => v,
            Err(e) => tool_err(format!("{e:#}")),
        });
    }
    call_tool(cfg, params)
}

/// 是不是通用记录仓的工具。
fn is_store_tool(name: &str) -> bool {
    matches!(
        name,
        "ncc_list_store_collections" | "ncc_list_store_records" | "ncc_get_store_record"
    ) || name.starts_with("ncc_store_")
}

/// 记录仓工具的实现（**范围门禁在这里**）。
fn store_call(
    cfg: &CliConfig,
    scope: Option<&StoreScope>,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let sarg = |k: &str| -> Option<String> {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let token = config::token_opt(cfg);
    let ns_qs = |ns: Option<String>| -> String {
        match ns {
            Some(n) if !n.is_empty() => {
                format!("?namespace={}", api::urlenc(n.trim_start_matches('@')))
            }
            _ => String::new(),
        }
    };

    // `--package` 收窄了模型面：只认「声明过的集合」，且动作要跟 `mode` 对得上。
    if let Some(sc) = scope {
        let avail = || {
            let items: Vec<String> = sc
                .items
                .iter()
                .map(|(r, _)| format!("{}（{}）", r.collection.trim(), r.mode_norm()))
                .collect();
            if items.is_empty() {
                "（空）".to_string()
            } else {
                items.join(" · ")
            }
        };
        let rest = name.strip_prefix("ncc_store_").ok_or_else(|| {
            anyhow::anyhow!(
                "这个模型面被收窄到包 {} 声明的集合，没有工具「{name}」。声明的集合：{}",
                sc.pkg,
                avail()
            )
        })?;
        let (op, col) = rest.split_once('_').ok_or_else(|| {
            anyhow::anyhow!("工具名「{name}」不合法（应为 ncc_store_<list|get|put>_<集合>）")
        })?;
        let Some(req) = sc
            .items
            .iter()
            .find(|(r, _)| r.collection.trim() == col)
            .map(|(r, _)| r)
        else {
            // 分开说："没声明"与"声明了但节点上没有"是两个不同的问题。
            if sc.missing.iter().any(|m| m == col) {
                return Err(anyhow::anyhow!(
                    "包 {} 声明了「{col}」，但这台节点上没有这个集合 —— 模型面不给它。\
                     先在这台节点上 `ncc store declare {col}`（或用另一台节点），再重起 `ncc mcp`",
                    sc.pkg
                ));
            }
            return Err(anyhow::anyhow!(
                "包 {} 没有声明集合「{col}」—— 模型只能看到声明过的（声明的集合：{}）。\
                 要给它就加进 hur.json 的 state.stores[] 再重起 `ncc mcp --package`",
                sc.pkg,
                avail()
            ));
        };
        let reads = matches!(req.mode_norm(), "read" | "readwrite");
        let writes = matches!(req.mode_norm(), "write" | "readwrite");
        // 声明成「只写」的集合没有读口子：写进去的东西**不回头读**（快照/上报类内容的口径）。
        if matches!(op, "list" | "get") && !reads {
            return Err(anyhow::anyhow!(
                "包 {} 把「{col}」声明成只写 —— 模型拿不到它的读口子（要读就改成 readwrite）",
                sc.pkg
            ));
        }
        if op == "put" && !writes {
            return Err(anyhow::anyhow!(
                "包 {} 只声明了**读**「{col}」—— 模型不写它。要写就在 hur.json 的 \
                 state.stores[] 里把它改成 readwrite 或 write（**改声明是有记录的动作**，\
                 别在工具里偷偷写）",
                sc.pkg
            ));
        }
        return match op {
            "list" => store_list(cfg, token.as_deref(), col, args),
            "get" => {
                let key = sarg("key").ok_or_else(|| anyhow::anyhow!("需要 key"))?;
                store_get(cfg, token.as_deref(), col, &key)
            }
            "put" => store_put(cfg, token.as_deref(), col, args),
            other => Err(anyhow::anyhow!("不认的工具动作「{other}」")),
        };
    }

    // 没给包：只读浏览。
    match name {
        "ncc_list_store_collections" => {
            let d = api::get(
                cfg,
                &format!("/api/store{}", ns_qs(sarg("namespace"))),
                token.as_deref(),
            )?;
            Ok(text(clip(&render_collections(&d))))
        }
        "ncc_list_store_records" => {
            let col = sarg("collection").ok_or_else(|| anyhow::anyhow!("需要 collection"))?;
            store_list(cfg, token.as_deref(), &col, args)
        }
        "ncc_get_store_record" => {
            let col = sarg("collection").ok_or_else(|| anyhow::anyhow!("需要 collection"))?;
            let key = sarg("key").ok_or_else(|| anyhow::anyhow!("需要 key"))?;
            store_get(cfg, token.as_deref(), col.as_str(), &key)
        }
        other => Err(anyhow::anyhow!("「{other}」不是浏览用的工具")),
    }
}

/// 列集合（人/模型读的紧凑形态）。
fn render_collections(d: &Value) -> String {
    let cols = d["collections"].as_array().cloned().unwrap_or_default();
    if cols.is_empty() {
        return "这台节点上还没有集合（先用 `ncc store declare` 声明一个）".to_string();
    }
    let mut out = format!("这台节点上有 {} 个集合：\n", cols.len());
    for c in &cols {
        out.push_str(&format!(
            "- {}（{} 条 · {} · {}）：{}\n",
            c["kind"].as_str().unwrap_or(""),
            c["records"].as_i64().unwrap_or(0),
            if c["mutable"].as_bool().unwrap_or(false) {
                "可改"
            } else {
                "不可变"
            },
            c["visibility"].as_str().unwrap_or("private"),
            c["summary"].as_str().unwrap_or("")
        ));
        out.push_str(&format!("    字段：{}\n", fields_brief(c)));
    }
    out
}

/// 列记录（`filter` 是嵌套对象，避免与保留参数撞车）。
fn store_list(cfg: &CliConfig, token: Option<&str>, col: &str, args: &Value) -> Result<Value> {
    let mut qs: Vec<String> = Vec::new();
    if let Some(ns) = args.get("namespace").and_then(|v| v.as_str()) {
        qs.push(format!(
            "namespace={}",
            api::urlenc(ns.trim_start_matches('@'))
        ));
    }
    if let Some(q) = args
        .get("q")
        .and_then(|v| v.as_str())
        .filter(|q| !q.trim().is_empty())
    {
        qs.push(format!("q={}", api::urlenc(q.trim())));
    }
    if let Some(f) = args.get("filter").and_then(|v| v.as_object()) {
        for (k, v) in f {
            let val = v
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string());
            if k.trim().is_empty() || val.trim().is_empty() {
                continue;
            }
            // 线上是 `f.<字段>=值`：加前缀才不会跟保留参数撞车（状态用 ?state=）。
            qs.push(format!(
                "f.{}={}",
                api::urlenc(k.trim()),
                api::urlenc(val.trim())
            ));
        }
    }
    if args
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        qs.push("archived=1".into());
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20)
        .clamp(1, 200);
    qs.push(format!("size={limit}"));
    let d = api::get(
        cfg,
        &format!("/api/store/{}?{}", api::urlenc(col), qs.join("&")),
        token,
    )?;
    let rows = d["records"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "{} 共 {} 条（列出 {} 条）：\n",
        d["collection"].as_str().unwrap_or(col),
        d["total"].as_i64().unwrap_or(0),
        rows.len()
    );
    for r in &rows {
        out.push_str(&format!(
            "- {}（rev {} · {} 字节 · 更新于 {}）\n",
            r["key"].as_str().unwrap_or(""),
            r["revision"].as_i64().unwrap_or(0),
            r["size"].as_i64().unwrap_or(0),
            r["updatedAt"].as_str().unwrap_or("")
        ));
        if let Some(f) = r["fields"].as_object().filter(|m| !m.is_empty()) {
            let pairs: Vec<String> = f
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string())
                    )
                })
                .collect();
            out.push_str(&format!("    {}\n", pairs.join(" · ")));
        }
        if !r["tags"].as_array().map(|t| t.is_empty()).unwrap_or(true) {
            out.push_str(&format!("    标签：{}\n", r["tags"]));
        }
    }
    if rows.is_empty() {
        out.push_str("（没有匹配的记录。注意：归档与过期的默认不列出；`filter` 只能用在声明过的可过滤字段上）\n");
    }
    Ok(text(clip(&out)))
}

/// 读一条（连正文）。
fn store_get(cfg: &CliConfig, token: Option<&str>, col: &str, key: &str) -> Result<Value> {
    let d = api::get(
        cfg,
        &format!("/api/store/{}/{}", api::urlenc(col), api::urlenc(key)),
        token,
    )?;
    let r = &d["record"];
    let mut out = format!(
        "{}/{}  rev {} · {} 字节 · checksum {}\n",
        d["collection"].as_str().unwrap_or(col),
        r["key"].as_str().unwrap_or(key),
        r["revision"].as_i64().unwrap_or(0),
        r["size"].as_i64().unwrap_or(0),
        r["checksum"].as_str().unwrap_or("")
    );
    if let Some(f) = r["fields"].as_object().filter(|m| !m.is_empty()) {
        for (k, v) in f {
            out.push_str(&format!(
                "  {k} = {}\n",
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            ));
        }
    }
    if r["expired"].as_bool().unwrap_or(false) {
        out.push_str("  ⚠️ 已过期（按\"过期即不存在\"的口径，它其实读不到了）\n");
    }
    out.push_str("\n--- 正文 ---\n");
    out.push_str(r["body"].as_str().unwrap_or(""));
    Ok(text(clip(&out)))
}

/// 写一条（**只有包声明了写才可能走到这里**；模型面默认不给写）。
fn store_put(cfg: &CliConfig, token: Option<&str>, col: &str, args: &Value) -> Result<Value> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("需要 key"))?;
    let mut req = json!({
        "key": key,
        "body": args.get("body").and_then(|v| v.as_str()).unwrap_or(""),
        "fields": args.get("fields").cloned().unwrap_or(json!({})),
    });
    if let Some(t) = args.get("tags").filter(|t| !t.is_null()) {
        req["tags"] = t.clone();
    }
    if let Some(n) = args
        .get("note")
        .and_then(|v| v.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        req["note"] = json!(n.trim());
    }
    let mut path = format!("/api/store/{}", api::urlenc(col));
    if let Some(ns) = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        path.push_str(&format!(
            "?namespace={}",
            api::urlenc(ns.trim_start_matches('@'))
        ));
    }
    let token = token.ok_or_else(|| anyhow::anyhow!("写记录需要凭据（先 ncc login）"))?;
    let d = api::post_json(cfg, &path, Some(token), &req)?;
    let r = &d["record"];
    Ok(text(format!(
        "{} {}/{} · rev {}{}",
        if d["created"].as_bool().unwrap_or(false) {
            "✅ 已建"
        } else {
            "✅ 已更新"
        },
        d["collection"].as_str().unwrap_or(col),
        r["key"].as_str().unwrap_or(&key),
        r["revision"].as_i64().unwrap_or(0),
        if d["duplicate"].as_bool().unwrap_or(false) {
            "（内容没变，没刷版本）"
        } else {
            ""
        }
    )))
}

fn call_tool(cfg: &CliConfig, params: Option<&Value>) -> Result<Value> {
    let p = params.cloned().unwrap_or(json!({}));
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = p.get("arguments").cloned().unwrap_or(json!({}));
    // 能力门禁：这个目标是哪台 ncc、它声明了什么。
    // 例：把 ncc_match_services 打到内网 registry 节点上，应该告知「该工具属于 services 能力」，
    // 而不是丢一个 404 给 Agent。未声明能力（老服务端）则放行。
    if let Some(cap) = tool_capability(name) {
        if let Err(e) = capability::ensure(cfg, cap) {
            return Ok(tool_err(format!("{e:#}")));
        }
    }
    let sarg = |k: &str| -> Option<String> {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    // 引用参数的新旧名：`ref` 是正名（“目标”现在指连接目标，不能再混用），
    // `target` 保留为兼容名（老 prompt / 老配置里写的还能用）。
    let rarg = |k: &str| -> Option<String> { sarg(k).or_else(|| sarg("target")) };
    let narg = |k: &str, d: i64, max: i64| -> i64 {
        args.get(k)
            .and_then(|v| v.as_i64())
            .unwrap_or(d)
            .clamp(1, max)
    };
    // 布尔参数（缺省 false）。服务端的 `?following=1` 只认字面 "1"，由调用处拼。
    let barg = |k: &str| -> bool { args.get(k).and_then(|v| v.as_bool()).unwrap_or(false) };
    let token = config::token_opt(cfg);

    // 工具级失败按 MCP 约定回 isError=true，而不是 JSON-RPC error
    let ok_or_text = |r: Result<Value>| match r {
        Ok(v) => v,
        Err(e) => tool_err(format!("调用失败：{e:#}")),
    };

    Ok(match name {
        "ncc_list_kinds" => ok_or_text((|| {
            let d = api::get(cfg, "/api/registry/kinds", None)?;
            let mut out = String::from("NCC 目录中的能力类型：\n");
            if let Some(ks) = d.get("kinds").and_then(|v| v.as_array()) {
                for k in ks {
                    out.push_str(&format!(
                        "- {}（{}）: {} 个已发布 — {}\n",
                        k.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                        k.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                        k.get("published").and_then(|v| v.as_i64()).unwrap_or(0),
                        k.get("desc").and_then(|v| v.as_str()).unwrap_or(""),
                    ));
                }
            }
            Ok(text(out))
        })()),

        "ncc_search_catalog" => ok_or_text((|| {
            let mut qs: Vec<String> = Vec::new();
            if let Some(q) = sarg("query") {
                qs.push(format!("q={}", urlenc(&q)));
            }
            if let Some(k) = sarg("kind") {
                qs.push(format!("kind={}", urlenc(&k)));
            }
            if let Some(t) = sarg("tag") {
                qs.push(format!("tag={}", urlenc(&t)));
            }
            if let Some(n) = sarg("namespace") {
                qs.push(format!("namespace={}", urlenc(&n)));
            }
            qs.push(format!("size={}", narg("limit", 20, 50)));
            let d = api::get(
                cfg,
                &format!("/api/registry?{}", qs.join("&")),
                token.as_deref(),
            )?;
            let total = d.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            let items = d
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                return Ok(text(format!("没有匹配的条目（共 {total} 条）。换个关键词，或先用 ncc_list_kinds 看看目录构成。")));
            }
            let mut out = format!("共 {total} 条，返回 {} 条：\n", items.len());
            for it in &items {
                let ns = it
                    .pointer("/namespace/slug")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let slug = it.get("slug").and_then(|v| v.as_str()).unwrap_or("");
                let tags = it
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- [{}] {}/{slug}@{} — {}{}{}\n",
                    it.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                    ns,
                    it.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                    it.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
                    if tags.is_empty() {
                        String::new()
                    } else {
                        format!("  #{tags}")
                    },
                    format!(
                        "  (⬇{})",
                        it.get("downloads").and_then(|v| v.as_i64()).unwrap_or(0)
                    ),
                ));
            }
            Ok(text(out))
        })()),

        "ncc_get_artifact" => ok_or_text((|| {
            let target = rarg("ref").context("缺少 ref")?;
            let d = api::get(
                cfg,
                &format!("/api/registry/{}", urlenc(&target)),
                token.as_deref(),
            )?;
            let it = d.get("item").cloned().unwrap_or(d);
            Ok(text(clip(&serde_json::to_string_pretty(&it)?)))
        })()),

        "ncc_fetch_artifact" => ok_or_text((|| {
            let target = rarg("ref").context("缺少 ref")?;
            let dl = api::get(
                cfg,
                &format!("/api/registry/{}/download", urlenc(&target)),
                token.as_deref(),
            )?;
            let url = dl
                .get("url")
                .and_then(|v| v.as_str())
                .context("下载响应缺少 url")?;
            let ns = dl
                .get("namespaceSlug")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let slug = dl.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            let ver = dl.get("version").and_then(|v| v.as_str()).unwrap_or("");
            let sha = dl.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
            let head = format!("{ns}/{slug}@{ver}\nurl: {url}\nsha256: {sha}\n\n");

            let agent = ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(60))
                .build();
            let resp = agent
                .get(url)
                .call()
                .map_err(|e| anyhow::anyhow!("拉取正文失败: {e}"))?;
            let mut buf: Vec<u8> = Vec::new();
            std::io::copy(&mut resp.into_reader(), &mut buf)?;

            match String::from_utf8(buf) {
                Ok(s) if is_texty(url) => Ok(text(format!("{head}{}", clip(&s)))),
                Ok(s) => Ok(text(format!(
                    "{head}（非文本类制品，正文 {} 字节，已省略）",
                    s.len()
                ))),
                Err(e) => {
                    let n = e.as_bytes().len();
                    Ok(text(format!(
                        "{head}（二进制制品，{n} 字节；请用上面的 url 直接下载）"
                    )))
                }
            }
        })()),

        "ncc_publish_artifact" => ok_or_text((|| {
            let token = token
                .clone()
                .context("发布需要登录：先运行 `ncc login`，或配置 API-Key")?;
            let kind = sarg("kind").context("缺少 kind")?;
            let name = sarg("name").context("缺少 name")?;

            let mut storage_url = sarg("url").unwrap_or_default();
            let mut sha = String::new();
            let mut size: i64 = 0;

            if let Some(content) = args.get("content").and_then(|v| v.as_str()) {
                let fname = sarg("filename").unwrap_or_else(|| format!("{}.txt", kind));
                let up = api::request(
                    cfg,
                    "POST",
                    "/api/registry/uploads",
                    Some(&token),
                    None,
                    Some(content.as_bytes()),
                    &[("X-Filename", fname.as_str())],
                )?;
                storage_url = up
                    .get("storageUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                sha = up
                    .get("sha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                size = up.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
            } else if let Some(path) = sarg("contentFile") {
                let bytes =
                    std::fs::read(&path).with_context(|| format!("读取文件失败: {path}"))?;
                let fname = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("upload.bin")
                    .to_string();
                let up = api::request(
                    cfg,
                    "POST",
                    "/api/registry/uploads",
                    Some(&token),
                    None,
                    Some(&bytes),
                    &[("X-Filename", fname.as_str())],
                )?;
                storage_url = up
                    .get("storageUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                sha = up
                    .get("sha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                size = up.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
            }

            if storage_url.is_empty() {
                anyhow::bail!("需要 content / contentFile / url 之一作为制品字节来源");
            }

            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let mut body = json!({
                "kind": kind, "name": name,
                "slug": sarg("slug"),
                "version": sarg("version").unwrap_or_else(|| "1.0.0".to_string()),
                "summary": sarg("summary").unwrap_or_default(),
                "tags": tags,
                "status": if args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) { "draft" } else { "published" },
                "visibility": sarg("visibility").unwrap_or_else(|| "public".to_string()),
                "storage": {
                    "url": storage_url,
                    "sha256": if sha.is_empty() { Value::Null } else { json!(sha) },
                    "size": if size == 0 { Value::Null } else { json!(size) },
                },
            });
            if let Some(m) = args.get("manifest") {
                if !m.is_null() {
                    body["manifest"] = m.clone();
                }
            }

            let d = api::post_json(cfg, "/api/registry", Some(&token), &body)?;
            let it = d.get("item").cloned().unwrap_or(Value::Null);
            Ok(text(format!(
                "✅ 已发布 [{}] {}/{}@{}  status={}\nid: {}\n存储: {}",
                it.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                it.pointer("/namespace/slug")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                it.get("slug").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                it.pointer("/storage/url")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )))
        })()),

        "ncc_whoami" => ok_or_text((|| {
            let token = token.clone().context("未登录：先运行 `ncc login`")?;
            let d = api::get(cfg, "/api/auth/me", Some(&token))?;
            let u = d.get("user").cloned().unwrap_or(Value::Null);
            let mut out = format!(
                "用户 {} <{}>  计划 {}\n",
                u.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                u.get("email").and_then(|v| v.as_str()).unwrap_or(""),
                u.get("plan").and_then(|v| v.as_str()).unwrap_or("free"),
            );
            out.push_str("命名空间：\n");
            if let Some(ns) = d.get("namespaces").and_then(|v| v.as_array()) {
                for n in ns {
                    out.push_str(&format!(
                        "- {} （{}，{}）\n",
                        n.get("slug").and_then(|v| v.as_str()).unwrap_or(""),
                        n.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                        if n.get("owner").and_then(|v| v.as_bool()).unwrap_or(false) {
                            "owner"
                        } else {
                            "member"
                        },
                    ));
                }
            }
            Ok(text(out))
        })()),

        "ncc_list_roles" => ok_or_text((|| {
            let d = api::get(cfg, "/api/profile/roles", None)?;
            let groups = d
                .get("groups")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let all = d
                .get("roles")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut out = String::from("NCC Profile 工作角色（id — 名称 — 说明）：\n");
            for g in &groups {
                let gid = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!(
                    "\n【{}】{}\n",
                    g.get("zh").and_then(|v| v.as_str()).unwrap_or(gid),
                    gid
                ));
                for r in all
                    .iter()
                    .filter(|r| r.get("group").and_then(|v| v.as_str()) == Some(gid))
                {
                    out.push_str(&format!(
                        "- {} — {}：{}\n",
                        r.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        r.get("zh").and_then(|v| v.as_str()).unwrap_or(""),
                        r.get("descZh").and_then(|v| v.as_str()).unwrap_or(""),
                    ));
                }
            }
            Ok(text(out))
        })()),

        "ncc_find_people" => ok_or_text((|| {
            let mut qs: Vec<String> = Vec::new();
            if let Some(r) = sarg("role") {
                qs.push(format!("role={}", urlenc(&r)));
            }
            if let Some(s) = sarg("skill") {
                qs.push(format!("skill={}", urlenc(&s)));
            }
            if let Some(q) = sarg("query") {
                qs.push(format!("q={}", urlenc(&q)));
            }
            if let Some(a) = sarg("availability") {
                qs.push(format!("availability={}", urlenc(&a)));
            }
            // 服务端只认字面 "1"（不是 true/yes），且未登录直接 401
            if barg("following") {
                qs.push("following=1".to_string());
            }
            qs.push(format!("size={}", narg("limit", 20, 50)));
            let d = api::get(
                cfg,
                &format!("/api/profiles?{}", qs.join("&")),
                token.as_deref(),
            )?;
            let total = d.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            let ps = d
                .get("profiles")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if ps.is_empty() {
                // following 的空结果多半不是「没人」，而是「你没关注 / 对方没进目录」——
                // 这两种情况给同一句「没有匹配」会让人白找一轮角色 id
                return Ok(text(if barg("following") {
                    format!("没有匹配的名片（共 {total} 位）。following 只看你关注的人，且对方得是公开名片 —— 用 `ncc profile following` 确认关注列表。")
                } else {
                    format!("没有匹配的名片（共 {total} 位）。可用 ncc_list_roles 确认角色 id。")
                }));
            }
            let mut out = format!("共 {total} 位，返回 {} 位：\n", ps.len());
            for p in &ps {
                let roles = p
                    .get("roles")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|r| r.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let skills = p
                    .get("skills")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|r| r.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- {}（{}）{} — 角色 {}｜技能 {}｜作品 {}｜{}\n",
                    p.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("handle").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("headline").and_then(|v| v.as_str()).unwrap_or(""),
                    roles,
                    skills,
                    p.get("works").and_then(|v| v.as_i64()).unwrap_or(0),
                    p.get("availability").and_then(|v| v.as_str()).unwrap_or(""),
                ));
            }
            Ok(text(out))
        })()),

        "ncc_get_profile" => ok_or_text((|| {
            let path = match sarg("username") {
                Some(u) => format!("/api/profiles/{}", urlenc(u.trim_start_matches('@'))),
                None => "/api/profile/me".to_string(),
            };
            let d = api::get(cfg, &path, token.as_deref())?;
            let p = d.get("profile").cloned().unwrap_or(Value::Null);
            if p.is_null() {
                return Ok(text(
                    "还没有名片。可在 ncc.ai 上创建，或用 `ncc profile set` 命令行创建。"
                        .to_string(),
                ));
            }
            let mut out = format!(
                "{}（{}）\n{}\n",
                p.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                p.get("handle").and_then(|v| v.as_str()).unwrap_or(""),
                p.get("headline").and_then(|v| v.as_str()).unwrap_or(""),
            );
            let list = |v: Option<&Value>| {
                v.and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            };
            let roles = list(p.get("roles"));
            if !roles.is_empty() {
                out.push_str(&format!("定位角色: {roles}\n"));
            }
            let skills = list(p.get("skills"));
            if !skills.is_empty() {
                out.push_str(&format!("技能: {skills}\n"));
            }
            if let Some(bio) = p.get("bio").and_then(|v| v.as_str()) {
                if !bio.is_empty() {
                    out.push_str(&format!("简介: {bio}\n"));
                }
            }
            if let Some(ws) = d.get("works").and_then(|v| v.as_array()) {
                if !ws.is_empty() {
                    out.push_str("\n作品集:\n");
                    for w in ws {
                        out.push_str(&format!(
                            "- {}{} → {}\n",
                            w.get("year")
                                .and_then(|v| v.as_str())
                                .map(|y| format!("[{y}] "))
                                .unwrap_or_default(),
                            w.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                            w.get("href").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                }
            }
            if let Some(is) = d.get("items").and_then(|v| v.as_array()) {
                if !is.is_empty() {
                    out.push_str("\n已发布能力:\n");
                    for i in is {
                        out.push_str(&format!(
                            "- [{}] {}/{}@{}\n",
                            i.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                            i.pointer("/namespace/slug")
                                .and_then(|v| v.as_str())
                                .unwrap_or(""),
                            i.get("slug").and_then(|v| v.as_str()).unwrap_or(""),
                            i.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                }
            }
            Ok(text(out))
        })()),

        // —— 节点层（只读）：我的节点 / 可连接节点 / 区域聚合 / 推荐 / 授权 ——
        // 只做读。写入类操作（连接节点、授权）一律让用户用 CLI 显式执行：
        // 这些动作会改变「谁能拿到我的东西」或「我的 Agent 会连谁」，不能让 Agent 自作主张。
        "ncc_list_nodes" => ok_or_text((|| {
            let kind = sarg("kind").unwrap_or_default();
            let q = sarg("q").unwrap_or_default();
            let can = sarg("can").unwrap_or_default();
            let v = if kind.is_empty() && q.is_empty() && can.is_empty() {
                crate::nodes::fetch_nodes(cfg)?
            } else {
                let mut qs: Vec<String> = Vec::new();
                if !kind.is_empty() {
                    qs.push(format!("kind={}", urlenc(&kind)));
                }
                if !q.is_empty() {
                    qs.push(format!("q={}", urlenc(&q)));
                }
                if !can.is_empty() {
                    qs.push(format!("can={}", urlenc(&can)));
                }
                let path = format!("/api/nodes?{}", qs.join("&"));
                api::get(cfg, &path, config::token_opt(cfg).as_deref())?
            };
            Ok(text(clip(&crate::nodes::render_nodes(&v))))
        })()),

        "ncc_discover_nodes" => ok_or_text((|| {
            let can = sarg("can").unwrap_or_default();
            let v = crate::nodes::fetch_discover(
                cfg,
                sarg("kind").unwrap_or_default().as_str(),
                &can,
                narg("limit", 30, 60) as u32,
            )?;
            let region = sarg("region").unwrap_or_default();
            if region.is_empty() {
                Ok(text(clip(&crate::nodes::render_discover(&v))))
            } else {
                // 区域过滤时改走 recommend（服务端在那边才按区域筛 + 排序）
                let r = crate::nodes::fetch_recommend(
                    cfg,
                    &region,
                    sarg("kind").unwrap_or_default().as_str(),
                    &can,
                    narg("limit", 30, 60) as u32,
                )?;
                Ok(text(clip(&crate::nodes::render_recommend(&r))))
            }
        })()),

        "ncc_region_profile" => ok_or_text((|| {
            Ok(text(clip(&crate::nodes::render_region_profile(
                &crate::nodes::fetch_region_profile(cfg)?,
            ))))
        })()),

        "ncc_recommend_nodes" => ok_or_text((|| {
            let v = crate::nodes::fetch_recommend(
                cfg,
                sarg("region").unwrap_or_default().as_str(),
                sarg("kind").unwrap_or_default().as_str(),
                &sarg("can").unwrap_or_default(),
                narg("limit", 20, 50) as u32,
            )?;
            Ok(text(clip(&crate::nodes::render_recommend(&v))))
        })()),

        "ncc_list_grants" => ok_or_text((|| {
            let dir = sarg("direction").unwrap_or_else(|| "outgoing".to_string());
            let v = crate::nodes::fetch_grants(cfg, &dir)?;
            Ok(text(clip(&crate::nodes::render_grants(&v, &dir))))
        })()),

        "ncc_list_traces" => ok_or_text((|| {
            let q = [
                ("ref", sarg("ref")),
                ("kind", sarg("kind")),
                ("status", sarg("status")),
                ("agent", sarg("agent")),
                ("tag", sarg("tag")),
                ("since", sarg("since")),
                ("size", Some(narg("limit", 20, 200).to_string())),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|x| format!("{k}={}", api::urlenc(x))))
            .collect::<Vec<_>>()
            .join("&");
            let path = if q.is_empty() {
                "/api/traces".to_string()
            } else {
                format!("/api/traces?{q}")
            };
            let d = api::get(cfg, &path, token.as_deref())?;
            let rows = d["traces"].as_array().cloned().unwrap_or_default();
            let mut out = format!(
                "运行轨迹 {} 条（可见范围 {}）：\n",
                d["total"].as_i64().unwrap_or(0),
                d["scope"].as_str().unwrap_or("-")
            );
            if rows.is_empty() {
                out.push_str("（没有匹配的轨迹。轨迹默认私有：要么这些轨迹不属于你，要么还没 push 上来。）\n");
            }
            for r in rows {
                let ev = r.get("evaluation").cloned().unwrap_or(json!({}));
                let marks = [
                    ev.get("grade")
                        .and_then(|v| v.as_str())
                        .map(|g| format!("结论 {g}")),
                    ev.get("scoreMilli")
                        .and_then(|v| v.as_i64())
                        .map(|s| format!("得分 {:.3}", s as f64 / 1000.0)),
                    ev.get("split")
                        .and_then(|v| v.as_str())
                        .map(|s| format!("切分 {s}")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                out.push_str(&format!(
                    "- {} 〔{} · {}〕 ref={} {} ms · {} 步 · payload={} · {}{}\n",
                    r["id"].as_str().unwrap_or("-"),
                    r["kind"].as_str().unwrap_or("-"),
                    r["status"].as_str().unwrap_or("-"),
                    r.pointer("/subject/ref")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-"),
                    r["durationMs"].as_i64().unwrap_or(0),
                    r["steps"].as_i64().unwrap_or(0),
                    r["payload"].as_str().unwrap_or("-"),
                    r["at"].as_str().unwrap_or(""),
                    if marks.is_empty() {
                        String::new()
                    } else {
                        format!("  [{marks}]")
                    },
                ));
            }
            Ok(text(clip(&out)))
        })()),

        "ncc_trace_stats" => ok_or_text((|| {
            let q = [
                ("ref", sarg("ref")),
                ("kind", sarg("kind")),
                ("since", sarg("since")),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|x| format!("{k}={}", api::urlenc(x))))
            .collect::<Vec<_>>()
            .join("&");
            let path = if q.is_empty() {
                "/api/traces/stats".to_string()
            } else {
                format!("/api/traces/stats?{q}")
            };
            let d = api::get(cfg, &path, token.as_deref())?;
            Ok(text(clip(&format!(
                "运行轨迹聚合（取样 {} 条{}）：\n{}",
                d["sampled"].as_i64().unwrap_or(0),
                if d["truncated"].as_bool().unwrap_or(false) {
                    "，被截断"
                } else {
                    ""
                },
                serde_json::to_string_pretty(&d["stats"]).unwrap_or_default()
            ))))
        })()),

        // ---- 三样状态（知识库 / 记忆 / 检查点）：只读 ----
        "ncc_list_kb" => ok_or_text((|| {
            let q = [
                ("namespace", sarg("namespace")),
                ("kind", sarg("kind")),
                ("tag", sarg("tag")),
                ("q", sarg("query")),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|x| format!("{k}={}", api::urlenc(x))))
            .collect::<Vec<_>>();
            let mut qs = q;
            qs.push(format!("size={}", narg("limit", 20, 200)));
            let d = api::get(cfg, &format!("/api/kb?{}", qs.join("&")), token.as_deref())?;
            let mut out = String::new();
            let docs = d["docs"].as_array().cloned().unwrap_or_default();
            if docs.is_empty() {
                out.push_str("（没有匹配的文档）\n");
            }
            for x in &docs {
                out.push_str(&format!(
                    "- {} v{} 〔{}〕 {} —— {}\n",
                    x["ref"].as_str().unwrap_or("-"),
                    x["revision"].as_i64().unwrap_or(0),
                    x["kind"].as_str().unwrap_or("-"),
                    x["title"].as_str().unwrap_or("-"),
                    x["summary"].as_str().unwrap_or("")
                ));
            }
            out.push_str(&format!(
                "\n共 {} 条（范围 {}）· 取正文用 ncc_get_kb\n⚠ 关键词检索，不是向量检索。",
                d["total"].as_i64().unwrap_or(0),
                d["scope"].as_str().unwrap_or("-")
            ));
            Ok(text(clip(&out)))
        })()),

        "ncc_get_kb" => ok_or_text((|| {
            let reference = rarg("ref").unwrap_or_default();
            if reference.is_empty() {
                return Err(anyhow::anyhow!("缺 ref（@命名空间/slug 或 KD-…）"));
            }
            let mut path = format!("/api/kb/{}", reference.trim());
            if let Some(r) = args.get("revision").and_then(|v| v.as_i64()) {
                path.push_str(&format!("?revision={r}"));
            }
            let d = api::get(cfg, &path, token.as_deref())?;
            Ok(text(clip(&format!(
                "{}  {}\n类型 {} · 格式 {} · 版本 v{} · 校验 {}\n摘要 {}\n\n{}",
                d["ref"].as_str().unwrap_or("-"),
                d["title"].as_str().unwrap_or("-"),
                d["kind"].as_str().unwrap_or("-"),
                d["format"].as_str().unwrap_or("-"),
                d["revision"].as_i64().unwrap_or(0),
                d["checksum"].as_str().unwrap_or("-"),
                d["summary"].as_str().unwrap_or("-"),
                d["content"].as_str().unwrap_or("")
            ))))
        })()),

        "ncc_list_mem" => ok_or_text((|| {
            let mut qs = [
                ("namespace", sarg("namespace")),
                ("subject", sarg("subject")),
                ("prefix", sarg("prefix")),
                ("kind", sarg("kind")),
                ("source", sarg("source")),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|x| format!("{k}={}", api::urlenc(x))))
            .collect::<Vec<_>>();
            qs.push(format!("limit={}", narg("limit", 50, 1000)));
            let d = api::get(cfg, &format!("/api/mem?{}", qs.join("&")), token.as_deref())?;
            let mut out = String::new();
            for m in d["memories"].as_array().cloned().unwrap_or_default() {
                out.push_str(&format!(
                    "- [{}] {} / {} = {}{}{}\n",
                    m["kind"].as_str().unwrap_or("-"),
                    m["subject"].as_str().unwrap_or("-"),
                    m["key"].as_str().unwrap_or("-"),
                    m["value"].as_str().unwrap_or(""),
                    if m["pinned"].as_bool().unwrap_or(false) {
                        "  📌"
                    } else {
                        ""
                    },
                    match m["expiresAt"].as_str() {
                        Some(t) => format!("  （{t} 过期）"),
                        None => String::new(),
                    }
                ));
            }
            if out.is_empty() {
                out.push_str("（没有记忆）\n");
            }
            out.push_str(&format!(
                "\n共 {} 条（范围 {}）· 记忆默认私有；写入要用户跑 `ncc mem set`。",
                d["total"].as_i64().unwrap_or(0),
                d["scope"].as_str().unwrap_or("-")
            ));
            Ok(text(clip(&out)))
        })()),

        "ncc_get_mem" => ok_or_text((|| {
            let key = sarg("key").unwrap_or_default();
            if key.is_empty() {
                return Err(anyhow::anyhow!("缺 key"));
            }
            let mut qs = vec![
                format!("key={}", api::urlenc(&key)),
                format!(
                    "subject={}",
                    api::urlenc(&sarg("subject").unwrap_or_else(|| "self".into()))
                ),
            ];
            if let Some(ns) = sarg("namespace") {
                qs.push(format!("namespace={}", api::urlenc(&ns)));
            }
            let d = api::get(
                cfg,
                &format!("/api/mem/lookup?{}", qs.join("&")),
                token.as_deref(),
            )?;
            let m = &d["memory"];
            Ok(text(clip(&format!(
                "{} / {}  〔{} · rev {}〕\n{}{}",
                m["subject"].as_str().unwrap_or("-"),
                m["key"].as_str().unwrap_or("-"),
                m["kind"].as_str().unwrap_or("-"),
                m["revision"].as_i64().unwrap_or(0),
                m["value"].as_str().unwrap_or(""),
                match (m["source"].as_str(), m["expiresAt"].as_str()) {
                    (Some(src), Some(exp)) => format!("\n（来源 {src} · {exp} 过期）"),
                    (Some(src), None) => format!("\n（来源 {src}）"),
                    (None, Some(exp)) => format!("\n（{exp} 过期）"),
                    (None, None) => String::new(),
                }
            ))))
        })()),

        "ncc_list_ckpt" => ok_or_text((|| {
            let mut qs = [
                ("ref", rarg("ref")),
                ("label", sarg("label")),
                ("q", sarg("query")),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|x| format!("{k}={}", api::urlenc(x))))
            .collect::<Vec<_>>();
            qs.push(format!("limit={}", narg("limit", 20, 200)));
            let d = api::get(
                cfg,
                &format!("/api/ckpt?{}", qs.join("&")),
                token.as_deref(),
            )?;
            let mut out = String::new();
            for c in d["checkpoints"].as_array().cloned().unwrap_or_default() {
                out.push_str(&format!(
                    "- {} 〔{} · step {}〕 {} {} · {} 字节{} · {}\n",
                    c["id"].as_str().unwrap_or("-"),
                    c["label"].as_str().unwrap_or("-"),
                    c["step"].as_i64().unwrap_or(0),
                    c["name"].as_str().unwrap_or("-"),
                    c["digest"].as_str().unwrap_or("-"),
                    c["size"].as_i64().unwrap_or(0),
                    if c["subjectRef"].as_str().unwrap_or("").is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", c["subjectRef"].as_str().unwrap_or("-"))
                    },
                    c["createdAt"].as_str().unwrap_or("-")
                ));
            }
            if out.is_empty() {
                out.push_str("（没有检查点）\n");
            }
            out.push_str(
                "\n取字节请让用户跑 `ncc ckpt pull <id> --out <文件>`（客户端会核对摘要）—— \
                 检查点的价值就是「拿回来的是原来那份」，工具里不给字节通道。",
            );
            Ok(text(clip(&out)))
        })()),

        // ---- 网关控制面（只读）----
        "ncc_list_gateways" => ok_or_text((|| {
            let d = gw_summary(cfg, token.as_deref(), sarg("namespace").as_deref())?;
            let rows = d["gateways"].as_array().cloned().unwrap_or_default();
            let mut out = String::new();
            if rows.is_empty() {
                out.push_str(
                    "（名下还没有网关。客户自装的那台机器上跑 `ncc gateway bind` 就会出现在这里 —— \
                     注册是持凭据的人的动作，不在工具里。）\n",
                );
            }
            for r in &rows {
                let g = &r["gateway"];
                let u = &r["usage"];
                out.push_str(&format!(
                    "- {}  {}  @{}  [{}]{}\n",
                    g["name"].as_str().unwrap_or("-"),
                    g["id"].as_str().unwrap_or("-"),
                    g["namespace"].as_str().unwrap_or("-"),
                    g["status"].as_str().unwrap_or("-"),
                    if g["status"].as_str().unwrap_or("")
                        == g["statusReported"].as_str().unwrap_or("")
                    {
                        String::new()
                    } else {
                        format!("（自报 {}）", g["statusReported"].as_str().unwrap_or("-"))
                    }
                ));
                out.push_str(&format!(
                    "    版本 {} · 最近心跳 {} · 摘要窗口 {} · 留存 {}h\n",
                    g["version"].as_str().unwrap_or("-"),
                    g["lastSeenAt"].as_str().unwrap_or("从未"),
                    g["auditWindows"].as_i64().unwrap_or(0),
                    g["retentionHours"].as_i64().unwrap_or(0)
                ));
                out.push_str(&format!(
                    "    用量 请求 {}（放行 {} / 拒绝 {}）· 出站 {} · 活跃 {} 天\n",
                    u["requests"].as_i64().unwrap_or(0),
                    u["allowed"].as_i64().unwrap_or(0),
                    u["denied"].as_i64().unwrap_or(0),
                    crate::gwreport::human_bytes(u["egressBytes"].as_i64().unwrap_or(0)),
                    u["days"].as_i64().unwrap_or(0)
                ));
            }
            let t = &d["totals"];
            out.push_str(&format!(
                "\n共 {} 台（限额 Free {} / Pro {}）· 窗口内合计 请求 {} / 出站 {}\n\
                 ⚠ 在线状态由控制面**按心跳超时推导**（不是网关自报）；用量是**网关自报的计数**。\n\
                 细节（主机名 / 状态桶 / 延迟分位）用 ncc_gateway_audit。",
                d["total"].as_i64().unwrap_or(0),
                d["limits"]["free"].as_i64().unwrap_or(0),
                d["limits"]["pro"].as_i64().unwrap_or(0),
                t["requests"].as_i64().unwrap_or(0),
                crate::gwreport::human_bytes(t["egressBytes"].as_i64().unwrap_or(0))
            ));
            Ok(text(clip(&out)))
        })()),

        "ncc_gateway_audit" => ok_or_text((|| {
            let want =
                sarg("gateway").ok_or_else(|| anyhow::anyhow!("需要 gateway（GW-… 或名字）"))?;
            let (id, name) = gw_resolve(cfg, token.as_deref(), &want)?;
            let q = [
                ("since", sarg("since")),
                ("limit", Some(narg("limit", 20, 200).to_string())),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|x| format!("{k}={}", api::urlenc(x))))
            .collect::<Vec<_>>()
            .join("&");
            let d = api::get(
                cfg,
                &format!("/api/gateways/{id}/audit?{q}"),
                token.as_deref(),
            )?;
            let rows = d["summaries"].as_array().cloned().unwrap_or_default();
            let mut out = format!("{}（{id}）留存摘要 {} 条\n", name, rows.len());
            if rows.is_empty() {
                out.push_str(
                    "（还没有摘要。可能：网关刚接上、这段时间没有流量、或已过留存期。）\n",
                );
            }
            for r in &rows {
                out.push_str(&format!(
                    "- {} → {}  请求 {}（放行 {} / 拒绝 {}）· 出站 {} / 入站 {} · p50 {}ms p95 {}ms max {}ms\n",
                    r["windowStart"].as_str().unwrap_or("-"),
                    r["windowEnd"].as_str().unwrap_or("-"),
                    r["requests"].as_i64().unwrap_or(0),
                    r["allowed"].as_i64().unwrap_or(0),
                    r["denied"].as_i64().unwrap_or(0),
                    crate::gwreport::human_bytes(r["egressBytes"].as_i64().unwrap_or(0)),
                    crate::gwreport::human_bytes(r["ingressBytes"].as_i64().unwrap_or(0)),
                    r["latencyP50Ms"].as_i64().unwrap_or(0),
                    r["latencyP95Ms"].as_i64().unwrap_or(0),
                    r["latencyMaxMs"].as_i64().unwrap_or(0)
                ));
                let line = [
                    ("主机名", "topDomains"),
                    ("状态", "statusBuckets"),
                    ("拒绝理由", "denyReasons"),
                    ("路由", "routes"),
                    ("方向", "sides"),
                ]
                .iter()
                .filter_map(|(label, key)| {
                    let list = r[*key].as_array()?;
                    if list.is_empty() {
                        return None;
                    }
                    let joined = list
                        .iter()
                        .map(|b| {
                            format!(
                                "{}×{}",
                                b["name"].as_str().unwrap_or("-"),
                                b["count"].as_i64().unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    Some(format!("{label} {joined}"))
                })
                .collect::<Vec<_>>()
                .join(" · ");
                if !line.is_empty() {
                    out.push_str(&format!("    {line}\n"));
                }
            }
            out.push_str(
                "\n⚠ 控制面**只有摘要**：路径 / 载荷 / 凭据不在（路径常带订单号这类业务标识，故意不上报）。\n\
                 要逐条审计（含路径）只能在**那台网关本机**跑 `ncc gateway audit`。\n\
                 计数是网关自报的：签名证明来源与完整，不证明内容为真。",
            );
            Ok(text(clip(&out)))
        })()),

        "ncc_gateway_usage" => ok_or_text((|| {
            let want =
                sarg("gateway").ok_or_else(|| anyhow::anyhow!("需要 gateway（GW-… 或名字）"))?;
            let (id, name) = gw_resolve(cfg, token.as_deref(), &want)?;
            let q = match sarg("since") {
                Some(v) => format!("?since={}", api::urlenc(&v)),
                None => String::new(),
            };
            let d = api::get(
                cfg,
                &format!("/api/gateways/{id}/usage{q}"),
                token.as_deref(),
            )?;
            Ok(text(clip(&format!(
                "{}（{id}）\n窗口 {} 个 · 活跃 {} 天 · 请求 {}（放行 {} / 拒绝 {}）· 出站 {}\n\
                 区间 {} → {}（统计起点 {} · 最近心跳 {}）\n⚠ {}",
                name,
                d["windows"].as_i64().unwrap_or(0),
                d["days"].as_i64().unwrap_or(0),
                d["requests"].as_i64().unwrap_or(0),
                d["allowed"].as_i64().unwrap_or(0),
                d["denied"].as_i64().unwrap_or(0),
                crate::gwreport::human_bytes(d["egressBytes"].as_i64().unwrap_or(0)),
                d["firstWindow"].as_str().unwrap_or("-"),
                d["lastWindow"].as_str().unwrap_or("-"),
                d["since"].as_str().unwrap_or("-"),
                d["lastSeenAt"].as_str().unwrap_or("-"),
                d["basis"].as_str().unwrap_or("-")
            ))))
        })()),

        // ---- 跨局域网直连（P2P，判断面：不搬运业务字节）----
        // 红线：发现 ≠ 授权 ≠ 字节通道；STUN 可由 NCC 托管，**TURN 必须客户自托管**；
        // 打洞失败就明确报错，不降级为中心中转。所以这里只做「判断」，不接字节。
        "ncc_p2p_probe" => ok_or_text((|| {
            let offline = args
                .get("offline")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let (_, txt) = crate::p2p::probe_run(cfg, sarg("stun").as_deref(), offline);
            Ok(text(clip(&format!(
                "（注：这算的是**跑 MCP 的这台机器**，不是目标节点）\n{}",
                txt.trim_start()
            ))))
        })()),

        "ncc_p2p_check" => ok_or_text((|| {
            let addr = sarg("addr");
            let peer = sarg("peer");
            match (&addr, &peer) {
                (None, None) => {
                    anyhow::bail!("需要 addr（对端映射 ip:port）或 peer（对端节点引用）之一")
                }
                (Some(_), Some(_)) => {
                    anyhow::bail!(
                        "addr 与 peer 只能给一个：addr 直接对打（不走信令），peer 走控制面信令"
                    )
                }
                _ => {}
            }
            if peer.is_some() {
                // 走信令要控制面声明 p2p 能力；不声明就明确说清楚，别丢 404 给模型。
                capability::ensure(cfg, "p2p")?;
            }
            let a = crate::p2p::CheckArgs {
                peer,
                addr,
                from: sarg("from"),
                wait: narg("wait", 12, 60) as u64,
                stun: sarg("stun"),
                session: None,
                // json 只影响 CLI 的输出格式；MCP 靠 quiet=true 静默，这里给什么值都一样。
                json: false,
            };
            let v = crate::p2p::check_flow(cfg, &a, true)?;
            Ok(text(clip(&crate::p2p::render_check(&v))))
        })()),

        "ncc_p2p_node" => ok_or_text((|| {
            // /api/p2p/self 只存在于内网 ncc-registry 节点（云端只有控制面：信令/票据/ICE）。
            let meta = capability::probe(cfg);
            if meta.kind == "hub" {
                anyhow::bail!(
                    "当前目标是云端控制面（{}）：它只提供 P2P 控制面（信令/票据/ICE），没有节点侧画像。\n  要看内网节点那台机器的条件：切到节点目标（`ncc target use <节点名>`）再调；\n  要看**本机**条件：用 ncc_p2p_probe。",
                    cfg.base_url()
                );
            }
            let tok = token.clone().context(
                "看节点画像需要登录：先运行 `ncc login`（或 `ncc registry login`），或配置 API-Key",
            )?;
            let v = crate::registryp2p::self_json(cfg, &tok)?;
            Ok(text(clip(&render_p2p_node(&v))))
        })()),

        // ---- 对外服务（NCC Service）----
        "ncc_match_services" => ok_or_text((|| {
            let intent = sarg("intent")
                .or_else(|| sarg("query"))
                .ok_or_else(|| anyhow::anyhow!("需要 intent（你想办什么事）"))?;
            let category = sarg("category").unwrap_or_default();
            let region = sarg("region").unwrap_or_default();
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let limit = narg("limit", 5, 20) as u32;
            let v = crate::services::fetch_match(cfg, &intent, &category, &tags, &region, limit)?;
            Ok(text(clip(&crate::services::render_match(&v))))
        })()),

        "ncc_match_index" => ok_or_text((|| {
            let intent = sarg("intent")
                .or_else(|| sarg("query"))
                .ok_or_else(|| anyhow::anyhow!("需要 intent（你要办什么事）"))?;
            let channel = sarg("channel").unwrap_or_default();
            let region = sarg("region").unwrap_or_default();
            let want = sarg("want").unwrap_or_else(|| "supply".to_string());
            let limit = narg("limit", 5, 20) as i64;
            let v = crate::index::fetch_match(cfg, &intent, &channel, &region, &want, limit)?;
            Ok(text(&crate::index::render_match_text(&v)))
        })()),

        "ncc_list_index_channels" => ok_or_text((|| {
            let prefix = sarg("prefix").unwrap_or_default();
            let path = if prefix.trim().is_empty() {
                "/api/index/channels".to_string()
            } else {
                format!("/api/index/channels?prefix={prefix}")
            };
            let v = crate::api::get(cfg, &path, crate::config::token_opt(cfg).as_deref())?;
            let rows = v
                .get("channels")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                return Ok(text("还没有频道（没人登记过索引）。"));
            }
            let mut out = String::new();
            for r in &rows {
                let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!(
                    "{}：{} 条（供给 {} / 需求 {}）· {} 人登记\n",
                    s("channel"),
                    r.get("entries").and_then(|v| v.as_i64()).unwrap_or(0),
                    r.get("supply").and_then(|v| v.as_i64()).unwrap_or(0),
                    r.get("needs").and_then(|v| v.as_i64()).unwrap_or(0),
                    r.get("providers").and_then(|v| v.as_i64()).unwrap_or(0),
                ));
            }
            Ok(text(&out))
        })()),

        "ncc_list_services" => ok_or_text((|| {
            let category = sarg("category").unwrap_or_default();
            let tag = sarg("tag").unwrap_or_default();
            let region = sarg("region").unwrap_or_default();
            let limit = narg("limit", 20, 50) as u32;
            let v = crate::services::fetch_list(cfg, &category, &tag, &region, limit)?;
            Ok(text(clip(&crate::services::render_list(&v))))
        })()),

        "ncc_get_service" => ok_or_text((|| {
            let target =
                rarg("ref").ok_or_else(|| anyhow::anyhow!("需要 ref（@提供方/标识 或 SV-…）"))?;
            let v = crate::services::fetch_show(cfg, &target)?;
            Ok(text(clip(&crate::services::render_show(&v))))
        })()),

        "ncc_service_categories" => ok_or_text((|| {
            let v = api::get(cfg, "/api/services/catalog", None)?;
            let mut out = String::from("业务分类（用于 ncc_match_services 的 category）：\n");
            for c in v["categories"].as_array().cloned().unwrap_or_default() {
                let n = c["count"].as_i64().unwrap_or(0);
                out.push_str(&format!(
                    "  {:<12} {:<14}（{}）  {}\n",
                    c["id"].as_str().unwrap_or(""),
                    c["zh"].as_str().unwrap_or(""),
                    if n > 0 {
                        format!("{n} 条服务")
                    } else {
                        "暂无".to_string()
                    },
                    c["descZh"].as_str().unwrap_or("")
                ));
            }
            out.push_str("\n接入方式：");
            for p in v["protocols"].as_array().cloned().unwrap_or_default() {
                out.push_str(&format!(
                    " {}（{}）",
                    p["id"].as_str().unwrap_or(""),
                    p["zh"].as_str().unwrap_or("")
                ));
            }
            out.push_str("\n授权方式：");
            for a in v["accessModes"].as_array().cloned().unwrap_or_default() {
                out.push_str(&format!(
                    " {}（{}）",
                    a["id"].as_str().unwrap_or(""),
                    a["zh"].as_str().unwrap_or("")
                ));
            }
            out.push_str(&format!(
                "\n\n目录里已有 {} 条公开服务，来自 {} 个提供方。",
                v["total"], v["providers"]
            ));
            Ok(text(clip(&out)))
        })()),

        // ---- 团队配置（NCC Config，只读）----
        "ncc_list_configs" => ok_or_text((|| {
            let ns = sarg("namespace").unwrap_or_default();
            let kind = sarg("kind").unwrap_or_default();
            let env = sarg("env").unwrap_or_default();
            let mine = args.get("mine").and_then(|v| v.as_bool()).unwrap_or(false);
            let v = crate::configs::fetch_configs(cfg, &ns, &kind, &env, mine)?;
            Ok(text(clip(&crate::configs::render_configs(&v))))
        })()),

        "ncc_get_config" => ok_or_text((|| {
            let target =
                rarg("ref").ok_or_else(|| anyhow::anyhow!("需要 ref（@命名空间/slug）"))?;
            let reveal = args
                .get("reveal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let revision = args.get("revision").and_then(|v| v.as_i64());
            let v = crate::configs::fetch_config(cfg, &target, reveal, revision)?;
            Ok(text(clip(&crate::configs::render_config(&v))))
        })()),

        other => tool_err(format!("未知工具: {other}")),
    })
}

/* ---------------- 小工具 ---------------- */

/// 节点侧画像的人读文本（MCP 专用：与 `ncc registry p2p self` 的信息一致，
/// 但改成适合模型快速抓要点的排布）。
/// 网关列表/汇总（`namespace` 给了就按命名空间过滤）。
fn gw_summary(cfg: &CliConfig, token: Option<&str>, namespace: Option<&str>) -> Result<Value> {
    let path = match namespace {
        Some(ns) => format!("/api/gateways?namespace={}", api::urlenc(ns)),
        None => "/api/gateways/summary".to_string(),
    };
    api::get(cfg, &path, token)
}

/// 把「网关 id 或名字」解析成 id。
///
/// 名字**不保证唯一**（同一个人在个人与组织命名空间下可以各有一台同名网关），
/// 所以命中多台时把候选列出来让模型去问用户 —— 拿错网关的摘要等于给出错误的
/// 合规结论，宁可报错也不要瞎挑一台。
fn gw_resolve(cfg: &CliConfig, token: Option<&str>, want: &str) -> Result<(String, String)> {
    let want = want.trim();
    if want.starts_with("GW-") {
        return Ok((want.to_string(), want.to_string()));
    }
    let d = gw_summary(cfg, token, None)?;
    let rows = d["gateways"].as_array().cloned().unwrap_or_default();
    let hit = |g: &Value| {
        (
            g["id"].as_str().unwrap_or("").to_string(),
            g["name"].as_str().unwrap_or("").to_string(),
        )
    };
    let exact: Vec<_> = rows
        .iter()
        .map(|r| &r["gateway"])
        .filter(|g| {
            g["name"].as_str().unwrap_or("") == want || g["id"].as_str().unwrap_or("") == want
        })
        .map(hit)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    let fuzzy: Vec<_> = rows
        .iter()
        .map(|r| &r["gateway"])
        .filter(|g| g["name"].as_str().unwrap_or("").contains(want))
        .map(hit)
        .collect();
    let pick = if exact.len() > 1 { exact } else { fuzzy };
    match pick.len() {
        1 => Ok(pick[0].clone()),
        0 => anyhow::bail!(
            "找不到网关「{want}」。用 ncc_list_gateways 看名下有哪些（名字或 GW-… id 都认）。"
        ),
        _ => anyhow::bail!(
            "「{want}」匹配到 {} 台网关，请指定 id：\n{}",
            pick.len(),
            pick.iter()
                .map(|(id, name)| format!("  - {name}  {id}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

fn render_p2p_node(v: &Value) -> String {
    let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("");
    let n = |p: &str| v.pointer(p).and_then(|x| x.as_u64()).unwrap_or(0);
    let p = v.get("profile").cloned().unwrap_or(Value::Null);
    let serve = v.get("serve").cloned().unwrap_or(Value::Null);
    let mut out = format!(
        "目标节点 {}（{} · {}{}）\n",
        s("/node/id"),
        s("/node/name"),
        s("/node/role"),
        if s("/node/region").is_empty() {
            String::new()
        } else {
            format!(" · {}", s("/node/region"))
        },
    );
    out.push_str(&format!(
        "  本机出网 IP {}（{}）· UDP 本地端口 {}\n  公网映射 {}\n",
        s("/profile/localIpv4"),
        s("/profile/localAddrKind"),
        n("/profile/localPort"),
        if s("/profile/mapped").is_empty() {
            "（没探到）"
        } else {
            s("/profile/mapped")
        },
    ));
    out.push_str(&format!(
        "  STUN 可达 {}/{} · 映射 {}（{}）· 过滤 {}（{}）\n  结论 {}\n  建议：{}\n",
        n("/profile/serversReached"),
        n("/profile/serversTried"),
        s("/profile/mappingBehavior"),
        s("/profile/mappingMethod"),
        s("/profile/filteringBehavior"),
        s("/profile/filteringMethod"),
        s("/profile/verdict"),
        s("/profile/advice"),
    ));
    let servers = p
        .get("servers")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    out.push_str(&format!(
        "  STUN 列表：{}\n",
        if servers.is_empty() {
            "（未配置，用内置默认）"
        } else {
            &servers
        }
    ));
    if serve.get("on").and_then(|x| x.as_bool()) == Some(true) {
        let peers = serve
            .get("peers")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "  可被打洞入口：已开 —— 对端应发往 {} · 已应答 {} 次 · 收到对端回包 {} 次\n",
            if s("/serve/mapped").is_empty() {
                "-"
            } else {
                s("/serve/mapped")
            },
            n("/serve/requestsTaken"),
            n("/serve/responsesSeen"),
        ));
        out.push_str(&format!(
            "    反向打洞对端：{}\n",
            if peers.is_empty() {
                "无（对方可能打不进：地址/端口相关过滤的 NAT 必须双方同时发）"
            } else {
                &peers
            }
        ));
        if !s("/serve/note").is_empty() {
            out.push_str(&format!("    提示：{}\n", s("/serve/note")));
        }
    } else {
        out.push_str("  可被打洞入口：关（别人打不进来；要开得由用户在节点上跑 `ncc registry p2p serve --on`）\n");
    }
    out
}

/// 文本类制品的粗略判定（决定是直接把内容给模型，还是只给下载地址）。
fn is_texty(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    const EXTS: [&str; 14] = [
        ".md",
        ".markdown",
        ".txt",
        ".json",
        ".yaml",
        ".yml",
        ".toml",
        ".sh",
        ".bash",
        ".py",
        ".js",
        ".ts",
        ".rs",
        ".go",
    ];
    EXTS.iter().any(|e| path.ends_with(e))
}

/// 按字符（非字节）截断，避免把中文截成半个字。
fn clip(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= MAX_TEXT {
        return s.to_string();
    }
    let head: String = chars[..MAX_TEXT].iter().collect();
    format!("{head}\n…（已截断，原始 {} 字符）", chars.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 契约（`agent/harness.json`）里写的工具，必须**恰好**是这里真的暴露的那些。
    ///
    /// 为什么值得一条测试：这份清单是分发给外部 runtime 看的（`kind=harness` 的
    /// `harness.tools`），漂了它不会报错 —— 只会让别人的 runtime 以为有个不存在的工具，
    /// 或者看不到新加的工具。加/改工具时若忘了同步 manifest，这条测试会立刻红。
    #[test]
    fn harness_manifest_lists_exactly_the_exposed_tools() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent/harness.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读不了 {}：{e}", path.display()));
        let manifest: Value =
            serde_json::from_str(&text).expect("agent/harness.json 不是合法 JSON");
        let listed: Vec<String> = manifest["harness"]["tools"]
            .as_array()
            .expect("harness.tools 必须是数组")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        let exposed: Vec<String> = tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            listed, exposed,
            "agent/harness.json 的工具清单与 mcp.rs 暴露的不一致 —— 改一边就要同步另一边"
        );
    }

    /// 每个工具的能力门禁 / 名字都不该有重复（重复会让 dispatch 只走第一个）。
    #[test]
    fn tool_names_are_unique() {
        let mut names: Vec<String> = tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default().to_string())
            .collect();
        let total = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), total, "工具名有重复");
    }

    /// `tool_capability` 里写的能力名必须和服务端词表对得上（拼错 = 门禁永远放行或永远拒绝）。
    #[test]
    fn capabilities_are_from_the_shared_vocabulary() {
        for t in tools() {
            let name = t["name"].as_str().unwrap_or_default();
            if let Some(cap) = tool_capability(name) {
                assert!(
                    [
                        "services", "config", "nodes", "grants", "trace", "kb", "mem", "ckpt",
                        "p2p", "profile", "gateway", "index"
                    ]
                    .contains(&cap),
                    "{name} 的能力名 `{cap}` 不在共享词表里"
                );
            }
        }
    }
}
