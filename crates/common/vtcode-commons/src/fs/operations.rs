//! File utility functions for common operations

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Ensure a directory exists, creating it if necessary
/// # Errors
/// Returns an error when the requested filesystem operation or serialization fails.
pub async fn ensure_dir_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)
            .await
            .with_context(|| format!("Failed to create directory: {}", path.display()))?;
    }
    Ok(())
}

/// Read a file with contextual error message
pub async fn read_file_with_context(path: &Path, context: &str) -> Result<String> {
    fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {}: {}", context, path.display()))
}

/// Write a file with contextual error message, ensuring parent directory exists
pub async fn write_file_with_context(path: &Path, content: &str, context: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_exists(parent).await?;
    }
    fs::write(path, content)
        .await
        .with_context(|| format!("Failed to write {}: {}", context, path.display()))
}

/// Write a file atomically with a contextual error message, ensuring the
/// parent directory exists.
///
/// The content is first written to a temporary file created in the same
/// directory as `path` (so the final rename stays on the same filesystem and
/// is therefore atomic), then the temp file is renamed onto `path`. This
/// prevents concurrent readers -- e.g. another vtcode process sharing the
/// same workspace -- from ever observing a partially written file.
///
/// On rename failure the temp file is best-effort removed before returning
/// the error.
pub async fn write_file_atomic_with_context(path: &Path, content: &str, context: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_exists(parent).await?;
    }

    let temp_path = atomic_temp_path(path);

    fs::write(&temp_path, content)
        .await
        .with_context(|| format!("Failed to write {}: {}", context, temp_path.display()))?;

    if let Err(err) = fs::rename(&temp_path, path).await {
        let _ = fs::remove_file(&temp_path).await;
        return Err(err).with_context(|| format!("Failed to write {}: {}", context, path.display()));
    }

    Ok(())
}

/// Build a unique temp file path in the same directory as `path`, suitable
/// for a write-then-rename atomic publish of `path`.
fn atomic_temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("vtcode-atomic-write");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);

    dir.join(format!(".{file_name}.tmp-{}-{nanos:x}-{counter:x}", std::process::id()))
}

/// Write a JSON file
pub async fn write_json_file<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(data)
        .with_context(|| format!("Failed to serialize data for {}", path.display()))?;

    write_file_with_context(path, &json, "JSON data").await
}

/// Read a category-owned private file without following symlinks.
pub async fn read_private_file_no_follow(path: &Path) -> Result<Vec<u8>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::VtCodePaths::read_file_no_follow(&path))
        .await
        .context("private file read task panicked")?
}

/// Create a category-owned private file without following a final symlink.
pub async fn create_private_file(path: &Path) -> Result<std::fs::File> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::VtCodePaths::create_private_file(&path))
        .await
        .context("private file creation task panicked")?
}

/// Atomically write a category-owned private file without following symlinks.
pub async fn write_private_file_atomic(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.to_path_buf();
    let contents = contents.as_ref().to_vec();
    tokio::task::spawn_blocking(move || crate::VtCodePaths::write_private_file_atomic(&path, &contents))
        .await
        .context("private file write task panicked")?
}

/// Atomically create a category-owned private file when the destination is absent.
///
/// Returns `true` when this call published the file and `false` when another
/// writer had already created it.
pub async fn write_private_file_atomic_if_absent(path: &Path, contents: impl AsRef<[u8]>) -> Result<bool> {
    let path = path.to_path_buf();
    let contents = contents.as_ref().to_vec();
    tokio::task::spawn_blocking(move || crate::VtCodePaths::write_private_file_atomic_if_absent(&path, &contents))
        .await
        .context("private file create task panicked")?
}

/// Run a blocking operation while holding an exclusive private file lock.
pub async fn with_private_file_lock<T, F>(path: &Path, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::VtCodePaths::with_private_file_lock(&path, operation))
        .await
        .context("private file lock task panicked")?
}

/// Read and deserialize a category-owned private JSON file.
pub async fn read_private_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let contents = read_private_file_no_follow(path).await?;
    serde_json::from_slice(&contents).with_context(|| format!("Failed to parse private JSON from {}", path.display()))
}

