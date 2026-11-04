/// Agent Client Protocol specific environment keys
pub mod acp {
    #[derive(Debug, Clone, Copy)]
    pub enum AgentClientProtocolEnvKey {
        Enabled,
        ZedEnabled,
        ZedToolsReadFileEnabled,
        ZedToolsListFilesEnabled,
        ZedWorkspaceTrust,
    }

    impl AgentClientProtocolEnvKey {
        pub(crate) fn as_str(self) -> &'static str {
            match self {
                Self::Enabled => "DIAPASON_ACP_ENABLED",
                Self::ZedEnabled => "DIAPASON_ACP_ZED_ENABLED",
                Self::ZedToolsReadFileEnabled => "DIAPASON_ACP_ZED_TOOLS_READ_FILE_ENABLED",
                Self::ZedToolsListFilesEnabled => "DIAPASON_ACP_ZED_TOOLS_LIST_FILES_ENABLED",
                Self::ZedWorkspaceTrust => "DIAPASON_ACP_ZED_WORKSPACE_TRUST",
            }
        }
    }
}
