use clap::ValueEnum;

/// Supported Agent Client Protocol clients
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AgentClientProtocolTarget {
    /// Agent Client Protocol client (legacy Zed identifier)
    Zed,
    /// Standard Agent Client Protocol client
    Standard,
}
