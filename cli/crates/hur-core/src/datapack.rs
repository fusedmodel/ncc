//! `datapack`：把**一次数据快照**封装成 HUR 包（`profile=kb-seed|mem-seed|ckpt-set|trace-set`）。
//!
//! 为什么要有这一层，而不是"把数据目录塞进包里"：
//!
//! * **快照与活状态是两回事**。kb / mem / ckpt 是会被反复写、会持续变大、默认私有的**状态**，
//!   它们住在节点上（见 `spec::StateDecl` 的说明）。能被打包、签名、分发的**只有快照** ——
//!   一份按了时间的、不可变的副本。把"活的记忆"当一个制品发出去，就是把别人的私人记忆装箱寄走。
//! * **快照必须说清三件事**：从哪儿来的（`source`）、什么时候的（`snapshot_at`）、能给谁看（`privacy`）。
//!   少了任何一样，这份包就只是"一坨来历不明的字节"。
//! * **数据不该能执行**。数据类 profile 在校验里被**禁止**带 `entry` 与 `permissions.network`
//!   （R12）—— 这一层负责让生成出来的包天生就合规，而不是等着别人去挑错。
//!
//! 目录约定：所有快照文件放 `data/` 下（`kb/` 保持原义：随包走的本地知识库文件）。

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::pack;
use crate::profile;
use crate::spec::{self, DataDecl, DataDoc, Deps, HurPackage, Permissions, PublishInfo, PKG_SPEC};
use crate::{tpl, VERSION};

/// 生成一份快照包要填的那些"关于这次快照"的话（与数据本身无关）。
#[derive(Debug, Clone, Default)]
pub struct SeedInput {
    /// 数据类 profile：`kb-seed` / `mem-seed` / `ckpt-set` / `trace-set`
    pub profile: String,
    pub name: String,
    pub version: String,
    pub summary: String,
    /// 来源：`@命名空间/slug`，或 `local` 这类本机来源
    pub source: String,
    /// 从哪个目标取的（目标名；空 = 本机）
    pub source_target: String,
    /// 快照时刻（RFC3339）
    pub snapshot_at: String,
    /// `public` | `internal` | `private`
    pub privacy: String,
    pub license: String,
    /// 轨迹类专用：`digest` | `preview` | `full`
    pub payload: Option<String>,
    pub note: String,
}

