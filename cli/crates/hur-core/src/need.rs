//! 需求表达式：`cpu>=4,mem>=16G,tool:docker|podman` —— **一处定义，多处使用**。
//!
//! 谁在用它：
//!
//! * **任务档位**（`compute::TASKS`）：`build-rust` 的 `needs` 就是一组这样的表达式；
//! * **算力画像的评估**（`ncc profile node show` / `fit`）：这台机器满不满足；
//! * **脚手架的前置条件**（`SCAFFOLD.md` 的 `requires`，规则 S8）：这份脚手架要什么；
//! * **平台的粗筛**（`/api/compute/fit`）：平台侧有一份**同语义的镜像**（它不能依赖本 crate），
//!   两侧用同一组用例各自钉一份单测 —— 改这一侧要同步改那一侧。
//!
//! 语法故意很小：`字段 操作符 数值`，或 `tool:名`（存在性）。写错的表达**当场报错**，
//! 不"猜一个意思继续跑" —— 猜错的代价是把任务派到跑不动的机器上。

use anyhow::{anyhow, bail, Result};

/// 一条需求。字段：`cpu` / `mem` / `disk` / `gpu` / `gpumem` / `service`（个数）/
/// `tool:<名>`（可用竖线二选一）/ `service:<名>` / `tag:<名>`。
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub field: String,
    pub op: String,
    pub value: f64,
    /// 原样保留（报错与展示时用原话，别让人看到被改写过的需求）
    pub raw: String,
}

/// 一条需求的判定结果。
#[derive(Debug, Clone)]
pub struct Verdict {
    pub need: String,
    pub ok: bool,
    /// 不满足时：现在的值（"这台机器现在是多少"）
    pub actual: String,
    /// 不满足时：差什么（"补什么才够"）
    pub missing: String,
}

/// 一条画像/工程能提供的事实。字段语义由实现方给（画像按 MB/GB，脚手架按声明）。
pub trait Facts {
    /// 数值型字段的当前值（未知字段给 0）。
    fn number(&self, field: &str) -> f64;
    /// 存在性：`kind` 是 `tool` / `service` / `tag` 之一。
    fn has(&self, kind: &str, name: &str) -> bool;
    /// 人读的"现在是多少"（不满足时展示）。
    fn describe(&self, field: &str, value: f64) -> String {
        let unit = match field {
            "mem" | "gpumem" => "MB",
            "disk" => "GB",
            _ => "",
        };
        format!("{}{}", fmt_num(value), unit)
    }
}

/// 解析需求表达式：逗号分隔，空白的忽略。
pub fn parse_need(expr: &str) -> Result<Vec<Need>> {
    let mut out = Vec::new();
    for raw in expr.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        out.push(parse_one(raw)?);
    }
    if out.is_empty() {
        bail!("需求表达式是空的（例：cpu>=4,mem>=16G,tool:docker）");
    }
    Ok(out)
}

/// 单条（`parse_need` 的一步；脚手架只判单条时也用它）。
pub fn parse_one(raw: &str) -> Result<Need> {
    if let Some((k, v)) = raw.split_once(':') {
        let k = k.trim().to_lowercase();
        let v = v.trim();
        if v.is_empty() {
            bail!("需求「{raw}」少了名字（例：tool:docker）");
        }
        if !matches!(k.as_str(), "tool" | "service" | "tag") {
            bail!("需求「{raw}」的字段不认识（只支持 tool: / service: / tag: 前缀，或 cpu>=N / mem>=2G 这类）");
        }
        return Ok(Need { field: format!("{k}:{v}"), op: ">=".into(), value: 1.0, raw: raw.to_string() });
    }
    for op in [">=", "<=", ">", "<", "="] {
        if let Some((f, v)) = raw.split_once(op) {
            let field = f.trim().to_lowercase();
            if !matches!(field.as_str(), "cpu" | "mem" | "disk" | "gpu" | "gpumem" | "service") {
                bail!(
                    "需求「{raw}」的字段「{field}」不认识（支持 cpu / mem / disk / gpu / gpumem / service，或 tool:名）"
                );
            }
            let value = parse_amount(v.trim(), &field)
                .ok_or_else(|| anyhow!("需求「{raw}」的数值看不懂（例：2 或 16G）"))?;
            return Ok(Need { field, op: op.to_string(), value, raw: raw.to_string() });
        }
    }
    bail!("需求「{raw}」看不懂（例：cpu>=4 / mem>=16G / disk>=100G / tool:docker）")
}

/// `2` / `2G` / `512M` / `1T` → 数值。内存与显存统一成 MB，磁盘统一成 GB。
pub fn parse_amount(s: &str, field: &str) -> Option<f64> {
    let up = s.to_ascii_uppercase();
    let (num, unit) = up
        .strip_suffix('T')
        .map(|n| (n, "T"))
        .or_else(|| up.strip_suffix('G').map(|n| (n, "G")))
        .or_else(|| up.strip_suffix('M').map(|n| (n, "M")))
        .unwrap_or((up.as_str(), ""));
    let n: f64 = num.trim().parse().ok()?;
    if n < 0.0 {
        return None;
    }
    let is_mem = matches!(field, "mem" | "gpumem");
    Some(n
        * match (unit, is_mem) {
            ("T", true) => 1024.0 * 1024.0,
            ("G", true) => 1024.0,
            ("", true) => 1.0,
            ("M", true) => 1.0,
            ("T", false) => 1024.0,
            ("G", false) => 1.0,
            ("M", false) => 1.0 / 1024.0,
            _ => 1.0,
        })
}

/// 评估一组需求。
pub fn evaluate<F: Facts + ?Sized>(f: &F, needs: &[Need]) -> Vec<Verdict> {
    needs.iter().map(|n| check_one(f, n)).collect()
}

