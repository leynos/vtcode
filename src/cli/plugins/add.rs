use anyhow::Context;
use diapason_agent_plugins::{FileSystemPluginInstaller, PluginInstaller};

pub(super) async fn handle_add(source: String, name: Option<String>) -> anyhow::Result<()> {
    // `install` runs `git clone` (a blocking subprocess) and recursive
    // filesystem copies; run it off the async executor. See `# Blocking`
    // docs in `src/agent/runloop/git.rs`.
    let installed = tokio::task::spawn_blocking(move || {
        let installer = FileSystemPluginInstaller::new();
        installer.install(&source, name)
    })
    .await
    .context("plugin install task panicked")?
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Installed plugin {} to {}", installed.name, installed.path.display());
    println!(
        "Validated: {} skill(s), {} MCP server(s)",
        installed.loaded.skills.len(),
        installed.loaded.mcp.as_ref().map(|m| m.servers.len()).unwrap_or(0)
    );
    Ok(())
}
