//! Runs fenced examples published in user-facing documentation.
//!
//! The documentation style guide requires every user-facing fence to be
//! executable and exercised by the test suite. These tests load the shipped
//! text rather than a copied fixture, so a guide cannot drift away from the
//! behaviour it describes.

#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

#[path = "support/doc_examples.rs"]
mod doc_examples;

use diapason_core::config::constants::tools;
use diapason_core::metrics::{MetricsCollector, ToolRecoveryEvent, ToolRecoveryMetrics};
use diapason_core::tools::ToolRegistry;
use doc_examples::load_example;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// The fixture the versioned-patch example in the guide is written against.
const VERSIONED_PATCH_FIXTURE: &str = "fn main() {\nold\n}\n";

/// The published versioned-patch example applies to the fixture it names.
///
/// The example carries a literal `expected_content_hash`, so this test is the
/// contract that keeps that hash honest: it must describe the fixture, and the
/// patch must apply through the real registry with the hash as its
/// precondition.
#[tokio::test]
async fn versioned_patch_example_applies_to_its_fixture() {
    let example = load_example("docs/guides/patch-safety.md", "patch-safety-versioned-patch")
        .expect("the guide must publish a marked versioned-patch example");
    assert_eq!(example.language, "json", "the example must stay a JSON request body");
    let request: Value = serde_json::from_str(&example.body).expect("the published example must be valid JSON");

    let fixture_hash =
        format!("sha256:{}", diapason_commons::utils::calculate_sha256(VERSIONED_PATCH_FIXTURE.as_bytes()));
    assert_eq!(
        request["expected_content_hash"], fixture_hash,
        "the published hash must describe the fixture the example is written against"
    );

    let workspace = TempDir::new().expect("temp workspace");
    fs::create_dir_all(workspace.path().join("src")).expect("create src directory");
    fs::write(workspace.path().join("src/app.rs"), VERSIONED_PATCH_FIXTURE).expect("write fixture");

    let registry = ToolRegistry::new(workspace.path().to_path_buf()).await;
    registry.initialize_async().await.expect("initialize registry");
    registry.allow_all_tools().await.expect("allow tools");

    let result = registry
        .execute_tool(tools::APPLY_PATCH, request)
        .await
        .expect("apply_patch must run");
    assert_eq!(result["success"], true, "the published example must apply: {result}");
    assert_eq!(
        fs::read_to_string(workspace.path().join("src/app.rs")).expect("read patched file"),
        "fn main() {\nnew\n}\n"
    );
}

/// The published tool-recovery summary matches what a collector exports.
///
/// The counters in the guide are the user-visible contract for recovery
/// observability, so the events recorded here must produce exactly the
/// published JSON. A renamed field or a miscounted event breaks this test
/// rather than silently changing what users are told to expect.
#[test]
fn tool_recovery_summary_example_matches_the_collector() {
    let example = load_example("docs/users-guide.md", "users-guide-tool-recovery-summary")
        .expect("the users guide must publish a marked tool-recovery summary example");
    assert_eq!(example.language, "json", "the example must stay a JSON metrics summary");
    #[derive(serde::Deserialize)]
    struct PublishedSummary {
        tool_recovery: ToolRecoveryMetrics,
    }
    let published: PublishedSummary =
        serde_json::from_str(&example.body).expect("the published example must be a JSON metrics summary");

    let collector = MetricsCollector::new();
    collector.record_tool_recovery_events(ToolRecoveryEvent::StagedCall, 3);
    collector.record_tool_recovery_events(ToolRecoveryEvent::ReplacedCall, 3);
    for event in [
        ToolRecoveryEvent::InterruptedCall,
        ToolRecoveryEvent::RelocatedResult,
        ToolRecoveryEvent::RepairedArchive,
        ToolRecoveryEvent::AmbiguousHistory,
        ToolRecoveryEvent::StaleWriteStdinRequest,
        ToolRecoveryEvent::DuplicateToolCallBatch,
    ] {
        collector.record_tool_recovery(event);
    }

    assert_eq!(
        published.tool_recovery,
        collector.get_tool_recovery_metrics(),
        "the published counters must describe the events the collector records"
    );
}
