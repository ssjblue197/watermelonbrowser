//! Scenario Automation — MVP vertical slice (No-AI).
//!
//! Engine sống ở Rust backend, thực thi cây block qua trait `ActionExecutor`
//! (impl thật bọc `McpServer::dispatch_tool_call`). Xem
//! `docs/scenario-automation-design.md` cho thiết kế đầy đủ.

pub mod actions;
pub mod ai;
pub mod executor;
pub mod interpolate;
pub mod manager;
pub mod model;
pub mod scheduler;
pub mod store;
