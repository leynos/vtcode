use super::*;

#[test]
fn session_archive_retention_prunes_oldest_by_count() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    for idx in 0..3 {
        let path = temp_dir.path().join(format!("session-{idx}.json"));
        fs::write(&path, format!("{{\"idx\":{idx}}}"))
            .with_context(|| format!("failed to write {}", path.display()))?;
        std::thread::sleep(Duration::from_millis(5));
    }

    apply_session_retention_with_limits(
        temp_dir.path(),
        SessionRetentionLimits {
            max_files: 2,
            max_age_days: 365,
            max_total_size_bytes: 10 * BYTES_PER_MB,
        },
    )?;

    let mut remaining = fs::read_dir(temp_dir.path())
        .context("failed to list retained session files")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|path| path.file_name().and_then(|name| name.to_str()).map(|name| name.to_string()))
        .collect::<Vec<_>>();
    remaining.sort();

    assert_eq!(remaining.len(), 2);
    assert!(!remaining.iter().any(|name| name == "session-0.json"));
    Ok(())
}

#[test]
fn session_archive_retention_prunes_by_total_size() -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    for idx in 0..2 {
        let path = temp_dir.path().join(format!("session-{idx}.json"));
        fs::write(&path, "x".repeat(800_000)).with_context(|| format!("failed to write {}", path.display()))?;
        std::thread::sleep(Duration::from_millis(5));
    }

    apply_session_retention_with_limits(
        temp_dir.path(),
        SessionRetentionLimits {
            max_files: 10,
            max_age_days: 365,
            max_total_size_bytes: BYTES_PER_MB,
        },
    )?;

    let remaining = fs::read_dir(temp_dir.path())
        .context("failed to list retained session files")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();

    assert_eq!(remaining.len(), 1);
    let remaining_name = remaining[0].file_name().and_then(|name| name.to_str()).unwrap_or_default();
    assert_eq!(remaining_name, "session-1.json");
    Ok(())
}
