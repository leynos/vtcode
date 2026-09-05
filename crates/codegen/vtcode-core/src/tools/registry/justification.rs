/// Tool Justification System
///
/// Captures agent reasoning before high-risk tool execution to improve approval UX
/// and enable learning of approval patterns.
use crate::tools::registry::risk_scorer::RiskLevel;
use anyhow::{Context, Result};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vtcode_commons::VtCodePaths;

/// Justification provided by the agent for executing a high-risk tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolJustification {
    /// Tool being justified
    pub tool_name: String,
    /// Brief explanation from the agent
    pub reason: String,
    /// Expected outcome of tool execution
    pub expected_outcome: Option<String>,
    /// Risk level that triggered justification
    pub risk_level: String,
    /// Timestamp when justification was provided
    pub timestamp: String,
}

impl ToolJustification {
    /// Create a new tool justification
    pub fn new(tool_name: impl Into<String>, reason: impl Into<String>, risk_level: &RiskLevel) -> Self {
        Self {
            tool_name: tool_name.into(),
            reason: reason.into(),
            expected_outcome: None,
            risk_level: format!("{risk_level:?}"),
            timestamp: chrono::Local::now().to_rfc3339(),
        }
    }

    /// Add expected outcome to justification
    pub fn with_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.expected_outcome = Some(outcome.into());
        self
    }

    /// Format justification for display in approval dialog
    pub fn format_for_dialog(&self) -> Vec<String> {
        let mut lines = vec![];

        lines.push(String::new());
        lines.push("Agent Reasoning:".to_owned());

        // Wrap reason text if needed - iterate directly without collecting
        for line in self.reason.lines() {
            let wrapped = textwrap::fill(&format!("  {line}"), 78);
            for wrapped_line in wrapped.lines() {
                lines.push(wrapped_line.to_owned());
            }
        }

        if let Some(outcome) = &self.expected_outcome {
            lines.push(String::new());
            lines.push("Expected Outcome:".to_owned());
            let wrapped = textwrap::fill(&format!("  {outcome}"), 78);
            for wrapped_line in wrapped.lines() {
                lines.push(wrapped_line.to_owned());
            }
        }

        lines.push(String::new());
        lines.push(format!("Risk Level: {}", self.risk_level));

        lines
    }
}

/// Tracks approval patterns to learn from user decisions
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApprovalPattern {
    /// Stable approval key used for lookup and persistence
    pub tool_name: String,
    /// Human-readable label for prompts and summaries
    #[serde(default)]
    pub display_name: Option<String>,
    /// Number of times user approved
    pub approve_count: u32,
    /// Number of times user denied
    pub deny_count: u32,
    /// Last decision (true = approve, false = deny)
    pub last_decision: Option<bool>,
    /// Most recent reason (if available)
    pub recent_reason: Option<String>,
}

impl ApprovalPattern {
    /// Compute approval rate (0.0 to 1.0)
    pub fn approval_rate(&self) -> f32 {
        let total = self.approve_count + self.deny_count;
        if total == 0 {
            0.0
        } else {
            self.approve_count as f32 / total as f32
        }
    }

    /// Check if this tool has high approval rate (>80%)
    pub fn has_high_approval_rate(&self) -> bool {
        self.approval_count() >= 3 && self.approval_rate() > 0.8
    }

    /// Return approval count
    pub fn approval_count(&self) -> u32 {
        self.approve_count
    }

    pub fn display_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.display_name.as_deref().unwrap_or(fallback)
    }
}

/// Merge an on-disk pattern into the in-memory entry by taking the max of
/// counters and preferring any non-`None` metadata from disk. Conservative:
/// undercount is safer (more prompts) than overcount (fewer prompts → risk).
fn merge_pattern_from_disk(local: &mut ApprovalPattern, disk: &ApprovalPattern) {
    local.approve_count = local.approve_count.max(disk.approve_count);
    local.deny_count = local.deny_count.max(disk.deny_count);
    if disk.display_name.is_some() {
        local.display_name = disk.display_name.clone();
    }
    if disk.last_decision.is_some() {
        local.last_decision = disk.last_decision;
    }
    if disk.recent_reason.is_some() {
        local.recent_reason = disk.recent_reason.clone();
    }
}

fn merge_pattern_map(local: &mut HashMap<String, ApprovalPattern>, disk_patterns: HashMap<String, ApprovalPattern>) {
    for (key, disk) in disk_patterns {
        local
            .entry(key)
            .and_modify(|local| merge_pattern_from_disk(local, &disk))
            .or_insert(disk);
    }
}

/// Manager for approval pattern learning and justifications
pub struct JustificationManager {
    cache_dir: PathBuf,
    legacy_pattern_files: Vec<PathBuf>,
    patterns: std::sync::Arc<std::sync::Mutex<HashMap<String, ApprovalPattern>>>,
}

