use anyhow::{Context, Result, anyhow};
use hashbrown::HashMap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::{ConfigManager, VTCodeConfig};
use crate::defaults;

/// Configuration watcher that monitors config files for changes
/// and automatically reloads them when modifications are detected.
pub struct ConfigWatcher {
    workspace_path: PathBuf,
    last_load_time: Arc<Mutex<Instant>>,
    current_config: Arc<Mutex<Option<VTCodeConfig>>>,
    watcher: Option<RecommendedWatcher>,
    debounce_duration: Duration,
    last_event_time: Arc<Mutex<Instant>>,
}

impl ConfigWatcher {
    /// Create a new ConfigWatcher for the given workspace.
    #[must_use]
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            last_load_time: Arc::new(Mutex::new(Instant::now())),
            current_config: Arc::new(Mutex::new(None)),
            watcher: None,
            debounce_duration: Duration::from_millis(500),
            last_event_time: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Initialize the file watcher and load initial configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the initial config load fails or when the watcher
    /// cannot subscribe to config parent directories.
    pub async fn initialize(&mut self) -> Result<()> {
        self.load_config().await?;

        let last_event_time = Arc::clone(&self.last_event_time);
        let debounce_duration = self.debounce_duration;

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let now = Instant::now();
                    if let Ok(mut last_time) = last_event_time.lock()
                        && now.duration_since(*last_time) >= debounce_duration
                    {
                        *last_time = now;
                        if is_relevant_config_event(&event) {
                            tracing::debug!("Config file changed: {:?}", event);
                        }
                    }
                }
            },
            notify::Config::default(),
        )?;

        for path in get_config_file_paths(&self.workspace_path) {
            if let Some(parent) = path.parent() {
                watcher
                    .watch(parent, RecursiveMode::NonRecursive)
                    .with_context(|| format!("Failed to watch config directory: {parent:?}"))?;
            }
        }

        self.watcher = Some(watcher);
        Ok(())
    }

    /// Load or reload configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when internal watcher state cannot be updated.
    pub async fn load_config(&mut self) -> Result<()> {
        ConfigManager::invalidate_workspace_cache(&self.workspace_path);
        let config = ConfigManager::load_from_workspace(&self.workspace_path)
            .ok()
            .map(|manager| manager.config().clone());

        let mut current = self
            .current_config
            .lock()
            .map_err(|e| anyhow!("config watcher state lock poisoned: {e}"))?;
        *current = config;
        drop(current);

        let mut last_load = self
            .last_load_time
            .lock()
            .map_err(|e| anyhow!("config watcher timestamp lock poisoned: {e}"))?;
        *last_load = Instant::now();

        Ok(())
    }

    /// Get the current configuration, reloading if the watcher detected changes.
    pub async fn get_config(&mut self) -> Option<VTCodeConfig> {
        if self.should_reload().await
            && let Err(err) = self.load_config().await
        {
            tracing::warn!("Failed to reload config: {}", err);
        }

        self.current_config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    async fn should_reload(&self) -> bool {
        let Ok(last_event) = self.last_event_time.lock() else {
            return false;
        };
        let Ok(last_load) = self.last_load_time.lock() else {
            return false;
        };

        *last_event > *last_load
    }

    /// Get the last load time for debugging.
    #[must_use]
    pub async fn last_load_time(&self) -> Instant {
        self.last_load_time
            .lock()
            .map(|instant| *instant)
            .unwrap_or_else(|_| Instant::now())
    }
}

/// Simple config watcher that polls file mtimes instead of using filesystem events.
pub struct SimpleConfigWatcher {
    workspace_path: PathBuf,
    additional_paths: Vec<PathBuf>,
    last_load_time: Instant,
    last_check_time: Instant,
    check_interval: Duration,
    last_modified_times: HashMap<PathBuf, Option<SystemTime>>,
    debounce_duration: Duration,
    last_reload_attempt: Option<Instant>,
}

