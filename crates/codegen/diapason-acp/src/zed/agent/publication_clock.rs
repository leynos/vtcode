//! Supplies the monotonic clock that times ACP publication sends.
//!
//! This is a driven port. Both publication commands - the provider rate-limit
//! notice and the compaction activity update - report how long their transport
//! send took, and that duration must be a function of explicit inputs rather
//! than of the ambient process clock. Production wiring installs the system
//! edge; a command test substitutes a stepped clock and asserts the exact
//! reported duration.

use std::sync::Arc;
use std::time::Instant;

/// Supplies monotonic instants to the publication commands.
pub(super) trait PublicationClock: Send + Sync {
    /// Return the current monotonic instant.
    fn now(&self) -> Instant;
}

/// System implementation of [`PublicationClock`].
///
/// This is the edge: it is the only place in the publication path that reads
/// the process clock.
struct SystemPublicationClock;

impl PublicationClock for SystemPublicationClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Return the process-wide publication clock.
pub(super) fn publication_clock() -> Arc<dyn PublicationClock> {
    Arc::new(SystemPublicationClock)
}
