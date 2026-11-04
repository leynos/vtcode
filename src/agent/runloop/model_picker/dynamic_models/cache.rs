use hashbrown::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use diapason_commons::DiapasonPaths;
use diapason_commons::fs::{read_private_file_no_follow, with_private_file_lock};
use diapason_core::config::models::Provider;
use diapason_core::utils::dot_config::get_dot_manager;
use serde::{Deserialize, Serialize};

use super::endpoints::default_provider_base;

const DYNAMIC_MODEL_CACHE_FILENAME: &str = "dynamic_local_models.json";
const DYNAMIC_MODEL_CACHE_TTL_SECS: u64 = 300;

type CacheEntries = HashMap<String, CachedDynamicModelEntry>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDynamicModelEntry {
    provider: String,
    base_url: String,
    fetched_at: u64,
    models: Vec<String>,
}

pub(super) struct CachedDynamicModelStore {
    entries: CacheEntries,
    dirty: bool,
}

impl CachedDynamicModelStore {
    pub(super) async fn load() -> Self {
        let Some(path) = dynamic_model_cache_path() else {
            return Self { entries: HashMap::new(), dirty: false };
        };

        // The XDG path policy changed in 1bffe07ee. The old implementation
        // stored this cache under the config directory's cache/models child,
        // while older installations used the legacy home directory. Read
        // both locations so a partial migration cannot discard a warm cache.
        // Current entries win when files contain the same provider/base-url key.
        let mut candidates = vec![(path.clone(), false)];
        if let Some(legacy_paths) = legacy_dynamic_model_cache_paths() {
            for legacy_path in legacy_paths {
                if !candidates.iter().any(|(candidate, _)| candidate == &legacy_path) {
                    candidates.push((legacy_path, true));
                }
            }
        }

        let (entries, needs_legacy_republish) = load_cache_entries(candidates).await;

        if !entries.is_empty() {
            if needs_legacy_republish {
                match serde_json::to_vec_pretty(&entries) {
                    Ok(serialized) => {
                        let lock_path = path.clone();
                        let destination = path.clone();
                        let publish = with_private_file_lock(&lock_path, move || {
                            DiapasonPaths::write_private_file_atomic_if_absent(&destination, &serialized).map(|_| ())
                        })
                        .await;
                        if let Err(error) = publish {
                            tracing::debug!(
                                path = %path.display(),
                                %error,
                                "Failed to republish legacy dynamic model cache"
                            );
                        }
                    }
                    Err(error) => tracing::debug!(%error, "Failed to serialize legacy dynamic model cache"),
                }
            }
            return Self { entries, dirty: false };
        }

        Self { entries: HashMap::new(), dirty: false }
    }

    pub(super) async fn persist(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let Some(path) = dynamic_model_cache_path() else {
            return Ok(());
        };

        let lock_path = path.clone();
        let entries = self.entries.clone();
        with_private_file_lock(&lock_path, move || {
            let mut merged = match DiapasonPaths::read_file_no_follow(&path) {
                Ok(data) => match serde_json::from_slice::<CacheEntries>(&data) {
                    Ok(entries) => entries,
                    Err(_) => HashMap::new(),
                },
                Err(error) if is_not_found(&error) => HashMap::new(),
                Err(error) => return Err(error),
            };
            merged.extend(entries);
            let serialized = serde_json::to_vec_pretty(&merged)?;
            DiapasonPaths::write_private_file_atomic(&path, &serialized)?;
            Ok(())
        })
        .await?;
        self.dirty = false;
        Ok(())
    }

    pub(super) fn for_provider(&self, provider: Provider) -> Self {
        let provider = provider.to_string();
        Self {
            entries: self
                .entries
                .iter()
                .filter(|(_, entry)| entry.provider == provider)
                .map(|(key, entry)| (key.clone(), entry.clone()))
                .collect(),
            dirty: self.dirty,
        }
    }

    pub(super) fn merge_provider(&mut self, provider: Provider, other: Self) {
        let provider = provider.to_string();
        for (key, entry) in other.entries {
            if entry.provider == provider {
                self.entries.insert(key, entry);
            }
        }
        self.dirty |= other.dirty;
    }

    pub(super) fn fresh_models(&self, provider: Provider, base_url: Option<&str>) -> Option<Vec<String>> {
        let (_, resolved_base) = normalize_base_url(provider, base_url);
        self.fresh_models_for_base(provider, &resolved_base)
    }