pub fn all_ok(vs: &[Verdict]) -> bool {
    vs.iter().all(|v| v.ok)
}

fn check_one<F: Facts + ?Sized>(f: &F, n: &Need) -> Verdict {
    let raw = n.raw.clone();
    if let Some(name) = n.field.strip_prefix("tool:") {
        // `tool:docker|podman`：竖线表示"二者之一"
        let hit = name.split('|').map(|s| s.trim()).find(|alt| f.has("tool", alt));
        return match hit {
            Some(alt) => Verdict { need: raw, ok: true, actual: format!("已装 {alt}"), missing: String::new() },
            None => Verdict {
                need: raw,
                ok: false,
                actual: "未安装".into(),
                missing: format!("装一个 {name}（或用带它的节点）"),
            },
        };
    }
    if let Some(name) = n.field.strip_prefix("service:") {
        return if f.has("service", name) {
            Verdict { need: raw, ok: true, actual: format!("可达 {name}"), missing: String::new() }
        } else {
            Verdict {
                need: raw,
                ok: false,
                actual: "画像里没有这个服务面".into(),
                missing: format!("确认本机能访问 {name}，再 `ncc profile node scan` 一次"),
            }
        };
    }
    if let Some(name) = n.field.strip_prefix("tag:") {
        return if f.has("tag", name) {
            Verdict { need: raw, ok: true, actual: format!("有 {name}"), missing: String::new() }
        } else {
            Verdict { need: raw, ok: false, actual: "没有这个标签".into(), missing: format!("需要标签 {name}") }
        };
    }
    let actual = f.number(&n.field);
    let ok = compare(actual, &n.op, n.value);
    let unit = match n.field.as_str() {
        "mem" | "gpumem" => "MB",
        "disk" => "GB",
        _ => "",
    };
    Verdict {
        need: raw,
        ok,
        actual: f.describe(&n.field, actual),
        missing: if ok {
            String::new()
        } else {
            format!("需要 {} {} {}{}", n.field, n.op, fmt_num(n.value), unit)
        },
    }
}

pub fn compare(actual: f64, op: &str, want: f64) -> bool {
    match op {
        ">=" => actual >= want,
        ">" => actual > want,
        "<=" => actual <= want,
        "<" => actual < want,
        "=" => (actual - want).abs() < f64::EPSILON,
        _ => false,
    }
}

pub fn fmt_num(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        cpu: f64,
        mem: f64,
        disk: f64,
        gpu: f64,
        gpumem: f64,
        tools: Vec<&'static str>,
        services: Vec<&'static str>,
        tags: Vec<&'static str>,
    }

    impl Facts for Fake {
        fn number(&self, field: &str) -> f64 {
            match field {
                "cpu" => self.cpu,
                "mem" => self.mem,
                "disk" => self.disk,
                "gpu" => self.gpu,
                "gpumem" => self.gpumem,
                "service" => self.services.len() as f64,
                _ => 0.0,
            }
        }
        fn has(&self, kind: &str, name: &str) -> bool {
            match kind {
                "tool" => self.tools.iter().any(|t| *t == name),
                "service" => self.services.iter().any(|t| *t == name),
                "tag" => self.tags.iter().any(|t| *t == name),
                _ => false,
            }
        }
    }

    fn fake() -> Fake {
        Fake {
            cpu: 8.0,
            mem: 16384.0,
            disk: 200.0,
            gpu: 1.0,
            gpumem: 12288.0,
            tools: vec!["cargo", "docker"],
            services: vec!["office"],
            tags: vec!["build-rust"],
        }
    }

    /// **需求用例**（与平台侧 `store::compute::tests::需求用例` 同一组：两侧语义必须一致）
    #[test]
    fn 需求用例() {
        let f = fake();
        let ok = |expr: &str| all_ok(&evaluate(&f, &parse_need(expr).unwrap()));
        assert!(ok("cpu>=8"));
        assert!(ok("cpu>=4,mem>=16G"));
        assert!(ok("mem>=16384"));
        assert!(!ok("mem>=32G"), "32G > 16G");
        assert!(ok("disk>=100G"));
        assert!(!ok("disk>=500G"));
        assert!(ok("gpu>=1"));
        assert!(!ok("gpu>=2"));
        assert!(ok("gpumem>=8G"), "12G 显存 >= 8G");
        assert!(!ok("gpumem>=16G"));
        assert!(ok("tool:cargo"));
        assert!(ok("tool:docker|podman"), "竖线是二选一");
        assert!(!ok("tool:podman"));
        assert!(ok("service:office"));
        assert!(!ok("service:nowhere"));
        assert!(ok("service>=1"));
        assert!(ok("tag:build-rust"));
        assert!(!ok("tag:gpu"));
        assert!(parse_need("cores>=4").is_err());
        assert!(parse_need("cpu").is_err());
        assert!(parse_need("tool:").is_err());
    }

    #[test]
    fn 单位与缺什么() {
        let f = fake();
        assert_eq!(parse_need("mem>=16G").unwrap()[0].value, 16384.0);
        assert_eq!(parse_need("mem>=512M").unwrap()[0].value, 512.0);
        assert_eq!(parse_need("mem>=1T").unwrap()[0].value, 1024.0 * 1024.0);
        assert_eq!(parse_need("disk>=100G").unwrap()[0].value, 100.0);
        let v = evaluate(&f, &parse_need("mem>=32G").unwrap());
        assert!(!v[0].ok);
        assert!(v[0].missing.contains("32768MB"), "{}", v[0].missing);
        assert_eq!(v[0].actual, "16384MB");
    }
}
