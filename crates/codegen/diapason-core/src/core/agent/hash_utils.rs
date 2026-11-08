//! Hash utilities for tool catalogue and system prompt caching.
//!
//! Provides hashing functions for tool definitions, system prompts, and
//! low-signal attempt deduplication keys.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::Serialize;
use serde_json::Value;

use crate::llm::provider::ToolDefinition;

/// Hash a value using the default hasher.
pub fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Hash a serializable value as JSON.
pub fn hash_json_value<T: Serialize + ?Sized>(value: &T) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    serde_json::to_writer(HasherWriter::new(&mut hasher), value).ok().map(|_| {
        hasher.write_u8(0xff);
        hasher.finish()
    })
}

/// Hash tool definitions for cache key computation.
pub fn hash_tool_definitions(tools: Option<&[ToolDefinition]>) -> Option<u64> {
    tools.and_then(hash_json_value)
}

/// Compute a stable hash of the system prompt prefix.
///
/// Strips runtime sections (tool catalogue, context, active tools) so the hash
/// remains stable across turns even as runtime context changes.
pub fn stable_system_prefix_hash(system_prompt: &str) -> u64 {
    let stable_prefix = system_prompt
        .split("\n## Active Tools\n")
        .next()
        .unwrap_or(system_prompt)
        .split("\n[Runtime Tool Catalogue]\n")
        .next()
        .unwrap_or(system_prompt)
        // Prompts supplied by embedders may still carry the pre-rename section.
        .split("\n[Runtime Tool Catalog]\n")
        .next()
        .unwrap_or(system_prompt)
        .split("\n[Runtime Context]\n")
        .next()
        .unwrap_or(system_prompt)
        .split("\n[Context]\n")
        .next()
        .unwrap_or(system_prompt)
        .trim_end();
    hash_value(&stable_prefix)
}

/// Generate a deduplication key for low-signal tool attempts.
pub fn low_signal_attempt_key(name: &str, args: &Value) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut input_len = 0usize;
    if serde_json::to_writer(HashingWriter::new(&mut hash, &mut input_len), args).is_err() {
        for byte in b"{}" {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
            input_len = input_len.saturating_add(1);
        }
    }

    format!("{name}:len{input_len}-fnv{hash:016x}")
}

struct HashingWriter<'a> {
    hash: &'a mut u64,
    input_len: &'a mut usize,
}

impl<'a> HashingWriter<'a> {
    fn new(hash: &'a mut u64, input_len: &'a mut usize) -> Self {
        Self { hash, input_len }
    }
}

impl std::io::Write for HashingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for byte in buf {
            *self.hash ^= u64::from(*byte);
            *self.hash = self.hash.wrapping_mul(0x100000001b3);
            *self.input_len = self.input_len.saturating_add(1);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct HasherWriter<'a, H> {
    hasher: &'a mut H,
}

impl<'a, H> HasherWriter<'a, H> {
    fn new(hasher: &'a mut H) -> Self {
        Self { hasher }
    }
}

impl<H: Hasher> std::io::Write for HasherWriter<'_, H> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Tests for the stable system-prefix hash contract.
///
/// The hash must ignore runtime catalogue metadata, including the pre-rename
/// section, while still changing when the stable prompt prefix changes.
#[cfg(test)]
mod tests {
    use super::stable_system_prefix_hash;

    #[test]
    fn stable_prefix_hash_ignores_runtime_catalogue_metadata() {
        let earlier = "Stable guidance.\n[Runtime Tool Catalogue]\n- version: 1\n- epoch: 2\n";
        let later = "Stable guidance.\n[Runtime Tool Catalogue]\n- version: 9\n- epoch: 42\n";

        assert_eq!(stable_system_prefix_hash(earlier), stable_system_prefix_hash(later));
    }

    #[test]
    fn stable_prefix_hash_ignores_legacy_runtime_catalogue_metadata() {
        let earlier = "Stable guidance.\n[Runtime Tool Catalog]\n- version: 1\n";
        let later = "Stable guidance.\n[Runtime Tool Catalog]\n- version: 2\n";

        assert_eq!(stable_system_prefix_hash(earlier), stable_system_prefix_hash(later));
    }

    #[test]
    fn stable_prefix_hash_changes_with_the_stable_prefix() {
        assert_ne!(
            stable_system_prefix_hash("Stable guidance for a reviewer."),
            stable_system_prefix_hash("Stable guidance for an implementer.")
        );
    }

    #[test]
    fn stable_prefix_hash_ignores_appended_catalogue_metadata() {
        let prefix = "Stable guidance.";
        let with_metadata = "Stable guidance.\n[Runtime Tool Catalogue]\n- version: 3\n- epoch: 4\n";

        assert_eq!(stable_system_prefix_hash(prefix), stable_system_prefix_hash(with_metadata));
    }

    #[test]
    fn stable_prefix_hash_distinguishes_prefixes_with_catalogue_header() {
        let reviewer = "Stable guidance for a reviewer.\n[Runtime Tool Catalogue]\n- version: 1\n";
        let implementer = "Stable guidance for an implementer.\n[Runtime Tool Catalogue]\n- version: 1\n";

        assert_ne!(stable_system_prefix_hash(reviewer), stable_system_prefix_hash(implementer));
    }
}