impl JustificationManager {
    /// Create a new justification manager
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::new_with_legacy_pattern_files(cache_dir, Vec::new())
    }

    /// Create a manager that can recover approval patterns from older cache locations.
    pub(crate) fn new_with_legacy_pattern_files(
        cache_dir: PathBuf,
        legacy_pattern_files: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let canonical_pattern_file = cache_dir.join("approval_patterns.json");
        let mut legacy_pattern_files = legacy_pattern_files
            .into_iter()
            .filter(|path| path != &canonical_pattern_file)
            .collect::<Vec<_>>();
        legacy_pattern_files.dedup();

        let patterns = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
        let manager = Self { cache_dir, legacy_pattern_files, patterns };

        // Try to load existing patterns
        let _ = manager.load_patterns();

        manager
    }

    /// Load approval patterns from disk and merge into the in-memory map.
    ///
    /// Merging (rather than replacing) keeps in-memory increments that have not
    /// yet been flushed to disk — important when refreshing right before an
    /// auto-approval check while a concurrent vtcode session may have written
    /// newer counts to the same file.
    fn load_patterns(&self) -> Result<()> {
        let canonical_pattern_file = self.cache_dir.join("approval_patterns.json");
        let canonical_state = read_patterns_file(&canonical_pattern_file)?;
        let canonical_missing = matches!(&canonical_state, PatternFileState::Missing);
        let mut load_error = None;
        match canonical_state {
            PatternFileState::Valid(patterns) => {
                self.merge_loaded_patterns([patterns])?;
                return Ok(());
            }
            PatternFileState::Missing => {}
            PatternFileState::Malformed(error) => load_error = Some(error),
        }

        let mut loaded_patterns = Vec::new();
        for patterns_file in &self.legacy_pattern_files {
            match read_patterns_file(patterns_file) {
                Ok(PatternFileState::Valid(patterns)) => loaded_patterns.push(patterns),
                Ok(PatternFileState::Missing) => {}
                Ok(PatternFileState::Malformed(error)) | Err(error) => load_error = Some(error),
            }
        }

        if loaded_patterns.is_empty() {
            if let Some(error) = load_error {
                return Err(error);
            }
            return Ok(());
        }

        self.merge_loaded_patterns(loaded_patterns)?;

        // The old file remains in place as a rollback source, but make the
        // recovered data available at the canonical path immediately. This
        // also prevents every fresh session from re-reading the legacy file.
        if canonical_missing {
            self.persist_patterns_if_absent()
        } else {
            Ok(())
        }
    }

    fn merge_loaded_patterns<I>(&self, loaded_patterns: I) -> Result<()>
    where
        I: IntoIterator<Item = HashMap<String, ApprovalPattern>>,
    {
        let mut patterns = self
            .patterns
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock patterns: {e}"))?;

        for loaded in loaded_patterns {
            merge_pattern_map(&mut patterns, loaded);
        }
        drop(patterns);

        Ok(())
    }

    /// Re-read patterns from disk, merging with any in-memory state.
    pub fn refresh_patterns(&self) -> Result<()> {
        self.load_patterns()
    }

    /// Get approval pattern for a key
    pub fn get_pattern(&self, approval_key: &str) -> Option<ApprovalPattern> {
        if let Ok(patterns) = self.patterns.lock() {
            patterns.get(approval_key).cloned()
        } else {
            None
        }
    }

    /// Record user approval decision
    pub fn record_decision(
        &self,
        approval_key: &str,
        display_name: Option<&str>,
        approved: bool,
        reason: Option<String>,
    ) {
        let should_persist = if let Ok(mut patterns) = self.patterns.lock() {
            let pattern = patterns.entry(approval_key.to_owned()).or_insert_with(|| ApprovalPattern {
                tool_name: approval_key.to_owned(),
                display_name: display_name.map(str::to_owned),
                approve_count: 0,
                deny_count: 0,
                last_decision: None,
                recent_reason: None,
            });

            if let Some(display_name) = display_name {
                pattern.display_name = Some(display_name.to_owned());
            }

            if approved {
                pattern.approve_count += 1;
            } else {
                pattern.deny_count += 1;
            }

            pattern.last_decision = Some(approved);
            pattern.recent_reason = reason;
            true
        } else {
            false
        };

        // Persist to disk after releasing the lock.
        if should_persist {
            let _ = self.persist_patterns();
        }
    }

    /// Persist patterns to disk
    ///
    /// Clone the patterns under the mutex, then merge and write under the
    /// process-shared file lock outside the mutex.
    fn persist_patterns(&self) -> Result<()> {
        let (patterns_file, mut patterns_snapshot) = self.current_patterns_snapshot()?;
        VtCodePaths::with_private_file_lock(&patterns_file, || {
            if let PatternFileState::Valid(disk_patterns) = read_patterns_file(&patterns_file)? {
                merge_pattern_map(&mut patterns_snapshot, disk_patterns);
            }
            let content = serde_json::to_vec_pretty(&patterns_snapshot)?;
            VtCodePaths::write_private_file_atomic(&patterns_file, &content)
                .context("failed to write approval patterns cache")?;
            Ok(())
        })
    }

    fn persist_patterns_if_absent(&self) -> Result<()> {
        let (patterns_file, content) = self.serialized_patterns()?;
        VtCodePaths::with_private_file_lock(&patterns_file, || {
            VtCodePaths::write_private_file_atomic_if_absent(&patterns_file, &content).map(|_| ())
        })
        .context("failed to publish approval patterns cache")
    }

    fn current_patterns_snapshot(&self) -> Result<(PathBuf, HashMap<String, ApprovalPattern>)> {
        VtCodePaths::ensure_user_dir(&self.cache_dir)?;
        let patterns_snapshot = {
            let patterns = self
                .patterns
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to lock patterns: {e}"))?;
            patterns.clone()
        };
        Ok((self.cache_dir.join("approval_patterns.json"), patterns_snapshot))
    }

    fn serialized_patterns(&self) -> Result<(PathBuf, Vec<u8>)> {
        let (patterns_file, patterns_snapshot) = self.current_patterns_snapshot()?;
        let content = serde_json::to_vec_pretty(&patterns_snapshot)?;
        Ok((patterns_file, content))
    }

    /// Get learning summary for a key
    pub fn get_learning_summary(&self, approval_key: &str) -> Option<String> {
        let pattern = self.get_pattern(approval_key)?;

        if pattern.approval_count() == 0 {
            return None;
        }

        Some(format!(
            "Approved {} of {} times ({:.0}%)",
            pattern.approve_count,
            pattern.approve_count + pattern.deny_count,
            pattern.approval_rate() * 100.0
        ))
    }
}

