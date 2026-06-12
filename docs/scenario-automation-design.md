# Scenario Automation — Thiết kế triển khai (best-practice)

> Bản thiết kế kỹ thuật để xây tính năng **Scenario Automation Builder** vào WaterMelon
> Browser, viết theo best-practice và gắn vào kiến trúc hiện tại (Tauri/Rust backend +
> Next.js frontend, lớp MCP tool sẵn có). Tài liệu này **độc lập** với spec gốc
> (`scenario-automation-spec.md` chỉ là tham khảo ngữ nghĩa). Engine viết bằng **Rust**;
> outbound actions (send/reply/post/submit) là **block hạng nhất, tự động đầy đủ**.

---

## 1. Mục tiêu & phạm vi
- Cho phép người dùng dựng kịch bản tự động hóa browser (RPA) chạy trên profile
  Camoufox/Cloak: điều hướng, đọc, tương tác, rẽ nhánh/vòng lặp, AI, hẹn giờ, xoay profile.
- **Outbound actions tự động hoàn toàn** (không bắt buộc duyệt tay); có cờ `dry_run`/
  `require_confirm` **tùy chọn, mặc định tắt**.
- Chạy nền độc lập cửa sổ UI; bền vững khi app khởi động lại; huỷ được; có trần an toàn.

---

## 2. Nguyên tắc thiết kế
1. **Reuse, không reimplement** — engine gọi lại logic tool sẵn có (`dispatch_tool_call`),
   không viết lại thao tác browser, không vòng qua HTTP MCP.
2. **Engine ở Rust backend** — scheduler/executor sống trong tiến trình Tauri, không phụ
   thuộc webview đang mở.
3. **Decouple bằng trait** — engine chỉ biết `ActionExecutor`/`AiClient`/`ScenarioStore`
   (trait), test được logic mà không cần browser/LLM thật.
4. **JSON ở biên, typed ở lõi** — định nghĩa block lưu/truyền dạng JSON linh hoạt; deserialize
   sang struct typed ngay lúc thực thi để validate + an toàn kiểu.
5. **An toàn theo mặc định** — caps chống runaway, cancellation, crash-recovery, mã hoá secret.

---

## 3. Kiến trúc tổng thể

```
Frontend (Next.js)  — Builder UI + Monitor (chỉ tạo/sửa/giám sát; không chạy engine)
        │ Tauri invoke (commands.rs)
        ▼
Rust backend  ── scenario/ ───────────────────────────────────┐
  Scheduler  →  Executor  →  ActionExecutor ─► dispatch_tool_call (mcp_server.rs)
       │            │            AiClient ─► reqwest → LLM provider
       │            │            Interpolate (variables)
       │            └─ Store: JSON-file (định nghĩa) + SQLite (run/steplog)
       ▼
  Browser engines: Cloak (CDP) · Camoufox (WebDriver BiDi)
```

Vị trí trong repo: thêm thư mục `src-tauri/src/scenario/`, đăng ký command trong `lib.rs`,
thêm UI ở `src/components/scenario/`.

---

## 4. Bố cục module

```
src-tauri/src/scenario/
  mod.rs          // re-export + khởi tạo singleton engine
  model.rs        // serde structs: Scenario, Block, Variable, Schedule, Run, StepLog, enums
  store.rs        // ScenarioStore trait + impl: JSON-file (định nghĩa) + SQLite (logs)
  interpolate.rs  // variable engine: {{var|filter}}, system vars, resolve_path
  actions.rs      // ActionExecutor trait + McpActionExecutor (gọi dispatch_tool_call)
  blocks.rs       // map BlockType → action; deserialize params typed; human-pacing helpers
  executor.rs     // walk block-tree, loop_stack, condition, on_error/retry, caps, cancel
  ai.rs           // AiClient trait + providers (anthropic/openai/gemini/ollama) + fallback
  scheduler.rs    // cron tick-loop, rotation, conflict detection, durability
  commands.rs     // #[tauri::command] CRUD scenario/schedule + run/stop + query runs
```

---

## 5. Lớp thực thi action (decouple + testable)

```rust
#[async_trait::async_trait]
pub trait ActionExecutor: Send + Sync {
    /// tool = tên MCP tool ("navigate", "click_by_index", ...); args = JSON arguments.
    async fn call(&self, profile_id: &str, tool: &str, args: serde_json::Value)
        -> Result<serde_json::Value, ActionError>;
}

/// Impl thật: bọc McpServer::dispatch_tool_call (đổi visibility sang pub(crate)).
pub struct McpActionExecutor;
// Impl mock trong test: trả kết quả định sẵn → test loop/condition/on_error KHÔNG cần browser.
```

