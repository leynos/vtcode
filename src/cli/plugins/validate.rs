use diapason_agent_plugins::{FileSystemPluginLoader, PluginLoader};

pub(super) async fn handle_validate(path: &std::path::Path) -> anyhow::Result<()> {
    let loader = FileSystemPluginLoader::new();
    let loaded = loader.load(path)?;
    println!("Valid plugin: {}", loaded.manifest.name);
    println!("Skills: {}", loaded.skills.len());
    if let Some(mcp) = &loaded.mcp {
        println!("MCP servers: {}", mcp.servers.len());
    }
    Ok(())
}
