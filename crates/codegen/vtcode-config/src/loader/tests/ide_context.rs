//! Tests for IDE context configuration round trips.

use super::*;

#[test]
fn ide_context_fields_round_trip_through_toml() {
    let mut config = VTCodeConfig::default();
    config.ide_context.enabled = false;
    config.ide_context.inject_into_prompt = false;
    config.ide_context.show_in_tui = false;
    config.ide_context.include_selection_text = false;
    config.ide_context.provider_mode = IdeContextProviderMode::Zed;
    config.ide_context.providers = IdeContextProvidersConfig {
        vscode_compatible: IdeContextProviderConfig { enabled: false },
        zed: IdeContextProviderConfig { enabled: true },
        generic: IdeContextProviderConfig { enabled: false },
    };

    let serialized = toml::to_string(&config).expect("serialize config");
    let parsed: VTCodeConfig = toml::from_str(&serialized).expect("parse config");

    assert!(!parsed.ide_context.enabled);
    assert!(!parsed.ide_context.inject_into_prompt);
    assert!(!parsed.ide_context.show_in_tui);
    assert!(!parsed.ide_context.include_selection_text);
    assert_eq!(parsed.ide_context.provider_mode, IdeContextProviderMode::Zed);
    assert!(!parsed.ide_context.providers.vscode_compatible.enabled);
    assert!(parsed.ide_context.providers.zed.enabled);
    assert!(!parsed.ide_context.providers.generic.enabled);
}
