//! Read extent coverage tests.

use super::*;

#[test]
fn read_extent_covers_query_rejects_larger_limit() {
    // Cached limit=200 must NOT cover query limit=220
    assert!(!read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":220}),
    ));

    // Cached limit=200 covers query limit=200 (same)
    assert!(read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
    ));

    // Cached limit=200 covers query limit=100 (subset)
    assert!(read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":100}),
    ));

    // Different offset must not match
    assert!(!read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        &json!({"action":"read","path":"AGENTS.md","offset":50,"limit":200}),
    ));
}

#[test]
fn read_extent_covers_query_rejects_different_raw_mode() {
    // Non-raw cached must NOT cover raw=true query
    assert!(!read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
    ));

    // Raw cached covers raw query
    assert!(read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
    ));

    // Raw cached must NOT cover non-raw query
    assert!(!read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
        &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
    ));
}

#[test]
fn read_extent_covers_query_handles_missing_limit() {
    // Both missing limit → matches (same default read)
    assert!(read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md"}),
        &json!({"action":"read","path":"AGENTS.md"}),
    ));

    // Cached has limit, query doesn't → mismatch
    assert!(!read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md","limit":200}),
        &json!({"action":"read","path":"AGENTS.md"}),
    ));

    // Cached has no limit, query does → mismatch
    assert!(!read_extent::extent_covers(
        &json!({"action":"read","path":"AGENTS.md"}),
        &json!({"action":"read","path":"AGENTS.md","limit":200}),
    ));
}
