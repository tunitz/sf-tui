//! Parsing of `sf org list --json` and `sf org display --json` responses.

use serde_json::Value;

use super::catalog::string_field;

/// One org from either the scratch or non-scratch section of `org list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgSummary {
    pub username: String,
    pub aliases: Vec<String>,
    pub org_id: String,
    pub is_default: bool,
    pub is_devhub: bool,
    pub connected_status: String,
    pub instance_url: String,
    pub scratch: bool,
}

impl OrgSummary {
    pub fn label(&self) -> String {
        self.aliases
            .first()
            .cloned()
            .unwrap_or_else(|| self.username.clone())
    }
}

/// A safe subset of `org display`; secrets are never shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDetail {
    pub fields: Vec<(String, String)>,
    pub instance_url: String,
    pub username: String,
}

pub(crate) const SENSITIVE_FIELDS: &[&str] = &[
    "accesstoken",
    "refreshtoken",
    "sfdxauthurl",
    "password",
    "clientsecret",
    "privatekey",
    "privatekeyfile",
];

/// Parse both scratch and non-scratch org arrays, tolerating alias string/array shapes.
pub fn parse_org_list(value: &Value) -> Vec<OrgSummary> {
    let result = value.get("result").unwrap_or(value);
    let mut orgs = Vec::new();
    for (key, scratch) in [("scratchOrgs", true), ("nonScratchOrgs", false)] {
        let Some(items) = result.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            orgs.push(OrgSummary {
                username: string_field(item, "username").unwrap_or_default(),
                aliases: string_list(item.get("aliases").or_else(|| item.get("alias"))),
                org_id: string_field(item, "orgId").unwrap_or_default(),
                is_default: item
                    .get("isDefaultUsername")
                    .or_else(|| item.get("isDefault"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                is_devhub: item
                    .get("isDefaultDevHubUsername")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                connected_status: string_field(item, "connectedStatus").unwrap_or_default(),
                instance_url: string_field(item, "instanceUrl").unwrap_or_default(),
                scratch,
            });
        }
    }
    // A flat array (legacy shape) is treated as a list of non-scratch orgs.
    if orgs.is_empty()
        && let Some(items) = result.as_array()
    {
        for item in items {
            orgs.push(OrgSummary {
                username: string_field(item, "username").unwrap_or_default(),
                aliases: string_list(item.get("aliases").or_else(|| item.get("alias"))),
                org_id: string_field(item, "orgId").unwrap_or_default(),
                is_default: false,
                is_devhub: false,
                connected_status: string_field(item, "connectedStatus").unwrap_or_default(),
                instance_url: string_field(item, "instanceUrl").unwrap_or_default(),
                scratch: false,
            });
        }
    }
    orgs.sort_by_key(OrgSummary::label);
    orgs
}

/// Read the configured default org from `sf config get target-org --json` or an org display result.
pub fn parse_current_org(value: &Value) -> Option<String> {
    let result = value.get("result").unwrap_or(value);
    if let Some(org) = result
        .get("target-org")
        .or_else(|| result.get("targetOrg"))
        .and_then(Value::as_str)
        .filter(|org| !org.is_empty())
    {
        return Some(org.to_owned());
    }
    // `sf config get --json` also returns an array of {name, value} entries on some versions.
    if let Some(entries) = result.as_array() {
        for entry in entries {
            let name = string_field(entry, "name").unwrap_or_default();
            if name.eq_ignore_ascii_case("target-org")
                && let Some(org) = string_field(entry, "value")
            {
                return Some(org);
            }
        }
    }
    if let Some(org) = result.as_str().filter(|org| !org.is_empty()) {
        return Some(org.to_owned());
    }
    if let Some(Value::Array(items)) = result.get("aliases")
        && let Some(alias) = items.iter().filter_map(Value::as_str).next()
    {
        return Some(alias.to_owned());
    }
    string_field(result, "username")
}

/// Convert `org display` result fields into displayable key/value pairs.
pub fn parse_org_display(value: &Value) -> OrgDetail {
    let result = value.get("result").unwrap_or(value);
    let mut fields = Vec::new();
    let mut instance_url = String::new();
    let mut username = String::new();
    if let Some(map) = result.as_object() {
        for (key, value) in map {
            let normalized: String = key
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .collect();
            if SENSITIVE_FIELDS
                .iter()
                .any(|secret| normalized.eq_ignore_ascii_case(secret))
            {
                continue;
            }
            let text = display_value(value);
            if text.is_empty() {
                continue;
            }
            match key.as_str() {
                "instanceUrl" => instance_url = text.clone(),
                "username" => username = text.clone(),
                _ => {}
            }
            fields.push((key.clone(), text));
        }
    }
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    OrgDetail {
        fields,
        instance_url,
        username,
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => vec![text.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => String::new(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(&crate::json_view::sanitize(value))
                .unwrap_or_else(|_| "unprintable".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_list() -> Value {
        serde_json::json!({
          "status": 0,
          "result": {
            "scratchOrgs": [{
              "username": "test-user@example.invalid",
              "aliases": ["test-scratch"],
              "orgId": "00D000000000001",
              "isDefaultUsername": true,
              "isDefaultDevHubUsername": false,
              "connectedStatus": "Connected",
              "instanceUrl": "https://example.invalid.my.salesforce.com"
            }],
            "nonScratchOrgs": [{
              "username": "dev-user@example.invalid",
              "alias": "test-devhub",
              "orgId": "00D000000000002",
              "isDefaultDevHubUsername": true,
              "connectedStatus": "Connected",
              "instanceUrl": "https://example.invalid.my.salesforce.com"
            }]
          }
        })
    }

    #[test]
    fn org_list_parses_both_sections_and_flags() {
        let orgs = parse_org_list(&sample_list());
        assert_eq!(orgs.len(), 2);
        assert!(orgs.iter().any(|org| org.scratch && org.is_default));
        assert!(orgs.iter().any(|org| !org.scratch && org.is_devhub));
        assert!(orgs.iter().any(|org| org.label() == "test-devhub"));
    }

    #[test]
    fn current_org_is_read_from_config_or_display() {
        let config = serde_json::json!({"result":{"target-org":"test-scratch"}});
        assert_eq!(parse_current_org(&config).as_deref(), Some("test-scratch"));
        let display = serde_json::json!({"result":{"username":"test@example.invalid","aliases":["test-scratch"]}});
        assert_eq!(parse_current_org(&display).as_deref(), Some("test-scratch"));
        let entries = serde_json::json!({"result":[
            {"name":"target-org","value":"test-dev"},
            {"name":"other","value":"ignored"}
        ]});
        assert_eq!(parse_current_org(&entries).as_deref(), Some("test-dev"));
    }

    #[test]
    fn org_display_filters_credentials() {
        let value = serde_json::json!({"result": {
            "username": "test-user@example.invalid",
            "instanceUrl": "https://example.invalid.my.salesforce.com",
            "accessToken": "super-secret-token",
            "sfdxAuthUrl": "force://super-secret",
            "auth": {"refreshToken": "nested-secret"},
            "id": "00D000000000001"
        }});
        let detail = parse_org_display(&value);
        assert_eq!(detail.username, "test-user@example.invalid");
        assert!(!detail.fields.iter().any(|(key, value)| {
            key.to_ascii_lowercase().contains("token")
                || value.contains("super-secret")
                || value.contains("nested-secret")
        }));
    }
}
