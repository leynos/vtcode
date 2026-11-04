use std::env;
use std::path::Path;

use super::types::{UpdateAction, UpdateExecutionStrategy};
use diapason_commons::canonicalize;

const CURL_INSTALL_COMMAND: &str =
    "curl -fsSL https://raw.githubusercontent.com/leynos/vtcode/main/scripts/install.sh | bash";
const WINDOWS_INSTALL_COMMAND: &str =
    "irm https://raw.githubusercontent.com/leynos/vtcode/main/scripts/install.ps1 | iex";
const SOURCE_BUILD_UPDATE_COMMAND: &str =
    "cargo install --git https://github.com/leynos/vtcode.git --locked --force diapason";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallSource {
    Standalone,
    Homebrew,
    Cargo,
    Npm,
    Scoop,
    Snap,
    Flatpak,
    Nix,
    Winget,
}

impl InstallSource {
    pub(crate) fn is_managed(self) -> bool {
        !matches!(self, Self::Standalone)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Scoop => "scoop",
            Self::Snap => "snap",
            Self::Flatpak => "flatpak",
            Self::Nix => "nix",
            Self::Winget => "winget",
        }
    }

    pub(crate) fn update_action(self) -> UpdateAction {
        self.update_action_for_os(env::consts::OS)
    }

    pub(crate) fn update_action_for_os(self, os: &str) -> UpdateAction {
        match self {
            Self::Standalone if os == "windows" => UpdateAction {
                source_label: self.label(),
                display_command: WINDOWS_INSTALL_COMMAND,
                execution: UpdateExecutionStrategy::PowerShell,
                prefer_path_relaunch: false,
            },
            Self::Standalone => UpdateAction {
                source_label: self.label(),
                display_command: CURL_INSTALL_COMMAND,
                execution: UpdateExecutionStrategy::Shell,
                prefer_path_relaunch: false,
            },
            _ => UpdateAction {
                source_label: self.label(),
                display_command: SOURCE_BUILD_UPDATE_COMMAND,
                execution: if os == "windows" {
                    UpdateExecutionStrategy::PowerShell
                } else {
                    UpdateExecutionStrategy::Shell
                },
                prefer_path_relaunch: true,
            },
        }
    }
}

pub(super) fn detect_install_source() -> InstallSource {
    let exe = match env::current_exe() {
        Ok(path) => path,
        Err(_) => return InstallSource::Standalone,
    };

    let canonical = canonicalize(&exe).unwrap_or(exe);
    detect_install_source_from_path(&canonical)
}

pub(super) fn detect_install_source_from_path(path: &Path) -> InstallSource {
    let path_text = path.to_string_lossy().to_ascii_lowercase();

    // --- Unix package managers (path uses /) ---

    if path_text.contains("/cellar/")
        || path_text.contains("/homebrew/")
        || path_text.contains("/linuxbrew/")
        || path_text.contains("/opt/homebrew/")
    {
        return InstallSource::Homebrew;
    }

    if path_text.contains("/.cargo/bin/") {
        return InstallSource::Cargo;
    }

    if path_text.contains("/node_modules/") || path_text.contains("/npm/") {
        return InstallSource::Npm;
    }

    // Nix: after canonicalize(), ~/.nix-profile/bin/diapason resolves to /nix/store/...
    // Non-canonicalized paths use /.nix-profile/ (with dot).
    if path_text.contains("/nix/store/") || path_text.contains("/.nix-profile/") {
        return InstallSource::Nix;
    }

    // Snap: executables exposed at /snap/bin/<name>, data under /snap/<name>/
    if path_text.contains("/snap/") {
        return InstallSource::Snap;
    }

    // Flatpak: system at /var/lib/flatpak/app/, user at ~/.local/share/flatpak/app/
    if path_text.contains("/flatpak/app/") || path_text.contains("/flatpak/exports/") {
        return InstallSource::Flatpak;
    }

    // --- Windows package managers (path uses \) ---

    // Scoop: per-user at ~\scoop\apps\, global at %ProgramData%\scoop\apps\,
    // shims at ~\scoop\shims\
    if path_text.contains("\\scoop\\apps\\") || path_text.contains("\\scoop\\shims\\") {
        return InstallSource::Scoop;
    }

    // Winget: portable installs at %LOCALAPPDATA%\Microsoft\WinGet\Packages\
    if path_text.contains("\\winget\\packages\\") {
        return InstallSource::Winget;
    }

    InstallSource::Standalone
}

pub(super) fn get_target_triple() -> Option<&'static str> {
    get_target_triple_for(env::consts::OS, env::consts::ARCH)
}

/// Resolve the release asset target triple for an `(os, arch)` pair.
///
/// Returns `None` for platforms the release pipeline does not publish.
/// Windows ARM is intentionally unsupported until the build workflow produces
/// that target; returning `None` surfaces "Unsupported platform for
/// auto-update" instead of a misleading "no compatible archive found".
pub(super) fn get_target_triple_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