    pub(super) async fn fetch_with_cache<F, Fut>(
        &mut self,
        provider: Provider,
        mut base_url: Option<String>,
        fetch_fn: F,
    ) -> (Result<Vec<String>>, Option<String>)
    where
        F: Fn(Option<String>) -> Fut,
        Fut: Future<Output = Result<Vec<String>, anyhow::Error>>,
    {
        let (normalized_base_url, resolved_base) = normalize_base_url(provider, base_url.as_deref());
        base_url = normalized_base_url;
        let key = Self::cache_key(provider, &resolved_base);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

        if let Some(models) = self.fresh_models_for_base(provider, &resolved_base) {
            debug_assert!(self.entries.contains_key(&key));
            return (Ok(models), None);
        }

        match fetch_fn(base_url).await {
            Ok(models) => {
                self.entries.insert(
                    key,
                    CachedDynamicModelEntry {
                        provider: provider.to_string(),
                        base_url: resolved_base,
                        fetched_at: now,
                        models: models.clone(),
                    },
                );
                self.dirty = true;
                (Ok(models), None)
            }
            Err(err) => {
                if let Some(entry) = self.entries.get(&key) {
                    let warning = format!(
                        "Using cached {} models fetched {}s ago because {} was unreachable ({}).",
                        provider.label(),
                        now.saturating_sub(entry.fetched_at),
                        resolved_base,
                        err
                    );
                    return (Ok(entry.models.clone()), Some(warning));
                }
                (Err(err), None)
            }
        }
    }

    fn cache_key(provider: Provider, base_url: &str) -> String {
        format!("{provider}::{base_url}")
    }

    fn fresh_models_for_base(&self, provider: Provider, resolved_base: &str) -> Option<Vec<String>> {
        let key = Self::cache_key(provider, resolved_base);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        self.entries.get(&key).and_then(|entry| {
            (now.saturating_sub(entry.fetched_at) <= DYNAMIC_MODEL_CACHE_TTL_SECS).then(|| entry.models.clone())
        })
    }
}

fn normalize_base_url(provider: Provider, base_url: Option<&str>) -> (Option<String>, String) {
    let base_url = base_url
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty());
    let resolved_base = base_url
        .as_deref()
        .unwrap_or_else(|| default_provider_base(provider))
        .to_string();
    (base_url, resolved_base)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
    })
}

fn dynamic_model_cache_path() -> Option<PathBuf> {
    let manager = get_dot_manager().ok()?.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Some(manager.cache_dir("models").join(DYNAMIC_MODEL_CACHE_FILENAME))
}

fn legacy_dynamic_model_cache_paths() -> Option<Vec<PathBuf>> {
    let paths = DiapasonPaths::resolve().ok()?;
    let mut candidates = vec![
        paths.cache_dir().join("models").join(DYNAMIC_MODEL_CACHE_FILENAME),
        paths.config_dir().join("cache/models").join(DYNAMIC_MODEL_CACHE_FILENAME),
    ];
    candidates.dedup();
    Some(candidates)
}

