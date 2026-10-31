//! Bounded client metadata and legacy ACP archive migration.

use crate::acp;
use std::collections::BTreeMap;
use std::io::{self, Write};
use tracing::warn;
use vtcode_core::utils::session_archive::SessionArchiveMetadata;
pub(super) use vtcode_core::utils::session_archive::{
    MAX_SESSION_ARCHIVE_EXTENSION_BYTES, SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE,
};

pub(super) const ACP_SESSION_METADATA_NAMESPACE: &str = "dev.vtcode.acp";
pub(super) const MAX_ACP_METADATA_ENTRIES: usize = 64;

pub(super) struct BoundedAcpMeta {
    pub(super) value: Option<serde_json::Map<String, serde_json::Value>>,
    pub(super) discarded_entries: usize,
}

pub(super) fn merge_bounded_acp_meta(
    existing: Option<serde_json::Map<String, serde_json::Value>>,
    incoming: Option<acp::Meta>,
) -> BoundedAcpMeta {
    let preserve_presence = existing.is_some() || incoming.is_some();
    let mut merged = existing.unwrap_or_default().into_iter().collect::<BTreeMap<_, _>>();
    merged.extend(incoming.unwrap_or_default());

    let mut bounded = serde_json::Map::new();
    let mut discarded_entries = 0usize;
    for (key, value) in merged {
        if bounded.len() >= MAX_ACP_METADATA_ENTRIES {
            discarded_entries = discarded_entries.saturating_add(1);
            continue;
        }
        drop(bounded.insert(key.clone(), value));
        if serialized_acp_meta_size_at_most(&bounded, MAX_SESSION_ARCHIVE_EXTENSION_BYTES).is_none() {
            drop(bounded.remove(&key));
            discarded_entries = discarded_entries.saturating_add(1);
        }
    }

    BoundedAcpMeta {
        value: preserve_presence.then_some(bounded),
        discarded_entries,
    }
}

pub(super) fn migrate_legacy_acp_meta(metadata: &mut SessionArchiveMetadata) -> bool {
    // Older ACP archives used protocol-named top-level metadata fields. Core
    // preserves them generically; this boundary migrates only ACP-owned keys.
    let Some(mut legacy_fields) = metadata
        .extension(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE)
        .and_then(serde_json::Value::as_object)
        .cloned()
    else {
        return false;
    };

    let mut legacy_acp_meta = serde_json::Map::new();
    let mut had_legacy_acp_meta = false;
    for legacy_key in ["acp_meta", "client_metadata"] {
        let Some(value) = legacy_fields.get(legacy_key).and_then(serde_json::Value::as_object).cloned() else {
            continue;
        };
        had_legacy_acp_meta = true;
        drop(legacy_fields.remove(legacy_key));
        legacy_acp_meta.extend(value);
    }

    if !had_legacy_acp_meta {
        return false;
    }

    if legacy_fields.is_empty() {
        drop(metadata.remove_extension(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE));
    } else {
        metadata.set_extension(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE, serde_json::Value::Object(legacy_fields));
    }

    let current_acp_meta = metadata
        .extension(ACP_SESSION_METADATA_NAMESPACE)
        .and_then(|value| value.as_object());
    let incoming = current_acp_meta.cloned().map(acp::Meta::from_iter);
    let merged = merge_bounded_acp_meta(Some(legacy_acp_meta), incoming);
    if let Some(value) = merged.value {
        metadata.set_extension(ACP_SESSION_METADATA_NAMESPACE, serde_json::Value::Object(value));
    }
    if merged.discarded_entries > 0 {
        warn!(
            discarded_entries = merged.discarded_entries,
            max_entries = MAX_ACP_METADATA_ENTRIES,
            max_serialized_bytes = MAX_SESSION_ARCHIVE_EXTENSION_BYTES,
            "Discarded legacy ACP metadata entries beyond the archive budget"
        );
    }
    true
}

fn serialized_acp_meta_size_at_most(
    metadata: &serde_json::Map<String, serde_json::Value>,
    max_bytes: usize,
) -> Option<usize> {
    let mut writer = BoundedMetadataSizeWriter { bytes_written: 0, max_bytes };
    serde_json::to_writer(&mut writer, metadata).ok()?;
    Some(writer.bytes_written)
}

pub(super) struct BoundedMetadataSizeWriter {
    pub(super) bytes_written: usize,
    pub(super) max_bytes: usize,
}

impl Write for BoundedMetadataSizeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(new_size) = self.bytes_written.checked_add(bytes.len()) else {
            return Err(io::Error::other("serialized ACP metadata size overflowed"));
        };
        if new_size > self.max_bytes {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "serialized ACP metadata exceeded its size limit"));
        }
        self.bytes_written = new_size;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
