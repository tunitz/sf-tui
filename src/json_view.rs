//! Turning arbitrary SF JSON responses into compact, readable tables.

use serde_json::Value;

use crate::sf::org::SENSITIVE_FIELDS;

/// A tabular representation of a JSON document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonView {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

const MAX_COLUMNS: usize = 12;
const MAX_ROWS: usize = 200;
const MAX_CELL: usize = 120;

/// Build a table from arrays of objects, objects, or scalar JSON values.
pub fn table(value: &Value) -> JsonView {
    match value {
        Value::Array(items) => {
            let objects: Vec<&Value> = items.iter().filter(|item| item.is_object()).collect();
            if objects.is_empty() {
                return JsonView {
                    headers: vec!["value".to_owned()],
                    rows: items
                        .iter()
                        .take(MAX_ROWS)
                        .map(|item| vec![cell(item)])
                        .collect(),
                };
            }
            let mut headers = Vec::new();
            for object in &objects {
                if let Some(map) = object.as_object() {
                    for key in map.keys() {
                        if is_sensitive(key) || headers.contains(key) {
                            continue;
                        }
                        headers.push(key.clone());
                        if headers.len() >= MAX_COLUMNS {
                            break;
                        }
                    }
                }
                if headers.len() >= MAX_COLUMNS {
                    break;
                }
            }
            let rows = objects
                .iter()
                .take(MAX_ROWS)
                .map(|object| {
                    headers
                        .iter()
                        .map(|header| object.get(header).map(cell).unwrap_or_default())
                        .collect()
                })
                .collect();
            JsonView { headers, rows }
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> =
                map.iter().filter(|(key, _)| !is_sensitive(key)).collect();
            entries.sort_by_key(|(key, _)| *key);
            JsonView {
                headers: vec!["field".to_owned(), "value".to_owned()],
                rows: entries
                    .iter()
                    .take(MAX_ROWS)
                    .map(|(key, value)| vec![(*key).clone(), cell(value)])
                    .collect(),
            }
        }
        Value::Null => JsonView::default(),
        scalar => JsonView {
            headers: vec!["value".to_owned()],
            rows: vec![vec![cell(scalar)]],
        },
    }
}

fn is_sensitive(key: &str) -> bool {
    let normalized: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
    SENSITIVE_FIELDS
        .iter()
        .any(|secret| normalized.eq_ignore_ascii_case(secret))
}

fn cell(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => {
            let elements: Vec<String> = items.iter().map(cell).collect();
            elements.join(", ")
        }
        Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
        other => other.to_string(),
    };
    let single_line = text.replace(['\n', '\r'], " ");
    if single_line.chars().count() > MAX_CELL {
        let shortened: String = single_line.chars().take(MAX_CELL - 1).collect();
        format!("{shortened}…")
    } else {
        single_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_of_objects_becomes_a_table() {
        let value = serde_json::json!([{"username":"one@test.invalid","alias":"a"},{"username":"two@test.invalid","alias":"b"}]);
        let view = table(&value);
        assert_eq!(view.headers, vec!["alias", "username"]);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0][1], "one@test.invalid");
    }

    #[test]
    fn credentials_are_not_rendered() {
        let value = serde_json::json!({"username":"one@test.invalid","accessToken":"secret"});
        let view = table(&value);
        assert_eq!(view.headers, vec!["field".to_owned(), "value".to_owned()]);
        assert!(view.rows.iter().any(|row| row[0] == "username"));
        assert!(!view.headers.iter().any(|header| header.contains("Token")));
        assert!(
            !view
                .rows
                .iter()
                .flatten()
                .any(|cell| cell.contains("secret"))
        );
    }

    #[test]
    fn scalar_and_empty_values_are_handled() {
        assert_eq!(table(&Value::Null).rows.len(), 0);
        assert_eq!(table(&serde_json::json!(42)).rows[0][0], "42");
    }
}
