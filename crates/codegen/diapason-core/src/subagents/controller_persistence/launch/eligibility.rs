//! Which copies of one canonical record may take part in a launch selection.
//!
//! Identity filtering runs before launch precedence, so a copy that cannot be
//! attributed to a launch never competes for its canonical id. Filtering only
//! the chosen survivor instead would let a stale alias win an id and then be
//! dropped, taking an independent launch under the same id with it. The checks
//! here depend only on the copies' identities, never on input order or on
//! timestamps.
use std::collections::{BTreeMap, BTreeSet};

use super::super::super::background_ledger::{MigratedBackgroundLaunch, PersistedBackgroundRecord};

/// Rewrites every copy's id into the canonical owner-scoped form.
///
/// The load path and the save merge both key on the canonical id, so the
/// collapse must not depend on which id form an input happened to carry.
pub(super) fn canonical_copies(
    records: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> Vec<PersistedBackgroundRecord> {
    records
        .into_iter()
        .map(|mut record| {
            record.canonicalize_id();
            record
        })
        .collect()
}

/// Returns the canonical ids whose persisted copies carry more than one empty
/// launch id.
///
/// A canonical id identifies one record, so two persisted rows without a launch
/// identity cannot be told apart; those rows are dropped rather than one of them
/// being picked. Only the empty rows go: a row that names its own exec session
/// identifies a launch, so it is kept even under an ambiguous id, and dropping
/// the whole id would lose a live task. A snapshot copy that names its own exec
/// session is not removed with them either.
pub(super) fn ambiguous_empty_launch_ids(records: &[PersistedBackgroundRecord]) -> BTreeSet<String> {
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

/// Reports whether `record` is one of the rows an ambiguous id cannot tell apart.
///
/// The ambiguity is about rows without a launch identity. A copy that names its
/// own exec session is identified, so it keeps competing for the id.
pub(super) fn has_ambiguous_launch_identity(record: &PersistedBackgroundRecord, ambiguous: &BTreeSet<String>) -> bool {
    record.exec_session_id.is_empty() && ambiguous.contains(&record.id)
}

/// Names the launch a record is a copy of, ignoring its owner.
///
/// The ownerless base id of a launch is the key its ownerless copy carries, so
/// an owned and an ownerless copy of one launch compare equal once the owner is
/// cleared. It is taken from [`PersistedBackgroundRecord::ownerless_base_id`],
/// which is the same derivation the migration evidence uses, so the two
/// comparisons cannot drift apart; only the exec session is cloned from the
/// record.
fn ownerless_launch(record: &PersistedBackgroundRecord) -> (String, String) {
    (record.ownerless_base_id(), record.exec_session_id.clone())
}

/// Collects every launch the inputs attribute to an owner.
///
/// The evidence is read from all input copies, not only the survivors: a
/// superseded owned launch has no owned survivor left, so reading only the
/// survivors would let its ownerless copy survive beside the replacement.
///
/// A copy with an empty exec session names no launch, so it cannot attribute
/// one to an owner. Treating `(base id, "")` as evidence would suppress an
/// ownerless row that is indistinguishable from it only because both name no
/// launch at all: two such rows can be different child sessions.
pub(super) fn owned_launch_keys<'a>(
    records: impl IntoIterator<Item = &'a PersistedBackgroundRecord>,
) -> BTreeSet<(String, String)> {
    records
        .into_iter()
        .filter(|record| record.owner_session_id.is_some() && !record.exec_session_id.is_empty())
        .map(ownerless_launch)
        .collect()
}

/// Whether `record` is the stale ownerless copy of a migrated launch.
///
/// The migration rewrote the durable row under its owner's id, and a later
/// replacement may have superseded that row entirely. The recorded evidence is
/// then the only thing that still names the launch, so a workspace copy that
/// was cached before the migration is recognized here and dropped rather than
/// restored beside the record its session now owns. An independent ownerless
/// launch of the same agent names a different exec session and survives.
///
/// Empty exec sessions are not alias evidence in either direction: a row that
/// names no launch cannot be shown to be the copy of the migrated one, and the
/// migration evidence may itself carry an empty id when an owned legacy row had
/// none.
pub(in crate::subagents::controller_persistence) fn is_migrated_alias(
    record: &PersistedBackgroundRecord,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> bool {
    if record.owner_session_id.is_some() || record.exec_session_id.is_empty() {
        return false;
    }
    // The base id is derived from the agent alone, so it is computed once and
    // compared against every evidence entry instead of per entry.
    let base_id = record.ownerless_base_id();
    migrated_launches.iter().any(|launch| {
        launch.base_id == base_id
            && !launch.exec_session_id.is_empty()
            && launch.exec_session_id == record.exec_session_id
    })
}

/// Reports whether `record` may compete for its canonical id at all.
///
/// An ownerless copy is dropped when another input copy attributes the same
/// launch to an owner, or when the migration evidence names it as the stale
/// copy of a migrated launch. An owned copy always competes.
pub(super) fn launch_is_eligible(
    record: &PersistedBackgroundRecord,
    owned_launches: &BTreeSet<(String, String)>,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> bool {
    record.owner_session_id.is_some()
        || (!owned_launches.contains(&ownerless_launch(record)) && !is_migrated_alias(record, migrated_launches))
}
