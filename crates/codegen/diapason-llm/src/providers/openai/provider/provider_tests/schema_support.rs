//! Schema traversal helper retained for OpenAI tool-payload tests.

use super::*;

#[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
fn schema_keyword_path(value: &Value, keywords: &[&str], path: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            let properties_object = path.ends_with(".properties");
            if !properties_object {
                for keyword in keywords {
                    if map.contains_key(*keyword) {
                        return Some(format!("{path}.{keyword}"));
                    }
                }
            }
            for (key, nested) in map {
                if let Some(found) = schema_keyword_path(nested, keywords, &format!("{path}.{key}")) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, nested)| schema_keyword_path(nested, keywords, &format!("{path}[{i}]"))),
        _ => None,
    }
}
