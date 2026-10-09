//! `.huf` —— Harness-Use **Files**：**面向用户的资源包**。
//!
//! # 为什么要跟 `.hur` 分开
//!
//! `.hur` 是**运行时的包**：有 `entry`，能进沙箱跑；`.huf` 是**给用户的文件**：文档、提示词、
//! 模板、静态资产、技能文本、知识库片段 —— 它**没有入口，也不能执行**。
//!
//! 分开的理由不是"看起来整齐"，而是两件事：
//! ① **收件人不同**：`.hur` 交给 runtime/宿主，`.huf` 交给人（读、放进编辑器、当资料）；
//! ② **判据要能一眼看出**：拿到一个文件，不该先打开看里面有没有 `entry` 才知道它会不会跑。
//!    扩展名就是那句判据（`.hur` 能跑 / `.huf` 不跑），本模块用 H3 把这条边界**硬拦**住。
//!
//! # 与 `.hur` 的关系
//!
//! **同一套容器**：`gzip(zip(huf.json, huf.lock, 内容…))`、同样的确定性字节规则（条目按路径
//! 排序、Stored、时间戳固定、gzip 头写死）、同样的防穿越解包、同样的 Minisign/ed25519 侧车签名。
//! 差别只有三处，全在 [`crate::spec::Format`] 里：清单名（`huf.json`）、规范号
//! （`harness-use-files/v1`）与内容目录（**没有 `src/`** —— 那是代码的落点）。
//!
//! 一句人话：**`.hur` 装「能跑的东西」，`.huf` 装「给人看的东西」，容器是同一个。**
//!
//! 规则编号 **H1~H9**（见 [`validate`] 与 [`check_sidecar`]；`.hur` 那边是 R1~R13）。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::pack::{self, PackOutcome};
use crate::spec::{
    content_files_in, rel, Format, HurLock, Issue, PublishInfo, HUF_KIND, HUF_LOCK_SPEC,
    HUF_MANIFEST, HUF_SPEC,
};

/// 这份文件**给谁看**（可省，缺省 `user`）。三种都能读的东西别都写成 `user`：
/// 运维手册给 `operator`、开发者文档给 `developer` —— 目录检索与网页展示都按它分组。
pub const AUDIENCES: [&str; 4] = ["user", "operator", "developer", "agent"];

fn default_audience() -> String {
    "user".to_string()
}

/// 资源包里**不许出现**的字段：它们每一个都意味着"这份包能跑"。
///
/// 逐个列出来的原因（而不是写"任何未知字段都拒"）：`huf.json` 将来加新字段不该被这条
/// 规则误伤 —— 拒的是**语义属于运行时的那些键**，不是"我没见过"。
pub const RUNTIME_KEYS: [&str; 10] = [
    "entry",
    "runtime",
    "capabilities",
    "agent",
    "harness",
    "sandbox",
    "policy",
    "egress",
    "permissions",
    "state",
];

/// `huf.json`：资源包的清单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HufManifest {
    pub spec: String,
    /// 恒为 `files`（[`HUF_KIND`]）
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    /// 一句话（目录列表里显示的那行）
    #[serde(default)]
    pub short: String,
    /// 展开说明（给人读的正文，不是摘要）
    #[serde(default)]
    pub summary: String,
    #[serde(default = "default_audience")]
    pub audience: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub deps: HufDeps,
    #[serde(default)]
    pub publish: PublishInfo,
}

/// 资源包的依赖：**只能依赖别的技能文本与知识库**。
///
/// 为什么结构上就没有 `harness`/`agent`/`mcp` 三个字段：资源包不引"能跑的东西"，
/// 否则"把文档包发给用户"就变成了"顺带往他机器上装一个运行时"。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HufDeps {
    #[serde(default)]
    pub skill: Vec<String>,
    #[serde(default)]
    pub kb: Vec<String>,
}

impl HufDeps {
    pub fn iter(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::new();
        for (k, list) in [("skill", &self.skill), ("kb", &self.kb)] {
            for r in list {
                out.push((k, r.as_str()));
            }
        }
        out
    }
}

impl HufManifest {
    /// 清单一句话摘要（`ncc huf inspect` / 目录展示）
    pub fn headline(&self) -> String {
        let s = if self.short.trim().is_empty() {
            &self.summary
        } else {
            &self.short
        };
        s.trim().to_string()
    }
}

