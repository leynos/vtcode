//! Durable session archive metadata and bounded opaque extensions.
use super::SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Metadata that accompanies every durable session snapshot.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionArchiveMetadata {
    /// Display label for the session workspace.
    pub workspace_label: String,
    /// Filesystem path of the workspace recorded when the session was created.
    pub workspace_path: String,
    /// Model identifier used by the session.
    pub model: String,
    /// Provider identifier used by the session.
    pub provider: String,
    /// UI theme active for the session.
    pub theme: String,
    /// Reasoning effort configured for the session.
    pub reasoning_effort: String,
    /// Opaque values owned by protocol adapters and keyed by stable namespace.
    ///
    /// Core preserves these values without interpreting their contents. The
    /// number of namespaces and their serialized size are bounded by
    /// [`super::MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES`] and
    /// [`super::MAX_SESSION_ARCHIVE_EXTENSION_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<SessionArchiveExtensions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Path to the session debug log, when one was written.
    pub debug_log_path: Option<String>,
    /// Names of skills loaded in this session
    #[serde(default)]
    pub loaded_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Stable identifier linking prompt-cache lineage for this session.
    pub prompt_cache_lineage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Thread identifier assigned by an external client, when available.
    pub external_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Identifier of the session from which this session was continued.
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Name of the primary agent selected for this session.
    pub primary_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Fork strategy recorded for this session, when it was created as a fork.
    pub fork_mode: Option<SessionForkMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Budget exhaustion details and continuation guidance, when applicable.
    pub continuation_metadata: Option<SessionContinuationMetadata>,
}

/// Namespaced, opaque values persisted with a session archive.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SessionArchiveExtensions(serde_json::Map<String, serde_json::Value>);

impl SessionArchiveExtensions {
    /// Create an empty extension set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return an extension value for `namespace`.
    pub fn get(&self, namespace: &str) -> Option<&serde_json::Value> {
        self.0.get(namespace)
    }

    /// Return a mutable extension value for `namespace`.
    pub fn get_mut(&mut self, namespace: &str) -> Option<&mut serde_json::Value> {
        self.0.get_mut(namespace)
    }

    /// Insert an opaque extension value, replacing the value in that namespace.
    pub fn insert(&mut self, namespace: impl Into<String>, value: serde_json::Value) -> Option<serde_json::Value> {
        self.0.insert(namespace.into(), value)
    }

    /// Remove and return the extension value for `namespace`.
    pub fn remove(&mut self, namespace: &str) -> Option<serde_json::Value> {
        self.0.remove(namespace)
    }

    /// Iterate over namespaces and their opaque values.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
        self.0.iter()
    }

    /// Return the number of namespaces.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Return whether no namespaces are present.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Deserialize)]
struct SessionArchiveMetadataWire {
    workspace_label: String,
    workspace_path: String,
    model: String,
    provider: String,
    theme: String,
    reasoning_effort: String,
    #[serde(default)]
    extensions: Option<SessionArchiveExtensions>,
    #[serde(default)]
    debug_log_path: Option<String>,
    #[serde(default)]
    loaded_skills: Vec<String>,
    #[serde(default)]
    prompt_cache_lineage_id: Option<String>,
    #[serde(default)]
    external_thread_id: Option<String>,
    #[serde(default)]
    parent_session_id: Option<String>,
    #[serde(default)]
    primary_agent: Option<String>,
    #[serde(default)]
    fork_mode: Option<SessionForkMode>,
    #[serde(default)]
    continuation_metadata: Option<SessionContinuationMetadata>,
    // Unknown top-level values from older writers stay opaque to the core
    // archive contract and are preserved under the generic legacy namespace.
    #[serde(flatten)]
    legacy_fields: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for SessionArchiveMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SessionArchiveMetadataWire::deserialize(deserializer)?;
        let had_extensions = wire.extensions.is_some() || !wire.legacy_fields.is_empty();
        let mut extensions = wire.extensions.unwrap_or_default();
        if !wire.legacy_fields.is_empty() {
            let mut legacy_fields = extensions
                .remove(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE)
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            legacy_fields.extend(wire.legacy_fields);
            drop(extensions.insert(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE, serde_json::Value::Object(legacy_fields)));
        }

