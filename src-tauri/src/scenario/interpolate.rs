//! Variable interpolation: `{{var}}`, `{{a.b.c}}`, `{{var | filter:arg}}`,
//! `{{random_from: ["a","b"]}}`. Biến runtime là `serde_json::Value` (kiểu động);
//! system vars là `String`.

use std::collections::HashMap;

use serde_json::Value;

/// Thay mọi `{{...}}` trong `template`. Biến thiếu → "" + warning.
pub fn interpolate(
  template: &str,
  vars: &HashMap<String, Value>,
  sys: &HashMap<String, String>,
  warnings: &mut Vec<String>,
) -> String {
  let mut out = String::new();
  let mut i = 0;
  while i < template.len() {
    if template[i..].starts_with("{{") {
      if let Some(end) = template[i + 2..].find("}}") {
        let token = &template[i + 2..i + 2 + end];
        out.push_str(&resolve_token(token, vars, sys, warnings));
        i = i + 2 + end + 2;
        continue;
      }
    }
    // Đẩy 1 ký tự (an toàn UTF-8: i luôn ở ranh giới char).
    let ch = template[i..].chars().next().unwrap();
    out.push(ch);
    i += ch.len_utf8();
  }
  out
}

fn resolve_token(
  token: &str,
  vars: &HashMap<String, Value>,
  sys: &HashMap<String, String>,
  warnings: &mut Vec<String>,
) -> String {
  let token = token.trim();

  if let Some(rest) = token.strip_prefix("random_from:") {
    if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(rest.trim()) {
      if !arr.is_empty() {
        let idx = (rand::random::<u64>() % arr.len() as u64) as usize;
        return value_to_string(&arr[idx]);
      }
    }
    return String::new();
  }

  let mut parts = token.split('|');
  let var_path = parts.next().unwrap_or("").trim();

  let mut value: Value = if let Some(s) = sys.get(var_path) {
    Value::String(s.clone())
  } else {
    match resolve_path(vars, var_path) {
      Some(v) => v,
      None => {
        warnings.push(format!("Variable not found: {var_path}"));
        Value::Null
      }
    }
  };

  for raw in parts {
    let f = raw.trim();
    let (name, arg) = match f.split_once(':') {
      Some((n, a)) => (n.trim(), Some(a.trim())),
      None => (f, None),
    };
    value = apply_filter(name, arg, value);
  }

  value_to_string(&value)
}

fn resolve_path(vars: &HashMap<String, Value>, path: &str) -> Option<Value> {
  let mut iter = path.split('.');
  let first = iter.next()?;
  let mut current = vars.get(first)?.clone();
  for key in iter {
    current = current.get(key)?.clone();
  }
  Some(current)
}

fn apply_filter(name: &str, arg: Option<&str>, v: Value) -> Value {
  match name {
    "length" => match &v {
      Value::Array(a) => Value::from(a.len()),
      Value::String(s) => Value::from(s.chars().count()),
      _ => Value::from(0),
    },
    "upper" => Value::String(value_to_string(&v).to_uppercase()),
    "lower" => Value::String(value_to_string(&v).to_lowercase()),
    "truncate" => {
      let n: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(100);
      let s = value_to_string(&v);
      if s.chars().count() > n {
        Value::String(format!("{}...", s.chars().take(n).collect::<String>()))
      } else {
        Value::String(s)
      }
    }
    "join" => {
      let sep = arg.unwrap_or(",");
      match &v {
        Value::Array(a) => {
          Value::String(a.iter().map(value_to_string).collect::<Vec<_>>().join(sep))
        }
        _ => v,
      }
    }
    "first" => match v {
      Value::Array(a) => a.into_iter().next().unwrap_or(Value::Null),
      other => other,
    },
    "last" => match v {
      Value::Array(a) => a.into_iter().next_back().unwrap_or(Value::Null),
      other => other,
    },
    "json" => Value::String(v.to_string()),
    "format_vnd" => {
      let n = match &v {
        Value::Number(num) => num.as_f64().unwrap_or(0.0),
        Value::String(s) => s.parse().unwrap_or(0.0),
        _ => 0.0,
      };
      Value::String(format!("{} ₫", thousands_sep(n)))
    }
    // Filter lạ → giữ nguyên giá trị (engine có thể chọn fail tùy chính sách).
    _ => v,
  }
}

fn thousands_sep(n: f64) -> String {
  let int = n.trunc().abs() as u64;
  let digits = int.to_string();
  let mut out = String::new();
  for (i, ch) in digits.chars().enumerate() {
    if i > 0 && (digits.len() - i).is_multiple_of(3) {
      out.push('.');
    }
    out.push(ch);
  }
  if n < 0.0 {
    format!("-{out}")
  } else {
    out
  }
}

fn value_to_string(v: &Value) -> String {
  match v {
    Value::String(s) => s.clone(),
    Value::Null => String::new(),
    other => other.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn ctx() -> (HashMap<String, Value>, HashMap<String, String>) {
    let mut vars = HashMap::new();
    vars.insert("name".to_string(), json!("alice"));
    vars.insert("price".to_string(), json!(1250000));
    vars.insert(
      "item".to_string(),
      json!({ "title": "Hello", "url": "x.com" }),
    );
    vars.insert("tags".to_string(), json!(["a", "b", "c"]));
    let mut sys = HashMap::new();
    sys.insert("date".to_string(), "2026-06-09".to_string());
    (vars, sys)
  }

  #[test]
  fn basic_var_and_system() {
    let (vars, sys) = ctx();
    let mut w = Vec::new();
    assert_eq!(
      interpolate("Hi {{name}} on {{date}}", &vars, &sys, &mut w),
      "Hi alice on 2026-06-09"
    );
    assert!(w.is_empty());
  }

  #[test]
  fn nested_path_and_filters() {
    let (vars, sys) = ctx();
    let mut w = Vec::new();
    assert_eq!(interpolate("{{item.title}}", &vars, &sys, &mut w), "Hello");
    assert_eq!(
      interpolate("{{name | upper}}", &vars, &sys, &mut w),
      "ALICE"
    );
    assert_eq!(interpolate("{{tags | length}}", &vars, &sys, &mut w), "3");
    assert_eq!(
      interpolate("{{tags | join:- }}", &vars, &sys, &mut w),
      "a-b-c"
    );
    assert_eq!(
      interpolate("{{price | format_vnd}}", &vars, &sys, &mut w),
      "1.250.000 ₫"
    );
  }

  #[test]
  fn missing_var_warns_and_blanks() {
    let (vars, sys) = ctx();
    let mut w = Vec::new();
    assert_eq!(interpolate("x={{nope}}", &vars, &sys, &mut w), "x=");
    assert_eq!(w.len(), 1);
  }
}
