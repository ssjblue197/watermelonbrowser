use axum::{
  body::Body,
  extract::State,
  http::{header, Request, StatusCode},
  middleware::{self, Next},
  response::{IntoResponse, Response},
  routing::{get, post},
  Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use tauri::AppHandle;
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::browser::ProxySettings;
use crate::cloud_auth::CLOUD_AUTH;
use crate::group_manager::GROUP_MANAGER;
use crate::profile::{BrowserProfile, ProfileManager};
use crate::proxy_manager::PROXY_MANAGER;
use crate::settings_manager::SettingsManager;

/// Live WebSocket connection to a Camoufox WebDriver-BiDi session.
type BidiWs =
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Parse our `bidi://127.0.0.1:<port>` sentinel (emitted by `get_cdp_ws_url`
/// for Camoufox) back into a port. Returns `None` for real CDP `ws://` URLs.
fn bidi_port(ws_url: &str) -> Option<u16> {
  ws_url.strip_prefix("bidi://127.0.0.1:")?.parse().ok()
}

/// A `page`-type CDP target that is actually the DevTools UI (`devtools://…`),
/// not a real web page. Automation must never select these — picking one makes
/// `type_text`/`run_js` operate on the DevTools document instead of the site.
fn is_devtools_target(t: &serde_json::Value) -> bool {
  t.get("url")
    .and_then(|v| v.as_str())
    .is_some_and(|u| u.starts_with("devtools://"))
}

/// Reshape a WebDriver-BiDi `script.evaluate` result into the CDP
/// `Runtime.evaluate` shape the interaction handlers already parse
/// (`{result:{value,type}}` on success, `{exceptionDetails:{text,...}}` on
/// throw).
fn bidi_eval_to_cdp(r: &serde_json::Value) -> serde_json::Value {
  if r.get("type").and_then(|v| v.as_str()) == Some("exception") {
    let text = r
      .get("exceptionDetails")
      .and_then(|e| e.get("text"))
      .and_then(|v| v.as_str())
      .unwrap_or("Evaluation exception");
    return serde_json::json!({
      "exceptionDetails": { "text": text, "exception": { "description": text } }
    });
  }
  let rv = r.get("result");
  let value = rv
    .and_then(|v| v.get("value"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
  let vtype = rv
    .and_then(|v| v.get("type"))
    .cloned()
    .unwrap_or(serde_json::json!("undefined"));
  serde_json::json!({ "result": { "value": value, "type": vtype } })
}

/// A cached, long-lived WebDriver-BiDi session for one Camoufox debug port.
/// Reused across tool calls (pooled per port) so we pay the connect +
/// `session.new` + `getTree` handshake once instead of on every command.
struct BidiConn {
  ws: BidiWs,
  /// Top-level browsing context (the open tab); stable across same-tab navigations.
  context: String,
  /// Monotonic BiDi command id.
  next_id: u64,
  /// Whether we've already `session.subscribe`d to `browsingContext.load`.
  load_subscribed: bool,
}

/// A browser-interaction operation expressed protocol-agnostically, so the
/// pooled-connection runner (`bidi_exec`) can execute it with reconnect-on-fail
/// without each call site duplicating the lock/retry dance.
enum BidiOp {
  Eval {
    expression: String,
    await_promise: bool,
    /// Wait for a `browsingContext.load` after evaluating (script may navigate).
    wait_load: bool,
  },
  Navigate {
    url: String,
  },
  Screenshot {
    format: String,
    quality: Option<i64>,
    full_page: bool,
  },
  LayoutMetrics,
  PerformKeys {
    actions: Vec<serde_json::Value>,
  },
  /// Set the files of a file `<input>` (resolved by CSS selector) — BiDi `input.setFiles`.
  SetFiles {
    selector: String,
    files: Vec<String>,
  },
  /// Enumerate open tabs (top-level browsing contexts) as `[{index,url}]`.
  ListTabs,
  /// Open a new tab; navigate to `url` when non-empty. Makes it the active context.
  NewTab {
    url: String,
  },
  /// Make the tab at `index` (from `ListTabs` order) the active context.
  SwitchTab {
    index: usize,
  },
  /// Close the tab at `index`; the active context resets to a remaining tab.
  CloseTab {
    index: usize,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
  pub name: String,
  pub description: String,
  pub input_schema: serde_json::Value,
}

/// JavaScript executed in the target page to enumerate visible interactive
/// elements. Returns a JSON string `{elements, count, truncated}` where
/// `elements` is the newline-joined labeled list. Live references are stashed
/// on `window.__watermelon_interactive` so subsequent `click_by_index` /
/// `type_by_index` calls can resolve `index → Element` without round-tripping
/// a selector. `__MAX_CHARS__` is substituted at call time.
const INTERACTIVE_ELEMENTS_JS: &str = r#"(() => {
  const SELECTORS = 'a, button, input, select, textarea, [role="button"], [role="link"], [role="checkbox"], [role="radio"], [role="tab"], [role="menuitem"], [role="combobox"], [role="option"], [contenteditable=""], [contenteditable="true"], [tabindex]:not([tabindex="-1"])';
  const ATTRS = ['type','name','id','role','aria-label','aria-checked','aria-expanded','placeholder','title','value','href','alt'];
  const MAX_CHARS = __MAX_CHARS__;
  const interactive = [];
  const lines = [];
  let truncated = false;
  let total = 0;
  const nodes = document.querySelectorAll(SELECTORS);
  for (const el of nodes) {
    if (el.disabled) continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const style = window.getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none' || style.opacity === '0') continue;
    const tag = el.tagName.toLowerCase();
    const parts = [];
    for (const a of ATTRS) {
      const v = el.getAttribute(a);
      if (v) parts.push(a + '="' + String(v).slice(0,100).replace(/"/g,'\\"') + '"');
    }
    let text = '';
    if (!['INPUT','TEXTAREA','SELECT'].includes(el.tagName)) {
      text = (el.innerText || el.textContent || '').trim().replace(/\s+/g,' ').slice(0,100);
    }
    const idx = interactive.length;
    const line = '[' + idx + ']<' + tag + (parts.length ? ' ' + parts.join(' ') : '') + '>' + text + '</' + tag + '>';
    if (total + line.length + 1 > MAX_CHARS) { truncated = true; break; }
    total += line.length + 1;
    interactive.push(el);
    lines.push(line);
  }
  window.__watermelon_interactive = interactive;
  return JSON.stringify({ elements: lines.join('\n'), count: interactive.length, truncated: truncated });
})()"#;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct McpRequest {
  jsonrpc: String,
  id: Option<serde_json::Value>,
  method: String,
  params: Option<serde_json::Value>,
}

const PROTOCOL_VERSION: &str = "2025-11-25";
const SERVER_NAME: &str = "watermelon-browser";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize)]
pub struct McpResponse {
  jsonrpc: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  id: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  result: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<McpError>,
}

#[derive(Debug, Serialize)]
pub struct McpError {
  code: i32,
  message: String,
}

const DEFAULT_MCP_PORT: u16 = 51080;

struct McpSession {
  initialized: bool,
}

struct McpServerInner {
  app_handle: Option<AppHandle>,
  token: Option<String>,
  shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
  sessions: HashMap<String, McpSession>,
}

#[derive(Clone)]
struct McpHttpState {
  server: &'static McpServer,
  token: String,
}

pub struct McpServer {
  inner: Arc<AsyncMutex<McpServerInner>>,
  is_running: AtomicBool,
  port: AtomicU16,
  /// Pooled Camoufox BiDi connections, keyed by debug port. The per-port
  /// `Mutex<Option<BidiConn>>` serializes commands to one browser (Firefox
  /// allows a single BiDi session) and is reset to `None` on a dead socket.
  bidi_pool: AsyncMutex<HashMap<u16, Arc<AsyncMutex<Option<BidiConn>>>>>,
  /// Active CDP tab per profile (Chromium/Cloak), keyed by profile id → target id.
  /// `get_cdp_ws_url` prefers this target so post-`switch_tab` actions follow the
  /// chosen tab; falls back to the first page target when unset or the target is
  /// gone (so single-tab flows are unchanged). Camoufox tracks the active tab on
  /// the pooled `BidiConn.context` instead.
  active_targets: AsyncMutex<HashMap<String, String>>,
}

impl McpServer {
  fn new() -> Self {
    Self {
      inner: Arc::new(AsyncMutex::new(McpServerInner {
        app_handle: None,
        token: None,
        shutdown_tx: None,
        sessions: HashMap::new(),
      })),
      is_running: AtomicBool::new(false),
      port: AtomicU16::new(0),
      bidi_pool: AsyncMutex::new(HashMap::new()),
      active_targets: AsyncMutex::new(HashMap::new()),
    }
  }

  pub fn instance() -> &'static McpServer {
    &MCP_SERVER
  }

  pub fn is_running(&self) -> bool {
    self.is_running.load(Ordering::SeqCst)
  }

  async fn require_paid_subscription(_feature: &str) -> Result<(), McpError> {
    // Pro gate removed: all MCP tools are available without a paid subscription.
    Ok(())
  }

  pub fn get_port(&self) -> Option<u16> {
    let port = self.port.load(Ordering::SeqCst);
    if port > 0 {
      Some(port)
    } else {
      None
    }
  }

  pub async fn start(&self, app_handle: AppHandle) -> Result<u16, String> {
    if self.is_running() {
      return Err("MCP server is already running".to_string());
    }

    let settings_manager = SettingsManager::instance();
    let settings = settings_manager
      .load_settings()
      .map_err(|e| format!("Failed to load settings: {e}"))?;

    // Get or generate token
    let existing_token = settings_manager
      .get_mcp_token(&app_handle)
      .await
      .ok()
      .flatten();

    let token = if let Some(t) = existing_token {
      t
    } else {
      settings_manager
        .generate_mcp_token(&app_handle)
        .await
        .map_err(|e| format!("Failed to generate MCP token: {e}"))?
    };

    // Determine port (use saved port, or try default, or random)
    let preferred_port = settings.mcp_port.unwrap_or(DEFAULT_MCP_PORT);
    let actual_port = self.bind_to_available_port(preferred_port).await?;

    // Save port if it changed
    if settings.mcp_port != Some(actual_port) {
      let mut new_settings = settings;
      new_settings.mcp_port = Some(actual_port);
      settings_manager
        .save_settings(&new_settings)
        .map_err(|e| format!("Failed to save settings: {e}"))?;
    }

    // Store state
    let mut inner = self.inner.lock().await;
    inner.app_handle = Some(app_handle);
    inner.token = Some(token.clone());

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    inner.shutdown_tx = Some(shutdown_tx);

    self.port.store(actual_port, Ordering::SeqCst);
    self.is_running.store(true, Ordering::SeqCst);

    // Start HTTP server in background
    let http_state = McpHttpState {
      server: McpServer::instance(),
      token,
    };
    tokio::spawn(Self::run_http_server(actual_port, http_state, shutdown_rx));

    log::info!("[mcp] Server started on port {}", actual_port);
    Ok(actual_port)
  }

  async fn bind_to_available_port(&self, preferred: u16) -> Result<u16, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], preferred));
    if TcpListener::bind(addr).await.is_ok() {
      return Ok(preferred);
    }

    for _ in 0..10 {
      let port = 51000 + (rand::random::<u16>() % 1000);
      let addr = SocketAddr::from(([127, 0, 0, 1], port));
      if TcpListener::bind(addr).await.is_ok() {
        return Ok(port);
      }
    }

    Err("Could not find available port for MCP server".to_string())
  }

  async fn run_http_server(
    port: u16,
    state: McpHttpState,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
  ) {
    let app = Router::new()
      .route(
        "/mcp/{token}",
        post(Self::handle_mcp_post)
          .get(Self::handle_mcp_get)
          .delete(Self::handle_mcp_delete),
      )
      .route(
        "/mcp",
        post(Self::handle_mcp_post)
          .get(Self::handle_mcp_get)
          .delete(Self::handle_mcp_delete),
      )
      .route("/health", get(Self::handle_health))
      .layer(middleware::from_fn_with_state(
        state.clone(),
        Self::auth_middleware,
      ))
      .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let server = async {
      match TcpListener::bind(addr).await {
        Ok(listener) => {
          log::info!("[mcp] Server listening on http://127.0.0.1:{}/mcp", port);
          if let Err(e) = axum::serve(listener, app).await {
            log::error!("[mcp] Server error: {}", e);
          }
        }
        Err(e) => {
          log::error!("[mcp] Failed to bind on port {}: {}", port, e);
        }
      }
    };

    tokio::select! {
      _ = server => {},
      _ = shutdown_rx => {
        log::info!("[mcp] Server shutting down");
      },
    }
  }

  async fn auth_middleware(
    State(state): State<McpHttpState>,
    req: Request<Body>,
    next: Next,
  ) -> Result<Response, StatusCode> {
    let path = req.uri().path();

    if path == "/health" {
      return Ok(next.run(req).await);
    }

    // Check token from URL path: /mcp/{token}
    let path_token = path
      .strip_prefix("/mcp/")
      .filter(|t| !t.is_empty() && !t.contains('/'));

    // Check token from Authorization header
    let header_token = req
      .headers()
      .get(header::AUTHORIZATION)
      .and_then(|h| h.to_str().ok())
      .and_then(|h| h.strip_prefix("Bearer "));

    let valid =
      path_token == Some(state.token.as_str()) || header_token == Some(state.token.as_str());

    if !valid {
      return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(req).await)
  }

  async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({
      "status": "ok",
      "server": SERVER_NAME,
      "version": SERVER_VERSION,
      "protocolVersion": PROTOCOL_VERSION,
    }))
  }

  async fn handle_mcp_get() -> impl IntoResponse {
    // We don't support server-initiated SSE streams
    StatusCode::METHOD_NOT_ALLOWED
  }

  async fn handle_mcp_delete(
    State(state): State<McpHttpState>,
    req: Request<Body>,
  ) -> impl IntoResponse {
    let session_id = req
      .headers()
      .get("mcp-session-id")
      .and_then(|h| h.to_str().ok())
      .map(|s| s.to_string());

    if let Some(sid) = session_id {
      let mut inner = state.server.inner.lock().await;
      inner.sessions.remove(&sid);
      log::info!("[mcp] Session terminated: {}", sid);
    }

    StatusCode::OK
  }

  async fn handle_mcp_post(State(state): State<McpHttpState>, req: Request<Body>) -> Response {
    let session_id = req
      .headers()
      .get("mcp-session-id")
      .and_then(|h| h.to_str().ok())
      .map(|s| s.to_string());

    let body_bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
      Ok(b) => b,
      Err(_) => {
        return (StatusCode::BAD_REQUEST, "Invalid request body").into_response();
      }
    };

    let request: McpRequest = match serde_json::from_slice(&body_bytes) {
      Ok(r) => r,
      Err(_) => {
        return (StatusCode::BAD_REQUEST, "Invalid JSON").into_response();
      }
    };

    let is_notification = request.id.is_none();
    let method = request.method.clone();

    // Handle initialize (no session required)
    if method == "initialize" {
      let response = state.server.handle_initialize(request).await;
      match response {
        Ok((session_id, result)) => {
          let body = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(result.0),
            result: Some(result.1),
            error: None,
          };
          Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header("mcp-session-id", &session_id)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
        }
        Err((id, error)) => {
          let body = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            result: None,
            error: Some(error),
          };
          Json(body).into_response()
        }
      }
    } else if is_notification {
      // Notifications (like notifications/initialized) -> 202 Accepted
      if method == "notifications/initialized" {
        if let Some(sid) = &session_id {
          let mut inner = state.server.inner.lock().await;
          if let Some(session) = inner.sessions.get_mut(sid) {
            session.initialized = true;
          }
        }
      }
      StatusCode::ACCEPTED.into_response()
    } else {
      // Validate session exists
      if let Some(sid) = &session_id {
        let inner = state.server.inner.lock().await;
        if !inner.sessions.contains_key(sid) {
          return StatusCode::NOT_FOUND.into_response();
        }
      }

      let response = state.server.handle_request(request).await;
      Json(response).into_response()
    }
  }

  pub async fn stop(&self) -> Result<(), String> {
    if !self.is_running() {
      return Err("MCP server is not running".to_string());
    }

    let mut inner = self.inner.lock().await;
    inner.app_handle = None;
    inner.token = None;
    inner.sessions.clear();

    // Send shutdown signal
    if let Some(tx) = inner.shutdown_tx.take() {
      let _ = tx.send(());
    }

    self.port.store(0, Ordering::SeqCst);
    self.is_running.store(false, Ordering::SeqCst);

    log::info!("[mcp] Server stopped");
    Ok(())
  }

  pub fn get_tools(&self) -> Vec<McpTool> {
    vec![
      McpTool {
        name: "list_profiles".to_string(),
        description: "List all Camoufox and Cloak browser profiles".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "get_profile".to_string(),
        description: "Get details of a specific browser profile".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to retrieve"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "run_profile".to_string(),
        description: "Launch a browser profile with an optional URL. Requires an active Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to launch"
            },
            "url": {
              "type": "string",
              "description": "Optional URL to open in the browser"
            },
            "headless": {
              "type": "boolean",
              "description": "Run the browser in headless mode"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "kill_profile".to_string(),
        description: "Stop a running browser profile. Requires an active Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to stop"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "create_profile".to_string(),
        description: "Create a new browser profile".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "name": {
              "type": "string",
              "description": "Name for the new profile"
            },
            "browser": {
              "type": "string",
              "enum": ["camoufox", "cloak"],
              "description": "Browser engine to use"
            },
            "proxy_id": {
              "type": "string",
              "description": "Optional proxy UUID to assign"
            },
            "launch_hook": {
              "type": "string",
              "description": "Optional HTTP(S) URL to call before launch for transient proxy overrides"
            },
            "group_id": {
              "type": "string",
              "description": "Optional group UUID to assign"
            },
            "tags": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Optional tags for the profile"
            }
          },
          "required": ["name", "browser"]
        }),
      },
      McpTool {
        name: "update_profile".to_string(),
        description: "Update an existing browser profile's settings".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to update"
            },
            "name": {
              "type": "string",
              "description": "New name for the profile"
            },
            "proxy_id": {
              "type": "string",
              "description": "Proxy UUID to assign (empty string to remove)"
            },
            "launch_hook": {
              "type": "string",
              "description": "Launch hook URL to assign (empty string to remove)"
            },
            "group_id": {
              "type": "string",
              "description": "Group UUID to assign (empty string to remove)"
            },
            "tags": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Tags for the profile (replaces existing tags)"
            },
            "extension_group_id": {
              "type": "string",
              "description": "Extension group UUID to assign (empty string to remove)"
            },
            "proxy_bypass_rules": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Proxy bypass rules (replaces existing rules)"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "delete_profile".to_string(),
        description: "Delete a browser profile and all its data".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to delete"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "list_tags".to_string(),
        description: "List all tags used across profiles".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "list_proxies".to_string(),
        description: "List all configured proxies".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "get_profile_status".to_string(),
        description: "Check if a browser profile is currently running".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to check"
            }
          },
          "required": ["profile_id"]
        }),
      },
      // Group management tools
      McpTool {
        name: "list_groups".to_string(),
        description: "List all profile groups".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "get_group".to_string(),
        description: "Get details of a specific group".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "group_id": {
              "type": "string",
              "description": "The UUID of the group to retrieve"
            }
          },
          "required": ["group_id"]
        }),
      },
      McpTool {
        name: "create_group".to_string(),
        description: "Create a new profile group".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "name": {
              "type": "string",
              "description": "The name for the new group"
            }
          },
          "required": ["name"]
        }),
      },
      McpTool {
        name: "update_group".to_string(),
        description: "Update an existing group's name".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "group_id": {
              "type": "string",
              "description": "The UUID of the group to update"
            },
            "name": {
              "type": "string",
              "description": "The new name for the group"
            }
          },
          "required": ["group_id", "name"]
        }),
      },
      McpTool {
        name: "delete_group".to_string(),
        description: "Delete a profile group".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "group_id": {
              "type": "string",
              "description": "The UUID of the group to delete"
            }
          },
          "required": ["group_id"]
        }),
      },
      McpTool {
        name: "assign_profiles_to_group".to_string(),
        description: "Assign one or more profiles to a group".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_ids": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Array of profile UUIDs to assign"
            },
            "group_id": {
              "type": "string",
              "description": "The UUID of the group to assign to (null to remove from group)"
            }
          },
          "required": ["profile_ids"]
        }),
      },
      // Full proxy management tools
      McpTool {
        name: "get_proxy".to_string(),
        description: "Get details of a specific proxy".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "proxy_id": {
              "type": "string",
              "description": "The UUID of the proxy to retrieve"
            }
          },
          "required": ["proxy_id"]
        }),
      },
      McpTool {
        name: "create_proxy".to_string(),
        description: "Create a new proxy configuration.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "name": {
              "type": "string",
              "description": "The name for the new proxy"
            },
            "proxy_type": {
              "type": "string",
              "enum": ["http", "https", "socks4", "socks5"],
              "description": "The type of proxy (for regular proxies)"
            },
            "host": {
              "type": "string",
              "description": "The proxy host address (for regular proxies)"
            },
            "port": {
              "type": "integer",
              "description": "The proxy port number (for regular proxies)"
            },
            "username": {
              "type": "string",
              "description": "Optional username for authentication (for regular proxies)"
            },
            "password": {
              "type": "string",
              "description": "Optional password for authentication (for regular proxies)"
            }
          },
          "required": ["name", "proxy_type", "host", "port"]
        }),
      },
      McpTool {
        name: "update_proxy".to_string(),
        description: "Update an existing proxy configuration".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "proxy_id": {
              "type": "string",
              "description": "The UUID of the proxy to update"
            },
            "name": {
              "type": "string",
              "description": "New name for the proxy"
            },
            "proxy_type": {
              "type": "string",
              "enum": ["http", "https", "socks4", "socks5"],
              "description": "The type of proxy (for regular proxies)"
            },
            "host": {
              "type": "string",
              "description": "The proxy host address (for regular proxies)"
            },
            "port": {
              "type": "integer",
              "description": "The proxy port number (for regular proxies)"
            },
            "username": {
              "type": "string",
              "description": "Optional username for authentication (for regular proxies)"
            },
            "password": {
              "type": "string",
              "description": "Optional password for authentication (for regular proxies)"
            }
          },
          "required": ["proxy_id"]
        }),
      },
      McpTool {
        name: "delete_proxy".to_string(),
        description: "Delete a proxy configuration".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "proxy_id": {
              "type": "string",
              "description": "The UUID of the proxy to delete"
            }
          },
          "required": ["proxy_id"]
        }),
      },
      McpTool {
        name: "export_proxies".to_string(),
        description: "Export all proxy configurations".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "format": {
              "type": "string",
              "enum": ["json", "txt"],
              "description": "Export format (json for structured data, txt for URL format)"
            }
          },
          "required": ["format"]
        }),
      },
      McpTool {
        name: "import_proxies".to_string(),
        description: "Import proxy configurations from JSON or TXT content".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "content": {
              "type": "string",
              "description": "The proxy configuration content to import"
            },
            "format": {
              "type": "string",
              "enum": ["json", "txt"],
              "description": "Import format (json or txt)"
            },
            "name_prefix": {
              "type": "string",
              "description": "Optional prefix for imported proxy names (default: 'Imported')"
            }
          },
          "required": ["content", "format"]
        }),
      },
      // VPN management tools
      McpTool {
        name: "import_vpn".to_string(),
        description: "Import a WireGuard (.conf) configuration".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "content": {
              "type": "string",
              "description": "Raw WireGuard config file content"
            },
            "filename": {
              "type": "string",
              "description": "Original filename (.conf)"
            },
            "name": {
              "type": "string",
              "description": "Optional display name for the VPN config"
            }
          },
          "required": ["content", "filename"]
        }),
      },
      McpTool {
        name: "list_vpn_configs".to_string(),
        description: "List all stored VPN configurations".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "delete_vpn".to_string(),
        description: "Delete a VPN configuration".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "vpn_id": {
              "type": "string",
              "description": "The UUID of the VPN config to delete"
            }
          },
          "required": ["vpn_id"]
        }),
      },
      McpTool {
        name: "connect_vpn".to_string(),
        description: "Connect to a VPN configuration".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "vpn_id": {
              "type": "string",
              "description": "The UUID of the VPN config to connect"
            }
          },
          "required": ["vpn_id"]
        }),
      },
      McpTool {
        name: "disconnect_vpn".to_string(),
        description: "Disconnect from a VPN".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "vpn_id": {
              "type": "string",
              "description": "The UUID of the VPN to disconnect"
            }
          },
          "required": ["vpn_id"]
        }),
      },
      McpTool {
        name: "get_vpn_status".to_string(),
        description: "Get the connection status of a VPN".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "vpn_id": {
              "type": "string",
              "description": "The UUID of the VPN to check"
            }
          },
          "required": ["vpn_id"]
        }),
      },
      // Fingerprint management tools
      McpTool {
        name: "get_profile_fingerprint".to_string(),
        description: "Get the fingerprint configuration for a Camoufox or Cloak profile"
          .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "update_profile_fingerprint".to_string(),
        description:
          "Update the fingerprint configuration for a Camoufox or Cloak profile. Requires an active Pro subscription."
            .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to update"
            },
            "fingerprint": {
              "type": "string",
              "description": "JSON string of the fingerprint configuration, or null to clear"
            },
            "os": {
              "type": "string",
              "enum": ["windows", "macos", "linux"],
              "description": "Operating system for fingerprint generation"
            },
            "randomize_fingerprint_on_launch": {
              "type": "boolean",
              "description": "Whether to generate a new fingerprint on every launch"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "update_profile_proxy_bypass_rules".to_string(),
        description:
          "Update proxy bypass rules for a profile. Requests matching these rules will connect directly, bypassing the proxy."
            .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to update"
            },
            "rules": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Array of bypass rules. Supports hostnames (e.g. 'example.com'), IP addresses, and regex patterns."
            }
          },
          "required": ["profile_id", "rules"]
        }),
      },
      McpTool {
        name: "update_profile_dns_blocklist".to_string(),
        description:
          "Update the DNS blocklist level for a profile. Blocks ads, trackers, and malware domains at the proxy level."
            .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to update"
            },
            "level": {
              "type": "string",
              "enum": ["none", "light", "normal", "pro", "pro_plus", "ultimate"],
              "description": "DNS blocklist level. 'none' disables blocking."
            }
          },
          "required": ["profile_id", "level"]
        }),
      },
      McpTool {
        name: "get_dns_blocklist_status".to_string(),
        description: "Get the cache status of all DNS blocklist tiers including entry counts and freshness.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "list_extensions".to_string(),
        description: "List all managed browser extensions. Requires Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "list_extension_groups".to_string(),
        description: "List all extension groups. Requires Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "create_extension_group".to_string(),
        description: "Create a new extension group. Requires Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "name": { "type": "string", "description": "Name for the extension group" }
          },
          "required": ["name"]
        }),
      },
      McpTool {
        name: "delete_extension".to_string(),
        description: "Delete a managed extension. Requires Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "extension_id": { "type": "string", "description": "The extension ID to delete" }
          },
          "required": ["extension_id"]
        }),
      },
      McpTool {
        name: "delete_extension_group".to_string(),
        description: "Delete an extension group. Requires Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "group_id": { "type": "string", "description": "The extension group ID to delete" }
          },
          "required": ["group_id"]
        }),
      },
      McpTool {
        name: "assign_extension_group_to_profile".to_string(),
        description: "Assign an extension group to a profile, or remove the assignment. Requires Pro subscription.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The profile ID" },
            "extension_group_id": { "type": "string", "description": "The extension group ID, or empty string to remove" }
          },
          "required": ["profile_id"]
        }),
      },
      // Cookie management tools
      McpTool {
        name: "import_profile_cookies".to_string(),
        description: "Import cookies into a Camoufox or Cloak profile from a JSON array (Puppeteer / EditThisCookie format) or a Netscape cookies.txt. Format is auto-detected. The browser must not be running.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the target profile"
            },
            "content": {
              "type": "string",
              "description": "Raw cookie file content (JSON array or Netscape cookies.txt)"
            }
          },
          "required": ["profile_id", "content"]
        }),
      },
      // Team lock tools
      McpTool {
        name: "get_team_locks".to_string(),
        description: "List all active team profile locks. Requires team plan.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {},
          "required": []
        }),
      },
      McpTool {
        name: "get_team_lock_status".to_string(),
        description: "Check if a profile is locked by a team member. Requires team plan.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the profile to check"
            }
          },
          "required": ["profile_id"]
        }),
      },
      // Browser interaction tools
      McpTool {
        name: "navigate".to_string(),
        description: "Navigate a running browser profile to a URL. Waits for the page to fully load before returning.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "url": {
              "type": "string",
              "description": "The URL to navigate to"
            }
          },
          "required": ["profile_id", "url"]
        }),
      },
      McpTool {
        name: "screenshot".to_string(),
        description: "Take a screenshot of the current page in a running browser profile. Returns base64-encoded image."
          .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "format": {
              "type": "string",
              "enum": ["png", "jpeg", "webp"],
              "description": "Image format (default: png)"
            },
            "quality": {
              "type": "integer",
              "description": "Image quality 0-100 for jpeg/webp (default: 80)"
            },
            "full_page": {
              "type": "boolean",
              "description": "Capture the full scrollable page (default: false)"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "evaluate_javascript".to_string(),
        description:
          "Execute JavaScript in the context of the current page and return the result. Works with both static and dynamically-generated content. Set wait_for_load=true if the script triggers navigation (e.g., form.submit())."
            .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "expression": {
              "type": "string",
              "description": "JavaScript expression to evaluate"
            },
            "await_promise": {
              "type": "boolean",
              "description": "Whether to await the result if it's a Promise (default: false)"
            },
            "wait_for_load": {
              "type": "boolean",
              "description": "Wait for page load after execution, use when the script triggers navigation like form.submit() (default: false)"
            }
          },
          "required": ["profile_id", "expression"]
        }),
      },
      McpTool {
        name: "click_element".to_string(),
        description: "Click on an element identified by a CSS selector. If the click triggers a page navigation, waits for the new page to load before returning.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "selector": {
              "type": "string",
              "description": "CSS selector for the element to click"
            }
          },
          "required": ["profile_id", "selector"]
        }),
      },
      McpTool {
        name: "type_text".to_string(),
        description: "Focus an element by CSS selector and type text into it. By default uses realistic human-like typing with variable speed, natural errors, and self-corrections. Only set instant=true when you are certain the target does not have bot detection (e.g. browser address bars, developer tools, internal apps) — using instant on public websites risks the profile being flagged as a bot.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "selector": {
              "type": "string",
              "description": "CSS selector for the input element"
            },
            "text": {
              "type": "string",
              "description": "Text to type into the element"
            },
            "clear_first": {
              "type": "boolean",
              "description": "Clear the input before typing (default: true)"
            },
            "instant": {
              "type": "boolean",
              "description": "Paste all text at once instead of human typing. WARNING: only use on targets without bot detection — using this on public websites risks the profile being flagged."
            },
            "wpm": {
              "type": "number",
              "description": "Target words per minute for human typing (default: 80)"
            }
          },
          "required": ["profile_id", "selector", "text"]
        }),
      },
      McpTool {
        name: "press_key".to_string(),
        description: "Press a single keyboard key on the currently focused element, optionally with modifier keys held. Use for keys that type_text cannot send — Enter to submit, Tab to move focus, Escape to dismiss, arrows, or shortcuts like Control+A. Sent via real CDP/BiDi input events (not synthetic JS), so it is indistinguishable from a human keypress.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "key": {
              "type": "string",
              "description": "Key to press. Named keys: Enter, Tab, Escape, Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, Space. Or a single printable character (e.g. 'a')."
            },
            "modifiers": {
              "type": "array",
              "items": { "type": "string", "enum": ["Control", "Shift", "Alt", "Meta"] },
              "description": "Modifier keys to hold while pressing (e.g. [\"Control\"] for Ctrl+key)."
            }
          },
          "required": ["profile_id", "key"]
        }),
      },
      McpTool {
        name: "upload_file".to_string(),
        description: "Set the file(s) of a file <input> element (selected by CSS selector) without opening the OS file picker. Paths must be absolute and exist on the machine running the browser.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The UUID of the running profile" },
            "selector": { "type": "string", "description": "CSS selector for the <input type=file> element" },
            "files": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Absolute paths of files to attach (one for single-file inputs)"
            }
          },
          "required": ["profile_id", "selector", "files"]
        }),
      },
      McpTool {
        name: "list_tabs".to_string(),
        description: "List the open tabs of a profile as an indexed array of {index, url, title}. The index is stable for use with switch_tab / close_tab until tabs are opened or closed.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The UUID of the running profile" }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "new_tab".to_string(),
        description: "Open a new tab and make it the active tab for subsequent actions. Optionally navigate it to a URL.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The UUID of the running profile" },
            "url": { "type": "string", "description": "Optional URL to open in the new tab" }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "switch_tab".to_string(),
        description: "Make the tab at the given index (from list_tabs order) the active tab; subsequent navigate/click/type actions target it.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The UUID of the running profile" },
            "index": { "type": "integer", "description": "Zero-based tab index from list_tabs" }
          },
          "required": ["profile_id", "index"]
        }),
      },
      McpTool {
        name: "close_tab".to_string(),
        description: "Close the tab at the given index (from list_tabs order). The active tab resets to a remaining tab.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The UUID of the running profile" },
            "index": { "type": "integer", "description": "Zero-based tab index from list_tabs" }
          },
          "required": ["profile_id", "index"]
        }),
      },
      McpTool {
        name: "get_page_content".to_string(),
        description:
          "Get the content of the current page. Works with both static HTML and JavaScript-rendered content."
            .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "format": {
              "type": "string",
              "enum": ["html", "text"],
              "description": "Content format: 'html' for full HTML, 'text' for visible text only (default: text)"
            },
            "selector": {
              "type": "string",
              "description": "Optional CSS selector to get content of a specific element instead of the whole page"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "get_page_info".to_string(),
        description: "Get metadata about the current page including URL, title, and readiness state"
          .to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "get_interactive_elements".to_string(),
        description: "Enumerate visible interactive elements on the page (buttons, links, inputs, etc.) as a compact indexed list. The returned indices are stable for the current page and can be used with click_by_index and type_by_index instead of guessing CSS selectors. Call this before click_by_index / type_by_index, and re-call after any navigation or major DOM change. Far cheaper in tokens than get_page_content for agentic browsing.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "max_chars": {
              "type": "integer",
              "description": "Cap on the serialized output length (default: 40000). The response carries a `truncated` flag if the list was cut off — narrow the viewport or scroll if you need elements past the cutoff."
            }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "click_by_index".to_string(),
        description: "Click the element at the given index from the last get_interactive_elements call. Indices are valid until the next navigation. If the click triggers navigation, waits for the new page to load before returning.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "index": {
              "type": "integer",
              "description": "Zero-based index from the last get_interactive_elements response"
            }
          },
          "required": ["profile_id", "index"]
        }),
      },
      McpTool {
        name: "type_by_index".to_string(),
        description: "Focus the element at the given index from the last get_interactive_elements call and type text into it. Same human-like-typing defaults as type_text; only set instant=true when you're sure the target lacks bot detection.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": {
              "type": "string",
              "description": "The UUID of the running profile"
            },
            "index": {
              "type": "integer",
              "description": "Zero-based index from the last get_interactive_elements response"
            },
            "text": {
              "type": "string",
              "description": "Text to type into the element"
            },
            "clear_first": {
              "type": "boolean",
              "description": "Clear the input before typing (default: true)"
            },
            "instant": {
              "type": "boolean",
              "description": "Paste all text at once instead of human typing. WARNING: only use on targets without bot detection."
            },
            "wpm": {
              "type": "number",
              "description": "Target words per minute for human typing (default: 80)"
            }
          },
          "required": ["profile_id", "index", "text"]
        }),
      },
      // Scenario automation
      McpTool {
        name: "run_scenario".to_string(),
        description: "Run a scenario-automation flow on a running profile and return the per-step result. Provide either `scenario` (inline JSON) or `scenario_id` (loaded from the store).".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "profile_id": { "type": "string", "description": "The UUID of the running profile to drive" },
            "scenario": { "type": "object", "description": "Inline scenario definition (id, name, blocks, ...)" },
            "scenario_id": { "type": "string", "description": "Load a saved scenario by id instead of inline" },
            "triggered_by": { "type": "string", "description": "manual | schedule | api (default: api)" }
          },
          "required": ["profile_id"]
        }),
      },
      McpTool {
        name: "list_scenario_runs".to_string(),
        description: "List recent scenario runs from the run history.".to_string(),
        input_schema: serde_json::json!({
          "type": "object",
          "properties": {
            "limit": { "type": "integer", "description": "Max rows to return (default: 50)" }
          },
          "required": []
        }),
      },
    ]
  }

  async fn handle_initialize(
    &self,
    request: McpRequest,
  ) -> Result<(String, (serde_json::Value, serde_json::Value)), (serde_json::Value, McpError)> {
    let id = request.id.clone().unwrap_or(serde_json::Value::Null);

    if !self.is_running() {
      return Err((
        id,
        McpError {
          code: -32001,
          message: "MCP server is not running".to_string(),
        },
      ));
    }

    // Create session
    let session_id = Uuid::new_v4().to_string();
    {
      let mut inner = self.inner.lock().await;
      inner
        .sessions
        .insert(session_id.clone(), McpSession { initialized: false });
    }

    let result = serde_json::json!({
      "protocolVersion": PROTOCOL_VERSION,
      "capabilities": {
        "tools": {
          "listChanged": false
        }
      },
      "serverInfo": {
        "name": SERVER_NAME,
        "version": SERVER_VERSION,
      },
      "instructions": "WaterMelon Browser MCP server. Use tools/list to discover available browser automation tools."
    });

    log::info!("[mcp] New session initialized: {}", session_id);
    Ok((session_id, (id, result)))
  }

  pub async fn handle_request(&self, request: McpRequest) -> McpResponse {
    let id = request.id.clone().unwrap_or(serde_json::Value::Null);

    if !self.is_running() {
      return McpResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(id),
        result: None,
        error: Some(McpError {
          code: -32001,
          message: "MCP server is not running".to_string(),
        }),
      };
    }

    let result = match request.method.as_str() {
      "ping" => Ok(serde_json::json!({})),
      "tools/list" => self.handle_tools_list().await,
      "tools/call" => self.handle_tool_call(request.params).await,
      _ => Err(McpError {
        code: -32601,
        message: format!("Method not found: {}", request.method),
      }),
    };

    match result {
      Ok(value) => McpResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(id),
        result: Some(value),
        error: None,
      },
      Err(error) => McpResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(id),
        result: None,
        error: Some(error),
      },
    }
  }

  async fn handle_tools_list(&self) -> Result<serde_json::Value, McpError> {
    Ok(serde_json::json!({
      "tools": self.get_tools()
    }))
  }

  async fn handle_tool_call(
    &self,
    params: Option<serde_json::Value>,
  ) -> Result<serde_json::Value, McpError> {
    let params = params.ok_or_else(|| McpError {
      code: -32602,
      message: "Missing parameters".to_string(),
    })?;

    let tool_name = params
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing tool name".to_string(),
      })?;

    let arguments = params
      .get("arguments")
      .cloned()
      .unwrap_or(serde_json::json!({}));

    // Surface the call in logs so customer reports show which tools the MCP
    // client is actually invoking (and therefore which gate any subsequent
    // error came from). Log only the tool name and the profile_id arg —
    // arbitrary URLs / JS / selectors can be sensitive.
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .unwrap_or("<none>");
    log::info!("[mcp] tools/call name={tool_name} profile_id={profile_id}");

    let started = std::time::Instant::now();
    let result = self.dispatch_tool_call(tool_name, &arguments).await;
    let elapsed_ms = started.elapsed().as_millis();
    match &result {
      Ok(_) => {
        log::info!(
          "[mcp] tools/call name={tool_name} profile_id={profile_id} -> ok ({elapsed_ms} ms)"
        );
      }
      Err(e) => {
        log::warn!(
          "[mcp] tools/call name={tool_name} profile_id={profile_id} -> error code={} msg={:?} ({elapsed_ms} ms)",
          e.code,
          e.message
        );
      }
    }
    result
  }

  pub(crate) async fn dispatch_tool_call(
    &self,
    tool_name: &str,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    match tool_name {
      "list_profiles" => self.handle_list_profiles().await,
      "get_profile" => self.handle_get_profile(arguments).await,
      "run_profile" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_run_profile(arguments).await
      }
      "kill_profile" => self.handle_kill_profile(arguments).await,
      "create_profile" => self.handle_create_profile(arguments).await,
      "update_profile" => self.handle_update_profile(arguments).await,
      "add_profile_tag" => self.handle_add_profile_tag(arguments).await,
      "delete_profile" => self.handle_delete_profile(arguments).await,
      "list_tags" => self.handle_list_tags().await,
      "list_proxies" => self.handle_list_proxies().await,
      "get_profile_status" => self.handle_get_profile_status(arguments).await,
      // Group management
      "list_groups" => self.handle_list_groups().await,
      "get_group" => self.handle_get_group(arguments).await,
      "create_group" => self.handle_create_group(arguments).await,
      "update_group" => self.handle_update_group(arguments).await,
      "delete_group" => self.handle_delete_group(arguments).await,
      "assign_profiles_to_group" => self.handle_assign_profiles_to_group(arguments).await,
      // Full proxy management
      "get_proxy" => self.handle_get_proxy(arguments).await,
      "create_proxy" => self.handle_create_proxy(arguments).await,
      "update_proxy" => self.handle_update_proxy(arguments).await,
      "delete_proxy" => self.handle_delete_proxy(arguments).await,
      // Proxy import/export
      "export_proxies" => self.handle_export_proxies(arguments).await,
      "import_proxies" => self.handle_import_proxies(arguments).await,
      // VPN management
      "import_vpn" => self.handle_import_vpn(arguments).await,
      "list_vpn_configs" => self.handle_list_vpn_configs().await,
      "delete_vpn" => self.handle_delete_vpn(arguments).await,
      "connect_vpn" => self.handle_connect_vpn(arguments).await,
      "disconnect_vpn" => self.handle_disconnect_vpn(arguments).await,
      "get_vpn_status" => self.handle_get_vpn_status(arguments).await,
      // Fingerprint management — viewing and editing both require a paid plan.
      "get_profile_fingerprint" => {
        Self::require_paid_subscription("Fingerprint").await?;
        self.handle_get_profile_fingerprint(arguments).await
      }
      "update_profile_fingerprint" => {
        Self::require_paid_subscription("Fingerprint").await?;
        self.handle_update_profile_fingerprint(arguments).await
      }
      "update_profile_proxy_bypass_rules" => {
        self
          .handle_update_profile_proxy_bypass_rules(arguments)
          .await
      }
      // DNS blocklist management
      "update_profile_dns_blocklist" => self.handle_update_profile_dns_blocklist(arguments).await,
      "get_dns_blocklist_status" => self.handle_get_dns_blocklist_status().await,
      // Extension management
      "list_extensions" => self.handle_list_extensions().await,
      "list_extension_groups" => self.handle_list_extension_groups().await,
      "create_extension_group" => self.handle_create_extension_group(arguments).await,
      "delete_extension" => self.handle_delete_extension_mcp(arguments).await,
      "delete_extension_group" => self.handle_delete_extension_group_mcp(arguments).await,
      "assign_extension_group_to_profile" => {
        self
          .handle_assign_extension_group_to_profile(arguments)
          .await
      }
      // Cookie management
      "import_profile_cookies" => self.handle_import_profile_cookies(arguments).await,
      // Team lock tools
      "get_team_locks" => self.handle_get_team_locks().await,
      "get_team_lock_status" => self.handle_get_team_lock_status(arguments).await,
      // Browser interaction tools (require paid subscription)
      "navigate" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_navigate(arguments).await
      }
      "screenshot" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_screenshot(arguments).await
      }
      "evaluate_javascript" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_evaluate_javascript(arguments).await
      }
      "click_element" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_click_element(arguments).await
      }
      "type_text" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_type_text(arguments).await
      }
      "get_page_content" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_get_page_content(arguments).await
      }
      "get_page_info" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_get_page_info(arguments).await
      }
      "get_interactive_elements" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_get_interactive_elements(arguments).await
      }
      "click_by_index" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_click_by_index(arguments).await
      }
      "type_by_index" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_type_by_index(arguments).await
      }
      "press_key" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_press_key(arguments).await
      }
      "upload_file" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_upload_file(arguments).await
      }
      "list_tabs" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_list_tabs(arguments).await
      }
      "new_tab" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_new_tab(arguments).await
      }
      "switch_tab" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_switch_tab(arguments).await
      }
      "close_tab" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_close_tab(arguments).await
      }
      // Scenario automation
      "run_scenario" => {
        Self::require_paid_subscription("Browser automation").await?;
        self.handle_run_scenario(arguments).await
      }
      "list_scenario_runs" => self.handle_list_scenario_runs(arguments).await,
      _ => Err(McpError {
        code: -32602,
        message: format!("Unknown tool: {tool_name}"),
      }),
    }
  }

  async fn handle_run_scenario(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    use crate::scenario::manager::ScenarioManager;
    use crate::scenario::model::Scenario;
    use crate::scenario::store::ScenarioStore;

    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    // Phải đang chạy (reuse helper sẵn có).
    let _ = self.get_running_profile(profile_id)?;

    let store = ScenarioStore::default_location();
    let scenario: Scenario = if let Some(obj) = arguments.get("scenario") {
      serde_json::from_value(obj.clone()).map_err(|e| McpError {
        code: -32602,
        message: format!("Invalid scenario: {e}"),
      })?
    } else if let Some(id) = arguments.get("scenario_id").and_then(|v| v.as_str()) {
      store.load_scenario(id).ok_or_else(|| McpError {
        code: -32000,
        message: format!("Scenario not found: {id}"),
      })?
    } else {
      return Err(McpError {
        code: -32602,
        message: "Provide `scenario` or `scenario_id`".to_string(),
      });
    };

    let triggered_by = arguments
      .get("triggered_by")
      .and_then(|v| v.as_str())
      .unwrap_or("api")
      .to_string();

    // Logic chạy + ghi lịch sử dùng chung với Tauri command & scheduler tick.
    let summary = ScenarioManager::instance()
      .run_and_record(profile_id, scenario, &triggered_by)
      .await;
    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": serde_json::to_string_pretty(&summary).unwrap_or_default() }]
    }))
  }

  async fn handle_list_scenario_runs(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let limit = arguments
      .get("limit")
      .and_then(|v| v.as_i64())
      .unwrap_or(50);
    let runs = crate::scenario::store::ScenarioStore::default_location().list_runs(limit);
    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": serde_json::to_string_pretty(&runs).unwrap_or_default() }]
    }))
  }

  async fn handle_list_profiles(&self) -> Result<serde_json::Value, McpError> {
    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    // Filter to only Camoufox and Cloak profiles
    let filtered: Vec<&BrowserProfile> = profiles
      .iter()
      .filter(|p| p.browser == "camoufox" || p.browser == "cloak")
      .collect();

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&filtered).unwrap_or_default()
      }]
    }))
  }

  async fn handle_get_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    // Check if it's a Camoufox or Cloak profile
    if profile.browser != "camoufox" && profile.browser != "cloak" {
      return Err(McpError {
        code: -32000,
        message: "MCP only supports Camoufox and Cloak profiles".to_string(),
      });
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&profile).unwrap_or_default()
      }]
    }))
  }

  async fn handle_run_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Launching profiles programmatically is a paid feature.
    Self::require_paid_subscription("Launching a profile").await?;

    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let url = arguments.get("url").and_then(|v| v.as_str());
    let headless = arguments
      .get("headless")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);

    // Get the profile
    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    // Check if it's a Camoufox or Cloak profile
    if profile.browser != "camoufox" && profile.browser != "cloak" {
      return Err(McpError {
        code: -32000,
        message: "MCP only supports Camoufox and Cloak profiles".to_string(),
      });
    }

    // Team lock check
    crate::team_lock::acquire_team_lock_if_needed(profile)
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: e,
      })?;

    // Get app handle to launch
    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    // Launch a fresh instance, honoring the requested headless mode. The CDP
    // port is self-allocated and discovered later via get_cdp_port_for_profile.
    crate::browser_runner::launch_browser_profile_impl(
      app_handle.clone(),
      profile.clone(),
      url.map(|s| s.to_string()),
      None,
      headless,
      true,
    )
    .await
    .map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to launch browser: {e}"),
    })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Browser profile '{}' launched successfully", profile.name)
      }]
    }))
  }

  async fn handle_kill_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Stopping profiles programmatically is a paid feature.
    Self::require_paid_subscription("Killing a profile").await?;

    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    // Get the profile
    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    // Check if it's a Camoufox or Cloak profile
    if profile.browser != "camoufox" && profile.browser != "cloak" {
      return Err(McpError {
        code: -32000,
        message: "MCP only supports Camoufox and Cloak profiles".to_string(),
      });
    }

    // Get app handle to kill
    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    // Kill the browser
    crate::browser_runner::BrowserRunner::instance()
      .kill_browser_process(app_handle.clone(), profile)
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to kill browser: {e}"),
      })?;

    crate::team_lock::release_team_lock_if_needed(profile).await;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Browser profile '{}' stopped successfully", profile.name)
      }]
    }))
  }

  async fn handle_create_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing name".to_string(),
      })?;
    let browser = arguments
      .get("browser")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing browser".to_string(),
      })?;

    if browser != "camoufox" && browser != "cloak" {
      return Err(McpError {
        code: -32602,
        message: "browser must be 'camoufox' or 'cloak'".to_string(),
      });
    }

    let proxy_id = arguments
      .get("proxy_id")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());
    let launch_hook = arguments
      .get("launch_hook")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());
    let group_id = arguments
      .get("group_id")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());
    let tags: Option<Vec<String>> = arguments.get("tags").and_then(|v| {
      v.as_array().map(|arr| {
        arr
          .iter()
          .filter_map(|item| item.as_str().map(|s| s.to_string()))
          .collect()
      })
    });

    // Pick the latest downloaded version for this browser
    let registry = crate::downloaded_browsers_registry::DownloadedBrowsersRegistry::instance();
    let versions = registry.get_downloaded_versions(browser);
    let version = versions.first().ok_or_else(|| McpError {
      code: -32000,
      message: format!("No downloaded version found for {browser}. Download it first."),
    })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let mut profile = ProfileManager::instance()
      .create_profile_with_group(
        app_handle,
        name,
        browser,
        version,
        "stable",
        proxy_id,
        None,
        None,
        None,
        group_id,
        false,
        None,
        launch_hook,
      )
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to create profile: {e}"),
      })?;

    if let Some(tags) = tags {
      let _ =
        ProfileManager::instance().update_profile_tags(app_handle, &profile.name, tags.clone());
      profile.tags = tags;
      if let Ok(profiles) = ProfileManager::instance().list_profiles() {
        let _ = crate::tag_manager::TAG_MANAGER
          .lock()
          .map(|manager| manager.rebuild_from_profiles(&profiles));
      }
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Profile '{}' created (id: {})", profile.name, profile.id)
      }]
    }))
  }

  async fn handle_update_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;
    let pm = ProfileManager::instance();

    if let Some(new_name) = arguments.get("name").and_then(|v| v.as_str()) {
      pm.rename_profile(app_handle, profile_id, new_name)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to rename profile: {e}"),
        })?;
    }

    if let Some(proxy_id) = arguments.get("proxy_id").and_then(|v| v.as_str()) {
      let pid = if proxy_id.is_empty() {
        None
      } else {
        Some(proxy_id.to_string())
      };
      pm.update_profile_proxy(app_handle.clone(), profile_id, pid)
        .await
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to update proxy: {e}"),
        })?;
    }

    if let Some(launch_hook) = arguments.get("launch_hook").and_then(|v| v.as_str()) {
      let normalized = if launch_hook.is_empty() {
        None
      } else {
        Some(launch_hook.to_string())
      };
      pm.update_profile_launch_hook(app_handle, profile_id, normalized)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to update launch hook: {e}"),
        })?;
    }

    if let Some(group_id) = arguments.get("group_id").and_then(|v| v.as_str()) {
      let gid = if group_id.is_empty() {
        None
      } else {
        Some(group_id.to_string())
      };
      pm.assign_profiles_to_group(app_handle, vec![profile_id.to_string()], gid)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to update group: {e}"),
        })?;
    }

    if let Some(tags) = arguments.get("tags").and_then(|v| v.as_array()) {
      let tag_list: Vec<String> = tags
        .iter()
        .filter_map(|item| item.as_str().map(|s| s.to_string()))
        .collect();
      pm.update_profile_tags(app_handle, profile_id, tag_list)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to update tags: {e}"),
        })?;
      if let Ok(profiles) = pm.list_profiles() {
        let _ = crate::tag_manager::TAG_MANAGER
          .lock()
          .map(|manager| manager.rebuild_from_profiles(&profiles));
      }
    }

    if let Some(ext_group_id) = arguments.get("extension_group_id").and_then(|v| v.as_str()) {
      let eid = if ext_group_id.is_empty() {
        None
      } else {
        Some(ext_group_id.to_string())
      };
      pm.update_profile_extension_group(profile_id, eid)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to update extension group: {e}"),
        })?;
    }

    if let Some(rules) = arguments
      .get("proxy_bypass_rules")
      .and_then(|v| v.as_array())
    {
      let rule_list: Vec<String> = rules
        .iter()
        .filter_map(|item| item.as_str().map(|s| s.to_string()))
        .collect();
      pm.update_profile_proxy_bypass_rules(app_handle, profile_id, rule_list)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to update proxy bypass rules: {e}"),
        })?;
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Profile '{profile_id}' updated successfully")
      }]
    }))
  }

  async fn handle_delete_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    ProfileManager::instance()
      .delete_profile(app_handle, profile_id)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to delete profile: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Profile '{profile_id}' deleted successfully")
      }]
    }))
  }

  async fn handle_list_tags(&self) -> Result<serde_json::Value, McpError> {
    let tags = crate::tag_manager::TAG_MANAGER
      .lock()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to access tag manager: {e}"),
      })?
      .get_all_tags()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to get tags: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&tags).unwrap_or_default()
      }]
    }))
  }

  async fn handle_list_proxies(&self) -> Result<serde_json::Value, McpError> {
    let proxies = PROXY_MANAGER.get_stored_proxies();

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&proxies).unwrap_or_default()
      }]
    }))
  }

  async fn handle_get_profile_status(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    // Get the profile
    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    // Check if it's a Camoufox or Cloak profile
    if profile.browser != "camoufox" && profile.browser != "cloak" {
      return Err(McpError {
        code: -32000,
        message: "MCP only supports Camoufox and Cloak profiles".to_string(),
      });
    }

    let is_running = profile.process_id.is_some();

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::json!({
          "profile_id": profile_id,
          "is_running": is_running
        }).to_string()
      }]
    }))
  }

  // Group management handlers
  async fn handle_list_groups(&self) -> Result<serde_json::Value, McpError> {
    let groups = GROUP_MANAGER
      .lock()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to lock group manager: {e}"),
      })?
      .get_all_groups()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list groups: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&groups).unwrap_or_default()
      }]
    }))
  }

  async fn handle_get_group(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let group_id = arguments
      .get("group_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing group_id".to_string(),
      })?;

    let groups = GROUP_MANAGER
      .lock()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to lock group manager: {e}"),
      })?
      .get_all_groups()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list groups: {e}"),
      })?;

    let group = groups
      .iter()
      .find(|g| g.id == group_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Group not found: {group_id}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&group).unwrap_or_default()
      }]
    }))
  }

  async fn handle_create_group(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing name".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let group = GROUP_MANAGER
      .lock()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to lock group manager: {e}"),
      })?
      .create_group(app_handle, name.to_string())
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to create group: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Group '{}' created successfully with ID: {}", group.name, group.id)
      }]
    }))
  }

  async fn handle_update_group(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let group_id = arguments
      .get("group_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing group_id".to_string(),
      })?;

    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing name".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let group = GROUP_MANAGER
      .lock()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to lock group manager: {e}"),
      })?
      .update_group(app_handle, group_id.to_string(), name.to_string())
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to update group: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Group '{}' updated successfully", group.name)
      }]
    }))
  }

  async fn handle_delete_group(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let group_id = arguments
      .get("group_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing group_id".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    GROUP_MANAGER
      .lock()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to lock group manager: {e}"),
      })?
      .delete_group(app_handle, group_id.to_string())
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to delete group: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Group '{}' deleted successfully", group_id)
      }]
    }))
  }

  async fn handle_assign_profiles_to_group(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_ids: Vec<String> = arguments
      .get("profile_ids")
      .and_then(|v| v.as_array())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_ids".to_string(),
      })?
      .iter()
      .filter_map(|v| v.as_str().map(|s| s.to_string()))
      .collect();

    let group_id = arguments
      .get("group_id")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    ProfileManager::instance()
      .assign_profiles_to_group(app_handle, profile_ids.clone(), group_id.clone())
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to assign profiles to group: {e}"),
      })?;

    let group_name = group_id.as_deref().unwrap_or("default");
    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("{} profile(s) assigned to group '{}'", profile_ids.len(), group_name)
      }]
    }))
  }

  // Full proxy management handlers
  async fn handle_get_proxy(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let proxy_id = arguments
      .get("proxy_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing proxy_id".to_string(),
      })?;

    let proxies = PROXY_MANAGER.get_stored_proxies();
    let proxy = proxies
      .iter()
      .find(|p| p.id == proxy_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Proxy not found: {proxy_id}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&proxy).unwrap_or_default()
      }]
    }))
  }

  async fn handle_create_proxy(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing name".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let proxy_type = arguments
      .get("proxy_type")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing proxy_type".to_string(),
      })?;

    let host = arguments
      .get("host")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing host".to_string(),
      })?;

    let port = arguments
      .get("port")
      .and_then(|v| v.as_u64())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing port".to_string(),
      })? as u16;

    let username = arguments
      .get("username")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());
    let password = arguments
      .get("password")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    let proxy_settings = ProxySettings {
      proxy_type: proxy_type.to_string(),
      host: host.to_string(),
      port,
      username,
      password,
    };

    let proxy = PROXY_MANAGER
      .create_stored_proxy(app_handle, name.to_string(), proxy_settings)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to create proxy: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Proxy '{}' created successfully with ID: {}", proxy.name, proxy.id)
      }]
    }))
  }

  async fn handle_update_proxy(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let proxy_id = arguments
      .get("proxy_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing proxy_id".to_string(),
      })?;

    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    // Build proxy_settings if any settings fields are provided
    let has_settings = arguments.get("proxy_type").is_some()
      || arguments.get("host").is_some()
      || arguments.get("port").is_some();

    let proxy_settings = if has_settings {
      // Get existing proxy to use as defaults
      let proxies = PROXY_MANAGER.get_stored_proxies();
      let existing = proxies
        .iter()
        .find(|p| p.id == proxy_id)
        .ok_or_else(|| McpError {
          code: -32000,
          message: format!("Proxy not found: {proxy_id}"),
        })?;

      let proxy_type = arguments
        .get("proxy_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| existing.proxy_settings.proxy_type.clone());

      let host = arguments
        .get("host")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| existing.proxy_settings.host.clone());

      let port = arguments
        .get("port")
        .and_then(|v| v.as_u64())
        .map(|p| p as u16)
        .unwrap_or(existing.proxy_settings.port);

      let username = arguments
        .get("username")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| existing.proxy_settings.username.clone());

      let password = arguments
        .get("password")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| existing.proxy_settings.password.clone());

      Some(ProxySettings {
        proxy_type,
        host,
        port,
        username,
        password,
      })
    } else {
      None
    };

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let proxy = PROXY_MANAGER
      .update_stored_proxy(app_handle, proxy_id, name, proxy_settings)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to update proxy: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Proxy '{}' updated successfully", proxy.name)
      }]
    }))
  }

  async fn handle_delete_proxy(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let proxy_id = arguments
      .get("proxy_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing proxy_id".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    PROXY_MANAGER
      .delete_stored_proxy(app_handle, proxy_id)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to delete proxy: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Proxy '{}' deleted successfully", proxy_id)
      }]
    }))
  }

  async fn handle_export_proxies(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let format = arguments
      .get("format")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing format".to_string(),
      })?;

    let content = match format {
      "json" => PROXY_MANAGER.export_proxies_json().map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to export proxies: {e}"),
      })?,
      "txt" => PROXY_MANAGER.export_proxies_txt(),
      _ => {
        return Err(McpError {
          code: -32602,
          message: format!("Invalid format '{}', must be 'json' or 'txt'", format),
        })
      }
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": content
      }]
    }))
  }

  async fn handle_import_proxies(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let content = arguments
      .get("content")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing content".to_string(),
      })?;

    let format = arguments
      .get("format")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing format".to_string(),
      })?;

    let name_prefix = arguments
      .get("name_prefix")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let result = match format {
      "json" => PROXY_MANAGER
        .import_proxies_json(app_handle, content)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to import proxies: {e}"),
        })?,
      "txt" => {
        use crate::proxy_manager::{ProxyManager, ProxyParseResult};

        let parse_results = ProxyManager::parse_txt_proxies(content);
        let parsed: Vec<_> = parse_results
          .into_iter()
          .filter_map(|r| {
            if let ProxyParseResult::Parsed(p) = r {
              Some(p)
            } else {
              None
            }
          })
          .collect();

        if parsed.is_empty() {
          return Err(McpError {
            code: -32000,
            message: "No valid proxies found in content".to_string(),
          });
        }

        PROXY_MANAGER
          .import_proxies_from_parsed(app_handle, parsed, name_prefix)
          .map_err(|e| McpError {
            code: -32000,
            message: format!("Failed to import proxies: {e}"),
          })?
      }
      _ => {
        return Err(McpError {
          code: -32602,
          message: format!("Invalid format '{}', must be 'json' or 'txt'", format),
        })
      }
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!(
          "Import complete: {} imported, {} skipped, {} errors",
          result.imported_count,
          result.skipped_count,
          result.errors.len()
        )
      }]
    }))
  }

  // Cookie management handlers
  async fn handle_import_profile_cookies(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let content = arguments
      .get("content")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing content".to_string(),
      })?;

    let app_handle = {
      let inner = self.inner.lock().await;
      inner
        .app_handle
        .as_ref()
        .ok_or_else(|| McpError {
          code: -32000,
          message: "MCP server not properly initialized".to_string(),
        })?
        .clone()
    };

    let result =
      crate::cookie_manager::CookieManager::import_cookies(&app_handle, profile_id, content)
        .await
        .map_err(|e| McpError {
          code: -32000,
          message: format!("Failed to import cookies: {e}"),
        })?;

    if let Some(scheduler) = crate::sync::get_global_scheduler() {
      let profile_manager = crate::profile::manager::ProfileManager::instance();
      if let Ok(profiles) = profile_manager.list_profiles() {
        if let Some(profile) = profiles.iter().find(|p| p.id.to_string() == profile_id) {
          if profile.is_sync_enabled() {
            let pid = profile_id.to_string();
            tauri::async_runtime::spawn(async move {
              scheduler.queue_profile_sync(pid).await;
            });
          }
        }
      }
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!(
          "Import complete: {} imported, {} replaced, {} parse error(s)",
          result.cookies_imported,
          result.cookies_replaced,
          result.errors.len()
        )
      }]
    }))
  }

  // VPN management handlers
  async fn handle_import_vpn(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let content = arguments
      .get("content")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing content".to_string(),
      })?;

    let filename = arguments
      .get("filename")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing filename".to_string(),
      })?;

    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    let storage = crate::vpn::VPN_STORAGE.lock().map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to lock VPN storage: {e}"),
    })?;

    let config = storage
      .import_config(content, filename, name)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to import VPN config: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!(
          "VPN '{}' ({}) imported successfully with ID: {}",
          config.name,
          config.vpn_type,
          config.id
        )
      }]
    }))
  }

  async fn handle_list_vpn_configs(&self) -> Result<serde_json::Value, McpError> {
    let storage = crate::vpn::VPN_STORAGE.lock().map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to lock VPN storage: {e}"),
    })?;

    let configs = storage.list_configs().map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to list VPN configs: {e}"),
    })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&configs).unwrap_or_default()
      }]
    }))
  }

  async fn handle_delete_vpn(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let vpn_id = arguments
      .get("vpn_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing vpn_id".to_string(),
      })?;

    // First disconnect if connected (stop VPN worker)
    let _ = crate::vpn_worker_runner::stop_vpn_worker_by_vpn_id(vpn_id).await;

    let storage = crate::vpn::VPN_STORAGE.lock().map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to lock VPN storage: {e}"),
    })?;

    storage.delete_config(vpn_id).map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to delete VPN config: {e}"),
    })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("VPN '{}' deleted successfully", vpn_id)
      }]
    }))
  }

  async fn handle_connect_vpn(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let vpn_id = arguments
      .get("vpn_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing vpn_id".to_string(),
      })?;

    // Start VPN worker process
    crate::vpn_worker_runner::start_vpn_worker(vpn_id)
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to connect VPN: {e}"),
      })?;

    // Update last_used timestamp
    {
      let storage = crate::vpn::VPN_STORAGE.lock().map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to lock VPN storage: {e}"),
      })?;
      let _ = storage.update_last_used(vpn_id);
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("VPN '{}' connected successfully", vpn_id)
      }]
    }))
  }

  async fn handle_disconnect_vpn(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let vpn_id = arguments
      .get("vpn_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing vpn_id".to_string(),
      })?;

    crate::vpn_worker_runner::stop_vpn_worker_by_vpn_id(vpn_id)
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to disconnect VPN: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("VPN '{}' disconnected successfully", vpn_id)
      }]
    }))
  }

  async fn handle_get_vpn_status(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let vpn_id = arguments
      .get("vpn_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing vpn_id".to_string(),
      })?;

    let connected =
      if let Some(worker) = crate::vpn_worker_storage::find_vpn_worker_by_vpn_id(vpn_id) {
        worker
          .pid
          .map(crate::proxy_storage::is_process_running)
          .unwrap_or(false)
      } else {
        false
      };

    let status = crate::vpn::VpnStatus {
      connected,
      vpn_id: vpn_id.to_string(),
      connected_at: None,
      bytes_sent: None,
      bytes_received: None,
      last_handshake: None,
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&status).unwrap_or_default()
      }]
    }))
  }

  // Fingerprint management handlers
  async fn handle_get_profile_fingerprint(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    let fingerprint_info = match profile.browser.as_str() {
      "camoufox" => {
        let config = profile
          .camoufox_config
          .as_ref()
          .cloned()
          .unwrap_or_default();
        serde_json::json!({
          "browser": "camoufox",
          "fingerprint": config.fingerprint,
          "os": config.os,
          "randomize_fingerprint_on_launch": config.randomize_fingerprint_on_launch,
          "screen_max_width": config.screen_max_width,
          "screen_max_height": config.screen_max_height,
          "screen_min_width": config.screen_min_width,
          "screen_min_height": config.screen_min_height,
        })
      }
      "cloak" => {
        let config = profile.cloak_config.as_ref().cloned().unwrap_or_default();
        serde_json::json!({
          "browser": "cloak",
          "seed": config.seed,
          "os": config.os,
          "randomize_seed_on_launch": config.randomize_seed_on_launch,
          "timezone": config.timezone,
          "locale": config.locale,
          "screen_width": config.screen_width,
          "screen_height": config.screen_height,
        })
      }
      _ => {
        return Err(McpError {
          code: -32000,
          message: "MCP only supports Camoufox and Cloak profiles".to_string(),
        })
      }
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&fingerprint_info).unwrap_or_default()
      }]
    }))
  }

  async fn handle_update_profile_fingerprint(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: fingerprint editing available without a subscription.

    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let fingerprint = arguments.get("fingerprint").and_then(|v| v.as_str());
    let os = arguments.get("os").and_then(|v| v.as_str());
    let randomize = arguments
      .get("randomize_fingerprint_on_launch")
      .and_then(|v| v.as_bool());

    // Pro gate removed: cross-OS fingerprint spoofing available without a subscription.

    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    match profile.browser.as_str() {
      "camoufox" => {
        let mut config = profile
          .camoufox_config
          .as_ref()
          .cloned()
          .unwrap_or_default();
        if let Some(fp) = fingerprint {
          config.fingerprint = Some(fp.to_string());
        }
        if let Some(os_val) = os {
          config.os = Some(os_val.to_string());
        }
        if let Some(r) = randomize {
          config.randomize_fingerprint_on_launch = Some(r);
        }
        ProfileManager::instance()
          .update_camoufox_config(app_handle.clone(), profile_id, config)
          .await
          .map_err(|e| McpError {
            code: -32000,
            message: format!("Failed to update camoufox config: {e}"),
          })?;
      }
      "cloak" => {
        let mut config = profile.cloak_config.as_ref().cloned().unwrap_or_default();
        // Cloak's fingerprint is a numeric seed; accept it via the `fingerprint`
        // argument parsed as a u32.
        if let Some(fp) = fingerprint {
          config.seed = fp.parse::<u32>().ok();
        }
        if let Some(os_val) = os {
          config.os = Some(os_val.to_string());
        }
        if let Some(r) = randomize {
          config.randomize_seed_on_launch = Some(r);
        }
        ProfileManager::instance()
          .update_cloak_config(app_handle.clone(), profile_id, config)
          .await
          .map_err(|e| McpError {
            code: -32000,
            message: format!("Failed to update cloak config: {e}"),
          })?;
      }
      _ => {
        return Err(McpError {
          code: -32000,
          message: "MCP only supports Camoufox and Cloak profiles".to_string(),
        })
      }
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Fingerprint configuration updated for profile '{}'", profile.name)
      }]
    }))
  }

  async fn handle_update_profile_proxy_bypass_rules(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let rules: Vec<String> = arguments
      .get("rules")
      .and_then(|v| v.as_array())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing rules array".to_string(),
      })?
      .iter()
      .filter_map(|v| v.as_str().map(|s| s.to_string()))
      .collect();

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let profile = ProfileManager::instance()
      .update_profile_proxy_bypass_rules(app_handle, profile_id, rules.clone())
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to update proxy bypass rules: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!(
          "Proxy bypass rules updated for profile '{}': {} rule(s) configured",
          profile.name,
          rules.len()
        )
      }]
    }))
  }

  async fn handle_add_profile_tag(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let tag = arguments
      .get("tag")
      .and_then(|v| v.as_str())
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing tag".to_string(),
      })?;

    let inner = self.inner.lock().await;
    let app_handle = inner.app_handle.as_ref().ok_or_else(|| McpError {
      code: -32000,
      message: "MCP server not properly initialized".to_string(),
    })?;

    let pm = ProfileManager::instance();
    let mut profile = pm
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?
      .into_iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    // Append then let update_profile_tags dedup, so re-tagging is idempotent.
    profile.tags.push(tag.to_string());
    let updated = pm
      .update_profile_tags(app_handle, profile_id, profile.tags)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to add tag: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Tagged profile '{}' with '{}'", updated.name, tag)
      }]
    }))
  }

  async fn handle_update_profile_dns_blocklist(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let level = arguments
      .get("level")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing level".to_string(),
      })?;

    let dns_blocklist = if level == "none" {
      None
    } else {
      Some(level.to_string())
    };

    let profile = ProfileManager::instance()
      .update_profile_dns_blocklist(profile_id, dns_blocklist)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to update DNS blocklist: {e}"),
      })?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!(
          "DNS blocklist updated for profile '{}': {}",
          profile.name,
          level
        )
      }]
    }))
  }

  async fn handle_get_dns_blocklist_status(&self) -> Result<serde_json::Value, McpError> {
    let statuses = crate::dns_blocklist::BlocklistManager::get_cache_status();
    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&statuses).unwrap_or_default()
      }]
    }))
  }

  async fn handle_list_extensions(&self) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: extension management available without a subscription.
    let mgr = crate::extension_manager::EXTENSION_MANAGER.lock().unwrap();
    let extensions = mgr.list_extensions().map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to list extensions: {e}"),
    })?;
    Ok(serde_json::to_value(extensions).unwrap())
  }

  async fn handle_list_extension_groups(&self) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: extension management available without a subscription.
    let mgr = crate::extension_manager::EXTENSION_MANAGER.lock().unwrap();
    let groups = mgr.list_groups().map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to list extension groups: {e}"),
    })?;
    Ok(serde_json::to_value(groups).unwrap())
  }

  async fn handle_create_extension_group(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: extension management available without a subscription.
    let name = arguments
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing required parameter: name".to_string(),
      })?;
    let mgr = crate::extension_manager::EXTENSION_MANAGER.lock().unwrap();
    let group = mgr.create_group(name.to_string()).map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to create extension group: {e}"),
    })?;
    Ok(serde_json::to_value(group).unwrap())
  }

  async fn handle_delete_extension_mcp(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: extension management available without a subscription.
    let extension_id = arguments
      .get("extension_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing required parameter: extension_id".to_string(),
      })?;
    let mgr = crate::extension_manager::EXTENSION_MANAGER.lock().unwrap();
    mgr
      .delete_extension_internal(extension_id)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to delete extension: {e}"),
      })?;
    Ok(serde_json::json!({"success": true}))
  }

  async fn handle_delete_extension_group_mcp(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: extension management available without a subscription.
    let group_id = arguments
      .get("group_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing required parameter: group_id".to_string(),
      })?;
    let mgr = crate::extension_manager::EXTENSION_MANAGER.lock().unwrap();
    // For MCP, we don't have an app_handle, but we need one for sync deletion.
    // Use the delete_group_internal which skips sync remote deletion.
    mgr.delete_group_internal(group_id).map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to delete extension group: {e}"),
    })?;
    if let Err(e) = crate::events::emit_empty("extensions-changed") {
      log::error!("Failed to emit extensions-changed event: {e}");
    }
    Ok(serde_json::json!({"success": true}))
  }

  async fn handle_assign_extension_group_to_profile(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    // Pro gate removed: extension management available without a subscription.
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing required parameter: profile_id".to_string(),
      })?;
    let extension_group_id = arguments
      .get("extension_group_id")
      .and_then(|v| v.as_str())
      .map(|s| {
        if s.is_empty() {
          None
        } else {
          Some(s.to_string())
        }
      })
      .unwrap_or(None);

    // Validate compatibility if assigning
    if let Some(ref gid) = extension_group_id {
      let profile_manager = ProfileManager::instance();
      let profiles = profile_manager.list_profiles().map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;
      let profile = profiles
        .iter()
        .find(|p| p.id.to_string() == profile_id)
        .ok_or_else(|| McpError {
          code: -32000,
          message: format!("Profile '{profile_id}' not found"),
        })?;
      let mgr = crate::extension_manager::EXTENSION_MANAGER.lock().unwrap();
      mgr
        .validate_group_compatibility(gid, &profile.browser)
        .map_err(|e| McpError {
          code: -32000,
          message: format!("{e}"),
        })?;
    }

    let profile_manager = ProfileManager::instance();
    let profile = profile_manager
      .update_profile_extension_group(profile_id, extension_group_id)
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to assign extension group: {e}"),
      })?;
    Ok(serde_json::to_value(profile).unwrap())
  }

  async fn handle_get_team_locks(&self) -> Result<serde_json::Value, McpError> {
    if !CLOUD_AUTH.is_on_team_plan().await {
      return Err(McpError {
        code: -32000,
        message: "Team features require an active team plan".to_string(),
      });
    }
    let locks = crate::team_lock::TEAM_LOCK.get_locks().await;
    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&locks).unwrap_or_default()
      }]
    }))
  }

  async fn handle_get_team_lock_status(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    if !CLOUD_AUTH.is_on_team_plan().await {
      return Err(McpError {
        code: -32000,
        message: "Team features require an active team plan".to_string(),
      });
    }
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let lock_status = crate::team_lock::TEAM_LOCK
      .get_lock_status(profile_id)
      .await;
    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&lock_status).unwrap_or_default()
      }]
    }))
  }

  // --- CDP utility methods for browser interaction ---

  async fn get_cdp_port_for_profile(&self, profile: &BrowserProfile) -> Result<u16, McpError> {
    let profiles_dir = ProfileManager::instance().get_profiles_dir();
    let profile_path = profile.get_profile_data_path(&profiles_dir);
    let profile_path_str = profile_path.to_string_lossy();

    // Retry a few times — port info may not be stored yet right after launch
    for attempt in 0..10 {
      if attempt > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
      }
      let port = if profile.browser == "camoufox" {
        crate::camoufox_manager::CamoufoxManager::instance()
          .get_cdp_port(&profile_path_str)
          .await
      } else if profile.browser == "cloak" {
        crate::cloak_manager::CloakManager::instance()
          .get_cdp_port(&profile_path_str)
          .await
      } else {
        None
      };
      if let Some(p) = port {
        return Ok(p);
      }
    }

    Err(McpError {
      code: -32000,
      message: format!(
        "No CDP connection available for profile '{}'. Make sure the browser is running.",
        profile.name
      ),
    })
  }

  async fn get_cdp_ws_url(&self, profile: &BrowserProfile, port: u16) -> Result<String, McpError> {
    // Camoufox is a Playwright-Firefox build: it does NOT speak Chromium CDP.
    // `--remote-debugging-port` (which we always pass at launch) starts the
    // Firefox Remote Agent serving WebDriver BiDi over a WebSocket at
    // `ws://127.0.0.1:<port>/session` — the HTTP `/json` CDP discovery used
    // below returns 404 there. Return a `bidi://` sentinel so the send_* layer
    // routes to the BiDi translation instead of CDP. The session/context are
    // established lazily on first use.
    if profile.browser == "camoufox" {
      return Ok(format!("bidi://127.0.0.1:{port}"));
    }

    let url = format!("http://127.0.0.1:{port}/json");
    let client = reqwest::Client::new();

    // Retry connecting to CDP endpoint (browser may still be starting up)
    let max_attempts = 15;
    let mut last_err = String::new();
    for attempt in 0..max_attempts {
      if attempt > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
      }
      match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
      {
        Ok(resp) => match resp.json::<Vec<serde_json::Value>>().await {
          Ok(targets) => {
            let pages: Vec<&serde_json::Value> = targets
              .iter()
              .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
              .filter(|t| !is_devtools_target(t))
              .collect();
            // Prefer the tab selected via `switch_tab`/`new_tab` if it still exists;
            // otherwise fall back to the first page (single-tab flows unchanged).
            let active_id = self
              .active_targets
              .lock()
              .await
              .get(&profile.id.to_string())
              .cloned();
            let chosen = active_id
              .as_deref()
              .and_then(|id| {
                pages
                  .iter()
                  .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
              })
              .or_else(|| pages.first())
              .copied();
            if let Some(ws_url) = chosen
              .and_then(|t| t.get("webSocketDebuggerUrl"))
              .and_then(|v| v.as_str())
            {
              return Ok(ws_url.to_string());
            }
            last_err = "No page target found in browser".to_string();
          }
          Err(e) => {
            last_err = format!("Failed to parse CDP targets: {e}");
          }
        },
        Err(e) => {
          last_err = format!("Failed to connect to browser CDP endpoint: {e}");
        }
      }
    }

    Err(McpError {
      code: -32000,
      message: last_err,
    })
  }

  /// Page-type CDP targets (open tabs) for a Chromium/Cloak debug port, in the
  /// browser's own order. Each entry keeps `id`, `title`, `url`, `webSocketDebuggerUrl`.
  async fn cdp_page_targets(&self, port: u16) -> Result<Vec<serde_json::Value>, McpError> {
    let url = format!("http://127.0.0.1:{port}/json");
    let client = reqwest::Client::new();
    let targets = client
      .get(&url)
      .timeout(std::time::Duration::from_secs(3))
      .send()
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to connect to browser CDP endpoint: {e}"),
      })?
      .json::<Vec<serde_json::Value>>()
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to parse CDP targets: {e}"),
      })?;
    Ok(
      targets
        .into_iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .filter(|t| !is_devtools_target(t))
        .collect(),
    )
  }

  /// Browser-level CDP WebSocket (`/json/version`) for sending `Target.*` commands
  /// like create/activate/close that are not scoped to a single page.
  async fn get_cdp_browser_ws_url(&self, port: u16) -> Result<String, McpError> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = reqwest::Client::new();
    let body = client
      .get(&url)
      .timeout(std::time::Duration::from_secs(3))
      .send()
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to connect to browser CDP endpoint: {e}"),
      })?
      .json::<serde_json::Value>()
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to parse CDP version info: {e}"),
      })?;
    body
      .get("webSocketDebuggerUrl")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string())
      .ok_or_else(|| McpError {
        code: -32000,
        message: "No browser WebSocket endpoint found".to_string(),
      })
  }

  // --- WebDriver BiDi (Camoufox / Firefox) — see ARCHITECTURE for protocol notes ---

  /// Get (creating if needed) the per-port connection cell. Holds the pool lock
  /// only briefly; the returned `Arc<Mutex<…>>` is locked by the caller for the
  /// duration of the BiDi command.
  async fn bidi_cell(&self, port: u16) -> Arc<AsyncMutex<Option<BidiConn>>> {
    let mut pool = self.bidi_pool.lock().await;
    pool
      .entry(port)
      .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
      .clone()
  }

  /// Open a BiDi session against the Firefox Remote Agent and resolve the
  /// top-level browsing context. Retries the WebSocket connect for a few
  /// seconds since the agent may not be listening immediately after launch.
  async fn bidi_connect(&self, port: u16) -> Result<BidiConn, McpError> {
    use tokio_tungstenite::connect_async;

    let url = format!("ws://127.0.0.1:{port}/session");
    let mut last = String::new();
    let mut ws = None;
    for attempt in 0..12 {
      if attempt > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
      }
      match connect_async(&url).await {
        Ok((stream, _)) => {
          ws = Some(stream);
          break;
        }
        Err(e) => last = e.to_string(),
      }
    }
    let ws = ws.ok_or_else(|| McpError {
      // Transport-class error code so callers can distinguish "browser not
      // reachable" from a protocol error.
      code: -32099,
      message: format!("Failed to connect to Camoufox BiDi WebSocket: {last}"),
    })?;

    let mut conn = BidiConn {
      ws,
      context: String::new(),
      next_id: 0,
      load_subscribed: false,
    };
    self
      .bidi_rpc(
        &mut conn,
        "session.new",
        serde_json::json!({ "capabilities": {} }),
      )
      .await?;
    let tree = self
      .bidi_rpc(&mut conn, "browsingContext.getTree", serde_json::json!({}))
      .await?;
    conn.context = tree
      .get("contexts")
      .and_then(|c| c.get(0))
      .and_then(|c| c.get("context"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32000,
        message: "No browsing context found in Camoufox".to_string(),
      })?
      .to_string();

    Ok(conn)
  }

  /// Send one BiDi command on a pooled connection and wait for the matching
  /// response, skipping interleaved events. Transport-class failures (send/recv
  /// error, closed socket, timeout) use code -32099 so `bidi_exec` reconnects;
  /// BiDi protocol errors and parse failures use -32000 (not retried).
  async fn bidi_rpc(
    &self,
    conn: &mut BidiConn,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    use futures_util::sink::SinkExt;
    use futures_util::stream::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    conn.next_id += 1;
    let id = conn.next_id;
    let cmd = serde_json::json!({ "id": id, "method": method, "params": params });
    conn
      .ws
      .send(Message::Text(cmd.to_string().into()))
      .await
      .map_err(|e| McpError {
        code: -32099,
        message: format!("Failed to send BiDi command: {e}"),
      })?;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(120);
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(McpError {
          code: -32099,
          message: format!("Timed out waiting for BiDi response to {method}"),
        });
      }
      let msg = match tokio::time::timeout(remaining, conn.ws.next()).await {
        Ok(Some(Ok(m))) => m,
        Ok(Some(Err(e))) => {
          return Err(McpError {
            code: -32099,
            message: format!("BiDi WebSocket error: {e}"),
          })
        }
        Ok(None) => {
          return Err(McpError {
            code: -32099,
            message: "BiDi WebSocket closed unexpectedly".to_string(),
          })
        }
        Err(_) => {
          return Err(McpError {
            code: -32099,
            message: format!("Timed out waiting for BiDi response to {method}"),
          })
        }
      };
      if let Message::Text(text) = msg {
        let resp: serde_json::Value =
          serde_json::from_str(text.as_str()).map_err(|e| McpError {
            code: -32000,
            message: format!("Failed to parse BiDi response: {e}"),
          })?;
        if resp.get("id") == Some(&serde_json::json!(id)) {
          if resp.get("type").and_then(|v| v.as_str()) == Some("error") {
            let m = resp
              .get("message")
              .and_then(|v| v.as_str())
              .unwrap_or("unknown");
            return Err(McpError {
              code: -32000,
              message: format!("BiDi error ({method}): {m}"),
            });
          }
          return Ok(resp.get("result").cloned().unwrap_or(serde_json::json!({})));
        }
        // Any other id / an event ("type":"event") — keep reading.
      }
    }
  }

  /// After an action that may navigate, drain messages until a
  /// `browsingContext.load` event for our context arrives (best-effort, bounded).
  /// Stale load events were already consumed by the preceding `bidi_rpc`, so
  /// this waits for the *next* load. Returns on timeout or socket close.
  async fn bidi_wait_load(&self, conn: &mut BidiConn, timeout_secs: u64) {
    use futures_util::stream::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return;
      }
      match tokio::time::timeout(remaining, conn.ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
          if let Ok(resp) = serde_json::from_str::<serde_json::Value>(text.as_str()) {
            if resp.get("method").and_then(|v| v.as_str()) == Some("browsingContext.load")
              && resp
                .get("params")
                .and_then(|p| p.get("context"))
                .and_then(|v| v.as_str())
                == Some(conn.context.as_str())
            {
              return;
            }
          }
        }
        Ok(Some(Ok(_))) => {}
        _ => return,
      }
    }
  }

  /// Execute a single BiDi op sequence on an already-open connection, returning
  /// the result in CDP shape (so the interaction handlers parse it unchanged).
  async fn bidi_run_op(
    &self,
    conn: &mut BidiConn,
    op: &BidiOp,
  ) -> Result<serde_json::Value, McpError> {
    let ctx = conn.context.clone();
    match op {
      BidiOp::Eval {
        expression,
        await_promise,
        wait_load,
      } => {
        if *wait_load && !conn.load_subscribed {
          self
            .bidi_rpc(
              conn,
              "session.subscribe",
              serde_json::json!({ "events": ["browsingContext.load"] }),
            )
            .await?;
          conn.load_subscribed = true;
        }
        let r = self
          .bidi_rpc(
            conn,
            "script.evaluate",
            serde_json::json!({
              "expression": expression,
              "target": { "context": ctx },
              "awaitPromise": *await_promise,
            }),
          )
          .await?;
        if *wait_load {
          self.bidi_wait_load(conn, 30).await;
        }
        Ok(bidi_eval_to_cdp(&r))
      }
      BidiOp::Navigate { url } => {
        self
          .bidi_rpc(
            conn,
            "browsingContext.navigate",
            serde_json::json!({ "context": ctx, "url": url, "wait": "complete" }),
          )
          .await?;
        Ok(serde_json::json!({}))
      }
      BidiOp::Screenshot {
        format,
        quality,
        full_page,
      } => {
        let mut p = serde_json::json!({
          "context": ctx,
          "format": { "type": format!("image/{format}") },
        });
        if format != "png" {
          if let Some(q) = quality {
            p["format"]["quality"] = serde_json::json!((*q as f64) / 100.0);
          }
        }
        if *full_page {
          p["origin"] = serde_json::json!("document");
        }
        let r = self
          .bidi_rpc(conn, "browsingContext.captureScreenshot", p)
          .await?;
        Ok(serde_json::json!({ "data": r.get("data").cloned().unwrap_or(serde_json::json!("")) }))
      }
      BidiOp::LayoutMetrics => {
        let r = self
          .bidi_rpc(
            conn,
            "script.evaluate",
            serde_json::json!({
              "expression": "JSON.stringify({w: document.documentElement.scrollWidth, h: document.documentElement.scrollHeight})",
              "target": { "context": ctx },
              "awaitPromise": false,
            }),
          )
          .await?;
        let dims_str = r
          .get("result")
          .and_then(|v| v.get("value"))
          .and_then(|v| v.as_str())
          .unwrap_or("{}");
        let dims: serde_json::Value =
          serde_json::from_str(dims_str).unwrap_or(serde_json::json!({}));
        Ok(serde_json::json!({
          "contentSize": {
            "width": dims.get("w").cloned().unwrap_or(serde_json::json!(1920)),
            "height": dims.get("h").cloned().unwrap_or(serde_json::json!(1080)),
          }
        }))
      }
      BidiOp::PerformKeys { actions } => {
        self
          .bidi_rpc(
            conn,
            "input.performActions",
            serde_json::json!({
              "context": ctx,
              "actions": [ { "type": "key", "id": "kbd", "actions": actions } ],
            }),
          )
          .await?;
        Ok(serde_json::json!({}))
      }
      BidiOp::SetFiles { selector, files } => {
        // Resolve the element to a BiDi shared reference (owned by the realm root
        // so it survives until used), then hand it to input.setFiles.
        let esc = selector.replace('\\', "\\\\").replace('"', "\\\"");
        let r = self
          .bidi_rpc(
            conn,
            "script.evaluate",
            serde_json::json!({
              "expression": format!("document.querySelector(\"{esc}\")"),
              "target": { "context": ctx },
              "awaitPromise": false,
              "resultOwnership": "root",
            }),
          )
          .await?;
        let shared_id = r
          .get("result")
          .and_then(|v| v.get("sharedId"))
          .and_then(|v| v.as_str())
          .ok_or_else(|| McpError {
            code: -32000,
            message: format!("File input not found: {selector}"),
          })?
          .to_string();
        self
          .bidi_rpc(
            conn,
            "input.setFiles",
            serde_json::json!({
              "context": ctx,
              "element": { "sharedId": shared_id },
              "files": files,
            }),
          )
          .await?;
        Ok(serde_json::json!({}))
      }
      BidiOp::ListTabs => {
        let tabs = self.bidi_list_contexts(conn).await?;
        Ok(serde_json::json!({ "tabs": tabs }))
      }
      BidiOp::NewTab { url } => {
        let r = self
          .bidi_rpc(
            conn,
            "browsingContext.create",
            serde_json::json!({ "type": "tab" }),
          )
          .await?;
        let new_ctx = r
          .get("context")
          .and_then(|v| v.as_str())
          .ok_or_else(|| McpError {
            code: -32000,
            message: "browsingContext.create returned no context".to_string(),
          })?
          .to_string();
        conn.context = new_ctx.clone();
        if !url.is_empty() {
          self
            .bidi_rpc(
              conn,
              "browsingContext.navigate",
              serde_json::json!({ "context": new_ctx, "url": url, "wait": "complete" }),
            )
            .await?;
        }
        Ok(serde_json::json!({ "context": conn.context }))
      }
      BidiOp::SwitchTab { index } => {
        let tabs = self.bidi_list_contexts(conn).await?;
        let target = tabs.get(*index).ok_or_else(|| McpError {
          code: -32000,
          message: format!("No tab at index {index}"),
        })?;
        let new_ctx = target
          .get("context")
          .and_then(|v| v.as_str())
          .unwrap_or_default()
          .to_string();
        self
          .bidi_rpc(
            conn,
            "browsingContext.activate",
            serde_json::json!({ "context": new_ctx }),
          )
          .await?;
        conn.context = new_ctx;
        Ok(serde_json::json!({ "context": conn.context }))
      }
      BidiOp::CloseTab { index } => {
        let tabs = self.bidi_list_contexts(conn).await?;
        let target = tabs.get(*index).ok_or_else(|| McpError {
          code: -32000,
          message: format!("No tab at index {index}"),
        })?;
        let close_ctx = target
          .get("context")
          .and_then(|v| v.as_str())
          .unwrap_or_default()
          .to_string();
        self
          .bidi_rpc(
            conn,
            "browsingContext.close",
            serde_json::json!({ "context": close_ctx }),
          )
          .await?;
        // Re-resolve the active context to a surviving tab.
        let remaining = self.bidi_list_contexts(conn).await?;
        conn.context = remaining
          .first()
          .and_then(|t| t.get("context"))
          .and_then(|v| v.as_str())
          .unwrap_or_default()
          .to_string();
        Ok(serde_json::json!({ "context": conn.context }))
      }
    }
  }

  /// Top-level browsing contexts (tabs) as `[{index, context, url}]` in tree order.
  async fn bidi_list_contexts(
    &self,
    conn: &mut BidiConn,
  ) -> Result<Vec<serde_json::Value>, McpError> {
    let tree = self
      .bidi_rpc(conn, "browsingContext.getTree", serde_json::json!({}))
      .await?;
    let mut out = Vec::new();
    if let Some(contexts) = tree.get("contexts").and_then(|v| v.as_array()) {
      for (i, c) in contexts.iter().enumerate() {
        out.push(serde_json::json!({
          "index": i,
          "context": c.get("context").and_then(|v| v.as_str()).unwrap_or_default(),
          "url": c.get("url").and_then(|v| v.as_str()).unwrap_or_default(),
        }));
      }
    }
    Ok(out)
  }

  /// Run a BiDi op on the pooled connection for `port`, opening it on first use
  /// and reconnecting once if the cached socket turned out to be dead.
  async fn bidi_exec(&self, port: u16, op: BidiOp) -> Result<serde_json::Value, McpError> {
    let cell = self.bidi_cell(port).await;
    let mut guard = cell.lock().await;
    let mut last_err: Option<McpError> = None;
    for attempt in 0..2 {
      if guard.is_none() {
        match self.bidi_connect(port).await {
          Ok(c) => *guard = Some(c),
          Err(e) if e.code == -32099 && attempt == 0 => {
            last_err = Some(e);
            continue;
          }
          Err(e) => return Err(e),
        }
      }
      let conn = guard.as_mut().expect("connection ensured above");
      match self.bidi_run_op(conn, &op).await {
        Ok(v) => return Ok(v),
        // Transport error: drop the dead connection and retry once.
        Err(e) if e.code == -32099 && attempt == 0 => {
          *guard = None;
          last_err = Some(e);
          continue;
        }
        Err(e) => return Err(e),
      }
    }
    Err(last_err.unwrap_or_else(|| McpError {
      code: -32000,
      message: "BiDi operation failed".to_string(),
    }))
  }

  /// BiDi equivalent of `send_cdp` for the (non-navigating) CDP methods the
  /// interaction handlers use against Camoufox.
  async fn send_bidi(
    &self,
    port: u16,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let op = match method {
      "Runtime.evaluate" => BidiOp::Eval {
        expression: params
          .get("expression")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string(),
        await_promise: params
          .get("awaitPromise")
          .and_then(|v| v.as_bool())
          .unwrap_or(false),
        wait_load: false,
      },
      "Page.captureScreenshot" => BidiOp::Screenshot {
        format: params
          .get("format")
          .and_then(|v| v.as_str())
          .unwrap_or("png")
          .to_string(),
        quality: params.get("quality").and_then(|v| v.as_i64()),
        full_page: params
          .get("captureBeyondViewport")
          .and_then(|v| v.as_bool())
          .unwrap_or(false),
      },
      "Page.getLayoutMetrics" => BidiOp::LayoutMetrics,
      "Input.insertText" => {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let mut actions = Vec::new();
        for ch in text.chars() {
          let v = ch.to_string();
          actions.push(serde_json::json!({ "type": "keyDown", "value": v }));
          actions.push(serde_json::json!({ "type": "keyUp", "value": v }));
        }
        BidiOp::PerformKeys { actions }
      }
      other => {
        return Err(McpError {
          code: -32000,
          message: format!("Camoufox automation: CDP method '{other}' is not yet bridged to BiDi"),
        })
      }
    };
    self.bidi_exec(port, op).await
  }

  /// BiDi equivalent of `send_cdp_and_wait_for_load`. `browsingContext.navigate`
  /// with `wait:"complete"` waits for load natively, so no event plumbing.
  async fn send_bidi_wait(
    &self,
    port: u16,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let op = match method {
      "Page.navigate" => BidiOp::Navigate {
        url: params
          .get("url")
          .and_then(|v| v.as_str())
          .unwrap_or("about:blank")
          .to_string(),
      },
      "Runtime.evaluate" => BidiOp::Eval {
        expression: params
          .get("expression")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string(),
        await_promise: params
          .get("awaitPromise")
          .and_then(|v| v.as_bool())
          .unwrap_or(false),
        wait_load: true,
      },
      other => {
        return Err(McpError {
          code: -32000,
          message: format!("Camoufox automation: CDP method '{other}' is not yet bridged to BiDi"),
        })
      }
    };
    self.bidi_exec(port, op).await
  }

  /// BiDi human-typing into the focused element via `input.performActions`.
  /// Reuses the same MarkovTyper timing model as the CDP path, replaying the
  /// inter-keystroke delays as BiDi `pause` actions. `` is the WebDriver
  /// key value for Backspace.
  async fn send_bidi_keystrokes(
    &self,
    port: u16,
    text: &str,
    wpm: Option<f64>,
  ) -> Result<(), McpError> {
    use crate::human_typing::{MarkovTyper, TypingAction};

    let events = MarkovTyper::new(text, wpm).run();
    let mut actions = Vec::new();
    let mut last_time = 0.0_f64;
    for event in &events {
      let delay_ms = ((event.time - last_time) * 1000.0).round();
      if delay_ms > 0.0 {
        actions.push(serde_json::json!({ "type": "pause", "duration": delay_ms as u64 }));
      }
      last_time = event.time;
      let value = match &event.action {
        TypingAction::Char(ch) => ch.to_string(),
        TypingAction::Backspace => "\u{E003}".to_string(),
      };
      actions.push(serde_json::json!({ "type": "keyDown", "value": value }));
      actions.push(serde_json::json!({ "type": "keyUp", "value": value }));
    }

    self
      .bidi_exec(port, BidiOp::PerformKeys { actions })
      .await
      .map(|_| ())
  }

  async fn send_cdp(
    &self,
    ws_url: &str,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    use futures_util::sink::SinkExt;
    use futures_util::stream::StreamExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    // Camoufox / Firefox: speak WebDriver BiDi instead of CDP.
    if let Some(port) = bidi_port(ws_url) {
      return self.send_bidi(port, method, params).await;
    }

    let (mut ws_stream, _) = connect_async(ws_url).await.map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to connect to CDP WebSocket: {e}"),
    })?;

    let command = serde_json::json!({
      "id": 1,
      "method": method,
      "params": params
    });

    ws_stream
      .send(Message::Text(command.to_string().into()))
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to send CDP command: {e}"),
      })?;

    while let Some(msg) = ws_stream.next().await {
      let msg = msg.map_err(|e| McpError {
        code: -32000,
        message: format!("CDP WebSocket error: {e}"),
      })?;
      if let Message::Text(text) = msg {
        let response: serde_json::Value =
          serde_json::from_str(text.as_str()).map_err(|e| McpError {
            code: -32000,
            message: format!("Failed to parse CDP response: {e}"),
          })?;
        if response.get("id") == Some(&serde_json::json!(1)) {
          if let Some(error) = response.get("error") {
            return Err(McpError {
              code: -32000,
              message: format!("CDP error: {error}"),
            });
          }
          return Ok(
            response
              .get("result")
              .cloned()
              .unwrap_or(serde_json::json!({})),
          );
        }
      }
    }

    Err(McpError {
      code: -32000,
      message: "No response received from CDP".to_string(),
    })
  }

  async fn send_human_keystrokes(
    &self,
    ws_url: &str,
    text: &str,
    wpm: Option<f64>,
  ) -> Result<(), McpError> {
    use crate::human_typing::{MarkovTyper, TypingAction};
    use futures_util::sink::SinkExt;
    use futures_util::stream::StreamExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    // Camoufox / Firefox: route to BiDi input (input.performActions).
    if let Some(port) = bidi_port(ws_url) {
      return self.send_bidi_keystrokes(port, text, wpm).await;
    }

    let events = MarkovTyper::new(text, wpm).run();

    let (mut ws_stream, _) = connect_async(ws_url).await.map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to connect to CDP WebSocket: {e}"),
    })?;

    let mut cmd_id = 1u64;
    let mut last_time = 0.0;

    for event in &events {
      let delay = event.time - last_time;
      if delay > 0.0 {
        tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
      }
      last_time = event.time;

      match &event.action {
        TypingAction::Char(ch) => {
          let text_str = ch.to_string();
          // keyDown
          let down = serde_json::json!({
            "id": cmd_id,
            "method": "Input.dispatchKeyEvent",
            "params": {
              "type": "keyDown",
              "text": text_str,
              "key": text_str,
              "unmodifiedText": text_str,
            }
          });
          cmd_id += 1;
          ws_stream
            .send(Message::Text(down.to_string().into()))
            .await
            .map_err(|e| McpError {
              code: -32000,
              message: format!("Failed to send key event: {e}"),
            })?;
          // Drain response
          let _ = ws_stream.next().await;

          // keyUp
          let up = serde_json::json!({
            "id": cmd_id,
            "method": "Input.dispatchKeyEvent",
            "params": {
              "type": "keyUp",
              "key": text_str,
            }
          });
          cmd_id += 1;
          ws_stream
            .send(Message::Text(up.to_string().into()))
            .await
            .map_err(|e| McpError {
              code: -32000,
              message: format!("Failed to send key event: {e}"),
            })?;
          let _ = ws_stream.next().await;
        }
        TypingAction::Backspace => {
          let down = serde_json::json!({
            "id": cmd_id,
            "method": "Input.dispatchKeyEvent",
            "params": {
              "type": "keyDown",
              "key": "Backspace",
              "code": "Backspace",
              "windowsVirtualKeyCode": 8,
              "nativeVirtualKeyCode": 8,
            }
          });
          cmd_id += 1;
          ws_stream
            .send(Message::Text(down.to_string().into()))
            .await
            .map_err(|e| McpError {
              code: -32000,
              message: format!("Failed to send key event: {e}"),
            })?;
          let _ = ws_stream.next().await;

          let up = serde_json::json!({
            "id": cmd_id,
            "method": "Input.dispatchKeyEvent",
            "params": {
              "type": "keyUp",
              "key": "Backspace",
              "code": "Backspace",
              "windowsVirtualKeyCode": 8,
              "nativeVirtualKeyCode": 8,
            }
          });
          cmd_id += 1;
          ws_stream
            .send(Message::Text(up.to_string().into()))
            .await
            .map_err(|e| McpError {
              code: -32000,
              message: format!("Failed to send key event: {e}"),
            })?;
          let _ = ws_stream.next().await;
        }
      }
    }

    Ok(())
  }

  /// Send a CDP command and wait for the page to finish loading.
  /// Uses a single WebSocket connection to: enable Page events, send the command,
  /// wait for the command response, then wait for `Page.loadEventFired`.
  async fn send_cdp_and_wait_for_load(
    &self,
    ws_url: &str,
    method: &str,
    params: serde_json::Value,
    timeout_secs: u64,
  ) -> Result<serde_json::Value, McpError> {
    use futures_util::sink::SinkExt;
    use futures_util::stream::StreamExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    // Camoufox / Firefox: BiDi navigate waits for load natively (wait:"complete").
    if let Some(port) = bidi_port(ws_url) {
      return self.send_bidi_wait(port, method, params).await;
    }

    let (mut ws_stream, _) = connect_async(ws_url).await.map_err(|e| McpError {
      code: -32000,
      message: format!("Failed to connect to CDP WebSocket: {e}"),
    })?;

    // Enable Page domain events so we receive loadEventFired
    let enable_cmd = serde_json::json!({
      "id": 1,
      "method": "Page.enable",
      "params": {}
    });
    ws_stream
      .send(Message::Text(enable_cmd.to_string().into()))
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to send Page.enable: {e}"),
      })?;

    // Wait for Page.enable response
    loop {
      let msg = ws_stream
        .next()
        .await
        .ok_or_else(|| McpError {
          code: -32000,
          message: "WebSocket closed waiting for Page.enable response".to_string(),
        })?
        .map_err(|e| McpError {
          code: -32000,
          message: format!("CDP WebSocket error: {e}"),
        })?;
      if let Message::Text(text) = msg {
        let resp: serde_json::Value = serde_json::from_str(text.as_str()).unwrap_or_default();
        if resp.get("id") == Some(&serde_json::json!(1)) {
          break;
        }
      }
    }

    // Send the actual command (e.g., Page.navigate)
    let command = serde_json::json!({
      "id": 2,
      "method": method,
      "params": params
    });
    ws_stream
      .send(Message::Text(command.to_string().into()))
      .await
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to send CDP command: {e}"),
      })?;

    // Wait for command response and then for Page.loadEventFired
    let mut command_result = None;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);

    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        // Timed out waiting for load — return the command result if we have it
        break;
      }

      let msg = match tokio::time::timeout(remaining, ws_stream.next()).await {
        Ok(Some(Ok(msg))) => msg,
        Ok(Some(Err(e))) => {
          return Err(McpError {
            code: -32000,
            message: format!("CDP WebSocket error: {e}"),
          });
        }
        Ok(None) => break, // stream ended
        Err(_) => break,   // timeout
      };

      if let Message::Text(text) = msg {
        let response: serde_json::Value = serde_json::from_str(text.as_str()).unwrap_or_default();

        // Check for command response
        if response.get("id") == Some(&serde_json::json!(2)) {
          if let Some(error) = response.get("error") {
            return Err(McpError {
              code: -32000,
              message: format!("CDP error: {error}"),
            });
          }
          command_result = Some(
            response
              .get("result")
              .cloned()
              .unwrap_or(serde_json::json!({})),
          );
        }

        // Check for Page.loadEventFired — page is fully loaded
        if response.get("method") == Some(&serde_json::json!("Page.loadEventFired")) {
          break;
        }
      }
    }

    // Disable Page domain events
    let disable_cmd = serde_json::json!({
      "id": 3,
      "method": "Page.disable",
      "params": {}
    });
    let _ = ws_stream
      .send(Message::Text(disable_cmd.to_string().into()))
      .await;

    command_result.ok_or_else(|| McpError {
      code: -32000,
      message: "No response received from CDP".to_string(),
    })
  }

  fn get_running_profile(&self, profile_id: &str) -> Result<BrowserProfile, McpError> {
    let profiles = ProfileManager::instance()
      .list_profiles()
      .map_err(|e| McpError {
        code: -32000,
        message: format!("Failed to list profiles: {e}"),
      })?;

    let profile = profiles
      .into_iter()
      .find(|p| p.id.to_string() == profile_id)
      .ok_or_else(|| McpError {
        code: -32000,
        message: format!("Profile not found: {profile_id}"),
      })?;

    if profile.browser != "camoufox" && profile.browser != "cloak" {
      return Err(McpError {
        code: -32000,
        message: "MCP only supports Camoufox and Cloak profiles".to_string(),
      });
    }

    if profile.process_id.is_none() {
      return Err(McpError {
        code: -32000,
        message: format!("Profile '{}' is not running", profile.name),
      });
    }

    Ok(profile)
  }

  // --- Browser interaction handlers ---

  async fn handle_navigate(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let url = arguments
      .get("url")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing url".to_string(),
      })?;

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    self
      .send_cdp_and_wait_for_load(
        &ws_url,
        "Page.navigate",
        serde_json::json!({ "url": url }),
        30,
      )
      .await?;

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Navigated to {url}")
      }]
    }))
  }

  async fn handle_screenshot(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let format = arguments
      .get("format")
      .and_then(|v| v.as_str())
      .unwrap_or("png");
    let quality = arguments.get("quality").and_then(|v| v.as_i64());
    let full_page = arguments
      .get("full_page")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let mut params = serde_json::json!({ "format": format });

    if let Some(q) = quality {
      params["quality"] = serde_json::json!(q);
    }

    if full_page {
      let layout = self
        .send_cdp(&ws_url, "Page.getLayoutMetrics", serde_json::json!({}))
        .await?;

      if let Some(content_size) = layout.get("contentSize") {
        params["clip"] = serde_json::json!({
          "x": 0,
          "y": 0,
          "width": content_size.get("width").and_then(|v| v.as_f64()).unwrap_or(1920.0),
          "height": content_size.get("height").and_then(|v| v.as_f64()).unwrap_or(1080.0),
          "scale": 1
        });
        params["captureBeyondViewport"] = serde_json::json!(true);
      }
    }

    let result = self
      .send_cdp(&ws_url, "Page.captureScreenshot", params)
      .await?;

    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .unwrap_or_default();

    Ok(serde_json::json!({
      "content": [{
        "type": "image",
        "data": data,
        "mimeType": format!("image/{format}")
      }]
    }))
  }

  async fn handle_evaluate_javascript(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let expression = arguments
      .get("expression")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing expression".to_string(),
      })?;
    let await_promise = arguments
      .get("await_promise")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);
    let wait_for_load = arguments
      .get("wait_for_load")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let cdp_params = serde_json::json!({
      "expression": expression,
      "returnByValue": true,
      "awaitPromise": await_promise,
    });

    let result = if wait_for_load {
      self
        .send_cdp_and_wait_for_load(&ws_url, "Runtime.evaluate", cdp_params, 30)
        .await?
    } else {
      self
        .send_cdp(&ws_url, "Runtime.evaluate", cdp_params)
        .await?
    };

    let value = if let Some(exception) = result.get("exceptionDetails") {
      let text = exception
        .get("text")
        .or_else(|| {
          exception
            .get("exception")
            .and_then(|e| e.get("description"))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown error");
      serde_json::json!({ "error": text })
    } else if let Some(r) = result.get("result") {
      let val = r.get("value").cloned().unwrap_or(serde_json::json!(null));
      serde_json::json!({ "value": val, "type": r.get("type") })
    } else {
      serde_json::json!({ "value": null })
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&value).unwrap_or_default()
      }]
    }))
  }

  async fn handle_click_element(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let selector = arguments
      .get("selector")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing selector".to_string(),
      })?;

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let selector_escaped = selector.replace('\\', "\\\\").replace('\'', "\\'");
    let js = format!(
      r#"(() => {{
        const el = document.querySelector('{}');
        if (!el) throw new Error('Element not found: {}');
        el.scrollIntoView({{block: 'center'}});
        el.click();
        return true;
      }})()"#,
      selector_escaped, selector_escaped
    );

    // Use send_cdp_and_wait_for_load: if the click triggers navigation,
    // we wait for the new page to load. If not, the 10s timeout expires
    // and we return immediately.
    let result = self
      .send_cdp_and_wait_for_load(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": js,
          "returnByValue": true,
        }),
        10,
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let msg = exception
        .get("exception")
        .and_then(|e| e.get("description"))
        .or_else(|| exception.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("Click failed");
      return Err(McpError {
        code: -32000,
        message: msg.to_string(),
      });
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Clicked element: {selector}")
      }]
    }))
  }

  async fn handle_type_text(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let selector = arguments
      .get("selector")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing selector".to_string(),
      })?;
    let text = arguments
      .get("text")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing text".to_string(),
      })?;
    let clear_first = arguments
      .get("clear_first")
      .and_then(|v| v.as_bool())
      .unwrap_or(true);
    let instant = arguments
      .get("instant")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);
    let wpm = arguments.get("wpm").and_then(|v| v.as_f64());

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let selector_escaped = selector.replace('\\', "\\\\").replace('\'', "\\'");
    let focus_js = if clear_first {
      format!(
        r#"(() => {{
          const el = document.querySelector('{}');
          if (!el) throw new Error('Element not found: {}');
          el.scrollIntoView({{block: 'center'}});
          el.focus();
          el.value = '';
          el.dispatchEvent(new Event('input', {{bubbles: true}}));
          return true;
        }})()"#,
        selector_escaped, selector_escaped
      )
    } else {
      format!(
        r#"(() => {{
          const el = document.querySelector('{}');
          if (!el) throw new Error('Element not found: {}');
          el.scrollIntoView({{block: 'center'}});
          el.focus();
          return true;
        }})()"#,
        selector_escaped, selector_escaped
      )
    };

    let focus_result = self
      .send_cdp(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": focus_js,
          "returnByValue": true,
        }),
      )
      .await?;

    if let Some(exception) = focus_result.get("exceptionDetails") {
      let msg = exception
        .get("exception")
        .and_then(|e| e.get("description"))
        .or_else(|| exception.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("Focus failed");
      return Err(McpError {
        code: -32000,
        message: msg.to_string(),
      });
    }

    if instant {
      self
        .send_cdp(
          &ws_url,
          "Input.insertText",
          serde_json::json!({ "text": text }),
        )
        .await?;
    } else {
      self.send_human_keystrokes(&ws_url, text, wpm).await?;
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Typed text into element: {selector}")
      }]
    }))
  }

  /// CDP `Input.dispatchKeyEvent` fields for a named key: (key, code, virtual key code).
  /// Returns `None` for unknown names; single printable characters are handled separately.
  fn cdp_named_key(name: &str) -> Option<(&'static str, &'static str, i64)> {
    Some(match name {
      "Enter" => ("Enter", "Enter", 13),
      "Tab" => ("Tab", "Tab", 9),
      "Escape" | "Esc" => ("Escape", "Escape", 27),
      "Backspace" => ("Backspace", "Backspace", 8),
      "Delete" | "Del" => ("Delete", "Delete", 46),
      "ArrowUp" | "Up" => ("ArrowUp", "ArrowUp", 38),
      "ArrowDown" | "Down" => ("ArrowDown", "ArrowDown", 40),
      "ArrowLeft" | "Left" => ("ArrowLeft", "ArrowLeft", 37),
      "ArrowRight" | "Right" => ("ArrowRight", "ArrowRight", 39),
      "Home" => ("Home", "Home", 36),
      "End" => ("End", "End", 35),
      "PageUp" => ("PageUp", "PageUp", 33),
      "PageDown" => ("PageDown", "PageDown", 34),
      "Space" => (" ", "Space", 32),
      _ => return None,
    })
  }

  /// WebDriver (BiDi) key value for a named key — special code points in the PUA.
  fn bidi_named_key(name: &str) -> Option<&'static str> {
    Some(match name {
      "Enter" => "\u{E007}",
      "Tab" => "\u{E004}",
      "Escape" | "Esc" => "\u{E00C}",
      "Backspace" => "\u{E003}",
      "Delete" | "Del" => "\u{E017}",
      "ArrowUp" | "Up" => "\u{E013}",
      "ArrowDown" | "Down" => "\u{E015}",
      "ArrowLeft" | "Left" => "\u{E012}",
      "ArrowRight" | "Right" => "\u{E014}",
      "Home" => "\u{E011}",
      "End" => "\u{E010}",
      "PageUp" => "\u{E00E}",
      "PageDown" => "\u{E00F}",
      "Space" => " ",
      _ => return None,
    })
  }

  /// Modifier-key info: (cdp key, cdp code, virtual key code, CDP modifier bit, BiDi key value).
  /// CDP modifier bitmask: Alt=1, Control=2, Meta=4, Shift=8.
  fn modifier_info(name: &str) -> Option<(&'static str, &'static str, i64, i64, &'static str)> {
    Some(match name {
      "Shift" => ("Shift", "ShiftLeft", 16, 8, "\u{E008}"),
      "Control" | "Ctrl" => ("Control", "ControlLeft", 17, 2, "\u{E009}"),
      "Alt" | "Option" => ("Alt", "AltLeft", 18, 1, "\u{E00A}"),
      "Meta" | "Cmd" | "Command" => ("Meta", "MetaLeft", 91, 4, "\u{E03D}"),
      _ => return None,
    })
  }

  async fn handle_press_key(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let key = arguments
      .get("key")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing key".to_string(),
      })?;
    let modifiers: Vec<String> = arguments
      .get("modifiers")
      .and_then(|v| v.as_array())
      .map(|a| {
        a.iter()
          .filter_map(|m| m.as_str().map(|s| s.to_string()))
          .collect()
      })
      .unwrap_or_default();

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    if let Some(port) = bidi_port(&ws_url) {
      self.press_key_bidi(port, key, &modifiers).await?;
    } else {
      self.press_key_cdp(&ws_url, key, &modifiers).await?;
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Pressed key: {key}")
      }]
    }))
  }

  /// Press a key via CDP `Input.dispatchKeyEvent`. Holds modifiers around the
  /// main key by sending each modifier's keyDown first and keyUp (reversed) last,
  /// carrying the accumulated `modifiers` bitmask on every event.
  async fn press_key_cdp(
    &self,
    ws_url: &str,
    key: &str,
    modifiers: &[String],
  ) -> Result<(), McpError> {
    let mut bitmask = 0i64;
    let mut mod_keys: Vec<(&'static str, &'static str, i64)> = Vec::new();
    for m in modifiers {
      let (mk, mc, mvk, bit, _) = Self::modifier_info(m).ok_or_else(|| McpError {
        code: -32602,
        message: format!("Unknown modifier: {m}"),
      })?;
      bitmask |= bit;
      mod_keys.push((mk, mc, mvk));
    }

    // Resolve the main key into CDP fields. Named keys carry a code+VK; a single
    // printable character also carries `text` so it inserts — unless a non-shift
    // modifier (Alt/Ctrl/Meta) is held, which makes it a shortcut, not a character.
    let (cdp_key, cdp_code, vk, text): (String, Option<&str>, Option<i64>, Option<String>) =
      if let Some((k, c, v)) = Self::cdp_named_key(key) {
        (k.to_string(), Some(c), Some(v), None)
      } else {
        let mut chars = key.chars();
        let ch = chars.next();
        if ch.is_none() || chars.next().is_some() {
          return Err(McpError {
            code: -32602,
            message: format!("Unsupported key: {key}"),
          });
        }
        let ch = ch.expect("checked Some above");
        let suppress_text = (bitmask & 0b0000_0111) != 0; // Alt|Control|Meta
        (
          ch.to_string(),
          None,
          None,
          if suppress_text {
            None
          } else {
            Some(ch.to_string())
          },
        )
      };

    for (mk, mc, mvk) in &mod_keys {
      self
        .send_cdp(
          ws_url,
          "Input.dispatchKeyEvent",
          serde_json::json!({
            "type": "keyDown", "key": mk, "code": mc,
            "windowsVirtualKeyCode": mvk, "nativeVirtualKeyCode": mvk,
            "modifiers": bitmask,
          }),
        )
        .await?;
    }

    let mut down = serde_json::Map::new();
    down.insert("type".into(), serde_json::json!("keyDown"));
    down.insert("key".into(), serde_json::json!(cdp_key));
    down.insert("modifiers".into(), serde_json::json!(bitmask));
    if let Some(c) = cdp_code {
      down.insert("code".into(), serde_json::json!(c));
    }
    if let Some(v) = vk {
      down.insert("windowsVirtualKeyCode".into(), serde_json::json!(v));
      down.insert("nativeVirtualKeyCode".into(), serde_json::json!(v));
    }
    if let Some(t) = &text {
      down.insert("text".into(), serde_json::json!(t));
      down.insert("unmodifiedText".into(), serde_json::json!(t));
    }
    let mut up = down.clone();
    up.insert("type".into(), serde_json::json!("keyUp"));
    up.remove("text");
    up.remove("unmodifiedText");

    self
      .send_cdp(
        ws_url,
        "Input.dispatchKeyEvent",
        serde_json::Value::Object(down),
      )
      .await?;
    self
      .send_cdp(
        ws_url,
        "Input.dispatchKeyEvent",
        serde_json::Value::Object(up),
      )
      .await?;

    for (mk, mc, mvk) in mod_keys.iter().rev() {
      self
        .send_cdp(
          ws_url,
          "Input.dispatchKeyEvent",
          serde_json::json!({
            "type": "keyUp", "key": mk, "code": mc,
            "windowsVirtualKeyCode": mvk, "nativeVirtualKeyCode": mvk,
          }),
        )
        .await?;
    }

    Ok(())
  }

  /// Press a key via BiDi `input.performActions`: keyDown each modifier, the main
  /// key down+up, then keyUp the modifiers in reverse.
  async fn press_key_bidi(
    &self,
    port: u16,
    key: &str,
    modifiers: &[String],
  ) -> Result<(), McpError> {
    let mut mod_values: Vec<&'static str> = Vec::new();
    let mut actions: Vec<serde_json::Value> = Vec::new();
    for m in modifiers {
      let (_, _, _, _, val) = Self::modifier_info(m).ok_or_else(|| McpError {
        code: -32602,
        message: format!("Unknown modifier: {m}"),
      })?;
      actions.push(serde_json::json!({ "type": "keyDown", "value": val }));
      mod_values.push(val);
    }

    let main = if let Some(v) = Self::bidi_named_key(key) {
      v.to_string()
    } else {
      let mut chars = key.chars();
      let ch = chars.next();
      if ch.is_none() || chars.next().is_some() {
        return Err(McpError {
          code: -32602,
          message: format!("Unsupported key: {key}"),
        });
      }
      ch.expect("checked Some above").to_string()
    };

    actions.push(serde_json::json!({ "type": "keyDown", "value": main }));
    actions.push(serde_json::json!({ "type": "keyUp", "value": main }));
    for val in mod_values.iter().rev() {
      actions.push(serde_json::json!({ "type": "keyUp", "value": val }));
    }

    self
      .bidi_exec(port, BidiOp::PerformKeys { actions })
      .await
      .map(|_| ())
  }

  async fn handle_upload_file(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let selector = arguments
      .get("selector")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing selector".to_string(),
      })?;
    let files: Vec<String> = arguments
      .get("files")
      .and_then(|v| v.as_array())
      .map(|a| {
        a.iter()
          .filter_map(|f| f.as_str().map(|s| s.to_string()))
          .collect()
      })
      .unwrap_or_default();
    if files.is_empty() {
      return Err(McpError {
        code: -32602,
        message: "No files provided".to_string(),
      });
    }

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;
    let count = files.len();

    if let Some(port) = bidi_port(&ws_url) {
      self
        .bidi_exec(
          port,
          BidiOp::SetFiles {
            selector: selector.to_string(),
            files,
          },
        )
        .await?;
    } else {
      // CDP: resolve the input node, then DOM.setFileInputFiles.
      let doc = self
        .send_cdp(
          &ws_url,
          "DOM.getDocument",
          serde_json::json!({ "depth": 0 }),
        )
        .await?;
      let root_id = doc
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| McpError {
          code: -32000,
          message: "Could not read document root".to_string(),
        })?;
      let found = self
        .send_cdp(
          &ws_url,
          "DOM.querySelector",
          serde_json::json!({ "nodeId": root_id, "selector": selector }),
        )
        .await?;
      let node_id = found
        .get("nodeId")
        .and_then(|v| v.as_i64())
        .filter(|n| *n != 0)
        .ok_or_else(|| McpError {
          code: -32000,
          message: format!("File input not found: {selector}"),
        })?;
      self
        .send_cdp(
          &ws_url,
          "DOM.setFileInputFiles",
          serde_json::json!({ "nodeId": node_id, "files": files }),
        )
        .await?;
    }

    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": format!("Attached {count} file(s) to {selector}") }]
    }))
  }

  async fn handle_list_tabs(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let tabs = if let Some(port) = bidi_port(&ws_url) {
      self
        .bidi_exec(port, BidiOp::ListTabs)
        .await?
        .get("tabs")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]))
    } else {
      let pages = self.cdp_page_targets(cdp_port).await?;
      serde_json::Value::Array(
        pages
          .iter()
          .enumerate()
          .map(|(i, t)| {
            serde_json::json!({
              "index": i,
              "url": t.get("url").and_then(|v| v.as_str()).unwrap_or_default(),
              "title": t.get("title").and_then(|v| v.as_str()).unwrap_or_default(),
            })
          })
          .collect(),
      )
    };

    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": serde_json::to_string_pretty(&tabs).unwrap_or_default() }]
    }))
  }

  async fn handle_new_tab(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let url = arguments
      .get("url")
      .and_then(|v| v.as_str())
      .unwrap_or("about:blank");
    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    if let Some(port) = bidi_port(&ws_url) {
      self
        .bidi_exec(
          port,
          BidiOp::NewTab {
            url: url.to_string(),
          },
        )
        .await?;
    } else {
      let browser_ws = self.get_cdp_browser_ws_url(cdp_port).await?;
      let created = self
        .send_cdp(
          &browser_ws,
          "Target.createTarget",
          serde_json::json!({ "url": url }),
        )
        .await?;
      if let Some(target_id) = created.get("targetId").and_then(|v| v.as_str()) {
        self
          .active_targets
          .lock()
          .await
          .insert(profile.id.to_string(), target_id.to_string());
      }
    }

    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": format!("Opened new tab: {url}") }]
    }))
  }

  async fn handle_switch_tab(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let index = arguments.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    if let Some(port) = bidi_port(&ws_url) {
      self.bidi_exec(port, BidiOp::SwitchTab { index }).await?;
    } else {
      let pages = self.cdp_page_targets(cdp_port).await?;
      let target = pages.get(index).ok_or_else(|| McpError {
        code: -32000,
        message: format!("No tab at index {index}"),
      })?;
      let target_id = target
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
      let browser_ws = self.get_cdp_browser_ws_url(cdp_port).await?;
      self
        .send_cdp(
          &browser_ws,
          "Target.activateTarget",
          serde_json::json!({ "targetId": target_id }),
        )
        .await?;
      self
        .active_targets
        .lock()
        .await
        .insert(profile.id.to_string(), target_id);
    }

    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": format!("Switched to tab {index}") }]
    }))
  }

  async fn handle_close_tab(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let index = arguments.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    if let Some(port) = bidi_port(&ws_url) {
      self.bidi_exec(port, BidiOp::CloseTab { index }).await?;
    } else {
      let pages = self.cdp_page_targets(cdp_port).await?;
      let target = pages.get(index).ok_or_else(|| McpError {
        code: -32000,
        message: format!("No tab at index {index}"),
      })?;
      let target_id = target
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
      let browser_ws = self.get_cdp_browser_ws_url(cdp_port).await?;
      self
        .send_cdp(
          &browser_ws,
          "Target.closeTarget",
          serde_json::json!({ "targetId": target_id }),
        )
        .await?;
      // Drop the active pointer if we just closed the active tab.
      let pid = profile.id.to_string();
      let mut active = self.active_targets.lock().await;
      if active.get(&pid) == Some(&target_id) {
        active.remove(&pid);
      }
    }

    Ok(serde_json::json!({
      "content": [{ "type": "text", "text": format!("Closed tab {index}") }]
    }))
  }

  async fn handle_get_page_content(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let format = arguments
      .get("format")
      .and_then(|v| v.as_str())
      .unwrap_or("text");
    let selector = arguments.get("selector").and_then(|v| v.as_str());
    let max_chars = arguments
      .get("max_chars")
      .and_then(|v| v.as_u64())
      .map(|n| n as usize)
      .unwrap_or(40_000);

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let js = if let Some(sel) = selector {
      let sel_escaped = sel.replace('\\', "\\\\").replace('\'', "\\'");
      if format == "html" {
        format!(
          r#"(() => {{
            const el = document.querySelector('{}');
            return el ? el.outerHTML : null;
          }})()"#,
          sel_escaped
        )
      } else {
        format!(
          r#"(() => {{
            const el = document.querySelector('{}');
            return el ? el.innerText : null;
          }})()"#,
          sel_escaped
        )
      }
    } else if format == "html" {
      "document.documentElement.outerHTML".to_string()
    } else {
      "document.body.innerText".to_string()
    };

    let result = self
      .send_cdp(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": js,
          "returnByValue": true,
        }),
      )
      .await?;

    let content = result
      .get("result")
      .and_then(|r| r.get("value"))
      .and_then(|v| v.as_str())
      .unwrap_or("");

    // Cap output so a 500 KB DOM dump doesn't blow out the agent's context.
    // Slice on character boundaries (chars().take().collect()) rather than
    // byte indices, since the latter would panic on multi-byte boundaries.
    let total_chars = content.chars().count();
    let (text, truncated) = if total_chars > max_chars {
      (content.chars().take(max_chars).collect::<String>(), true)
    } else {
      (content.to_string(), false)
    };

    let payload = if truncated {
      format!(
        "{text}\n\n[truncated: showing {max_chars} of {total_chars} chars — call with a larger max_chars or use get_interactive_elements for an indexed view]"
      )
    } else {
      text
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": payload
      }]
    }))
  }

  async fn handle_get_page_info(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let result = self
      .send_cdp(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": "JSON.stringify({url: location.href, title: document.title, readyState: document.readyState})",
          "returnByValue": true,
        }),
      )
      .await?;

    let info_str = result
      .get("result")
      .and_then(|r| r.get("value"))
      .and_then(|v| v.as_str())
      .unwrap_or("{}");

    let info: serde_json::Value = serde_json::from_str(info_str).unwrap_or(serde_json::json!({}));

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&info).unwrap_or_default()
      }]
    }))
  }

  async fn handle_get_interactive_elements(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let max_chars = arguments
      .get("max_chars")
      .and_then(|v| v.as_u64())
      .map(|n| n as usize)
      .unwrap_or(40_000);

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    // Walk the DOM for visible, non-disabled interactive elements, label them
    // with a zero-based index, and cache the live references on
    // `window.__watermelon_interactive` so click_by_index / type_by_index can
    // resolve the index → Element without round-tripping a selector.
    let js = INTERACTIVE_ELEMENTS_JS.replace("__MAX_CHARS__", &max_chars.to_string());

    let result = self
      .send_cdp(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": js,
          "returnByValue": true,
        }),
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let msg = exception
        .get("exception")
        .and_then(|e| e.get("description"))
        .or_else(|| exception.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("Enumeration failed");
      return Err(McpError {
        code: -32000,
        message: msg.to_string(),
      });
    }

    let payload_str = result
      .get("result")
      .and_then(|r| r.get("value"))
      .and_then(|v| v.as_str())
      .unwrap_or("{}");

    let payload: serde_json::Value =
      serde_json::from_str(payload_str).unwrap_or(serde_json::json!({}));
    let elements = payload
      .get("elements")
      .and_then(|v| v.as_str())
      .unwrap_or("");
    let count = payload.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let truncated = payload
      .get("truncated")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);

    let header = if truncated {
      format!("{count} interactive elements (truncated at {max_chars} chars — re-call with a larger max_chars or scroll the page):")
    } else {
      format!("{count} interactive elements:")
    };

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("{header}\n{elements}")
      }]
    }))
  }

  async fn handle_click_by_index(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let index = arguments
      .get("index")
      .and_then(|v| v.as_u64())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing index".to_string(),
      })?;

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    let js = format!(
      r#"(() => {{
        const arr = window.__watermelon_interactive;
        if (!arr || !arr[{index}]) throw new Error('No element at index {index}. Call get_interactive_elements first or after navigation.');
        const el = arr[{index}];
        el.scrollIntoView({{block: 'center'}});
        el.click();
        return true;
      }})()"#
    );

    let result = self
      .send_cdp_and_wait_for_load(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": js,
          "returnByValue": true,
        }),
        10,
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let msg = exception
        .get("exception")
        .and_then(|e| e.get("description"))
        .or_else(|| exception.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("Click failed");
      return Err(McpError {
        code: -32000,
        message: msg.to_string(),
      });
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Clicked element at index {index}")
      }]
    }))
  }

  async fn handle_type_by_index(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<serde_json::Value, McpError> {
    let profile_id = arguments
      .get("profile_id")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing profile_id".to_string(),
      })?;
    let index = arguments
      .get("index")
      .and_then(|v| v.as_u64())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing index".to_string(),
      })?;
    let text = arguments
      .get("text")
      .and_then(|v| v.as_str())
      .ok_or_else(|| McpError {
        code: -32602,
        message: "Missing text".to_string(),
      })?;
    let clear_first = arguments
      .get("clear_first")
      .and_then(|v| v.as_bool())
      .unwrap_or(true);
    let instant = arguments
      .get("instant")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);
    let wpm = arguments.get("wpm").and_then(|v| v.as_f64());

    let profile = self.get_running_profile(profile_id)?;
    let cdp_port = self.get_cdp_port_for_profile(&profile).await?;
    let ws_url = self.get_cdp_ws_url(&profile, cdp_port).await?;

    // Mirrors handle_type_text's focus step but resolves the element via the
    // cached index instead of a CSS selector.
    let focus_js = if clear_first {
      format!(
        r#"(() => {{
          const arr = window.__watermelon_interactive;
          if (!arr || !arr[{index}]) throw new Error('No element at index {index}. Call get_interactive_elements first or after navigation.');
          const el = arr[{index}];
          el.scrollIntoView({{block: 'center'}});
          el.focus();
          el.value = '';
          el.dispatchEvent(new Event('input', {{bubbles: true}}));
          return true;
        }})()"#
      )
    } else {
      format!(
        r#"(() => {{
          const arr = window.__watermelon_interactive;
          if (!arr || !arr[{index}]) throw new Error('No element at index {index}. Call get_interactive_elements first or after navigation.');
          const el = arr[{index}];
          el.scrollIntoView({{block: 'center'}});
          el.focus();
          return true;
        }})()"#
      )
    };

    let focus_result = self
      .send_cdp(
        &ws_url,
        "Runtime.evaluate",
        serde_json::json!({
          "expression": focus_js,
          "returnByValue": true,
        }),
      )
      .await?;

    if let Some(exception) = focus_result.get("exceptionDetails") {
      let msg = exception
        .get("exception")
        .and_then(|e| e.get("description"))
        .or_else(|| exception.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("Focus failed");
      return Err(McpError {
        code: -32000,
        message: msg.to_string(),
      });
    }

    if instant {
      self
        .send_cdp(
          &ws_url,
          "Input.insertText",
          serde_json::json!({ "text": text }),
        )
        .await?;
    } else {
      self.send_human_keystrokes(&ws_url, text, wpm).await?;
    }

    Ok(serde_json::json!({
      "content": [{
        "type": "text",
        "text": format!("Typed text into element at index {index}")
      }]
    }))
  }

  /// Read the realized fingerprint from a running profile's live page by
  /// evaluating a fixed JS snippet. Works for both Camoufox (BiDi) and Cloak
  /// (CDP) because `send_cdp` routes by the ws-url scheme. Returns the collected
  /// object (userAgent, platform, screen, WebGL vendor/renderer, timezone, …).
  pub async fn read_live_fingerprint(&self, profile_id: &str) -> Result<serde_json::Value, String> {
    let profile = self
      .get_running_profile(profile_id)
      .map_err(|e| e.message)?;
    let cdp_port = self
      .get_cdp_port_for_profile(&profile)
      .await
      .map_err(|e| e.message)?;
    let ws_url = self
      .get_cdp_ws_url(&profile, cdp_port)
      .await
      .map_err(|e| e.message)?;

    let expression = r#"(() => {
      const out = {};
      const set = (k, fn) => { try { out[k] = fn(); } catch (e) {} };
      set('userAgent', () => navigator.userAgent);
      set('platform', () => navigator.platform);
      set('vendor', () => navigator.vendor);
      set('language', () => navigator.language);
      set('languages', () => navigator.languages);
      set('hardwareConcurrency', () => navigator.hardwareConcurrency);
      set('deviceMemory', () => navigator.deviceMemory);
      set('maxTouchPoints', () => navigator.maxTouchPoints);
      set('webdriver', () => navigator.webdriver);
      set('doNotTrack', () => navigator.doNotTrack);
      set('screen', () => ({
        width: screen.width, height: screen.height,
        availWidth: screen.availWidth, availHeight: screen.availHeight,
        colorDepth: screen.colorDepth, pixelDepth: screen.pixelDepth,
      }));
      set('devicePixelRatio', () => window.devicePixelRatio);
      set('innerSize', () => `${window.innerWidth}x${window.innerHeight}`);
      set('outerSize', () => `${window.outerWidth}x${window.outerHeight}`);
      set('timezone', () => Intl.DateTimeFormat().resolvedOptions().timeZone);
      set('timezoneOffset', () => new Date().getTimezoneOffset());
      try {
        const c = document.createElement('canvas');
        const gl = c.getContext('webgl') || c.getContext('experimental-webgl');
        if (gl) {
          const dbg = gl.getExtension('WEBGL_debug_renderer_info');
          if (dbg) {
            out.webglVendor = gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL);
            out.webglRenderer = gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL);
          }
        }
      } catch (e) {}
      return out;
    })()"#;

    let params = serde_json::json!({
      "expression": expression,
      "returnByValue": true,
      "awaitPromise": false,
    });

    let result = self
      .send_cdp(&ws_url, "Runtime.evaluate", params)
      .await
      .map_err(|e| e.message)?;

    if let Some(exception) = result.get("exceptionDetails") {
      let text = exception
        .get("exception")
        .and_then(|e| e.get("description"))
        .or_else(|| exception.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("Failed to evaluate fingerprint script");
      return Err(text.to_string());
    }

    let value = result
      .get("result")
      .and_then(|r| r.get("value"))
      .cloned()
      .unwrap_or(serde_json::json!({}));
    Ok(value)
  }
}