/// 写一份快照包到 `dir`（会创建目录）：
/// `data/**` + `hur.json` + `hur.lock`，并**先自己校验一遍**（不合规就不产出）。
///
/// 为什么要先校验：这份包是要签名、要发布、要给别人的。生成器放出一份自己都不合规的包，
/// 等于把问题推给下一个接手的人 —— 而 R12 的判定本来就在手边。
pub fn write(dir: &Path, input: &SeedInput, docs: Vec<(DataDoc, Vec<u8>)>) -> Result<HurPackage> {
    let prof = input.profile.trim();
    let Some(def) = profile::get(prof) else {
        bail!(
            "profile「{prof}」不在规范里（数据类可选：{}）",
            profile::PROFILES
                .iter()
                .filter(|p| p.data)
                .map(|p| p.name)
                .collect::<Vec<_>>()
                .join(" / ")
        );
    };
    if !def.data {
        bail!("profile「{prof}」不是数据快照（{}）—— 数据包必须走数据类 profile", def.summary);
    }
    if docs.is_empty() {
        bail!("快照里没有任何文件（空的快照不该被打包 —— 它说明不了任何事）");
    }
    if !spec::DATA_PRIVACY.contains(&input.privacy.trim()) {
        bail!(
            "privacy「{}」不合法（可选：{}）",
            input.privacy,
            spec::DATA_PRIVACY.join(" / ")
        );
    }

    std::fs::create_dir_all(dir).with_context(|| format!("建目录 {} 失败", dir.display()))?;
    let created = !dir.join(spec::MANIFEST).exists();
    for (doc, bytes) in &docs {
        let p = doc.path.trim();
        if !p.starts_with(spec::DATA_DIR) || p.contains("..") {
            bail!("快照文件「{p}」必须落在 {} 下（且不能跳出去）", spec::DATA_DIR);
        }
        let abs = dir.join(p);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, bytes).with_context(|| format!("写 {} 失败", abs.display()))?;
    }

    let slug = tpl::slug(&input.name);
    let snapshot_at = if input.snapshot_at.trim().is_empty() {
        bail!("缺 snapshot_at（快照必须说清「什么时候的数据」）");
    } else {
        input.snapshot_at.trim().to_string()
    };
    let pkg = HurPackage {
        spec: PKG_SPEC.to_string(),
        // 包内 kind 只决定 id 前缀与模板；**这份包是什么由 profile 说**
        // （目录 kind 由 `profile::registry_kinds` 映射，见 `publish::registry_kind`）。
        kind: "harness".to_string(),
        profile: Some(prof.to_string()),
        id: format!("H-{}-{}-{}", tpl::slug(if input.source.trim().is_empty() { "data" } else { &input.source }), slug, tpl::rand_hex(6)),
        name: input.name.trim().to_string(),
        version: if input.version.trim().is_empty() { "0.1.0".to_string() } else { input.version.trim().to_string() },
        short: tpl::acronym(&input.name),
        domain: if input.source_target.trim().is_empty() { "data".to_string() } else { tpl::slug(&input.source_target) },
        summary: input.summary.trim().to_string(),
        // 数据快照**不许有入口**（R12 会拦）—— 这里就不写
        entry: String::new(),
        runtime: String::new(),
        capabilities: vec![],
        deps: Deps::default(),
        permissions: Permissions::default(),
        publish: PublishInfo::default(),
        agent: None,
        data: Some(DataDecl {
            source: input.source.trim().to_string(),
            source_target: input.source_target.trim().to_string(),
            snapshot_at,
            privacy: input.privacy.trim().to_string(),
            license: input.license.trim().to_string(),
            payload: input.payload.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
            note: input.note.trim().to_string(),
            docs: docs.into_iter().map(|(d, _)| d).collect(),
        }),
        state: None,
        security: None,
        egress: None,
    };

    // 先按规范自己过一遍：不合规就不留下半成品（fail-closed）。
    // 顺序要紧：清单与 lock 都得先落盘，校验才读得到（lock 是"文件摘要"的唯一来源）。
    std::fs::write(dir.join(spec::MANIFEST), format!("{}\n", serde_json::to_string_pretty(&pkg)?))?;
    pack::build_lock(dir)?;
    let issues = spec::validate(&pkg, dir, spec::read_lock(dir).as_ref(), true);
    let errs: Vec<&spec::Issue> = issues.iter().filter(|i| i.level == spec::Level::Error).collect();
    if !errs.is_empty() {
        // 刚建的目录就删掉：留下一份不合规的包比报错更糟（下一个人会以为它是好的）
        if created {
            let _ = std::fs::remove_dir_all(dir);
        }
        bail!(
            "生成出来的快照包不合规（这是我们的 bug，不是你的数据的问题）：\n  {}",
            errs.iter().map(|i| format!("[{}] {}", i.rule, i.msg)).collect::<Vec<_>>().join("\n  ")
        );
    }
    Ok(pkg)
}

/// 读一份快照包（校验 + 返回清单与根目录）。
pub fn read(dir: &Path) -> Result<(HurPackage, PathBuf)> {
    let root = pack::find_root(dir)?;
    let pkg = spec::read_pkg(&root)?;
    Ok((pkg, root))
}

/// 快照包里某个文件的内容（已按 `data.docs[].path` 定位）。
pub fn doc_bytes(root: &Path, doc: &DataDoc) -> Result<Vec<u8>> {
    let p = root.join(doc.path.trim());
    std::fs::read(&p).with_context(|| format!("读快照文件 {} 失败", p.display()))
}

/// 人读一行：这份快照是什么、什么时候的、给谁看。
pub fn describe(pkg: &HurPackage) -> String {
    let prof = pkg.profile_name();
    match pkg.data.as_ref() {
        None => format!("{prof}（没有 data{{}} 声明）"),
        Some(d) => format!(
            "{prof} · 来源 {} · {} · 隐私 {} · {} 份{}{}",
            if d.source.trim().is_empty() { "（未声明）" } else { d.source.trim() },
            if d.snapshot_at.trim().is_empty() { "（未声明时刻）" } else { d.snapshot_at.trim() },
            if d.privacy.trim().is_empty() { "（未声明）" } else { d.privacy.trim() },
            d.docs.len(),
            if d.license.trim().is_empty() { " · ⚠️ 未声明许可".to_string() } else { format!(" · 许可 {}", d.license.trim()) },
            match d.payload.as_deref() {
                Some(p) => format!(" · payload {p}"),
                None => String::new(),
            }
        ),
    }
}

