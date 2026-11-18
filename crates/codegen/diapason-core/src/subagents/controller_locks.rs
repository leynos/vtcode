//! Mutexes shared by one controller tree.
//!
//! Two controller-level exclusions used to be process-global statics: the
//! adoption lock that serialises subtree grafting and id allocation, and the
//! persistence lock that serialises background-ledger saves. They work, but a
//! static cannot be injected or observed independently. This scope owns both
//! mutexes so a controller can be given its own tree, while the default still
//! resolves to one process-global instance. That default preserves the old
//! guarantee that controllers created without an explicit scope exclude each
//! other across the whole process.
use std::sync::{Arc, Mutex, OnceLock};

/// The adoption and persistence mutexes one controller tree shares.
///
/// A child-scoped controller adopts its parent's scope, so a whole tree
/// serialises grafting and ledger writes against itself. Controllers created
/// without an explicit scope share one process-global instance, returned by
/// `default_lock_scope`.
///
/// Controllers that can graft into one another must therefore share one
/// scope: an injected, distinct scope only serialises the controllers given
/// it. Production controllers leave the config field `None`, so they all take
/// the shared default and keep the original cross-tree exclusion.
pub struct SubagentLockScope {
    /// Serialises subtree adoption and child-id allocation.
    ///
    /// Adopting takes the joining slot's write guard and the target's read
    /// guard, a symmetric pair that would deadlock if two controllers were
    /// attached in opposing directions at once. Allocation holds the same lock
    /// so a scope cannot forward to another tree between resolving the chain
    /// and reserving an id.
    adoption: Mutex<()>,
    /// Serialises background-ledger saves so the last write is the newest.
    persistence: tokio::sync::Mutex<()>,
}

impl SubagentLockScope {
    /// Creates a fresh, independent set of controller locks.
    #[must_use]
    pub fn new() -> Self {
        Self {
            adoption: Mutex::new(()),
            persistence: tokio::sync::Mutex::new(()),
        }
    }

    /// Returns the adoption lock this tree holds.
    pub(crate) fn adoption(&self) -> &Mutex<()> {
        &self.adoption
    }

    /// Returns the background-ledger persistence lock this tree holds.
    pub(crate) fn persistence(&self) -> &tokio::sync::Mutex<()> {
        &self.persistence
    }
}

impl Default for SubagentLockScope {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the process-global scope used when a controller was not given one.
///
/// Keeping one shared instance is what makes controllers without an injected
/// scope exclude each other exactly as the former statics did; it is not a
/// fallback that weakens the guarantee.
pub(crate) fn default_lock_scope() -> Arc<SubagentLockScope> {
    static DEFAULT: OnceLock<Arc<SubagentLockScope>> = OnceLock::new();
    Arc::clone(DEFAULT.get_or_init(|| Arc::new(SubagentLockScope::new())))
}
