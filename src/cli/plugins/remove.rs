use anyhow::Context;
use diapason_agent_plugins::{FileSystemPluginInstaller, PluginInstaller};

pub(super) async fn handle_remove(name: &str) -> anyhow::Result<()> {
    let name = name.to_string();
    // `remove` does a blocking recursive `remove_dir_all`; run it off the
    // async executor. See `# Blocking` docs in `src/agent/runloop/git.rs`.
    tokio::task::spawn_blocking({
        let name = name.clone();
        move || {
            let installer = FileSystemPluginInstaller::new();
            installer.remove(&name)
        }
    })
    .await
    .context("plugin remove task panicked")?
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Removed plugin {}", name);
    Ok(())
}
