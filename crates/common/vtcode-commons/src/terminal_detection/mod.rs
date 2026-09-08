//! Detects terminal emulators and exposes their supported configuration surface.

mod detection;
mod paths;
#[cfg(test)]
mod tests;
mod types;

pub use detection::is_ghostty_terminal;
pub use types::{TerminalFeature, TerminalSetupAvailability, TerminalType};
