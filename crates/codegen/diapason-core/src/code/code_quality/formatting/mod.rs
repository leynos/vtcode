use crate::code::code_quality::config::FormatConfig;
use std::path::Path;
use std::process::Command;

/// Result of formatting operation
#[derive(Debug, Clone)]
pub struct FormatResult {
    pub success: bool,
    pub formatted_content: Option<String>,
    pub error_message: Option<String>,
    pub tool_used: String,
}

/// Formatting orchestrator that manages multiple formatters
pub struct FormattingOrchestrator {
    configs: Vec<FormatConfig>,
}

impl Default for FormattingOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl FormattingOrchestrator {
    pub fn new() -> Self {
        let mut orchestrator = Self { configs: Vec::new() };

        // Register default formatters
        orchestrator.register(FormatConfig::rustfmt());
        orchestrator.register(FormatConfig::prettier());
        orchestrator.register(FormatConfig::black());

        orchestrator
    }

    /// Register a formatting configuration
    pub fn register(&mut self, config: FormatConfig) {
        self.configs.push(config);
    }

    /// Format a file based on its extension
    pub async fn format_file(&self, file_path: &Path) -> Option<FormatResult> {
        let extension = file_path.extension()?.to_str()?;
        let extension_with_dot = format!(".{extension}");

        for config in &self.configs {
            if config.enabled && config.file_extensions.contains(&extension_with_dot) {
                return Some(self.run_formatter(config, file_path).await);
            }
        }

        None
    }

    async fn run_formatter(&self, config: &FormatConfig, file_path: &Path) -> FormatResult {
        // Execute the formatting tool off the async executor —
        // `Command::output()` blocks until the subprocess exits. See the
        // `# Blocking` docs in `src/agent/runloop/git.rs` for the pattern.
        let config = config.clone();
        let file_path = file_path.to_path_buf();
        match tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(&config.command[0]);
            for arg in &config.args {
                cmd.arg(arg);
            }
            cmd.arg(&file_path);
            cmd.output()
        })
        .await
        {
            Ok(Ok(output)) => {
                if output.status.success() {
                    FormatResult {
                        success: true,
                        formatted_content: None,
                        error_message: None,
                        tool_used: config.tool_name.clone(),
                    }
                } else {
                    let error_msg = String::from_utf8_lossy(&output.stderr).into_owned();
                    FormatResult {
                        success: false,
                        formatted_content: None,
                        error_message: Some(error_msg),
                        tool_used: config.tool_name.clone(),
                    }
                }
            }
            Ok(Err(e)) => FormatResult {
                success: false,
                formatted_content: None,
                error_message: Some(format!("Failed to execute {}: {}", config.tool_name, e)),
                tool_used: config.tool_name.clone(),
            },
            Err(e) => FormatResult {
                success: false,
                formatted_content: None,
                error_message: Some(format!("Formatter task failed: {e}")),
                tool_used: config.tool_name.clone(),
            },
        }
    }
}