> Best-practice: engine **không** tham chiếu trực tiếp `McpServer` singleton → unit test toàn
> bộ control-flow bằng mock executor.

---

## 6. Data model (`model.rs`)

```rust
#[derive(Serialize, Deserialize, Clone)]
pub struct Scenario {
    pub id: String,                 // uuid
    pub name: String,
    pub description: Option<String>,
    pub version: u32,               // tăng mỗi lần save (migration-friendly)
    pub tags: Vec<String>,
    pub ai_mode: AiMode,            // Ai | NoAi | Auto
    pub ai_provider_id: Option<String>,
    pub on_error: OnError,          // Stop | Skip | Retry
    pub blocks: Vec<Block>,         // cây block (nesting qua children)
    pub variables: Vec<VariableDef>,
    pub caps: RunCaps,              // trần an toàn
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Block {
    pub id: String,
    pub r#type: String,             // "open_url", "click", "post", ... (JSON ở biên)
    pub label: Option<String>,
    pub ai_enabled: bool,
    pub params: serde_json::Value,            // typed hoá lúc thực thi
    pub fallback_params: Option<serde_json::Value>,
    pub children: Vec<Block>,                 // Loop/Condition chứa children
    pub branch_else: Option<Vec<Block>>,      // Condition: nhánh else
    pub on_error: Option<OnError>,            // override scenario-level
    pub timeout_ms: Option<u64>,
    pub dry_run: bool,                         // TÙY CHỌN, mặc định false (outbound auto đầy đủ)
    pub disabled: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RunCaps {                 // chống runaway (spec gốc thiếu)
    pub max_steps: u32,              // vd 2000
    pub max_loop_iterations: u32,    // vd 1000
    pub max_total_secs: u64,         // vd 3600
    pub max_ai_tokens: u64,          // vd 200_000 — kiểm soát chi phí
}
```

Variable / Schedule / ProfileAssignment / Run / StepLog: giữ ngữ nghĩa như spec gốc
(§4.3–4.7) nhưng là **Rust struct**; `params`/`value` runtime dùng `serde_json::Value`.

**Typed-at-boundary** — mỗi handler block parse params riêng:
```rust
#[derive(Deserialize)] struct OpenUrlParams { url: String, wait_mode: Option<WaitMode> }
let p: OpenUrlParams = serde_json::from_value(block.params.clone())?; // validate tại đây
```

---

## 7. Storage (`store.rs`)
| Dữ liệu | Lưu ở | Lý do |
|---|---|---|
| Scenario, Block, Variable, Schedule, ProfileAssignment | **JSON file** (`data_dir/scenarios/<id>.json`, `atomic_write`) | Theo convention profile (`metadata.json`); ít bản ghi, dễ export/diff |
| ScenarioRun, StepLog | **SQLite** (`data_dir/scenarios/runs.db`, `rusqlite`) | Hàng nghìn dòng/lần chạy, cần query/filter/aggregate; rusqlite đã là dep |
| Screenshot của step | File PNG (`cache_dir/scenario_shots/`), lưu path trong StepLog | Không nhét blob vào DB |
| API key của AI provider | **Mã hoá** (tái dùng Argon2+AES-GCM ở `settings_manager`) | Bí mật at-rest |

`ScenarioStore` là trait → dễ test bằng in-memory impl.

---

## 8. Variable interpolation (`interpolate.rs`)
- Cú pháp `{{var}}`, `{{a.b.c}}`, `{{var | filter:arg}}`, `{{random_from: [...]}}` — ngữ nghĩa như spec gốc §5.
- System vars: `date`, `time`, `datetime`, `profile_name`, `profile_id`, `run_id`, `loop_index`, `scenario_name`, `item` (trong for-each).
- Filters: `length`, `format_vnd`, `truncate:N`, `upper`, `lower`, `join:sep`, `first`, `last`, `json`.
- Implement bằng `regex_lite` (đã có dep) + `serde_json::Value` cho giá trị động; random dùng `rand`.
- Error: biến thiếu → `""` + warning vào StepLog; filter lạ → block fail theo `on_error`.

---

## 9. Execution engine (`executor.rs`)

**Runtime context** giữ: `run_id`, `scenario`, `profile`, `ai_available`, `variables`,
`warnings`, `loop_stack: Vec<LoopFrame>`, `cancel: CancellationToken`, bộ đếm caps.

