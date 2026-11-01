//! Collects regression tests for custom-provider routing and request behavior.
#[path = "tests/admission.rs"]
mod admission;
#[path = "tests/command_auth.rs"]
mod command_auth;
#[path = "tests/openai_streaming.rs"]
mod openai_streaming;
#[path = "tests/profiles.rs"]
mod profiles;
#[path = "tests/rate_limit_headers.rs"]
mod rate_limit_headers;

#[path = "tests/error_metadata.rs"]
mod error_metadata;
