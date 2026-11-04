//! Behavioural coverage for custom OpenAI Responses providers through ACP.
//!
//! [`support`] owns the shared ACP harness; each submodule covers one
//! behavioural area so no test file outgrows the module-size budget.
//!
//! This module is declared with `#[path = "responses_tests.rs"]` from
//! `handlers.rs`, so child modules are declared with explicit paths rather than
//! relying on directory-relative resolution.

#[path = "responses_tests/support.rs"]
mod support;

#[path = "responses_tests/cancellation.rs"]
mod cancellation;
#[path = "responses_tests/deadlines.rs"]
mod deadlines;
#[path = "responses_tests/disconnect.rs"]
mod disconnect;
#[path = "responses_tests/previews.rs"]
mod previews;
#[path = "responses_tests/retry.rs"]
mod retry;
#[path = "responses_tests/tool_input_cancellation.rs"]
mod tool_input_cancellation;
#[path = "responses_tests/vidaimock.rs"]
mod vidaimock;
