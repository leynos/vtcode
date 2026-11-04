#![allow(
    missing_docs,
    reason = "Integration tests exercise apply_patch containment through its public API."
)]

use diapason_core::tools::editing::Patch;
use tempfile::TempDir;

#[test]
fn parser_rejects_lexically_traversing_patch_paths() {
    // A parent-directory traversal is rejected at parse time: it can never
    // produce a safe target however it is later joined against the workspace.
    let patch_text = "*** Begin Patch\n*** Add File: ../escaped.txt\n+escaped\n*** End Patch\n";
    assert!(Patch::parse(patch_text).is_err(), "parser accepted {patch_text:?}");
}

#[test]
fn parser_accepts_absolute_patch_paths() {
    // Absolute paths parse successfully; containment is enforced at apply time
    // against the workspace root rather than rejected here.
    let patch_text = "*** Begin Patch\n*** Add File: /tmp/escaped.txt\n+escaped\n*** End Patch\n";
    assert!(Patch::parse(patch_text).is_ok(), "absolute path should parse, got: {patch_text:?}");
}

#[tokio::test]
async fn absolute_path_outside_workspace_is_rejected_at_apply() {
    let workspace = TempDir::new().expect("workspace should be created");
    let patch = Patch::parse("*** Begin Patch\n*** Add File: /tmp/escaped.txt\n+escaped\n*** End Patch\n")
        .expect("absolute path should parse");

    let error = patch
        .apply(workspace.path())
        .await
        .expect_err("absolute path outside workspace must be rejected at apply");

    assert!(
        error.to_string().contains("containment") || error.to_string().contains("escapes workspace"),
        "unexpected error message: {error}"
    );
}

#[tokio::test]
async fn absolute_path_inside_workspace_applies() {
    let workspace = TempDir::new().expect("workspace should be created");
    let target = workspace.path().join("README.md");
    let patch_text = format!("*** Begin Patch\n*** Add File: {}\n+hello\n*** End Patch\n", target.display());
    let patch = Patch::parse(&patch_text).expect("absolute in-workspace path should parse");

    let result = patch
        .apply(workspace.path())
        .await
        .expect("absolute in-workspace path should apply");
    assert!(!result.is_empty());

    let content = tokio::fs::read_to_string(&target)
        .await
        .expect("created file should be readable");
    assert_eq!(content, "hello\n");
}

#[tokio::test]
async fn applies_a_valid_workspace_relative_path() {
    let workspace = TempDir::new().expect("workspace should be created");
    let patch = Patch::parse("*** Begin Patch\n*** Add File: src/created.txt\n+safe\n*** End Patch\n")
        .expect("valid patch should parse");

    let _ = patch
        .apply(workspace.path())
        .await
        .expect("workspace-relative patch should apply");

    let content = tokio::fs::read_to_string(workspace.path().join("src/created.txt"))
        .await
        .expect("created file should be readable");
    assert_eq!(content, "safe\n");
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_is_rejected_before_any_patch_mutation() {
    use std::os::unix::fs::symlink;

    let workspace = TempDir::new().expect("workspace should be created");
    let outside = TempDir::new().expect("outside directory should be created");
    let outside_file = outside.path().join("victim.txt");
    tokio::fs::write(&outside_file, "outside\n")
        .await
        .expect("outside file should be written");
    symlink(outside.path(), workspace.path().join("escape")).expect("symlink should be created");

    let patch = Patch::parse(
        "*** Begin Patch\n*** Add File: created.txt\n+must not exist\n*** Delete File: escape/victim.txt\n*** End Patch\n",
    )
    .expect("patch with workspace-relative paths should parse");

    let error = patch
        .apply(workspace.path())
        .await
        .expect_err("symlink escape must be rejected");

    assert!(error.to_string().contains("escapes the workspace root via symlink"));
    assert!(!workspace.path().join("created.txt").exists(), "preflight must reject before creating files");
    let content = tokio::fs::read_to_string(&outside_file)
        .await
        .expect("outside file should remain readable");
    assert_eq!(content, "outside\n");
}
