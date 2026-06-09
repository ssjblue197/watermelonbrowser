//! Persistence: định nghĩa Scenario → JSON file (như profile metadata); lịch sử
//! chạy (Run + StepLog) → SQLite. Hàm nhận `dir` tường minh để test bằng temp dir.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;

use crate::scenario::model::{Scenario, StepLog};

/// Bản ghi 1 lần chạy để persist (gom từ RunContext sau khi engine chạy xong).
pub struct RunRecord {
  pub id: String,
  pub scenario_id: String,
  pub profile_id: String,
  pub triggered_by: String, // "manual" | "schedule" | "api"
  pub status: String,       // "success" | "failed" | "stopped"
  pub started_at: String,   // ISO
  pub finished_at: String,
  pub duration_ms: u128,
  pub error: Option<String>,
  pub warnings: Vec<String>,
  pub variables: HashMap<String, Value>,
  pub steps: Vec<StepLog>,
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
  pub id: String,
  pub scenario_id: String,
  pub profile_id: String,
  pub status: String,
  pub started_at: String,
  pub duration_ms: i64,
  pub steps_ok: i64,
  pub steps_failed: i64,
}

pub struct ScenarioStore {
  dir: PathBuf,
  db: PathBuf,
}

impl ScenarioStore {
  pub fn new(dir: PathBuf) -> Self {
    let db = dir.join("runs.db");
    Self { dir, db }
  }

  /// Vị trí mặc định: `<data_dir>/scenarios`.
  pub fn default_location() -> Self {
    Self::new(crate::app_dirs::data_dir().join("scenarios"))
  }

  fn ensure_dir(&self) -> std::io::Result<()> {
    fs::create_dir_all(&self.dir)
  }

  // --- Scenario JSON ---

  pub fn save_scenario(&self, s: &Scenario) -> std::io::Result<()> {
    self.ensure_dir()?;
    let path = self.dir.join(format!("{}.json", s.id));
    let json = serde_json::to_string_pretty(s)
      .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write(&path, json.as_bytes())
  }

  pub fn load_scenario(&self, id: &str) -> Option<Scenario> {
    let path = self.dir.join(format!("{id}.json"));
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
  }

