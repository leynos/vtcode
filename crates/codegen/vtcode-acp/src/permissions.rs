//! Public permission API facade for ACP client prompts and tool authorization.

mod prompter;
#[cfg(test)]
mod tests;

pub use prompter::{AcpPermissionPrompter, DefaultPermissionPrompter, PermissionToolContext};