impl SimpleConfigWatcher {
    #[must_use]
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            additional_paths: Vec::new(),
            last_load_time: Instant::now(),
            last_check_time: Instant::now(),
            check_interval: Duration::from_secs(10),
            last_modified_times: HashMap::new(),
            debounce_duration: Duration::from_millis(1000),
            last_reload_attempt: None,
        }
    }

    /// Create a polling watcher that tracks all workspace and user-level
    /// configuration locations supported by the current defaults provider.
    ///
    /// Loading the manager here is best-effort. If a config file is malformed,
    /// the watcher still tracks the provider's default paths so a subsequent
    /// correction or newly-created user config can be observed.
    #[must_use]
    pub fn new_with_user_config_paths(workspace_path: PathBuf) -> Self {
        let mut watcher = Self::new(workspace_path.clone());
        if let Ok(manager) = ConfigManager::load_from_workspace(&workspace_path) {
            for path in manager.user_config_paths() {
                watcher.add_watch_path(path);
            }
        } else {
            let defaults = defaults::current_config_defaults();
            for path in defaults.home_config_paths(defaults.config_file_name()) {
                watcher.add_watch_path(path);
            }
        }
        watcher.seed_current_mtimes();
        watcher
    }

    /// Watch an additional config file (for example the user-level `vtcode.toml`)
    /// in addition to the workspace-local files.
    pub fn add_watch_path(&mut self, path: PathBuf) {
        if !self.additional_paths.contains(&path) {
            self.additional_paths.push(path);
        }
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = get_config_file_paths(&self.workspace_path);
        for path in &self.additional_paths {
            if !paths.iter().any(|existing| existing == path) {
                paths.push(path.clone());
            }
        }
        paths
    }

    fn seed_current_mtimes(&mut self) {
        for target in self.watch_paths() {
            let current_modified = latest_modified(&target);
            self.last_modified_times.insert(target, current_modified);
        }
    }

    pub fn should_reload(&mut self) -> bool {
        let now = Instant::now();

        if now.duration_since(self.last_check_time) < self.check_interval {
            return false;
        }
        self.last_check_time = now;

        let mut changed_paths = Vec::new();
        for target in self.watch_paths() {
            let current_modified = latest_modified(&target);
            match self.last_modified_times.get(&target).copied() {
                Some(previous) if previous != current_modified => {
                    // Keep the old baseline until the debounce window has
                    // elapsed. Otherwise a rapid edit can be consumed by a
                    // suppressed poll and never trigger a reload.
                    changed_paths.push((target, current_modified));
                }
                Some(_) => {}
                None => {
                    // The initial observation establishes a baseline. A
                    // later `None -> Some(mtime)` transition is a change.
                    self.last_modified_times.insert(target, current_modified);
                }
            }
        }

        if changed_paths.is_empty() {
            return false;
        }

        if let Some(last_attempt) = self.last_reload_attempt
            && now.duration_since(last_attempt) < self.debounce_duration
        {
            return false;
        }

        for (target, modified) in changed_paths {
            self.last_modified_times.insert(target, modified);
        }
        self.last_reload_attempt = Some(now);
        true
    }

    pub fn load_config(&mut self) -> Option<VTCodeConfig> {
        ConfigManager::invalidate_workspace_cache(&self.workspace_path);
        let config = ConfigManager::load_from_workspace(&self.workspace_path)
            .ok()
            .map(|manager| manager.config().clone());

        self.last_load_time = Instant::now();
        self.last_modified_times.clear();
        for target in self.watch_paths() {
            self.last_modified_times.insert(target.clone(), latest_modified(&target));
        }

        config
    }

    pub fn set_check_interval(&mut self, seconds: u64) {
        self.check_interval = Duration::from_secs(seconds);
    }

    pub fn set_debounce_duration(&mut self, millis: u64) {
        self.debounce_duration = Duration::from_millis(millis);
    }
}

fn is_relevant_config_event(event: &notify::Event) -> bool {
    let relevant_files = ["vtcode.toml", "theme.toml"];

    match &event.kind {
        notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_) => {
            event.paths.iter().any(|path| {
                path.file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| relevant_files.contains(&file_name))
            })
        }
        _ => false,
    }
}

fn get_config_file_paths(workspace_path: &Path) -> Vec<PathBuf> {
    vec![
        workspace_path.join("vtcode.toml"),
        workspace_path.join(".vtcode").join("theme.toml"),
    ]
}

