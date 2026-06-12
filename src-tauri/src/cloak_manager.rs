//! CloakManager — launches the Cloak (CloakHQ/cloakbrowser) patched Chromium and
//! tracks running instances for CDP automation.
//!
//! Cloak derives its whole fingerprint from a numeric `--fingerprint=<seed>`
//! flag (no runtime injection, no token) plus a few
//! `--fingerprint-*` flags applied at launch — the binary auto-generates
//! GPU/screen/hardware from the seed. So this manager just assembles args, spawns
//! the process, waits for CDP, and records the debug port. No fingerprint
//! injection, no token, no terms gate. It speaks standard Chromium CDP, so the
//! existing `mcp_server` CDP path drives it without changes.

use crate::browser_runner::BrowserRunner;
use crate::profile::BrowserProfile;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex as AsyncMutex;

/// Persisted per-profile Cloak configuration. The identity is the `seed`; every
/// other field is an optional `--fingerprint-*` refinement.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloakConfig {
  /// `--fingerprint=<seed>` (10000–99999). None → generated on first launch.
  #[serde(default)]
  pub seed: Option<u32>,
  /// Generate a fresh seed on every launch (rotates identity).
  #[serde(default)]
  pub randomize_seed_on_launch: Option<bool>,
  /// `--fingerprint-platform` — "windows" | "macos" | "linux".
  #[serde(default)]
  pub os: Option<String>,
  /// `--fingerprint-timezone` (IANA, e.g. "America/New_York").
  #[serde(default)]
  pub timezone: Option<String>,
  /// `--fingerprint-locale` + `--lang` (BCP-47, e.g. "en-US").
  #[serde(default)]
  pub locale: Option<String>,
  #[serde(default)]
  pub screen_width: Option<u32>,
  #[serde(default)]
  pub screen_height: Option<u32>,
  #[serde(default)]
  pub block_images: Option<bool>,
  #[serde(default)]
  pub block_webrtc: Option<bool>,
  #[serde(default)]
  pub block_webgl: Option<bool>,
  /// Reserved for phase 2 (webrtc-ip auto from proxy exit). Kept for config-form parity.
  #[serde(default)]
  pub geoip: Option<serde_json::Value>,
  #[serde(default, skip_serializing)]
  pub proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct CloakLaunchResult {
  pub id: String,
  #[serde(alias = "process_id")]
  pub processId: Option<u32>,
  #[serde(alias = "profile_path")]
  pub profilePath: Option<String>,
  pub url: Option<String>,
  pub cdp_port: Option<u16>,
  /// The seed actually used (newly generated when randomizing or when none was
  /// stored). The caller persists it back to the profile config.
  #[serde(default, skip_serializing)]
  pub used_seed: Option<u32>,
}

struct CloakInstance {
  process_id: Option<u32>,
  profile_path: Option<String>,
  url: Option<String>,
  cdp_port: Option<u16>,
}

struct CloakManagerInner {
  instances: HashMap<String, CloakInstance>,
}

pub struct CloakManager {
  inner: Arc<AsyncMutex<CloakManagerInner>>,
  http_client: Client,
}

#[derive(Debug, Deserialize)]
struct CdpTarget {
  #[serde(rename = "type")]
  target_type: String,
  #[serde(rename = "webSocketDebuggerUrl")]
  websocket_debugger_url: Option<String>,
}

/// Default `--fingerprint-platform` for the host when the profile didn't pin one.
fn host_platform() -> &'static str {
  if cfg!(target_os = "macos") {
    "macos"
  } else if cfg!(target_os = "linux") {
    "linux"
  } else {
    "windows"
  }
}

