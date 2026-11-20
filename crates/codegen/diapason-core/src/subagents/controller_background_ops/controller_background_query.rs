//! Read-only projections over background subprocess state, threads and output.

use self::controller_background_liveness::{bounded_liveness_reason, exec_session_is_missing};

use super::controller_background_settlement::restart_allowed;

use super::*;

/// Resolves a caller-supplied background record id to the loaded map key.
///
/// The map is keyed by canonical owner-scoped id, while a client can still hold
/// the workspace-global id an older release persisted. An exact lookup then
/// misses a running record, so the pre-owner-scoping id is tried next. The
/// caller's authority decides which candidate that fallback may select:
///
/// - An exact key must be addressable by `owner`; a foreign exact match is not
///   the caller's record, so the alias search runs instead.
/// - An alias candidate is considered only when `owner` may address it. This is
///   the precedence rule: a session's own record always beats another's, and
///   an ownerless legacy row can neither shadow nor precede it. Candidates are
///   examined in map key order.
/// - [`TaskOwner::Workspace`] may address every record, so the first alias
///   match in key order wins, preserving the permissive operator view.
/// - [`TaskOwner::SessionTree`] proves nothing about a background record (see
///   [`TaskOwner::background_owned`]), so it resolves nothing here.
///
/// The ownership rejection each call site applies after resolution stays in
/// place. Narrowing the fallback keeps a foreign or ownerless row from
/// shadowing the caller's own record; it does not replace that check.
fn resolve_background_key(state: &ControllerState, owner: TaskOwner<'_>, target: &str) -> Option<String> {
    if state
        .background_children
        .get(target)
        .is_some_and(|record| owner.background_owned(record.owner_session_id.as_deref()))
    {
        return Some(target.to_owned());
    }
    state
        .background_children
        .iter()
        .find(|(_, record)| {
            background_record_id(record.agent_name.as_str()) == target
                && owner.background_owned(record.owner_session_id.as_deref())
        })
        .map(|(key, _)| key.clone())
}

/// Resolves a caller-supplied id to the record it names for `owner`.
///
/// Only a record `owner` may address is selected; see
/// [`resolve_background_key`] for the precedence rule.
pub(super) fn resolve_background_target<'a>(
    state: &'a ControllerState,
    owner: TaskOwner<'_>,
    target: &str,
) -> Option<&'a BackgroundRecord> {
    state.background_children.get(&resolve_background_key(state, owner, target)?)
}

/// Resolves a caller-supplied id to the record it names for `owner`, for mutation.
pub(super) fn resolve_background_target_mut<'a>(
    state: &'a mut ControllerState,
    owner: TaskOwner<'_>,
    target: &str,
) -> Option<&'a mut BackgroundRecord> {
    state
        .background_children
        .get_mut(&resolve_background_key(state, owner, target)?)
}

struct CapturedSubagentThread {
    id: String,
    session_id: String,
    parent_thread_id: String,
    agent_name: String,
    display_label: String,
    status: SubagentStatus,
    background: bool,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    archive_path: Option<PathBuf>,
    transcript_path: Option<PathBuf>,
    effective_config: DiapasonConfig,
    thread_handle: Option<ThreadRuntimeHandle>,
    archive_metadata: Option<crate::utils::session_archive::SessionArchiveMetadata>,
    stored_messages: Vec<Message>,
    recent_events: Vec<crate::exec::events::ThreadEvent>,
}

