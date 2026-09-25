//! 域名白名单判定（与 `hur-core::spec::host_allowed` 同语义，独立实现以便沙箱单飞）。
//!
//! 规则：`localhost` / `127.0.0.1` / `::1` 始终允许（本机开发）；其余必须显式声明，
//! 支持 `*.example.com` 通配（匹配后缀本身与任意子域）。

pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    let h = host.trim().to_ascii_lowercase();
    if matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return true;
    }
    allow.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        if a.is_empty() {
            return false;
        }
        match a.strip_prefix("*.") {
            Some(suffix) => h == suffix || h.ends_with(&format!(".{suffix}")),
            None => h == a,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_is_always_allowed_but_others_are_not() {
        let allow = vec!["api.hotel.example.com".to_string(), "*.trusted.dev".to_string()];
        assert!(host_allowed("localhost", &[]));
        assert!(host_allowed("127.0.0.1", &[]));
        assert!(host_allowed("api.hotel.example.com", &allow));
        // host_of() 负责剥端口；这里传进来带端口时按"不匹配"处理（宁可严一点）
        assert!(!host_allowed("api.hotel.example.com:8443", &allow));
        assert!(host_allowed("trusted.dev", &allow));
        assert!(host_allowed("a.b.trusted.dev", &allow));
        assert!(!host_allowed("evil.trusted.dev.evil.com", &allow));
        assert!(!host_allowed("evil.example.com", &allow));
        assert!(!host_allowed("api.hotel.example.com", &[]));
    }
}