/// Assemble the full Chromium argument list for a Cloak launch. Pure function so
/// the flag logic is unit-testable without spawning a browser.
#[allow(clippy::too_many_arguments)]
fn build_cloak_args(
  port: u16,
  profile_path: &str,
  config: &CloakConfig,
  seed: u32,
  proxy_url: Option<&str>,
  ephemeral: bool,
  extension_paths: &[String],
  headless: bool,
) -> Vec<String> {
  let mut args = vec![
    format!("--remote-debugging-port={port}"),
    "--remote-debugging-address=127.0.0.1".to_string(),
    format!("--user-data-dir={profile_path}"),
    "--no-first-run".to_string(),
    "--no-default-browser-check".to_string(),
    "--disable-background-mode".to_string(),
    "--disable-component-update".to_string(),
    "--disable-background-timer-throttling".to_string(),
    "--disable-session-crashed-bubble".to_string(),
    "--hide-crash-restore-bubble".to_string(),
    "--disable-infobars".to_string(),
    "--use-mock-keychain".to_string(),
    "--password-store=basic".to_string(),
  ];

  // --- Fingerprint flags (the heart of Cloak) ---
  args.push(format!("--fingerprint={seed}"));
  let platform: &str = match config.os.as_deref() {
    Some(p) => p,
    None => host_platform(),
  };
  args.push(format!("--fingerprint-platform={platform}"));
  if let Some(tz) = config.timezone.as_deref().filter(|s| !s.is_empty()) {
    args.push(format!("--fingerprint-timezone={tz}"));
  }
  if let Some(locale) = config.locale.as_deref().filter(|s| !s.is_empty()) {
    args.push(format!("--fingerprint-locale={locale}"));
    args.push(format!("--lang={locale}"));
  }
  // Always pin screen dimensions. The binary auto-generates them from the seed,
  // but that value is incoherent when the platform is spoofed across hosts
  // (e.g. macOS profile launched on Windows collapses screen.width to 1). Pass
  // explicit defaults matching Cloak's documented per-platform sizes — 1920x1080
  // for Windows/Linux, 1440x900 for macOS — unless the user pinned a size.
  let (default_w, default_h) = if platform == "macos" {
    (1440u32, 900u32)
  } else {
    (1920u32, 1080u32)
  };
  args.push(format!(
    "--fingerprint-screen-width={}",
    config.screen_width.unwrap_or(default_w)
  ));
  args.push(format!(
    "--fingerprint-screen-height={}",
    config.screen_height.unwrap_or(default_h)
  ));

  // WebGL/WebRTC/image blocking via standard Chromium switches.
  if config.block_webgl.unwrap_or(false) {
    args.push("--disable-3d-apis".to_string());
  }
  if config.block_webrtc.unwrap_or(false) {
    args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_string());
  }
  if config.block_images.unwrap_or(false) {
    args.push("--blink-settings=imagesEnabled=false".to_string());
  }

  // WebGL/WebGPU need this on Windows (and any headed software-GPU env).
  if cfg!(target_os = "windows") || !headless {
    args.push("--ignore-gpu-blocklist".to_string());
  }

  if headless {
    args.push("--headless=new".to_string());
  }

  #[cfg(target_os = "linux")]
  {
    // Sandbox flags are only needed on Linux (root/Docker/Xvfb). On Windows/macOS
    // `--no-sandbox` triggers a Chromium warning infobar and weakens security.
    args.push("--no-sandbox".to_string());
    args.push("--disable-setuid-sandbox".to_string());
    args.push("--disable-dev-shm-usage".to_string());
  }

  if ephemeral {
    args.push("--disk-cache-size=1".to_string());
    args.push("--disable-breakpad".to_string());
    args.push("--disable-crash-reporter".to_string());
    args.push("--no-service-autorun".to_string());
    args.push("--disable-sync".to_string());
  }

  if !extension_paths.is_empty() {
    args.push(format!("--load-extension={}", extension_paths.join(",")));
  }

  if let Some(proxy) = proxy_url {
    let pac_data = format!(
      "data:application/x-ns-proxy-autoconfig,function FindProxyForURL(url,host){{return \"PROXY {}\";}}",
      proxy.trim_start_matches("http://").trim_start_matches("https://")
    );
    args.push(format!("--proxy-pac-url={pac_data}"));
    args.push("--dns-prefetch-disable".to_string());
  }

  args
}

