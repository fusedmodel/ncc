//! `hur-core` —— HUR 制品的可复用实现（规范 / 校验 / 打包 / registry / 安装）。
//!
//! 设计：**一套实现，两个前端**。
//! - CLI：`src/main.rs`（bin `hur`，给人和脚本用）
//! - 内置到桌面端 Agent：`agent/src-tauri` 直接 `use hur_core::*`，把同样的能力
//!   暴露成 Tauri 命令（GUI 的校验 / 打包 / 发布 / 安装与 CLI 完全同源，不再有第二份实现）。
//!
//! 边界：`pack` / `verify` / `build` / `sign` **全程离线**（签名不联网、不上传私钥）；
//! 只有 `registry` 与 `install`（远端）需要网络。

pub mod cfg;
pub mod dep;
pub mod install;
pub mod interop;
pub mod mcp;
pub mod pack;
pub mod policy;
pub mod publish;
pub mod registry;
pub mod sign;
pub mod spec;
pub mod tpl;

/// 规范标识（对外可见）
pub const SPEC: &str = spec::PKG_SPEC;

/// 版本（CLI 与宿主都用它对外报告）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
