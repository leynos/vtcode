//! Extracted regression tests; production source remains byte-identical.

#[cfg(unix)]
use std::fs;

use super::super::*;
#[cfg(unix)]
use super::migration_paths;
use tempfile::tempdir;

#[test]
fn resolver_honours_explicit_overrides() {
    let paths = VtCodePaths::from_environment(&[
        ("VTCODE_HOME", "/ignored"),
        ("VTCODE_CONFIG", "/config"),
        ("VTCODE_DATA", "/data"),
    ])
    .expect("resolve paths");
    assert_eq!(paths.config_dir(), Path::new("/config"));
    assert_eq!(paths.data_dir(), Path::new("/data"));
    assert_eq!(paths.legacy_home_dir(), Path::new("/ignored"));
    assert_ne!(paths.auth_dir(), Path::new("/data/auth"));
}

#[test]
fn resolver_defaults_are_absolute_and_categories_are_separate() {
    let paths = VtCodePaths::from_environment(&[]).expect("resolve defaults");
    assert!(paths.config_dir().is_absolute());
    assert!(paths.data_dir().is_absolute());
    assert!(paths.runtime_dir().is_absolute());
    assert_eq!(paths.auth_file(), paths.auth_dir().join("auth.json"));
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
#[test]
fn resolver_ignores_relative_xdg_inputs() {
    let paths = VtCodePaths::from_environment(&[
        ("HOME", "/tmp/vtcode-home"),
        ("XDG_CONFIG_HOME", "relative/config"),
        ("XDG_RUNTIME_DIR", "relative/runtime"),
        ("XDG_BIN_HOME", "relative/bin"),
    ])
    .expect("relative XDG root should be ignored");
    assert_eq!(paths.config_dir(), Path::new("/tmp/vtcode-home/.config/vtcode"));
    assert_eq!(paths.runtime_dir(), Path::new("/tmp/vtcode-home/.local/state/vtcode/runtime"));
    assert_eq!(paths.executable_dir(), Path::new("/tmp/vtcode-home/.local/bin"));
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
#[test]
fn resolver_preserves_xdg_search_order_and_defaults_empty_values() {
    let paths = VtCodePaths::from_environment(&[
        ("HOME", "/tmp/vtcode-home"),
        ("XDG_CONFIG_DIRS", "/first:/second"),
        ("XDG_DATA_DIRS", " "),
    ])
    .expect("resolve search roots");
    assert_eq!(paths.system_config_dirs(), &[PathBuf::from("/first"), PathBuf::from("/second")]);
    assert_eq!(paths.system_data_dirs(), &[PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
#[test]
fn system_config_paths_convert_xdg_preference_order_to_layer_order() {
    let paths = VtCodePaths::from_environment(&[("HOME", "/tmp/vtcode-home"), ("XDG_CONFIG_DIRS", "/first:/second")])
        .expect("resolve search roots");

    assert_eq!(
        paths.system_config_paths("vtcode.toml").expect("resolve system config paths"),
        vec![
            PathBuf::from("/etc/vtcode/vtcode.toml"),
            PathBuf::from("/second/vtcode/vtcode.toml"),
            PathBuf::from("/first/vtcode/vtcode.toml"),
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_macos_resolution_ignores_xdg_roots() {
    let paths = VtCodePaths::from_environment(&[
        ("HOME", "/tmp/vtcode-home"),
        ("XDG_CONFIG_HOME", "/tmp/xdg/config"),
        ("XDG_DATA_HOME", "/tmp/xdg/data"),
        ("XDG_STATE_HOME", "/tmp/xdg/state"),
        ("XDG_CACHE_HOME", "/tmp/xdg/cache"),
    ])
    .expect("resolve native macOS paths");
    assert!(!paths.config_dir().starts_with("/tmp/xdg"));
    assert!(!paths.data_dir().starts_with("/tmp/xdg"));
    assert!(paths.config_dir().to_string_lossy().contains("com.vinhnx.vtcode"));
}

#[cfg(target_os = "windows")]
#[test]
fn native_windows_resolution_ignores_xdg_roots() {
    let paths = VtCodePaths::from_environment(&[
        ("HOME", r"C:\\Users\\vtcode"),
        ("XDG_CONFIG_HOME", r"C:\\xdg\\config"),
        ("XDG_DATA_HOME", r"C:\\xdg\\data"),
        ("XDG_STATE_HOME", r"C:\\xdg\\state"),
        ("XDG_CACHE_HOME", r"C:\\xdg\\cache"),
    ])
    .expect("resolve native Windows paths");
    assert!(!paths.config_dir().to_string_lossy().contains("xdg"));
    assert!(!paths.data_dir().to_string_lossy().contains("xdg"));
}

#[cfg(unix)]
#[test]
fn runtime_and_auth_storage_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    let runtime = paths.ensure_runtime_dir().expect("create runtime");
    let auth = paths.ensure_auth_dir().expect("create auth");
    let auth_file = paths.create_auth_file("credentials.json").expect("create auth file");

    assert_eq!(fs::metadata(runtime).expect("runtime metadata").permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(auth).expect("auth metadata").permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(auth_file).expect("auth file metadata").permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn newly_created_user_directories_are_private_but_existing_modes_are_preserved() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    fs::create_dir_all(paths.config_dir()).expect("existing config directory");
    fs::set_permissions(paths.config_dir(), fs::Permissions::from_mode(0o755)).expect("set existing mode");

    let _ = paths.ensure_config_dir().expect("preserve existing config directory");
    let _ = paths.ensure_data_dir().expect("create data directory");
    let _ = paths.ensure_state_child_dir("sessions").expect("create state child");
    let _ = paths.ensure_cache_child_dir("prompts").expect("create cache child");
    let _ = paths.ensure_executable_dir().expect("create executable directory");

    assert_eq!(fs::metadata(paths.config_dir()).expect("config metadata").permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(paths.data_dir()).expect("data metadata").permissions().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(paths.state_dir().join("sessions"))
            .expect("state child metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(paths.cache_dir().join("prompts"))
            .expect("cache child metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(paths.executable_dir())
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    symlink(temp.path().join("outside"), paths.cache_dir().join("unsafe")).expect("create cache symlink");
    assert!(paths.ensure_cache_child_dir("unsafe/nested").is_err());
}
