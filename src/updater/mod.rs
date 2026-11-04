mod archive;
mod cache;
mod download;
mod github;
mod install_source;
mod interactive;
mod preflight;
mod progress;
mod release_notes;
mod types;

use anyhow::{Context, Result, bail};
use diapason_config::update::UpdateConfig;
use semver::Version;
use tracing::{debug, info};

pub(crate) use install_source::InstallSource;
pub(crate) use interactive::{
    InlineUpdateOutcome, append_notice_highlight, display_release_notes, display_update_notice, execute_inline_update,
    run_inline_update_prompt,
};
pub(crate) use preflight::{get_preflight_notice, run_preflight_check};
pub(crate) use progress::UpdateProgress;
pub(crate) use types::{
    InstallOutcome, StartupUpdateCheck, StartupUpdateNotice, UpdateExecutionStrategy, UpdateGuidance, UpdateInfo,
    VersionInfo,
};

/// Auto-updater for Diapason binary from GitHub Releases
pub(crate) struct Updater {
    current_version: Version,
    config: UpdateConfig,
}

impl Updater {
    pub(crate) fn new(current_version_str: &str) -> Result<Self> {
        let current_version = Version::parse(current_version_str)
            .with_context(|| format!("Invalid version format: {current_version_str}"))?;

        let config = UpdateConfig::load().unwrap_or_else(|e| {
            debug!("Failed to load update config, using defaults: {}", e);
            UpdateConfig::default()
        });

        Ok(Self { current_version, config })
    }

    pub(crate) fn current_version(&self) -> &Version {
        &self.current_version
    }

    pub(crate) fn config(&self) -> &UpdateConfig {
        &self.config
    }

    pub(crate) fn release_url(version: &Version) -> String {
        github::release_url(version)
    }

    pub(crate) async fn check_for_updates(&mut self) -> Result<Option<UpdateInfo>> {
        debug!("Checking for Diapason updates (channel: {})...", self.config.channel);

        if let Some(pinned_version) = self.config.pinned_version() {
            if self.config.should_auto_unpin() {
                // Auto-unpin: fetch the latest release and, if it is newer
                // than the pinned version, clear the pin so the update proceeds.
                debug!("Version pinned to {} with auto-unpin enabled, checking for newer release", pinned_version);
                let latest =
                    github::fetch_latest_release(self, self.config.download_timeout_secs, &self.config.channel).await?;
                if let Some(info) = latest.as_ref()
                    && info.version > *pinned_version
                {
                    info!("Auto-unpinning: newer version {} available (pinned: {})", info.version, pinned_version);
                    self.config.clear_pin();
                    let _ = self.config.save();
                    return Ok(latest);
                }
                debug!("Pinned version is still latest, keeping pin");
                return Ok(None);
            }
            debug!("Version pinned to {}, skipping update check", pinned_version);
            return Ok(None);
        }

        let latest =
            github::fetch_latest_release(self, self.config.download_timeout_secs, &self.config.channel).await?;

        if latest.as_ref().is_some_and(|info| info.version > self.current_version) {
            if let Some(latest) = latest.as_ref() {
                info!("New version available: {} (current: {})", latest.version, self.current_version);
            }
        } else {
            debug!("Already on latest version");
        }

        Ok(latest)
    }

    pub(crate) fn startup_update_check(&self) -> Result<StartupUpdateCheck> {
        if self.config.check_interval_hours == 0 {
            debug!("Startup update checks disabled by configuration");
            return Ok(StartupUpdateCheck::default());
        }

        if let Some(pinned_version) = self.config.pinned_version() {
            debug!("Version pinned to {}, suppressing startup update prompt", pinned_version);
            return Ok(StartupUpdateCheck::default());
        }

        let snapshot = cache::read_snapshot()?;
        let dismissed = snapshot.dismissed_version.as_ref();

        let cached_notice = snapshot.latest_version.as_ref().and_then(|latest_version| {
            if snapshot.latest_was_newer && latest_version > &self.current_version && dismissed != Some(latest_version)
            {
                Some(self.notice_for_version(latest_version.clone()))
            } else {
                None
            }
        });

        Ok(StartupUpdateCheck {
            cached_notice,
            should_refresh: self.config.is_check_due(snapshot.last_checked),
        })
    }

    pub(crate) async fn refresh_startup_update_cache(&self) -> Result<Option<StartupUpdateNotice>> {
        if self.config.check_interval_hours == 0 || self.config.is_pinned() {
            return Ok(None);
        }

        let latest = match github::fetch_latest_release_info(self.config.download_timeout_secs).await {
            Ok(info) => info,
            Err(err) => {
                let _ = cache::record_failed_check();
                return Err(err);
            }
        };

        let latest_is_newer = latest.version > self.current_version;
        cache::record_successful_check_with_notes(
            Some(&latest.version),
            latest_is_newer,
            Some(latest.release_notes.as_str()),
        )?;

        Ok(latest_is_newer.then(|| self.notice_for_version(latest.version)))
    }

