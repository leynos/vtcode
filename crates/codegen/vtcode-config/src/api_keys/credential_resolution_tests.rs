//! Unit tests for credential-resolution precedence and storage boundaries.

use super::*;

#[test]
fn resolver_prefers_process_environment_over_workspace_dotenv() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".env"), "MYCORP_API_KEY=workspace-key\n").expect("write dotenv");

    with_override("MYCORP_API_KEY", Some("process-key"), || {
        let resolved = resolve_credential_with_mode(
            "mycorp",
            "MYCORP_API_KEY",
            Some(workspace.path()),
            crate::auth::AuthCredentialsStoreMode::File,
        )
        .expect("resolve credential")
        .expect("credential");
        assert_eq!(resolved.secret.as_deref(), Some("process-key"));
        assert_eq!(resolved.source, CredentialSource::Env);
        assert_eq!(resolved.identity.provider(), "mycorp");
        assert_eq!(resolved.identity.key_name(), "MYCORP_API_KEY");
    });
}

#[test]
fn resolver_prefers_alternate_process_environment_over_primary_workspace_dotenv() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".env"), "GEMINI_API_KEY=workspace-key\n").expect("write dotenv");

    with_overrides(&[("GEMINI_API_KEY", None), ("GOOGLE_API_KEY", Some("process-key"))], || {
        let resolved = resolve_credential_with_mode(
            "gemini",
            "GEMINI_API_KEY",
            Some(workspace.path()),
            crate::auth::AuthCredentialsStoreMode::File,
        )
        .expect("resolve credential")
        .expect("credential");
        assert_eq!(resolved.secret.as_deref(), Some("process-key"));
        assert_eq!(resolved.source, CredentialSource::Env);
        assert_eq!(resolved.env_var.as_deref(), Some("GOOGLE_API_KEY"));
    });
}

#[test]
fn resolver_reads_workspace_dotenv_for_custom_provider_key() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".env"), "MYCORP_BILLING_KEY=workspace-key\n").expect("write dotenv");

    with_override("MYCORP_BILLING_KEY", None, || {
        let resolved = resolve_credential_with_mode(
            "mycorp",
            "mycorp_billing_key",
            Some(workspace.path()),
            crate::auth::AuthCredentialsStoreMode::File,
        )
        .expect("resolve credential")
        .expect("credential");
        assert_eq!(resolved.secret.as_deref(), Some("workspace-key"));
        assert_eq!(resolved.source, CredentialSource::Workspace);
        assert_eq!(resolved.env_var.as_deref(), Some("MYCORP_BILLING_KEY"));
    });
}

#[test]
fn resolver_does_not_reuse_legacy_storage_for_non_default_key() {
    with_override("MIMO_TOKEN_PLAN_KEY", None, || {
        let resolved = resolve_credential_with_mode(
            "mimo",
            "MIMO_TOKEN_PLAN_KEY",
            None,
            crate::auth::AuthCredentialsStoreMode::File,
        )
        .expect("resolve credential");
        assert!(resolved.is_none());
    });
}
