//! AI layer: client cho các block AI (ai_decide/ai_check/ai_write/...). Structured
//! output qua Anthropic tool-use; OpenAI-compatible (openai/ollama/gemini) qua
//! response_format json. Fallback khi không có provider được xử lý ở executor.
//!
//! Khoá API mã hoá at-rest nên cấu hình provider nạp từ ngoài (xem store/settings);
//! ở đây chỉ là client thuần.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// HTTP client cho mọi lời gọi LLM. PHẢI có timeout: một request treo (mạng chậm,
/// endpoint không phản hồi, model sai) nếu không sẽ khiến "Test"/block AI treo vô
/// hạn, không báo gì cho người dùng. Timeout → trả AiError rõ ràng để UI hiện lỗi.
fn build_http_client() -> reqwest::Client {
  reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(15))
    .timeout(Duration::from_secs(60))
    .build()
    .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
  Anthropic,
  Openai,
  Gemini,
  Ollama,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
  pub provider: Provider,
  pub model: String,
  #[serde(default)]
  pub api_key: String,
  /// Cho Ollama/Gemini-compat hoặc proxy. Mặc định theo provider.
  #[serde(default)]
  pub base_url: Option<String>,
  #[serde(default = "default_max_tokens")]
  pub max_tokens: u32,
  #[serde(default = "default_temperature")]
  pub temperature: f32,
}

fn default_max_tokens() -> u32 {
  1024
}
fn default_temperature() -> f32 {
  0.3
}

impl AiProviderConfig {
  /// Mặc định rẻ/nhanh cho các block AI.
  pub fn anthropic_haiku(api_key: impl Into<String>) -> Self {
    Self {
      provider: Provider::Anthropic,
      model: "claude-haiku-4-5".to_string(),
      api_key: api_key.into(),
      base_url: None,
      max_tokens: default_max_tokens(),
      temperature: default_temperature(),
    }
  }
}

#[derive(Debug)]
pub struct AiError(pub String);
impl std::fmt::Display for AiError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}
impl std::error::Error for AiError {}

pub struct AiRequest {
  pub system: String,
  pub prompt: String,
  /// Nếu Some → ép structured JSON theo schema; None → trả text.
  pub schema: Option<Value>,
}

#[derive(Debug, Default)]
pub struct AiResult {
  pub json: Option<Value>,
  pub text: Option<String>,
  pub input_tokens: u64,
  pub output_tokens: u64,
}

#[async_trait::async_trait]
pub trait AiClient: Send + Sync {
  async fn run(&self, req: AiRequest) -> Result<AiResult, AiError>;
}

// ---------- Anthropic ----------

/// Body builder thuần (test được không cần mạng). Structured output dùng tool-use:
/// một tool tên "emit" với input_schema = schema, ép `tool_choice`.
pub fn build_anthropic_body(req: &AiRequest, cfg: &AiProviderConfig) -> Value {
  let mut body = json!({
    "model": cfg.model,
    "max_tokens": cfg.max_tokens,
    "temperature": cfg.temperature,
    "system": req.system,
    "messages": [{ "role": "user", "content": req.prompt }],
  });
  if let Some(schema) = &req.schema {
    body["tools"] = json!([{
      "name": "emit",
      "description": "Return the structured result.",
      "input_schema": schema,
    }]);
    body["tool_choice"] = json!({ "type": "tool", "name": "emit" });
  }
  body
}

pub struct AnthropicClient {
  cfg: AiProviderConfig,
  http: reqwest::Client,
}

impl AnthropicClient {
  pub fn new(cfg: AiProviderConfig) -> Self {
    Self {
      cfg,
      http: build_http_client(),
    }
  }
}

#[async_trait::async_trait]
impl AiClient for AnthropicClient {
  async fn run(&self, req: AiRequest) -> Result<AiResult, AiError> {
    let url = self
      .cfg
      .base_url
      .clone()
      .unwrap_or_else(|| "https://api.anthropic.com".to_string());
    let body = build_anthropic_body(&req, &self.cfg);
    let resp = self
      .http
      .post(format!("{url}/v1/messages"))
      .header("x-api-key", &self.cfg.api_key)
      .header("anthropic-version", "2023-06-01")
      .header("content-type", "application/json")
      .json(&body)
      .send()
      .await
      .map_err(|e| AiError(format!("request failed: {e}")))?;
    let status = resp.status();
    let v: Value = resp
      .json()
      .await
      .map_err(|e| AiError(format!("bad response: {e}")))?;
    if !status.is_success() {
      return Err(AiError(format!("anthropic error {status}: {v}")));
    }
    let usage = v.get("usage");
    let input_tokens = usage
      .and_then(|u| u.get("input_tokens"))
      .and_then(|t| t.as_u64())
      .unwrap_or(0);
    let output_tokens = usage
      .and_then(|u| u.get("output_tokens"))
      .and_then(|t| t.as_u64())
      .unwrap_or(0);

    let mut result = AiResult {
      input_tokens,
      output_tokens,
      ..Default::default()
    };
    // content[] — tìm tool_use (structured) hoặc text.
    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
      for b in blocks {
        match b.get("type").and_then(|t| t.as_str()) {
          Some("tool_use") => {
            result.json = b.get("input").cloned();
          }
          Some("text") => {
            let t = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
            result.text = Some(match result.text.take() {
              Some(prev) => prev + t,
              None => t.to_string(),
            });
          }
          _ => {}
        }
      }
    }
    Ok(result)
  }
}