impl CloakManager {
  fn new() -> Self {
    Self {
      inner: Arc::new(AsyncMutex::new(CloakManagerInner {
        instances: HashMap::new(),
      })),
      http_client: Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("Failed to build reqwest client for cloak_manager"),
    }
  }

  pub fn instance() -> &'static CloakManager {
    &CLOAK_MANAGER
  }

  async fn find_free_port() -> Result<u16, Box<dyn std::error::Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
  }

  async fn wait_for_cdp_ready(
    &self,
    port: u16,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let max_attempts = 120;
    let delay = Duration::from_millis(500);

    let mut last_error: Option<String> = None;
    for attempt in 0..max_attempts {
      match self.http_client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
          log::info!("Cloak CDP ready on port {port} after {attempt} attempts");
          return Ok(());
        }
        Ok(resp) => {
          last_error = Some(format!("HTTP {} from {url}", resp.status()));
          tokio::time::sleep(delay).await;
        }
        Err(e) => {
          last_error = Some(format!("request failed: {e}"));
          tokio::time::sleep(delay).await;
        }
      }
    }

    let detail = last_error.unwrap_or_else(|| "no attempts completed".to_string());
    log::error!("Cloak CDP not ready after {max_attempts} attempts on port {port}: {detail}");
    Err(
      format!("Cloak CDP not ready after {max_attempts} attempts on port {port}: {detail}").into(),
    )
  }

  #[allow(clippy::too_many_arguments)]
  pub async fn launch_cloak(
    &self,
    _app_handle: &AppHandle,
    profile: &BrowserProfile,
    profile_path: &str,
    config: &CloakConfig,
    url: Option<&str>,
    proxy_url: Option<&str>,
    ephemeral: bool,
    extension_paths: &[String],
    remote_debugging_port: Option<u16>,
    headless: bool,
  ) -> Result<CloakLaunchResult, Box<dyn std::error::Error + Send + Sync>> {
    let executable_path = BrowserRunner::instance()
      .get_browser_executable_path(profile)
      .map_err(|e| format!("Failed to get Cloak executable path: {e}"))?;

    let port = match remote_debugging_port {
      Some(p) => p,
      None => Self::find_free_port().await?,
    };

    // Resolve the seed: regenerate when randomizing or when none stored.
    let randomize = config.randomize_seed_on_launch.unwrap_or(false);
    let seed = match config.seed {
      Some(s) if !randomize => s,
      _ => {
        use rand::RngExt;
        rand::rng().random_range(10000..=99999)
      }
    };
    let used_seed = (config.seed != Some(seed)).then_some(seed);

    log::info!("Launching Cloak on CDP port {port} (seed={seed}, detached)");

    let mut args = build_cloak_args(
      port,
      profile_path,
      config,
      seed,
      proxy_url,
      ephemeral,
      extension_paths,
      headless,
    );

    // Phase 2 — geoip: derive WebRTC IP + timezone/locale/location from the proxy
    // exit IP so the binary spoofs a coherent geo identity. Fail-open: any error
    // just skips the geo flags (the seed identity still stands). Disabled when the
    // profile explicitly sets `geoip: false`.
    let geoip_enabled = !matches!(config.geoip, Some(serde_json::Value::Bool(false)));
    if geoip_enabled {
      if let Some(proxy) = proxy_url {
        match crate::ip_utils::fetch_public_ip(Some(proxy)).await {
          Ok(ip) => {
            args.push(format!("--fingerprint-webrtc-ip={ip}"));
            if let Ok(geo) = crate::camoufox::geolocation::get_geolocation(&ip) {
              if config
                .timezone
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_none()
              {
                args.push(format!("--fingerprint-timezone={}", geo.timezone));
              }
              if config.locale.as_deref().filter(|s| !s.is_empty()).is_none() {
                let loc = geo.locale.as_string();
                args.push(format!("--fingerprint-locale={loc}"));
                args.push(format!("--lang={loc}"));
              }
              args.push(format!(
                "--fingerprint-location={},{}",
                geo.latitude, geo.longitude
              ));
              log::info!("Cloak geo applied from {ip}: tz={}", geo.timezone);
            }
          }
          Err(e) => log::warn!("Cloak geoip: failed to resolve exit IP, skipping: {e}"),
        }
      }
    }

    let mut command = TokioCommand::new(&executable_path);
    command
      .args(&args)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null());

    let child = command
      .spawn()
      .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
        let hint = if e.raw_os_error() == Some(14001) {
          ". This usually means the Visual C++ Redistributable is not installed. \
           Download it from https://aka.ms/vs/17/release/vc_redist.x64.exe"
        } else {
          ""
        };
        format!("Failed to spawn Cloak: {e}{hint}").into()
      })?;
    let process_id = child.id();
    drop(child);

    self.wait_for_cdp_ready(port).await?;

    // Navigate the first tab if a URL was requested (standard CDP).
    if let Some(url) = url {
      if let Ok(targets) = self.get_cdp_targets(port).await {
        if let Some(ws_url) = targets
          .iter()
          .find(|t| t.target_type == "page")
          .and_then(|t| t.websocket_debugger_url.as_deref())
        {
          if let Err(e) = self
            .send_cdp_command(ws_url, "Page.navigate", json!({ "url": url }))
            .await
          {
            log::error!("Cloak: failed to navigate to URL: {e}");
          }
        }
      }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let instance = CloakInstance {
      process_id,
      profile_path: Some(profile_path.to_string()),
      url: url.map(|s| s.to_string()),
      cdp_port: Some(port),
    };
    self
      .inner
      .lock()
      .await
      .instances
      .insert(id.clone(), instance);

    Ok(CloakLaunchResult {
      id,
      processId: process_id,
      profilePath: Some(profile_path.to_string()),
      url: url.map(|s| s.to_string()),
      cdp_port: Some(port),
      used_seed,
    })
  }

  async fn get_cdp_targets(
    &self,
    port: u16,
  ) -> Result<Vec<CdpTarget>, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("http://127.0.0.1:{port}/json");
    let resp = self.http_client.get(&url).send().await?;
    Ok(resp.json().await?)
  }

  async fn send_cdp_command(
    &self,
    ws_url: &str,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    use futures_util::sink::SinkExt;
    use futures_util::stream::StreamExt;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let (mut ws_stream, _) = connect_async(ws_url).await?;
    let command = json!({ "id": 1, "method": method, "params": params });
    ws_stream
      .send(Message::Text(command.to_string().into()))
      .await?;

    while let Some(msg) = ws_stream.next().await {
      match msg? {
        Message::Text(text) => {
          let response: serde_json::Value = serde_json::from_str(text.as_str())?;
          if response.get("id") == Some(&json!(1)) {
            if let Some(error) = response.get("error") {
              return Err(format!("CDP error: {error}").into());
            }
            return Ok(response.get("result").cloned().unwrap_or(json!({})));
          }
        }
        Message::Close(_) => break,
        _ => {}
      }
    }
    Err("No response received from CDP".into())
  }

  pub async fn stop_cloak(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut inner = self.inner.lock().await;
    if let Some(instance) = inner.instances.remove(id) {
      if let Some(pid) = instance.process_id {
        #[cfg(unix)]
        {
          use nix::sys::signal::{kill, Signal};
          use nix::unistd::Pid;
          let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        #[cfg(windows)]
        {
          use std::os::windows::process::CommandExt;
          const CREATE_NO_WINDOW: u32 = 0x08000000;
          let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        }
        log::info!("Stopped Cloak instance {id} (PID: {pid})");
      }
    }
    Ok(())
  }

  /// Open a URL in a new tab of a running Cloak instance via the CDP HTTP endpoint.
  pub async fn open_url_in_tab(
    &self,
    profile_path: &str,
    url: &str,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port = self
      .get_cdp_port(profile_path)
      .await
      .ok_or("Cloak instance (with CDP port) not found for profile")?;

    let new_tab_url = format!(
      "http://127.0.0.1:{port}/json/new?{}",
      urlencoding::encode(url)
    );
    let resp = self
      .http_client
      .put(&new_tab_url)
      .send()
      .await
      .map_err(|e| format!("Failed to open new tab: {e}"))?;
    if !resp.status().is_success() {
      return Err(format!("CDP /json/new returned HTTP {}", resp.status()).into());
    }
    Ok(())
  }

  pub async fn get_cdp_port(&self, profile_path: &str) -> Option<u16> {
    let inner = self.inner.lock().await;
    let target_path = std::path::Path::new(profile_path)
      .canonicalize()
      .unwrap_or_else(|_| std::path::Path::new(profile_path).to_path_buf());

    for instance in inner.instances.values() {
      if let Some(path) = &instance.profile_path {
        let instance_path = std::path::Path::new(path)
          .canonicalize()
          .unwrap_or_else(|_| std::path::Path::new(path).to_path_buf());
        if instance_path == target_path {
          return instance.cdp_port;
        }
      }
    }
    None
  }

  pub async fn find_cloak_by_profile(&self, profile_path: &str) -> Option<CloakLaunchResult> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};

    let mut inner = self.inner.lock().await;
    let target_path = std::path::Path::new(profile_path)
      .canonicalize()
      .unwrap_or_else(|_| std::path::Path::new(profile_path).to_path_buf());

    let mut found_id: Option<String> = None;
    for (id, instance) in &inner.instances {
      if let Some(path) = &instance.profile_path {
        let instance_path = std::path::Path::new(path)
          .canonicalize()
          .unwrap_or_else(|_| std::path::Path::new(path).to_path_buf());
        if instance_path == target_path {
          found_id = Some(id.clone());
          break;
        }
      }
    }

    if let Some(id) = found_id {
      if let Some(instance) = inner.instances.get(&id) {
        if let Some(pid) = instance.process_id {
          let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
          );
          if system.process(sysinfo::Pid::from_u32(pid)).is_some() {
            return Some(CloakLaunchResult {
              id: id.clone(),
              processId: instance.process_id,
              profilePath: instance.profile_path.clone(),
              url: instance.url.clone(),
              cdp_port: instance.cdp_port,
              used_seed: None,
            });
          }
          inner.instances.remove(&id);
          return None;
        }
      }
    }

    // GUI restarted but Cloak still running: recover via system scan.
    if let Some((pid, found_profile_path, cdp_port)) =
      Self::find_cloak_process_by_profile(&target_path)
    {
      let instance_id = format!("recovered_{pid}");
      inner.instances.insert(
        instance_id.clone(),
        CloakInstance {
          process_id: Some(pid),
          profile_path: Some(found_profile_path.clone()),
          url: None,
          cdp_port,
        },
      );
      return Some(CloakLaunchResult {
        id: instance_id,
        processId: Some(pid),
        profilePath: Some(found_profile_path),
        url: None,
        cdp_port,
        used_seed: None,
      });
    }

    None
  }

  /// Scan system processes for a Chromium process using a specific profile path.
  fn find_cloak_process_by_profile(
    target_path: &std::path::Path,
  ) -> Option<(u32, String, Option<u16>)> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};

    let system = System::new_with_specifics(
      RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
    );
    let target_path_str = target_path.to_string_lossy();

    for (pid, process) in system.processes() {
      let cmd = process.cmd();
      if cmd.is_empty() {
        continue;
      }
      let exe_name = process.name().to_string_lossy().to_lowercase();
      if !(exe_name.contains("cloak")
        || exe_name.contains("chromium")
        || exe_name.contains("chrome"))
      {
        continue;
      }
      // Skip child processes (renderer/GPU/utility) — only the browser process
      // lacks a --type= argument.
      if cmd
        .iter()
        .any(|a| a.to_str().is_some_and(|s| s.starts_with("--type=")))
      {
        continue;
      }

      let mut matched = false;
      let mut cdp_port: Option<u16> = None;
      for arg in cmd.iter() {
        if let Some(arg_str) = arg.to_str() {
          if let Some(dir_val) = arg_str.strip_prefix("--user-data-dir=") {
            let cmd_path = std::path::Path::new(dir_val)
              .canonicalize()
              .unwrap_or_else(|_| std::path::Path::new(dir_val).to_path_buf());
            if cmd_path == *target_path {
              matched = true;
            }
          }
          if let Some(port_val) = arg_str.strip_prefix("--remote-debugging-port=") {
            cdp_port = port_val.parse().ok();
          }
        }
      }
      if matched {
        return Some((pid.as_u32(), target_path_str.to_string(), cdp_port));
      }
    }
    None
  }
}

