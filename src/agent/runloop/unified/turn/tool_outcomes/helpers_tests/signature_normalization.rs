//! Read signature normalization tests.

use super::*;

// --- read_normalized_signature_key tests ---

#[test]
fn read_normalized_signature_key_normalizes_file_operation_read_offset() {
    let args_a = json!({"action": "read", "path": "src/lib.rs", "offset": 0, "limit": 100});
    let args_b = json!({"action": "read", "path": "src/lib.rs", "offset": 50, "limit": 200});
    let key_a = read_normalized_signature_key("file_operation", &args_a);
    let key_b = read_normalized_signature_key("file_operation", &args_b);
    assert_eq!(key_a, key_b, "same file read with different offset/limit should produce the same normalized key");
}

#[test]
fn read_normalized_signature_key_preserves_encoding() {
    let utf8 = json!({"action": "read", "path": "src/lib.rs", "encoding": "utf8"});
    let base64 = json!({"action": "read", "path": "src/lib.rs", "encoding": "base64"});

    assert_ne!(
        read_normalized_signature_key("file_operation", &utf8),
        read_normalized_signature_key("file_operation", &base64),
        "different encodings produce different tool output and must not reuse one another"
    );
}

#[test]
fn read_normalized_signature_key_differentiates_different_paths() {
    let args_a = json!({"action": "read", "path": "src/lib.rs"});
    let args_b = json!({"action": "read", "path": "src/main.rs"});
    let key_a = read_normalized_signature_key("file_operation", &args_a);
    let key_b = read_normalized_signature_key("file_operation", &args_b);
    assert_ne!(key_a, key_b, "different paths must produce different keys");
}

#[test]
fn read_normalized_signature_key_includes_code_search_limit_and_normalizes_filter_order() {
    let args_a = json!({
        "query": "Widget",
        "path": "src",
        "file_types": ["rust", "typescript"],
        "result_types": ["text", "definition"],
        "max_results": 10
    });
    let args_b = json!({
        "query": "Widget",
        "path": "src",
        "file_types": ["typescript", "rs"],
        "result_types": ["definition", "text"],
        "max_results": 100
    });
    let key_a = read_normalized_signature_key(tools::CODE_SEARCH, &args_a);
    let key_b = read_normalized_signature_key(tools::CODE_SEARCH, &args_b);
    assert_ne!(key_a, key_b, "different effective limits must not share one code-search replay identity");

    let args_default = json!({
        "query": " Widget ",
        "path": "src",
        "file_types": ["rs", "typescript"],
        "result_types": ["definition", "text"]
    });
    let args_explicit_default = json!({
        "query": "Widget",
        "path": "src",
        "file_types": ["typescript", "rust"],
        "result_types": ["text", "definition"],
        "max_results": 20
    });
    assert_eq!(
        read_normalized_signature_key(tools::CODE_SEARCH, &args_default),
        read_normalized_signature_key(tools::CODE_SEARCH, &args_explicit_default),
        "omitted and explicit default limits must share replay identity"
    );
}

#[test]
fn read_normalized_signature_key_preserves_mutation_for_write() {
    let args_a = json!({"path": "src/lib.rs", "content": "old"});
    let args_b = json!({"path": "src/lib.rs", "content": "new"});
    let key_a = read_normalized_signature_key("file_operation", &args_a);
    let key_b = read_normalized_signature_key("file_operation", &args_b);
    assert_ne!(key_a, key_b, "mutating writes must NOT be normalized away");
}