enum PatternFileState {
    Missing,
    Valid(HashMap<String, ApprovalPattern>),
    Malformed(anyhow::Error),
}

fn read_patterns_file(path: &Path) -> Result<PatternFileState> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(PatternFileState::Missing),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect approval patterns cache {}", path.display()));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        anyhow::bail!("approval patterns cache is not a regular file: {}", path.display());
    }

    let content = match String::from_utf8(VtCodePaths::read_file_no_follow(path)?) {
        Ok(content) => content,
        Err(error) => {
            return Ok(PatternFileState::Malformed(anyhow::anyhow!(
                "failed to read approval patterns cache {} as UTF-8: {error}",
                path.display()
            )));
        }
    };
    match serde_json::from_str::<HashMap<String, ApprovalPattern>>(&content) {
        Ok(patterns) => Ok(PatternFileState::Valid(patterns)),
        Err(error) => Ok(PatternFileState::Malformed(anyhow::anyhow!(
            "failed to parse approval patterns cache {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_justification_creation() {
        let just = ToolJustification::new("read_file", "Need to understand code structure", &RiskLevel::Low)
            .with_outcome("Will analyse the AST to provide better context");

        assert_eq!(just.tool_name, "read_file");
        assert!(just.reason.contains("understand"));
        assert!(just.expected_outcome.is_some());
    }

    #[test]
    fn test_justification_formatting() {
        let just =
            ToolJustification::new("run_command", "Execute build to check for compilation errors", &RiskLevel::High)
                .with_outcome("Will produce build output for analysis");

        let formatted = just.format_for_dialog();
        assert!(formatted.iter().any(|l| l.contains("Agent Reasoning")));
        assert!(formatted.iter().any(|l| l.contains("Expected Outcome")));
        assert!(formatted.iter().any(|l| l.contains("Risk Level")));
    }

    #[test]
    fn test_approval_pattern_calculation() {
        let mut pattern = ApprovalPattern {
            tool_name: "read_file".to_owned(),
            display_name: None,
            approve_count: 9,
            deny_count: 1,
            last_decision: Some(true),
            recent_reason: None,
        };

        assert!((pattern.approval_rate() - 0.9).abs() < f32::EPSILON);
        assert!(pattern.has_high_approval_rate());

        pattern.approve_count = 3;
        pattern.deny_count = 7;
        assert!(!pattern.has_high_approval_rate()); // < 0.8 rate
    }

    #[test]
    fn test_justification_manager_basic() {
        let temp_dir = std::env::temp_dir().join(format!("vtcode_test_{}", std::process::id()));
        let manager = JustificationManager::new(temp_dir.clone());

        manager.record_decision("read_file", Some("Read File"), true, None);
        manager.record_decision("read_file", Some("Read File"), true, None);
        manager.record_decision("read_file", Some("Read File"), false, None);

        let pattern = manager.get_pattern("read_file").unwrap();
        assert_eq!(pattern.approve_count, 2);
        assert_eq!(pattern.deny_count, 1);
        assert!((pattern.approval_rate() - 2.0 / 3.0).abs() < f32::EPSILON);
        assert_eq!(pattern.display_name.as_deref(), Some("Read File"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_justification_manager_preserves_new_display_name() {
        let temp_dir = std::env::temp_dir().join(format!("vtcode_test_{}", std::process::id()));
        let manager = JustificationManager::new(temp_dir.clone());

        manager.record_decision("shell:key", Some("command `cargo test`"), true, None);
        manager.record_decision("shell:key", Some("commands starting with `cargo`"), true, None);

        let pattern = manager.get_pattern("shell:key").unwrap();
        assert_eq!(pattern.display_name.as_deref(), Some("commands starting with `cargo`"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
