//! 依赖关系检查：**声明（`hur.json`）↔ 锁（`hur.lock`）↔ 实际字节** 三方对账。
//!
//! 为什么单独一个模块：R4 只说"本地路径必须存在、远程依赖必须登记进锁"，但作者真正要问的是
//! 「我这个包现在能不能发 / 到底哪条依赖断了」。所以这里给的是**一份可读的对账单**，
//! 而不是又一条 yes/no 规则 —— `ncc hur dep` 与（将来）`hur dep` 共用它。
//!
//! 六种状态（各自都有可操作的 detail）：
//!
//! | 状态 | 含义 | 谁的问题 |
//! |---|---|---|
//! | `ok` | 本地依赖存在且哈希与锁一致；registry 依赖已锁定 | — |
//! | `unlocked` | 声明了这条依赖，但锁里没有 → 还没 `build` | 跑一次 `build` |
//! | `missing` | 本地路径不存在（不论锁里有没有） | 依赖被删/路径写错 |
//! | `drift` | 本地依赖存在，但哈希与锁不一致 | 依赖被改过 → 重新 `build` 并**重新签名** |
//! | `unresolved` | registry 引用仍是 `unresolved`（build 允许、pack/verify 会拒） | 发布前必须解析 |
//! | `stale` | 锁里有、声明里没有（残留） | 重新 `build` 收敛 |

use anyhow::Result;
use serde::Serialize;
use std::path::Path;

use crate::spec::{HurLock, HurPackage};

pub const DEP_SPEC: &str = "harness-use-dep-report/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepStatus {
    Ok,
    Unlocked,
    Missing,
    Drift,
    Unresolved,
    Stale,
}

