use std::path::PathBuf;

use diapason_core::cli::args::PluginsSubcommand;

use crate::startup::StartupContext;

use diapason_agent_plugins::plugin_roots_for;

pub(super) async fn dispatch_plugins_command(_startup: &StartupContext, cmd: PluginsSubcommand) -> anyhow::Result<()> {
    match cmd {
        PluginsSubcommand::List => list::handle_list().await,
        PluginsSubcommand::Info { name } => info::handle_info(&name).await,
        PluginsSubcommand::Validate { path } => validate::handle_validate(&path).await,
        PluginsSubcommand::Add { source, name } => add::handle_add(source, name).await,
        PluginsSubcommand::Remove { name } => remove::handle_remove(&name).await,
    }
}

fn plugin_roots() -> Vec<PathBuf> {
    plugin_roots_for(&std::env::current_dir().unwrap_or_default())
}

mod add;
mod info;
mod list;
mod remove;
mod validate;