lazy_static::lazy_static! {
  static ref MCP_SERVER: McpServer = McpServer::new();
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_mcp_tools_count() {
    let server = McpServer::new();
    let tools = server.get_tools();

    // Should have at least 37 tools (30 + 7 browser interaction tools)
    assert!(tools.len() >= 37);

    // Check tool names
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    // Profile tools
    assert!(tool_names.contains(&"list_profiles"));
    assert!(tool_names.contains(&"get_profile"));
    assert!(tool_names.contains(&"run_profile"));
    assert!(tool_names.contains(&"kill_profile"));
    assert!(tool_names.contains(&"get_profile_status"));
    // Group tools
    assert!(tool_names.contains(&"list_groups"));
    assert!(tool_names.contains(&"get_group"));
    assert!(tool_names.contains(&"create_group"));
    assert!(tool_names.contains(&"update_group"));
    assert!(tool_names.contains(&"delete_group"));
    assert!(tool_names.contains(&"assign_profiles_to_group"));
    // Proxy tools
    assert!(tool_names.contains(&"list_proxies"));
    assert!(tool_names.contains(&"get_proxy"));
    assert!(tool_names.contains(&"create_proxy"));
    assert!(tool_names.contains(&"update_proxy"));
    assert!(tool_names.contains(&"delete_proxy"));
    // Proxy import/export tools
    assert!(tool_names.contains(&"export_proxies"));
    assert!(tool_names.contains(&"import_proxies"));
    // VPN tools
    assert!(tool_names.contains(&"import_vpn"));
    assert!(tool_names.contains(&"list_vpn_configs"));
    assert!(tool_names.contains(&"delete_vpn"));
    assert!(tool_names.contains(&"connect_vpn"));
    assert!(tool_names.contains(&"disconnect_vpn"));
    assert!(tool_names.contains(&"get_vpn_status"));
    // Fingerprint tools
    assert!(tool_names.contains(&"get_profile_fingerprint"));
    assert!(tool_names.contains(&"update_profile_fingerprint"));
    assert!(tool_names.contains(&"update_profile_proxy_bypass_rules"));
    // Extension tools
    assert!(tool_names.contains(&"list_extensions"));
    assert!(tool_names.contains(&"list_extension_groups"));
    assert!(tool_names.contains(&"create_extension_group"));
    assert!(tool_names.contains(&"delete_extension"));
    assert!(tool_names.contains(&"delete_extension_group"));
    assert!(tool_names.contains(&"assign_extension_group_to_profile"));
    // Cookie tools
    assert!(tool_names.contains(&"import_profile_cookies"));
    // Team lock tools
    assert!(tool_names.contains(&"get_team_locks"));
    assert!(tool_names.contains(&"get_team_lock_status"));
    // Browser interaction tools
    assert!(tool_names.contains(&"navigate"));
    assert!(tool_names.contains(&"screenshot"));
    assert!(tool_names.contains(&"evaluate_javascript"));
    assert!(tool_names.contains(&"click_element"));
    assert!(tool_names.contains(&"type_text"));
    assert!(tool_names.contains(&"get_page_content"));
    assert!(tool_names.contains(&"get_page_info"));
  }

  #[test]
  fn test_mcp_server_initial_state() {
    let server = McpServer::new();
    assert!(!server.is_running());
  }
}