        Ok(Self {
            workspace_label: wire.workspace_label,
            workspace_path: wire.workspace_path,
            model: wire.model,
            provider: wire.provider,
            theme: wire.theme,
            reasoning_effort: wire.reasoning_effort,
            extensions: had_extensions.then_some(extensions),
            debug_log_path: wire.debug_log_path,
            loaded_skills: wire.loaded_skills,
            prompt_cache_lineage_id: wire.prompt_cache_lineage_id,
            external_thread_id: wire.external_thread_id,
            parent_session_id: wire.parent_session_id,
            primary_agent: wire.primary_agent,
            fork_mode: wire.fork_mode,
            continuation_metadata: wire.continuation_metadata,
        })
    }
}

/// Strategy used to create a child archive from a source session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionForkMode {
    /// Copy the full source history into the fork.
    FullCopy,
    /// Start from a summarized representation of the source history.
    Summarized,
}

/// Budget condition that ended the prior session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionContinuationExhaustionReason {
    /// The configured maximum dollar budget was reached.
    MaxBudgetUsd,
}

/// Suggested way to proceed after a session reaches its budget limit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionContinuationRecommendedAction {
    /// Continue using the available summary as the starting context.
    ContinueFromSummary,
    /// Continue with the full saved conversation history.
    ContinueFullHistory,
    /// Start a new session without continuing the saved conversation.
    StartFresh,
}

/// Continuation information recorded when a session reaches a configured budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationMetadata {
    /// Budget condition that ended the session.
    pub exhaustion_reason: SessionContinuationExhaustionReason,
    /// Configured maximum budget in integer millionths of a US dollar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd_micros: Option<u64>,
    /// Actual cost in integer millionths of a US dollar, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_cost_usd_micros: Option<u64>,
    /// Whether a continuation summary is available.
    #[serde(default)]
    pub summary_available: bool,
    /// Suggested action for continuing after the budget limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<SessionContinuationRecommendedAction>,
}

impl SessionContinuationMetadata {
    /// Build budget-limit metadata from dollar amounts and summary availability.
    pub fn budget_limit(max_budget_usd: f64, actual_cost_usd: f64, summary_available: bool) -> Self {
        Self {
            exhaustion_reason: SessionContinuationExhaustionReason::MaxBudgetUsd,
            max_budget_usd_micros: usd_to_micros(max_budget_usd),
            actual_cost_usd_micros: usd_to_micros(actual_cost_usd),
            summary_available,
            recommended_action: Some(SessionContinuationRecommendedAction::ContinueFromSummary),
        }
    }

    /// Return the configured limit in dollars, if it was recorded.
    pub fn max_budget_usd(&self) -> Option<f64> {
        self.max_budget_usd_micros.map(micros_to_usd)
    }

    /// Return the actual cost in dollars, if it was recorded.
    pub fn actual_cost_usd(&self) -> Option<f64> {
        self.actual_cost_usd_micros.map(micros_to_usd)
    }

    /// Return whether this metadata describes exhaustion of a maximum budget.
    pub fn is_budget_limit(&self) -> bool {
        matches!(self.exhaustion_reason, SessionContinuationExhaustionReason::MaxBudgetUsd)
    }
}

fn usd_to_micros(value: f64) -> Option<u64> {
    if !value.is_finite() || value.is_sign_negative() {
        return None;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "The preceding finite/nonnegative guard ensures the rounded cast value is nonnegative."
    )]
    Some((value * 1_000_000.0).round() as u64)
}

