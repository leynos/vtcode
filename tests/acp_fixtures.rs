#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Clone)]
#[allow(
    dead_code,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
pub struct PermissionFixture {
    #[serde(rename = "sessionId")]
    pub session_id: Value,
    #[serde(rename = "toolCall")]
    pub tool_call: Value,
    pub arguments: Value,
}

pub fn read_file_permission() -> Result<PermissionFixture, serde_json::Error> {
    load_fixture(include_str!("fixtures/acp/permission_read_file.json"))
}

pub fn list_files_permission() -> Result<PermissionFixture, serde_json::Error> {
    load_fixture(include_str!("fixtures/acp/permission_list_files.json"))
}

fn load_fixture(contents: &str) -> Result<PermissionFixture, serde_json::Error> {
    serde_json::from_str(contents)
}
