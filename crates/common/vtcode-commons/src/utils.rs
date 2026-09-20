//! Generic utility functions

use anyhow::{Context, Result};
use num_traits::ToPrimitive;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn saturating_float_to_u8(value: f32) -> u8 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    value.to_u8().unwrap_or(u8::MAX)
}

/// Get current Unix timestamp in seconds
#[inline]
pub fn current_timestamp() -> u64 {
    current_timestamp_result().unwrap_or(0)
}

/// Get current Unix timestamp in seconds as a fallible operation.
#[inline]
fn current_timestamp_result() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before UNIX_EPOCH while generating timestamp")?
        .as_secs())
}

/// Calculate the SHA256 hash of `content` and return it as a 64-character
/// lowercase hex string (the standard hex encoding of the 32-byte digest).
///
/// Use this helper whenever a caller needs a stable, ASCII-safe fingerprint
/// of arbitrary bytes - for example, hashing file contents for change
/// detection, config fingerprints, or cache keys.
pub fn calculate_sha256(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);

    for byte in digest {
        output.push(nibble_to_hex(byte >> 4));
        output.push(nibble_to_hex(byte & 0x0f));
    }

    output
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'a' + (nibble - 10)),
        _ => '?',
    }
}

/// Extract a string value from a simple TOML key assignment within the `[package]` section
pub fn extract_toml_str(content: &str, key: &str) -> Option<String> {
    // Only consider the [package] section to avoid matching other tables
    let pkg_section = content
        .find("[package]")
        .and_then(|start| content.get(start..))
        .unwrap_or(content);

    // Example target: name = "vtcode"
    let pattern = format!(r#"(?m)^\s*{}\s*=\s*"([^"]+)"\s*$"#, regex::escape(key));
    let re = Regex::new(&pattern).ok()?;
    re.captures(pkg_section)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_owned()))
}

/// Get the first meaningful section of the README/markdown as an excerpt
pub fn extract_readme_excerpt(md: &str, max_len: usize) -> String {
    // Take from start until we pass the first major sections or hit max_len
    let mut excerpt = String::with_capacity(max_len.min(md.len()));
    for line in md.lines() {
        // Stop if we reach a deep section far into the doc
        if excerpt.len() > max_len {
            break;
        }
        excerpt.push_str(line);
        excerpt.push('\n');
        // Prefer stopping after an initial overview section
        if line.trim().starts_with("## ") && excerpt.len() > (max_len / 2) {
            break;
        }
    }
    crate::formatting::truncate_byte_budget(&excerpt, max_len, "...\n")
}

/// Safe text replacement with validation
pub fn safe_replace_text(content: &str, old_str: &str, new_str: &str) -> Result<String> {
    if old_str.is_empty() {
        return Err(anyhow::anyhow!("old_string cannot be empty"));
    }

    if !content.contains(old_str) {
        return Err(anyhow::anyhow!("Text '{old_str}' not found in content"));
    }

    Ok(content.replace(old_str, new_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_excerpt_does_not_split_utf8() {
        let markdown = "你".repeat(700);

        assert_eq!(extract_readme_excerpt(&markdown, 1201), format!("{}...\n", "你".repeat(400)));
    }

    #[test]
    fn saturating_float_to_u8_matches_float_cast_edges() {
        assert_eq!(saturating_float_to_u8(-1.0), 0, "negative values saturate at zero");
        assert_eq!(saturating_float_to_u8(f32::NAN), 0, "NaN casts to zero");
        assert_eq!(saturating_float_to_u8(12.75), 12, "finite values truncate toward zero");
        assert_eq!(saturating_float_to_u8(f32::MAX), u8::MAX, "large values saturate at u8::MAX");
    }
}