/// 解析清单文本。**先判规范号**：拿一份 `hur.json` 过来要给出"该用哪条命令"的答案，
/// 而不是一串 serde 字段名错误（用户手里只有这一个文件，看不出是两种东西）。
pub fn parse(text: &str) -> Result<HufManifest> {
    let raw: serde_json::Value =
        serde_json::from_str(text).map_err(|e| anyhow!("{HUF_MANIFEST} 不是合法 JSON：{e}"))?;
    if let Some(spec) = raw.get("spec").and_then(|s| s.as_str()) {
        if spec == crate::spec::PKG_SPEC {
            bail!(
                "这是 **.hur** 的清单（spec={spec}）：它装的是能跑的包 —— 改用 `ncc hur` 那条线；\n\
                 面向用户的资源包请写 spec={HUF_SPEC}、kind={HUF_KIND}（`ncc huf init` 会生成一份）"
            );
        }
    }
    let m: HufManifest = serde_json::from_value(raw).map_err(|e| {
        anyhow!("{HUF_MANIFEST} 解析失败：{e}（最小清单：spec/kind/id/name/version）")
    })?;
    Ok(m)
}

/// 读工程目录里的 `huf.json`
pub fn read(dir: &Path) -> Result<HufManifest> {
    let p = dir.join(HUF_MANIFEST);
    if !p.exists() {
        bail!(
            "当前目录没有 {HUF_MANIFEST}（用 `ncc huf init` 生成，或 cd 到资源包目录；\
             可执行包用的是 hur.json，那是 `ncc hur`）"
        );
    }
    parse(&std::fs::read_to_string(&p)?)
}

