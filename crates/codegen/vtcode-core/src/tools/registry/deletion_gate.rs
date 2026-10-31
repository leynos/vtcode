//! Deletion gate for the `exec_command` family.
//!
//! Command preflight defers a supported workspace-scoped `rm` to the executor,
//! which is the only layer that knows the launch working directory and the
//! exact command text handed to the shell. This gate re-runs the shared
//! recognizer over every command representation and then assesses each
//! resolved target against the workspace.

use std::path::Path;

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::ToolRegistry;
use super::executors::exec_command::PreparedExecCommand;
use crate::tools::validation::commands::{self, DeletionCommandSet, DeletionDecision};

impl ToolRegistry {
    /// Reject unsupported deletion shapes and verify every confirmed deletion
    /// target resolves inside the workspace.
    ///
    /// Recognition covers both the verbatim payload fields and the prepared
    /// shell string, because either can become the text the shell actually
    /// runs.
    pub(super) async fn enforce_deletion_policy(
        &self,
        args: &Value,
        prepared_command: &PreparedExecCommand,
        working_dir: &Path,
        confirm: bool,
    ) -> Result<()> {
        let mut extra = Vec::new();
        if prepared_command.command == prepared_command.requested_command {
            extra.push(shell_words::join(prepared_command.requested_command.iter().map(String::as_str)));
        } else if let Some(shell_command) = prepared_command.command.last() {
            extra.push(shell_command.clone());
        }
        let candidates = commands::deletion_candidates(args, extra);

        let plans = match commands::classify_deletion_commands(&candidates) {
            DeletionCommandSet::NotDeletion => return Ok(()),
            DeletionCommandSet::Unsupported { reason } => {
                return Err(anyhow!(
                    "deletion command is not supported: {reason}. Use one simple `rm` invocation with literal \
                     workspace paths and confirm=true"
                ));
            }
            DeletionCommandSet::Supported(plans) => plans,
        };

        if !confirm {
            return Err(anyhow!("deletion via exec_command requires confirm=true"));
        }

        for plan in &plans {
            match commands::assess_deletion_targets(&plan.operands, working_dir, self.workspace_root()).await {
                DeletionDecision::Allowed { .. } => {}
                DeletionDecision::Denied { target, reason } => {
                    return Err(anyhow!("refusing to delete '{target}': {reason}"));
                }
            }
        }

        Ok(())
    }
}
