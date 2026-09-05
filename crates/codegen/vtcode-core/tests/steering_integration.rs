//! Offline integration coverage for cancellation at the agent-runner boundary.
#[path = "support/config_defaults.rs"]
mod config_defaults;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use vtcode_core::config::{ModelId, VTCodeConfig};
use vtcode_core::core::agent::runner::{AgentRunner, RunnerSettings};
use vtcode_core::core::agent::steering::SteeringMessage;
use vtcode_core::core::agent::task::{Task, TaskOutcome};
use vtcode_core::core::agent::types::AgentType;
use vtcode_core::core::threads::ThreadBootstrap;

#[test]
fn prequeued_stop_cancels_before_the_runner_starts_a_model_turn() -> Result<()> {
    let workspace = tempfile::tempdir().context("create isolated steering workspace")?;
    let codex_home = workspace.path().join("codex-home");
    std::fs::create_dir(&codex_home).context("create isolated CODEX_HOME")?;

    temp_env::with_vars([("CODEX_HOME", Some(codex_home.as_os_str()))], || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build steering test runtime")?
            .block_on(async {
                let _config_defaults = config_defaults::IsolatedConfigDefaultsGuard::install(workspace.path()).await;
                let (steering_tx, steering_rx) = mpsc::unbounded_channel();
                let model = ModelId::Gemini37Flash;
                let mut config = VTCodeConfig::default();
                config.agent.provider = "gemini".to_owned();
                config.agent.default_model = model.to_string();
                config.skills.bundled.enabled = false;

                steering_tx
                    .send(SteeringMessage::SteerStop)
                    .context("queue stop before runner execution")?;

                let mut runner = AgentRunner::new_with_bootstrap(
                    AgentType::Single,
                    model,
                    "test-key".to_owned(),
                    workspace.path().to_path_buf(),
                    "steering-stop".to_owned(),
                    RunnerSettings::default(),
                    Some(steering_rx),
                    ThreadBootstrap::new(None),
                    Some(config),
                    None,
                )
                .await
                .context("create isolated runner")?;

                let task = Task {
                    id: "steering-stop".into(),
                    title: "stop before model turn".into(),
                    description: "The prequeued stop must prevent model execution.".into(),
                    instructions: None,
                };
                let result = runner.execute_task_with_retry(&task, &[], 1).await?;

                anyhow::ensure!(
                    result.outcome == TaskOutcome::Cancelled,
                    "prequeued stop should cancel before a provider request; outcome: {:?}",
                    result.outcome
                );
                anyhow::ensure!(
                    result.turns_executed == 0,
                    "prequeued stop should prevent model turns; observed {} turn(s)",
                    result.turns_executed
                );
                anyhow::ensure!(
                    result.created_contexts.is_empty(),
                    "prequeued stop should create no contexts: {:?}",
                    result.created_contexts
                );
                anyhow::ensure!(
                    result.modified_files.is_empty(),
                    "prequeued stop should modify no files: {:?}",
                    result.modified_files
                );
                anyhow::ensure!(
                    result.executed_commands.is_empty(),
                    "prequeued stop should execute no commands: {:?}",
                    result.executed_commands
                );

                Ok(())
            })
    })
}

// Pause/resume state-machine behaviour is asserted in
// `core::agent::runtime::tests`; this integration fixture only covers the
// prequeued-stop boundary without calling a provider.
