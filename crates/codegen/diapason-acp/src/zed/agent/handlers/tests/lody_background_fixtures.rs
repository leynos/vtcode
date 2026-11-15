//! Executable background-subagent fixtures for the Lody management tests.
//!
//! The constants and writers live apart from the protocol helpers so the
//! connection and request support in `lody_support` stays within its size
//! budget.

/// The demo background script: two lines of output, then a long sleep.
pub(super) const LODY_BACKGROUND_SCRIPT: &str = "#!/bin/sh\nprintf 'lody-output-one\\nlody-output-two\\n'\nsleep 30\n";

/// The demo background script with no sleep, so its process exits at once.
///
/// The exit-case regression rewrites the shared fixture with this body, so a
/// task's cached record can outlive its process without any controller path
/// settling it.
pub(super) const LODY_EXITING_BACKGROUND_SCRIPT: &str = "#!/bin/sh\nprintf 'lody-output-one\\nlody-output-two\\n'\n";

pub(super) fn write_lody_background_fixture(workspace: &std::path::Path) {
    let agent_dir = workspace.join(".diapason/agents");
    std::fs::create_dir_all(&agent_dir).expect("create Lody test agent directory");
    std::fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Lody management protocol fixture.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background fixture.
"#,
    )
    .expect("write Lody test agent");
    write_lody_background_script(workspace, LODY_BACKGROUND_SCRIPT);
}

/// Replaces the demo background script with `body`.
pub(super) fn write_lody_background_script(workspace: &std::path::Path, body: &str) {
    let scripts_dir = workspace.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create Lody test scripts directory");
    let script = scripts_dir.join("demo-background-subagent.sh");
    std::fs::write(&script, body).expect("write Lody background script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script)
            .expect("Lody background script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("make Lody background script executable");
    }
}
