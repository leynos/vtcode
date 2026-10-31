//! Tests for deletion recognition and workspace target assessment.

use super::*;
use tempfile::TempDir;

fn supported(command: &str) -> DeletionPlan {
    match recognize_deletion_command(command) {
        DeletionRecognition::Supported(plan) => plan,
        other => panic!("expected supported deletion for {command:?}, got {other:?}"),
    }
}

fn unsupported(command: &str) -> String {
    match recognize_deletion_command(command) {
        DeletionRecognition::Unsupported { reason } => reason,
        other => panic!("expected unsupported deletion for {command:?}, got {other:?}"),
    }
}

#[test]
fn recognizes_documented_short_and_long_forms() {
    for command in [
        "rm -f scratch/tmp.txt",
        "rm -r scratch",
        "rm -R scratch",
        "rm -d scratch",
        "rm -v -f scratch/tmp.txt",
        "rm -rvf scratch",
        "rm --recursive scratch",
        "rm --force scratch/tmp.txt",
        "rm --dir scratch",
        "rm --verbose scratch/tmp.txt",
        "rm -i scratch/tmp.txt",
        "rm -I scratch/tmp.txt",
        "rm --interactive scratch/tmp.txt",
        "rm --interactive=once scratch/tmp.txt",
        "rm -- -f",
    ] {
        assert!(recognize_deletion_command(command).is_supported(), "expected supported: {command}");
    }
}

#[test]
fn keeps_plain_rm_outside_the_dangerous_class_but_recognized() {
    let plan = supported("rm scratch/tmp.txt");
    assert_eq!(plan.operands, vec!["scratch/tmp.txt".to_string()]);
    assert!(!crate::command_safety::dangerous_commands::command_might_be_dangerous(&[
        "rm".to_string(),
        "scratch/tmp.txt".to_string(),
    ]));
}

#[test]
fn rejects_non_deletion_commands() {
    for command in ["ls -la", "echo rm", "cargo check", "rg rm src"] {
        assert_eq!(recognize_deletion_command(command), DeletionRecognition::NotDeletion, "unexpected: {command}");
    }
}

#[test]
fn classifies_matching_representations_as_one_deletion() {
    let commands = vec!["rm -f scratch/tmp.txt".to_string(), "rm -f scratch/tmp.txt".to_string()];
    let DeletionCommandSet::Supported(plans) = classify_deletion_commands(&commands) else {
        panic!("expected supported deletion set");
    };
    assert_eq!(plans, vec![supported("rm -f scratch/tmp.txt")]);
}

#[test]
fn classifies_absent_deletion() {
    let commands = vec!["ls -la".to_string(), "cargo check".to_string()];
    assert_eq!(classify_deletion_commands(&commands), DeletionCommandSet::NotDeletion);
}

#[test]
fn refuses_deletion_mixed_with_other_command_text() {
    // A call can carry both `command` and `raw_command`; the executor may
    // run either. Mixing a deletion with unrelated text must fail closed.
    let commands = vec![
        "rm -f scratch/tmp.txt".to_string(),
        "curl https://example.com".to_string(),
    ];
    let DeletionCommandSet::Unsupported { reason } = classify_deletion_commands(&commands) else {
        panic!("mixed representations must be refused");
    };
    assert!(reason.contains("do not all describe"), "unexpected reason: {reason}");
}

#[test]
fn refuses_differing_supported_deletions() {
    // Both are valid shapes, but only one can be executed, so the call is
    // ambiguous and must be refused.
    let commands = vec!["rm -f scratch/a.txt".to_string(), "rm -f scratch/b.txt".to_string()];
    assert!(matches!(classify_deletion_commands(&commands), DeletionCommandSet::Unsupported { .. }));
}

#[test]
fn refuses_unsupported_shape_among_representations() {
    let commands = vec!["rm -f scratch/tmp.txt".to_string(), "rm -f *.txt".to_string()];
    assert!(matches!(classify_deletion_commands(&commands), DeletionCommandSet::Unsupported { .. }));
}