impl SubagentController {
    /// Returns status entries for all tracked background subprocesses.
    pub async fn background_status_entries(&self) -> Vec<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        let invocation_owner = current_subagent_invocation_context().map(|context| context.parent_session_id);
        state
            .background_children
            .values()
            .filter(|record| invocation_owner.as_ref().is_none_or(|owner| &record.parent_session_id == owner))
            .map(BackgroundRecord::build_status_entry)
            .collect()
    }

    /// Returns the cached status projection with each active record's
    /// liveness overlaid from the exec-session manager.
    ///
    /// This is a pure query: unlike
    /// [`Self::refresh_background_processes`] it mutates no record, never
    /// respawns, never publishes a lifecycle update and never persists. A
    /// management list uses it so a read cannot restart work, while still
    /// reporting a process whose exec session has already ended.
    ///
    /// The read distinguishes a session that is genuinely absent from a read
    /// that failed for another reason. An absent session is the expected exit
    /// and is projected exactly as before. Any other fault keeps the record's
    /// cached status and marks the entry [`BackgroundLiveness::Unobserved`], so
    /// a listing never fabricates a terminal outcome it did not observe. One
    /// unobserved record does not abort the list.
    pub async fn background_status_entries_with_liveness(&self) -> Vec<BackgroundLivenessEntry> {
        let mut entries = {
            let state = self.state.read().await;
            let invocation_owner = current_subagent_invocation_context().map(|context| context.parent_session_id);
            state
                .background_children
                .values()
                .filter(|record| invocation_owner.as_ref().is_none_or(|owner| &record.parent_session_id == owner))
                .map(|record| {
                    (
                        record.build_status_entry(),
                        record.status.is_active(),
                        record.exec_session_id.clone(),
                        record.desired_enabled,
                        record.restart_attempts,
                        BackgroundLiveness::Observed,
                    )
                })
                .collect::<Vec<_>>()
        };

        for (entry, active, exec_session_id, desired_enabled, restart_attempts, liveness) in &mut entries {
            if !*active {
                continue;
            }
            let alive = if exec_session_id.is_empty() {
                false
            } else {
                match self.config.exec_sessions.snapshot_session(exec_session_id).await {
                    Ok(snapshot) => matches!(
                        snapshot.lifecycle_state,
                        Some(crate::tools::types::DiapasonSessionLifecycleState::Running)
                    ),
                    Err(error) if exec_session_is_missing(&error) => false,
                    Err(error) => {
                        // Nothing is known about the process, so keep the
                        // cached status instead of projecting an exit.
                        *liveness = BackgroundLiveness::Unobserved {
                            reason: bounded_liveness_reason(&error, exec_session_id),
                        };
                        continue;
                    }
                }
            };
            if alive {
                continue;
            }
            // A refresh settles an exited process as `Error` once its single
            // restart allowance is spent. A cached `Starting` status means the
            // process is still coming up: either its first exec session is not
            // registered yet, or the restart counter has been incremented but
            // the replacement exec session is not registered yet. It must stay
            // `Starting` rather than be projected as a failure, even when
            // `auto_restore` is disabled, because nothing has failed yet.
            entry.status = if *desired_enabled
                && (entry.status == BackgroundSubprocessStatus::Starting
                    || restart_allowed(self.config.vt_cfg.subagents.background.auto_restore, *restart_attempts))
            {
                BackgroundSubprocessStatus::Starting
            } else if *desired_enabled {
                BackgroundSubprocessStatus::Error
            } else {
                BackgroundSubprocessStatus::Stopped
            };
            // The ACP wire contract requires a number for a terminal task's
            // end timestamp and allows null only while the task is active. A
            // record that never observed an end instant therefore gets its own
            // last observed instant, not the query time, so repeated polls of
            // one unchanged record report the same end.
            if !entry.status.is_active() {
                entry.ended_at.get_or_insert(entry.updated_at);
            }
        }

        entries
            .into_iter()
            .map(|(entry, _, _, _, _, liveness)| BackgroundLivenessEntry { entry, liveness })
            .collect()
    }

    /// Returns a cached snapshot of a background subprocess including its
    /// preview output.
    ///
    /// This is a pure query over cached state: it never refreshes, so it never
    /// respawns a process, settles a record, publishes a lifecycle update or
    /// persists the ledger. Callers that need exit detection must call
    /// [`Self::refresh_then_background_snapshot`] or
    /// [`Self::refresh_background_processes`] explicitly.
    pub async fn background_snapshot(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessSnapshot> {
        let entry = {
            let state = self.state.read().await;
            let record = resolve_background_target(&state, owner, target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
            if !owner.background_owned(record.owner_session_id.as_deref()) {
                bail!("Unknown background subprocess {target}");
            }
            record.build_status_entry()
        };

        let preview = read_preview(self.output_reader.as_ref(), &entry, target).await?;

        Ok(BackgroundSubprocessSnapshot { entry, preview })
    }

    /// Refreshes background state, then returns the snapshot.
    ///
    /// The name states the side effect: [`Self::refresh_background_processes`]
    /// may settle a record, publish a lifecycle update, persist the ledger and
    /// respawn a process whose restart budget allows it. Use this when the
    /// caller needs the current exit state; use [`Self::background_snapshot`]
    /// when a cached read is enough.
    pub async fn refresh_then_background_snapshot(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessSnapshot> {
        let _ = self.refresh_background_processes().await?;
        self.background_snapshot(owner, target).await
    }

    /// Returns a caller-selected tail of background output.
    ///
    /// This is a read-only query: it reads the cached record and the live exec
    /// session output without refreshing process state, publishing lifecycle
    /// updates, or persisting state. Callers that need exit detection run
    /// [`Self::refresh_background_processes`] first as an explicit command.
    ///
    /// `max_lines` is the caller's already-validated line bound. Transport
    /// limits, defaults, and request validation belong to the adapter that
    /// owns the transport contract; this controller only applies the bound it
    /// is given. The existing [`Self::background_snapshot`] 24-line preview is
    /// unchanged.
    ///
    /// Storage access and archive fallback are delegated to the controller's
    /// [`BackgroundOutputReader`]; the controller keeps the line bound and the
    /// error mapping.
    pub async fn background_output_tail(&self, owner: TaskOwner<'_>, target: &str, max_lines: usize) -> Result<String> {
        let entry = {
            let state = self.state.read().await;
            let record = resolve_background_target(&state, owner, target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
            if !owner.background_owned(record.owner_session_id.as_deref()) {
                bail!("Unknown background subprocess {target}");
            }
            record.build_status_entry()
        };

        read_output_tail(self.output_reader.as_ref(), &entry, target, max_lines).await
    }

    /// Returns whether background subagents are enabled in the configuration.
    #[must_use]
    pub fn background_subagents_enabled(&self) -> bool {
        self.config.vt_cfg.subagents.background.enabled
    }

    /// Returns the configured default background agent name, if any.
    #[must_use]
    pub fn configured_default_background_agent(&self) -> Option<&str> {
        self.config
            .vt_cfg
            .subagents
            .background
            .default_agent
            .as_deref()
            .map(str::trim)
            .filter(|agent| !agent.is_empty())
    }
}

impl SubagentController {
    /// Returns a thread snapshot for a tracked child subagent by its target id.
    pub async fn snapshot_for_thread(&self, owner: TaskOwner<'_>, target: &str) -> Result<SubagentThreadSnapshot> {
        self.ensure_child_owned(owner, target).await?;
        let captured = self.capture_subagent_thread(target).await?;
        let snapshot = self.materialize_thread_snapshot(&captured).await;
        Ok(SubagentThreadSnapshot {
            id: captured.id,
            session_id: captured.session_id,
            parent_thread_id: captured.parent_thread_id,
            agent_name: captured.agent_name,
            display_label: captured.display_label,
            status: captured.status,
            background: captured.background,
            created_at: captured.created_at,
            updated_at: captured.updated_at,
            archive_path: captured.archive_path,
            transcript_path: captured.transcript_path,
            effective_config: captured.effective_config,
            snapshot,
            recent_events: captured.recent_events,
        })
    }

    async fn capture_subagent_thread(&self, target: &str) -> Result<CapturedSubagentThread> {
        let state = self.state.read().await;
        let record = state
            .children
            .get(target)
            .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
        let effective_config = record
            .effective_config
            .clone()
            .ok_or_else(|| anyhow!("Subagent {target} does not have a captured runtime configuration yet"))?;
        Ok(CapturedSubagentThread {
            id: record.id.clone(),
            session_id: record.session_id.clone(),
            parent_thread_id: record.parent_thread_id.clone(),
            agent_name: record.spec.name.clone(),
            display_label: record.display_label.clone(),
            status: record.status,
            background: record.background,
            created_at: record.created_at,
            updated_at: record.updated_at,
            archive_path: record.archive_path.clone(),
            transcript_path: record.transcript_path.clone(),
            effective_config,
            thread_handle: record.thread_handle.clone(),
            archive_metadata: record.archive_metadata.clone(),
            stored_messages: record.stored_messages.clone(),
            recent_events: record
                .thread_handle
                .as_ref()
                .map(ThreadRuntimeHandle::recent_events)
                .unwrap_or_default(),
        })
    }

    async fn materialize_thread_snapshot(&self, captured: &CapturedSubagentThread) -> ThreadSnapshot {
        if let Some(handle) = captured.thread_handle.as_ref() {
            return handle.snapshot();
        }

        let archive_listing = match captured.archive_path.as_ref() {
            Some(path) if tokio::fs::metadata(path).await.is_ok() => load_session_listing(path).await.ok(),
            _ => None,
        };
        let metadata = archive_listing
            .as_ref()
            .map(|listing| listing.snapshot.metadata.clone())
            .or_else(|| captured.archive_metadata.clone())
            .or_else(|| {
                Some(crate::core::threads::build_thread_archive_metadata(
                    &self.config.workspace_root,
                    captured.effective_config.agent.default_model.as_str(),
                    captured.effective_config.agent.provider.as_str(),
                    captured.effective_config.agent.theme.as_str(),
                    captured.effective_config.agent.reasoning_effort.as_str(),
                ))
            });
        ThreadSnapshot {
            thread_id: ThreadId::new(captured.session_id.clone()),
            metadata,
            archive_listing,
            messages: captured.stored_messages.clone(),
            loaded_skills: Vec::new(),
            turn_in_flight: false,
        }
    }
}

/// Reads the bounded preview through the task-keyed reader.
///
/// The controller builds a [`BackgroundTaskRef`] from the live record it holds,
/// so the reader sees the task identity and its own storage handles rather than
/// the whole record; no exec-session or path handle crosses this boundary into
/// the caller.
///
/// A task without output yet yields an empty preview, which is the absence of
/// output rather than a fault. Every read fault is propagated with the caller's
/// target in the context, for the live session and for the transcript or
/// archive alike: the archive read is I/O, so reporting its failure as an empty
/// preview would hide a real storage failure behind the same value used for "not
/// written yet". Callers that cannot act on the fault, such as a background
/// sidebar, handle the error at their own boundary instead.
pub(super) async fn read_preview(
    reader: &dyn BackgroundOutputReader,
    entry: &BackgroundSubprocessEntry,
    target: &str,
) -> Result<String> {
    let task = BackgroundTaskRef::from_entry(entry);
    match reader.read_output(&task).await {
        Ok(BackgroundOutput::Live(output) | BackgroundOutput::Archived(output)) => {
            Ok(extract_tail_lines(&output, SUBAGENT_PREVIEW_LINES))
        }
        Ok(BackgroundOutput::NotReady) => Ok(String::new()),
        Err(error) => Err(anyhow::Error::new(error)).with_context(|| format!("read background output for {target}")),
    }
}

/// Reads the bounded output tail through the task-keyed reader.
///
/// The controller builds a [`BackgroundTaskRef`] from the live record it holds,
/// so the reader sees the task identity and its own storage handles rather than
/// the whole record; no exec-session or path handle crosses this boundary into
/// the caller. The reader owns storage access and fallback selection, while the
/// controller keeps the line bound. Any read fault is propagated with the
/// caller's target in the context.
pub(super) async fn read_output_tail(
    reader: &dyn BackgroundOutputReader,
    entry: &BackgroundSubprocessEntry,
    target: &str,
    max_lines: usize,
) -> Result<String> {
    let task = BackgroundTaskRef::from_entry(entry);
    match reader.read_output(&task).await {
        Ok(BackgroundOutput::Live(output) | BackgroundOutput::Archived(output)) => {
            Ok(extract_tail_lines(&output, max_lines))
        }
        Ok(BackgroundOutput::NotReady) => Ok(String::new()),
        Err(error) => Err(anyhow::Error::new(error)).with_context(|| format!("read background output for {target}")),
    }
}
