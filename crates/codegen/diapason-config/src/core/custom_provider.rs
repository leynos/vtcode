//! Defines custom provider profiles, request policies, and their serialized configuration.
mod api_format;
mod auth;
mod profile;
mod provider;
mod request_policy;

pub use api_format::CustomProviderApiFormat;
pub use auth::CustomProviderCommandAuthConfig;
pub use profile::{CustomProviderProfileConfig, ResolvedCustomProviderProfile};
pub use provider::CustomProviderConfig;
pub use request_policy::CustomProviderRequestPolicyConfig;

#[cfg(test)]
mod tests;