**Vòng thực thi mỗi block:**
```
1. nếu disabled → skip
2. interpolate(params) theo context
3. chọn path: ai_enabled && ai_available ? AI : fallback_params
4. blocks.rs map type → ActionExecutor.call(...)   (hoặc xử lý Flow nội bộ)
5. kết quả:
     ok    → StepLog(ok), cập nhật variables, advance
     error → on_error: Stop | Skip | Retry(backoff, tối đa N) 
6. mỗi bước: kiểm caps (max_steps/iterations/duration/ai_tokens) + cancel token → vượt thì dừng sạch
```

**Block đặc biệt:** `Loop`/`ForEach` push `LoopFrame` → chạy children (kiểm `max_loop_iterations`);
`Condition` eval (rule hoặc AI) → chạy `children` (if) hoặc `branch_else`; `RunScenario` → sub-context;
`Break`/`Continue`/`Stop` điều khiển stack.

**Cancellation (bắt buộc):** mọi `wait`/`watch_duration` dùng `tokio::select!` với cancel token.
Một run = một tokio task; stop run = cancel token.

**Crash-recovery:** lúc app khởi động, quét Run `status=running` mồ côi → `interrupted`
(đúng pattern app đã dọn PID profile cũ ở `lib.rs` setup).

---

## 10. Block catalog & mapping

Nhóm Navigation / Read / Interact / Flow / AI / Timing / Data như spec (37 block). Outbound
(`post`, `reply`, `submit`, `send`) là **block Interact bình thường**: tổ hợp `type_text` +
`click_*`, tự động đầy đủ; có cờ `dry_run` tùy chọn (mặc định false).

**Mapping → action (sửa các chỗ spec map sai):**
| Block | Thực thi đúng |
|---|---|
| open_url | `navigate` |
| go_back/forward/refresh | `evaluate_javascript` (`history.go`, `location.reload`) + `wait_for_load` |
| scroll | `evaluate_javascript` (easing script) + jitter từ `rand` |
| get_page_text/element | `get_page_content` |
| screenshot / get_url | `screenshot` / `get_page_info` |
| find_elements | `get_interactive_elements` |
| click / type / post / reply / submit | `click_by_index`/`click_element` + `type_text` |
| press_key (input thật) | **CDP `Input.*` / BiDi `input.performActions`** (cần expose tool mới — KHÔNG dùng dispatchEvent) |
| **switch/new/close tab** | **CDP `Target.*` / BiDi `browsingContext.create\|close`** (tool mới — KHÔNG dùng JS) |
| **upload file** | **CDP `DOM.setFileInputFiles` / BiDi `input.setFiles`** (tool mới — KHÔNG dùng JS) |
| run_js | `evaluate_javascript` |
| AI blocks | `get_page_content`/`screenshot` → `AiClient` → parse structured |

> **MVP: single-tab trước**, hoãn tab-management → tránh phần khó nhất giai đoạn đầu.

**Human-pacing helpers** (nhịp tự nhiên, đỡ nặng site): scroll easing, reading-pause theo độ
dài text, `wait_random` — dùng `rand`.

---

## 11. AI layer (`ai.rs`)
```rust
#[async_trait::async_trait]
pub trait AiClient: Send + Sync {
    async fn run(&self, req: AiRequest) -> Result<AiResult, AiError>; // trả JSON đã validate + token count
}
```
- Providers: anthropic / openai / gemini / ollama (qua `reqwest`).
- **Structured output, KHÔNG regex text LLM**: Anthropic dùng tool-use; OpenAI `response_format`
  json_schema; → parse chắc chắn vào struct.
- Mặc định model rẻ/nhanh: `claude-haiku-4-5-20251001`. Token budget/run (cap `max_ai_tokens`).
- **Fallback luôn có** khi `ai_available=false` (provider chưa cấu hình hoặc chưa verify):
  ai_decide→top-N interactive, ai_summarize→raw text truncate, ai_check→false, ai_write→template...
- **Riêng tư:** nội dung trang gửi ra provider → PII rời máy; cho chọn **Ollama local** cho luồng nhạy cảm; nêu rõ trong UI.

---

## 12. Scheduler & rotation (`scheduler.rs`)
- Tick-loop nền (tái dùng pattern `tauri::async_runtime::spawn` + global scheduler như `sync`).
- Trigger: `cron` (crate cron) | `interval` | `manual` | `on_event` (qua API server sẵn có).
- Rotation: round_robin (track `last_used_profile_id`) | random | least_used (đếm Run 24h) | all_parallel.
- **Concurrency:** `Semaphore(max_parallel)` + **khoá theo profile** (1 profile chỉ 1 run — tái dùng việc app track process profile; per-port BiDi pool cũng serialize sẵn).
- **Conflict detection:** profile bận → đổi profile/retry; cooldown; time_window; max_runs_per_day; offset ngẫu nhiên giữa profiles cho nhịp tự nhiên (tránh chạy đồng loạt một lúc).
- **Durability:** `next_run_at` + Run lưu SQLite → app restart không mất lịch; recover run mồ côi.

