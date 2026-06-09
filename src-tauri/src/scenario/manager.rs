//! ScenarioManager: singleton điều phối scenario automation cho toàn app.
//!
//! Gom 4 thứ vào một nơi để MCP, Tauri command và scheduler tick dùng chung:
//!  - store (CRUD scenario + lịch sử run trong SQLite),
//!  - registry các run đang chạy (cancellation hợp tác + tập profile "busy"),
//!  - cấu hình AI provider (phần non-secret → JSON; api_key → file mã hoá),
//!  - lưu Schedule/ProfileAssignment + state nền cho tick-loop.
//!
//! `run_and_record` là core dùng chung: dựng Engine (kèm AI client nếu có), chạy,
//! ghi lịch sử, và đăng ký/huỷ đăng ký cancel-flag.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Timelike;
use serde::Serialize;
use serde_json::Value;

use crate::scenario::actions::McpActionExecutor;
use crate::scenario::ai::AiProviderConfig;
use crate::scenario::executor::{Engine, RunContext};
use crate::scenario::model::Scenario;
use crate::scenario::scheduler::{
  filter_available, is_in_time_window, pick_profiles, ProfileAssignment, Schedule, TriggerType,
};
use crate::scenario::store::{RunRecord, ScenarioStore};

/// Một run đang chạy: giữ cancel-flag để huỷ và meta để hiển thị.
struct ActiveRun {
  scenario_id: String,
  profile_id: String,
  started_at: String,
  cancel: Arc<AtomicBool>,
}

/// View serializable của một run đang chạy (cho command/UI).
#[derive(Debug, Serialize)]
pub struct RunInfo {
  pub run_id: String,
  pub scenario_id: String,
  pub profile_id: String,
  pub started_at: String,
}

/// State nền cho mỗi schedule (ephemeral — reset khi khởi động lại app).
#[derive(Default)]
struct ScheduleState {
  last_run_epoch: u64,
  day: String, // YYYY-MM-DD ứng với runs_today
  runs_today: u32,
  last_used_profile_id: Option<String>,
  profile_last_used: HashMap<String, u64>, // epoch theo profile, cho cooldown
}

pub struct ScenarioManager {
  store: ScenarioStore,
  running: Mutex<HashMap<String, ActiveRun>>,
  sched_state: Mutex<HashMap<String, ScheduleState>>,
}

lazy_static::lazy_static! {
  static ref SCENARIO_MANAGER: ScenarioManager = ScenarioManager::new();
}

impl ScenarioManager {
  fn new() -> Self {
    Self {
      store: ScenarioStore::default_location(),
      running: Mutex::new(HashMap::new()),
      sched_state: Mutex::new(HashMap::new()),
    }
  }

