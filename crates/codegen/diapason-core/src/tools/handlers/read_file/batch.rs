//! Batch read orchestration and progress state.
//!
//! This module owns only batch admission, bounded fan-out, result ordering, and
//! response assembly. Indentation-aware range semantics remain behind the
//! `ReadFileHandler::read_range` interface in the parent module; compatible
//! slice ranges share one bounded read context.

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use anyhow::{Result, bail};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ReadFileHandler, ReadRange, defaults};

/// Batch read arguments accepted by the `read_file` tool.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct BatchReadArgs {
    /// List of read requests to execute in parallel.
    pub reads: Vec<BatchReadRequest>,
    /// Maximum concurrent file reads (default: 8).
    #[serde(default = "defaults::max_concurrency")]
    pub max_concurrency: usize,
    /// Whether to show progress in UI (default: true).
    #[serde(default = "defaults::ui_progress")]
    pub ui_progress: bool,
}

/// A single file read request within a batch.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct BatchReadRequest {
    /// Absolute path to the file to read.
    pub file_path: String,
    /// Single range to read (mutually exclusive with `ranges`).
    #[serde(flatten)]
    pub range: Option<ReadRange>,
    /// Multiple ranges to read from the same file.
    #[serde(default)]
    pub ranges: Option<Vec<ReadRange>>,
}