/// 规范的版本（写进包的时候顺手记一下是谁生成的）。
pub fn generator() -> String {
    format!("ncc/{VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ncc-seed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn input(prof: &str, privacy: &str) -> SeedInput {
        SeedInput {
            profile: prof.into(),
            name: "Handbook seed".into(),
            version: "2026.9.0".into(),
            summary: "手册快照".into(),
            source: "@team/handbook".into(),
            source_target: "node".into(),
            snapshot_at: "2026-09-27T10:00:00Z".into(),
            privacy: privacy.into(),
            license: "CC-BY-4.0".into(),
            payload: None,
            note: String::new(),
        }
    }

    fn one_doc() -> Vec<(DataDoc, Vec<u8>)> {
        vec![(
            DataDoc { path: "data/handbook.md".into(), slug: "handbook".into(), title: "手册".into(), kind: "doc".into(), format: "markdown".into(), ..Default::default() },
            "# 手册\n".as_bytes().to_vec(),
        )]
    }

    #[test]
    fn writes_a_package_that_passes_its_own_validation() {
        let d = tmp("ok");
        let pkg = write(&d, &input("kb-seed", "internal"), one_doc()).unwrap();
        assert_eq!(pkg.profile_name(), "kb-seed");
        assert!(pkg.entry.is_empty(), "数据快照不该有入口");
        assert!(d.join("hur.json").is_file() && d.join("hur.lock").is_file());
        assert!(d.join("data/handbook.md").is_file());
        // lock 里必须记上这份文件（摘要的唯一来源）
        let lock = spec::read_lock(&d).unwrap();
        assert!(lock.files.contains_key("data/handbook.md"), "{:?}", lock.files);
        // 再读回来：还是同一份
        let (again, root) = read(&d).unwrap();
        assert_eq!(again.data.as_ref().unwrap().docs.len(), 1);
        assert_eq!(doc_bytes(&root, &again.data.as_ref().unwrap().docs[0]).unwrap(), "# 手册\n".as_bytes());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn refuses_bad_input_before_writing_anything() {
        let d = tmp("bad");
        let mut i = input("kb-seed", "internal");
        i.privacy = "secret".into();
        assert!(write(&d, &i, one_doc()).unwrap_err().to_string().contains("privacy"));
        // 非数据类 profile 不能拿它当数据包
        let mut i2 = input("kb-seed", "internal");
        i2.profile = "plugin".into();
        assert!(write(&d, &i2, one_doc()).unwrap_err().to_string().contains("不是数据快照"));
        // 空快照没有意义
        let mut i3 = input("kb-seed", "internal");
        i3.snapshot_at = String::new();
        assert!(write(&d, &i3, one_doc()).unwrap_err().to_string().contains("snapshot_at"));
        assert!(write(&d, &input("kb-seed", "internal"), vec![]).unwrap_err().to_string().contains("没有任何文件"));
        // 快照文件跑出 data/ 一律拒绝（数据要能一眼认出来）
        let escaped = vec![(
            DataDoc { path: "src/sneaky.md".into(), ..Default::default() },
            b"x".to_vec(),
        )];
        assert!(write(&d, &input("kb-seed", "internal"), escaped).unwrap_err().to_string().contains("data/"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn full_payload_cannot_be_public() {
        let d = tmp("payload");
        let mut i = input("trace-set", "public");
        i.payload = Some("full".into());
        let err = write(&d, &i, one_doc()).unwrap_err().to_string();
        assert!(err.contains("不合规"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn describe_says_what_is_missing_instead_of_guessing() {
        let d = tmp("desc");
        let mut i = input("mem-seed", "private");
        i.license = String::new();
        let pkg = write(&d, &i, one_doc()).unwrap();
        let line = describe(&pkg);
        assert!(line.contains("mem-seed") && line.contains("@team/handbook"), "{line}");
        assert!(line.contains("未声明许可"), "{line}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
