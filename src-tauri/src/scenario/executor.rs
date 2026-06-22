//! Execution engine: duyệt cây block (đệ quy qua Box::pin), xử lý Loop/Condition,
//! on_error/retry, caps chống runaway, và cancellation hợp tác (Arc<AtomicBool>).
//!
//! Engine chỉ phụ thuộc trait `ActionExecutor` → test control-flow bằng mock,
//! không cần browser.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::scenario::actions::ActionExecutor;
use crate::scenario::ai::{AiClient, AiRequest};
use crate::scenario::dataset::{build_seed, Dataset};
use crate::scenario::interpolate::interpolate;
use crate::scenario::model::{
  AiMode, Block, DataBinding, DataMode, OnError, RunCaps, Scenario, StepLog,
};

/// Trạng thái sống của một lần chạy.
pub struct RunContext {
  pub profile_id: String,
  pub variables: HashMap<String, Value>,
  pub system: HashMap<String, String>,
  pub warnings: Vec<String>,
  pub step_logs: Vec<StepLog>,
  pub steps_run: u32,
  pub loop_iterations: u32,
  /// Tổng token AI đã tiêu (input+output) — đối chiếu `caps.max_ai_tokens`.
  pub ai_tokens_used: u64,
  /// Chế độ AI mức scenario; quyết định block AI có dùng provider hay không.
  pub ai_mode: AiMode,
  /// Dataset đã nạp sẵn cho run (keyed theo dataset_id), do manager bơm vào trước
  /// khi chạy — giữ engine thuần (không gọi singleton). Dùng bởi pick_row/load_dataset.
  pub datasets: HashMap<String, Dataset>,
  pub started: Instant,
  pub cancel: Arc<AtomicBool>,
  pub caps: RunCaps,
}

impl RunContext {
  pub fn new(profile_id: impl Into<String>, caps: RunCaps, cancel: Arc<AtomicBool>) -> Self {
    let pid = profile_id.into();
    let now = chrono::Utc::now();
    let mut system = HashMap::new();
    system.insert("profile_id".to_string(), pid.clone());
    system.insert("date".to_string(), now.format("%Y-%m-%d").to_string());
    system.insert("time".to_string(), now.format("%H:%M:%S").to_string());
    system.insert("datetime".to_string(), now.to_rfc3339());
    Self {
      profile_id: pid,
      variables: HashMap::new(),
      system,
      warnings: Vec::new(),
      step_logs: Vec::new(),
      steps_run: 0,
      loop_iterations: 0,
      ai_tokens_used: 0,
      ai_mode: AiMode::default(),
      datasets: HashMap::new(),
      started: Instant::now(),
      cancel,
      caps,
    }
  }

  /// Bơm biến seed (vd 1 dòng dataset) trước khi chạy. Ghi đè biến trùng tên.
  pub fn seed_variables(&mut self, vars: HashMap<String, Value>) {
    for (k, v) in vars {
      self.variables.insert(k, v);
    }
  }
}

/// Tín hiệu điều khiển luồng trả lên trên.
enum Flow {
  Continue,
  Break,
  ContinueLoop,
  Stop,
}

