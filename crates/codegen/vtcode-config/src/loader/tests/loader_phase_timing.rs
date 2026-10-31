//! Tests for configuration loader phase timing.

use super::*;

#[test]
fn test_config_loader_phase_timing_recorded() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let manager = ConfigManager::load_from_workspace(temp_dir.path()).expect("load workspace config");
    let timing = manager.phase_timing().expect("phase timing should be recorded");
    assert!(
        timing.path_resolution_us > 0
            || timing.layer_loading_us > 0
            || timing.merge_and_parse_us > 0
            || timing.validation_us > 0
    );
}
