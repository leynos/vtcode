//! Tests for ACP file discovery and local tool behavior.

use super::*;

fn list_items_from_payload(payload: &Value) -> Vec<Value> {
    payload
        .get(TOOL_LIST_FILES_RESULT_KEY)
        .and_then(Value::as_object)
        .and_then(|result| result.get(TOOL_LIST_FILES_ITEMS_KEY))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn run_list_files_defaults_to_workspace_root() {
    let temp = TempDir::new().unwrap();
    let subdir = temp.path().join("src");
    fs::create_dir(&subdir).await.unwrap();
    let file_path = subdir.join("sample.txt");
    fs::write(&file_path, "hello").await.unwrap();

    let agent = build_agent(temp.path()).await;
    let report = agent.run_list_files(&json!({"path": "src"})).await.unwrap();

    assert!(matches!(report.status, ToolCallStatus::Completed));
    let payload = report.raw_output.unwrap();
    let items = list_items_from_payload(&payload);
    assert!(
        items.iter().any(|item| {
            item.get("name")
                .and_then(Value::as_str)
                .map(|name| name == "sample.txt")
                .or_else(|| {
                    item.get("path")
                        .and_then(Value::as_str)
                        .map(|path| path.ends_with("sample.txt"))
                })
                .unwrap_or(false)
        }),
        "payload: {payload}"
    );
}

#[tokio::test]
async fn run_list_files_accepts_uri_argument() {
    let temp = TempDir::new().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir_all(&nested).await.unwrap();
    let inner = nested.join("inner.txt");
    fs::write(&inner, "data").await.unwrap();

    let agent = build_agent(temp.path()).await;
    let uri = format!("file://{}", nested.to_string_lossy());
    let report = agent.run_list_files(&json!({ TOOL_LIST_FILES_URI_ARG: uri })).await.unwrap();

    assert!(matches!(report.status, ToolCallStatus::Completed));
    let payload = report.raw_output.unwrap();
    let items = list_items_from_payload(&payload);
    assert!(items.iter().any(|item| {
        item.get("path")
            .and_then(Value::as_str)
            .map(|path| path.contains("inner.txt"))
            .unwrap_or(false)
    }));
}
