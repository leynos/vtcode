//! Subagent memory appendix rendering.
use super::*;

#[test]
fn load_memory_appendix_renders_compact_summary() {
    let temp = TempDir::new().expect("tempdir");
    let memory_dir = temp
        .path()
        .join(diapason_commons::workspace_dir_name())
        .join("agent-memory/reviewer");
    std::fs::create_dir_all(&memory_dir).expect("memory dir");
    std::fs::write(
            memory_dir.join("MEMORY.md"),
            "# Reviewer Memory\n\n## Preferences\n- Keep diffs surgical.\n- Run focused tests before broad checks.\n- Prefer repo docs for orientation.\n- Ask only when a decision is materially blocked.\n- Additional long-form notes that should stay out of the prompt body.\n",
        )
        .expect("write memory");

    let appendix = load_memory_appendix(temp.path(), "reviewer", Some(SubagentMemoryScope::Project))
        .expect("appendix")
        .expect("memory appendix");

    assert!(appendix.contains("Persistent memory file:"));
    assert!(appendix.contains("Key points:"));
    assert!(appendix.contains("Keep diffs surgical."));
    assert!(appendix.contains("Open `MEMORY.md` when exact wording or more detail matters."));
    assert!(!appendix.contains("Current MEMORY.md excerpt"));
    assert!(!appendix.contains("## Preferences"));
}

#[tokio::test]
async fn async_load_memory_appendix_matches_sync_output() {
    let temp = TempDir::new().expect("tempdir");
    let memory_dir = temp
        .path()
        .join(diapason_commons::workspace_dir_name())
        .join("agent-memory/reviewer");
    std::fs::create_dir_all(&memory_dir).expect("memory dir");
    std::fs::write(
        memory_dir.join("MEMORY.md"),
        "# Reviewer Memory\n\n- Keep the patch focused.\n- Run nextest before the workspace gate.\n",
    )
    .expect("write memory");

    let sync =
        load_memory_appendix(temp.path(), "reviewer", Some(SubagentMemoryScope::Project)).expect("sync appendix");
    let asynchronous = load_memory_appendix_async(temp.path(), "reviewer", Some(SubagentMemoryScope::Project))
        .await
        .expect("async appendix");

    assert_eq!(asynchronous, sync);
}

#[test]
fn load_primary_memory_appendix_reads_existing_memory_without_write_guidance() {
    let temp = TempDir::new().expect("tempdir");
    let memory_dir = temp
        .path()
        .join(diapason_commons::workspace_dir_name())
        .join("agent-memory/reviewer");
    std::fs::create_dir_all(&memory_dir).expect("memory dir");
    std::fs::write(
        memory_dir.join("MEMORY.md"),
        "# Reviewer Memory\n\n## Preferences\n- Keep diffs surgical.\n- Run focused tests before broad checks.\n",
    )
    .expect("write memory");

    let appendix = load_primary_memory_appendix(temp.path(), "reviewer", Some(SubagentMemoryScope::Project))
        .expect("appendix")
        .expect("memory appendix");

    assert!(appendix.contains("Primary-agent memory file:"));
    assert!(appendix.contains("Loaded read-only for this request."));
    assert!(appendix.contains("Key points:"));
    assert!(appendix.contains("Keep diffs surgical."));
    assert!(!appendix.contains("Read and maintain `MEMORY.md`"));
    assert!(!appendix.contains("Create or update `MEMORY.md`"));
    assert!(!appendix.contains("Open `MEMORY.md` when exact wording or more detail matters."));
}

#[test]
fn load_primary_memory_appendix_missing_memory_is_noop_without_directory_creation() {
    let temp = TempDir::new().expect("tempdir");
    let memory_dir = temp
        .path()
        .join(diapason_commons::workspace_dir_name())
        .join("agent-memory/reviewer");

    let appendix =
        load_primary_memory_appendix(temp.path(), "reviewer", Some(SubagentMemoryScope::Project)).expect("appendix");

    assert!(appendix.is_none());
    assert!(!memory_dir.exists());
}
