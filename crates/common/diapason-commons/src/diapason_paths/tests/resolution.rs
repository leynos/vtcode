//! Extracted regression tests; production source remains byte-identical.

#[cfg(unix)]
use std::fs;

use super::super::*;
#[cfg(unix)]
use super::migration_paths;
use tempfile::tempdir;

#[test]
fn resolver_defaults_are_absolute_and_categories_are_separate() {
    let paths = DiapasonPaths::from_environment(&[]).expect("resolve defaults");
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
    let paths = DiapasonPaths::from_environment(&[
        ("HOME", "/tmp/diapason-home"),
        ("XDG_CONFIG_HOME", "relative/config"),
        ("XDG_RUNTIME_DIR", "relative/runtime"),
        ("XDG_BIN_HOME", "relative/bin"),
    ])
    .expect("relative XDG root should be ignored");
    assert_eq!(paths.config_dir(), Path::new("/tmp/diapason-home/.config/diapason"));
    assert_eq!(paths.runtime_dir(), Path::new("/tmp/diapason-home/.local/state/diapason/runtime"));
    assert_eq!(paths.executable_dir(), Path::new("/tmp/diapason-home/.local/bin"));
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
    let paths = DiapasonPaths::from_environment(&[
        ("HOME", "/tmp/diapason-home"),
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
    let paths =
        DiapasonPaths::from_environment(&[("HOME", "/tmp/diapason-home"), ("XDG_CONFIG_DIRS", "/first:/second")])
            .expect("resolve search roots");

    assert_eq!(
        paths.system_config_paths("config.toml").expect("resolve system config paths"),
        vec![
            PathBuf::from("/etc/diapason/config.toml"),
            PathBuf::from("/second/diapason/config.toml"),
            PathBuf::from("/first/diapason/config.toml"),
        ]
    );
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

#[test]
fn resolver_ignores_application_specific_root_overrides() {
    let temp = tempdir().expect("home root");
    let home = temp.path().to_str().expect("UTF-8 home");
    let paths = DiapasonPaths::from_environment(&[
        ("HOME", home),
        ("DIAPASON_HOME", "invalid-relative-home"),
        ("DIAPASON_CONFIG", "invalid-relative-config"),
        ("DIAPASON_DATA", "invalid-relative-data"),
    ])
    .expect("ignore former override variables");
    assert_eq!(paths.config_dir(), temp.path().join(".config/diapason"));
    assert_eq!(paths.data_dir(), temp.path().join(".local/share/diapason"));
    assert_eq!(paths.legacy_home_dir(), temp.path().join(".diapason"));
}

#[test]
fn canonical_files_and_durable_state_are_separate_from_legacy_home() {
    let temp = tempdir().expect("tempdir");
    let paths = migration_paths(&temp);
    assert_eq!(paths.config_file(), temp.path().join("config/config.toml"));
    assert_eq!(paths.agents_file(), temp.path().join("config/AGENTS.md"));
    assert_eq!(paths.logs_dir(), temp.path().join("state/logs"));
    assert_eq!(paths.audit_dir(), temp.path().join("state/audit"));
    assert_eq!(paths.sessions_dir(), temp.path().join("state/sessions"));
    assert_eq!(paths.telemetry_dir(), temp.path().join("state/telemetry"));
}

#[test]
fn resolver_ignores_vtcode_environment_and_existing_home_directories() {
    let temp = tempdir().expect("tempdir");
    let former_home = temp.path().join(".vtcode");
    fs::create_dir(&former_home).expect("create former application directory");
    fs::write(former_home.join("vtcode.toml"), "invalid old configuration").expect("write old configuration");
    let home = temp.path().to_str().expect("UTF-8 temporary directory");
    let old = former_home.to_str().expect("UTF-8 former directory");
    let paths = DiapasonPaths::from_environment(&[
        ("HOME", home),
        ("VTCODE_HOME", old),
        ("VTCODE_CONFIG", old),
        ("VTCODE_DATA", old),
    ])
    .expect("resolve without former environment");
    assert!(!paths.config_dir().starts_with(&former_home));
    assert!(!paths.state_dir().starts_with(&former_home));
    assert!(!paths.data_dir().starts_with(&former_home));
    assert!(!paths.config_file().exists());
    assert_eq!(
        fs::read_to_string(former_home.join("vtcode.toml")).expect("read unchanged fixture"),
        "invalid old configuration"
    );
}

#[test]
fn all_platforms_honour_explicit_xdg_roots() {
    let temp = tempdir().expect("temporary XDG roots");
    let config = temp.path().join("configuration");
    let state = temp.path().join("durable-state");
    let data = temp.path().join("application-data");
    let cache = temp.path().join("cached-data");
    let paths = DiapasonPaths::from_environment(&[
        ("XDG_CONFIG_HOME", config.to_str().expect("UTF-8 config path")),
        ("XDG_STATE_HOME", state.to_str().expect("UTF-8 state path")),
        ("XDG_DATA_HOME", data.to_str().expect("UTF-8 data path")),
        ("XDG_CACHE_HOME", cache.to_str().expect("UTF-8 cache path")),
    ])
    .expect("resolve XDG roots");
    assert_eq!(paths.config_dir(), config.join("diapason"));
    assert_eq!(paths.state_dir(), state.join("diapason"));
    assert_eq!(paths.data_dir(), data.join("diapason"));
    assert_eq!(paths.cache_dir(), cache.join("diapason"));
}
