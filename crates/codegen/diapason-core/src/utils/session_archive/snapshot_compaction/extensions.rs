//! Size-bounded handling for namespaced session archive extensions.

use super::super::SessionArchiveExtensions;
use super::super::{MAX_SESSION_ARCHIVE_EXTENSION_BYTES, MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES};
use serde::Serialize;

pub(in crate::utils) fn bound_session_archive_extensions(extensions: &mut Option<SessionArchiveExtensions>) -> bool {
    let Some(original) = extensions.take() else {
        return false;
    };

    let original_namespaces = original.len();
    let mut bounded = SessionArchiveExtensions::new();
    let mut changed = original_namespaces > MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES;

    for (index, (namespace, value)) in original.iter().enumerate() {
        if index >= MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES {
            changed = true;
            break;
        }

        drop(bounded.insert(namespace.clone(), value.clone()));
        if serialized_json_size_at_most(&bounded, MAX_SESSION_ARCHIVE_EXTENSION_BYTES).is_none() {
            drop(bounded.remove(namespace));
            changed = true;
        }
    }

    let retained_namespaces = bounded.len();
    let serialized_bytes = serialized_json_size_at_most(&bounded, MAX_SESSION_ARCHIVE_EXTENSION_BYTES).unwrap_or(0);
    if bounded.is_empty() {
        *extensions = (original_namespaces == 0).then_some(bounded);
    } else {
        *extensions = Some(bounded);
    }

    if changed && original_namespaces > retained_namespaces {
        tracing::warn!(
            original_namespaces,
            retained_namespaces,
            discarded_namespaces = original_namespaces.saturating_sub(retained_namespaces),
            serialized_bytes,
            max_namespaces = MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES,
            max_bytes = MAX_SESSION_ARCHIVE_EXTENSION_BYTES,
            "Truncated session archive extensions to configured limits"
        );
    }

    changed
}

pub(super) fn shrink_session_archive_extensions(extensions: &mut Option<SessionArchiveExtensions>) -> bool {
    let Some(namespaces) = extensions.as_mut() else {
        return false;
    };
    let Some(last_namespace) = namespaces.iter().last().map(|(namespace, _)| namespace.clone()) else {
        *extensions = None;
        return true;
    };

    drop(namespaces.remove(&last_namespace));
    if namespaces.is_empty() {
        *extensions = None;
    }
    true
}

pub(in crate::utils) fn serialized_json_size_at_most<T: Serialize>(value: &T, max_bytes: usize) -> Option<usize> {
    struct LimitedWriter {
        bytes_written: usize,
        max_bytes: usize,
    }

    impl std::io::Write for LimitedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let Some(new_size) = self.bytes_written.checked_add(bytes.len()) else {
                return Err(std::io::Error::other("serialized JSON size overflowed"));
            };
            if new_size > self.max_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "serialized JSON exceeded its size limit",
                ));
            }
            self.bytes_written = new_size;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LimitedWriter { bytes_written: 0, max_bytes };
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.bytes_written)
}