---

## 13. Tauri commands & frontend
- `commands.rs`: `create_scenario`, `update_scenario`, `list_scenarios`, `delete_scenario`,
  `run_scenario_now`, `stop_run`, `list_runs`, `get_run_steps`, `crud_schedule`,
  `crud_ai_provider`, `test_ai_provider`.
- Frontend `src/components/scenario/`: builder (palette + canvas nesting Loop/Condition +
  properties panel + AI/No-AI toggle + dry_run toggle), variable panel, run-monitor (run list +
  step timeline + detail + screenshot viewer). UI **chỉ gọi command**, không chạy engine.

---

## 14. Tái dùng từ codebase (đừng làm lại)
| Cần | Dùng lại |
|---|---|
| Thực thi action | `McpServer::dispatch_tool_call` → `pub(crate)` |
| Ghi JSON an toàn | `atomic_write` (`profile/manager.rs`) |
| Mã hoá secret | Argon2+AES-GCM (`settings_manager.rs`) |
| Task nền + scheduler pattern | `tauri::async_runtime::spawn`, `sync::get_global_scheduler` |
| Dọn state mồ côi lúc khởi động | pattern clear stale PID (`lib.rs` setup) |
| HTTP/LLM, random, regex, sqlite, time | `reqwest`, `rand`, `regex_lite`, `rusqlite`, `chrono`/`chrono_tz` (đều đã là dep) |
| CDP/BiDi cho tool mới (tab/upload/key) | `cloak_manager` (CDP) · BiDi layer trong `mcp_server.rs` |

---

## 15. Bảo mật, vận hành & trách nhiệm
- API key AI mã hoá at-rest; không log key.
- `run_js` = thực thi JS tuỳ ý — bản chất sản phẩm; cô lập + log đầy đủ.
- Caps + cancellation chặn runaway/chi phí AI ngoài kiểm soát.
- **Trách nhiệm người dùng (ToS):** auto-reply/auto-post tự động thường vi phạm điều khoản của
  Gmail/MXH/... và **dễ bị khóa tài khoản**; vì vậy `on_error`/retry/cooldown/rotation phải tốt
  để người dùng kiểm soát rủi ro. Tính năng dành cho tự động hóa **tài khoản/nội dung mình sở
  hữu/được ủy quyền**; không nhằm spam/fake-engagement/tạo tài khoản hàng loạt.

---

## 16. MVP & lộ trình (de-risk theo chiều dọc)
1. **MVP No-AI, manual trigger:** model + store(JSON) + interpolate + executor + `McpActionExecutor`,
   ~8 block (open_url, scroll, get_text, click_by_index, type/post, wait, condition, loop) → chạy
   thật end-to-end + StepLog SQLite. **Validate engine trước.**
2. **AI layer:** `AiClient` + structured output + fallback + token cap.
3. **Scheduler:** cron + rotation + conflict + durability + cancellation.
4. **Builder UI** (đắt nhất — sau khi engine vững).
5. **Polish:** export/import, version, tool tab/upload/press-key (CDP/BiDi), template sạch.

Ước lượng thực tế ~**14–17 tuần** (spec gốc 9–11 tuần là lạc quan ~40%).

---

## 17. Anti-pattern cần tránh
❌ Engine ở frontend JS · ❌ regex text LLM (dùng structured output) · ❌ tab/upload/press-key bằng
`dispatchEvent`/JS (dùng CDP/BiDi) · ❌ StepLog lưu JSON-file (dùng SQLite) · ❌ engine phụ thuộc
trực tiếp `McpServer` (dùng trait) · ❌ loop/AI không trần · ❌ chặn cứng outbound bằng human-gate
bắt buộc (đã bỏ — chỉ là cờ `dry_run` tùy chọn).

---

## 18. Mở rộng tương lai
- Webhook/`on_event` trigger qua `api_server` sẵn có.
- Marketplace template (export/import JSON đã có ở Phase 5).
- Per-step retry policy nâng cao (exponential backoff, jitter).

---

*Tài liệu thiết kế — chưa phải code. Cập nhật khi chốt chi tiết module.*
