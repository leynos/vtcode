//! Re-export the shared configuration loader implementation from the
//! `diapason-config` crate so downstream consumers can continue importing it
//! through `diapason_core::config::loader` while the logic lives in the
//! dedicated configuration crate.
pub use diapason_config::loader::*;
