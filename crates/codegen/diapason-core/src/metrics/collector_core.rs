//! Construction, queries, and summary export for the central metrics collector.
//!
//! The collector's storage fields are defined alongside it in the parent
//! module; this file owns the lifecycle and read-side methods that build and
//! sample that storage.

use std::sync::atomic::Ordering;

use super::*;

impl MetricsCollector {
    /// Create a new metrics collector.
    ///
    /// Admission counters are sampled from the real process-wide provider
    /// source, so a collector built through this entry point never exports a
    /// permanently zeroed admission snapshot in production. Callers that need a
    /// different source (tests, alternate hosts) use
    /// [`Self::with_admission_metrics_source`] instead.
    pub fn new() -> Self {
        Self::with_admission_metrics_source(Arc::new(
            crate::llm::admission_metrics::ProcessProviderAdmissionMetricsSource::new(),
        ))
    }

    /// Create a metrics collector that samples admission counters from `source`.
    ///
    /// This is the injection seam: production supplies the concrete provider
    /// adapter through [`Self::new`], while tests supply a fake source to make
    /// the exported values deterministic.
    pub fn with_admission_metrics_source(source: Arc<dyn ProviderAdmissionMetricsSource>) -> Self {
        Self {
            discovery: Arc::new(Mutex::new(DiscoveryMetrics::new())),
            execution: Arc::new(Mutex::new(ExecutionMetrics::new())),
            sdk: Arc::new(Mutex::new(SdkMetrics::new())),
            filtering: Arc::new(Mutex::new(FilteringMetrics::new())),
            patch_safety: Arc::new(Mutex::new(PatchSafetyMetrics::default())),
            tool_recovery: Arc::new(Mutex::new(ToolRecoveryMetrics::default())),
            planning: Arc::new(Mutex::new(PlanningValidationMetrics::default())),
            skills: Arc::new(Mutex::new(SkillMetrics::new())),
            security: Arc::new(Mutex::new(SecurityMetrics::new())),
            provider_requests: Arc::new(Mutex::new(ProviderRequestMetrics::default())),
            service_startup: Arc::new(Mutex::new(ServiceStartupMetrics::default())),
            task_lifecycle_notification_queue: Arc::new(Mutex::new(TaskLifecycleNotificationQueueMetrics::default())),
            acp_extension: Arc::new(Mutex::new(acp_metrics::AcpExtensionState::default())),
            workspace_runtime_cache_entries: Arc::new(AtomicU64::new(0)),
            admission_metrics_source: source,
            start_time: Instant::now(),
        }
    }

    /// Get the bounded aggregate provider-request metrics snapshot.
    ///
    /// The `custom_provider_*` fields are copied from the injected
    /// [`ProviderAdmissionMetricsSource`] on every call, so the sample is taken
    /// at read time rather than cached at construction. The counters are
    /// process-wide and shared across collectors, not scoped to this instance.
    ///
    /// Consistency: the source returns one [`ProviderAdmissionSnapshot`] per
    /// call, but the snapshot's own fields are loaded independently, so the
    /// result is a point-in-time sample that may mix instants under concurrent
    /// admission activity. It is never a transactional view.
    pub fn get_provider_request_metrics(&self) -> ProviderRequestMetrics {
        let admission = self.admission_metrics_source.snapshot();
        let mut metrics = self.provider_requests.lock().clone();
        metrics.custom_provider_queue_depth = admission.queue_depth;
        metrics.custom_provider_in_flight = admission.in_flight;
        metrics.custom_provider_queue_waits = admission.queue_waits;
        metrics.custom_provider_queue_wait_ms = admission.queue_wait_ms;
        metrics.custom_provider_queue_timeouts = admission.queue_timeouts;
        metrics
    }

    /// Get the bounded aggregate MCP startup metrics snapshot.
    pub fn get_service_startup_metrics(&self) -> ServiceStartupMetrics {
        self.service_startup.lock().clone()
    }

    /// Get aggregate measurements for the task-lifecycle notification queue.
    pub fn get_task_lifecycle_notification_queue_metrics(&self) -> TaskLifecycleNotificationQueueMetrics {
        self.task_lifecycle_notification_queue.lock().clone()
    }