#[test]
fn rejects_unsupported_rm_shapes() {
    for command in [
        "rm -f scratch/a scratch/b && rm -f scratch/c",
        "ls && rm -f scratch/tmp.txt",
        "rm -f scratch/tmp.txt | cat",
        "rm -f scratch/tmp.txt > out.txt",
        "rm -f scratch/tmp.txt &",
        "sudo rm -f scratch/tmp.txt",
        "env FOO=1 rm -f scratch/tmp.txt",
        "FOO=1 rm -f scratch/tmp.txt",
        "bash -c 'rm -f scratch/tmp.txt'",
        "xargs rm -f",
        "rm -f *.txt",
        "rm -f scratch/*",
        "rm -f ~/tmp.txt",
        "rm -f $FILE",
        "rm -f",
        "rm -rf",
        "rm --no-preserve-root -rf scratch",
        "rm --wat scratch/tmp.txt",
        "rm -q scratch/tmp.txt",
        "\\rm scratch/tmp.txt",
        "r\\m scratch/tmp.txt",
    ] {
        let reason = unsupported(command);
        assert!(!reason.is_empty(), "reason should be user-readable for {command}");
    }
}

#[test]
fn ignores_rm_used_only_as_an_argument_in_pipelines_and_chains() {
    // `rm` in argument position is ordinary text; refusing it would block
    // harmless searches and messages.
    for command in [
        "rg rm src | head",
        "cat README.md | grep rm",
        "cargo test && echo \"do not rm\"",
        "grep rm file && ls",
    ] {
        assert_eq!(
            recognize_deletion_command(command),
            DeletionRecognition::NotDeletion,
            "unexpected recognition for {command:?}"
        );
    }
}

#[test]
fn refuses_rm_invoked_inside_a_chain_or_shell_body() {
    for command in [
        "ls && rm -f scratch/tmp.txt > out",
        "sudo rm -f scratch/tmp.txt && ls",
        "bash -c 'rm -f scratch/tmp.txt' && ls",
        "rm -f scratch/tmp.txt | cat",
    ] {
        assert!(
            matches!(recognize_deletion_command(command), DeletionRecognition::Unsupported { .. }),
            "expected unsupported for {command:?}"
        );
    }
}

#[test]
fn rejects_operand_only_dot() {
    assert!(unsupported("rm -f .").contains("current directory"));
}

#[test]
fn parse_failure_with_rm_fails_closed() {
    let recognition = recognize_deletion_command("rm -f 'unterminated");
    assert!(
        matches!(recognition, DeletionRecognition::Unsupported { .. }),
        "an unparsable `rm` must fail closed, got {recognition:?}"
    );
}

