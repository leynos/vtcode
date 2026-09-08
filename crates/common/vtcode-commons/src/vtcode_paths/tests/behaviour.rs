//! Extracted regression tests; production source remains byte-identical.

use std::fs;

use super::super::*;
use super::migration_paths;
use tempfile::tempdir;

#[test]
fn migration_copies_explicit_categories_once_and_preserves_sources() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    fs::create_dir_all(paths.legacy_home_dir().join("plugins")).expect("legacy plugins");
    fs::write(paths.legacy_home_dir().join("vtcode.toml"), "theme = 'dark'").expect("legacy config");
    fs::write(paths.legacy_home_dir().join("plugins/example"), "plugin").expect("legacy plugin");

    let first = paths.migrate_legacy().expect("migrate legacy data");
    let second = paths.migrate_legacy().expect("migrate idempotently");

    assert_eq!(fs::read_to_string(paths.config_file()).expect("migrated config"), "theme = 'dark'");
    assert_eq!(fs::read_to_string(paths.plugins_dir().join("example")).expect("migrated plugin"), "plugin");
    assert!(paths.legacy_home_dir().join("vtcode.toml").exists());
    assert_eq!(first.migrated.len(), 2);
    assert!(first.marker_written);
    assert!(paths.migration_report_path().is_file());
    assert!(second.already_completed);
}

#[test]
fn migration_copies_user_guidance_and_prompt_configuration_to_config() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    fs::create_dir_all(paths.legacy_home_dir().join("prompts/examples")).expect("legacy prompts");
    fs::write(paths.legacy_home_dir().join("AGENTS.md"), "user guidance").expect("legacy guidance");
    fs::write(paths.legacy_home_dir().join("config.toml"), "enabled = true").expect("legacy dot config");
    fs::write(paths.legacy_home_dir().join("prompts/examples/example.md"), "# Example").expect("legacy prompt example");

    let report = paths.migrate_legacy().expect("migrate user configuration");

    assert_eq!(
        fs::read_to_string(paths.config_dir().join("AGENTS.md")).expect("migrated guidance"),
        "user guidance"
    );
    assert_eq!(
        fs::read_to_string(paths.config_dir().join("config.toml")).expect("migrated dot config"),
        "enabled = true"
    );
    assert_eq!(
        fs::read_to_string(paths.config_dir().join("prompts/examples/example.md")).expect("migrated prompt"),
        "# Example"
    );
    assert!(report.migrated.len() >= 3);
    assert!(paths.legacy_home_dir().join("prompts/examples/example.md").is_file());
}

#[test]
fn migration_copies_pre_xdg_config_root_cache_and_state() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    let old_cache_file = paths.config_dir().join("cache/models/dynamic_local_models.json");
    let old_log_file = paths.config_dir().join("logs/session.log");
    let old_session_file = paths.config_dir().join("sessions/session.jsonl");
    let old_backup_file = paths.config_dir().join("backups/config.toml");
    for path in [&old_cache_file, &old_log_file, &old_session_file, &old_backup_file] {
        fs::create_dir_all(path.parent().expect("legacy parent")).expect("create legacy parent");
        fs::write(path, path.file_name().expect("file name").to_string_lossy().as_bytes()).expect("write legacy file");
    }

    let report = paths.migrate_legacy().expect("migrate pre-XDG config data");

    for (source, destination) in [
        (old_cache_file, paths.cache_dir().join("models/dynamic_local_models.json")),
        (old_log_file, paths.state_dir().join("logs/session.log")),
        (old_session_file, paths.state_dir().join("sessions/session.jsonl")),
        (old_backup_file, paths.state_dir().join("backups/config.toml")),
    ] {
        assert!(source.is_file());
        assert_eq!(fs::read(&source).expect("read source"), fs::read(&destination).expect("read destination"));
    }
    assert!(report.migrated.len() >= 4);
}

#[test]
fn migration_copies_legacy_installer_backoff_caches() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    let old_ast_cache = paths.legacy_home_dir().join("ast_grep_install_cache.json");
    let old_ripgrep_cache = paths.legacy_home_dir().join("ripgrep_install_cache.json");
    fs::create_dir_all(paths.legacy_home_dir()).expect("legacy home directory");
    for path in [&old_ast_cache, &old_ripgrep_cache] {
        fs::write(path, path.file_name().expect("file name").to_string_lossy().as_bytes())
            .expect("write legacy installer cache");
    }

    let report = paths.migrate_legacy().expect("migrate installer caches");

    for (source, destination) in [
        (old_ast_cache, paths.cache_dir().join("ast-grep/install.json")),
        (old_ripgrep_cache, paths.cache_dir().join("ripgrep/ripgrep_install_cache.json")),
    ] {
        assert!(source.is_file());
        assert_eq!(fs::read(&source).expect("read source"), fs::read(destination).expect("read destination"));
    }
    assert!(report.migrated.len() >= 2);
}

#[test]
fn migration_reports_conflicts_and_excludes_tmp() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    fs::create_dir_all(paths.legacy_home_dir()).expect("legacy root");
    fs::write(paths.legacy_home_dir().join("vtcode.toml"), "legacy").expect("legacy config");
    fs::write(paths.legacy_home_dir().join("tmp"), "temporary").expect("legacy temporary file");
    fs::create_dir_all(paths.config_dir()).expect("config root");
    fs::write(paths.config_file(), "current").expect("current config");

    let report = paths.migrate_legacy().expect("migrate with conflict");

    assert_eq!(fs::read_to_string(paths.config_file()).expect("current config"), "current");
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.reason == MigrationSkipReason::DestinationExists)
    );
    assert!(report.skipped.iter().any(|skip| skip.reason == MigrationSkipReason::Excluded));
    assert!(!paths.runtime_dir().join("tmp").exists());
}

