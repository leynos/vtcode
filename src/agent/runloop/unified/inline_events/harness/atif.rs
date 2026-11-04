use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use diapason_core::exec::events::atif::{AtifAgent, AtifTrajectoryBuilder};
use tokio::task::spawn_blocking;

/// Optional ATIF trajectory exporter.
pub(crate) struct AtifExporter {
    builder: AtifTrajectoryBuilder,
    output_path: PathBuf,
}

impl AtifExporter {
    pub(crate) fn new(model: &str, output_path: PathBuf) -> Self {
        let agent = AtifAgent::diapason().with_model(model);
        Self {
            builder: AtifTrajectoryBuilder::new(agent),
            output_path,
        }
    }

    pub(crate) fn process_event(&mut self, event: &diapason_core::exec::events::ThreadEvent) {
        self.builder.process_event(event);
    }

    /// Finalize, serialize, and write off the async executor.
    pub(crate) async fn finish(self) -> Result<(u64, u64, u64)> {
        let Self { builder, output_path } = self;
        let (json, metrics) = spawn_blocking(move || {
            let trajectory = builder.finish(None);
            let metrics = trajectory
                .final_metrics
                .as_ref()
                .map(|final_metrics| {
                    (
                        final_metrics.total_prompt_tokens.unwrap_or(0),
                        final_metrics.total_completion_tokens.unwrap_or(0),
                        final_metrics.total_cached_tokens.unwrap_or(0),
                    )
                })
                .unwrap_or((0, 0, 0));
            let json = serde_json::to_vec_pretty(&trajectory)?;
            Ok::<_, serde_json::Error>((json, metrics))
        })
        .await
        .context("ATIF serialization task failed")??;

        spawn_blocking(move || {
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(output_path, json)
        })
        .await
        .context("ATIF write task failed")??;
        Ok(metrics)
    }
}
