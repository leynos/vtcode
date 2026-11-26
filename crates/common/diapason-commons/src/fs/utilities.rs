//! Synchronous filesystem and path-string utilities.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::image::has_supported_image_extension;

/// Ensure a directory exists (sync)
///
/// # Errors
///
/// Returns an error if `create_dir_all` fails to create the directory.
pub fn ensure_dir_exists_sync(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path).with_context(|| format!("Failed to create directory: {}", path.display()))?;
    }
    Ok(())
}

/// Read a file with contextual error message (sync)
///
/// # Errors
///
/// Returns an error if the file cannot be read, for example because it is
/// missing, the caller lacks permission, or its contents are not valid UTF-8.
/// The message names the supplied `context` and the path.
pub fn read_file_with_context_sync(path: &Path, context: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("Failed to read {}: {}", context, path.display()))
}

/// Write a file with contextual error message (sync)
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, or if the
/// contents cannot be written to the path.
pub fn write_file_with_context_sync(path: &Path, payload: &str, context: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_exists_sync(parent)?;
    }
    std::fs::write(path, payload).with_context(|| format!("Failed to write {}: {}", context, path.display()))
}

/// Write a JSON file (sync)
///
/// # Errors
///
/// Returns an error if `data` cannot be serialized to pretty-printed JSON, or
/// if the resulting document cannot be written.
pub fn write_json_file_sync<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(data)
        .with_context(|| format!("Failed to serialize data for {}", path.display()))?;

    write_file_with_context_sync(path, &json, "JSON data")
}

/// Read and parse a JSON file (sync)
///
/// # Errors
///
/// Returns an error if the file cannot be read, or if its contents are not
/// valid JSON for `T`.
pub fn read_json_file_sync<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content = read_file_with_context_sync(path, "JSON file")?;

    serde_json::from_str(&content).with_context(|| format!("Failed to parse JSON from {}", path.display()))
}

/// Check whether a path looks like an image file based on extension.
pub fn is_image_path(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };

    matches!(extension, "bmp" | "gif" | "jpeg" | "jpg" | "png" | "svg" | "tif" | "tiff" | "webp")
}

/// Check whether a string is a Windows absolute path (e.g., `C:\...` or `C:/...`).
pub fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() > 2
        && bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), Some(b'\\' | b'/'))
}

/// Remove backslash-escaped whitespace from a token.
///
/// A backslash followed by an ASCII whitespace character is replaced by the
/// whitespace character itself.  All other characters are passed through.
pub fn unescape_whitespace(token: &str) -> String {
    let mut result = String::with_capacity(token.len());
    let mut chars = token.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.peek()
            && next.is_ascii_whitespace()
        {
            result.push(*next);
            let _ = chars.next();
            continue;
        }
        result.push(ch);
    }
    result
}

/// Trim trailing text from a raw image path match.
///
/// When a regex greedily matches an image path that contains spaces, it may
/// also consume trailing prose (e.g., "/path/to/image.png can you see").
/// This function walks backwards through whitespace-delimited tokens to find
/// the longest prefix that looks like a valid image path.
///
/// The `candidate_check` closure receives a trimmed candidate string and
/// returns `true` if it should be accepted as a valid image path.
pub fn trim_trailing_image_path<F>(raw: &str, candidate_check: F) -> &str
where
    F: Fn(&str) -> bool,
{
    if candidate_check(raw) {
        return raw;
    }
    let mut candidate = raw.trim_end();
    while let Some(last_space) = candidate.rfind(' ') {
        candidate = candidate.get(..last_space).unwrap_or_default();
        if candidate_check(candidate) {
            return candidate;
        }
    }
    raw
}

/// Resolve an image-path candidate after unescaping whitespace and expanding
/// the `file://` scheme and `~/` home prefix.
///
/// Returns `None` when the candidate is home-relative but no home directory can
/// be determined.
fn resolve_image_path_candidate(candidate: &str) -> Option<PathBuf> {
    let unescaped = unescape_whitespace(candidate);
    let path_str = unescaped.strip_prefix("file://").unwrap_or(unescaped.as_str());
    match path_str.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|home| home.join(rest)),
        None => Some(PathBuf::from(path_str)),
    }
}

/// Convenience wrapper for [`trim_trailing_image_path`] that checks
/// image file extensions via [`has_supported_image_extension`].
///
/// Handles `file://` scheme and `~/` home expansion before checking.
pub fn trim_trailing_image_path_str(raw: &str) -> &str {
    trim_trailing_image_path(raw, |candidate| {
        resolve_image_path_candidate(candidate).is_some_and(|path| has_supported_image_extension(&path))
    })
}

#[cfg(test)]
mod tests {
    use super::trim_trailing_image_path_str;

    #[test]
    fn trim_trailing_image_path_str_stops_before_trailing_prose() {
        let raw = "/tmp/capture.png can you see this";
        assert_eq!(trim_trailing_image_path_str(raw), "/tmp/capture.png");
    }

    #[test]
    fn trim_trailing_image_path_str_leaves_unsupported_extension_untouched() {
        let raw = "/tmp/notes.txt can you see this";
        assert_eq!(trim_trailing_image_path_str(raw), raw);
    }

    #[test]
    fn trim_trailing_image_path_str_accepts_path_without_spaces() {
        let raw = "/tmp/capture.png";
        assert_eq!(trim_trailing_image_path_str(raw), raw);
    }
}
