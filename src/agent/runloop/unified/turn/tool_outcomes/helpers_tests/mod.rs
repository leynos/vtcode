//! Test-module imports and child-module wiring for tool-outcome helpers.

use serde_json::json;
use vtcode_core::config::constants::tools;

use super::*;

mod activity_tracking;
mod extent_coverage;
mod history_deduplication;
mod history_replay;
mod history_updates;
mod low_signal_tracking;
mod signature_normalization;
mod verification_pressure;
mod verification_recovery;
