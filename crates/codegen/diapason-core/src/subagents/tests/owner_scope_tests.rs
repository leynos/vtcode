//! Owner-scoped background records: persisted round-trips, per-owner
//! isolation of the shared ledger, and owner-qualified record ids.
//!
//! The cases split by concern: `ledger` covers persistence and the shared
//! workspace ledger, `refresh_launch` covers refresh adoption and managed
//! process launch, and `support` holds the fixtures both share.
use super::*;

mod ledger;
mod refresh_launch;
mod support;
