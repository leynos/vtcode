//! Pins the background-ledger merge contract.
//!
//! A save joins another owner's records instead of replacing them, keeps the
//! newest copy of a record both sides hold, and honours cancellation and
//! restart precedence, so concurrent controllers cannot discard each other's
//! state.

mod adoption_tests;
mod merge_oracle_tests;
mod reconciliation_tests;
