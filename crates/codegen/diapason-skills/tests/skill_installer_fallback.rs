//! Offline fallback contract for the skill-installer sample script.
//!
//! The embedded catalogue is the documented offline path, so this test pins
//! the observable behaviour: an unreachable `--source` URL still lists the
//! bundled skills and exits successfully.

use std::path::PathBuf;
use std::process::Command;

/// Locate the sample script shipped with the crate.
fn sample_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("skills")
        .join("assets")
        .join("samples")
        .join("skill-installer")
        .join("scripts")
        .join("list-curated-skills.py")
}

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn list_curated_skills_falls_back_to_embedded_catalogue() {
    if !python3_available() {
        eprintln!("python3 not available, skipping skill-installer offline fallback test");
        return;
    }

    let script = sample_script();
    assert!(script.is_file(), "skill-installer sample script missing: {}", script.display());

    let output = Command::new("python3")
        .arg(&script)
        .args(["--source", "http://127.0.0.1:9/catalogue.json", "--json"])
        .output()
        .expect("failed to run the skill-installer sample script");

    assert!(
        output.status.success(),
        "script failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Falling back to embedded catalogue"),
        "expected the offline fallback notice in stderr, got: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let skills: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("script stdout was not JSON: {error}\n{stdout}"));
    let names: Vec<&str> = skills
        .as_array()
        .expect("expected a JSON array of skills")
        .iter()
        .filter_map(|skill| skill.get("name").and_then(serde_json::Value::as_str))
        .collect();

    assert!(names.contains(&"skill-creator"), "missing skill-creator in {names:?}");
    assert!(names.contains(&"skill-installer"), "missing skill-installer in {names:?}");
}
