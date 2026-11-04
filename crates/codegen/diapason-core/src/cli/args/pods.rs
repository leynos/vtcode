use clap::Subcommand;

/// GPU pod management commands.
#[derive(Subcommand, Debug, Clone)]
pub enum PodsCommands {
    /// Start a model on the active pod.
    Start {
        /// Local model name used for lookup and storage.
        #[arg(long)]
        name: String,
        /// Hugging Face or provider model identifier to launch.
        #[arg(long)]
        model: String,
        /// Optional explicit pod name to store as the active pod.
        #[arg(long = "pod-name")]
        pod_name: Option<String>,
        /// SSH connection string used for the pod.
        #[arg(long)]
        ssh: Option<String>,
        /// GPU identifiers on the pod, repeated as `ID:NAME`.
        #[arg(long = "gpu", value_name = "ID:NAME", action = clap::ArgAction::Append)]
        gpus: Vec<String>,
        /// Optional remote models directory.
        #[arg(long = "models-path")]
        models_path: Option<String>,
        /// Optional exact profile name to use.
        #[arg(long)]
        profile: Option<String>,
        /// Optional requested GPU count.
        #[arg(long = "gpus")]
        gpus_count: Option<usize>,
        /// Optional override for `--gpu-memory-utilization` (percent).
        #[arg(long)]
        memory: Option<f32>,
        /// Optional override for `--max-model-len` (e.g. 4k, 32k, 131072).
        #[arg(long)]
        context: Option<String>,
    },

    /// Stop a running model on the active pod.
    Stop {
        /// Local model name to stop.
        #[arg(long)]
        name: String,
    },

    /// Stop every running model on the active pod.
    StopAll,

    /// List running models on the active pod.
    List,

    /// Stream logs for a running model on the active pod.
    Logs {
        /// Local model name whose logs should be streamed.
        #[arg(long)]
        name: String,
    },

    /// Show compatible and incompatible known models for the active pod.
    KnownModels,
}
