//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn terminal_feature_support_matches_expectations() {
    assert!(TerminalType::Ghostty.supports_feature(TerminalFeature::Multiline));
    assert!(TerminalType::Ghostty.supports_feature(TerminalFeature::CopyPaste));
    assert!(TerminalType::Ghostty.supports_feature(TerminalFeature::ShellIntegration));
    assert!(TerminalType::Ghostty.supports_feature(TerminalFeature::ThemeSync));
    assert!(TerminalType::Ghostty.supports_feature(TerminalFeature::Notifications));

    assert!(TerminalType::VSCode.supports_feature(TerminalFeature::Multiline));
    assert!(TerminalType::VSCode.supports_feature(TerminalFeature::Notifications));
    assert!(!TerminalType::VSCode.supports_feature(TerminalFeature::CopyPaste));

    assert!(TerminalType::Zed.supports_feature(TerminalFeature::Multiline));
    assert!(TerminalType::Zed.supports_feature(TerminalFeature::ThemeSync));
    assert!(TerminalType::Zed.supports_feature(TerminalFeature::Notifications));

    assert!(TerminalType::Warp.supports_feature(TerminalFeature::Notifications));

    assert!(!TerminalType::Unknown.supports_feature(TerminalFeature::Multiline));
    assert!(!TerminalType::Unknown.supports_feature(TerminalFeature::Notifications));
}

#[test]
fn terminal_names_match_current_labels() {
    assert_eq!(TerminalType::Kitty.name(), "Kitty");
    assert_eq!(TerminalType::Alacritty.name(), "Alacritty");
    assert_eq!(TerminalType::VSCode.name(), "VS Code");
}

#[test]
fn manual_setup_detection_matches_offer_state() {
    assert!(TerminalType::VSCode.requires_manual_setup());
    assert!(!TerminalType::ITerm2.requires_manual_setup());
    assert!(!TerminalType::Kitty.requires_manual_setup());
}

#[test]
fn native_multiline_terminals_are_not_offered_setup() {
    assert!(TerminalType::WezTerm.has_native_multiline_support());
    assert!(!TerminalType::WezTerm.should_offer_terminal_setup());
    assert!(TerminalType::ITerm2.has_native_multiline_support());
    assert!(!TerminalType::ITerm2.should_offer_terminal_setup());
    assert!(TerminalType::Warp.has_native_multiline_support());
    assert!(!TerminalType::Warp.should_offer_terminal_setup());
}

#[test]
fn supported_setup_terminals_are_offered_setup() {
    assert!(TerminalType::VSCode.should_offer_terminal_setup());
    assert!(TerminalType::Alacritty.should_offer_terminal_setup());
    assert!(TerminalType::Zed.should_offer_terminal_setup());
    assert!(!TerminalType::WindowsTerminal.should_offer_terminal_setup());
    assert!(!TerminalType::Hyper.should_offer_terminal_setup());
    assert!(!TerminalType::Tabby.should_offer_terminal_setup());
}

#[test]
fn ghostty_helper_matches_term_program_or_term() {
    assert!(is_ghostty_terminal(Some("Ghostty"), None));
    assert!(is_ghostty_terminal(None, Some("xterm-ghostty")));
    assert!(!is_ghostty_terminal(Some("WezTerm"), Some("xterm-256color")));
}
