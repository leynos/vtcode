//! Verifies the externally reachable `BehaviourConfig` serde and TOML contract.

use vtcode_ui::tui::core_tui::session::config::BehaviourConfig;

#[test]
fn behaviour_config_supports_external_serde_round_trip() {
    let default_serialized = toml::to_string(&BehaviourConfig::default()).expect("default behaviour config serializes");
    assert!(default_serialized.contains("max_input_lines = 10"));

    let config: BehaviourConfig =
        toml::from_str("max_input_lines = 37\nenable_history = false\nhistory_size = 7\nshow_queued_inputs = false")
            .expect("valid behaviour config");
    let serialized = toml::to_string(&config).expect("behaviour config serializes");

    assert!(serialized.contains("max_input_lines = 37"));
}
