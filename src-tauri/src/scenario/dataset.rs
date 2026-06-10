//! Dataset: bảng dữ liệu (mỗi dòng = bộ field) để seed biến cho scenario.
//!
//! Import từ paste/file: tự nhận JSON (mảng object/scalar) / TSV (paste từ sheet) /
//! CSV (quote-aware) / 1-cột (mỗi dòng 1 giá trị → cột `value`). Lưu JSON ở
//! `<data_dir>/scenarios/datasets/<id>.json` (xem manager.rs CRUD).
//!
//! Ô lưu dạng `Value::String` cho nguồn delimited (khớp interpolation vốn stringify);
//! nguồn JSON giữ nguyên kiểu. Dòng thiếu cột → bỏ key (→ "Variable not found" +
//! "" lúc interpolate). Dòng dư ô → ghi `DatasetParseError` + cắt bớt.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::scenario::model::DataBinding;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
  pub id: String,
  pub name: String,
  #[serde(default)]
  pub columns: Vec<String>,
  #[serde(default)]
  pub rows: Vec<Map<String, Value>>,
  #[serde(default)]
  pub created_at: String,
  #[serde(default)]
  pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Delimiter {
  #[default]
  Auto,
  Tab,
  Comma,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetParseError {
  pub line: usize,
  pub raw: String,
  pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetParseResult {
  pub columns: Vec<String>,
  pub rows: Vec<Map<String, Value>>,
  pub errors: Vec<DatasetParseError>,
}

/// Parse text người dùng dán/nạp thành columns + rows. Tự nhận JSON nếu bắt đầu
/// bằng `[`, ngược lại tách theo delimiter (Auto → sniff tab rồi comma).
pub fn parse_dataset(content: &str, delim: Delimiter, has_header: bool) -> DatasetParseResult {
  if content.trim_start().starts_with('[') {
    return parse_json(content);
  }
  parse_delimited(content, delim, has_header)
}

fn parse_json(content: &str) -> DatasetParseResult {
  let mut columns: Vec<String> = Vec::new();
  let mut rows: Vec<Map<String, Value>> = Vec::new();
  let mut errors = Vec::new();
  match serde_json::from_str::<Value>(content) {
    Ok(Value::Array(arr)) => {
      for (i, el) in arr.into_iter().enumerate() {
        match el {
          Value::Object(map) => {
            for k in map.keys() {
              if !columns.iter().any(|c| c == k) {
                columns.push(k.clone());
              }
            }
            rows.push(map);
          }
          v @ (Value::String(_) | Value::Number(_) | Value::Bool(_)) => {
            if !columns.iter().any(|c| c == "value") {
              columns.push("value".to_string());
            }
            let mut m = Map::new();
            m.insert("value".to_string(), v);
            rows.push(m);
          }
          other => errors.push(DatasetParseError {
            line: i + 1,
            raw: other.to_string(),
            reason: "row must be an object or scalar".to_string(),
          }),
        }
      }
    }
    Ok(_) => errors.push(DatasetParseError {
      line: 0,
      raw: String::new(),
      reason: "JSON must be an array".to_string(),
    }),
    Err(e) => errors.push(DatasetParseError {
      line: 0,
      raw: String::new(),
      reason: format!("invalid JSON: {e}"),
    }),
  }
  DatasetParseResult {
    columns,
    rows,
    errors,
  }
}

fn sniff(content: &str) -> char {
  for line in content.lines() {
    let l = line.trim();
    if l.is_empty() {
      continue;
    }
    if l.contains('\t') {
      return '\t';
    }
    if l.contains(',') {
      return ',';
    }
    return '\t'; // single column → delimiter không quan trọng
  }
  '\t'
}

fn parse_delimited(content: &str, delim: Delimiter, has_header: bool) -> DatasetParseResult {
  let sep = match delim {
    Delimiter::Tab => '\t',
    Delimiter::Comma => ',',
    Delimiter::Auto => sniff(content),
  };
  let mut errors = Vec::new();

  // Giữ số dòng gốc để báo lỗi; bỏ dòng trống.
  let data_lines: Vec<(usize, &str)> = content
    .lines()
    .enumerate()
    .filter(|(_, l)| !l.trim().is_empty())
    .map(|(i, l)| (i + 1, l))
    .collect();
  if data_lines.is_empty() {
    return DatasetParseResult {
      columns: vec![],
      rows: vec![],
      errors,
    };
  }

  let mut columns: Vec<String>;
  let start;
  if has_header {
    columns = split_row(data_lines[0].1, sep)
      .into_iter()
      .enumerate()
      .map(|(idx, c)| {
        let c = c.trim().to_string();
        if c.is_empty() {
          format!("col{}", idx + 1)
        } else {
          c
        }
      })
      .collect();
    dedup_columns(&mut columns);
    start = 1;
  } else {
    let n = split_row(data_lines[0].1, sep).len().max(1);
    columns = if n == 1 {
      vec!["value".to_string()]
    } else {
      (1..=n).map(|i| format!("col{i}")).collect()
    };
    start = 0;
  }

  let ncols = columns.len();
  let mut rows = Vec::new();
  for &(lineno, line) in &data_lines[start..] {
    let cells = split_row(line, sep);
    if cells.len() > ncols {
      errors.push(DatasetParseError {
        line: lineno,
        raw: line.to_string(),
        reason: format!("{} cells > {ncols} columns; extra truncated", cells.len()),
      });
    }
    let mut m = Map::new();
    for (idx, col) in columns.iter().enumerate() {
      // Ô vắng (dòng ngắn) → bỏ key; ô có (kể cả rỗng) → giữ chuỗi.
      if let Some(cell) = cells.get(idx) {
        m.insert(col.clone(), Value::String(cell.trim().to_string()));
      }
    }
    rows.push(m);
  }

  DatasetParseResult {
    columns,
    rows,
    errors,
  }
}

/// Tách 1 dòng. Tab → split thẳng (paste từ sheet); comma → quote-aware (CSV).
fn split_row(line: &str, sep: char) -> Vec<String> {
  if sep == '\t' {
    return line.split('\t').map(|s| s.to_string()).collect();
  }
  let mut out = Vec::new();
  let mut cur = String::new();
  let mut in_q = false;
  let mut chars = line.chars().peekable();
  while let Some(c) = chars.next() {
    if in_q {
      if c == '"' {
        if chars.peek() == Some(&'"') {
          cur.push('"');
          chars.next();
        } else {
          in_q = false;
        }
      } else {
        cur.push(c);
      }
    } else if c == '"' {
      in_q = true;
    } else if c == sep {
      out.push(std::mem::take(&mut cur));
    } else {
      cur.push(c);
    }
  }
  out.push(cur);
  out
}

fn dedup_columns(cols: &mut [String]) {
  let mut seen: HashMap<String, u32> = HashMap::new();
  for c in cols.iter_mut() {
    let n = seen.entry(c.clone()).or_insert(0);
    if *n > 0 {
      *c = format!("{c}_{}", *n + 1);
    }
    *n += 1;
  }
}

/// Dựng tập biến seed từ 1 dòng theo binding: có prefix → gói dưới namespace
/// (`{{row.reply}}`); không → trải phẳng thành biến top-level (`{{reply}}`).
pub fn build_seed(binding: &DataBinding, row: &Map<String, Value>) -> HashMap<String, Value> {
  let mut out = HashMap::new();
  match binding.prefix.as_deref() {
    Some(p) if !p.is_empty() => {
      out.insert(p.to_string(), Value::Object(row.clone()));
    }
    _ => {
      for (k, v) in row {
        out.insert(k.clone(), v.clone());
      }
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::scenario::model::DataMode;
  use serde_json::json;

  #[test]
  fn parses_tsv_with_header() {
    let r = parse_dataset(
      "subject\treply\nHi\tHello there\nYo\tWhat's up",
      Delimiter::Auto,
      true,
    );
    assert_eq!(r.columns, vec!["subject", "reply"]);
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0]["reply"], json!("Hello there"));
    assert!(r.errors.is_empty());
  }

  #[test]
  fn parses_csv_quoted() {
    let r = parse_dataset("a,b\n\"x,1\",\"y\"\"q\"", Delimiter::Comma, true);
    assert_eq!(r.columns, vec!["a", "b"]);
    assert_eq!(r.rows[0]["a"], json!("x,1"));
    assert_eq!(r.rows[0]["b"], json!("y\"q"));
  }

  #[test]
  fn single_column_txt_no_header() {
    let r = parse_dataset("Great video!\nThanks\nNice", Delimiter::Auto, false);
    assert_eq!(r.columns, vec!["value"]);
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[2]["value"], json!("Nice"));
  }

  #[test]
  fn ragged_short_row_omits_key_and_long_row_errors() {
    let r = parse_dataset("a\tb\tc\n1\t2\n1\t2\t3\t4", Delimiter::Tab, true);
    assert_eq!(r.rows[0].get("c"), None); // short → omit
    assert_eq!(r.rows[1]["c"], json!("3")); // extra "4" truncated
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].line, 3);
  }

  #[test]
  fn json_array_of_objects() {
    let r = parse_dataset(
      r#"[{"reply":"ok"},{"reply":"sure","extra":"x"}]"#,
      Delimiter::Auto,
      false,
    );
    assert_eq!(r.columns, vec!["reply", "extra"]);
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[1]["extra"], json!("x"));
  }

  #[test]
  fn build_seed_flat_vs_prefixed() {
    let mut row = Map::new();
    row.insert("reply".to_string(), json!("hi"));
    let flat = build_seed(
      &DataBinding {
        dataset_id: "d".into(),
        mode: DataMode::Random,
        prefix: None,
      },
      &row,
    );
    assert_eq!(flat["reply"], json!("hi"));
    let pre = build_seed(
      &DataBinding {
        dataset_id: "d".into(),
        mode: DataMode::Random,
        prefix: Some("row".into()),
      },
      &row,
    );
    assert_eq!(pre["row"], json!({ "reply": "hi" }));
    assert!(!pre.contains_key("reply"));
  }
}
