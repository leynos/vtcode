//! Which copy of a canonical background record survives a load or a save.
//!
//! Copies are reduced per launch before launches compete, so the answer does
//! not depend on the order the inputs arrive in. Load and save share this rule,
//! which is what stops a load from selecting a launch a save would discard.
use std::collections::{BTreeMap, BTreeSet};

use super::super::background_ledger::{MigratedBackgroundLaunch, PersistedBackgroundRecord};

mod eligibility;

/// Re-exported for [`super::merge`], which drops a migrated ownerless alias
/// before it can compete for its canonical id.
pub(in crate::subagents::controller_persistence) use self::eligibility::is_migrated_alias;
use self::eligibility::{
    ambiguous_empty_launch_ids, canonical_copies, has_ambiguous_launch_identity, launch_is_eligible, owned_launch_keys,
};

/// Names the exec sessions this snapshot started rather than merely refreshed.
///
/// A launch mints a new exec session and records when it began, while a refresh
/// keeps the session and only advances `updated_at`. A snapshot copy therefore
/// supersedes the persisted launch under its canonical id only when it names a
/// different session that began no earlier than the persisted one; a refreshed
/// copy of an earlier launch is left to yield to the persisted replacement.
///
/// A copy that carries no `started_at` proves no launch of its own, so its
/// `updated_at` alone must not displace a persisted row naming a different exec
/// session. The one exception is a copy that is still wanted and reports an
/// active status: that is a claim this snapshot has not finished launching, and
/// the reservation confirms the claim reached the ledger before it spawns, so
/// the ordinary moment comparison decides it. A settled copy without a start
/// instant is an abandoned or failed claim whose settlement describes a launch
/// that never ran; letting a later `updated_at` carry it over another
/// controller's replacement would erase a live launch from the ledger under the
/// name of one that was never started.
fn launched_by_snapshot(
    persisted: &[PersistedBackgroundRecord],
    snapshot: &[PersistedBackgroundRecord],
) -> Vec<(String, String)> {
    let current = reduce_persisted_launches(persisted.iter().cloned())
        .into_iter()
        .map(|record| (record.id.clone(), record))
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
            Some(persisted) => {
                record.exec_session_id != persisted.exec_session_id
                    && names_a_launch_of_its_own(&record)
                    && record.launch_moment() >= persisted.launch_moment()
                    && start_evidence_outranks_persisted(&record, persisted)
            }
        };
        if supersedes {
            launched.push((record.id, record.exec_session_id));
        }
    }
    launched
}

/// Whether this copy's launch evidence may supersede a different persisted launch.
///
/// A copy that recorded its start is a launch this controller began, and the
/// ordinary moment comparison decides it. A copy that has not recorded a start
/// is a claim: its `launch_moment` is only the moment the claim was written, and
/// comparing that against a persisted launch's `started_at` compares two
/// different quantities. A claim could then appear newer than a replacement
/// that was persisted after it merely because the replacement's process began
/// earlier, and the replacement would be erased from the ledger. Such a claim
/// can displace another claim, but a persisted launch that recorded a start
/// survives it.
fn start_evidence_outranks_persisted(
    record: &PersistedBackgroundRecord,
    persisted: &PersistedBackgroundRecord,
) -> bool {
    record.started_at.is_some() || persisted.started_at.is_none()
}

