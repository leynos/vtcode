//! Which copy of a canonical background record survives a load or a save.
//!
//! Copies are reduced per launch before launches compete, so the answer does
//! not depend on the order the inputs arrive in. Load and save share this rule,
//! which is what stops a load from selecting a launch a save would discard.
use std::collections::{BTreeMap, BTreeSet};

use super::super::background_ledger::{MigratedBackgroundLaunch, PersistedBackgroundRecord};

/// Names the exec sessions this snapshot started rather than merely refreshed.
///
/// A launch mints a new exec session and records when it began, while a refresh
/// keeps the session and only advances `updated_at`. A snapshot copy therefore
/// supersedes the persisted launch under its canonical id only when it names a
/// different session that began no earlier than the persisted one; a refreshed
/// copy of an earlier launch is left to yield to the persisted replacement.
fn launched_by_snapshot(
    persisted: &[PersistedBackgroundRecord],
    snapshot: &[PersistedBackgroundRecord],
) -> Vec<(String, String)> {
    let current = reduce_persisted_launches(persisted.iter().cloned())
        .into_iter()
        .map(|record| (record.id.clone(), (record.exec_session_id.clone(), record.launch_moment())))
        .collect::<BTreeMap<_, _>>();
    let mut launched = Vec::new();
    for mut record in snapshot.iter().cloned() {
        record.canonicalize_id();
        if record.exec_session_id.is_empty() {
            continue;
        }
        let supersedes = match current.get(&record.id) {
            // A launch the ledger has never held is this snapshot's own.
            None => true,
            // A different session that began at least as late as the persisted
            // one is a replacement this snapshot started. A tie keeps the
            // launch, matching the merge's snapshot-on-a-tie rule.
            Some((persisted_exec_session, persisted_moment)) => {
                record.exec_session_id != *persisted_exec_session && record.launch_moment() >= *persisted_moment
            }
        };
        if supersedes {
            launched.push((record.id, record.exec_session_id));
        }
    }
    launched
}

