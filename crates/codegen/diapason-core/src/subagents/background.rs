//! Persists and loads background subagent state, and provides the launch
//! specifications and bounded output-tail helpers the controller uses.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

use super::constants::SUBAGENT_PREVIEW_LINES;
use super::controller_helpers::sanitize_component;
use super::types::{PersistedBackgroundRecord, PersistedBackgroundState};
use crate::utils::file_utils::write_file_atomic_with_context;
use crate::utils::session_archive::{SessionListing, SessionSnapshot};

const BACKGROUND_DEMO_AGENT: &str = "background-demo";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundLaunchSpec {
    pub command: Vec<String>,
    pub use_pty: bool,
}

// ─── Background State Persistence ──────────────────────────────────────────

fn background_state_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(diapason_commons::workspace_dir_name())
        .join("state")
        .join("background_subagents.json")
}

/// Resolves the background ledger path for tests.
///
/// The ledger location stays a private production detail, but tests must build
/// fixtures through the same workspace-directory helper as the loader so a
/// workspace-directory override cannot desynchronise them.
#[cfg(test)]
pub(crate) fn background_state_path_for_tests(workspace_root: &Path) -> PathBuf {
    background_state_path(workspace_root)
}

/// Resolves the ledger's sidecar lock path for tests.
///
/// A save that never runs must leave no trace of itself, and the lock sidecar
/// is the other file such a save would create. Resolving it here keeps a test
/// from restating the lock's filename.
#[cfg(test)]
pub(crate) fn background_state_lock_path_for_tests(workspace_root: &Path) -> PathBuf {
    super::background_lock::background_state_lock_path(workspace_root)
}

// The lock lives in its own module so this one stays focused on launch
// specifications, ledger contents and archive readers; callers keep importing
// it from here.
pub(crate) use super::background_lock::{lock_background_state, lock_background_state_within};

/// Holds the background ledger's inter-process lock for one read-modify-write.
///
/// A caller outside this crate that must rewrite the ledger as stored JSON
/// takes this guard for the whole operation, so its rewrite cannot race a save
/// from any process. The ledger lock is the outer lock of the controller's
/// lock order: do not acquire a controller persistence mutex while holding it.
#[must_use = "the ledger lock is released when the guard is dropped"]
pub struct BackgroundLedgerGuard {
    _lock: super::background_lock::BackgroundStateLock,
}

/// Acquires the inter-process lock that guards the background ledger file.
pub async fn lock_background_ledger(workspace_root: &Path) -> Result<BackgroundLedgerGuard> {
    Ok(BackgroundLedgerGuard {
        _lock: lock_background_state(workspace_root).await?,
    })
}

pub(crate) async fn load_background_state(workspace_root: &Path) -> Result<PersistedBackgroundState> {
    let path = background_state_path(workspace_root);
    let raw = match fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedBackgroundState::default());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))
}

