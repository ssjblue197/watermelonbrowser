//! Data model cho Scenario Automation. "JSON ở biên, typed ở lõi": block lưu/truyền
//! dạng JSON linh hoạt (`params: Value`), deserialize sang struct typed lúc thực thi.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
  #[default]
  Stop,
  Skip,
  Retry,
}

/// Trần an toàn chống runaway (loop vô hạn / chạy quá lâu).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCaps {
  pub max_steps: u32,
  pub max_loop_iterations: u32,
  pub max_total_secs: u64,
}

impl Default for RunCaps {
  fn default() -> Self {
    Self {
      max_steps: 2000,
      max_loop_iterations: 1000,
      max_total_secs: 3600,
    }
  }
}

/// Một block trong cây kịch bản. `children`/`branch_else` cho nesting (Loop/Condition).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
  /// Tùy chọn khi gửi JSON; mặc định "" nếu thiếu (dùng cho step log).
  #[serde(default)]
  pub id: String,
  #[serde(rename = "type")]
  pub block_type: String,
  #[serde(default)]
  pub label: Option<String>,
  /// Cấu hình block (JSON linh hoạt). Strings bên trong được interpolate `{{var}}`.
  #[serde(default)]
  pub params: serde_json::Value,
  /// Block AI dùng AI path khi có provider; false → luôn dùng fallback.
  #[serde(default)]
  pub ai_enabled: bool,
  /// Children cho Loop / nhánh `if` của Condition.
  #[serde(default)]
  pub children: Vec<Block>,
  /// Nhánh `else` của Condition.
  #[serde(default)]
  pub branch_else: Option<Vec<Block>>,
  /// Override on_error mức scenario.
  #[serde(default)]
  pub on_error: Option<OnError>,
  /// Outbound action (send/post/reply): nếu true → KHÔNG gọi action thật, chỉ log.
  /// Mặc định false → tự động đầy đủ.
  #[serde(default)]
  pub dry_run: bool,
  #[serde(default)]
  pub disabled: bool,
}

impl Block {
  /// Helper tạo nhanh block trong test/code.
  pub fn new(block_type: &str, params: serde_json::Value) -> Self {
    Self {
      id: block_type.to_string(),
      block_type: block_type.to_string(),
      label: None,
      params,
      ai_enabled: false,
      children: Vec::new(),
      branch_else: None,
      on_error: None,
      dry_run: false,
      disabled: false,
    }
  }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AiMode {
  Ai,
  NoAi,
  #[default]
  Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
  pub id: String,
  pub name: String,
  #[serde(default)]
  pub description: Option<String>,
  #[serde(default)]
  pub ai_mode: AiMode,
  #[serde(default)]
  pub on_error: OnError,
  #[serde(default)]
  pub blocks: Vec<Block>,
  #[serde(default)]
  pub caps: RunCaps,
}

/// Log một bước thực thi (sẽ persist vào SQLite ở bản đầy đủ).
#[derive(Debug, Clone, Serialize)]
pub struct StepLog {
  pub block_id: String,
  pub block_type: String,
  /// "ok" | "skipped" | "failed" | "retried" | "dry_run"
  pub status: String,
  pub duration_ms: u128,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}
