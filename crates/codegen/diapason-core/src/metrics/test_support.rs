//! Test-only admission source that keeps collector counters inert.

use super::{ProviderAdmissionMetricsSource, ProviderAdmissionSnapshot};

/// Inert admission source used only by tests that do not care about admission.
///
/// Production never installs this: it exists so unit tests outside the metrics
/// module can build a collector without reading process-wide provider counters.
#[cfg(test)]
#[derive(Debug)]
pub struct ZeroProviderAdmissionMetricsSource;

#[cfg(test)]
impl ProviderAdmissionMetricsSource for ZeroProviderAdmissionMetricsSource {
    fn snapshot(&self) -> ProviderAdmissionSnapshot {
        ProviderAdmissionSnapshot::default()
    }
}