    /// Set the current number of cached entries for a runtime cache kind.
    pub fn set_runtime_cache_entries(&self, kind: RuntimeCacheKind, entries: usize) {
        match kind {
            RuntimeCacheKind::Workspace => {}
        }
        let entries = u64::try_from(entries).unwrap_or(u64::MAX);
        self.workspace_runtime_cache_entries.store(entries, Ordering::Relaxed);
    }

    /// Get the current number of entries in the workspace runtime cache.
    pub fn get_runtime_cache_entries(&self, kind: RuntimeCacheKind) -> u64 {
        match kind {
            RuntimeCacheKind::Workspace => self.workspace_runtime_cache_entries.load(Ordering::Relaxed),
        }
    }

    // ========== Queries ==========

    /// Get current discovery metrics snapshot
    pub fn get_discovery_metrics(&self) -> DiscoveryMetrics {
        self.discovery.lock().clone()
    }

    /// Get current execution metrics snapshot
    pub fn get_execution_metrics(&self) -> ExecutionMetrics {
        self.execution.lock().clone()
    }

    /// Get current SDK metrics snapshot
    pub fn get_sdk_metrics(&self) -> SdkMetrics {
        self.sdk.lock().clone()
    }

    /// Get current filtering metrics snapshot
    pub fn get_filtering_metrics(&self) -> FilteringMetrics {
        self.filtering.lock().clone()
    }

    /// Get the current planning validation metrics snapshot.
    pub fn get_planning_metrics(&self) -> PlanningValidationMetrics {
        self.planning.lock().clone()
    }

    /// Get the current `apply_patch` safety metrics snapshot.
    pub fn get_patch_safety_metrics(&self) -> PatchSafetyMetrics {
        self.patch_safety.lock().clone()
    }

    /// Get the current tool-recovery metrics snapshot.
    pub fn get_tool_recovery_metrics(&self) -> ToolRecoveryMetrics {
        self.tool_recovery.lock().clone()
    }

    /// Get current skill metrics snapshot
    pub fn get_skill_metrics(&self) -> SkillMetrics {
        self.skills.lock().clone()
    }

    /// Get current security metrics snapshot
    pub fn get_security_metrics(&self) -> SecurityMetrics {
        self.security.lock().clone()
    }

    /// Get comprehensive summary of all metrics
    pub fn get_summary(&self) -> MetricsSummary {
        MetricsSummary {
            timestamp: Utc::now(),
            session_duration_ms: self.start_time.elapsed().as_millis().min(u64::MAX as u128) as u64,
            discovery: self.get_discovery_metrics(),
            execution: self.get_execution_metrics(),
            sdk: self.get_sdk_metrics(),
            filtering: self.get_filtering_metrics(),
            patch_safety: self.get_patch_safety_metrics(),
            tool_recovery: self.get_tool_recovery_metrics(),
            planning: self.get_planning_metrics(),
            skills: self.get_skill_metrics(),
            security: self.get_security_metrics(),
            provider_requests: self.get_provider_request_metrics(),
            mcp_startup: self.get_service_startup_metrics(),
            task_lifecycle_notification_queue: self.get_task_lifecycle_notification_queue_metrics(),
            workspace_runtime_cache_entries: self.get_runtime_cache_entries(RuntimeCacheKind::Workspace),
        }
    }

    // ========== Export ==========

    /// Export metrics as JSON.
    ///
    /// The Lody ACP extension metrics are attached to the exported document
    /// rather than carried on [`MetricsSummary`]. Callers that build or
    /// destructure the typed summary therefore keep the struct's previous
    /// source-compatible shape, while the JSON export still publishes the
    /// extension family as a top-level `acp_extension` member.
    pub fn export_json(&self) -> anyhow::Result<serde_json::Value> {
        let summary = self.get_summary();
        let mut document = serde_json::to_value(summary)?;
        let object = document
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("metrics summary must serialize as a JSON object"))?;
        object.insert("acp_extension".to_string(), serde_json::to_value(self.get_acp_extension_metrics())?);
        Ok(document)
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}