/// Collects persisted records by canonical id, keeping one copy per launch.
///
/// Several entries can canonicalize to one key when a legacy ledger holds both
/// a pre-canonicalization id and its current form. The load path and the save
/// merge share [`join_by_canonical_id`]'s reduction rule, so the collapse does
/// not depend on the ledger's file order or the order a controller happens to
/// rebuild its map in. The ownership precedence applies here too, so a ledger
/// that holds both an ownerless and an owned copy of one launch loads only the
/// owned copy, which is the one that may be restored.
pub(crate) fn collect_by_canonical_id(
    records: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> BTreeMap<String, PersistedBackgroundRecord> {
    join_by_canonical_id(records, std::iter::empty(), &BTreeMap::new(), &[])
}

/// Returns the canonical ids whose persisted copies carry more than one empty
/// launch id.
///
/// A canonical id identifies one record, so two persisted rows without a launch
/// identity cannot be told apart; those rows are dropped rather than one of them
/// being picked. A snapshot copy that names its own exec session still
/// identifies a launch, so it is not removed with them.
fn ambiguous_empty_launch_ids(records: &[PersistedBackgroundRecord]) -> BTreeSet<String> {
    let mut empty_launches = BTreeMap::<&str, usize>::new();
    for record in records.iter().filter(|record| record.exec_session_id.is_empty()) {
        *empty_launches.entry(record.id.as_str()).or_default() += 1;
    }
    empty_launches
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Rewrites every copy's id into the canonical owner-scoped form.
///
/// The load path and the save merge both key on the canonical id, so the
/// collapse must not depend on which id form an input happened to carry.
fn canonical_copies(records: impl IntoIterator<Item = PersistedBackgroundRecord>) -> Vec<PersistedBackgroundRecord> {
    records
        .into_iter()
        .map(|mut record| {
            record.canonicalize_id();
            record
        })
        .collect()
}

/// One copy of a canonical record together with the merge side it came from.
///
/// The provenance is what lets a tie keep the snapshot copy, and what keeps a
/// stopped snapshot copy from erasing a different launch that another
/// controller has since persisted under the same canonical id.
struct LaunchCandidate {
    record: PersistedBackgroundRecord,
    /// Whether this copy arrived on the snapshot side of the merge.
    from_snapshot: bool,
}

/// The launch evidence one merge is allowed to act on.
///
/// Two different facts decide a snapshot copy's precedence. `expected_stops`
/// names the launches the snapshot is stopping, so a stop only governs the
/// launch it names. `launched` names the launches this snapshot itself started,
/// so only those may outrank a persisted row that names a different exec
/// session: a refresh of an earlier launch is not evidence that the persisted
/// replacement has been superseded.
struct MergeEvidence<'a> {
    expected_stops: &'a BTreeMap<String, String>,
    launched: &'a [(String, String)],
}

impl MergeEvidence<'_> {
    /// Whether `candidate` is a launch this snapshot itself started.
    ///
    /// Only a snapshot copy qualifies. A persisted row naming a differing exec
    /// session is a replacement another controller wrote, and re-writing it from
    /// a copy this controller never launched would erase a live process.
    fn launched_by_snapshot(&self, candidate: &LaunchCandidate) -> bool {
        candidate.from_snapshot
            && self.launched.iter().any(|(id, exec_session_id)| {
                id == &candidate.record.id && exec_session_id == &candidate.record.exec_session_id
            })
    }

    /// Whether `stop` is a stop of `other`'s launch that names its own session.
    ///
    /// `expected_stops` names the launches this save is stopping and the exec
    /// session each stop applies to. When `other` is a persisted copy of a
    /// different exec session under the same canonical id, it is a replacement
    /// launch rather than a stale copy of the stopped one, so the stop yields
    /// to it.
    fn stop_does_not_name_other_launch(&self, id: &str, stop: &LaunchCandidate, other: &LaunchCandidate) -> bool {
        self.expected_stops
            .get(id)
            .is_some_and(|exec_session_id| exec_session_id == &stop.record.exec_session_id)
            && !other.from_snapshot
            && stop.record.exec_session_id != other.record.exec_session_id
    }

    /// Whether `candidate` replaces `chosen` as `id`'s survivor.
    ///
    /// Freshness decides by `updated_at`, and a tie keeps the snapshot copy:
    /// an untouched snapshot equals the copy this controller loaded while any
    /// local change is newer than it. Without the provenance a tie fell to the
    /// lexicographically later exec session, which is not the documented rule.
    fn candidate_outranks(&self, id: &str, chosen: &LaunchCandidate, candidate: &LaunchCandidate) -> bool {
        if self.stop_does_not_name_other_launch(id, chosen, candidate) {
            return true;
        }
        if self.stop_does_not_name_other_launch(id, candidate, chosen) {
            return false;
        }
        // When the two sides name different exec sessions, the persisted copy is
        // a replacement launch and outranks the snapshot copy unless the
        // snapshot copy is a launch this save started. A refreshed copy of an
        // earlier launch keeps its exec session and only advances `updated_at`,
        // so it is not evidence that the replacement was superseded; letting
        // `updated_at` decide would erase a live process from the ledger. The
        // answer is computed from the two sides' provenance, not from which one
        // the reduction compared first.
        if chosen.from_snapshot != candidate.from_snapshot
            && chosen.record.exec_session_id != candidate.record.exec_session_id
        {
            let snapshot = if candidate.from_snapshot { candidate } else { chosen };
            if !self.launched_by_snapshot(snapshot) {
                return !candidate.from_snapshot;
            }
        }
        // Equal timestamps are a tie in both directions, so the snapshot
        // provenance decides it rather than the exec session's map order.
        candidate.record.is_at_least_as_new_as(&chosen.record)
            && (!chosen.record.is_at_least_as_new_as(&candidate.record)
                || (candidate.from_snapshot && !chosen.from_snapshot))
    }
}