    pub(crate) async fn install_update(&self, force: bool) -> Result<InstallOutcome> {
        // The CLI path shows download progress. The inline TUI path passes
        // false so download output does not leak into the alternate screen.
        self.install_update_with_progress(force, true).await
    }

    pub(crate) async fn install_update_with_progress(
        &self,
        force: bool,
        show_progress: bool,
    ) -> Result<InstallOutcome> {
        self.install_update_reported(force, show_progress, |_| {}).await
    }

    /// Install the latest release with per-phase progress reporting.
    ///
    /// `on_progress` is called with [`UpdateProgress`] events as the pipeline
    /// advances through download, checksum verification, extraction, and binary
    /// replacement. The download callback is throttled internally so callers can
    /// forward each event directly to the UI without flooding the render channel.
    pub(crate) async fn install_update_reported(
        &self,
        force: bool,
        show_progress: bool,
        mut on_progress: impl FnMut(UpdateProgress) + Send,
    ) -> Result<InstallOutcome> {
        let guidance = self.update_guidance();
        if guidance.source.is_managed() {
            bail!("Diapason was installed via {}. Update with: {}", guidance.source.label(), guidance.command());
        }

        let target = install_source::get_target_triple().context("Unsupported platform for auto-update")?;
        let release =
            github::fetch_latest_release_metadata(self.config.download_timeout_secs, &self.config.channel).await?;
        if !force && release.version <= self.current_version {
            return Ok(InstallOutcome::UpToDate(self.current_version.to_string()));
        }

        let (asset, archive_kind) = github::select_archive_asset(&release.assets, target)?;
        let checksum_asset = download::checksum_asset(&release.assets, &asset.name)
            .with_context(|| format!("no checksum metadata was published for update archive {}", asset.name))?;
        let binary_name = install_source::binary_name_for_target(target);
        let temporary_directory = tempfile::tempdir().context("Failed to create update temporary directory")?;
        // Keep remote asset names out of filesystem paths; GitHub metadata is
        // untrusted even though the selected URL and format were validated.
        let archive_path = temporary_directory.path().join("downloaded-update-archive");
        {
            let mut report_download = |downloaded: u64, total: Option<u64>| {
                on_progress(UpdateProgress::Downloading { downloaded, total });
            };
            download::download_asset(
                asset,
                &archive_path,
                std::time::Duration::from_secs(self.config.download_timeout_secs.max(1)),
                show_progress,
                Some(&mut report_download),
            )
            .await
            .context("Failed to download update archive")?;
        }

        on_progress(UpdateProgress::VerifyingChecksum);
        let metadata = download::download_checksum(
            checksum_asset,
            std::time::Duration::from_secs(self.config.download_timeout_secs.max(1)),
        )
        .await
        .with_context(|| format!("failed to download checksum metadata for update archive {}", asset.name))?;
        let expected = download::parse_checksum_metadata(&metadata, &asset.name)
            .with_context(|| format!("checksum metadata did not contain update archive {}", asset.name))?;
        download::verify_file_checksum(&archive_path, &expected)
            .context("downloaded update archive failed checksum verification")?;

        on_progress(UpdateProgress::Extracting);
        let extraction_directory = temporary_directory.path().join("extracted");
        let archive_path_for_extraction = archive_path.clone();
        let extracted_binary = tokio::task::spawn_blocking(move || {
            archive::extract_binary(&archive_path_for_extraction, archive_kind, &extraction_directory, binary_name)
        })
        .await
        .context("Update extraction task join failed")??;

        on_progress(UpdateProgress::ReplacingBinary);
        self_replace::self_replace(&extracted_binary).context("Failed to replace the current Diapason binary")?;
        Ok(InstallOutcome::Updated(release.version.to_string()))
    }

    pub(crate) fn update_guidance(&self) -> UpdateGuidance {
        let source = install_source::detect_install_source();
        UpdateGuidance { source, action: source.update_action() }
    }

    pub(crate) async fn list_versions(&self, limit: usize) -> Result<Vec<VersionInfo>> {
        debug!("Fetching available versions (limit: {})...", limit);
        github::list_versions(limit, self.config.download_timeout_secs).await
    }

    pub(crate) fn pin_version(&mut self, version: Version, reason: Option<String>, auto_unpin: bool) -> Result<()> {
        self.config.set_pin(version, reason, auto_unpin);
        self.config
            .save()
            .context("Failed to save update config after pinning version")?;
        Ok(())
    }

    pub(crate) fn unpin_version(&mut self) -> Result<()> {
        self.config.clear_pin();
        self.config
            .save()
            .context("Failed to save update config after unpinning version")?;
        Ok(())
    }

    pub(crate) fn is_pinned(&self) -> bool {
        self.config.is_pinned()
    }