pub(crate) async fn persist_background_state(
    workspace_root: &Path,
    records: Vec<PersistedBackgroundRecord>,
) -> Result<()> {
    let path = background_state_path(workspace_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let payload = serde_json::to_string_pretty(&PersistedBackgroundState { records })?;
    write_file_atomic_with_context(&path, &payload, "background subagent state").await?;
    Ok(())
}

// ─── Background Command Building ───────────────────────────────────────────

pub(crate) fn build_background_launch_spec(
    workspace_root: &Path,
    agent_name: &str,
    parent_session_id: &str,
    session_id: &str,
    prompt: &str,
    max_turns: Option<usize>,
    model_override: Option<&str>,
    reasoning_override: Option<&str>,
) -> Result<BackgroundLaunchSpec> {
    if agent_name == BACKGROUND_DEMO_AGENT {
        let script = workspace_root.join("scripts").join("demo-background-subagent.sh");
        return Ok(BackgroundLaunchSpec {
            command: vec![script.to_string_lossy().into_owned()],
            use_pty: false,
        });
    }

    Ok(BackgroundLaunchSpec {
        command: build_background_subagent_command(
            workspace_root,
            agent_name,
            parent_session_id,
            session_id,
            prompt,
            max_turns,
            model_override,
            reasoning_override,
        )?,
        use_pty: true,
    })
}

pub fn build_background_subagent_command(
    workspace_root: &Path,
    agent_name: &str,
    parent_session_id: &str,
    session_id: &str,
    prompt: &str,
    max_turns: Option<usize>,
    model_override: Option<&str>,
    reasoning_override: Option<&str>,
) -> Result<Vec<String>> {
    let executable = std::env::current_exe().context("Failed to resolve current diapason binary")?;
    let executable = resolve_background_subagent_executable_for_workspace(workspace_root, &executable);
    let mut command = vec![
        executable.to_string_lossy().into_owned(),
        "background-subagent".to_string(),
        "--workspace".to_string(),
        workspace_root.to_string_lossy().into_owned(),
        "--agent-name".to_string(),
        agent_name.to_string(),
        "--parent-session-id".to_string(),
        parent_session_id.to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--prompt".to_string(),
        prompt.to_string(),
    ];

    if let Some(max_turns) = max_turns {
        command.push("--max-turns".to_string());
        command.push(max_turns.to_string());
    }
    if let Some(model_override) = model_override
        && !model_override.trim().is_empty()
    {
        command.push("--model-override".to_string());
        command.push(model_override.to_string());
    }
    if let Some(reasoning_override) = reasoning_override
        && !reasoning_override.trim().is_empty()
    {
        command.push("--reasoning-override".to_string());
        command.push(reasoning_override.to_string());
    }

    Ok(command)
}

fn resolve_background_subagent_executable_for_workspace(workspace_root: &Path, current_exe: &Path) -> PathBuf {
    let workspace_target = workspace_root.join("target");
    if current_exe.starts_with(&workspace_target) {
        return current_exe.to_path_buf();
    }

    let binary_name = format!("diapason{}", std::env::consts::EXE_SUFFIX);
    for profile in ["debug", "release"] {
        let candidate = workspace_target.join(profile).join(&binary_name);
        if candidate.is_file() {
            return candidate;
        }
    }

    current_exe.to_path_buf()
}

pub fn background_record_id(agent_name: &str) -> String {
    format!("background-{}", sanitize_component(agent_name))
}

/// Returns the record id of a background subagent, scoped to its owner.
///
/// Owner-scoped controllers persist their records side by side, so two
/// sessions in one workspace can each run the same agent. The owner suffix
/// keeps those records distinct both in the in-memory map and in the shared
/// ledger; an unscoped controller keeps the workspace-global id.
///
/// The owner component is encoded injectively (see `encode_owner_component`),
/// so two distinct owners can never collapse into one record id. Owners made
/// only of `[A-Za-z0-9_-]` such as `session-a` are unchanged; any other owner,
/// including one with leading or trailing hyphens, maps to a different id. The
/// controller canonicalises persisted ids on load and on save, so no migration
/// file is needed, although a mixed-version deployment can briefly hold both
/// forms.
pub fn background_record_id_for_owner(agent_name: &str, owner_session_id: Option<&str>) -> String {
    let base = background_record_id(agent_name);
    let Some(owner_session_id) = owner_session_id else {
        return base;
    };
    if owner_session_id.is_empty() {
        return base;
    }
    let owner = encode_owner_component(owner_session_id);
    format!("{base}@{owner}")
}

/// Percent-encodes an owner id into an injective record-id component.
///
/// `[A-Za-z0-9_-]` bytes are kept verbatim and every other UTF-8 byte,
/// including `%` and `@`, is emitted as `%XX` with uppercase hex digits. Unlike
/// the session and exec-id sanitizer, this encoding never collapses distinct
/// characters into one or trims hyphens, so it cannot map two owners onto the
/// same id.
fn encode_owner_component(owner_session_id: &str) -> String {
    let mut encoded = String::with_capacity(owner_session_id.len());
    for byte in owner_session_id.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0F));
        }
    }
    encoded
}

/// Encodes one nibble as an uppercase hexadecimal digit.
///
/// The nibble is turned into a character arithmetically rather than by
/// indexing a lookup table, keeping the encoding inside the crate's indexing
/// policy while emitting the same uppercase `%XX` form.
fn hex_digit(nibble: u8) -> char {
    if nibble < 10 {
        char::from(b'0' + nibble)
    } else {
        char::from(b'A' + nibble - 10)
    }
}

// ─── Preview Utilities ─────────────────────────────────────────────────────

