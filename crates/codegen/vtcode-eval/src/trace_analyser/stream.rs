use std::io::{BufRead, Read};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{HarnessTraceSummary, LatencyAccumulator, LifecycleTiming, UsageAccounting, record_value};

#[derive(Default)]
struct AnalyserState {
    summary: HarnessTraceSummary,
    latencies: LatencyAccumulator,
    timing: LifecycleTiming,
    usage: UsageAccounting,
}

impl AnalyserState {
    fn record_bytes(&mut self, line: &[u8]) {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            return;
        }

        let value = match serde_json::from_slice::<Value>(line) {
            Ok(value) => value,
            Err(_) => {
                self.summary.malformed_lines = self.summary.malformed_lines.saturating_add(1);
                return;
            }
        };

        if !record_value(&value, &mut self.summary, &mut self.latencies, &mut self.timing, &mut self.usage) {
            self.summary.unrecognized_lines = self.summary.unrecognized_lines.saturating_add(1);
        }
    }

    fn record_oversized(&mut self) {
        self.summary.malformed_lines = self.summary.malformed_lines.saturating_add(1);
    }

    fn finish(self) -> HarnessTraceSummary {
        HarnessTraceSummary {
            latency: self.latencies.finish(),
            token_usage: self.usage.finish(),
            ..self.summary
        }
    }
}

pub(super) fn analyse_jsonl_reader<R: BufRead>(mut reader: R) -> Result<HarnessTraceSummary> {
    let mut state = AnalyserState::default();
    let mut line = Vec::with_capacity(8 * 1024);
    loop {
        line.clear();
        let bytes_read = {
            let mut limited_reader = reader.by_ref().take((super::MAX_TRACE_LINE_BYTES + 1) as u64);
            limited_reader.read_until(b'\n', &mut line).context("read JSONL trace line")?
        };
        if bytes_read == 0 {
            break;
        }
        if line.len() > super::MAX_TRACE_LINE_BYTES {
            state.record_oversized();
            if !line.ends_with(b"\n") {
                discard_oversized_record(&mut reader)?;
            }
            continue;
        }
        state.record_bytes(&line);
    }
    Ok(state.finish())
}

fn discard_oversized_record<R: BufRead>(reader: &mut R) -> Result<()> {
    loop {
        let buffered = reader.fill_buf().context("discard oversized JSONL trace line")?;
        if buffered.is_empty() {
            return Ok(());
        }
        if let Some(newline) = buffered.iter().position(|byte| *byte == b'\n') {
            reader.consume(newline + 1);
            return Ok(());
        }
        let buffered_len = buffered.len();
        reader.consume(buffered_len);
    }
}