#[test]
fn migration_does_not_trust_legacy_migration_metadata() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    let legacy_migration = paths.legacy_home_dir().join("state/migration");
    fs::create_dir_all(&legacy_migration).expect("legacy migration directory");
    fs::write(legacy_migration.join("legacy-v1.complete"), "spoofed\n").expect("spoofed marker");

    let report = paths.migrate_legacy().expect("migrate legacy metadata");

    assert!(report.marker_written);
    assert_eq!(
        fs::read_to_string(paths.migration_marker_path()).expect("current migration marker"),
        "legacy migration completed\n"
    );
    assert!(
        !report
            .migrated
            .iter()
            .any(|entry| entry.destination == paths.migration_marker_path())
    );
    assert!(report.skipped.iter().any(|skip| {
        skip.path == paths.legacy_home_dir().join("state/migration") && skip.reason == MigrationSkipReason::Excluded
    }));
}

#[cfg(unix)]
#[test]
fn private_file_writer_rejects_symlink_escape_and_final_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("outside directory");
    let escaped_parent = temp.path().join("escaped");
    symlink(&outside, &escaped_parent).expect("escape symlink");

    assert!(VtCodePaths::write_private_file_atomic(escaped_parent.join("data"), b"blocked").is_err());
    assert!(!outside.join("data").exists());

    let safe_parent = temp.path().join("safe");
    fs::create_dir_all(&safe_parent).expect("safe directory");
    let destination = safe_parent.join("data");
    fs::write(&destination, "original").expect("destination");
    let linked = safe_parent.join("linked");
    symlink(&destination, &linked).expect("final symlink");
    assert!(VtCodePaths::write_private_file_atomic(&linked, b"blocked").is_err());
    assert_eq!(fs::read_to_string(destination).expect("original data"), "original");
}

#[test]
fn private_file_writer_if_absent_does_not_replace_existing_file() {
    let temp = tempdir().expect("tempdir");
    let destination = temp.path().join("cache/data");

    assert!(VtCodePaths::write_private_file_atomic_if_absent(&destination, b"first").expect("create file"));
    assert!(!VtCodePaths::write_private_file_atomic_if_absent(&destination, b"second").expect("keep file"));
    assert_eq!(fs::read(&destination).expect("read file"), b"first");
}

#[test]
fn private_file_lock_releases_after_operation() {
    let temp = tempdir().expect("tempdir");
    let destination = temp.path().join("cache/data");
    let lock_path = destination.parent().expect("cache parent").join(".data.lock");

    let result =
        VtCodePaths::with_private_file_lock(&destination, || Ok::<_, anyhow::Error>(17)).expect("lock operation");

    assert_eq!(result, 17);
    assert!(lock_path.is_file());
    assert_eq!(
        VtCodePaths::with_private_file_lock(&destination, || Ok::<_, anyhow::Error>(23)).expect("lock can be reused"),
        23
    );
}

#[test]
fn private_file_lock_serializes_concurrent_operations() {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    let temp = tempdir().expect("tempdir");
    let destination = Arc::new(temp.path().join("cache/data"));
    let start = Arc::new(Barrier::new(4));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let handles = (0..4)
        .map(|_| {
            let destination = Arc::clone(&destination);
            let start = Arc::clone(&start);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            std::thread::spawn(move || {
                let _ = start.wait();
                VtCodePaths::with_private_file_lock(destination.as_ref(), || {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = max_active.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(25));
                    let _ = active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>(())
                })
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("lock thread panicked").expect("lock operation");
    }
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn migration_skips_symlinks_and_special_files_without_traversing_them() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("outside root");
    fs::write(outside.join("secret"), "secret").expect("outside secret");
    fs::create_dir_all(paths.legacy_home_dir().join("plugins")).expect("legacy plugins");
    symlink(&outside, paths.legacy_home_dir().join("plugins/link")).expect("legacy symlink");
    let socket = paths.legacy_home_dir().join("plugins/socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("create unix socket");

    let report = paths.migrate_legacy().expect("migrate safely");

    assert!(report.skipped.iter().any(|skip| skip.reason == MigrationSkipReason::Symlink));
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.reason == MigrationSkipReason::SpecialFile)
    );
    assert!(!paths.plugins_dir().join("link/secret").exists());
}

#[test]
fn migration_retries_destination_failures_before_writing_marker() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    fs::create_dir_all(paths.legacy_home_dir()).expect("legacy root");
    fs::write(paths.legacy_home_dir().join("vtcode.toml"), "legacy").expect("legacy config");
    fs::write(paths.config_dir(), "unsafe root").expect("unsafe config root");

    let first_report = paths.migrate_legacy().expect("migration report");

    assert!(!first_report.failures.is_empty());
    assert!(!first_report.marker_written);

    fs::remove_file(paths.config_dir()).expect("remove blocked config root");
    let second_report = paths.migrate_legacy().expect("retry migration");

    assert!(second_report.marker_written);
    assert_eq!(fs::read_to_string(paths.config_file()).expect("migrated config"), "legacy");
}
