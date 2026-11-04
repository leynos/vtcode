use std::path::{Path, PathBuf};

use anyhow::Result;
use diapason_config::OpenResponsesConfig;
use diapason_core::exec::events::ThreadEvent;
use diapason_core::open_responses::{OpenResponsesIntegration, SequencedEvent};

use super::legacy::LegacyWriter;

/// Optional Open Responses exporter state.
pub(crate) struct OpenResponsesExporter {
    integration: OpenResponsesIntegration,
    writer: Option<LegacyWriter>,
    output_path: Option<PathBuf>,
    sequence_counter: u64,
}

impl OpenResponsesExporter {
    #[cfg(test)]
    pub(crate) fn new_sync(
        config: OpenResponsesConfig,
        model: &str,
        output_path: Option<PathBuf>,
    ) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let writer = output_path.as_ref().map(|path| LegacyWriter::new_sync(path)).transpose()?;
        let mut integration = OpenResponsesIntegration::new(config);
        integration.start_response(model);
        Ok(Some(Self {
            integration,
            writer,
            output_path,
            sequence_counter: 0,
        }))
    }

    pub(crate) async fn new_async(
        config: OpenResponsesConfig,
        model: &str,
        output_path: Option<PathBuf>,
    ) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let writer = match output_path.clone() {
            Some(path) => Some(LegacyWriter::new_async(path).await?),
            None => None,
        };
        let mut integration = OpenResponsesIntegration::new(config);
        integration.start_response(model);
        Ok(Some(Self {
            integration,
            writer,
            output_path,
            sequence_counter: 0,
        }))
    }

    pub(crate) fn process_event(&mut self, event: &ThreadEvent, fallback_path: &Path) {
        self.integration.process_event(event);
        self.write_events(fallback_path);
    }

    fn write_events(&mut self, fallback_path: &Path) {
        let output_path = self
            .output_path
            .as_ref()
            .map_or_else(|| fallback_path.display().to_string(), |path| path.display().to_string());
        let Some(writer) = self.writer.as_ref() else {
            let _ = self.integration.take_events();
            return;
        };

        for stream_event in self.integration.take_events() {
            let sequence_number = self.sequence_counter;
            self.sequence_counter = self.sequence_counter.saturating_add(1);
            let sequenced = SequencedEvent::new(sequence_number, &stream_event);
            let Ok(json) = serde_json::to_string(&sequenced) else {
                tracing::warn!(target: "diapason.harness", phase = "open_responses_serialize", path = %output_path, "Open Responses event serialization failed");
                continue;
            };
            // `LegacyWriter::write_line` appends one newline. Keep a second
            // newline in the payload so each SSE event is separated by the
            // required blank line.
            let line = format!("event: {}\ndata: {json}\n", stream_event.event_type());
            if let Err(error) = writer.write_line(line) {
                tracing::warn!(target: "diapason.harness", phase = "open_responses_write", path = %output_path, error = %error, "Open Responses event write failed");
            }
        }
    }

    pub(crate) async fn finish_async(mut self, fallback_path: &Path) {
        let output_path = self
            .output_path
            .as_ref()
            .map_or_else(|| fallback_path.display().to_string(), |path| path.display().to_string());
        if self.integration.finish_response().is_none() {
            tracing::warn!(target: "diapason.harness", phase = "open_responses_finish", path = %output_path, "Open Responses finish produced no terminal response");
        }
        self.write_events(fallback_path);
        if let Some(writer) = self.writer.take() {
            if let Err(error) = writer.write_line("data: [DONE]".to_string()) {
                tracing::warn!(target: "diapason.harness", phase = "open_responses_finish", path = %output_path, error = %error, "Open Responses terminal marker write failed");
            }
            if let Err(error) = writer.flush().await {
                tracing::warn!(target: "diapason.harness", phase = "open_responses_flush", path = %output_path, error = %error, "Open Responses terminal marker flush failed");
            }
            log_diagnostics(&writer, &output_path);
        }
    }

    #[cfg(test)]
    pub(crate) fn finish_sync(mut self, fallback_path: &Path) {
        let output_path = self
            .output_path
            .as_ref()
            .map_or_else(|| fallback_path.display().to_string(), |path| path.display().to_string());
        if self.integration.finish_response().is_none() {
            tracing::warn!(target: "diapason.harness", phase = "open_responses_finish", path = %output_path, "Open Responses finish produced no terminal response");
        }
        self.write_events(fallback_path);
        if let Some(writer) = self.writer.take() {
            if let Err(error) = writer.write_line("data: [DONE]".to_string()) {
                tracing::warn!(target: "diapason.harness", phase = "open_responses_finish", path = %output_path, error = %error, "Open Responses terminal marker write failed");
            }
            if let Err(error) = writer.flush_sync() {
                tracing::warn!(target: "diapason.harness", phase = "open_responses_flush", path = %output_path, error = %error, "Open Responses terminal marker flush failed");
            }
        }
    }
}

fn log_diagnostics(writer: &LegacyWriter, output_path: &str) {
    let diagnostics = writer.diagnostics();
    if diagnostics.dropped_lines > 0 || diagnostics.write_failures > 0 {
        tracing::warn!(
            target: "diapason.harness",
            phase = "open_responses_drops",
            path = %output_path,
            dropped_lines = diagnostics.dropped_lines,
            dropped_bytes = diagnostics.dropped_bytes,
            write_failures = diagnostics.write_failures,
            "optional Open Responses export dropped data"
        );
    }
}