/// Result for a single file read in batch mode.
#[derive(Serialize, Clone, Debug)]
pub struct BatchReadResult {
    /// The file path that was read.
    pub file_path: String,
    /// Results for each range read.
    pub ranges: Vec<RangeResult>,
    /// Error if the entire file read failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result for a single range read.
#[derive(Serialize, Clone, Debug)]
pub struct RangeResult {
    /// Starting line offset.
    pub offset: usize,
    /// Lines actually read.
    pub lines_read: usize,
    /// Whether content was condensed.
    pub condensed: bool,
    /// Number of lines omitted if condensed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted_lines: Option<usize>,
    /// Whether a physical source line exceeded the bounded reader limit.
    #[serde(skip_serializing_if = "is_false")]
    pub line_truncated: bool,
    /// The content read.
    pub content: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Progress tracking for batch reads.
#[derive(Clone)]
pub struct BatchProgress {
    /// Total number of files to read.
    pub total_files: Arc<AtomicUsize>,
    /// Number of files completed.
    pub completed_files: Arc<AtomicUsize>,
    /// Current file being read.
    pub current_file: Arc<tokio::sync::RwLock<String>>,
    /// Total bytes to read (estimated).
    pub total_bytes: Arc<AtomicU64>,
    /// Bytes read so far.
    pub bytes_read: Arc<AtomicU64>,
}

impl BatchProgress {
    pub fn new(total_files: usize) -> Self {
        Self {
            total_files: Arc::new(AtomicUsize::new(total_files)),
            completed_files: Arc::new(AtomicUsize::new(0)),
            current_file: Arc::new(tokio::sync::RwLock::new(String::new())),
            total_bytes: Arc::new(AtomicU64::new(0)),
            bytes_read: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn file_started(&self, file_path: &str) {
        let mut current = self.current_file.write().await;
        *current = file_path.to_string();
    }

    pub fn file_completed(&self) {
        self.completed_files.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_bytes(&self, bytes: u64) {
        self.bytes_read.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn progress_percent(&self) -> f64 {
        let completed = self.completed_files.load(Ordering::Relaxed);
        let total = self.total_files.load(Ordering::Relaxed);
        if total == 0 {
            100.0
        } else {
            (completed as f64 / total as f64) * 100.0
        }
    }

    pub async fn status_line(&self) -> (String, String) {
        let completed = self.completed_files.load(Ordering::Relaxed);
        let total = self.total_files.load(Ordering::Relaxed);
        let current = self.current_file.read().await;
        let file_name = PathBuf::from(current.as_str())
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| current.clone());

        let left = format!("Reading {}/{}: {}", completed + 1, total, file_name);
        let right = format!("{:.0}%", self.progress_percent());
        (left, right)
    }
}

pub(super) async fn execute(handler: &ReadFileHandler, args: BatchReadArgs) -> Result<Value> {
    if args.reads.is_empty() {
        return Ok(json!({
            "success": false,
            "error": "No read requests provided"
        }));
    }

    if args.max_concurrency == 0 {
        bail!("max_concurrency must be greater than zero");
    }

    // A concurrency value larger than the batch cannot add useful work and
    // should not size the stream's in-flight buffer.
    let max_concurrency = args.max_concurrency.min(args.reads.len());
    let progress = BatchProgress::new(args.reads.len());
    let mut indexed_results: Vec<(usize, BatchReadResult)> = stream::iter(args.reads.into_iter().enumerate())
        .map(|(index, request)| {
            let progress = progress.clone();
            async move {
                progress.file_started(&request.file_path).await;
                let result = read_single_request(handler, &request).await;
                progress.file_completed();
                (index, result)
            }
        })
        .buffer_unordered(max_concurrency)
        .collect()
        .await;
    indexed_results.sort_unstable_by_key(|(index, _)| *index);
    let results: Vec<BatchReadResult> = indexed_results.into_iter().map(|(_, result)| result).collect();

    let content = assemble_content(&results);
    let all_success = results.iter().all(|result| result.error.is_none());
    Ok(json!({
        "success": all_success,
        "content": content,
        "items": results,
        "files_read": results.len(),
        "files_succeeded": results.iter().filter(|result| result.error.is_none()).count(),
        "no_spool": true
    }))
}

async fn read_single_request(handler: &ReadFileHandler, request: &BatchReadRequest) -> BatchReadResult {
    let path = PathBuf::from(&request.file_path);
    if !path.is_absolute() {
        return BatchReadResult {
            file_path: request.file_path.clone(),
            ranges: vec![],
            error: Some("file_path must be an absolute path".to_string()),
        };
    }

    let ranges = request
        .ranges
        .clone()
        .or_else(|| request.range.clone().map(|range| vec![range]))
        .unwrap_or_else(|| vec![ReadRange::default()]);

    if ranges.is_empty() {
        return BatchReadResult {
            file_path: request.file_path.clone(),
            ranges: vec![],
            error: None,
        };
    }

    if ranges.iter().all(|range| matches!(&range.mode, super::ReadMode::Slice)) {
        return match super::slice::read_ranges(&path, &ranges).await {
            Ok(slice_results) => {
                let super::slice::SliceReadRanges { results, error } = slice_results;
                let mut range_results = Vec::with_capacity(results.len());
                for (result, range) in results.into_iter().zip(ranges.iter()) {
                    match result {
                        Some(Ok(result)) => {
                            range_results.push(super::range_result_from_lines(
                                range.offset.max(1),
                                result.lines,
                                result.line_truncated,
                            ));
                        }
                        Some(Err(error)) => {
                            return BatchReadResult {
                                file_path: request.file_path.clone(),
                                ranges: range_results,
                                error: Some(error.to_string()),
                            };
                        }
                        None => {}
                    }
                }

                BatchReadResult {
                    file_path: request.file_path.clone(),
                    ranges: range_results,
                    error: error.map(|error| error.to_string()),
                }
            }
            Err(error) => BatchReadResult {
                file_path: request.file_path.clone(),
                ranges: vec![],
                error: Some(error.to_string()),
            },
        };
    }

    let mut range_results = Vec::with_capacity(ranges.len());
    for range in ranges {
        match handler.read_range(&path, &range).await {
            Ok(result) => range_results.push(result),
            Err(error) => {
                return BatchReadResult {
                    file_path: request.file_path.clone(),
                    ranges: range_results,
                    error: Some(error.to_string()),
                };
            }
        }
    }

    BatchReadResult {
        file_path: request.file_path.clone(),
        ranges: range_results,
        error: None,
    }
}

fn assemble_content(results: &[BatchReadResult]) -> String {
    // Reserve capacity for the joined content. Each result contributes at least
    // a header line plus its range content; estimate conservatively to avoid
    // repeated reallocations as the String grows.
    let estimated_bytes: usize = results
        .iter()
        .map(|result| {
            result.file_path.len() + result.ranges.iter().map(|r| r.content.len()).sum::<usize>() + 64 // headers, separators, line numbers
        })
        .sum();
    let mut content = String::with_capacity(estimated_bytes);
    let mut is_first = true;
    for result in results {
        if let Some(error) = &result.error {
            if !is_first {
                content.push_str("\n\n");
            }
            is_first = false;
            let _ = write!(content, "== {} (ERROR)\n{}", result.file_path, error);
            continue;
        }

        for range in &result.ranges {
            if !is_first {
                content.push_str("\n\n");
            }
            is_first = false;
            let end_line = range.offset.saturating_add(range.lines_read.saturating_sub(1));
            let _ = write!(content, "== {} (L{}..L{})\n{}", result.file_path, range.offset, end_line, range.content);
        }
    }
    content
}
