use serde::{Deserialize, Serialize};

/// The bounded categories emitted for planning validation rejections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanValidationRejectionReason {
    PlaceholderToken,
}

/// Metrics for plan validation failures that reach a definitive boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanningValidationMetrics {
    pub placeholder_token_rejections: u64,
}

impl PlanningValidationMetrics {
    pub fn record_rejection(&mut self, reason: PlanValidationRejectionReason) {
        match reason {
            PlanValidationRejectionReason::PlaceholderToken => {
                self.placeholder_token_rejections = self.placeholder_token_rejections.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PlanValidationRejectionReason;
    use crate::metrics::MetricsCollector;

    #[test]
    fn placeholder_rejection_is_bounded_in_summary_json_and_prometheus() {
        let collector = MetricsCollector::new();
        collector.record_plan_validation_rejection(PlanValidationRejectionReason::PlaceholderToken);
        collector.record_plan_validation_rejection(PlanValidationRejectionReason::PlaceholderToken);

        let summary = collector.get_summary();
        assert_eq!(summary.planning.placeholder_token_rejections, 2);

        let json = collector.export_json().expect("metrics JSON should serialize");
        assert_eq!(json["planning"]["placeholder_token_rejections"], 2);

        let prometheus = collector.export_prometheus();
        let series = "vtcode_plan_validation_rejected_total{reason=\"placeholder_token\"}";
        assert_eq!(prometheus.matches(series).count(), 1);
        assert!(prometheus.contains(&format!("{series} 2")));
    }
}
