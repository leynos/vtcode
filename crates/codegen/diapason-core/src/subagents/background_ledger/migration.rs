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

/// How many migration records are retained.
///
/// Evidence is only needed while a controller that cached the ownerless copy
/// can still save, so a generous bound keeps the ledger small without dropping
/// recent migrations. Entries are deduplicated and kept in encounter order.
pub(super) const MIGRATED_LAUNCH_LIMIT: usize = 256;

/// Deduplicates migration evidence and keeps the most recent entries.
///
/// Evidence is appended in encounter order, so the newest launches are the
/// last entries; dropping the oldest keeps the window in which a stale
/// controller can still save. A launch id is never reused, so a duplicate
/// entry adds nothing and is collapsed.
pub(super) fn retained_migrated_launches(
    launches: impl IntoIterator<Item = MigratedBackgroundLaunch>,
) -> Vec<MigratedBackgroundLaunch> {
    let mut retained: Vec<MigratedBackgroundLaunch> = Vec::new();
    for launch in launches {
        if !retained.contains(&launch) {
            retained.push(launch);
        }
    }
    let keep_from = retained.len().saturating_sub(MIGRATED_LAUNCH_LIMIT);
    retained.split_off(keep_from)
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

    /// Evidence is deduplicated and keeps the most recently encountered launches.
    #[test]
    fn evidence_is_deduplicated_and_bounded() {
        let entries = (0..MIGRATED_LAUNCH_LIMIT + 3)
            .map(|index| launch("background-worker", &format!("exec-{index}")))
            .chain([launch("background-worker", "exec-0")])
            .collect::<Vec<_>>();

        let retained = retained_migrated_launches(entries);

        assert_eq!(retained.len(), MIGRATED_LAUNCH_LIMIT);
        assert_eq!(retained.first().map(|entry| entry.exec_session_id.as_str()), Some("exec-3"));
        assert_eq!(retained.last().map(|entry| entry.exec_session_id.as_str()), Some("exec-258"));
    }
}
