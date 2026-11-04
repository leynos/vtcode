//! Re-export of [`diapason_commons::message_metadata`].
//!
//! The canonical definition lives in `diapason-commons` so that `diapason-llm` can
//! embed `MessageMetadata` directly in its `Message` type without depending on
//! `diapason-core`. This module preserves the historical
//! `crate::core::message_metadata` path used throughout the codebase.

pub use diapason_commons::message_metadata::{CompressionStatus, MessageMetadata};