// ---------- OpenAI-compatible (openai / ollama / gemini-compat) ----------

pub struct OpenAiCompatClient {
  cfg: AiProviderConfig,
  http: reqwest::Client,
}

impl OpenAiCompatClient {
  pub fn new(cfg: AiProviderConfig) -> Self {
    Self {
      cfg,
      http: build_http_client(),
    }
  }
  fn base(&self) -> String {
    self
      .cfg
      .base_url
      .clone()
      .unwrap_or_else(|| match self.cfg.provider {
        Provider::Ollama => "http://127.0.0.1:11434/v1".to_string(),
        Provider::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
        _ => "https://api.openai.com/v1".to_string(),
      })
  }
}

#[async_trait::async_trait]
impl AiClient for OpenAiCompatClient {
  async fn run(&self, req: AiRequest) -> Result<AiResult, AiError> {
    let mut body = json!({
      "model": self.cfg.model,
      "max_tokens": self.cfg.max_tokens,
      "temperature": self.cfg.temperature,
      "messages": [
        { "role": "system", "content": req.system },
        { "role": "user", "content": req.prompt },
      ],
    });
    if req.schema.is_some() {
      body["response_format"] = json!({ "type": "json_object" });
    }
    let resp = self
      .http
      .post(format!("{}/chat/completions", self.base()))
      .header("authorization", format!("Bearer {}", self.cfg.api_key))
      .header("content-type", "application/json")
      .json(&body)
      .send()
      .await
      .map_err(|e| AiError(format!("request failed: {e}")))?;
    let status = resp.status();
    let v: Value = resp
      .json()
      .await
      .map_err(|e| AiError(format!("bad response: {e}")))?;
    if !status.is_success() {
      return Err(AiError(format!("openai-compat error {status}: {v}")));
    }
    let content = v
      .pointer("/choices/0/message/content")
      .and_then(|c| c.as_str())
      .unwrap_or("")
      .to_string();
    let input_tokens = v
      .pointer("/usage/prompt_tokens")
      .and_then(|t| t.as_u64())
      .unwrap_or(0);
    let output_tokens = v
      .pointer("/usage/completion_tokens")
      .and_then(|t| t.as_u64())
      .unwrap_or(0);
    let json = if req.schema.is_some() {
      serde_json::from_str::<Value>(&content).ok()
    } else {
      None
    };
    Ok(AiResult {
      json,
      text: Some(content),
      input_tokens,
      output_tokens,
    })
  }
}

/// Factory: dựng client theo provider.
pub fn make_client(cfg: AiProviderConfig) -> Box<dyn AiClient> {
  match cfg.provider {
    Provider::Anthropic => Box::new(AnthropicClient::new(cfg)),
    _ => Box::new(OpenAiCompatClient::new(cfg)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn anthropic_body_structured_uses_tool_choice() {
    let cfg = AiProviderConfig::anthropic_haiku("k");
    let req = AiRequest {
      system: "sys".into(),
      prompt: "hi".into(),
      schema: Some(json!({ "type": "object", "properties": { "x": { "type": "string" } } })),
    };
    let body = build_anthropic_body(&req, &cfg);
    assert_eq!(body["model"], "claude-haiku-4-5");
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], "emit");
    assert_eq!(body["tools"][0]["name"], "emit");
    assert!(body["tools"][0]["input_schema"].is_object());
  }

  #[test]
  fn anthropic_body_text_has_no_tools() {
    let cfg = AiProviderConfig::anthropic_haiku("k");
    let req = AiRequest {
      system: "s".into(),
      prompt: "p".into(),
      schema: None,
    };
    let body = build_anthropic_body(&req, &cfg);
    assert!(body.get("tools").is_none());
    assert_eq!(body["messages"][0]["role"], "user");
  }
}
