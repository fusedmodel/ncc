//! `hur-core` —— HUR 制品的可复用实现（规范 / 校验 / 打包 / registry / 安装）。
//!
//! 设计：**一套实现，两个前端**。
//! - CLI：`src/main.rs`（bin `hur`，给人和脚本用）
//! - 内置到桌面端 Agent：`agent/src-tauri` 直接 `use hur_core::*`，把同样的能力
//!   暴露成 Tauri 命令（GUI 的校验 / 打包 / 发布 / 安装与 CLI 完全同源，不再有第二份实现）。
//!
//! 边界：`pack` / `verify` / `build` / `sign` **全程离线**（签名不联网、不上传私钥）；
//! 只有 `registry` 与 `install`（远端）需要网络。
// 默认 128 层不够 `schema.rs` 里那份 JSON Schema 字面量（properties 嵌套很深，
// `json!` 是递归展开的宏）。抬高上限比把 schema 拆成十几个中间变量更好读。
#![recursion_limit = "512"]

pub mod cfg;
pub mod datapack;
pub mod dep;
pub mod huf;
pub mod install;
pub mod interop;
pub mod mcp;
pub mod need;
pub mod pack;
pub mod policy;
pub mod profile;
pub mod publish;
pub mod registry;
pub mod schema;
pub mod sign;
pub mod spec;
pub mod spec_kit;
pub mod tpl;

/// 规范标识（对外可见）
pub const SPEC: &str = spec::PKG_SPEC;

/// 版本（CLI 与宿主都用它对外报告）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
