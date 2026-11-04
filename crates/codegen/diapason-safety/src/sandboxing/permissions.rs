//! Sandbox permissions for fine-grained access control.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Fine-grained permissions for sandbox operations.
///
/// These permissions allow individual tool calls to request specific
/// capabilities beyond the base sandbox policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    /// Use the default sandbox permissions from the policy.
    #[default]
    UseDefault,

    /// Request escalated permissions (requires approval).
    RequireEscalated,

    /// Request additional per-command sandbox permissions.
    WithAdditionalPermissions,

    /// Bypass the sandbox entirely (requires explicit approval).
    BypassSandbox,
}

impl SandboxPermissions {
    /// Normalize an implicit additional-permission request to its explicit
    /// sandboxed wire mode. Escalated and bypass modes are left unchanged so
    /// their conflict checks remain fail-closed at the execution boundary.
    #[must_use]
    pub fn normalized_for(self, additional_permissions: Option<&AdditionalPermissions>) -> Self {
        if self == Self::UseDefault && additional_permissions.is_some_and(|permissions| !permissions.is_empty()) {
            Self::WithAdditionalPermissions
        } else {
            self
        }
    }

    /// Check if this permission requires approval.
    fn requires_approval(&self) -> bool {
        matches!(self, Self::RequireEscalated | Self::WithAdditionalPermissions | Self::BypassSandbox)
    }

    /// Check if this permission requests full unsandboxed execution.
    pub fn requires_escalated_permissions(&self) -> bool {
        matches!(self, Self::RequireEscalated | Self::BypassSandbox)
    }

    /// Check if this permission requests any additional privileges.
    fn requires_additional_permissions(&self) -> bool {
        !matches!(self, Self::UseDefault)
    }

    /// Check if this permission requests additional sandboxed permissions.
    pub fn uses_additional_permissions(&self) -> bool {
        matches!(self, Self::WithAdditionalPermissions)
    }

    /// Check if this permission bypasses the sandbox.
    fn bypasses_sandbox(&self) -> bool {
        matches!(self, Self::BypassSandbox)
    }

    /// Merge with another permission, taking the more permissive one.
    fn merge(&self, other: &Self) -> Self {
        use SandboxPermissions::*;
        match (self, other) {
            (BypassSandbox, _) | (_, BypassSandbox) => BypassSandbox,
            (RequireEscalated, _) | (_, RequireEscalated) => RequireEscalated,
            (WithAdditionalPermissions, _) | (_, WithAdditionalPermissions) => WithAdditionalPermissions,
            (UseDefault, UseDefault) => UseDefault,
        }
    }
}

/// Additional per-command filesystem permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AdditionalPermissions {
    /// Additional filesystem paths to grant read access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fs_read: Vec<PathBuf>,
    /// Additional filesystem paths to grant write access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fs_write: Vec<PathBuf>,
}

impl AdditionalPermissions {
    pub fn is_empty(&self) -> bool {
        self.fs_read.is_empty() && self.fs_write.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_permissions() {
        let perm = SandboxPermissions::default();
        assert!(!perm.requires_approval());
        assert!(!perm.bypasses_sandbox());
    }

    #[test]
    fn test_escalated_permissions() {
        let perm = SandboxPermissions::RequireEscalated;
        assert!(perm.requires_approval());
        assert!(!perm.bypasses_sandbox());
        assert!(perm.requires_escalated_permissions());
    }

    #[test]
    fn test_bypass_permissions() {
        let perm = SandboxPermissions::BypassSandbox;
        assert!(perm.requires_approval());
        assert!(perm.bypasses_sandbox());
        assert!(perm.requires_escalated_permissions());
    }

    #[test]
    fn test_with_additional_permissions() {
        let perm = SandboxPermissions::WithAdditionalPermissions;
        assert!(perm.requires_approval());
        assert!(perm.requires_additional_permissions());
        assert!(perm.uses_additional_permissions());
        assert!(!perm.requires_escalated_permissions());
        assert!(!perm.bypasses_sandbox());
    }

    #[test]
    fn default_normalizes_for_non_empty_additional_permissions() {
        let additional = AdditionalPermissions {
            fs_read: vec![PathBuf::from("/tmp/reference")],
            fs_write: Vec::new(),
        };

        assert_eq!(
            SandboxPermissions::UseDefault.normalized_for(Some(&additional)),
            SandboxPermissions::WithAdditionalPermissions
        );
        assert_eq!(
            SandboxPermissions::RequireEscalated.normalized_for(Some(&additional)),
            SandboxPermissions::RequireEscalated
        );
        assert_eq!(
            SandboxPermissions::UseDefault.normalized_for(Some(&AdditionalPermissions::default())),
            SandboxPermissions::UseDefault
        );
    }

    #[test]
    fn test_merge_permissions() {
        use SandboxPermissions::*;

        assert_eq!(UseDefault.merge(&UseDefault), UseDefault);
        assert_eq!(UseDefault.merge(&WithAdditionalPermissions), WithAdditionalPermissions);
        assert_eq!(UseDefault.merge(&RequireEscalated), RequireEscalated);
        assert_eq!(RequireEscalated.merge(&BypassSandbox), BypassSandbox);
    }
}
