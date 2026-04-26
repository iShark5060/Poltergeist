use crate::models::PoltergeistConfig;
use serde_json::Value;

pub fn default_config() -> PoltergeistConfig {
    PoltergeistConfig::default()
}

pub fn merge_into_default(data: Option<Value>) -> PoltergeistConfig {
    let mut merged = PoltergeistConfig::default();
    let Some(Value::Object(root)) = data else {
        return merged;
    };

    if let Some(version) = root.get("version").and_then(|v| v.as_i64()) {
        merged.version = version as i32;
    }

    if let Some(settings) = root.get("settings") {
        merged.settings = serde_json::from_value(settings.clone()).unwrap_or_default();
    }

    if let Some(tree_personal) = root.get("tree_personal") {
        merged.tree_personal = serde_json::from_value(tree_personal.clone()).unwrap_or_default();
    } else if let Some(tree_legacy) = root.get("tree") {
        merged.tree_personal = serde_json::from_value(tree_legacy.clone()).unwrap_or_default();
    }

    if let Some(tree_team) = root.get("tree_team") {
        merged.tree_team = serde_json::from_value(tree_team.clone()).unwrap_or_default();
    }

    merged
}
