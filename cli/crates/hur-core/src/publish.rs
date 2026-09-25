//! 发布到 registry 的请求体构造（CLI 与桌面端 GUI 共用，避免两处漂移）。
//!
//! 约定：
//! - 包内 `kind` → registry `kind`：`agent` → `hur`（Harness Use 制品）、`harness` → `harness`、`repo` → `hur`；可用 `--kind` 覆盖。
//! - `manifest` 里放**完整 hur.json**；`kind=harness` 时额外附 `harness.loader/entry`（registry 对该 kind 有契约校验）。
//! - `storage` 由调用方在上传完成后填（`{url, sha256, size}`）。

use serde_json::{json, Value};

use crate::spec::{HurPackage, PKG_SPEC};
use crate::tpl::slug;

/// 包内 kind → registry kind（`override_kind` 非空时优先）
pub fn registry_kind(pkg: &HurPackage, override_kind: &str) -> String {
    if !override_kind.trim().is_empty() {
        return override_kind.trim().to_string();
    }
    match pkg.kind.as_str() {
        "harness" => "harness".to_string(),
        _ => "hur".to_string(),
    }
}

/// 条目 slug：优先 `publish.slug`，否则按包名 slug 化
pub fn item_slug(pkg: &HurPackage) -> String {
    let s = pkg.publish.slug.trim();
    if s.is_empty() {
        slug(&pkg.name)
    } else {
        s.to_string()
    }
}

/// 组 `POST /api/registry` 的请求体（storage 待上传后回填）
pub fn build_item_body(
    pkg: &HurPackage,
    sha256: &str,
    size: u64,
    registry: &str,
    visibility: &str,
    override_kind: &str,
) -> Value {
    let kind = registry_kind(pkg, override_kind);
    let mut manifest = serde_json::to_value(pkg).unwrap_or(json!({}));
    if pkg.kind == "harness" {
        if let Some(obj) = manifest.as_object_mut() {
            obj.insert(
                "harness".into(),
                json!({
                    "loader": PKG_SPEC,
                    "entry": pkg.entry,
                    "schema": format!("{}/src/{}", pkg.id, slug(&pkg.name)),
                }),
            );
        }
    }
    let summary = if pkg.summary.trim().is_empty() {
        format!("{} · {}", pkg.kind, pkg.domain)
    } else {
        pkg.summary.trim().to_string()
    };
    json!({
        "kind": kind,
        "name": pkg.name,
        "slug": item_slug(pkg),
        "version": pkg.version,
        "summary": summary,
        "tags": pkg.capabilities,
        "status": "published",
        "visibility": if visibility.trim().is_empty() {
            if pkg.publish.visibility.trim().is_empty() { "public" } else { pkg.publish.visibility.trim() }
        } else { visibility.trim() },
        "manifest": manifest,
        "storage": { "url": "", "sha256": sha256, "size": size },
        "publish": { "registry": if registry.trim().is_empty() { pkg.publish.registry.trim() } else { registry.trim() } },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tpl::{build_package, InitInput};

    fn pkg(kind: &str, name: &str) -> HurPackage {
        build_package(&InitInput {
            kind: kind.into(),
            role: String::new(),
            name: name.into(),
            domain: "hotel".into(),
            short: "T".into(),
            version: "1.2.3".into(),
            summary: String::new(),
            registry: String::new(),
            namespace: String::new(),
        })
    }

    #[test]
    fn agent_maps_to_hur_and_harness_keeps_contract() {
        let a = pkg("agent", "Hotel Agent");
        let body = build_item_body(&a, "sha", 10, "", "public", "");
        assert_eq!(body["kind"], "hur");
        assert_eq!(body["slug"], "hotel-agent");
        assert_eq!(body["storage"]["sha256"], "sha");

        let h = pkg("harness", "Booking API");
        let hb = build_item_body(&h, "sha", 10, "", "public", "");
        assert_eq!(hb["kind"], "harness");
        assert_eq!(hb["manifest"]["harness"]["loader"], PKG_SPEC);
        assert_eq!(hb["manifest"]["harness"]["entry"], h.entry);
    }

    #[test]
    fn kind_override_wins() {
        let a = pkg("agent", "X");
        assert_eq!(build_item_body(&a, "s", 1, "", "public", "scaffold")["kind"], "scaffold");
    }
}
