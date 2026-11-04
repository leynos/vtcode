use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use diapason_core::cli::args::Cli;
use diapason_core::config::loader::{ConfigBuilder, DiapasonConfig, set_explicit_config_path};
use diapason_core::utils::validation::validate_path_exists;

use super::first_run::maybe_run_first_run_setup;
use super::validation::{parse_cli_config_entries, resolve_config_path, resolve_workspace_path};

pub(super) struct LoadedStartupConfig {
    pub(super) workspace: PathBuf,
    pub(super) config: DiapasonConfig,
    pub(super) first_run_occurred: bool,
    pub(super) full_auto_requested: bool,
    pub(super) automation_prompt: Option<String>,
    pub(super) primary_agent_explicitly_configured: bool,
}

pub(super) async fn load_startup_config(args: &Cli) -> Result<LoadedStartupConfig> {
    let workspace_override = args.workspace_path.clone().or_else(|| args.workspace.clone());

    let workspace = resolve_workspace_path(workspace_override).context("Failed to resolve workspace directory")?;
    if args.workspace_path.is_some() {
        validate_path_exists(&workspace, "Workspace")?;
    }

    let (cli_config_path_override, inline_config_overrides) = parse_cli_config_entries(&args.config);
    let env_config_path_override = std::env::var("DIAPASON_CONFIG_PATH").ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    });
    let config_path_override = cli_config_path_override.or(env_config_path_override);

    let mut builder = ConfigBuilder::new().workspace(workspace.clone());
    let resolved_explicit_path = match config_path_override {
        Some(path_override) => {
            let resolved_path = resolve_config_path(&workspace, &path_override);
            builder = builder.config_file(resolved_path.clone());
            Some(resolved_path)
        }
        None => None,
    };
    // Unconditionally synchronize the session override so a previous
    // `load_startup_config` in the same process (e.g. a test or a reload)
    // cannot leak a stale explicit path into this load.
    set_explicit_config_path(resolved_explicit_path.clone());

    if !inline_config_overrides.is_empty() {
        builder = builder.cli_overrides(&inline_config_overrides);
    }

    if let Some(ref model) = args.model {
        builder = builder.cli_override("agent.default_model".to_owned(), toml::Value::String(model.clone()));
    }
    if let Some(ref provider) = args.provider {
        builder = builder.cli_override("agent.provider".to_owned(), toml::Value::String(provider.clone()));
    }

    let config_phase = std::time::Instant::now();
    let manager = match builder.build() {
        Ok(manager) => manager,
        Err(error) if super::is_config_reset_command(args) => {
            // The reset service can repair the selected malformed layer, so
            // do not make a broken effective stack prevent `config reset`
            // from reaching that service. Reset does not need provider auth,
            // model validation, or any agent runtime initialization.
            tracing::warn!("Configuration could not be loaded before reset: {error:#}");
            return Ok(LoadedStartupConfig {
                workspace,
                config: DiapasonConfig::default(),
                first_run_occurred: false,
                full_auto_requested: false,
                automation_prompt: None,
                primary_agent_explicitly_configured: false,
            });
        }
        Err(error) => return Err(error).context("Failed to load configuration"),
    };
    let config_duration = config_phase.elapsed();
    if let Some(timing) = manager.phase_timing() {
        tracing::debug!(target = "diapason.startup", ?timing, "configuration phases recorded");
        diapason_commons::startup_trace::record_duration(
            "config_path_resolution",
            std::time::Duration::from_micros(timing.path_resolution_us),
        );
        diapason_commons::startup_trace::record_duration(
            "config_layer_loading",
            std::time::Duration::from_micros(timing.layer_loading_us),
        );
        diapason_commons::startup_trace::record_duration(
            "config_merge_and_parse",
            std::time::Duration::from_micros(timing.merge_and_parse_us),
        );
        diapason_commons::startup_trace::record_duration(
            "config_validation",
            std::time::Duration::from_micros(timing.validation_us),
        );
    }
    diapason_commons::startup_trace::record_duration("config_loading", config_duration);
    let primary_agent_explicitly_configured = has_explicit_default_primary_agent(&manager.effective_config());
    let mut config = manager.config().clone();

    let (full_auto_requested, automation_prompt) = match args.full_auto.clone() {
        Some(value) if value.trim().is_empty() => (true, None),
        Some(value) => (true, Some(value)),
        None => (false, None),
    };

    let first_run_occurred = maybe_run_first_run_setup(args, &workspace, &mut config).await?;

    if automation_prompt.is_some() && args.command.is_some() {
        bail!("--auto/--full-auto with a prompt cannot be combined with other commands. Provide only the prompt.");
    }

    Ok(LoadedStartupConfig {
        workspace,
        config,
        first_run_occurred,
        full_auto_requested,
        automation_prompt,
        primary_agent_explicitly_configured,
    })
}