/// Whether `record` carries evidence of a launch of its own.
///
/// A copy that started has a `started_at`. A copy that is still wanted and
/// reports an active status is a claim whose launch has not finished starting:
/// the reservation persists that claim before it spawns and confirms the ledger
/// kept it, so the absent `started_at` there is the launch's current phase
/// rather than an absence of evidence. Anything else - a settled copy with no
/// start instant - describes a launch that never ran, and its `updated_at` is
/// not proof that a replay of it supersedes a launch another controller
/// persisted.
fn names_a_launch_of_its_own(record: &PersistedBackgroundRecord) -> bool {
    record.started_at.is_some() || (record.desired_enabled && record.status.is_active())
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
/// same reasoning excludes the rows that share an id without a launch identity:
/// they cannot be told apart, so they are dropped rather than resolved
/// arbitrarily. The exclusion applies to those rows alone. A copy that names its
/// own exec session identifies a launch and is kept even under such an id - on
/// the persisted side as much as on the snapshot side - because removing the
/// whole id would discard an identified launch that a load can list and a
/// reattach can restore.
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
    // Those rows are dropped before any launch is selected: keeping either would
    // let a stopped row outrank a desired one with no launch identity to decide
    // it. Only those rows go, not the id: a copy that names its own exec session
    // is an identified launch and keeps competing.
    let ambiguous = ambiguous_empty_launch_ids(&persisted);
    // Every launch an input copy attributes to an owner, read from all rows
    // before any copy is excluded: a superseded owned launch has no owned
    // survivor left, so reading only the survivors would let its ownerless copy
    // survive beside the replacement.
    let owned_launches = owned_launch_keys(persisted.iter().chain(snapshot.iter()));
    // Ineligible copies are excluded from both sides before precedence is
    // computed. Filtering only the chosen survivor would let a stale ownerless
    // alias of a migrated launch win its canonical id and be dropped, taking an
    // independent launch under the same id with it. The ambiguity rule applies
    // only to the rows it is about - those without a launch identity - so an
    // identified launch under the same id still competes.
    let persisted = persisted
        .into_iter()
        .filter(|record| {
            !has_ambiguous_launch_identity(record, &ambiguous)
                && launch_is_eligible(record, &owned_launches, migrated_launches)
        })
        .collect::<Vec<_>>();
    let snapshot = snapshot
        .into_iter()
        .filter(|record| {
            !has_ambiguous_launch_identity(record, &ambiguous)
                && launch_is_eligible(record, &owned_launches, migrated_launches)
        })
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

/// The key one copy is reduced under within its canonical id.
///
/// A named exec session is the launch. A legacy row names none, so the child
/// session it carries is the identity left: grouping two legacy copies together
/// on a shared blank value would let a stop of one launch govern a refresh of
/// another, and a restart of a legacy record mints a fresh child session. The
/// prefixes keep the two namespaces apart. A row that names neither falls into
/// the shared unidentified slot; the ambiguity rule has already excluded the
/// persisted duplicates that could reach here.
fn launch_group_key(record: &PersistedBackgroundRecord) -> String {
    if !record.exec_session_id.is_empty() {
        format!("exec:{}", record.exec_session_id)
    } else if !record.session_id.is_empty() {
        format!("session:{}", record.session_id)
    } else {
        String::new()
    }
}

/// Groups copies of each canonical record by launch, keeping one copy per launch.
///
/// Within one launch the survivor is chosen by these rules, in this order:
///
/// 1. a copy that no longer wants the launch outranks one that still does, so a
///    refresh that only advanced `updated_at` cannot resurrect a stopped task;
/// 2. a copy with a strictly newer `updated_at` outranks an older one;
/// 3. of two copies tied on intent and freshness, the one whose status records
///    the later phase of the same launch outranks the other: a settled copy
///    (`Stopped`, then `Error`) outranks an active one, and `Running` outranks
///    `Starting`. A recorded failure outranks a bare stop because the
///    settlement path never relabels a failure as completed;
/// 4. a copy the snapshot side carries outranks the persisted copy it equals.
///
/// Rule 3 is what breaks a tie deterministically. Two copies of one launch with
/// equal `updated_at` and agreeing intent used to replace each other, so the
/// ledger's file order decided which record a load reported; the later-phase
/// copy is now chosen whichever of the two is listed first. Rule 2 still comes
/// first, so a strictly newer copy always wins, whatever status it reports.
fn reduce_launch_copies(
    candidates: impl IntoIterator<Item = LaunchCandidate>,
) -> BTreeMap<String, BTreeMap<String, LaunchCandidate>> {
    let mut launches: BTreeMap<String, BTreeMap<String, LaunchCandidate>> = BTreeMap::new();
    for candidate in candidates {
        let id = candidate.record.id.clone();
        let launch_key = launch_group_key(&candidate.record);
        let sessions = launches.entry(id).or_default();
        match sessions.entry(launch_key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if copy_replaces(entry.get(), &candidate) {
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

/// Whether `candidate` outranks `current` within one launch.
///
/// The order is the one [`reduce_launch_copies`] documents, and the same
/// sequence decides whichever side a copy came from: a stopped copy outranks an
/// active copy of its launch, a strictly newer `updated_at` outranks an older
/// one, and a tie then keeps the snapshot copy over the persisted one.
fn copy_replaces(current: &LaunchCandidate, candidate: &LaunchCandidate) -> bool {
    if current.record.cancellation_outranks(&candidate.record) {
        return false;
    }
    if candidate.record.cancellation_outranks(&current.record) {
        return true;
    }
    if candidate.record.updated_at != current.record.updated_at {
        return candidate.record.updated_at > current.record.updated_at;
    }
    let candidate_rank = status_phase_rank(candidate.record.status);
    let current_rank = status_phase_rank(current.record.status);
    if candidate_rank != current_rank {
        return candidate_rank > current_rank;
    }
    // Both copies now agree on intent, freshness and the phase they record, so
    // only the side each came from is left to decide.
    later_arrival_wins(current, candidate)
}

/// Whether `candidate` beats `current` when every non-arrival rule has tied.
///
/// Separated so the same-side case reads explicitly: two copies from one side
/// are interchangeable, and the later arrival is kept.
fn later_arrival_wins(current: &LaunchCandidate, candidate: &LaunchCandidate) -> bool {
    match (current.from_snapshot, candidate.from_snapshot) {
        // A snapshot copy beats the persisted copy it equals.
        (false, true) => true,
        (true, false) => false,
        // Two copies from one side are interchangeable; keep the later arrival.
        _ => true,
    }
}

/// Orders two statuses of one launch by the phase they record.
///
/// Higher wins: `Error` (a recorded failure) over `Stopped` (a confirmed
/// stop), then `Running` (the later active phase), then `Starting` (the claim).
/// A failure outranks a stop because the settlement path never relabels a
/// failure as a completed stop; an active phase outranks a settled one only
/// through its rank, so a settled copy still beats any active copy.
const fn status_phase_rank(status: crate::subagents::BackgroundSubprocessStatus) -> u8 {
    match status {
        crate::subagents::BackgroundSubprocessStatus::Starting => 0,
        crate::subagents::BackgroundSubprocessStatus::Running => 1,
        crate::subagents::BackgroundSubprocessStatus::Stopped => 2,
        crate::subagents::BackgroundSubprocessStatus::Error => 3,
    }
}

#[cfg(test)]
mod launch_precedence_regressions;