  pub fn instance() -> &'static ScenarioManager {
    &SCENARIO_MANAGER
  }

  pub fn store(&self) -> &ScenarioStore {
    &self.store
  }

  fn scenarios_dir() -> PathBuf {
    crate::app_dirs::data_dir().join("scenarios")
  }

  // ---------- AI provider config ----------

  fn ai_config_path() -> PathBuf {
    Self::scenarios_dir().join("ai_provider.json")
  }

  /// Cấu hình AI hiện tại (api_key nạp từ file mã hoá). None nếu chưa cấu hình.
  pub fn get_ai_config(&self) -> Option<AiProviderConfig> {
    let raw = std::fs::read_to_string(Self::ai_config_path()).ok()?;
    let mut cfg: AiProviderConfig = serde_json::from_str(&raw).ok()?;
    if let Some(key) = crate::settings_manager::SettingsManager::instance().get_ai_api_key() {
      cfg.api_key = key;
    }
    Some(cfg)
  }

  /// Lưu cấu hình AI: phần non-secret ra JSON (api_key để rỗng), key ra file mã hoá.
  pub fn set_ai_config(&self, cfg: &AiProviderConfig) -> Result<(), String> {
    let dir = Self::scenarios_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create scenarios dir: {e}"))?;
    let mut to_store = cfg.clone();
    let api_key = std::mem::take(&mut to_store.api_key);
    let json = serde_json::to_string_pretty(&to_store).map_err(|e| e.to_string())?;
    std::fs::write(Self::ai_config_path(), json).map_err(|e| format!("write ai config: {e}"))?;
    let sm = crate::settings_manager::SettingsManager::instance();
    if api_key.is_empty() {
      sm.remove_ai_api_key().map_err(|e| e.to_string())?;
    } else {
      sm.store_ai_api_key(&api_key).map_err(|e| e.to_string())?;
    }
    Ok(())
  }

  pub fn clear_ai_config(&self) -> Result<(), String> {
    let _ = std::fs::remove_file(Self::ai_config_path());
    crate::settings_manager::SettingsManager::instance()
      .remove_ai_api_key()
      .map_err(|e| e.to_string())
  }

  // ---------- Run registry ----------

  pub fn active_runs(&self) -> Vec<RunInfo> {
    let guard = self.running.lock().unwrap();
    guard
      .iter()
      .map(|(run_id, r)| RunInfo {
        run_id: run_id.clone(),
        scenario_id: r.scenario_id.clone(),
        profile_id: r.profile_id.clone(),
        started_at: r.started_at.clone(),
      })
      .collect()
  }

  /// Huỷ một run đang chạy (set cancel-flag). Trả false nếu không tìm thấy.
  pub fn cancel_run(&self, run_id: &str) -> bool {
    let guard = self.running.lock().unwrap();
    match guard.get(run_id) {
      Some(r) => {
        r.cancel.store(true, Ordering::SeqCst);
        true
      }
      None => false,
    }
  }

  /// Tập profile đang có run chạy (để scheduler không trùng).
  pub fn busy_profiles(&self) -> HashSet<String> {
    self
      .running
      .lock()
      .unwrap()
      .values()
      .map(|r| r.profile_id.clone())
      .collect()
  }

  /// Core dùng chung: dựng engine (kèm AI nếu đã cấu hình), chạy scenario trên
  /// `profile_id`, ghi lịch sử, trả summary JSON. Caller phải đảm bảo profile đang
  /// chạy (các action sẽ fail từng bước nếu không).
  pub async fn run_and_record(
    &self,
    profile_id: &str,
    scenario: Scenario,
    triggered_by: &str,
  ) -> Value {
    let run_id = uuid::Uuid::new_v4().to_string();
    let started = chrono::Utc::now();
    let cancel = Arc::new(AtomicBool::new(false));

    self.running.lock().unwrap().insert(
      run_id.clone(),
      ActiveRun {
        scenario_id: scenario.id.clone(),
        profile_id: profile_id.to_string(),
        started_at: started.to_rfc3339(),
        cancel: cancel.clone(),
      },
    );

    let ai_client = self.get_ai_config().map(crate::scenario::ai::make_client);
    let exec = McpActionExecutor;
    let engine = match &ai_client {
      Some(c) => Engine::with_ai(&exec, c.as_ref()),
      None => Engine::new(&exec),
    };
    let ctx = RunContext::new(profile_id, scenario.caps.clone(), cancel);
    let ctx = engine.run(&scenario, ctx).await;
    let finished = chrono::Utc::now();

    self.running.lock().unwrap().remove(&run_id);

    let any_failed = ctx.step_logs.iter().any(|s| s.status == "failed");
    let stopped = ctx
      .warnings
      .iter()
      .any(|w| w.contains("stopped") || w.contains("cap"));
    let status = if any_failed {
      "failed"
    } else if stopped {
      "stopped"
    } else {
      "success"
    };

    let record = RunRecord {
      id: run_id.clone(),
      scenario_id: scenario.id.clone(),
      profile_id: profile_id.to_string(),
      triggered_by: triggered_by.to_string(),
      status: status.to_string(),
      started_at: started.to_rfc3339(),
      finished_at: finished.to_rfc3339(),
      duration_ms: (finished - started).num_milliseconds().max(0) as u128,
      error: None,
      warnings: ctx.warnings.clone(),
      variables: ctx.variables.clone(),
      steps: ctx.step_logs.clone(),
    };
    if let Err(e) = self.store.record_run(&record) {
      log::warn!("[scenario] record_run failed: {e}");
    }

    serde_json::json!({
      "run_id": run_id,
      "scenario_id": scenario.id,
      "status": status,
      "steps": serde_json::to_value(&ctx.step_logs).unwrap_or_default(),
      "variables": serde_json::to_value(&ctx.variables).unwrap_or_default(),
      "warnings": ctx.warnings,
    })
  }

  // ---------- Schedule / assignment persistence (JSON) ----------

  fn schedules_dir() -> PathBuf {
    Self::scenarios_dir().join("schedules")
  }
  fn assignments_dir() -> PathBuf {
    Self::scenarios_dir().join("assignments")
  }

  pub fn list_schedules(&self) -> Vec<Schedule> {
    read_json_dir(&Self::schedules_dir())
  }

  pub fn save_schedule(&self, s: &Schedule) -> Result<(), String> {
    write_json(&Self::schedules_dir(), &s.id, s)
  }

  pub fn delete_schedule(&self, id: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(Self::schedules_dir().join(format!("{id}.json")));
    let _ = std::fs::remove_file(Self::assignments_dir().join(format!("{id}.json")));
    self.sched_state.lock().unwrap().remove(id);
    Ok(())
  }

  pub fn get_assignment(&self, schedule_id: &str) -> Option<ProfileAssignment> {
    let path = Self::assignments_dir().join(format!("{schedule_id}.json"));
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
  }

  pub fn save_assignment(&self, a: &ProfileAssignment) -> Result<(), String> {
    write_json(&Self::assignments_dir(), &a.schedule_id, a)
  }

  /// Mở rộng assignment → danh sách profile_id (gồm cả các profile thuộc group_ids).
  fn expand_profiles(&self, asg: &ProfileAssignment) -> Vec<String> {
    let mut out: Vec<String> = asg.profile_ids.clone();
    if !asg.group_ids.is_empty() {
      if let Ok(profiles) = crate::profile::ProfileManager::instance().list_profiles() {
        for p in profiles {
          if let Some(gid) = &p.group_id {
            if asg.group_ids.contains(gid) {
              out.push(p.id.to_string());
            }
          }
        }
      }
    }
    // dedup giữ thứ tự
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
  }

  fn profile_is_running(&self, profile_id: &str) -> bool {
    crate::profile::ProfileManager::instance()
      .list_profiles()
      .map(|ps| {
        ps.iter()
          .any(|p| p.id.to_string() == profile_id && p.process_id.is_some())
      })
      .unwrap_or(false)
  }

  /// Một nhịp scheduler: với mỗi schedule Interval đang bật và đến hạn, chọn profile
  /// theo rotation rồi chạy scenario trên các profile ĐANG chạy. Profile chưa chạy
  /// được bỏ qua (auto-launch là bước tích hợp sau). Gọi định kỳ từ lib.rs.
  pub fn scheduler_tick(&self) {
    let now = chrono::Local::now();
    let now_min = now.hour() * 60 + now.minute();
    let today = now.format("%Y-%m-%d").to_string();
    let now_epoch = chrono::Utc::now().timestamp().max(0) as u64;

    for sch in self.list_schedules().into_iter().filter(|s| s.enabled) {
      if sch.trigger_type != TriggerType::Interval {
        continue; // Cron/Manual/OnEvent chưa wire ở phase này
      }
      let interval = match sch.interval_minutes {
        Some(m) if m > 0 => m,
        _ => continue,
      };
      if !is_in_time_window(
        now_min,
        sch.time_window_start.as_deref(),
        sch.time_window_end.as_deref(),
      ) {
        continue;
      }

      let Some(asg) = self.get_assignment(&sch.id) else {
        continue;
      };
      let busy = self.busy_profiles();
      let candidates = self.expand_profiles(&asg);

      // Tính toán + cập nhật state dưới lock (không await trong vùng này).
      let picked: Vec<String> = {
        let mut st = self.sched_state.lock().unwrap();
        let state = st.entry(sch.id.clone()).or_default();
        if state.day != today {
          state.day = today.clone();
          state.runs_today = 0;
        }
        if state.last_run_epoch != 0 && now_epoch < state.last_run_epoch + interval * 60 {
          continue;
        }
        if let Some(max) = sch.max_runs_per_day {
          if state.runs_today >= max {
            continue;
          }
        }
        let cooldown_secs = asg.cooldown_minutes * 60;
        let in_cd: HashSet<String> = state
          .profile_last_used
          .iter()
          .filter(|(_, &t)| cooldown_secs > 0 && now_epoch < t + cooldown_secs)
          .map(|(p, _)| p.clone())
          .collect();
        let avail = filter_available(&candidates, &busy, &in_cd);
        let mut asg_rot = asg.clone();
        asg_rot.last_used_profile_id = state.last_used_profile_id.clone();
        let picked = pick_profiles(&avail, &asg_rot, &HashMap::new());
        if picked.is_empty() {
          continue;
        }
        state.last_run_epoch = now_epoch;
        state.runs_today += 1;
        for pid in &picked {
          state.profile_last_used.insert(pid.clone(), now_epoch);
          state.last_used_profile_id = Some(pid.clone());
        }
        picked
      };

      let Some(scenario) = self.store.load_scenario(&sch.scenario_id) else {
        log::warn!(
          "[scenario][schedule] scenario {} not found for schedule {}",
          sch.scenario_id,
          sch.id
        );
        continue;
      };

      for pid in picked {
        if !self.profile_is_running(&pid) {
          log::info!(
            "[scenario][schedule] profile {pid} not running — skipping (auto-launch not yet wired)"
          );
          continue;
        }
        let scn = scenario.clone();
        tauri::async_runtime::spawn(async move {
          log::info!(
            "[scenario][schedule] running '{}' on profile {pid}",
            scn.name
          );
          let _ = ScenarioManager::instance()
            .run_and_record(&pid, scn, "schedule")
            .await;
        });
      }
    }
  }
}

fn read_json_dir<T: serde::de::DeserializeOwned>(dir: &std::path::Path) -> Vec<T> {
  let mut out = Vec::new();
  let Ok(entries) = std::fs::read_dir(dir) else {
    return out;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.extension().and_then(|e| e.to_str()) == Some("json") {
      if let Ok(c) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<T>(&c) {
          out.push(v);
        }
      }
    }
  }
  out
}

fn write_json<T: Serialize>(dir: &std::path::Path, id: &str, v: &T) -> Result<(), String> {
  std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
  let json = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
  let path = dir.join(format!("{id}.json"));
  let tmp = path.with_extension("tmp");
  std::fs::write(&tmp, json).map_err(|e| format!("write: {e}"))?;
  std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
}
