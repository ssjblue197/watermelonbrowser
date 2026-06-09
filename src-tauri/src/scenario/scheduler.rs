//! Scheduler core: model Schedule/ProfileAssignment + logic chọn profile (rotation),
//! kiểm tra khung giờ và xung đột. Đây là phần THUẦN, test được không cần runtime;
//! tick-loop nền (gọi run_scenario theo lịch) wiring vào lib.rs là bước tích hợp.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
  Cron,
  Interval,
  Manual,
  OnEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
  pub id: String,
  pub scenario_id: String,
  pub name: String,
  pub enabled: bool,
  pub trigger_type: TriggerType,
  #[serde(default)]
  pub cron_expr: Option<String>,
  #[serde(default)]
  pub interval_minutes: Option<u64>,
  #[serde(default)]
  pub timezone: Option<String>,
  /// "HH:MM"
  #[serde(default)]
  pub time_window_start: Option<String>,
  #[serde(default)]
  pub time_window_end: Option<String>,
  #[serde(default)]
  pub max_runs_per_day: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RotationMode {
  RoundRobin,
  Random,
  LeastUsed,
  AllParallel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileAssignment {
  pub schedule_id: String,
  #[serde(default)]
  pub profile_ids: Vec<String>,
  #[serde(default)]
  pub group_ids: Vec<String>,
  pub rotation_mode: RotationMode,
  #[serde(default = "default_max_parallel")]
  pub max_parallel: usize,
  #[serde(default)]
  pub cooldown_minutes: u64,
  #[serde(default)]
  pub last_used_profile_id: Option<String>,
  #[serde(default)]
  pub run_headless: bool,
}

fn default_max_parallel() -> usize {
  1
}

/// Lọc profile khả dụng: bỏ profile đang chạy (busy) và đang trong cooldown.
pub fn filter_available(
  candidates: &[String],
  busy: &HashSet<String>,
  in_cooldown: &HashSet<String>,
) -> Vec<String> {
  candidates
    .iter()
    .filter(|p| !busy.contains(*p) && !in_cooldown.contains(*p))
    .cloned()
    .collect()
}

/// Chọn profile theo rotation. `usage_counts` chỉ dùng cho LeastUsed.
/// Trả tối đa `max_parallel` profile (1 cho các mode tuần tự).
pub fn pick_profiles(
  available: &[String],
  assignment: &ProfileAssignment,
  usage_counts: &HashMap<String, u32>,
) -> Vec<String> {
  if available.is_empty() {
    return Vec::new();
  }
  match assignment.rotation_mode {
    RotationMode::AllParallel => available
      .iter()
      .take(assignment.max_parallel.max(1))
      .cloned()
      .collect(),
    RotationMode::Random => {
      let idx = (rand::random::<u64>() % available.len() as u64) as usize;
      vec![available[idx].clone()]
    }
    RotationMode::LeastUsed => {
      let mut best = &available[0];
      let mut best_n = usage_counts.get(best).copied().unwrap_or(0);
      for p in &available[1..] {
        let n = usage_counts.get(p).copied().unwrap_or(0);
        if n < best_n {
          best = p;
          best_n = n;
        }
      }
      vec![best.clone()]
    }
    RotationMode::RoundRobin => {
      // Lấy profile NGAY SAU last_used trong danh sách available (wrap).
      let start = match &assignment.last_used_profile_id {
        Some(last) => available
          .iter()
          .position(|p| p == last)
          .map(|i| i + 1)
          .unwrap_or(0),
        None => 0,
      };
      vec![available[start % available.len()].clone()]
    }
  }
}

/// "HH:MM" → phút trong ngày.
fn parse_hm(s: &str) -> Option<u32> {
  let (h, m) = s.split_once(':')?;
  let h: u32 = h.trim().parse().ok()?;
  let m: u32 = m.trim().parse().ok()?;
  if h > 23 || m > 59 {
    return None;
  }
  Some(h * 60 + m)
}

/// `now_minutes` = phút trong ngày (0..1440). Hỗ trợ khung qua nửa đêm (start>end).
pub fn is_in_time_window(now_minutes: u32, start: Option<&str>, end: Option<&str>) -> bool {
  let (s, e) = match (start, end) {
    (Some(s), Some(e)) => (parse_hm(s), parse_hm(e)),
    _ => return true, // không cấu hình → luôn cho phép
  };
  match (s, e) {
    (Some(s), Some(e)) => {
      if s <= e {
        now_minutes >= s && now_minutes <= e
      } else {
        // qua nửa đêm
        now_minutes >= s || now_minutes <= e
      }
    }
    _ => true,
  }
}

/// next_run cho trigger Interval (epoch giây). Cron để bước tích hợp sau.
pub fn next_interval_run(now_epoch_secs: u64, interval_minutes: u64) -> u64 {
  now_epoch_secs + interval_minutes.max(1) * 60
}

#[cfg(test)]
mod tests {
  use super::*;

  fn assignment(mode: RotationMode, last: Option<&str>) -> ProfileAssignment {
    ProfileAssignment {
      schedule_id: "s".into(),
      profile_ids: vec![],
      group_ids: vec![],
      rotation_mode: mode,
      max_parallel: 2,
      cooldown_minutes: 0,
      last_used_profile_id: last.map(|s| s.to_string()),
      run_headless: false,
    }
  }

  #[test]
  fn round_robin_advances_and_wraps() {
    let avail = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let counts = HashMap::new();
    assert_eq!(
      pick_profiles(
        &avail,
        &assignment(RotationMode::RoundRobin, Some("a")),
        &counts
      ),
      vec!["b".to_string()]
    );
    assert_eq!(
      pick_profiles(
        &avail,
        &assignment(RotationMode::RoundRobin, Some("c")),
        &counts
      ),
      vec!["a".to_string()] // wrap
    );
    assert_eq!(
      pick_profiles(&avail, &assignment(RotationMode::RoundRobin, None), &counts),
      vec!["a".to_string()]
    );
  }

  #[test]
  fn least_used_picks_min() {
    let avail = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let counts = HashMap::from([
      ("a".to_string(), 5u32),
      ("b".to_string(), 1),
      ("c".to_string(), 9),
    ]);
    assert_eq!(
      pick_profiles(&avail, &assignment(RotationMode::LeastUsed, None), &counts),
      vec!["b".to_string()]
    );
  }

  #[test]
  fn all_parallel_caps_at_max() {
    let avail = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let got = pick_profiles(
      &avail,
      &assignment(RotationMode::AllParallel, None),
      &HashMap::new(),
    );
    assert_eq!(got, vec!["a".to_string(), "b".to_string()]); // max_parallel=2
  }

  #[test]
  fn filter_removes_busy_and_cooldown() {
    let cand = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let busy = HashSet::from(["a".to_string()]);
    let cd = HashSet::from(["c".to_string()]);
    assert_eq!(filter_available(&cand, &busy, &cd), vec!["b".to_string()]);
  }

  #[test]
  fn time_window_same_day_and_overnight() {
    // 09:00–17:00
    assert!(is_in_time_window(10 * 60, Some("09:00"), Some("17:00")));
    assert!(!is_in_time_window(8 * 60, Some("09:00"), Some("17:00")));
    // qua nửa đêm 22:00–06:00
    assert!(is_in_time_window(23 * 60, Some("22:00"), Some("06:00")));
    assert!(is_in_time_window(2 * 60, Some("22:00"), Some("06:00")));
    assert!(!is_in_time_window(12 * 60, Some("22:00"), Some("06:00")));
    // không cấu hình
    assert!(is_in_time_window(12 * 60, None, None));
  }
}