pub(crate) fn has_explicit_default_primary_agent(config: &toml::Value) -> bool {
    has_top_level_config_key(config, "default_primary_agent")
}

fn has_top_level_config_key(config: &toml::Value, key: &str) -> bool {
    config.as_table().is_some_and(|table| table.contains_key(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use diapason_config::loader::explicit_config_path;
    use diapason_core::cli::args::Cli;
    use serial_test::serial;
    use tempfile::TempDir;

    /// RAII guard for the process-global session override so a panicking test
    /// cannot leak a stale explicit path into sibling tests.
    struct SessionOverrideGuard;

    impl SessionOverrideGuard {
        fn reset() -> Self {
            set_explicit_config_path(None);
            Self
        }
    }

    impl Drop for SessionOverrideGuard {
        fn drop(&mut self) {
            set_explicit_config_path(None);
        }
    }

    #[tokio::test]
    #[serial]
    async fn cli_config_path_override_loads_requested_file() {
        let _guard = SessionOverrideGuard::reset();
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace = temp_dir.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace dir");
        std::fs::create_dir(workspace.join(diapason_commons::workspace_dir_name())).expect("workspace dot dir");

        let config_path = temp_dir.path().join("custom-config.toml");
        std::fs::write(
            &config_path,
            r#"
[debug]
enable_tracing = true
"#,
        )
        .expect("custom config");

        let args = Cli::parse_from([
            "diapason",
            "--workspace",
            workspace.to_str().expect("workspace path"),
            "--config",
            config_path.to_str().expect("config path"),
        ]);

        let loaded = load_startup_config(&args).await.expect("startup config should load");

        assert!(loaded.config.debug.enable_tracing);
    }

    #[tokio::test]
    #[serial]
    async fn env_config_path_override_loads_requested_file_and_captures_session_override() {
        use diapason_commons::env_lock;

        let _guard = SessionOverrideGuard::reset();
        let env_guard = env_lock::lock();
        let previous = std::env::var_os("DIAPASON_CONFIG_PATH");

        let temp_dir = TempDir::new().expect("temp dir");
        let workspace = temp_dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::create_dir_all(workspace.join(diapason_commons::workspace_dir_name())).expect("workspace dot dir");

        let config_path = temp_dir.path().join("custom-env-config.toml");
        std::fs::write(&config_path, "[debug]\nenable_tracing = true\n").expect("custom env config");

        env_guard.set_var("DIAPASON_CONFIG_PATH", config_path.to_str().expect("config path"));

        let args = Cli::parse_from(["diapason", "--workspace", workspace.to_str().expect("workspace path")]);

        let loaded = load_startup_config(&args).await;
        let session_override = explicit_config_path();

        env_guard.restore_var("DIAPASON_CONFIG_PATH", previous);

        let loaded = loaded.expect("startup config should load from DIAPASON_CONFIG_PATH");
        assert!(loaded.config.debug.enable_tracing);
        assert_eq!(
            session_override.and_then(|path| diapason_commons::canonicalize(path).ok()),
            diapason_commons::canonicalize(&config_path).ok(),
            "startup must capture the resolved env path as the session override"
        );
    }

    #[test]
    fn detects_explicit_default_primary_agent_key() {
        let with_key = toml::Value::Table(r#"default_primary_agent = "duck""#.parse().expect("toml"));
        let without_key = toml::Value::Table(
            r#"[agent]
provider = "openai""#
                .parse()
                .expect("toml"),
        );

        assert!(has_explicit_default_primary_agent(&with_key));
        assert!(!has_explicit_default_primary_agent(&without_key));
    }
}