pub(super) fn binary_name_for_target(target: &str) -> &'static str {
    if target.contains("-pc-windows-") {
        "diapason.exe"
    } else {
        "diapason"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_unix_uses_shell_installer() {
        let action = InstallSource::Standalone.update_action_for_os("linux");
        assert_eq!(action.display_command, CURL_INSTALL_COMMAND);
        assert_eq!(action.execution, UpdateExecutionStrategy::Shell);
    }

    #[test]
    fn standalone_windows_uses_powershell_installer() {
        let action = InstallSource::Standalone.update_action_for_os("windows");
        assert_eq!(action.display_command, WINDOWS_INSTALL_COMMAND);
        assert_eq!(action.execution, UpdateExecutionStrategy::PowerShell);
    }

    #[test]
    fn target_binary_name_matches_platform_archive() {
        assert_eq!(binary_name_for_target("x86_64-unknown-linux-musl"), "diapason");
        assert_eq!(binary_name_for_target("x86_64-pc-windows-msvc"), "diapason.exe");
    }

    #[test]
    fn supported_target_triples_match_release_pipeline() {
        for (os, arch, target) in [
            ("macos", "x86_64", "x86_64-apple-darwin"),
            ("macos", "aarch64", "aarch64-apple-darwin"),
            ("linux", "x86_64", "x86_64-unknown-linux-musl"),
            ("linux", "aarch64", "aarch64-unknown-linux-gnu"),
            ("windows", "x86_64", "x86_64-pc-windows-msvc"),
        ] {
            assert_eq!(get_target_triple_for(os, arch), Some(target), "({os}, {arch}) should resolve to {target}");
        }
    }

    #[test]
    fn windows_aarch64_is_unsupported_until_pipeline_publishes_it() {
        assert_eq!(get_target_triple_for("windows", "aarch64"), None);
    }

    #[test]
    fn managed_sources_use_fork_source_build_command() {
        struct PlatformCase {
            os: &'static str,
            execution: UpdateExecutionStrategy,
        }
        let platforms = [
            PlatformCase {
                os: "linux",
                execution: UpdateExecutionStrategy::Shell,
            },
            PlatformCase {
                os: "windows",
                execution: UpdateExecutionStrategy::PowerShell,
            },
        ];
        for source in [
            InstallSource::Homebrew,
            InstallSource::Cargo,
            InstallSource::Npm,
            InstallSource::Scoop,
            InstallSource::Snap,
            InstallSource::Flatpak,
            InstallSource::Nix,
            InstallSource::Winget,
        ] {
            for platform in &platforms {
                let os = platform.os;
                let action = source.update_action_for_os(os);
                assert_eq!(action.display_command, SOURCE_BUILD_UPDATE_COMMAND, "{} on {os}", source.label());
                assert_eq!(action.execution, platform.execution, "{} on {os}", source.label());
                assert!(action.prefer_path_relaunch, "{} on {os}", source.label());
            }
        }
    }

    #[test]
    fn scoop_detected_on_windows_path() {
        assert_eq!(
            detect_install_source_from_path(Path::new("C:\\Users\\dev\\scoop\\apps\\diapason\\0.1.0\\diapason.exe")),
            InstallSource::Scoop
        );
    }

    #[test]
    fn scoop_shim_detected() {
        assert_eq!(
            detect_install_source_from_path(Path::new("C:\\Users\\dev\\scoop\\shims\\diapason.exe")),
            InstallSource::Scoop
        );
    }

    #[test]
    fn snap_detected() {
        assert_eq!(detect_install_source_from_path(Path::new("/snap/diapason/123/bin/diapason")), InstallSource::Snap);
    }

    #[test]
    fn nix_store_detected() {
        assert_eq!(
            detect_install_source_from_path(Path::new("/nix/store/abc123-diapason-0.1.0/bin/diapason")),
            InstallSource::Nix
        );
    }

    #[test]
    fn nix_profile_detected() {
        assert_eq!(
            detect_install_source_from_path(Path::new("/home/dev/.nix-profile/bin/diapason")),
            InstallSource::Nix
        );
    }

    #[test]
    fn winget_portable_detected() {
        assert_eq!(
            detect_install_source_from_path(Path::new(
                "C:\\Users\\dev\\AppData\\Local\\Microsoft\\WinGet\\Packages\\diapason\\diapason.exe"
            )),
            InstallSource::Winget
        );
    }

    #[test]
    fn flatpak_detected() {
        assert_eq!(
            detect_install_source_from_path(Path::new("/var/lib/flatpak/app/app/diapason/current/active/bin/diapason")),
            InstallSource::Flatpak
        );
    }

    #[test]
    fn all_new_sources_are_managed() {
        for source in [
            InstallSource::Scoop,
            InstallSource::Snap,
            InstallSource::Flatpak,
            InstallSource::Nix,
            InstallSource::Winget,
        ] {
            assert!(source.is_managed(), "{} should be managed", source.label());
        }
    }
}
