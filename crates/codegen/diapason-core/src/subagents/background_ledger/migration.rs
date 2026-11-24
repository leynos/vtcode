//! Durable evidence for launches a legacy owner migration attributed to a session.
//!
//! The owner migration rewrites an ownerless ledger row in place. A controller
//! that cached the ownerless copy before the migration therefore holds a launch
//! whose durable row now carries another id, and once an owned replacement
//! supersedes the migrated row no row is left that names the cached launch. The
//! evidence recorded here keeps that launch identity across saves so the stale
//! ownerless alias can be recognised and dropped without suppressing an
//! independent ownerless launch of the same agent, which names a different exec
//! session.
use serde::{Deserialize, Serialize};

/// One launch a legacy migration has already attributed to an owning session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct MigratedBackgroundLaunch {
    /// The ownerless id the launch carried before the migration.
    pub(crate) base_id: String,
    /// The exec session that identifies the migrated launch.
    pub(crate) exec_session_id: String,
}

/// Deduplicates migration evidence without ever evicting a launch.
///
/// Evidence must be retained for as long as a controller that cached the
/// ownerless copy can still save it. That window has no finite bound: a stale
/// controller may save at any later time, however many migrations have run in
/// between, so any fixed-size window eventually drops the evidence for a
/// launch that can still reappear and lets it resurface as an ownerless
/// duplicate beside its migrated row. Entries are therefore kept for the life
/// of the ledger, in encounter order, and only exact duplicates collapse.
/// Growth is bounded by the number of distinct launches ever migrated, because
/// a launch's id and exec session are unique and never reused.
pub(super) fn retained_migrated_launches(
    launches: impl IntoIterator<Item = MigratedBackgroundLaunch>,
) -> Vec<MigratedBackgroundLaunch> {
    let mut retained: Vec<MigratedBackgroundLaunch> = Vec::new();
    for launch in launches {
        if !retained.contains(&launch) {
            retained.push(launch);
        }
    }
    retained
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(base_id: &str, exec_session_id: &str) -> MigratedBackgroundLaunch {
        MigratedBackgroundLaunch {
            base_id: base_id.to_string(),
            exec_session_id: exec_session_id.to_string(),
        }
    }

    /// Evidence is deduplicated and never evicted.
    ///
    /// A stale controller can save its ownerless copy at any later time, so
    /// dropping the oldest evidence would let an early launch reappear as an
    /// ownerless duplicate beside its migrated row. Only the exact duplicate
    /// collapses; every distinct launch survives, including the first.
    #[test]
    fn evidence_is_deduplicated_and_never_evicted() {
        let entries = (0..5_000)
            .map(|index| launch("background-worker", &format!("exec-{index}")))
            .chain([launch("background-worker", "exec-0")])
            .collect::<Vec<_>>();

        let retained = retained_migrated_launches(entries);

        assert_eq!(retained.len(), 5_000, "only the duplicate entry collapses");
        assert_eq!(
            retained.first().map(|entry| entry.exec_session_id.as_str()),
            Some("exec-0"),
            "the oldest evidence must survive the newest migrations"
        );
        assert_eq!(retained.last().map(|entry| entry.exec_session_id.as_str()), Some("exec-4999"));
    }
}