/// 读任意位置：工程目录 或 `.huf` / `.huf.gz` 产物（产物先解到临时目录）。
pub fn read_any(path: &Path) -> Result<(HufManifest, PathBuf, Option<PathBuf>)> {
    if !crate::spec::is_archive_path(path) {
        let dir = pack::find_root_of(path, Format::Huf)?;
        return Ok((read(&dir)?, dir, None));
    }
    let tmp = std::env::temp_dir().join(format!("ncc-huf-read-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    unpack(path, &tmp)?;
    let m = read(&tmp)?;
    Ok((m, tmp.clone(), Some(tmp)))
}

/// 锁（`huf.lock`）：包身份的一半 —— 逐文件 sha256 + 版本，签名覆盖的是**它**
/// （连同清单与内容一起进确定性字节）。
///
/// 与 `.hur` 复用同一个结构（[`HurLock`]）：字段一模一样（spec/package/version/files），
/// 只有 spec 号不同。**不做第二套结构** —— 锁的语义本来就是"这份包的字节指纹清单"。
pub fn build_lock(dir: &Path) -> Result<HurLock> {
    let m = read(dir)?;
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for f in content_files_in(dir, Format::Huf.content_dirs()) {
        files.insert(rel(dir, &f), crate::spec::sha256_file(&f)?);
    }
    let lock = HurLock {
        spec: HUF_LOCK_SPEC.to_string(),
        package: m.id.clone(),
        version: m.version.clone(),
        // 资源包的依赖都是**别的包**（skill/kb），由目录侧解析 —— 本地路径依赖不在这里锁
        deps: BTreeMap::new(),
        files,
    };
    std::fs::write(
        dir.join(Format::Huf.lock()),
        format!("{}\n", serde_json::to_string_pretty(&lock)?),
    )
    .with_context(|| format!("写 {} 失败", dir.join(Format::Huf.lock()).display()))?;
    Ok(lock)
}

pub fn read_lock(dir: &Path) -> Option<HurLock> {
    let p = dir.join(Format::Huf.lock());
    if !p.exists() {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// 产包：`dist/<id>-<version>.huf.gz` + `.sha256`（确定性字节，与 `.hur` 同规则）
pub fn pack(dir: &Path) -> Result<PackOutcome> {
    pack::pack_for(dir, Format::Huf)
}

/// 解包（防穿越）：`.huf` 只能解出 `huf.json` 那份清单
pub fn unpack(archive: &Path, dest: &Path) -> Result<(Vec<String>, String, String)> {
    pack::unpack_for(archive, dest, Format::Huf)
}

/// 校验清单 + 内容 + 锁（**H1~H7**）。
///
/// **不含签名**：签名的对象是**产物那份字节**，工程目录这边要么核 `dist/` 里的产物、
/// 要么重算规范字节 —— 那条路在 [`check_signature`]（H8），产物摘要侧车在
/// [`check_sidecar`]（H9）。把它塞进这里会让"校验一个目录"悄悄变成"校验一个产物"。
///
/// 编号与 `.hur` 的 `R1~R13` **分开**：两套规则碰巧都得看清单，但判据不同，
/// 用同一串编号会让人以为可以互相对照。
pub fn validate(m: &HufManifest, dir: &Path, lock: Option<&HurLock>) -> Vec<Issue> {
    let mut out = Vec::new();

    // H1 规范号
    if m.spec != HUF_SPEC {
        out.push(Issue::err(
            "H1",
            format!("spec 必须是「{HUF_SPEC}」，当前是「{}」", m.spec),
        ));
    }
    // H2 kind
    if m.kind != HUF_KIND {
        out.push(Issue::err(
            "H2",
            format!(
                "kind 必须是「{HUF_KIND}」（资源包的唯一身份），当前是「{}」",
                m.kind
            ),
        ));
    }
    // H3 不许有"能跑"的字段 —— 这条是 `.huf` 存在的理由，必须硬拦
    if dir.join(crate::spec::MANIFEST).exists() {
        out.push(Issue::err(
            "H3",
            "目录里同时有 hur.json 与 huf.json：一个目录只能是**一种**包（能跑 / 给人看）—— \
             想把它变成可执行包，请用 `ncc hur` 那条线另开一个工程",
        ));
    }
    if let Some(raw) = read_raw(dir) {
        for k in RUNTIME_KEYS {
            if raw.get(k).is_some() {
                out.push(Issue::err(
                    "H3",
                    format!(
                        "资源包里不允许出现「{k}」：它意味着这份包能跑 —— 能跑的是 `.hur`（ncc hur）"
                    ),
                ));
            }
        }
    }
    // H4 id
    if !valid_id(&m.id) {
        out.push(Issue::err(
            "H4",
            format!(
                "id「{}」不合法：只含字母数字与 - _ . / @，且不以 - 开头或结尾（例：docs/onboarding）",
                m.id
            ),
        ));
    }
    // H5 名称与版本
    if m.name.trim().is_empty() {
        out.push(Issue::err(
            "H5",
            "name 不能为空（它就是用户在列表里看到的名字）",
        ));
    }
    if crate::spec::parse_semver(&m.version).is_none() {
        out.push(Issue::err(
            "H5",
            format!("version「{}」不是 x.y.z 形式的语义化版本", m.version),
        ));
    }
    // H6 给谁看
    if !AUDIENCES.contains(&m.audience.as_str()) {
        out.push(Issue::err(
            "H6",
            format!(
                "audience「{}」不在词表里（{}）",
                m.audience,
                AUDIENCES.join("|")
            ),
        ));
    }
    if m.headline().is_empty() {
        out.push(Issue::warn(
            "H6",
            "short / summary 都是空的：用户在目录里只会看到 id —— 补一句这是什么",
        ));
    }
    // H7 内容与锁
    let files = content_files_in(dir, Format::Huf.content_dirs());
    if files.is_empty() {
        out.push(Issue::err(
            "H7",
            format!(
                "资源包里一个文件也没有（内容目录：{}）",
                Format::Huf.content_dirs().join(" / ")
            ),
        ));
    }
    if !dir.join("docs").is_dir() {
        out.push(Issue::info("H7", "没有 docs/ 目录（纯资产包可以不写）"));
    }
    match lock {
        None => out.push(Issue::warn(
            "H7",
            "还没有 huf.lock：`ncc huf build` 生成（签名覆盖它；没锁的包只能自己用）",
        )),
        Some(l) => {
            let on_disk: BTreeMap<String, String> = files
                .iter()
                .map(|f| {
                    let r = rel(dir, f);
                    let h = crate::spec::sha256_file(f).unwrap_or_default();
                    (r, h)
                })
                .collect();
            for (path, sha) in &l.files {
                match on_disk.get(path) {
                    None => out.push(Issue::err(
                        "H7",
                        format!("锁里的「{path}」在磁盘上没有了 —— 重新 `ncc huf build`"),
                    )),
                    Some(actual) if actual != sha => out.push(Issue::err(
                        "H7",
                        format!(
                            "「{path}」的字节变了（锁 {sha}…，实际 {actual}…）—— 重新 build 再签"
                        ),
                    )),
                    _ => {}
                }
            }
            for path in on_disk.keys() {
                if !l.files.contains_key(path) {
                    out.push(Issue::warn(
                        "H7",
                        format!("「{path}」还没进锁（新加的文件？）—— 重新 `ncc huf build`"),
                    ));
                }
            }
            if l.package != m.id || l.version != m.version {
                out.push(Issue::warn(
                    "H7",
                    format!(
                        "锁里写的是 {} v{}，清单是 {} v{} —— 重新 build",
                        l.package, l.version, m.id, m.version
                    ),
                ));
            }
        }
    }
    out
}

/// 签名核对：签的是**产物字节**（`dist/<id>-<version>.huf.gz` 旁边的 `.minisig`）。
///
/// 三条路：产物在就核它（发给别人的正是这一份）；产物不在但签名在（刚签完、或 dist 被清过）
/// 就重算规范字节到临时文件核 —— 走的是同一套确定性字节规则，结果一样；都没有 = 未签名。
///
/// 规则编号 **H8**（`.hur` 的签名是 R9）：两套规则不要互相引用，否则用户会以为能对照。
pub fn check_signature(dir: &Path, m: &HufManifest, required: bool) -> crate::sign::SigReport {
    let dist = dir.join(crate::spec::DIST);
    let artifact = dist.join(crate::spec::huf_artifact_name(&m.id, &m.version));
    let sig = crate::sign::find_sig(&artifact);
    let store = crate::sign::store();
    if artifact.is_file() {
        return store.check_with_sig(&artifact, sig.as_deref(), required, None, "产物", "H8");
    }
    if let Some(sig) = sig {
        return match crate::pack::archive_bytes_for(dir, Format::Huf, false) {
            Ok((bytes, name)) => {
                let tmp = std::env::temp_dir()
                    .join(format!("huf-sigcheck-{}-{name}", std::process::id()));
                if let Err(e) = std::fs::write(&tmp, &bytes) {
                    crate::sign::SigReport::issue(
                        required,
                        Issue::err("H8", format!("写临时文件失败：{e}")),
                    )
                } else {
                    let r = store.check_with_sig(
                        &tmp,
                        Some(&sig),
                        required,
                        None,
                        "工程目录（重算的规范字节）",
                        "H8",
                    );
                    let _ = std::fs::remove_file(&tmp);
                    r
                }
            }
            Err(e) => crate::sign::SigReport::issue(
                required,
                Issue::err("H8", format!("无法重算规范字节来核对签名：{e:#}")),
            ),
        };
    }
    store.check_with_sig(&artifact, None, required, None, "工程目录", "H8")
}

/// 产物的 sha256 与侧车（`<产物>.sha256`）登记值对不对得上。
///
/// 规则号 **H9**（`.hur` 那边是 R6）。侧车不是包的一部分（不进容器、不参与签名），
/// 它的作用是"下载完先看一眼有没有被换过" —— 所以这里**只跟侧车比**，
/// 真值仍然由 H8 的签名与 H7 的锁共同确定。
pub fn check_sidecar(archive: &Path) -> Option<Issue> {
    let expect = crate::pack::sidecar_sha(archive)?;
    let actual = crate::spec::sha256_file(archive).ok()?;
    if expect == actual {
        return None;
    }
    Some(Issue::err(
        "H9",
        format!(
            "产物 sha256 与侧车登记值不一致：侧车 {expect}，实际 {actual}（字节被换过或侧车过期）"
        ),
    ))
}

/// 读清单的**原始 JSON**（H3 要按"键在不在"判，而不是按反序列化后的字段）
fn read_raw(dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join(HUF_MANIFEST)).ok()?;
    serde_json::from_str(&text).ok()
}

fn valid_id(id: &str) -> bool {
    if id.trim().is_empty() || id.len() > 96 {
        return false;
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@'))
    {
        return false;
    }
    !id.starts_with('-') && !id.ends_with('-') && !id.contains("..")
}

/// 生成一份资源包工程（`ncc huf init`）：`huf.json` + `docs/README.md`。
///
/// 骨架里**故意不放 `src/`**：那会让人以为资源包也能写代码。
pub fn init(dir: &Path, id: &str, name: &str, short: &str, force: bool) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let manifest = dir.join(HUF_MANIFEST);
    if manifest.exists() && !force {
        bail!("{} 已存在（--force 覆盖）", manifest.display());
    }
    let m = HufManifest {
        spec: HUF_SPEC.to_string(),
        kind: HUF_KIND.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        version: "0.1.0".to_string(),
        short: short.to_string(),
        summary: String::new(),
        audience: default_audience(),
        tags: Vec::new(),
        license: "Apache-2.0".to_string(),
        deps: HufDeps::default(),
        publish: PublishInfo::default(),
    };
    std::fs::write(
        &manifest,
        format!("{}\n", serde_json::to_string_pretty(&m)?),
    )?;

    let docs = dir.join("docs");
    std::fs::create_dir_all(&docs)?;
    let readme = docs.join("README.md");
    if !readme.exists() || force {
        std::fs::write(
            &readme,
            format!(
                "# {name}\n\n{short}\n\n<!--\n这份包是**给人看的**：docs/ 放文档、assets/ 放静态资源、\n\
                 skills/ 放技能文本、kb/ 放知识库片段、data/ 放数据快照。\n\
                 它没有入口、不能执行 —— 要能跑的包请用 `ncc hur init`（.hur）。\n-->\n"
            ),
        )?;
    }
    Ok(vec![manifest, readme])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("huf-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn project(tag: &str) -> PathBuf {
        let d = tmp(tag);
        init(&d, "docs/onboarding", "上手文档", "第一次用它看这份", false).unwrap();
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("assets/logo.txt"), b"ncc").unwrap();
        d
    }

    #[test]
    fn init_pack_unpack_roundtrip() {
        let d = project("roundtrip");
        let m = read(&d).unwrap();
        assert_eq!(m.spec, HUF_SPEC);
        assert_eq!(m.kind, "files");
        assert!(validate(&m, &d, None)
            .iter()
            .all(|i| i.level != crate::spec::Level::Error));

        let out = pack(&d).unwrap();
        assert!(
            out.file.to_string_lossy().ends_with(".huf.gz"),
            "{:?}",
            out.file
        );
        // 确定性：同样内容打两次必须同样是那一串 sha256
        let again = pack(&d).unwrap();
        assert_eq!(out.sha256, again.sha256);

        let dest = tmp("unpacked");
        let (files, id, version) = unpack(&out.file, &dest).unwrap();
        assert_eq!(id, "docs/onboarding");
        assert_eq!(version, "0.1.0");
        assert!(files.contains(&HUF_MANIFEST.to_string()));
        assert!(files.contains(&"assets/logo.txt".to_string()));
        // 解出来的目录本身也是一个合法的资源包工程
        let m2 = read(&dest).unwrap();
        assert!(validate(&m2, &dest, read_lock(&dest).as_ref())
            .iter()
            .all(|i| i.level != crate::spec::Level::Error));
    }

    #[test]
    fn runtime_fields_are_refused() {
        let d = project("runtime-fields");
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join(HUF_MANIFEST)).unwrap()).unwrap();
        raw["entry"] = serde_json::json!("src/agent.ts");
        std::fs::write(d.join(HUF_MANIFEST), raw.to_string()).unwrap();
        let m = read(&d).unwrap();
        let issues = validate(&m, &d, None);
        assert!(
            issues
                .iter()
                .any(|i| i.rule == "H3" && i.msg.contains("entry")),
            "entry 必须被 H3 拦下：{issues:?}"
        );
    }

    #[test]
    fn hur_manifest_gets_a_pointed_error() {
        let e = parse(r#"{"spec":"harness-use-package/v1","kind":"harness","id":"H-x","name":"x","version":"0.1.0"}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("ncc hur"), "{e}");
    }

    #[test]
    fn src_dir_is_not_content() {
        let d = project("no-src");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/main.rs"), b"fn main(){}").unwrap();
        let names: Vec<String> = content_files_in(&d, Format::Huf.content_dirs())
            .iter()
            .map(|f| rel(&d, f))
            .collect();
        assert!(!names.iter().any(|n| n.starts_with("src/")), "{names:?}");
    }

    #[test]
    fn archive_name_picks_the_right_format() {
        assert_eq!(
            Format::Huf,
            crate::spec::format_of_archive(Path::new("a-1.0.0.huf.gz"))
        );
        assert_eq!(
            Format::Hur,
            crate::spec::format_of_archive(Path::new("a-1.0.0.agent.hur.gz"))
        );
        assert!(crate::spec::is_archive_path(Path::new(
            "x/onboarding-0.1.0.huf.gz"
        )));
    }

    #[test]
    fn lock_drift_is_an_error() {
        let d = project("drift");
        build_lock(&d).unwrap();
        let m = read(&d).unwrap();
        std::fs::write(d.join("assets/logo.txt"), b"changed").unwrap();
        let issues = validate(&m, &d, read_lock(&d).as_ref());
        assert!(
            issues
                .iter()
                .any(|i| i.rule == "H7" && i.level == crate::spec::Level::Error),
            "{issues:?}"
        );
    }
}