/// Joins copies of each canonical record by launch, then by ownership and
/// freshness.
///
/// Copies are grouped first by canonical id and `exec_session_id`. Within one
/// launch a cancellation outranks an active copy in either direction, so a
/// refresh that only advanced `updated_at` cannot resurrect a stopped task.
/// Only the surviving copy of each launch is then compared by `updated_at`,
/// which is what lets an explicit restart supersede the launch it replaced: a
/// restart writes a new exec session, so it is never mistaken for a stale copy
/// of the stopped one. A tie keeps the snapshot copy over the persisted copy
/// it equals.
///
/// The persisted side is first reduced per canonical id to the launch a load
/// would choose, so a legacy ledger holding an older and a newer launch under
/// one id cannot make the older one deflect a stop that names the current one.
/// That reduction can also discard the ledger's stopped copy of a launch the
/// snapshot is refreshing: the snapshot's copy then competes only with the
/// different launch the reduction kept, and a later `updated_at` would let it
/// resurrect the stopped session. The persisted stopped copy of any session the
/// snapshot names is therefore retained, so the within-launch cancellation rule
/// decides between them. A stop then applies only to the launch it names: it
/// never replaces a persisted row of a different exec session, because that row
/// is a replacement another controller wrote. A refreshed copy of an earlier
/// launch is equally not evidence of supersession, so only a launch this
/// snapshot started may displace a persisted row naming a different session. A
/// restart writes a new exec session, so it matches no stopped copy and still
/// supersedes the launch it replaced.
///
/// A copy that records an owner outranks an ownerless copy of the same
/// launch, whatever their timestamps. Ownerless and owned copies of one launch
/// canonicalize to different keys, because the key is derived from the owner,
/// so this cannot be decided inside a group: an ownerless copy is dropped when
/// any input copy of the same launch (`agent_name` and `exec_session_id`)
/// records an owner. That is what stops a controller that loaded a row before
/// its owner was recorded — the ACP launch workspace, which restores ownerless
/// rows before any session resumes — from saving the ownerless copy back
/// alongside the migrated record and leaving a restorable duplicate of a
/// launch a session now owns.
///
/// The ownership evidence is taken from every input copy, not only from the
/// launches that survived the reduction. A newer owned launch supersedes the
/// older owned launch it replaces, so the older launch may have no owned
/// survivor for an ownerless copy to match against; reading only the survivors
/// would let that ownerless copy survive beside the replacement. Dropping the
/// ownerless copy is the conservative choice whenever an owned copy exists in
/// the merge: the owned copy is the one every scope agrees on, an ownerless
/// copy in memory is not authoritatively ownerless, and dropping it loses at
/// most a refresh that cannot be persisted deterministically.
///
/// Ineligible copies are excluded from both sides before launch precedence is
/// computed, not only from the chosen survivor. A stale ownerless alias of a
/// migrated launch could otherwise win its canonical id and be dropped
/// afterwards, discarding an independent launch under the same id with it. The
/// same reasoning excludes the persisted rows that share an id without a launch
/// identity: those rows cannot be told apart, but dropping the whole id would
/// also remove a snapshot copy that names its own exec session.
///
/// Reducing per launch before comparing launches is what makes the choice
/// independent of input order. A single comparator over every copy of the key
/// is cyclic because cancellation dominance and timestamp order disagree: with
/// an active `exec-1` copy at t3, a cancelled `exec-1` copy at t1 and an active
/// `exec-2` copy at t2, file order `[cancelled, exec-2, active]` keeps the
/// active `exec-1` copy and resurrects the stopped launch, while
/// `[active, cancelled, exec-2]` keeps `exec-2`. The ownership filter is also
/// independent of input order: it depends only on which launches the inputs
/// attribute to an owner, not on where their copies appeared or which of them
/// the reduction kept.
pub(super) fn join_by_canonical_id(
    persisted: impl IntoIterator<Item = PersistedBackgroundRecord>,
    snapshot: impl IntoIterator<Item = PersistedBackgroundRecord>,
    expected_stops: &BTreeMap<String, String>,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> BTreeMap<String, PersistedBackgroundRecord> {
    let persisted = canonical_copies(persisted);
    let snapshot = canonical_copies(snapshot);
    // An empty exec session cannot identify a launch, so two persisted copies
    // that share a canonical id and both carry one might be different launches.
    // They are dropped before any launch is selected: keeping either would let a
    // stopped row outrank a desired one with no launch identity to decide it,
    // while dropping the whole id would also discard a snapshot copy that names
    // its own exec session.
    let ambiguous = ambiguous_empty_launch_ids(&persisted);
    // Every launch an input copy attributes to an owner, read from all rows
    // before any copy is excluded: a superseded owned launch has no owned
    // survivor left, so reading only the survivors would let its ownerless copy
    // survive beside the replacement.
    let owned_launches = persisted
        .iter()
        .chain(snapshot.iter())
        .filter(|record| record.owner_session_id.is_some())
        .map(ownerless_launch)
        .collect::<BTreeSet<_>>();
    // Ineligible copies are excluded from both sides before precedence is
    // computed. Filtering only the chosen survivor would let a stale ownerless
    // alias of a migrated launch win its canonical id and be dropped, taking an
    // independent launch under the same id with it.
    let eligible = |record: &PersistedBackgroundRecord| {
        record.owner_session_id.is_some()
            || (!owned_launches.contains(&ownerless_launch(record)) && !is_migrated_alias(record, migrated_launches))
    };
    let persisted = persisted
        .into_iter()
        .filter(|record| !ambiguous.contains(&record.id) && eligible(record))
        .collect::<Vec<_>>();
    // A snapshot copy only competes while it can be told apart from the
    // persisted rows. It always can when it names its own exec session, which is
    // the launch this controller is running; without that identity it is as
    // ambiguous as the rows the id already had, so the id keeps dropping.
    let snapshot = snapshot
        .into_iter()
        .filter(|record| (!ambiguous.contains(&record.id) || !record.exec_session_id.is_empty()) && eligible(record))
        .collect::<Vec<_>>();
    // The launches this snapshot started are derived from the two sides: a
    // snapshot session the ledger does not hold for its canonical id, or one
    // that began after the persisted launch, is a new launch. A copy that keeps
    // the persisted session and only advanced `updated_at` is a refresh of the
    // earlier launch and must yield to a persisted replacement.
    let launched = launched_by_snapshot(&persisted, &snapshot);
    let evidence = MergeEvidence { expected_stops, launched: &launched };
    // The persisted side is reduced per canonical id to the launch a load would
    // choose, before the snapshot competes with it. A legacy ledger can hold an
    // older and a newer launch under one id; only the newer one is what a load
    // selects, so treating the older as a replacement launch would let it
    // deflect a stop that names the current one. The reduction must not hide a
    // stop of a session the snapshot refreshes, though: that snapshot copy would
    // then compete only with the different launch the reduction kept and could
    // resurrect the stopped session on `updated_at` alone.
    let reduced = reduce_persisted_launches(persisted.iter().cloned());
    let refreshed_sessions = snapshot
        .iter()
        .filter(|record| !record.exec_session_id.is_empty())
        .map(|record| (record.id.clone(), record.exec_session_id.clone()))
        .collect::<BTreeSet<_>>();
    let retained_stops = persisted
        .iter()
        .filter(|record| {
            !record.desired_enabled
                && refreshed_sessions.contains(&(record.id.clone(), record.exec_session_id.clone()))
                && !reduced
                    .iter()
                    .any(|kept| kept.id == record.id && kept.exec_session_id == record.exec_session_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let persisted = reduced.into_iter().chain(retained_stops).collect::<Vec<_>>();
    let candidates = persisted
        .into_iter()
        .map(|record| LaunchCandidate { record, from_snapshot: false })
        .chain(
            snapshot
                .into_iter()
                .map(|record| LaunchCandidate { record, from_snapshot: true }),
        );
    reduce_launch_copies(candidates)
        .into_iter()
        .filter_map(|(id, sessions)| {
            // An id is only inserted together with at least one launch copy, so
            // the empty group this skips is unreachable. It is skipped rather
            // than unwrapped: a merge of persisted state must never panic, and
            // dropping an id it cannot represent loses at most one record.
            let chosen = sessions.into_values().reduce(|chosen, candidate| {
                if evidence.candidate_outranks(&id, &chosen, &candidate) {
                    candidate
                } else {
                    chosen
                }
            })?;
            Some((id, chosen.record))
        })
        .collect()
}

/// Whether `record` is the stale ownerless copy of a migrated launch.
///
/// The migration rewrote the durable row under its owner's id, and a later
/// replacement may have superseded that row entirely. The recorded evidence is
/// then the only thing that still names the launch, so a workspace copy that
/// was cached before the migration is recognised here and dropped rather than
/// restored beside the record its session now owns. An independent ownerless
/// launch of the same agent names a different exec session and survives.
pub(super) fn is_migrated_alias(
    record: &PersistedBackgroundRecord,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> bool {
    record.owner_session_id.is_none()
        && migrated_launches.iter().any(|launch| {
            launch.base_id == record.ownerless_base_id() && launch.exec_session_id == record.exec_session_id
        })
}

/// Reduces the persisted side per canonical id to the copy a load would choose.
///
/// A legacy ledger, or one an owner migration has rewritten, can hold several
/// launches under one canonical id. Only the newest of them is what a load
/// selects, so this is the launch a stop must be compared against. A cancelled
/// copy still outranks an active copy of the same launch, matching
/// [`join_by_canonical_id`]'s within-launch rule.
fn reduce_persisted_launches(
    persisted: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> Vec<PersistedBackgroundRecord> {
    let evidence = MergeEvidence { expected_stops: &BTreeMap::new(), launched: &[] };
    let candidates = persisted.into_iter().map(|mut record| {
        record.canonicalize_id();
        LaunchCandidate { record, from_snapshot: false }
    });
    reduce_launch_copies(candidates)
        .into_iter()
        .filter_map(|(id, sessions)| {
            sessions
                .into_values()
                .reduce(|chosen, candidate| {
                    if evidence.candidate_outranks(&id, &chosen, &candidate) {
                        candidate
                    } else {
                        chosen
                    }
                })
                .map(|chosen| chosen.record)
        })
        .collect()
}

/// Groups copies of each canonical record by launch, keeping one copy per launch.
fn reduce_launch_copies(
    candidates: impl IntoIterator<Item = LaunchCandidate>,
) -> BTreeMap<String, BTreeMap<String, LaunchCandidate>> {
    let mut launches: BTreeMap<String, BTreeMap<String, LaunchCandidate>> = BTreeMap::new();
    for candidate in candidates {
        let id = candidate.record.id.clone();
        let exec_session_id = candidate.record.exec_session_id.clone();
        let sessions = launches.entry(id).or_default();
        match sessions.entry(exec_session_id) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Cancellation dominance decides within one launch; freshness
                // only decides between copies that agree on intent. A stopped
                // current copy must survive a newer active copy, so it is
                // checked before freshness. A tie keeps the later entry, which
                // is the snapshot copy when the two sides agree.
                let replace = {
                    let current = entry.get();
                    !current.record.cancellation_outranks(&candidate.record)
                        && (candidate.record.cancellation_outranks(&current.record)
                            || candidate.record.is_at_least_as_new_as(&current.record))
                };
                if replace {
                    entry.insert(candidate);
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
        }
    }
    launches
}

/// Names the launch a record is a copy of, ignoring its owner.
///
/// The ownerless base id of a launch is the key its ownerless copy carries, so
/// an owned and an ownerless copy of one launch compare equal once the owner is
/// cleared and the id recomputed.
fn ownerless_launch(record: &PersistedBackgroundRecord) -> (String, String) {
    let mut base = record.clone();
    base.owner_session_id = None;
    base.canonicalize_id();
    (base.id, base.exec_session_id)
}

#[cfg(test)]
mod launch_precedence_regressions {
    //! Pins that a refreshed copy never resurrects a launch the ledger stopped,
    //! and that an ineligible alias cannot win its canonical id.

    use super::*;

    /// Builds one persisted copy under the canonical id for its agent and owner.
    fn launch_copy(
        agent: &str,
        owner: Option<&str>,
        exec_session_id: &str,
        desired_enabled: bool,
        updated_at: &str,
    ) -> PersistedBackgroundRecord {
        let id = crate::subagents::background::background_record_id_for_owner(agent, owner);
        serde_json::from_value(serde_json::json!({
            "id": id,
            "owner_session_id": owner,
            "agent_name": agent,
            "display_label": agent,
            "description": agent,
            "source": "builtin",
            "color": null,
            "session_id": format!("session-{exec_session_id}"),
            "parent_session_id": owner.unwrap_or("parent-session"),
            "exec_session_id": exec_session_id,
            "desired_enabled": desired_enabled,
            "status": if desired_enabled { "running" } else { "stopped" },
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": updated_at,
            "started_at": null,
            "ended_at": null,
            "pid": null,
            "prompt": "work",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }))
        .expect("persisted launch copy")
    }

    /// A refresh of a stopped launch must not resurrect it.
    ///
    /// The ledger holds a stopped copy of `exec-stopped` and a newer
    /// `exec-current` under one canonical id, so the per-id reduction keeps the
    /// current launch and drops the stop. A snapshot copy that merely refreshes
    /// `exec-stopped` must not then win on `updated_at`; the persisted stop must
    /// compete with it so the cancellation rule applies. A restart, which names
    /// a new exec session, still supersedes the stopped launch.
    #[test]
    fn a_refreshed_copy_of_a_stopped_session_does_not_replace_the_persisted_launch() {
        let persisted = vec![
            launch_copy("worker", Some("session-a"), "exec-stopped", false, "2026-08-28T00:00:01Z"),
            launch_copy("worker", Some("session-a"), "exec-current", true, "2026-08-28T00:00:02Z"),
        ];
        let snapshot = vec![launch_copy(
            "worker",
            Some("session-a"),
            "exec-stopped",
            true,
            "2026-08-28T00:00:03Z",
        )];

        let merged = join_by_canonical_id(persisted, snapshot, &BTreeMap::new(), &[]);

        assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
        let record = merged.values().next().expect("the surviving launch");
        assert_eq!(
            record.exec_session_id, "exec-current",
            "a refresh of the stopped session must not replace the launch the ledger kept"
        );
        assert!(record.desired_enabled, "the surviving launch is still desired");
    }

    /// A restart with a new exec session still supersedes the stopped launch.
    ///
    /// Retaining the persisted stop of a refreshed session must not block a
    /// restart: it names a new exec session, so it matches no stopped copy and
    /// the within-launch cancellation rule does not apply to it.
    #[test]
    fn a_restart_with_a_new_exec_session_supersedes_the_stopped_launch() {
        let persisted = vec![
            launch_copy("worker", Some("session-a"), "exec-stopped", false, "2026-08-28T00:00:01Z"),
            launch_copy("worker", Some("session-a"), "exec-current", true, "2026-08-28T00:00:02Z"),
        ];
        let snapshot = vec![launch_copy(
            "worker",
            Some("session-a"),
            "exec-restart",
            true,
            "2026-08-28T00:00:04Z",
        )];

        let merged = join_by_canonical_id(persisted, snapshot, &BTreeMap::new(), &[]);

        assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
        let record = merged.values().next().expect("the surviving launch");
        assert_eq!(
            record.exec_session_id, "exec-restart",
            "a restart names a new exec session and supersedes the launch it replaced"
        );
        assert!(record.desired_enabled, "the restarted launch is desired");
    }

    /// A migrated alias must not win its id and suppress an independent launch.
    ///
    /// The ledger holds an independent ownerless `exec-workspace`, while the
    /// snapshot still carries a cached ownerless copy of a launch whose
    /// migration is recorded under `exec-migrated`. The alias has the later
    /// timestamp, so it used to win the canonical-id reduction and then be
    /// dropped, removing `exec-workspace` from the ledger with it.
    #[test]
    fn a_migrated_alias_does_not_suppress_an_independent_ownerless_launch() {
        let ledger = vec![launch_copy(
            "worker",
            None,
            "exec-workspace",
            true,
            "2026-08-28T00:00:01Z",
        )];
        let snapshot = vec![launch_copy(
            "worker",
            None,
            "exec-migrated",
            true,
            "2026-08-28T00:00:09Z",
        )];
        let evidence = vec![MigratedBackgroundLaunch {
            base_id: crate::subagents::background::background_record_id_for_owner("worker", None),
            exec_session_id: "exec-migrated".to_string(),
        }];

        let merged = join_by_canonical_id(ledger, snapshot, &BTreeMap::new(), &evidence);

        assert_eq!(merged.len(), 1, "the independent launch keeps the canonical id");
        let record = merged.values().next().expect("the independent launch");
        assert_eq!(
            record.exec_session_id, "exec-workspace",
            "a newer migrated alias must not suppress an independent ownerless launch"
        );
    }
}