fn micros_to_usd(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

impl SessionArchiveMetadata {
    /// Create metadata with the required workspace, model, provider, theme, and reasoning fields.
    pub fn new(
        workspace_label: impl Into<String>,
        workspace_path: impl Into<String>,
        model: impl Into<String>,
        provider: impl Into<String>,
        theme: impl Into<String>,
        reasoning_effort: impl Into<String>,
    ) -> Self {
        Self {
            workspace_label: workspace_label.into(),
            workspace_path: workspace_path.into(),
            model: model.into(),
            provider: provider.into(),
            theme: theme.into(),
            reasoning_effort: reasoning_effort.into(),
            extensions: None,
            debug_log_path: None,
            loaded_skills: Vec::new(),
            prompt_cache_lineage_id: None,
            external_thread_id: None,
            parent_session_id: None,
            primary_agent: None,
            fork_mode: None,
            continuation_metadata: None,
        }
    }

    /// Set the names of skills loaded for this session.
    pub fn with_loaded_skills(mut self, skills: Vec<String>) -> Self {
        self.loaded_skills = skills;
        self
    }

    /// Set debug log path associated with this archive.
    pub fn with_debug_log_path(mut self, path: Option<String>) -> Self {
        self.debug_log_path = path;
        self
    }

    /// Set the prompt-cache lineage identifier.
    pub fn with_prompt_cache_lineage_id(mut self, lineage_id: impl Into<String>) -> Self {
        self.prompt_cache_lineage_id = Some(lineage_id.into());
        self
    }

    /// Set the external thread identifier associated with this session.
    pub fn with_external_thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.external_thread_id = Some(thread_id.into());
        self
    }

    /// Generate and set a prompt-cache lineage identifier if none is present.
    pub fn ensure_prompt_cache_lineage_id(mut self) -> Self {
        if self.prompt_cache_lineage_id.is_none() {
            self.prompt_cache_lineage_id = Some(format!("lineage-{}", Uuid::new_v4()));
        }
        self
    }

    /// Set the parent session identifier for this continuation or fork.
    pub fn with_parent_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.parent_session_id = Some(session_id.into());
        self
    }

    /// Set the name of the primary agent selected for this session.
    pub fn with_primary_agent(mut self, primary_agent: impl Into<String>) -> Self {
        self.primary_agent = Some(primary_agent.into());
        self
    }

    /// Set the fork strategy recorded for this session.
    pub fn with_fork_mode(mut self, fork_mode: SessionForkMode) -> Self {
        self.fork_mode = Some(fork_mode);
        self
    }

    /// Set or clear continuation metadata.
    pub fn with_continuation_metadata(mut self, continuation_metadata: Option<SessionContinuationMetadata>) -> Self {
        self.continuation_metadata = continuation_metadata;
        self
    }

    /// Borrow continuation metadata when it represents a budget limit.
    pub fn budget_limit_continuation(&self) -> Option<&SessionContinuationMetadata> {
        self.continuation_metadata
            .as_ref()
            .filter(|continuation| continuation.is_budget_limit())
    }

    /// Return an opaque value associated with `namespace`.
    pub fn extension(&self, namespace: &str) -> Option<&serde_json::Value> {
        self.extensions.as_ref()?.get(namespace)
    }

    /// Set or replace an opaque extension value in `namespace`.
    pub fn set_extension(&mut self, namespace: impl Into<String>, value: serde_json::Value) {
        let extensions = self.extensions.get_or_insert_with(SessionArchiveExtensions::new);
        drop(extensions.insert(namespace, value));
    }

    /// Remove and return an opaque value associated with `namespace`.
    pub fn remove_extension(&mut self, namespace: &str) -> Option<serde_json::Value> {
        let extensions = self.extensions.as_mut()?;
        let removed = extensions.remove(namespace);
        if extensions.is_empty() {
            self.extensions = None;
        }
        removed
    }

    pub(super) fn fork_seed(&self) -> Self {
        Self {
            workspace_label: self.workspace_label.clone(),
            workspace_path: self.workspace_path.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            theme: self.theme.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            extensions: self.extensions.clone(),
            debug_log_path: self.debug_log_path.clone(),
            loaded_skills: self.loaded_skills.clone(),
            prompt_cache_lineage_id: self.prompt_cache_lineage_id.clone(),
            external_thread_id: self.external_thread_id.clone(),
            parent_session_id: None,
            primary_agent: self.primary_agent.clone(),
            fork_mode: None,
            continuation_metadata: None,
        }
    }
}