pub fn extract_tail_lines(content: &str, max_lines: usize) -> String {
    let lines: Vec<_> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

pub async fn load_archive_preview(path: &Path) -> Result<String> {
    load_archive_output_tail(path, SUBAGENT_PREVIEW_LINES).await
}

pub(crate) async fn load_archive_output_tail(path: &Path, max_lines: usize) -> Result<String> {
    let transcript = load_archive_transcript(path).await?;
    Ok(extract_tail_lines(&transcript, max_lines))
}

/// Loads the full archived transcript, without applying a line bound.
///
/// Callers own any bounding; the controller keeps its preview and requested
/// tail limits when it reads through an injected output reader.
pub(crate) async fn load_archive_transcript(path: &Path) -> Result<String> {
    let listing = load_session_listing(path).await?;
    Ok(listing.snapshot.transcript.join("\n"))
}

async fn load_session_listing(path: &Path) -> Result<SessionListing> {
    let raw = fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read session archive {}", path.display()))?;
    let snapshot: SessionSnapshot =
        serde_json::from_str(&raw).with_context(|| format!("Failed to parse session archive {}", path.display()))?;
    Ok(SessionListing { path: path.to_path_buf(), snapshot })
}

#[must_use]
pub(crate) fn exec_session_is_running(session: &crate::tools::types::DiapasonExecSession) -> bool {
    matches!(session.lifecycle_state, Some(crate::tools::types::DiapasonSessionLifecycleState::Running))
}

pub fn subagent_display_label(spec: &diapason_config::SubagentSpec) -> String {
    spec.nickname_candidates.first().cloned().unwrap_or_else(|| spec.name.clone())
}

#[cfg(test)]
mod tests {
    use super::{
        background_record_id, background_record_id_for_owner, build_background_launch_spec, encode_owner_component,
        resolve_background_subagent_executable_for_workspace,
    };
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    proptest! {
        /// Owner-scoped ids stay injective for arbitrary owner strings.
        ///
        /// The owner component is percent-encoded, so an owner that contains the
        /// `@` separator, the `%` escape character, a path separator or a space
        /// cannot collapse into another owner's id. An absent or empty owner
        /// keeps the workspace-global id, so unscoped controllers are unaffected.
        #[test]
        fn owner_scoped_record_ids_stay_injective(
            owners in prop::collection::vec("[A-Za-z0-9_:/. @%-]{0,24}", 0..=8),
            agent_name in "[A-Za-z0-9_-]{1,16}",
        ) {
            let mut seen: BTreeMap<String, String> = BTreeMap::new();
            for owner in &owners {
                if owner.is_empty() {
                    prop_assert_eq!(
                        background_record_id_for_owner(&agent_name, Some(owner)),
                        background_record_id(&agent_name),
                        "an empty owner must keep the workspace-global id",
                    );
                    continue;
                }
                let id = background_record_id_for_owner(&agent_name, Some(owner));
                if let Some(existing) = seen.insert(id.clone(), owner.clone()) {
                    prop_assert_eq!(
                        existing,
                        owner.clone(),
                        "distinct owners must not share a record id: {}",
                        id,
                    );
                }
            }
            prop_assert_eq!(
                background_record_id_for_owner(&agent_name, None),
                background_record_id(&agent_name),
                "an absent owner must keep the workspace-global id",
            );
        }
    }

    /// The owner component keeps safe bytes verbatim and percent-encodes every
    /// other byte as uppercase `%XX`, including a non-ASCII multi-byte value.
    #[test]
    fn owner_component_percent_encodes_unsafe_bytes_uppercase() {
        assert_eq!(encode_owner_component("session-a_1"), "session-a_1");
        assert_eq!(encode_owner_component("A1-_:. %é\u{000f}"), "A1-_%3A%2E%20%25%C3%A9%0F");
    }

    #[test]
    fn prefers_workspace_built_binary_when_current_exe_is_external() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_root = temp_dir.path();
        let candidate = workspace_root.join("target/debug/diapason");
        fs::create_dir_all(candidate.parent().expect("parent")).expect("mkdir");
        fs::write(&candidate, b"binary").expect("write candidate");

        let resolved =
            resolve_background_subagent_executable_for_workspace(workspace_root, Path::new("/usr/local/bin/diapason"));

        assert_eq!(resolved, candidate);
    }

    #[test]
    fn keeps_current_exe_when_already_running_workspace_binary() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_root = temp_dir.path();
        let current = workspace_root.join("target/debug/diapason");
        fs::create_dir_all(current.parent().expect("parent")).expect("mkdir");
        fs::write(&current, b"binary").expect("write current");

        let resolved = resolve_background_subagent_executable_for_workspace(workspace_root, &current);

        assert_eq!(resolved, current);
    }

    #[test]
    fn background_demo_launch_uses_direct_script_pipe_session() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_root = temp_dir.path();

        let launch = build_background_launch_spec(
            workspace_root,
            "background-demo",
            "parent",
            "child",
            "Report readiness once.",
            Some(4),
            None,
            None,
        )
        .expect("background demo launch");

        assert!(!launch.use_pty);
        assert_eq!(
            launch.command,
            vec![
                workspace_root
                    .join("scripts")
                    .join("demo-background-subagent.sh")
                    .to_string_lossy()
                    .into_owned()
            ]
        );
    }
}