async fn load_cache_entries(candidates: impl IntoIterator<Item = (PathBuf, bool)>) -> (CacheEntries, bool) {
    let mut entries = HashMap::new();
    let mut needs_legacy_republish = false;

    for (candidate, is_legacy) in candidates {
        let Ok(data) = read_private_file_no_follow(&candidate).await else {
            continue;
        };
        let Ok(candidate_entries) = serde_json::from_slice::<CacheEntries>(&data) else {
            continue;
        };

        for (key, entry) in candidate_entries {
            if let hashbrown::hash_map::Entry::Vacant(slot) = entries.entry(key) {
                slot.insert(entry);
                if is_legacy {
                    needs_legacy_republish = true;
                }
            }
        }
    }

    (entries, needs_legacy_republish)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::fs;

    fn cache_entry(
        provider: Provider,
        base_url: &str,
        model: &str,
        fetched_at: u64,
    ) -> (String, CachedDynamicModelEntry) {
        (
            CachedDynamicModelStore::cache_key(provider, base_url),
            CachedDynamicModelEntry {
                provider: provider.to_string(),
                base_url: base_url.to_string(),
                fetched_at,
                models: vec![model.to_string()],
            },
        )
    }

    #[tokio::test]
    async fn fresh_cache_skips_fetch() {
        let mut store = CachedDynamicModelStore {
            entries: [(
                CachedDynamicModelStore::cache_key(Provider::Ollama, default_provider_base(Provider::Ollama)),
                CachedDynamicModelEntry {
                    provider: Provider::Ollama.to_string(),
                    base_url: default_provider_base(Provider::Ollama).to_string(),
                    fetched_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                    models: vec!["cached-model".to_string()],
                },
            )]
            .into_iter()
            .collect(),
            dirty: false,
        };
        let called = Arc::new(AtomicBool::new(false));
        let called_by_fetch = Arc::clone(&called);

        let (result, warning) = store
            .fetch_with_cache(Provider::Ollama, None, move |_| {
                called_by_fetch.store(true, Ordering::Relaxed);
                async { Ok(vec!["network-model".to_string()]) }
            })
            .await;

        assert_eq!(result.unwrap(), vec!["cached-model"]);
        assert!(warning.is_none());
        assert!(!called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn legacy_cache_entries_are_loaded_without_overwriting_current_entries() {
        let temp_dir = tempfile::tempdir().unwrap();
        let current_path = temp_dir.path().join("current.json");
        let legacy_path = temp_dir.path().join("legacy.json");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let current_key = CachedDynamicModelStore::cache_key(Provider::Ollama, "http://localhost:11434");
        let legacy_only_key = CachedDynamicModelStore::cache_key(Provider::LlamaCpp, "http://localhost:8080/v1");

        let current_entries = [cache_entry(
            Provider::Ollama,
            "http://localhost:11434",
            "current-model",
            now,
        )]
        .into_iter()
        .collect::<CacheEntries>();
        let legacy_entries = [
            cache_entry(Provider::Ollama, "http://localhost:11434", "legacy-model", now),
            cache_entry(Provider::LlamaCpp, "http://localhost:8080/v1", "legacy-only-model", now),
        ]
        .into_iter()
        .collect::<CacheEntries>();

        fs::write(&current_path, serde_json::to_vec(&current_entries).unwrap())
            .await
            .unwrap();
        fs::write(&legacy_path, serde_json::to_vec(&legacy_entries).unwrap())
            .await
            .unwrap();

        let (entries, needs_legacy_republish) = load_cache_entries([(current_path, false), (legacy_path, true)]).await;

        assert!(needs_legacy_republish);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[&current_key].models, vec!["current-model"]);
        assert_eq!(entries[&legacy_only_key].models, vec!["legacy-only-model"]);
    }

    #[tokio::test]
    async fn legacy_cache_with_no_new_entries_does_not_need_republish() {
        let temp_dir = tempfile::tempdir().unwrap();
        let current_path = temp_dir.path().join("current.json");
        let legacy_path = temp_dir.path().join("legacy.json");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let entries = [cache_entry(
            Provider::Ollama,
            "http://localhost:11434",
            "cached-model",
            now,
        )]
        .into_iter()
        .collect::<CacheEntries>();

        fs::write(&current_path, serde_json::to_vec(&entries).unwrap()).await.unwrap();
        fs::write(&legacy_path, serde_json::to_vec(&entries).unwrap()).await.unwrap();

        let (loaded, needs_legacy_republish) = load_cache_entries([(current_path, false), (legacy_path, true)]).await;

        assert_eq!(loaded.len(), 1);
        assert!(!needs_legacy_republish);
    }

    #[tokio::test]
    async fn provider_cache_views_merge_without_overwriting_other_providers() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let ollama_key = CachedDynamicModelStore::cache_key(Provider::Ollama, "http://localhost:11434");
        let llamacpp_key = CachedDynamicModelStore::cache_key(Provider::LlamaCpp, "http://localhost:8080/v1");
        let entries = [
            cache_entry(
                Provider::Ollama,
                "http://localhost:11434",
                "old-ollama",
                now.saturating_sub(DYNAMIC_MODEL_CACHE_TTL_SECS + 1),
            ),
            cache_entry(Provider::LlamaCpp, "http://localhost:8080/v1", "llamacpp", now),
        ]
        .into_iter()
        .collect();
        let mut store = CachedDynamicModelStore { entries, dirty: false };
        let mut ollama_store = store.for_provider(Provider::Ollama);

        let (result, _) = ollama_store
            .fetch_with_cache(Provider::Ollama, None, |_| async { Ok(vec!["new-ollama".to_string()]) })
            .await;

        assert_eq!(result.unwrap(), vec!["new-ollama"]);
        store.merge_provider(Provider::Ollama, ollama_store);
        assert_eq!(store.entries[&ollama_key].models, vec!["new-ollama"]);
        assert_eq!(store.entries[&llamacpp_key].models, vec!["llamacpp"]);
    }

    #[test]
    fn provider_cache_view_preserves_pending_network_persist() {
        let store = CachedDynamicModelStore { entries: HashMap::new(), dirty: true };

        assert!(store.for_provider(Provider::Ollama).dirty);
    }
}
