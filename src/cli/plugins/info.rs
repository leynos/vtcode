use diapason_agent_plugins::{FileSystemPluginLoader, PluginLoader};

pub(super) async fn handle_info(name: &str) -> anyhow::Result<()> {
    let roots = super::plugin_roots();
    let loader = FileSystemPluginLoader::new();
    for root in roots {
        let plugin_dir = root.join(name);
        if !plugin_dir.is_dir() {
            continue;
        }
        let plugin_json = plugin_dir.join("plugin.json");
        if !plugin_json.is_file() {
            continue;
        }
        let loaded = loader.load(&plugin_dir)?;
        println!("Name: {}", loaded.manifest.name);
        println!("Version: {}", loaded.manifest.version.as_deref().unwrap_or("unknown"));
        println!("Description: {}", loaded.manifest.description.as_deref().unwrap_or("(none)"));
        println!("Root: {}", loaded.root.display());
        println!("Skills:");
        for skill in &loaded.skills {
            println!("  - {} ({})", skill.name, skill.skill_md_path.display());
        }
        if let Some(mcp) = &loaded.mcp {
            println!("MCP servers:");
            for (name, server) in &mcp.servers {
                match server {
                    diapason_agent_plugins::ServerConfig::Stdio(s) => {
                        println!("  - {} (stdio): {} {:?}", name, s.command, s.args);
                    }
                    diapason_agent_plugins::ServerConfig::StreamableHttp(h) => {
                        println!("  - {} (streamable-http): {}", name, h.url);
                    }
                    diapason_agent_plugins::ServerConfig::Sse(s) => {
                        println!("  - {} (sse): {} (unsupported)", name, s.url);
                    }
                }
            }
        }
        return Ok(());
    }
    anyhow::bail!("plugin not found: {}", name);
}