#[derive(Debug)]
enum EngineError {
  Cap(&'static str),
  Stopped,
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Ép một Value về số nếu được (number, hoặc string parse được).
fn as_number(v: &Value) -> Option<f64> {
  match v {
    Value::Number(n) => n.as_f64(),
    Value::String(s) => s.trim().parse().ok(),
    Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
    _ => None,
  }
}

/// Value → chuỗi để so sánh (không bọc string trong dấu nháy như `to_string`).
fn value_as_str(v: &Value) -> String {
  match v {
    Value::String(s) => s.clone(),
    Value::Null => String::new(),
    other => other.to_string(),
  }
}

/// Bằng nhau có ép kiểu: hai vế đều là số → so số; còn lại so chuỗi.
fn values_equal(a: &Value, b: &Value) -> bool {
  match (as_number(a), as_number(b)) {
    (Some(x), Some(y)) => x == y,
    _ => value_as_str(a) == value_as_str(b),
  }
}

/// Hoàn tất khi cancel-flag bật. Dùng trong `tokio::select!` để ngắt action giữa chừng.
async fn poll_cancel(flag: Arc<AtomicBool>) {
  while !flag.load(Ordering::Relaxed) {
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Trích text từ envelope kết quả MCP: `{ content: [{ type, text }] }`.
fn text_of(v: &Value) -> String {
  v.get("content")
    .and_then(|c| c.get(0))
    .and_then(|c| c.get("text"))
    .and_then(|t| t.as_str())
    .unwrap_or("")
    .to_string()
}

pub struct Engine<'e> {
  exec: &'e dyn ActionExecutor,
  ai: Option<&'e dyn AiClient>,
}

impl<'e> Engine<'e> {
  pub fn new(exec: &'e dyn ActionExecutor) -> Self {
    Self { exec, ai: None }
  }

  /// Engine với AI provider (cho các block ai_*). Không có → block AI dùng fallback.
  pub fn with_ai(exec: &'e dyn ActionExecutor, ai: &'e dyn AiClient) -> Self {
    Self { exec, ai: Some(ai) }
  }

  /// Chạy scenario; trả lại RunContext (chứa step_logs, variables, warnings).
  pub async fn run(&self, scenario: &Scenario, mut ctx: RunContext) -> RunContext {
    ctx.ai_mode = scenario.ai_mode;
    match self
      .run_blocks(&scenario.blocks, &mut ctx, scenario.on_error)
      .await
    {
      Ok(_) => {}
      Err(EngineError::Cap(c)) => ctx.warnings.push(format!("Run hit cap: {c}")),
      Err(EngineError::Stopped) => ctx.warnings.push("Run stopped on error".to_string()),
    }
    ctx
  }

  fn run_blocks<'a>(
    &'a self,
    blocks: &'a [Block],
    ctx: &'a mut RunContext,
    default_err: OnError,
  ) -> BoxFut<'a, Result<Flow, EngineError>> {
    Box::pin(async move {
      for block in blocks {
        if self.cancelled(ctx) {
          return Ok(Flow::Stop);
        }
        match self.run_block(block, ctx, default_err).await? {
          Flow::Continue => {}
          other => return Ok(other),
        }
      }
      Ok(Flow::Continue)
    })
  }

  async fn run_block(
    &self,
    block: &Block,
    ctx: &mut RunContext,
    default_err: OnError,
  ) -> Result<Flow, EngineError> {
    if block.disabled {
      return Ok(Flow::Continue);
    }

    ctx.steps_run += 1;
    if ctx.steps_run > ctx.caps.max_steps {
      return Err(EngineError::Cap("max_steps"));
    }
    if ctx.started.elapsed().as_secs() > ctx.caps.max_total_secs {
      return Err(EngineError::Cap("max_total_secs"));
    }

    let start = Instant::now();
    let on_err = block.on_error.unwrap_or(default_err);

    // Control-flow blocks.
    match block.block_type.as_str() {
      "loop" => {
        let f = self.run_loop(block, ctx, default_err).await?;
        self.push_log(ctx, block, start, "ok", None);
        return Ok(f);
      }
      "condition" => {
        let f = self.run_condition(block, ctx, default_err).await?;
        self.push_log(ctx, block, start, "ok", None);
        return Ok(f);
      }
      "for_each" => {
        let f = self.run_for_each(block, ctx, default_err).await?;
        self.push_log(ctx, block, start, "ok", None);
        return Ok(f);
      }
      "break" => return Ok(Flow::Break),
      "continue" => return Ok(Flow::ContinueLoop),
      "stop" => return Ok(Flow::Stop),
      _ => {}
    }

    // Leaf action với retry tùy on_error.
    let max_attempts = if matches!(on_err, OnError::Retry) {
      3
    } else {
      1
    };
    let mut last_err = String::new();
    for attempt in 0..max_attempts {
      if attempt > 0 {
        self.sleep_ms(2000, ctx).await;
        if self.cancelled(ctx) {
          return Ok(Flow::Stop);
        }
      }
      match self.run_leaf(block, ctx).await {
        Ok(()) => {
          let status = if block.dry_run {
            "dry_run"
          } else if attempt > 0 {
            "retried"
          } else {
            "ok"
          };
          self.push_log(ctx, block, start, status, None);
          return Ok(Flow::Continue);
        }
        Err(e) => last_err = e,
      }
    }

    match on_err {
      OnError::Skip => {
        self.push_log(ctx, block, start, "skipped", Some(last_err));
        Ok(Flow::Continue)
      }
      _ => {
        self.push_log(ctx, block, start, "failed", Some(last_err));
        Err(EngineError::Stopped)
      }
    }
  }

  async fn run_loop(
    &self,
    block: &Block,
    ctx: &mut RunContext,
    default_err: OnError,
  ) -> Result<Flow, EngineError> {
    let p = self.interp_value(&block.params, ctx);
    let count = p.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    for _ in 0..count {
      ctx.loop_iterations += 1;
      if ctx.loop_iterations > ctx.caps.max_loop_iterations {
        return Err(EngineError::Cap("max_loop_iterations"));
      }
      if self.cancelled(ctx) {
        return Ok(Flow::Stop);
      }
      match self.run_blocks(&block.children, ctx, default_err).await? {
        Flow::Break => break,
        Flow::Stop => return Ok(Flow::Stop),
        _ => {}
      }
    }
    Ok(Flow::Continue)
  }

  /// Lặp qua list từ biến `source`; mỗi vòng set biến `item` + `loop_index`.
  async fn run_for_each(
    &self,
    block: &Block,
    ctx: &mut RunContext,
    default_err: OnError,
  ) -> Result<Flow, EngineError> {
    let p = self.interp_value(&block.params, ctx);
    let source = p
      .get("source")
      .and_then(|v| v.as_str())
      .unwrap_or_default()
      .to_string();
    let items: Vec<Value> = match ctx.variables.get(&source) {
      Some(Value::Array(a)) => a.clone(),
      _ => Vec::new(),
    };
    for (idx, item) in items.into_iter().enumerate() {
      ctx.loop_iterations += 1;
      if ctx.loop_iterations > ctx.caps.max_loop_iterations {
        return Err(EngineError::Cap("max_loop_iterations"));
      }
      if self.cancelled(ctx) {
        return Ok(Flow::Stop);
      }
      ctx.variables.insert("item".to_string(), item);
      ctx
        .variables
        .insert("loop_index".to_string(), Value::from(idx));
      match self.run_blocks(&block.children, ctx, default_err).await? {
        Flow::Break => break,
        Flow::Stop => return Ok(Flow::Stop),
        _ => {}
      }
    }
    Ok(Flow::Continue)
  }

  async fn run_condition(
    &self,
    block: &Block,
    ctx: &mut RunContext,
    default_err: OnError,
  ) -> Result<Flow, EngineError> {
    let p = self.interp_value(&block.params, ctx);
    let cond = self.eval_condition(&p, ctx);
    if cond {
      self.run_blocks(&block.children, ctx, default_err).await
    } else if let Some(else_blocks) = &block.branch_else {
      self.run_blocks(else_blocks, ctx, default_err).await
    } else {
      Ok(Flow::Continue)
    }
  }

  /// Rule đơn giản (No-AI). Dạng mới: `{variable, op, value}` với
  /// op ∈ equals|not_equals|less_than|greater_than|contains. Dạng cũ vẫn nhận:
  /// `{variable, equals}` / `{variable, less_than}`.
  ///
  /// So sánh có ép kiểu: nếu CẢ hai vế parse được số → so sánh số (nên biến số `5`
  /// khớp `"5"` người dùng gõ ở UI). Ngược lại so sánh chuỗi.
  fn eval_condition(&self, p: &Value, ctx: &RunContext) -> bool {
    let actual = p
      .get("variable")
      .and_then(|v| v.as_str())
      .and_then(|name| ctx.variables.get(name))
      .cloned()
      .unwrap_or(Value::Null);

    // Toán tử + toán hạng: ưu tiên `op`/`value`, fallback key cũ.
    let (op, rhs) = if let Some(op) = p.get("op").and_then(|v| v.as_str()) {
      (op, p.get("value").cloned().unwrap_or(Value::Null))
    } else if let Some(eq) = p.get("equals") {
      ("equals", eq.clone())
    } else if let Some(lt) = p.get("less_than") {
      ("less_than", lt.clone())
    } else if let Some(gt) = p.get("greater_than") {
      ("greater_than", gt.clone())
    } else {
      return false;
    };

    match op {
      "equals" => values_equal(&actual, &rhs),
      "not_equals" => !values_equal(&actual, &rhs),
      "less_than" => match (as_number(&actual), as_number(&rhs)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
      },
      "greater_than" => match (as_number(&actual), as_number(&rhs)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
      },
      "contains" => match &actual {
        Value::Array(items) => items.iter().any(|x| values_equal(x, &rhs)),
        _ => value_as_str(&actual).contains(&value_as_str(&rhs)),
      },
      _ => false,
    }
  }

  async fn run_leaf(&self, block: &Block, ctx: &mut RunContext) -> Result<(), String> {
    let p = self.interp_value(&block.params, ctx);
    let dry = block.dry_run;

    match block.block_type.as_str() {
      "open_url" => {
        let url = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
        self
          .act(ctx, dry, "navigate", json!({ "url": url }))
          .await?;
      }
      "get_page_text" => {
        let r = self
          .act(ctx, dry, "get_page_content", json!({ "format": "text" }))
          .await?;
        self.store_output(&p, text_of(&r), ctx);
      }
      "get_url" => {
        let r = self.act(ctx, dry, "get_page_info", json!({})).await?;
        self.store_output(&p, text_of(&r), ctx);
      }
      "screenshot" => {
        self.act(ctx, dry, "screenshot", json!({})).await?;
      }
      "click_by_index" => {
        let index = p.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
        self
          .act(ctx, dry, "click_by_index", json!({ "index": index }))
          .await?;
      }
      "click" => {
        let selector = p.get("selector").and_then(|v| v.as_str()).unwrap_or("");
        self
          .act(ctx, dry, "click_element", json!({ "selector": selector }))
          .await?;
      }
      // Outbound actions là block bình thường, tự động đầy đủ (dry_run tùy chọn).
      "type_text" | "post" | "reply" | "submit" => {
        let selector = p.get("selector").and_then(|v| v.as_str()).unwrap_or("");
        let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
        self
          .act(
            ctx,
            dry,
            "type_text",
            json!({ "selector": selector, "text": text }),
          )
          .await?;
      }
      "press_key" => {
        let key = p.get("key").and_then(|v| v.as_str()).unwrap_or("Enter");
        // Modifiers accept a JSON array or a comma-separated string ("Control, Shift").
        let modifiers = match p.get("modifiers") {
          Some(Value::Array(a)) => Value::Array(a.clone()),
          Some(Value::String(s)) => Value::Array(
            s.split(',')
              .map(|x| x.trim())
              .filter(|x| !x.is_empty())
              .map(|x| Value::String(x.to_string()))
              .collect(),
          ),
          _ => Value::Array(vec![]),
        };
        self
          .act(
            ctx,
            dry,
            "press_key",
            json!({ "key": key, "modifiers": modifiers }),
          )
          .await?;
      }
      "upload_file" => {
        let selector = p.get("selector").and_then(|v| v.as_str()).unwrap_or("");
        // Files accept a JSON array or a comma/newline-separated string of paths.
        let files = match p.get("files") {
          Some(Value::Array(a)) => Value::Array(a.clone()),
          Some(Value::String(s)) => Value::Array(
            s.split(['\n', ','])
              .map(|x| x.trim())
              .filter(|x| !x.is_empty())
              .map(|x| Value::String(x.to_string()))
              .collect(),
          ),
          _ => Value::Array(vec![]),
        };
        self
          .act(
            ctx,
            dry,
            "upload_file",
            json!({ "selector": selector, "files": files }),
          )
          .await?;
      }
      "new_tab" => {
        let url = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
        self.act(ctx, dry, "new_tab", json!({ "url": url })).await?;
      }
      "switch_tab" => {
        let index = p.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
        self
          .act(ctx, dry, "switch_tab", json!({ "index": index }))
          .await?;
      }
      "close_tab" => {
        let index = p.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
        self
          .act(ctx, dry, "close_tab", json!({ "index": index }))
          .await?;
      }
      "list_tabs" => {
        let r = self.act(ctx, dry, "list_tabs", json!({})).await?;
        self.store_output(&p, text_of(&r), ctx);
      }
      "wait" => {
        let secs = p.get("seconds").and_then(|v| v.as_f64()).unwrap_or(1.0);
        self.sleep_ms((secs * 1000.0) as u64, ctx).await;
      }
      "set_variable" => {
        if let Some(name) = p.get("name").and_then(|v| v.as_str()) {
          let value = p.get("value").cloned().unwrap_or(Value::Null);
          ctx.variables.insert(name.to_string(), value);
        }
      }
      "log" => {
        let msg = p.get("message").and_then(|v| v.as_str()).unwrap_or("");
        ctx.warnings.push(format!("[log] {msg}"));
      }
      "go_back" | "go_forward" => {
        let steps = p.get("steps").and_then(|v| v.as_i64()).unwrap_or(1);
        let n = if block.block_type == "go_back" {
          -steps
        } else {
          steps
        };
        self
          .act(
            ctx,
            dry,
            "evaluate_javascript",
            json!({ "expression": format!("history.go({n})"), "wait_for_load": true }),
          )
          .await?;
      }
      "refresh" => {
        self
          .act(
            ctx,
            dry,
            "evaluate_javascript",
            json!({ "expression": "location.reload()", "wait_for_load": true }),
          )
          .await?;
      }
      "scroll" => {
        // Cuộn mượt với easing (nhịp tự nhiên). distance_px<0 → cuộn lên.
        let dist = p.get("distance_px").and_then(|v| v.as_i64()).unwrap_or(800);
        let dur = p.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(900);
        let script = format!(
          "(async()=>{{const D={dur},dist={dist},s=window.scrollY,t=performance.now();\
           while(true){{const e=performance.now()-t,p=Math.min(e/D,1),\
           k=p<0.5?4*p*p*p:1-Math.pow(-2*p+2,3)/2;window.scrollTo(0,s+dist*k);\
           if(p>=1)break;await new Promise(r=>setTimeout(r,16));}}return scrollY;}})()"
        );
        self
          .act(
            ctx,
            dry,
            "evaluate_javascript",
            json!({ "expression": script, "await_promise": true }),
          )
          .await?;
      }
      "find_elements" => {
        let r = self
          .act(ctx, dry, "get_interactive_elements", json!({}))
          .await?;
        self.store_output(&p, text_of(&r), ctx);
      }
      "get_page_html" => {
        let r = self
          .act(ctx, dry, "get_page_content", json!({ "format": "html" }))
          .await?;
        self.store_output(&p, text_of(&r), ctx);
      }
      "run_js" => {
        let expr = p.get("expression").and_then(|v| v.as_str()).unwrap_or("");
        // `evaluate_javascript` đánh giá biểu thức (KHÔNG bọc hàm), nên `return ...`
        // ở mức trên cùng sẽ lỗi "return not in function". Bọc trong IIFE: nếu code
        // có `return` → dạng block; nếu là biểu thức trần → trả thẳng giá trị.
        let wrapped = if expr.trim().is_empty() {
          String::new()
        } else if expr.contains("return") {
          format!("(() => {{ {expr} }})()")
        } else {
          format!("(() => ({expr}))()")
        };
        let r = self
          .act(
            ctx,
            dry,
            "evaluate_javascript",
            json!({ "expression": wrapped }),
          )
          .await?;
        self.store_output(&p, text_of(&r), ctx);
      }
      "wait_random" => {
        let min = p.get("min_s").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let max = p.get("max_s").and_then(|v| v.as_f64()).unwrap_or(3.0);
        let span = (max - min).max(0.0);
        let secs = min + (rand::random::<u64>() as f64 / u64::MAX as f64) * span;
        self.sleep_ms((secs * 1000.0) as u64, ctx).await;
      }
      "pick_row" => {
        let dataset_id = p.get("dataset_id").and_then(|v| v.as_str()).unwrap_or("");
        let prefix = p
          .get("prefix")
          .and_then(|v| v.as_str())
          .filter(|s| !s.is_empty())
          .map(|s| s.to_string());
        let want_random = p.get("mode").and_then(|v| v.as_str()) == Some("random");
        // Lấy dòng (clone) trong block riêng để kết thúc mượn `ctx.datasets`
        // trước khi ghi `ctx.variables`.
        let chosen = {
          match ctx.datasets.get(dataset_id) {
            Some(ds) if !ds.rows.is_empty() => {
              let len = ds.rows.len();
              let idx = match (
                p.get("index").and_then(|v| v.as_i64()),
                p.get("index").and_then(|v| v.as_str()),
              ) {
                (Some(i), _) => i.rem_euclid(len as i64) as usize,
                (None, Some(s)) => s
                  .trim()
                  .parse::<i64>()
                  .map(|i| i.rem_euclid(len as i64) as usize)
                  .unwrap_or(0),
                _ if want_random => (rand::random::<u64>() % len as u64) as usize,
                _ => 0,
              };
              Some(ds.rows[idx].clone())
            }
            _ => None,
          }
        };
        match chosen {
          Some(row) => {
            let binding = DataBinding {
              dataset_id: dataset_id.to_string(),
              mode: DataMode::Random,
              prefix,
            };
            for (k, v) in build_seed(&binding, &row) {
              ctx.variables.insert(k, v);
            }
          }
          None => ctx.warnings.push(format!(
            "[pick_row] dataset '{dataset_id}' missing or empty"
          )),
        }
      }
      "load_dataset" => {
        let dataset_id = p.get("dataset_id").and_then(|v| v.as_str()).unwrap_or("");
        let out = p
          .get("output_variable")
          .and_then(|v| v.as_str())
          .unwrap_or("rows")
          .to_string();
        let arr = ctx
          .datasets
          .get(dataset_id)
          .map(|ds| Value::Array(ds.rows.iter().cloned().map(Value::Object).collect()));
        match arr {
          Some(a) => {
            ctx.variables.insert(out, a);
          }
          None => ctx
            .warnings
            .push(format!("[load_dataset] dataset '{dataset_id}' not found")),
        }
      }
      "ai_check" | "ai_write" | "ai_decide" | "ai_extract" | "ai_summarize" | "ai_find_element" => {
        self.run_ai_block(block, &p, ctx).await?;
      }
      "set_profile_tag" => {
        // Add a tag to the profile this run is driving (e.g. tag a profile with
        // the account email once login succeeds). Routed through MCP because the
        // executor has no AppHandle; the handler calls ProfileManager.
        let tag = p.get("tag").and_then(|v| v.as_str()).unwrap_or("");
        if !tag.is_empty() {
          self
            .act(ctx, dry, "add_profile_tag", json!({ "tag": tag }))
            .await?;
        }
      }
      other => return Err(format!("Unknown block type: {other}")),
    }
    Ok(())
  }

  /// Chạy block AI: nếu có provider + ai_enabled → gọi AI (structured), else fallback.
  async fn run_ai_block(
    &self,
    block: &Block,
    p: &Value,
    ctx: &mut RunContext,
  ) -> Result<(), String> {
    let out_var = p
      .get("output_variable")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    // ai_mode mức scenario quyết định: NoAi → luôn fallback; Ai → luôn AI;
    // Auto → theo cờ ai_enabled của block. Vượt trần token cũng rơi fallback.
    let ai_on = match ctx.ai_mode {
      AiMode::NoAi => false,
      AiMode::Ai => true,
      AiMode::Auto => block.ai_enabled,
    };
    let budget_left = ctx.ai_tokens_used < ctx.caps.max_ai_tokens;
    if ai_on && !budget_left {
      ctx.warnings.push(format!(
        "[ai] token cap {} reached → fallback",
        ctx.caps.max_ai_tokens
      ));
    }

    if ai_on && budget_left {
      if let Some(ai) = self.ai {
        let mut prompt = p
          .get("prompt")
          .or_else(|| p.get("question"))
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        let inputs = p
          .get("input")
          .and_then(|v| v.as_array())
          .cloned()
          .unwrap_or_default();
        let wants = |k: &str| inputs.iter().any(|x| x.as_str() == Some(k));
        if wants("page_text") {
          let r = self
            .act(ctx, false, "get_page_content", json!({ "format": "text" }))
            .await?;
          prompt.push_str("\n\n[PAGE TEXT]\n");
          prompt.push_str(&text_of(&r));
        }
        if wants("interactive_list") {
          let r = self
            .act(ctx, false, "get_interactive_elements", json!({}))
            .await?;
          prompt.push_str("\n\n[INTERACTIVE]\n");
          prompt.push_str(&text_of(&r));
        }
        let schema = match block.block_type.as_str() {
          "ai_check" => Some(
            json!({ "type": "object", "properties": { "result": { "type": "boolean" } }, "required": ["result"] }),
          ),
          "ai_find_element" => Some(
            json!({ "type": "object", "properties": { "index": { "type": "integer" } }, "required": ["index"] }),
          ),
          "ai_decide" | "ai_extract" => Some(
            json!({ "type": "object", "properties": { "items": { "type": "array" } }, "required": ["items"] }),
          ),
          _ => None,
        };
        let req = AiRequest {
          system:
            "You are a browser-automation assistant. Follow the requested output format exactly."
              .to_string(),
          prompt,
          schema,
        };
        match ai.run(req).await {
          Ok(res) => {
            let val = match block.block_type.as_str() {
              "ai_check" => json!(res
                .json
                .as_ref()
                .and_then(|j| j.get("result"))
                .and_then(|b| b.as_bool())
                .unwrap_or(false)),
              "ai_find_element" => res
                .json
                .as_ref()
                .and_then(|j| j.get("index"))
                .cloned()
                .unwrap_or(json!(0)),
              "ai_decide" | "ai_extract" => res
                .json
                .as_ref()
                .and_then(|j| j.get("items"))
                .cloned()
                .or_else(|| res.json.clone())
                .unwrap_or(json!([])),
              _ => json!(res.text.clone().unwrap_or_default()),
            };
            if let Some(name) = &out_var {
              ctx.variables.insert(name.clone(), val);
            }
            ctx.ai_tokens_used += res.input_tokens + res.output_tokens;
            ctx.warnings.push(format!(
              "[ai] {} ok (tokens {}+{}, total {}/{})",
              block.block_type,
              res.input_tokens,
              res.output_tokens,
              ctx.ai_tokens_used,
              ctx.caps.max_ai_tokens
            ));
            return Ok(());
          }
          Err(e) => ctx
            .warnings
            .push(format!("[ai] {} error → fallback: {e}", block.block_type)),
        }
      }
    }
    self.ai_fallback(block, p, &out_var, ctx).await
  }

  async fn ai_fallback(
    &self,
    block: &Block,
    p: &Value,
    out_var: &Option<String>,
    ctx: &mut RunContext,
  ) -> Result<(), String> {
    let val = match block.block_type.as_str() {
      "ai_check" => json!(false),
      "ai_write" => json!(p
        .get("fallback_text")
        .and_then(|v| v.as_str())
        .unwrap_or("")),
      "ai_find_element" => json!(0),
      "ai_summarize" => {
        let r = self
          .act(ctx, false, "get_page_content", json!({ "format": "text" }))
          .await?;
        let max = p.get("max_length").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
        json!(text_of(&r).chars().take(max).collect::<String>())
      }
      _ => json!([]),
    };
    if let Some(name) = out_var {
      ctx.variables.insert(name.clone(), val);
    }
    ctx
      .warnings
      .push(format!("[ai] fallback {}", block.block_type));
    Ok(())
  }

  /// Gọi MCP tool; honor dry_run (không thực thi side-effect). Action chạy đua
  /// (`tokio::select!`) với cancel-flag nên một call dài (navigate/AI) vẫn ngắt
  /// được giữa chừng thay vì phải đợi xong block.
  async fn act(
    &self,
    ctx: &RunContext,
    dry: bool,
    tool: &str,
    args: Value,
  ) -> Result<Value, String> {
    if dry {
      return Ok(json!({ "dry_run": true }));
    }
    tokio::select! {
      biased;
      _ = poll_cancel(ctx.cancel.clone()) => Err("cancelled".to_string()),
      r = self.exec.call(&ctx.profile_id, tool, args) => r.map_err(|e| e.to_string()),
    }
  }

  fn store_output(&self, p: &Value, text: String, ctx: &mut RunContext) {
    if let Some(name) = p.get("output_variable").and_then(|v| v.as_str()) {
      ctx.variables.insert(name.to_string(), Value::String(text));
    }
  }

  /// Interpolate đệ quy mọi string trong một Value.
  fn interp_value(&self, v: &Value, ctx: &mut RunContext) -> Value {
    match v {
      Value::String(s) => Value::String(interpolate(
        s,
        &ctx.variables,
        &ctx.system,
        &mut ctx.warnings,
      )),
      Value::Array(a) => Value::Array(a.iter().map(|x| self.interp_value(x, ctx)).collect()),
      Value::Object(o) => {
        let mut m = serde_json::Map::new();
        for (k, val) in o {
          m.insert(k.clone(), self.interp_value(val, ctx));
        }
        Value::Object(m)
      }
      other => other.clone(),
    }
  }

  async fn sleep_ms(&self, ms: u64, ctx: &RunContext) {
    let mut left = ms;
    while left > 0 {
      if self.cancelled(ctx) {
        return;
      }
      let chunk = left.min(100);
      tokio::time::sleep(Duration::from_millis(chunk)).await;
      left -= chunk;
    }
  }

  fn cancelled(&self, ctx: &RunContext) -> bool {
    ctx.cancel.load(Ordering::Relaxed)
  }

  fn push_log(
    &self,
    ctx: &mut RunContext,
    block: &Block,
    start: Instant,
    status: &str,
    error: Option<String>,
  ) {
    ctx.step_logs.push(StepLog {
      block_id: block.id.clone(),
      block_type: block.block_type.clone(),
      status: status.to_string(),
      duration_ms: start.elapsed().as_millis(),
      error,
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::scenario::actions::ActionError;
  use crate::scenario::model::{AiMode, Block, Scenario};
  use std::sync::Mutex;

  #[derive(Default)]
  struct MockExec {
    calls: Mutex<Vec<(String, String, Value)>>,
  }

  #[async_trait::async_trait]
  impl ActionExecutor for MockExec {
    async fn call(&self, profile_id: &str, tool: &str, args: Value) -> Result<Value, ActionError> {
      self
        .calls
        .lock()
        .unwrap()
        .push((profile_id.to_string(), tool.to_string(), args));
      Ok(json!({ "content": [{ "type": "text", "text": "mock" }] }))
    }
  }

  fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap()
  }

  #[test]
  fn set_var_condition_loop_and_dry_run() {
    let mock = MockExec::default();
    let engine = Engine::new(&mock);

    let mut cond = Block::new("condition", json!({ "variable": "flag", "equals": true }));
    cond.children = vec![Block::new("log", json!({ "message": "yes" }))];
    cond.branch_else = Some(vec![Block::new("log", json!({ "message": "no" }))]);

    let mut lp = Block::new("loop", json!({ "count": 3 }));
    lp.children = vec![Block::new(
      "open_url",
      json!({ "url": "https://example.com" }),
    )];

    // Outbound dry_run: KHÔNG được gọi action thật.
    let mut post = Block::new("post", json!({ "selector": "#box", "text": "hi" }));
    post.dry_run = true;

    let scenario = Scenario {
      id: "s".into(),
      name: "t".into(),
      description: None,
      ai_mode: AiMode::NoAi,
      on_error: OnError::Stop,
      caps: RunCaps::default(),
      data_source: None,
      blocks: vec![
        Block::new("set_variable", json!({ "name": "flag", "value": true })),
        cond,
        lp,
        post,
      ],
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = RunContext::new("profile-1", scenario.caps.clone(), cancel);
    let ctx = rt().block_on(engine.run(&scenario, ctx));

    let calls = mock.calls.lock().unwrap();
    let navs = calls.iter().filter(|(_, t, _)| t == "navigate").count();
    assert_eq!(navs, 3, "loop count=3 → navigate 3 lần");
    assert!(
      !calls.iter().any(|(_, t, _)| t == "type_text"),
      "post.dry_run=true → KHÔNG gọi type_text"
    );
    assert!(calls.iter().all(|(pid, _, _)| pid == "profile-1"));

    assert!(ctx
      .step_logs
      .iter()
      .any(|s| s.block_type == "post" && s.status == "dry_run"));
    assert!(ctx.warnings.iter().any(|w| w == "[log] yes"));
    assert!(!ctx.warnings.iter().any(|w| w == "[log] no"));
  }

  #[test]
  fn run_js_wraps_expression_so_return_works() {
    let mock = MockExec::default();
    let engine = Engine::new(&mock);
    let scenario = Scenario {
      id: "s".into(),
      name: "t".into(),
      description: None,
      ai_mode: AiMode::NoAi,
      on_error: OnError::Stop,
      caps: RunCaps::default(),
      data_source: None,
      blocks: vec![
        Block::new("run_js", json!({ "expression": "return document.title" })),
        Block::new("run_js", json!({ "expression": "document.title" })),
      ],
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = RunContext::new("p", scenario.caps.clone(), cancel);
    let _ = rt().block_on(engine.run(&scenario, ctx));

    let calls = mock.calls.lock().unwrap();
    let exprs: Vec<String> = calls
      .iter()
      .filter(|(_, t, _)| t == "evaluate_javascript")
      .map(|(_, _, a)| {
        a.get("expression")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string()
      })
      .collect();
    assert_eq!(exprs.len(), 2);
    // `return ...` → bọc block; biểu thức trần → trả thẳng giá trị.
    assert_eq!(exprs[0], "(() => { return document.title })()");
    assert_eq!(exprs[1], "(() => (document.title))()");
  }

  #[test]
  fn tab_upload_and_presskey_blocks_map_to_tools() {
    let mock = MockExec::default();
    let engine = Engine::new(&mock);
    let scenario = Scenario {
      id: "s".into(),
      name: "t".into(),
      description: None,
      ai_mode: AiMode::NoAi,
      on_error: OnError::Stop,
      caps: RunCaps::default(),
      data_source: None,
      blocks: vec![
        Block::new(
          "press_key",
          json!({ "key": "Enter", "modifiers": "Control, Shift" }),
        ),
        Block::new(
          "upload_file",
          json!({ "selector": "#f", "files": "/a.txt\n/b.txt" }),
        ),
        Block::new("new_tab", json!({ "url": "https://x.com" })),
        Block::new("switch_tab", json!({ "index": 2 })),
        Block::new("close_tab", json!({ "index": 1 })),
        Block::new("list_tabs", json!({ "output_variable": "tabs" })),
      ],
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = RunContext::new("p", scenario.caps.clone(), cancel);
    let ctx = rt().block_on(engine.run(&scenario, ctx));

    let calls = mock.calls.lock().unwrap();
    let by_tool = |name: &str| {
      calls
        .iter()
        .find(|(_, t, _)| t == name)
        .map(|(_, _, a)| a.clone())
    };

    // Comma-separated modifier string → array of 2.
    let pk = by_tool("press_key").expect("press_key called");
    assert_eq!(pk.get("key").and_then(|v| v.as_str()), Some("Enter"));
    assert_eq!(
      pk.get("modifiers").and_then(|v| v.as_array()).map(Vec::len),
      Some(2)
    );

    // Newline-separated file paths → array of 2.
    let up = by_tool("upload_file").expect("upload_file called");
    assert_eq!(
      up.get("files").and_then(|v| v.as_array()).map(Vec::len),
      Some(2)
    );

    assert!(by_tool("new_tab").is_some());
    assert_eq!(
      by_tool("switch_tab").and_then(|a| a.get("index").and_then(|v| v.as_i64())),
      Some(2)
    );
    assert!(by_tool("close_tab").is_some());
    // list_tabs stores the (mock) tool output into the requested variable.
    assert!(ctx.variables.contains_key("tabs"));
  }

  #[test]
  fn seed_and_dataset_blocks_populate_variables() {
    use crate::scenario::dataset::Dataset;

    let mock = MockExec::default();
    let engine = Engine::new(&mock);

    let mut r0 = serde_json::Map::new();
    r0.insert("reply".to_string(), json!("hello"));
    let mut r1 = serde_json::Map::new();
    r1.insert("reply".to_string(), json!("hi"));
    let ds = Dataset {
      id: "d".into(),
      name: "d".into(),
      columns: vec!["reply".into()],
      rows: vec![r0, r1],
      created_at: String::new(),
      updated_at: String::new(),
    };

    let scenario = Scenario {
      id: "s".into(),
      name: "t".into(),
      description: None,
      ai_mode: AiMode::NoAi,
      on_error: OnError::Stop,
      caps: RunCaps::default(),
      data_source: None,
      blocks: vec![
        Block::new("pick_row", json!({ "dataset_id": "d", "index": 1 })),
        Block::new("log", json!({ "message": "{{reply}}" })),
        Block::new(
          "load_dataset",
          json!({ "dataset_id": "d", "output_variable": "all" }),
        ),
      ],
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let mut ctx = RunContext::new("p", scenario.caps.clone(), cancel);
    ctx.datasets.insert("d".to_string(), ds);
    // seed_variables also smoke-tested:
    let mut pre = HashMap::new();
    pre.insert("greeting".to_string(), json!("hey"));
    ctx.seed_variables(pre);

    let ctx = rt().block_on(engine.run(&scenario, ctx));

    assert_eq!(ctx.variables.get("greeting"), Some(&json!("hey")));
    assert_eq!(ctx.variables.get("reply"), Some(&json!("hi"))); // index 1
    assert!(ctx.warnings.iter().any(|w| w == "[log] hi"));
    assert_eq!(
      ctx
        .variables
        .get("all")
        .and_then(|v| v.as_array())
        .map(|a| a.len()),
      Some(2)
    );
  }

  #[test]
  fn unknown_block_stops_on_error() {
    let mock = MockExec::default();
    let engine = Engine::new(&mock);
    let scenario = Scenario {
      id: "s".into(),
      name: "t".into(),
      description: None,
      ai_mode: AiMode::NoAi,
      on_error: OnError::Stop,
      caps: RunCaps::default(),
      data_source: None,
      blocks: vec![
        Block::new("totally_unknown", json!({})),
        Block::new("open_url", json!({ "url": "https://x.com" })),
      ],
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = RunContext::new("p", scenario.caps.clone(), cancel);
    let ctx = rt().block_on(engine.run(&scenario, ctx));

    // Block lỗi (stop) → block sau KHÔNG chạy.
    assert!(mock.calls.lock().unwrap().is_empty());
    assert!(ctx.step_logs.iter().any(|s| s.status == "failed"));
    assert!(ctx.warnings.iter().any(|w| w.contains("stopped")));
  }

  #[test]
  fn condition_coerces_types_and_operators() {
    let mock = MockExec::default();
    let engine = Engine::new(&mock);

    // count = 5 (NUMBER). Các điều kiện so với chuỗi người dùng gõ ở UI.
    let mut eq = Block::new(
      "condition",
      json!({ "op": "equals", "variable": "count", "value": "5" }),
    );
    eq.children = vec![Block::new("log", json!({ "message": "eq" }))];

    let mut gt = Block::new(
      "condition",
      json!({ "op": "greater_than", "variable": "count", "value": "3" }),
    );
    gt.children = vec![Block::new("log", json!({ "message": "gt" }))];

    let mut lt = Block::new(
      "condition",
      json!({ "op": "less_than", "variable": "count", "value": "3" }),
    );
    lt.children = vec![Block::new("log", json!({ "message": "lt-should-not" }))];

    // contains trên mảng.
    let mut ct = Block::new(
      "condition",
      json!({ "op": "contains", "variable": "tags", "value": "b" }),
    );
    ct.children = vec![Block::new("log", json!({ "message": "ct" }))];

    // Legacy key `equals` vẫn hoạt động.
    let mut legacy = Block::new("condition", json!({ "variable": "count", "equals": 5 }));
    legacy.children = vec![Block::new("log", json!({ "message": "legacy" }))];

    let scenario = Scenario {
      id: "s".into(),
      name: "t".into(),
      description: None,
      ai_mode: AiMode::NoAi,
      on_error: OnError::Stop,
      caps: RunCaps::default(),
      data_source: None,
      blocks: vec![
        Block::new("set_variable", json!({ "name": "count", "value": 5 })),
        Block::new(
          "set_variable",
          json!({ "name": "tags", "value": ["a", "b", "c"] }),
        ),
        eq,
        gt,
        lt,
        ct,
        legacy,
      ],
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let ctx = RunContext::new("p", scenario.caps.clone(), cancel);
    let ctx = rt().block_on(engine.run(&scenario, ctx));

    let has = |m: &str| ctx.warnings.iter().any(|w| w == &format!("[log] {m}"));
    assert!(has("eq"), "number 5 == string \"5\"");
    assert!(has("gt"), "5 > 3");
    assert!(has("ct"), "tags contains b");
    assert!(has("legacy"), "legacy equals key still works");
    assert!(!has("lt-should-not"), "5 < 3 phải false");
  }
}