impl DepStatus {
    /// 是否算"错误"（`missing` / `drift`）。`unresolved` 只是提醒：`build` 允许，
    /// 但 `pack` / `verify` 会当错 —— 所以它自己在 detail 里说清。
    pub fn is_error(self) -> bool {
        matches!(self, DepStatus::Missing | DepStatus::Drift)
    }
    pub fn label(self) -> &'static str {
        match self {
            DepStatus::Ok => "OK",
            DepStatus::Unlocked => "未锁定",
            DepStatus::Missing => "缺失",
            DepStatus::Drift => "漂移",
            DepStatus::Unresolved => "未解析",
            DepStatus::Stale => "残留",
        }
    }
    fn glyph(self) -> &'static str {
        match self {
            DepStatus::Ok => "✔",
            DepStatus::Unresolved | DepStatus::Unlocked => "·",
            DepStatus::Stale => "!",
            DepStatus::Missing | DepStatus::Drift => "✘",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DepItem {
    /// 依赖分组：harness / agent / skill / kb / mcp
    pub kind: String,
    /// 声明里的原始引用（`./x` / `@ns/slug` / `x.hur`）
    pub reference: String,
    /// local-file | local-dir | registry
    pub source: String,
    pub status: DepStatus,
    /// 锁里登记的哈希
    pub locked: Option<String>,
    /// 现场重算的哈希（本地依赖才有）
    pub actual: Option<String>,
    /// 人读说明（要能直接照着做）
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DepReport {
    pub spec: String,
    pub package: String,
    pub version: String,
    /// 有没有 `hur.lock`
    pub lock_present: bool,
    pub items: Vec<DepItem>,
    /// 计数（按状态）
    pub summary: std::collections::BTreeMap<String, usize>,
    /// 有错误的条数（missing + drift）
    pub errors: usize,
    /// 提醒条数（unresolved + unlocked + stale）
    pub warns: usize,
    /// 有没有 `.hur/lock.json`（工程层锁：团队工程同步用，和包锁不是一回事）
    pub project_lock: bool,
}

/// 对账：声明 → 锁 → 实际字节
pub fn inspect(dir: &Path, pkg: &HurPackage, lock: Option<&HurLock>) -> Result<DepReport> {
    let declared: Vec<(String, String)> = pkg.deps.iter().into_iter().map(|(k, r)| (k.to_string(), r.to_string())).collect();
    let mut items: Vec<DepItem> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();

    for (kind, reference) in &declared {
        let r = reference.trim();
        if r.is_empty() {
            continue;
        }
        seen.push((kind.clone(), r.to_string()));
        let locked = lock
            .and_then(|l| l.deps.get(kind))
            .and_then(|rows| rows.iter().find(|x| x.r#ref.trim() == r));
        let is_local = crate::spec::is_local_ref(r);
        let source = if is_local {
            if dir.join(r).is_dir() {
                "local-dir"
            } else {
                "local-file"
            }
        } else {
            "registry"
        };

        let actual = if is_local { crate::pack::local_dep_hash(dir, r)? } else { None };
        let locked_sha = locked.and_then(|x| x.sha256.clone());
        let locked_resolved = locked.map(|x| x.resolved.clone());

        let (status, detail) = if is_local {
            match (&actual, locked) {
                (None, _) => (
                    DepStatus::Missing,
                    format!("本地依赖「{r}」不存在（包内路径 {}）—— 补上文件/目录，或从 deps 里去掉", r.trim_start_matches("./")),
                ),
                (Some(_), None) => (DepStatus::Unlocked, format!("本地依赖「{r}」存在但还没进锁：跑一次 `build`")),
                (Some(a), Some(_)) => match &locked_sha {
                    Some(l) if l == a => (DepStatus::Ok, format!("本地依赖「{r}」与锁一致")),
                    Some(l) => (
                        DepStatus::Drift,
                        format!("本地依赖「{r}」被改过（锁 {} / 实际 {}）—— 重新 `build`，**并且重新签名**（签名覆盖锁）", short(l), short(a)),
                    ),
                    None => (DepStatus::Unlocked, format!("锁里「{r}」没有哈希（可能是手工改的锁）：重新 `build`")),
                },
            }
        } else {
            match locked {
                None => (DepStatus::Unlocked, format!("远程依赖「{r}」不在锁里：跑一次 `build`（会登记为 unresolved）")),
                Some(_) if locked_resolved.as_deref() == Some("unresolved") => (
                    DepStatus::Unresolved,
                    format!("远程依赖「{r}」尚未解析：`build` 允许，但 `pack` / `verify` 会拒绝 —— 发布前先解析到确切版本/摘要"),
                ),
                Some(_) => (DepStatus::Ok, format!("远程依赖「{r}」已锁定（{}）", locked_resolved.unwrap_or_default())),
            }
        };
        items.push(DepItem { kind: kind.clone(), reference: r.to_string(), source: source.to_string(), status, locked: locked_sha, actual, detail });
    }

    // 锁里有、声明里没有 → 残留（提醒）
    if let Some(l) = lock {
        for (kind, rows) in &l.deps {
            for row in rows {
                let key = (kind.clone(), row.r#ref.trim().to_string());
                if seen.iter().any(|s| s == &key) {
                    continue;
                }
                items.push(DepItem {
                    kind: kind.clone(),
                    reference: row.r#ref.clone(),
                    source: "lock-only".into(),
                    status: DepStatus::Stale,
                    locked: row.sha256.clone(),
                    actual: None,
                    detail: format!("锁里还留着「{}」（声明里已没有）：重新 `build` 收敛", row.r#ref),
                });
            }
        }
    }

    items.sort_by(|a, b| b.status.rank().cmp(&a.status.rank()).then(a.kind.cmp(&b.kind)).then(a.reference.cmp(&b.reference)));

    let mut summary = std::collections::BTreeMap::new();
    for it in &items {
        let k = format!("{:?}", it.status).to_lowercase();
        *summary.entry(k).or_insert(0) += 1;
    }
    let errors = items.iter().filter(|i| i.status.is_error()).count();
    let warns = items
        .iter()
        .filter(|i| matches!(i.status, DepStatus::Unresolved | DepStatus::Unlocked | DepStatus::Stale))
        .count();
    let project_lock = dir.join(crate::interop::PROJECT_DIR).join(crate::interop::PROJECT_LOCK).is_file();

    Ok(DepReport {
        spec: DEP_SPEC.into(),
        package: pkg.id.clone(),
        version: pkg.version.clone(),
        lock_present: lock.is_some(),
        items,
        summary,
        errors,
        warns,
        project_lock,
    })
}

impl DepStatus {
    /// 排序权重：问题在前
    fn rank(self) -> u8 {
        match self {
            DepStatus::Missing | DepStatus::Drift => 3,
            DepStatus::Unresolved => 2,
            DepStatus::Unlocked | DepStatus::Stale => 1,
            DepStatus::Ok => 0,
        }
    }
}

fn short(s: &str) -> String {
    if s.len() > 12 {
        format!("{}…", &s[..12])
    } else {
        s.to_string()
    }
}

/// 人读渲染
pub fn render(r: &DepReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("依赖对账 {} v{}（声明 ↔ 锁 ↔ 实际字节）\n", r.package, r.version));
    if !r.lock_present {
        out.push_str("  ⚠ 没有 hur.lock：先 `build` 才能对账（也才能签名/发布）\n");
    }
    if r.items.is_empty() {
        out.push_str("  这个包没有声明任何依赖\n");
    }
    for it in &r.items {
        out.push_str(&format!("  {} [{}] {:<9} {}\n", it.status.glyph(), it.kind, it.status.label(), it.reference));
        out.push_str(&format!("      {}\n", it.detail));
    }
    out.push_str(&format!(
        "\n合计：{} 条 · 错误 {} · 提醒 {}（未解析/未锁定/残留）\n",
        r.items.len(),
        r.errors,
        r.warns
    ));
    if r.project_lock {
        out.push_str("  另：工程层有 .hur/lock.json（团队工程同步用，与包锁无关）\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Deps, HurPackage, Permissions, PublishInfo};

    fn pkg_with(deps: Deps) -> HurPackage {
        HurPackage {
            egress: None,
            spec: crate::spec::PKG_SPEC.into(),
            kind: "agent".into(),
            id: "A-dep-test-000001".into(),
            name: "dep test".into(),
            version: "0.1.0".into(),
            short: String::new(),
            domain: String::new(),
            summary: String::new(),
            entry: "src/agent.ts".into(),
            runtime: String::new(),
            capabilities: vec![],
            deps,
            permissions: Permissions::default(),
            publish: PublishInfo::default(),
            agent: None,
            security: None,
        }
    }

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    /// 写清单（`build_lock` 从磁盘读 `hur.json`，不看你内存里的那个对象）
    fn write_manifest(dir: &Path, pkg: &HurPackage) {
        std::fs::write(dir.join(crate::spec::MANIFEST), serde_json::to_string_pretty(pkg).unwrap()).unwrap();
    }

    fn tmp() -> Tmp {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("hur-dep-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    #[test]
    fn declares_locked_missing_and_drift() {
        let t = tmp();
        let dir = &t.0;
        // 一个存在的本地依赖 + 一个不存在的
        std::fs::write(dir.join("dep.hur"), b"dep-bytes").unwrap();
        let deps = Deps { agent: vec!["./dep.hur".into(), "./gone.hur".into()], harness: vec!["@ns/slug".into()], ..Default::default() };
        let pkg = pkg_with(deps.clone());
        write_manifest(dir, &pkg);

        let lock = crate::pack::build_lock(dir).unwrap();
        let rep = inspect(dir, &pkg, Some(&lock)).unwrap();
        // 本地存在的 → ok；不存在的 → missing；registry → unresolved
        let by_ref = |r: &str| rep.items.iter().find(|i| i.reference == r).unwrap().status.clone();
        assert_eq!(by_ref("./dep.hur"), DepStatus::Ok);
        assert_eq!(by_ref("./gone.hur"), DepStatus::Missing);
        assert_eq!(by_ref("@ns/slug"), DepStatus::Unresolved);
        assert_eq!(rep.errors, 1, "{:?}", rep.items);

        // 改字节 → drift
        std::fs::write(dir.join("dep.hur"), b"dep-bytes-CHANGED").unwrap();
        let rep2 = inspect(dir, &pkg, Some(&lock)).unwrap();
        assert_eq!(rep2.items.iter().find(|i| i.reference == "./dep.hur").unwrap().status, DepStatus::Drift);
        assert!(rep2.items.iter().find(|i| i.reference == "./dep.hur").unwrap().detail.contains("重新签名"));

        // 重新 build 后 → 有锁但"声明里没有"的依赖会变 stale
        std::fs::write(dir.join("gone.hur"), b"back").unwrap();
        let lock2 = crate::pack::build_lock(dir).unwrap();
        let pkg2 = pkg_with(Deps { agent: vec!["./dep.hur".into()], ..Default::default() });
        let rep3 = inspect(dir, &pkg2, Some(&lock2)).unwrap();
        assert!(rep3.items.iter().any(|i| i.status == DepStatus::Stale && i.reference == "./gone.hur"), "{:?}", rep3.items);
        assert_eq!(rep3.errors, 0);
    }

    #[test]
    fn missing_lock_reports_unlocked() {
        let t = tmp();
        let dir = &t.0;
        std::fs::write(dir.join("dep.hur"), b"x").unwrap();
        let pkg = pkg_with(Deps { agent: vec!["./dep.hur".into()], ..Default::default() });
        write_manifest(dir, &pkg);
        let rep = inspect(dir, &pkg, None).unwrap();
        assert!(!rep.lock_present);
        assert_eq!(rep.items[0].status, DepStatus::Unlocked);
        assert!(rep.items[0].detail.contains("build"));
        assert_eq!(rep.errors, 0, "没锁只是提醒，不是错误");
    }

    #[test]
    fn dir_dep_uses_same_hash_as_lock() {
        let t = tmp();
        let dir = &t.0;
        std::fs::create_dir_all(dir.join("vendor/x/src")).unwrap();
        std::fs::write(dir.join("vendor/x/src/a.ts"), b"a").unwrap();
        let pkg = pkg_with(Deps { harness: vec!["./vendor/x".into()], ..Default::default() });
        write_manifest(dir, &pkg);
        let lock = crate::pack::build_lock(dir).unwrap();
        let rep = inspect(dir, &pkg, Some(&lock)).unwrap();
        assert_eq!(rep.items[0].status, DepStatus::Ok);
        assert_eq!(rep.items[0].locked, rep.items[0].actual, "锁与现场必须同一算法");
        // 改目录里的文件 → drift（目录依赖也能发现）
        std::fs::write(dir.join("vendor/x/src/a.ts"), b"b").unwrap();
        let rep2 = inspect(dir, &pkg, Some(&lock)).unwrap();
        assert_eq!(rep2.items[0].status, DepStatus::Drift);
    }
}
