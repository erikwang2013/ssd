// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 小盾核心：编排、RPC 契约类型与处理器。

pub mod api;
pub mod export;
pub mod fs_read;
pub mod handlers;
pub mod notify;
pub mod scan_task;
pub mod store;

/// 扫描 worker 内部（仅 `crate::scan_task` 使用，公开 API 走 `scan_task`）。
mod scan_worker;

#[cfg(test)]
pub(crate) mod testutil;
