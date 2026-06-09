//! Lớp thực thi action — decouple engine khỏi browser/MCP để test được.
//!
//! Engine chỉ phụ thuộc trait `ActionExecutor`. Impl thật (`McpActionExecutor`) bọc
//! `McpServer::dispatch_tool_call`; test dùng mock.

use serde_json::Value;

#[derive(Debug)]
pub struct ActionError(pub String);

impl std::fmt::Display for ActionError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl std::error::Error for ActionError {}

#[async_trait::async_trait]
pub trait ActionExecutor: Send + Sync {
  /// Gọi một MCP tool trên `profile_id`. `args` là JSON arguments; impl tự chèn
  /// `profile_id` vào args trước khi dispatch.
  async fn call(&self, profile_id: &str, tool: &str, args: Value) -> Result<Value, ActionError>;
}

/// Impl thật: định tuyến vào logic tool nội bộ (không qua HTTP).
pub struct McpActionExecutor;

#[async_trait::async_trait]
impl ActionExecutor for McpActionExecutor {
  async fn call(
    &self,
    profile_id: &str,
    tool: &str,
    mut args: Value,
  ) -> Result<Value, ActionError> {
    if !args.is_object() {
      args = serde_json::json!({});
    }
    if let Some(obj) = args.as_object_mut() {
      obj.insert(
        "profile_id".to_string(),
        Value::String(profile_id.to_string()),
      );
    }
    crate::mcp_server::McpServer::instance()
      .dispatch_tool_call(tool, &args)
      .await
      // McpError fields là private — dùng Debug để giữ thông điệp lỗi.
      .map_err(|e| ActionError(format!("{e:?}")))
  }
}