  pub fn list_scenarios(&self) -> Vec<Scenario> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&self.dir) else {
      return out;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.extension().and_then(|e| e.to_str()) == Some("json")
        && path.file_name().and_then(|n| n.to_str()) != Some("runs.db")
      {
        if let Ok(c) = fs::read_to_string(&path) {
          if let Ok(s) = serde_json::from_str::<Scenario>(&c) {
            out.push(s);
          }
        }
      }
    }
    out
  }

  pub fn delete_scenario(&self, id: &str) -> std::io::Result<()> {
    let path = self.dir.join(format!("{id}.json"));
    if path.exists() {
      fs::remove_file(path)?;
    }
    Ok(())
  }

  // --- Run history (SQLite) ---

  fn open_db(&self) -> rusqlite::Result<Connection> {
    if let Err(e) = self.ensure_dir() {
      return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(e)));
    }
    let conn = Connection::open(&self.db)?;
    conn.execute_batch(
      "CREATE TABLE IF NOT EXISTS runs (
         id TEXT PRIMARY KEY, scenario_id TEXT, profile_id TEXT, triggered_by TEXT,
         status TEXT, started_at TEXT, finished_at TEXT, duration_ms INTEGER,
         error TEXT, warnings_json TEXT, variables_json TEXT
       );
       CREATE TABLE IF NOT EXISTS step_logs (
         id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT, seq INTEGER,
         block_id TEXT, block_type TEXT, status TEXT, duration_ms INTEGER, error TEXT
       );
       CREATE INDEX IF NOT EXISTS idx_steps_run ON step_logs(run_id);",
    )?;
    Ok(conn)
  }

  pub fn record_run(&self, run: &RunRecord) -> rusqlite::Result<()> {
    let mut conn = self.open_db()?;
    let tx = conn.transaction()?;
    tx.execute(
      "INSERT OR REPLACE INTO runs
        (id, scenario_id, profile_id, triggered_by, status, started_at, finished_at,
         duration_ms, error, warnings_json, variables_json)
       VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
      params![
        run.id,
        run.scenario_id,
        run.profile_id,
        run.triggered_by,
        run.status,
        run.started_at,
        run.finished_at,
        run.duration_ms as i64,
        run.error,
        serde_json::to_string(&run.warnings).unwrap_or_default(),
        serde_json::to_string(&run.variables).unwrap_or_default(),
      ],
    )?;
    for (seq, step) in run.steps.iter().enumerate() {
      tx.execute(
        "INSERT INTO step_logs (run_id, seq, block_id, block_type, status, duration_ms, error)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
          run.id,
          seq as i64,
          step.block_id,
          step.block_type,
          step.status,
          step.duration_ms as i64,
          step.error,
        ],
      )?;
    }
    tx.commit()
  }

  pub fn list_runs(&self, limit: i64) -> Vec<RunSummary> {
    let Ok(conn) = self.open_db() else {
      return Vec::new();
    };
    let sql = "SELECT r.id, r.scenario_id, r.profile_id, r.status, r.started_at, r.duration_ms,
        (SELECT COUNT(*) FROM step_logs s WHERE s.run_id=r.id AND s.status IN ('ok','retried','dry_run')),
        (SELECT COUNT(*) FROM step_logs s WHERE s.run_id=r.id AND s.status='failed')
      FROM runs r ORDER BY r.started_at DESC LIMIT ?1";
    let Ok(mut stmt) = conn.prepare(sql) else {
      return Vec::new();
    };
    let rows = stmt.query_map(params![limit], |row| {
      Ok(RunSummary {
        id: row.get(0)?,
        scenario_id: row.get(1)?,
        profile_id: row.get(2)?,
        status: row.get(3)?,
        started_at: row.get(4)?,
        duration_ms: row.get(5)?,
        steps_ok: row.get(6)?,
        steps_failed: row.get(7)?,
      })
    });
    match rows {
      Ok(it) => it.flatten().collect(),
      Err(_) => Vec::new(),
    }
  }
}

fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
  let tmp = path.with_extension("tmp");
  fs::write(&tmp, data)?;
  fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::scenario::model::{Block, Scenario};
  use serde_json::json;

  fn temp_dir() -> PathBuf {
    let p = std::env::temp_dir().join(format!("scn-store-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).unwrap();
    p
  }

  #[test]
  fn scenario_roundtrip_and_run_record() {
    let dir = temp_dir();
    let store = ScenarioStore::new(dir.clone());

    let mut s = Scenario {
      id: "abc".into(),
      name: "demo".into(),
      description: None,
      ai_mode: Default::default(),
      on_error: Default::default(),
      caps: Default::default(),
      blocks: vec![Block::new("open_url", json!({ "url": "https://x.com" }))],
    };
    store.save_scenario(&s).unwrap();
    s = store.load_scenario("abc").unwrap();
    assert_eq!(s.name, "demo");
    assert_eq!(store.list_scenarios().len(), 1);

    let run = RunRecord {
      id: "run1".into(),
      scenario_id: "abc".into(),
      profile_id: "p1".into(),
      triggered_by: "manual".into(),
      status: "success".into(),
      started_at: "2026-06-09T00:00:00Z".into(),
      finished_at: "2026-06-09T00:00:01Z".into(),
      duration_ms: 1000,
      error: None,
      warnings: vec!["w".into()],
      variables: HashMap::new(),
      steps: vec![StepLog {
        block_id: "b1".into(),
        block_type: "open_url".into(),
        status: "ok".into(),
        duration_ms: 50,
        error: None,
      }],
    };
    store.record_run(&run).unwrap();
    let runs = store.list_runs(10);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].steps_ok, 1);
    assert_eq!(runs[0].status, "success");

    let _ = fs::remove_dir_all(&dir);
  }
}
