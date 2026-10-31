//! Recognition and path assessment for workspace-scoped `rm` deletion.
//!
//! The execution policy layer intentionally keeps *authorization* separate
//! from *recognition*: this module answers only whether a command is one
//! supported `rm` invocation and whether every operand stays inside the
//! workspace. Confirmation, approvals, configured deny rules, and sandbox
//! enforcement remain the responsibility of the caller.
//!
//! Recognition is deliberately strict and fails closed; see
//! [`recognition`] for the accepted shapes. Target assessment reuses the
//! workspace containment helpers; see [`assessment`].

mod assessment;
mod recognition;

#[cfg(test)]
mod tests;

pub use assessment::{DeletionDecision, assess_deletion_targets};
pub use recognition::{
    DeletionCommandSet, DeletionPlan, DeletionRecognition, classify_deletion_commands, recognize_deletion_command,
};