#[tokio::test]
async fn allows_existing_and_missing_workspace_targets() {
    let workspace = TempDir::new().expect("workspace");
    let existing = workspace.path().join("scratch/tmp.txt");
    std::fs::create_dir_all(existing.parent().expect("parent")).expect("mkdir");
    std::fs::write(&existing, b"data").expect("write");

    let decision = assess_deletion_targets(
        &["scratch/tmp.txt".to_string(), "scratch/missing.txt".to_string()],
        workspace.path(),
        workspace.path(),
    )
    .await;

    match decision {
        DeletionDecision::Allowed { targets } => {
            assert_eq!(targets.len(), 2);
            assert!(targets[0].starts_with(workspace.path()));
            assert!(targets[1].starts_with(workspace.path()));
        }
        other => panic!("expected allowed, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn denies_workspace_root_and_aliases() {
    let workspace = TempDir::new().expect("workspace");

    for operand in [".", "./", "sub/.."] {
        let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
        assert!(matches!(decision, DeletionDecision::Denied { .. }), "operand {operand} should be denied");
    }

    let alias = workspace.path().join("root-alias");
    std::os::unix::fs::symlink(workspace.path(), &alias).expect("symlink");
    let decision = assess_deletion_targets(&["root-alias".to_string()], workspace.path(), workspace.path()).await;
    assert!(matches!(decision, DeletionDecision::Denied { reason, .. } if reason.contains("workspace root")));
}

#[tokio::test]
async fn denies_lexical_and_absolute_escapes() {
    let workspace = TempDir::new().expect("workspace");

    for operand in ["../outside.txt", "/etc/hostname", "/"] {
        let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
        assert!(matches!(decision, DeletionDecision::Denied { .. }), "operand {operand} should be denied");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn denies_symlink_escapes_including_final_component() {
    let workspace = TempDir::new().expect("workspace");
    let outside = TempDir::new().expect("outside");
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, b"secret").expect("write");

    std::os::unix::fs::symlink(outside.path(), workspace.path().join("dir-link")).expect("dir symlink");
    std::os::unix::fs::symlink(&outside_file, workspace.path().join("file-link")).expect("file symlink");

    for operand in ["dir-link/secret.txt", "file-link", "dir-link"] {
        let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
        assert!(matches!(decision, DeletionDecision::Denied { .. }), "operand {operand} should be denied");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn allows_symlinks_that_stay_inside_the_workspace() {
    let workspace = TempDir::new().expect("workspace");
    let real = workspace.path().join("real");
    std::fs::create_dir_all(&real).expect("mkdir");
    std::fs::write(real.join("tmp.txt"), b"data").expect("write");
    std::os::unix::fs::symlink(&real, workspace.path().join("alias")).expect("symlink");

    let decision = assess_deletion_targets(&["alias/tmp.txt".to_string()], workspace.path(), workspace.path()).await;
    assert!(matches!(decision, DeletionDecision::Allowed { .. }), "got {decision:?}");
}

#[tokio::test]
async fn denies_protected_metadata_components() {
    let workspace = TempDir::new().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".git/objects")).expect("mkdir");

    for operand in [".git/config", ".git/objects/ab", "nested/.vtcode/state"] {
        let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
        let denied_for_protected_metadata = matches!(
            &decision,
            DeletionDecision::Denied { reason, .. } if reason.contains("protected metadata")
        );
        assert!(denied_for_protected_metadata, "operand {operand} should be denied, got {decision:?}");
    }
}

#[tokio::test]
async fn denies_case_variants_of_protected_metadata_names() {
    // Case-insensitive filesystems treat `.GIT` and `.git` as one directory.
    let workspace = TempDir::new().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".GIT")).expect("mkdir");

    let decision = assess_deletion_targets(&[".GIT/config".to_string()], workspace.path(), workspace.path()).await;
    assert!(
        matches!(&decision, DeletionDecision::Denied { reason, .. } if reason.contains("protected metadata")),
        "a differently cased protected directory must be denied, got {decision:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn denies_protected_metadata_reached_through_a_symlink() {
    let workspace = TempDir::new().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".git")).expect("mkdir");
    std::fs::write(workspace.path().join(".git/config"), b"data").expect("write");
    std::os::unix::fs::symlink(workspace.path().join(".git"), workspace.path().join("git-alias")).expect("symlink");

    let decision = assess_deletion_targets(&["git-alias/config".to_string()], workspace.path(), workspace.path()).await;
    assert!(
        matches!(&decision, DeletionDecision::Denied { reason, .. } if reason.contains("protected metadata")),
        "a symlink into .git must not bypass protected metadata, got {decision:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn denies_protected_metadata_when_the_workspace_path_is_symlinked() {
    // The resolved target is canonical, so it must be compared against the
    // canonical workspace root. A lexically normalized root would silently
    // skip the protected-metadata check when the workspace path itself
    // contains a symlink.
    let parent = TempDir::new().expect("parent");
    let real_workspace = parent.path().join("real");
    std::fs::create_dir_all(real_workspace.join(".git")).expect("mkdir");
    std::fs::write(real_workspace.join(".git/config"), b"data").expect("write");
    let linked_workspace = parent.path().join("link");
    std::os::unix::fs::symlink(&real_workspace, &linked_workspace).expect("workspace symlink");
    std::os::unix::fs::symlink(linked_workspace.join(".git"), linked_workspace.join("git-alias"))
        .expect("metadata symlink");

    let decision =
        assess_deletion_targets(&["git-alias/config".to_string()], &linked_workspace, &linked_workspace).await;
    assert!(
        matches!(&decision, DeletionDecision::Denied { reason, .. } if reason.contains("protected metadata")),
        "a workspace behind a symlinked parent must still protect metadata, got {decision:?}"
    );
}
