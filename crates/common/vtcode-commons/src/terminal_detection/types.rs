//! Terminal types, capabilities, setup requirements, and display names.

/// A terminal emulator recognized by `VTCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalType {
    Ghostty,
    Kitty,
    Alacritty,
    WezTerm,
    TerminalApp,
    Xterm,
    Zed,
    Warp,
    ITerm2,
    VSCode,
    WindowsTerminal,
    Hyper,
    Tabby,
    Unknown,
}

/// Terminal features that can be configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFeature {
    Multiline,
    CopyPaste,
    ShellIntegration,
    ThemeSync,
    Notifications,
}

/// How VT Code should present `/terminal-setup` for a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSetupAvailability {
    NativeSupport,
    Offered,
    GuidanceOnly,
}

impl TerminalType {
    /// Returns whether this terminal supports `feature`.
    pub fn supports_feature(&self, feature: TerminalFeature) -> bool {
        match (self, feature) {
            (TerminalType::Ghostty, _) => true,
            (TerminalType::Kitty, _) => true,
            (TerminalType::Alacritty, _) => true,
            (TerminalType::WezTerm, _) => true,
            (TerminalType::TerminalApp, TerminalFeature::Multiline) => true,
            (TerminalType::TerminalApp, TerminalFeature::ShellIntegration) => true,
            (TerminalType::TerminalApp, TerminalFeature::Notifications) => true,
            (TerminalType::TerminalApp, _) => false,
            (TerminalType::Xterm, TerminalFeature::Multiline) => true,
            (TerminalType::Xterm, TerminalFeature::Notifications) => true,
            (TerminalType::Xterm, _) => false,
            (TerminalType::Zed, TerminalFeature::Multiline) => true,
            (TerminalType::Zed, TerminalFeature::ThemeSync) => true,
            (TerminalType::Zed, TerminalFeature::Notifications) => true,
            (TerminalType::Zed, _) => false,
            (TerminalType::Warp, TerminalFeature::Multiline) => true,
            (TerminalType::Warp, TerminalFeature::Notifications) => true,
            (TerminalType::Warp, _) => false,
            (TerminalType::ITerm2, _) => true,
            (TerminalType::VSCode, TerminalFeature::Multiline) => true,
            (TerminalType::VSCode, TerminalFeature::Notifications) => true,
            (TerminalType::VSCode, _) => false,
            (TerminalType::WindowsTerminal, _) => true,
            (TerminalType::Hyper, _) => true,
            (TerminalType::Tabby, _) => true,
            (TerminalType::Unknown, _) => false,
        }
    }

    /// Returns whether multiline input works without `VTCode` configuration.
    pub fn has_native_multiline_support(&self) -> bool {
        matches!(
            self,
            TerminalType::Ghostty
                | TerminalType::Kitty
                | TerminalType::WezTerm
                | TerminalType::ITerm2
                | TerminalType::Warp
        )
    }

    /// Returns how `VTCode` should present `/terminal-setup` for this terminal.
    pub fn terminal_setup_availability(&self) -> TerminalSetupAvailability {
        match self {
            TerminalType::Ghostty
            | TerminalType::Kitty
            | TerminalType::WezTerm
            | TerminalType::ITerm2
            | TerminalType::Warp => TerminalSetupAvailability::NativeSupport,
            TerminalType::Alacritty | TerminalType::Zed | TerminalType::VSCode => TerminalSetupAvailability::Offered,
            TerminalType::TerminalApp
            | TerminalType::Xterm
            | TerminalType::WindowsTerminal
            | TerminalType::Hyper
            | TerminalType::Tabby
            | TerminalType::Unknown => TerminalSetupAvailability::GuidanceOnly,
        }
    }

    /// Returns whether `/terminal-setup` should appear in slash discovery surfaces.
    pub fn should_offer_terminal_setup(&self) -> bool {
        matches!(self.terminal_setup_availability(), TerminalSetupAvailability::Offered)
    }

    /// Returns the human-readable terminal name.
    pub fn name(&self) -> &'static str {
        match self {
            TerminalType::Ghostty => "Ghostty",
            TerminalType::Kitty => "Kitty",
            TerminalType::Alacritty => "Alacritty",
            TerminalType::WezTerm => "WezTerm",
            TerminalType::TerminalApp => "Terminal.app",
            TerminalType::Xterm => "xterm",
            TerminalType::Zed => "Zed",
            TerminalType::Warp => "Warp",
            TerminalType::ITerm2 => "iTerm2",
            TerminalType::VSCode => "VS Code",
            TerminalType::WindowsTerminal => "Windows Terminal",
            TerminalType::Hyper => "Hyper",
            TerminalType::Tabby => "Tabby",
            TerminalType::Unknown => "Unknown",
        }
    }
    /// Check if terminal requires manual setup (vs automatic config).
    pub(super) fn requires_manual_setup(&self) -> bool {
        self.should_offer_terminal_setup()
    }
}

impl TerminalFeature {
    /// Returns the human-readable feature name.
    pub fn name(&self) -> &'static str {
        match self {
            TerminalFeature::Multiline => "Shift+Enter Multiline Input",
            TerminalFeature::CopyPaste => "Enhanced Copy/Paste",
            TerminalFeature::ShellIntegration => "Shell Integration",
            TerminalFeature::ThemeSync => "Theme Synchronization",
            TerminalFeature::Notifications => "System Notifications",
        }
    }
}