    pub(crate) fn pinned_version(&self) -> Option<&Version> {
        self.config.pinned_version()
    }

    pub(crate) fn notice_for_version(&self, latest_version: Version) -> StartupUpdateNotice {
        StartupUpdateNotice {
            current_version: self.current_version.clone(),
            latest_version,
            guidance: self.update_guidance(),
        }
    }
}

/// Check whether release notes should be shown for the current version.
///
/// Returns `true` if the current version has not been recorded as "seen" in the
/// update cache.
pub(crate) fn should_show_release_notes_for_current_version() -> bool {
    let current = match Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(v) => v,
        Err(_) => return false,
    };
    match cache::read_snapshot() {
        Ok(snapshot) => snapshot.last_seen_version.as_ref() != Some(&current),
        Err(err) => {
            debug!("Failed to read update cache for release notes check: {err}");
            false
        }
    }
}

pub(crate) fn cached_current_release_highlights() -> Option<(Version, Vec<String>)> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
    let (_, release_notes) = cache::current_release_info(&current)?;
    let parsed = release_notes::parse_highlights(&current, &release_notes);
    (!parsed.items.is_empty()).then_some((current, parsed.items))
}

/// Record that the current version's release notes have been shown.
pub(crate) fn record_current_version_seen() {
    if let Ok(current) = Version::parse(env!("CARGO_PKG_VERSION")) {
        let _ = cache::record_seen_version(&current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_version_parsing() {
        let updater = Updater::new("0.58.4").expect("updater");
        assert_eq!(updater.current_version().major, 0);
        assert_eq!(updater.current_version().minor, 58);
        assert_eq!(updater.current_version().patch, 4);
    }

    #[test]
    fn test_install_source_detection() {
        assert_eq!(
            install_source::detect_install_source_from_path(Path::new(
                "/opt/homebrew/Cellar/diapason/0.1/bin/diapason"
            )),
            InstallSource::Homebrew
        );
        assert_eq!(
            install_source::detect_install_source_from_path(Path::new("/Users/dev/.cargo/bin/diapason")),
            InstallSource::Cargo
        );
        assert_eq!(
            install_source::detect_install_source_from_path(Path::new(
                "/usr/local/lib/node_modules/diapason/bin/diapason"
            )),
            InstallSource::Npm
        );
        assert_eq!(
            install_source::detect_install_source_from_path(Path::new("/usr/local/bin/diapason")),
            InstallSource::Standalone
        );
    }

    #[test]
    fn startup_update_check_respects_disabled_interval() {
        let updater = Updater {
            current_version: Version::parse("0.111.0").expect("version"),
            config: UpdateConfig { check_interval_hours: 0, ..UpdateConfig::default() },
        };

        let check = updater.startup_update_check().expect("startup check");
        assert!(check.cached_notice.is_none());
        assert!(!check.should_refresh);
    }

    #[test]
    fn startup_update_check_respects_pinned_version() {
        let mut config = UpdateConfig::default();
        config.set_pin(Version::parse("0.111.0").expect("version"), None, false);
        let updater = Updater {
            current_version: Version::parse("0.111.0").expect("version"),
            config,
        };

        let check = updater.startup_update_check().expect("startup check");
        assert!(check.cached_notice.is_none());
        assert!(!check.should_refresh);
    }

    #[test]
    fn startup_update_check_suppresses_dismissed_version() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("temp dir");
        let previous = cache::set_cache_dir_override_for_tests(Some(temp_dir.path().to_path_buf()));

        let dismissed = Version::parse("0.113.0").expect("dismissed");
        cache::record_successful_check(Some(&dismissed), true).expect("write cache");
        cache::record_dismissed_version(&dismissed).expect("record dismissal");

        let updater = Updater {
            current_version: Version::parse("0.111.0").expect("current"),
            config: UpdateConfig::default(),
        };

        let check = updater.startup_update_check().expect("startup check");
        assert!(check.cached_notice.is_none(), "Dismissed version should not produce a cached notice");

        cache::set_cache_dir_override_for_tests(previous);
    }

    #[test]
    fn startup_update_check_shows_undismissed_newer_version() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("temp dir");
        let previous = cache::set_cache_dir_override_for_tests(Some(temp_dir.path().to_path_buf()));

        let latest = Version::parse("0.114.0").expect("latest");
        cache::record_successful_check(Some(&latest), true).expect("write cache");
        // Only dismissed 0.113.0, not 0.114.0 — notice should appear
        cache::record_dismissed_version(&Version::parse("0.113.0").expect("older")).expect("record dismissal");

        let updater = Updater {
            current_version: Version::parse("0.111.0").expect("current"),
            config: UpdateConfig::default(),
        };

        let check = updater.startup_update_check().expect("startup check");
        assert!(check.cached_notice.is_some(), "Newer, non-dismissed version should produce a cached notice");

        cache::set_cache_dir_override_for_tests(previous);
    }
}
