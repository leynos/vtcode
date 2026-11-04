//! Tool call batching for parallel execution.
//!
//! Groups tool calls into sequential and parallel batches based on their
//! read-only classification and preflight safety status. Read-only tools
//! can be parallelized; mutating tools must be sequential.

use serde_json::Value;

/// A single step in a fallback chain.
#[derive(Debug, Clone)]
pub struct FallbackStep {
    pub tool_name: String,
    pub args: Value,
}

/// A recommended fallback tool call if the primary fails.
#[derive(Debug, Clone)]
pub struct FallbackRecommendation {
    pub tool_name: String,
    pub args: Value,
    /// Ordered chain of additional fallbacks to try if the primary
    /// `tool_name`/`args` fails.  Each step is tried in sequence.
    pub chain: Vec<FallbackStep>,
}

impl FallbackRecommendation {
    /// Returns `true` when the recommendation carries a non-empty tool name
    /// so downstream callers can skip invalid fallbacks early.
    pub fn is_valid(&self) -> bool {
        !self.tool_name.trim().is_empty()
    }

    /// Total number of fallback steps including the primary.
    pub fn step_count(&self) -> usize {
        1 + self.chain.len()
    }

    /// Iterate all fallback steps: primary first, then chain.
    pub fn steps(&self) -> impl Iterator<Item = FallbackStep> {
        std::iter::once(FallbackStep {
            tool_name: self.tool_name.clone(),
            args: self.args.clone(),
        })
        .chain(self.chain.iter().cloned())
    }
}

/// A prepared tool call with its classification and effective arguments.
#[derive(Debug, Clone)]
pub struct PreparedToolCall {
    pub canonical_name: String,
    pub readonly_classification: bool,
    pub parallel_safe_after_preflight: bool,
    pub effective_args: Value,
    pub fallback_recommendation: Option<FallbackRecommendation>,
    pub already_preflighted: bool,
}

impl PreparedToolCall {
    pub fn new(
        canonical_name: String,
        readonly_classification: bool,
        parallel_safe_after_preflight: bool,
        effective_args: Value,
    ) -> Self {
        Self {
            canonical_name,
            readonly_classification,
            parallel_safe_after_preflight,
            effective_args,
            fallback_recommendation: None,
            already_preflighted: true,
        }
    }

    pub fn with_fallback_recommendation(mut self, fallback_recommendation: Option<FallbackRecommendation>) -> Self {
        self.fallback_recommendation = fallback_recommendation;
        self
    }

    pub fn can_parallelize(&self) -> bool {
        self.readonly_classification && self.parallel_safe_after_preflight
    }
}

/// Whether a batch of tool calls should be executed sequentially or in parallel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedToolBatchKind {
    Sequential,
    ParallelReadonly,
}

/// A batch of tool calls grouped by execution strategy.
#[derive(Debug, Clone)]
pub struct PreparedToolBatch {
    pub kind: PreparedToolBatchKind,
    pub calls: Vec<PreparedToolCall>,
}

impl PreparedToolBatch {
    pub fn plan_layout(
        parallelizable: impl IntoIterator<Item = bool>,
        allow_parallel: bool,
    ) -> Vec<(PreparedToolBatchKind, usize)> {
        Self::plan_layout_with_limit(parallelizable, allow_parallel, 0)
    }

    /// Plans contiguous execution groups while respecting an optional
    /// parallel fan-out limit. A limit of zero means unlimited parallelism.
    pub fn plan_layout_with_limit(
        parallelizable: impl IntoIterator<Item = bool>,
        allow_parallel: bool,
        max_parallel: usize,
    ) -> Vec<(PreparedToolBatchKind, usize)> {
        let mut layout = Vec::new();
        let mut parallel_batch_len = 0usize;

        for can_parallelize in parallelizable {
            if allow_parallel && can_parallelize {
                parallel_batch_len += 1;
                continue;
            }

            if parallel_batch_len > 0 {
                push_parallel_batch_layout(&mut layout, &mut parallel_batch_len, max_parallel);
            }
            layout.push((PreparedToolBatchKind::Sequential, 1));
        }

        push_parallel_batch_layout(&mut layout, &mut parallel_batch_len, max_parallel);

        layout
    }

    /// Compatibility wrapper for callers that still provide tool names.
    ///
    /// Scheduling is based only on the validated parallel-safety bit; tool
    /// names never affect the layout. New callers should use
    /// [`Self::plan_layout`] or [`Self::plan_layout_with_limit`].
    #[deprecated(note = "tool names do not affect scheduling; use plan_layout")]
    pub fn plan_layout_with_names<'a>(
        calls: impl IntoIterator<Item = (bool, &'a str)>,
        allow_parallel: bool,
    ) -> Vec<(PreparedToolBatchKind, usize)> {
        Self::plan_layout(calls.into_iter().map(|(can_parallelize, _)| can_parallelize), allow_parallel)
    }

    pub fn plan(calls: impl IntoIterator<Item = PreparedToolCall>, allow_parallel: bool) -> Vec<Self> {
        Self::plan_with_limit(calls, allow_parallel, 0)
    }

    pub fn plan_with_limit(
        calls: impl IntoIterator<Item = PreparedToolCall>,
        allow_parallel: bool,
        max_parallel: usize,
    ) -> Vec<Self> {
        let calls: Vec<_> = calls.into_iter().collect();
        let layout = Self::plan_layout_with_limit(
            calls.iter().map(PreparedToolCall::can_parallelize),
            allow_parallel,
            max_parallel,
        );
        let mut calls = calls.into_iter();

        layout
            .into_iter()
            .map(|(kind, len)| Self { kind, calls: calls.by_ref().take(len).collect() })
            .collect()
    }
}

fn push_parallel_batch_layout(
    layout: &mut Vec<(PreparedToolBatchKind, usize)>,
    parallel_batch_len: &mut usize,
    max_parallel: usize,
) {
    let mut remaining = *parallel_batch_len;
    let effective_limit = if max_parallel == 0 { usize::MAX } else { max_parallel };

    while remaining > 0 {
        let batch_len = remaining.min(effective_limit);
        match batch_len {
            1 => layout.push((PreparedToolBatchKind::Sequential, 1)),
            len => layout.push((PreparedToolBatchKind::ParallelReadonly, len)),
        }
        remaining -= batch_len;
    }

    *parallel_batch_len = 0;
}

/// Whether all calls in a batch are parallel-safe.
pub fn is_parallel_safe_tool_batch(calls: &[PreparedToolCall]) -> bool {
    calls.iter().all(PreparedToolCall::can_parallelize)
}

#[cfg(test)]
mod tests {
    use super::{PreparedToolBatch, PreparedToolBatchKind};

    #[test]
    fn bounded_layout_splits_parallel_groups_without_reordering() {
        let layout = PreparedToolBatch::plan_layout_with_limit([true, true, true, false, true], true, 2);

        assert_eq!(
            layout,
            vec![
                (PreparedToolBatchKind::ParallelReadonly, 2),
                (PreparedToolBatchKind::Sequential, 1),
                (PreparedToolBatchKind::Sequential, 1),
                (PreparedToolBatchKind::Sequential, 1),
            ]
        );
    }

    #[test]
    fn single_parallel_call_is_serialized_when_limit_is_one() {
        let layout = PreparedToolBatch::plan_layout_with_limit([true], true, 1);

        assert_eq!(layout, vec![(PreparedToolBatchKind::Sequential, 1)]);
    }
}