/// Serialize and atomically write a category-owned private JSON file.
pub async fn write_private_json_file<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(data)
        .with_context(|| format!("Failed to serialize private JSON for {}", path.display()))?;
    write_private_file_atomic(path, json).await
}

/// Read and parse a JSON file
pub async fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content = read_file_with_context(path, "JSON file").await?;

    serde_json::from_str(&content).with_context(|| format!("Failed to parse JSON from {}", path.display()))
}

/// Parse JSON with context for better error messages
pub fn parse_json_with_context<T: for<'de> Deserialize<'de>>(content: &str, context: &str) -> Result<T> {
    serde_json::from_str(content).with_context(|| format!("Failed to parse JSON from {context}"))
}

/// Serialize JSON with context
pub fn serialize_json_with_context<T: Serialize>(data: &T, context: &str) -> Result<String> {
    serde_json::to_string(data).with_context(|| format!("Failed to serialize JSON for {context}"))
}

/// Serialize JSON pretty with context
pub fn serialize_json_pretty_with_context<T: Serialize>(data: &T, context: &str) -> Result<String> {
    serde_json::to_string_pretty(data).with_context(|| format!("Failed to pretty-serialize JSON for {context}"))
}

/// Parse JSON into a typed value, returning `None` on failure.
///
/// Intended for non-critical, best-effort parsing where a missing or malformed
/// value should be silently ignored. Use `parse_json_with_context` when the
/// caller needs an actionable error.
#[inline]
#[must_use]
pub fn try_parse_json<T: for<'de> Deserialize<'de>>(input: &str) -> Option<T> {
    serde_json::from_str(input).ok()
}

/// Parse JSON into an untyped `Value`, returning `None` on failure.
///
/// Same semantics as `try_parse_json` but avoids a type annotation at the call
/// site when only dynamic inspection is needed.
#[inline]
#[must_use]
pub fn try_parse_json_value(input: &str) -> Option<serde_json::Value> {
    serde_json::from_str(input).ok()
}

/// Parse JSON into a typed value, falling back to `Default` on failure.
///
/// A parse failure is logged at `debug` level with the provided `label` so the
/// failure is visible in traces without being fatal.
#[inline]
pub fn parse_json_or_default<T: for<'de> Deserialize<'de> + Default>(input: &str, label: &str) -> T {
    serde_json::from_str(input).unwrap_or_else(|err| {
        tracing::debug!(label, %err, "JSON parse failed, using default");
        T::default()
    })
}

/// Canonicalize path with context.
///
/// Uses [`crate::paths::canonicalize`] (backed by `dunce`) to avoid Windows
/// `\\?\` verbatim prefixes from `std::fs::canonicalize`.
pub fn canonicalize_with_context(path: &Path, context: &str) -> Result<PathBuf> {
    crate::paths::canonicalize(path)
        .with_context(|| format!("Failed to canonicalize {} path: {}", context, path.display()))
}

/// Canonicalize path with context (async).
///
/// `dunce::canonicalize` is a synchronous syscall; we wrap it in
/// `spawn_blocking` to preserve the async interface without blocking the
/// runtime, matching the behaviour of `tokio::fs::canonicalize`.
pub async fn canonicalize_with_context_async(path: &Path, context: &str) -> Result<PathBuf> {
    let path = path.to_path_buf();
    let path_display = path.display().to_string();
    // `?` coerces JoinError → anyhow::Error via the blanket From impl.
    let result = tokio::task::spawn_blocking(move || crate::paths::canonicalize(&path)).await?;
    result.with_context(|| format!("Failed to canonicalize {context} path: {path_display}"))
}

/// Read a file to string with contextual error (async)
pub async fn read_to_string_async(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))
}

/// Write a file with contextual error (async)
pub async fn write_async(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    fs::write(path, contents)
        .await
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Create directories recursively with contextual error (async)
pub async fn create_dir_all_async(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .await
        .with_context(|| format!("Failed to create {}", path.display()))
}

/// Remove a file with contextual error (async)
pub async fn remove_file_async(path: &Path) -> Result<()> {
    fs::remove_file(path)
        .await
        .with_context(|| format!("Failed to remove {}", path.display()))
}

/// Rename a file with contextual error (async)
pub async fn rename_async(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)
        .await
        .with_context(|| format!("Failed to rename {} to {}", from.display(), to.display()))
}