fn latest_modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::SimpleConfigWatcher;

    fn open_check_window(watcher: &mut SimpleConfigWatcher) {
        // Advance the internal poll clock past the check interval so a change is
        // observed immediately instead of waiting out the default 10s interval.
        watcher.last_check_time = Instant::now().checked_sub(Duration::from_secs(11)).unwrap_or_else(Instant::now);
    }

    #[test]
    fn detects_workspace_config_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("vtcode.toml");
        std::fs::write(&config_path, "mode = \"auto\"\n").expect("write config");

        let mut watcher = SimpleConfigWatcher::new(dir.path().to_path_buf());
        watcher.set_debounce_duration(0);
        open_check_window(&mut watcher);

        assert!(!watcher.should_reload(), "baseline poll records mtimes, sees no change");

        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&config_path, "mode = \"hidden\"\n").expect("rewrite config");
        open_check_window(&mut watcher);

        assert!(watcher.should_reload(), "modified workspace config must trigger reload");
    }

    #[test]
    fn tracks_additional_watch_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_config = dir.path().join("vtcode.toml");
        let extra_config: PathBuf = dir.path().join("user-config.toml");
        std::fs::write(&workspace_config, "mode = \"auto\"\n").expect("write config");
        std::fs::write(&extra_config, "key = 1\n").expect("write extra config");

        let mut watcher = SimpleConfigWatcher::new(dir.path().to_path_buf());
        watcher.add_watch_path(extra_config.clone());
        watcher.set_debounce_duration(0);
        open_check_window(&mut watcher);

        assert!(!watcher.should_reload(), "baseline poll records mtimes, sees no change");

        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&extra_config, "key = 2\n").expect("rewrite extra config");
        open_check_window(&mut watcher);

        assert!(watcher.should_reload(), "modified additional config must trigger reload");
    }

    #[test]
    fn detects_creation_of_missing_additional_watch_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let extra_config = dir.path().join("created-user-config.toml");

        let mut watcher = SimpleConfigWatcher::new(dir.path().to_path_buf());
        watcher.add_watch_path(extra_config.clone());
        watcher.set_debounce_duration(0);
        open_check_window(&mut watcher);

        assert!(!watcher.should_reload(), "missing path establishes a baseline");

        std::fs::write(&extra_config, "key = 1\n").expect("create extra config");
        open_check_window(&mut watcher);

        assert!(watcher.should_reload(), "creating a watched config must trigger reload");
    }

    #[test]
    fn detects_change_before_first_poll_after_baseline_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let extra_config = dir.path().join("user-vtcode.toml");
        std::fs::write(&extra_config, "mode = \"auto\"\n").expect("write config");

        let mut watcher = SimpleConfigWatcher::new(dir.path().to_path_buf());
        watcher.add_watch_path(extra_config.clone());
        watcher.seed_current_mtimes();
        watcher.set_debounce_duration(0);
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&extra_config, "mode = \"command\"\n").expect("modify config");
        open_check_window(&mut watcher);

        assert!(watcher.should_reload(), "a change after baseline seeding must trigger reload");
    }

    #[test]
    fn does_not_consume_a_change_during_debounce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("vtcode.toml");
        std::fs::write(&config_path, "mode = \"auto\"\n").expect("write config");

        let mut watcher = SimpleConfigWatcher::new(dir.path().to_path_buf());
        watcher.set_debounce_duration(0);
        open_check_window(&mut watcher);
        assert!(!watcher.should_reload(), "baseline poll records mtimes");

        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&config_path, "mode = \"hidden\"\n").expect("rewrite config");
        open_check_window(&mut watcher);
        assert!(watcher.should_reload(), "first change is outside the debounce window");

        watcher.set_debounce_duration(60_000);
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&config_path, "mode = \"command\"\n").expect("rewrite config again");
        open_check_window(&mut watcher);
        assert!(!watcher.should_reload(), "second change is debounced");

        watcher.set_debounce_duration(0);
        open_check_window(&mut watcher);
        assert!(watcher.should_reload(), "debounced change remains observable");
    }
}
