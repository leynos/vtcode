//! Test-only setters for the publication and compaction dependencies.
//!
//! Production wiring installs the process-wide recorders, clock, and activity
//! source. A command test substitutes values it owns so another attempt cannot
//! satisfy or perturb what it observes.

use super::ZedAgent;
use super::compaction::CompactionActivitySource;
use super::publication_clock::PublicationClock;
use std::sync::Arc;

impl ZedAgent {
    /// Install isolated publication recorders, for tests that assert exact
    /// deltas.
    ///
    /// Production wiring installs the process-wide recorders; a command test
    /// substitutes recorders it owns so a concurrent publisher cannot perturb
    /// the counts it observes.
    pub(crate) fn set_publication_recorders(
        &mut self,
        notice: Arc<dyn crate::metrics::PublicationRecorder>,
        compaction_update: Arc<dyn crate::metrics::PublicationRecorder>,
    ) {
        self.notice_recorder = notice;
        self.compaction_update_recorder = compaction_update;
    }

    /// Install an isolated compaction recorder, for tests that assert exact
    /// deltas rather than a shared process-wide snapshot.
    ///
    /// Production wiring installs the process-wide recorder; a command test
    /// substitutes a recorder it owns so another attempt cannot satisfy or
    /// perturb the counts it observes.
    pub(crate) fn set_compaction_recorder(&mut self, recorder: Arc<dyn crate::metrics::CompactionRecorder>) {
        self.compaction_recorder = recorder;
    }

    /// Install a controlled activity identity and clock, for tests that assert
    /// the published activity and its duration exactly.
    ///
    /// Production wiring installs the system source; a command test substitutes
    /// fixed values so the assertions do not depend on the wall clock or on a
    /// randomly generated identifier.
    pub(in crate::zed::agent) fn set_compaction_activity_source(&mut self, source: Arc<dyn CompactionActivitySource>) {
        self.compaction_activity_source = source;
    }

    /// Install a controlled publication clock, for tests that assert the
    /// elapsed time a send reports exactly.
    ///
    /// Production wiring installs the system clock; a command test substitutes
    /// a stepped clock so the span's `elapsed_ms` is the injected interval
    /// rather than however long the transport actually took.
    pub(in crate::zed::agent) fn set_publication_clock(&mut self, clock: Arc<dyn PublicationClock>) {
        self.publication_clock = clock;
    }
}
