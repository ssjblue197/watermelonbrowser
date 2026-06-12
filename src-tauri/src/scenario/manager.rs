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

use chrono::{Datelike, Timelike};
use serde::Serialize;
use serde_json::Value;

use crate::scenario::actions::McpActionExecutor;
use crate::scenario::ai::AiProviderConfig;
use crate::scenario::dataset::{build_seed, Dataset};
use crate::scenario::executor::{Engine, RunContext};
use crate::scenario::model::{DataBinding, DataMode, Scenario};
use crate::scenario::scheduler::{
  cron_matches, filter_available, is_in_time_window, pick_profiles, ProfileAssignment, Schedule,
  TriggerType,
};
use crate::scenario::store::{RunRecord, ScenarioStore};

/// Khớp biểu thức cron với "bây giờ" trong timezone của lịch (mặc định UTC nếu
/// `tz` rỗng/không hợp lệ). dow: 0-6, Chủ nhật = 0.
fn cron_due_now(expr: &str, tz: Option<&str>) -> bool {
  let utc = chrono::Utc::now();
  let parsed = tz.and_then(|s| s.trim().parse::<chrono_tz::Tz>().ok());
  let (minute, hour, dom, month, dow) = match parsed {
    Some(tz) => {
      let d = utc.with_timezone(&tz);
      (
        d.minute(),
        d.hour(),
        d.day(),
        d.month(),
        d.weekday().num_days_from_sunday(),
      )
    }
    None => (
      utc.minute(),
      utc.hour(),
      utc.day(),
      utc.month(),
      utc.weekday().num_days_from_sunday(),
    ),
  };
  cron_matches(expr, minute, hour, dom, month, dow)
}

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
  /// Con trỏ tuần tự cho dataset, key "{scenario_id}:{dataset_id}" → bộ đếm thô
  /// (lấy `% len` lúc đọc). Lazy-load từ `_cursors.json`, persist sau mỗi lần tăng.
  ds_cursors: Mutex<HashMap<String, u64>>,
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
      ds_cursors: Mutex::new(HashMap::new()),
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
    // api_key rỗng → giữ nguyên key đã lưu (cho phép sửa model mà không nhập lại
    // key). Xoá key dùng clear_ai_config.
    if !api_key.is_empty() {
      crate::settings_manager::SettingsManager::instance()
        .store_ai_api_key(&api_key)
        .map_err(|e| e.to_string())?;
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

  /// Dọn các run mồ côi (status=running) còn sót từ phiên trước. Gọi đúng 1 lần
  /// lúc khởi động app (xem lib.rs setup), trước khi có run mới. Trả số run đã đánh
  /// dấu interrupted.
  pub fn recover_interrupted_runs(&self) -> usize {
    match self.store.recover_interrupted_runs() {
      Ok(n) => n,
      Err(e) => {
        log::warn!("[scenario] recover_interrupted_runs failed: {e}");
        0
      }
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

    // Ghi dấu vết "running" vào SQLite ngay từ đầu → nếu app crash giữa chừng, run
    // sẽ được dọn thành "interrupted" lúc khởi động lại thay vì biến mất.
    if let Err(e) = self.store.begin_run(
      &run_id,
      &scenario.id,
      profile_id,
      triggered_by,
      &started.to_rfc3339(),
    ) {
      log::warn!("[scenario] begin_run failed: {e}");
    }

    let ai_client = self.get_ai_config().map(crate::scenario::ai::make_client);
    let exec = McpActionExecutor;
    let engine = match &ai_client {
      Some(c) => Engine::with_ai(&exec, c.as_ref()),
      None => Engine::new(&exec),
    };
    let mut ctx = RunContext::new(profile_id, scenario.caps.clone(), cancel);

    // Nạp sẵn các dataset mà block pick_row/load_dataset tham chiếu (engine thuần,
    // không gọi singleton).
    for id in collect_dataset_ids(&scenario.blocks) {
      if let Some(ds) = self.get_dataset(&id) {
        ctx.datasets.insert(id, ds);
      }
    }
    // Seed 1 dòng nếu scenario gắn data source.
    if let Some(binding) = &scenario.data_source {
      match self.pick_dataset_row(&scenario.id, binding) {
        Ok(seed) => ctx.seed_variables(seed),
        Err(w) => ctx.warnings.push(w),
      }
    }

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

  // ---------- Dataset persistence + row selection ----------

  fn datasets_dir() -> PathBuf {
    Self::scenarios_dir().join("datasets")
  }
  fn cursors_path() -> PathBuf {
    Self::datasets_dir().join("_cursors.json")
  }

  pub fn list_datasets(&self) -> Vec<Dataset> {
    read_json_dir(&Self::datasets_dir())
  }

  pub fn get_dataset(&self, id: &str) -> Option<Dataset> {
    let raw = std::fs::read_to_string(Self::datasets_dir().join(format!("{id}.json"))).ok()?;
    serde_json::from_str(&raw).ok()
  }

  pub fn save_dataset(&self, d: &Dataset) -> Result<(), String> {
    write_json(&Self::datasets_dir(), &d.id, d)
  }

  pub fn delete_dataset(&self, id: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(Self::datasets_dir().join(format!("{id}.json")));
    // Dọn mọi con trỏ tuần tự liên quan dataset này.
    let mut cur = self.ds_cursors.lock().unwrap();
    cur.retain(|k, _| !k.ends_with(&format!(":{id}")));
    let snapshot = cur.clone();
    drop(cur);
    let _ = std::fs::write(
      Self::cursors_path(),
      serde_json::to_string(&snapshot).unwrap_or_default(),
    );
    Ok(())
  }

  /// Chỉ số tuần tự kế tiếp cho (scenario, dataset). Đọc bộ đếm thô, trả `% len`,
  /// tăng + persist. `% len` lúc đọc giữ trong khoảng khi dataset đổi kích thước.
  fn next_seq_index(&self, scenario_id: &str, dataset_id: &str, len: usize) -> usize {
    if len == 0 {
      return 0;
    }
    let key = format!("{scenario_id}:{dataset_id}");
    let mut cur = self.ds_cursors.lock().unwrap();
    // Lazy-load 1 lần: nếu map rỗng thử nạp từ đĩa.
    if cur.is_empty() {
      if let Ok(raw) = std::fs::read_to_string(Self::cursors_path()) {
        if let Ok(loaded) = serde_json::from_str::<HashMap<String, u64>>(&raw) {
          *cur = loaded;
        }
      }
    }
    let counter = cur.entry(key).or_insert(0);
    let idx = (*counter % len as u64) as usize;
    *counter = counter.wrapping_add(1);
    let snapshot = cur.clone();
    drop(cur);
    if let Err(e) = std::fs::write(
      Self::cursors_path(),
      serde_json::to_string(&snapshot).unwrap_or_default(),
    ) {
      log::warn!("[scenario][data] persist cursors failed: {e}");
    }
    idx
  }

  /// Chọn 1 dòng dataset theo binding → tập biến seed. Err nếu dataset thiếu/rỗng.
  fn pick_dataset_row(
    &self,
    scenario_id: &str,
    b: &DataBinding,
  ) -> Result<HashMap<String, Value>, String> {
    let ds = self
      .get_dataset(&b.dataset_id)
      .ok_or_else(|| format!("[data] dataset not found: {}", b.dataset_id))?;
    if ds.rows.is_empty() {
      return Err(format!("[data] dataset '{}' is empty", ds.name));
    }
    let idx = match b.mode {
      DataMode::Random => (rand::random::<u64>() % ds.rows.len() as u64) as usize,
      DataMode::Sequential => self.next_seq_index(scenario_id, &b.dataset_id, ds.rows.len()),
    };
    Ok(build_seed(b, &ds.rows[idx]))
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

  pub fn get_schedule(&self, schedule_id: &str) -> Option<Schedule> {
    let raw =
      std::fs::read_to_string(Self::schedules_dir().join(format!("{schedule_id}.json"))).ok()?;
    serde_json::from_str(&raw).ok()
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
  /// theo rotation rồi chạy scenario. Profile chưa chạy sẽ được TỰ KHỞI ĐỘNG (headless
  /// theo `assignment.run_headless`), chạy kịch bản, rồi đóng lại. Gọi định kỳ từ lib.rs.
  pub fn scheduler_tick(&self, app_handle: tauri::AppHandle) {
    let now = chrono::Local::now();
    let now_min = now.hour() * 60 + now.minute();
    let today = now.format("%Y-%m-%d").to_string();
    let now_epoch = chrono::Utc::now().timestamp().max(0) as u64;

    for sch in self.list_schedules().into_iter().filter(|s| s.enabled) {
      // Manual/OnEvent: scheduler không tự kích hoạt.
      let interval_secs: u64 = match sch.trigger_type {
        TriggerType::Interval => match sch.interval_minutes {
          Some(m) if m > 0 => m * 60,
          _ => continue,
        },
        TriggerType::Cron => 0, // dùng cron_matches thay cho interval
        TriggerType::Manual | TriggerType::OnEvent => continue,
      };
      // Cron: khớp thời điểm hiện tại theo timezone của lịch.
      let cron_due = match sch.trigger_type {
        TriggerType::Cron => match sch.cron_expr.as_deref() {
          Some(expr) if !expr.trim().is_empty() => cron_due_now(expr, sch.timezone.as_deref()),
          _ => continue,
        },
        _ => false,
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
        match sch.trigger_type {
          TriggerType::Cron => {
            // Khớp phút hiện tại + chưa chạy trong phút này (tick 60s).
            if !cron_due || state.last_run_epoch / 60 == now_epoch / 60 {
              continue;
            }
          }
          _ => {
            if state.last_run_epoch != 0 && now_epoch < state.last_run_epoch + interval_secs {
              continue;
            }
          }
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

      // Một lịch có thể gắn nhiều kịch bản; nạp toàn bộ (bỏ qua id không tồn tại).
      let scenarios: Vec<crate::scenario::model::Scenario> =
        crate::scenario::scheduler::effective_scenario_ids(&sch)
          .into_iter()
          .filter_map(|id| self.store.load_scenario(&id))
          .collect();
      if scenarios.is_empty() {
        log::warn!(
          "[scenario][schedule] no valid scenarios for schedule {}",
          sch.id
        );
        continue;
      }

      self.launch_run_close_profiles(
        picked,
        scenarios,
        asg.run_headless,
        app_handle.clone(),
        "schedule",
      );
    }
  }

  /// Chạy ngay một lịch theo assignment + rotation, bỏ qua các cổng thời gian
  /// (interval/cron/time-window/cooldown/max-per-day) — dùng cho nút "Run now"
  /// và lịch Manual. Trả về số profile được chọn để chạy. Profile chưa chạy sẽ
  /// tự khởi động rồi đóng lại (giống scheduler), profile đang chạy giữ nguyên.
  pub fn run_schedule_now(
    &self,
    schedule_id: &str,
    app_handle: tauri::AppHandle,
  ) -> Result<usize, String> {
    let sch = self
      .get_schedule(schedule_id)
      .ok_or_else(|| format!("Schedule not found: {schedule_id}"))?;
    let asg = self
      .get_assignment(&sch.id)
      .ok_or_else(|| "No profiles assigned to this schedule".to_string())?;

    let busy = self.busy_profiles();
    let candidates = self.expand_profiles(&asg);
    // Manual run: chỉ né profile đang bận (đang có run khác), KHÔNG áp cooldown.
    let avail = filter_available(&candidates, &busy, &HashSet::new());

    let now_epoch = chrono::Utc::now().timestamp().max(0) as u64;
    let picked: Vec<String> = {
      let mut st = self.sched_state.lock().unwrap();
      let state = st.entry(sch.id.clone()).or_default();
      let mut asg_rot = asg.clone();
      asg_rot.last_used_profile_id = state.last_used_profile_id.clone();
      let picked = pick_profiles(&avail, &asg_rot, &HashMap::new());
      for pid in &picked {
        state.profile_last_used.insert(pid.clone(), now_epoch);
        state.last_used_profile_id = Some(pid.clone());
      }
      picked
    };
    if picked.is_empty() {
      return Err("No available profile (all assigned profiles are busy)".to_string());
    }

    let scenarios: Vec<crate::scenario::model::Scenario> =
      crate::scenario::scheduler::effective_scenario_ids(&sch)
        .into_iter()
        .filter_map(|id| self.store.load_scenario(&id))
        .collect();
    if scenarios.is_empty() {
      return Err("Schedule has no valid scenarios".to_string());
    }

    let count = picked.len();
    self.launch_run_close_profiles(picked, scenarios, asg.run_headless, app_handle, "manual");
    Ok(count)
  }

  /// Trên mỗi profile đã chọn: tự khởi động nếu chưa chạy, chạy lần lượt tất cả
  /// kịch bản (thứ tự xáo trộn riêng cho từng profile), rồi đóng các profile do
  /// CHÍNH hàm này mở. Dùng chung bởi scheduler tick và "Run now".
  fn launch_run_close_profiles(
    &self,
    picked: Vec<String>,
    scenarios: Vec<crate::scenario::model::Scenario>,
    headless: bool,
    app_handle: tauri::AppHandle,
    triggered_by: &'static str,
  ) {
    for pid in picked {
      let mut list = scenarios.clone();
      crate::scenario::scheduler::shuffle_in_place(&mut list);
      // Profile đã chạy sẵn (user mở) thì KHÔNG đóng sau khi xong; profile do hàm
      // này tự mở thì đóng lại để không tích tụ cửa sổ ẩn.
      let already_running = self.profile_is_running(&pid);
      let app = app_handle.clone();
      tauri::async_runtime::spawn(async move {
        let mut launched = None;
        if !already_running {
          let profile = crate::profile::ProfileManager::instance()
            .list_profiles()
            .ok()
            .and_then(|ps| ps.into_iter().find(|p| p.id.to_string() == pid));
          let Some(profile) = profile else {
            log::warn!("[scenario][run] profile {pid} not found for auto-launch");
            return;
          };
          if profile.browser != "wayfern"
            && profile.browser != "camoufox"
            && profile.browser != "cloak"
          {
            log::warn!(
              "[scenario][run] profile {pid} browser '{}' không hỗ trợ automation",
              profile.browser
            );
            return;
          }
          // force_new=true → bản chạy mới có remote-debugging cho MCP; headless theo cờ.
          match crate::browser_runner::launch_browser_profile_impl(
            app.clone(),
            profile.clone(),
            None,
            None,
            headless,
            true,
          )
          .await
          {
            Ok(updated) => {
              log::info!("[scenario][run] auto-launched profile {pid} (headless={headless})");
              launched = Some(updated);
              // Cho browser ít giây để bật cổng automation trước khi chạy action.
              tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
            Err(e) => {
              log::error!("[scenario][run] auto-launch {pid} failed: {e}");
              return;
            }
          }
        }

        for scn in list {
          log::info!("[scenario][run] running '{}' on profile {pid}", scn.name);
          let _ = ScenarioManager::instance()
            .run_and_record(&pid, scn, triggered_by)
            .await;
        }

        // Chỉ đóng profile do CHÍNH hàm này mở.
        if let Some(profile) = launched {
          match crate::browser_runner::kill_browser_profile(app.clone(), profile).await {
            Ok(_) => log::info!("[scenario][run] stopped auto-launched profile {pid}"),
            Err(e) => {
              log::error!("[scenario][run] failed to stop auto-launched {pid}: {e}")
            }
          }
        }
      });
    }
  }
}

/// Thu thập mọi `dataset_id` literal mà block pick_row/load_dataset tham chiếu
/// (đệ quy children + branch_else), khử trùng lặp — để nạp sẵn vào RunContext.
fn collect_dataset_ids(blocks: &[crate::scenario::model::Block]) -> Vec<String> {
  fn walk(blocks: &[crate::scenario::model::Block], out: &mut Vec<String>) {
    for b in blocks {
      if matches!(b.block_type.as_str(), "pick_row" | "load_dataset") {
        if let Some(id) = b.params.get("dataset_id").and_then(|v| v.as_str()) {
          if !id.is_empty() && !out.iter().any(|x| x == id) {
            out.push(id.to_string());
          }
        }
      }
      walk(&b.children, out);
      if let Some(eb) = &b.branch_else {
        walk(eb, out);
      }
    }
  }
  let mut out = Vec::new();
  walk(blocks, &mut out);
  out
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
