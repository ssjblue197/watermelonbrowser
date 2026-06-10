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
  /// Kịch bản đơn (legacy). Giữ để tương thích lịch cũ; ưu tiên `scenario_ids`.
  #[serde(default)]
  pub scenario_id: String,
  /// Danh sách kịch bản chạy mỗi lần đến hạn. Nếu rỗng → dùng `scenario_id`.
  #[serde(default)]
  pub scenario_ids: Vec<String>,
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

/// Một phần (cách nhau dấu phẩy) của một trường cron khớp `val`.
/// Hỗ trợ `*`, `a`, `a-b`, `*/n`, `a-b/n`, `a/n` (từ a, bước n đến max).
fn cron_part_matches(part: &str, val: u32, min: u32, max: u32) -> bool {
  let (range_part, step) = match part.split_once('/') {
    Some((r, s)) => match s.parse::<u32>() {
      Ok(n) if n > 0 => (r, n),
      _ => return false,
    },
    None => (part, 1),
  };
  let (lo, hi) = if range_part == "*" {
    (min, max)
  } else if let Some((a, b)) = range_part.split_once('-') {
    match (a.parse::<u32>(), b.parse::<u32>()) {
      (Ok(a), Ok(b)) => (a, b),
      _ => return false,
    }
  } else {
    match range_part.parse::<u32>() {
      // số đơn không bước → khớp đúng giá trị; có bước → từ n tới max
      Ok(n) if step == 1 => return val == n,
      Ok(n) => (n, max),
      Err(_) => return false,
    }
  };
  val >= lo && val <= hi && (val - lo).is_multiple_of(step)
}

/// Một trường cron (có thể nhiều phần `a,b,c`) khớp `val` trong [min,max].
fn cron_field_matches(field: &str, val: u32, min: u32, max: u32) -> bool {
  field
    .split(',')
    .any(|part| cron_part_matches(part, val, min, max))
}

/// Khớp biểu thức cron 5 trường `min hour dom month dow` với thời điểm cho trước
/// (dow: 0-6, Chủ nhật = 0). Quy tắc chuẩn: nếu cả dom lẫn dow đều khác `*` thì
/// khớp khi MỘT trong hai khớp (OR); ngược lại AND như các trường khác.
pub fn cron_matches(
  expr: &str,
  minute: u32,
  hour: u32,
  dom: u32,
  month: u32,
  dow: u32,
) -> bool {
  let f: Vec<&str> = expr.split_whitespace().collect();
  if f.len() != 5 {
    return false;
  }
  let dom_restricted = f[2] != "*";
  let dow_restricted = f[4] != "*";
  let dom_ok = cron_field_matches(f[2], dom, 1, 31);
  let dow_ok = cron_field_matches(f[4], dow, 0, 6);
  let day_ok = if dom_restricted && dow_restricted {
    dom_ok || dow_ok
  } else {
    dom_ok && dow_ok
  };
  cron_field_matches(f[0], minute, 0, 59)
    && cron_field_matches(f[1], hour, 0, 23)
    && cron_field_matches(f[3], month, 1, 12)
    && day_ok
}

/// Danh sách kịch bản hiệu lực của một lịch: `scenario_ids` nếu có, ngược lại
/// suy ra từ `scenario_id` (lịch cũ một-kịch-bản).
pub fn effective_scenario_ids(sch: &Schedule) -> Vec<String> {
  if !sch.scenario_ids.is_empty() {
    sch.scenario_ids.clone()
  } else if sch.scenario_id.is_empty() {
    Vec::new()
  } else {
    vec![sch.scenario_id.clone()]
  }
}

/// Trộn ngẫu nhiên tại chỗ (Fisher–Yates, dùng `rand`). Mỗi profile gọi riêng để
/// có một trình tự kịch bản khác nhau.
pub fn shuffle_in_place<T>(items: &mut [T]) {
  let n = items.len();
  if n < 2 {
    return;
  }
  for i in (1..n).rev() {
    let j = (rand::random::<u64>() % (i as u64 + 1)) as usize;
    items.swap(i, j);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cron_every_6_hours() {
    // "0 */6 * * *" → phút 0, giờ 0/6/12/18.
    let e = "0 */6 * * *";
    assert!(cron_matches(e, 0, 0, 15, 6, 3));
    assert!(cron_matches(e, 0, 12, 1, 1, 0));
    assert!(!cron_matches(e, 0, 5, 1, 1, 0)); // giờ 5 không khớp
    assert!(!cron_matches(e, 30, 6, 1, 1, 0)); // phút 30 không khớp
  }

  #[test]
  fn cron_weekday_morning() {
    // "30 9 * * 1-5" → 9:30 Thứ 2–6 (dow 1..=5).
    let e = "30 9 * * 1-5";
    assert!(cron_matches(e, 30, 9, 10, 3, 1)); // Thứ 2
    assert!(cron_matches(e, 30, 9, 10, 3, 5)); // Thứ 6
    assert!(!cron_matches(e, 30, 9, 10, 3, 0)); // Chủ nhật
    assert!(!cron_matches(e, 31, 9, 10, 3, 1)); // phút 31
  }

  #[test]
  fn cron_lists_and_dom_dow_or() {
    assert!(cron_matches("0,30 * * * *", 30, 14, 1, 1, 2));
    assert!(cron_matches("0 0 1,15 * *", 0, 0, 15, 8, 4));
    // dom=13 và dow=Thứ 2(1) đều bị giới hạn → OR: khớp nếu là ngày 13 HOẶC Thứ 2.
    assert!(cron_matches("0 0 13 * 1", 0, 0, 13, 9, 3)); // đúng ngày 13
    assert!(cron_matches("0 0 13 * 1", 0, 0, 9, 9, 1)); // đúng Thứ 2
    assert!(!cron_matches("0 0 13 * 1", 0, 0, 9, 9, 3)); // không phải cả hai
  }

  #[test]
  fn cron_invalid_expr() {
    assert!(!cron_matches("* * *", 0, 0, 1, 1, 0)); // thiếu trường
    assert!(!cron_matches("bad 0 * * *", 0, 0, 1, 1, 0));
  }

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
  fn effective_ids_prefers_list_else_legacy() {
    let mk = |id: &str, ids: Vec<&str>| Schedule {
      id: "s".into(),
      scenario_id: id.into(),
      scenario_ids: ids.into_iter().map(String::from).collect(),
      name: "n".into(),
      enabled: true,
      trigger_type: TriggerType::Interval,
      cron_expr: None,
      interval_minutes: Some(1),
      timezone: None,
      time_window_start: None,
      time_window_end: None,
      max_runs_per_day: None,
    };
    assert_eq!(effective_scenario_ids(&mk("a", vec![])), vec!["a"]);
    assert_eq!(
      effective_scenario_ids(&mk("a", vec!["x", "y"])),
      vec!["x", "y"]
    );
    assert!(effective_scenario_ids(&mk("", vec![])).is_empty());
  }

  #[test]
  fn shuffle_preserves_multiset() {
    let mut v = vec![1, 2, 3, 4, 5];
    shuffle_in_place(&mut v);
    v.sort();
    assert_eq!(v, vec![1, 2, 3, 4, 5]);
    let mut one = vec![42];
    shuffle_in_place(&mut one);
    assert_eq!(one, vec![42]);
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