lazy_static::lazy_static! {
  static ref CLOAK_MANAGER: CloakManager = CloakManager::new();
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn build_cloak_args_sets_seed_and_fingerprint_flags() {
    let config = CloakConfig {
      seed: Some(42069),
      os: Some("windows".to_string()),
      timezone: Some("America/New_York".to_string()),
      locale: Some("en-US".to_string()),
      block_webgl: Some(true),
      ..Default::default()
    };
    let args = build_cloak_args(9222, "/tmp/p", &config, 42069, None, false, &[], false);

    assert!(args.contains(&"--fingerprint=42069".to_string()));
    assert!(args.contains(&"--fingerprint-platform=windows".to_string()));
    assert!(args.contains(&"--fingerprint-timezone=America/New_York".to_string()));
    assert!(args.contains(&"--fingerprint-locale=en-US".to_string()));
    assert!(args.contains(&"--lang=en-US".to_string()));
    assert!(args.contains(&"--disable-3d-apis".to_string()));
    assert!(args.contains(&"--remote-debugging-port=9222".to_string()));
    // Screen dims default to the Windows/Linux size when unset.
    assert!(args.contains(&"--fingerprint-screen-width=1920".to_string()));
    assert!(args.contains(&"--fingerprint-screen-height=1080".to_string()));
    // No proxy / extensions / headless requested.
    assert!(!args.iter().any(|a| a.starts_with("--proxy-pac-url=")));
    assert!(!args.contains(&"--headless=new".to_string()));
  }

  #[test]
  fn build_cloak_args_defaults_platform_and_wires_proxy() {
    let config = CloakConfig::default();
    let args = build_cloak_args(
      5500,
      "/tmp/p",
      &config,
      11111,
      Some("http://127.0.0.1:8080"),
      true,
      &["/ext/a".to_string()],
      true,
    );
    assert!(args.contains(&"--fingerprint=11111".to_string()));
    assert!(args
      .iter()
      .any(|a| a.starts_with("--fingerprint-platform=")));
    assert!(args.iter().any(|a| a.starts_with("--proxy-pac-url=")));
    assert!(args.contains(&"--load-extension=/ext/a".to_string()));
    assert!(args.contains(&"--headless=new".to_string()));
    assert!(args.contains(&"--disk-cache-size=1".to_string()));
  }

  #[test]
  fn build_cloak_args_pins_macos_screen_defaults() {
    let config = CloakConfig {
      os: Some("macos".to_string()),
      ..Default::default()
    };
    let args = build_cloak_args(9333, "/tmp/p", &config, 42069, None, false, &[], false);
    assert!(args.contains(&"--fingerprint-screen-width=1440".to_string()));
    assert!(args.contains(&"--fingerprint-screen-height=900".to_string()));
  }
}
