//! The background record and its status projections.
//!
//! A background record is both runtime state and a ledger payload, and its
//! projections are what management transports serve. They sit together so a
//! field added for persistence is visible beside the projection that carries it.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::foreground::StatusEntryBuilder;
use super::status::{BackgroundSubprocessEntry, BackgroundSubprocessStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundRecord {
    pub(crate) id: String,
    pub(crate) owner_session_id: Option<String>,
    pub(crate) agent_name: String,
    pub(crate) display_label: String,
    pub(crate) description: String,
    pub(crate) source: String,
    /// The serialized key `color` is fixed by the schema; `colour` is accepted as a British spelling alias.
    #[serde(rename = "color", alias = "colour")]
    pub(crate) colour: Option<String>,
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) parent_session_id: String,
    pub(crate) exec_session_id: String,
    pub(crate) desired_enabled: bool,
    pub(crate) status: BackgroundSubprocessStatus,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) pid: Option<u32>,
    pub(crate) prompt: String,
    pub(crate) summary: Option<String>,
    /// The outcome the run itself reported.
    ///
    /// A stop's cleanup fault is never merged into this text, so a confirmed
    /// close cannot truncate a genuine failure and a failure that happens to
    /// quote the close-fault wording is not mistaken for one.
    pub(crate) error: Option<String>,
    /// The durable evidence that a stop's close is still owed.
    ///
    /// Kept apart from `error` so the two outcomes cannot overwrite each
    /// other. It blocks a start until a later close confirms the process is
    /// gone, and the status projection never serves it to a client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) close_fault: Option<String>,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) transcript_path: Option<PathBuf>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model_override: Option<String>,
    pub(crate) reasoning_override: Option<String>,
    pub(crate) restart_attempts: u8,
}

// ─── Status Entry Builders ──────────────────────────────────────────────────

impl StatusEntryBuilder for BackgroundRecord {
    type Entry = BackgroundSubprocessEntry;

    fn build_status_entry(&self) -> BackgroundSubprocessEntry {
        BackgroundSubprocessEntry {
            id: self.id.clone(),
            owner_session_id: self.owner_session_id.clone(),
            session_id: self.session_id.clone(),
            exec_session_id: self.exec_session_id.clone(),
            agent_name: self.agent_name.clone(),
            display_label: self.display_label.clone(),
            description: self.description.clone(),
            source: self.source.clone(),
            colour: self.colour.clone(),
            status: self.status,
            desired_enabled: self.desired_enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            pid: self.pid,
            summary: self.summary.clone(),
            // The close fault is durable evidence this record still owes a
            // close, and it is kept on the record for that reason. It is not a
            // run outcome, so a client is not told about it: the status and the
            // pending-stop summary already carry that state, and the text names
            // an exec session and can quote the transport error behind it.
            error: self.error.clone(),
            archive_path: self.archive_path.clone(),
            transcript_path: self.transcript_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundSubprocessSnapshot {
    pub entry: BackgroundSubprocessEntry,
    #[serde(default)]
    pub preview: String,
}

/// How one background record's liveness was obtained.
///
/// A listing that overlays liveness must not invent a terminal status when the
/// exec-session manager cannot answer: "the process exited" and "the read
/// failed" are different facts. This outcome carries that distinction to the
/// caller without changing the wire shape of a background entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundLiveness {
    /// The exec-session manager answered, or no read was needed because the
    /// record is already settled; the entry reflects the observation.
    Observed,
    /// The liveness read failed for a reason other than the session being
    /// absent. The entry keeps its cached status, and `reason` is a short,
    /// bounded diagnostic that contains no paths or secrets.
    Unobserved { reason: String },
}

/// One background status projection paired with how it was observed.
///
/// The entry is exactly the projection a caller would serve; the outcome only
/// qualifies it, so an adapter can log an unobserved record and still return
/// the list.
#[derive(Debug, Clone)]
pub struct BackgroundLivenessEntry {
    /// The status projection, with liveness overlaid only when observed.
    pub entry: BackgroundSubprocessEntry,
    /// Whether the projection's liveness was actually observed.
    pub liveness: BackgroundLiveness,
}

#[cfg(test)]
mod tests {
    //! Verifies the background projection keeps its documented colour aliases.

    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    #[test]
    fn background_subprocess_entry_accepts_colour_and_serializes_the_fixed_key() {
        let now = DateTime::<Utc>::UNIX_EPOCH;
        let entry = BackgroundSubprocessEntry {
            id: "background-1".to_string(),
            session_id: "session-1".to_string(),
            exec_session_id: "exec-1".to_string(),
            agent_name: "colour-agent".to_string(),
            display_label: "Colour agent".to_string(),
            description: "A background wire fixture".to_string(),
            source: "project".to_string(),
            colour: Some("orchid".to_string()),
            owner_session_id: None,
            status: BackgroundSubprocessStatus::Running,
            desired_enabled: true,
            created_at: now,
            updated_at: now,
            started_at: Some(now),
            ended_at: None,
            pid: None,
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
        };

        let mut canonical = serde_json::to_value(&entry).expect("the background entry should serialize");
        assert_eq!(canonical.get("color"), Some(&json!("orchid")), "background output should use the fixed wire key");
        let canonical_value = canonical
            .as_object_mut()
            .expect("a serialized background entry should be an object")
            .remove("color")
            .expect("the fixed wire key should be present");
        let previous_british_alias = canonical
            .as_object_mut()
            .expect("a serialized background entry should remain an object")
            .insert("colour".to_string(), canonical_value);
        assert!(previous_british_alias.is_none(), "the British alias should be absent before deserialization");

        let british: BackgroundSubprocessEntry =
            serde_json::from_value(canonical).expect("the British background-entry alias should deserialize");
        assert_eq!(
            british.colour.as_deref(),
            Some("orchid"),
            "the British alias should preserve the background colour"
        );
    }
}
