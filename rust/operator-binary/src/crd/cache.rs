use moka::future::CacheBuilder;
use serde::{Deserialize, Serialize};
use stackable_operator::{
    schemars::{self, JsonSchema},
    shared::time::Duration,
};

/// Default time-to-live for cached responses.
pub(crate) const DEFAULT_CACHE_ENTRY_TIME_TO_LIVE: Duration = Duration::from_minutes_unchecked(1);

/// Default upper bound on the number of cached responses.
///
/// The cache lives in an info-fetcher sidecar, which runs with a 128Mi memory limit, so it must not
/// be allowed to grow without bound: cache keys are built from caller-supplied parameters, and any
/// caller who can name a resource can mint a distinct key per request. This bound keeps the worst
/// case well inside the limit while being far above the number of distinct users and resources a
/// single OPA is realistically asked about within one [`DEFAULT_CACHE_ENTRY_TIME_TO_LIVE`].
pub(crate) const DEFAULT_CACHE_MAX_ENTRIES: u64 = 10_000;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    /// How long responses should be cached for.
    #[serde(default = "default_entry_time_to_live")]
    pub entry_time_to_live: Duration,

    /// Maximum number of entries to cache.
    ///
    /// Once it is reached, the least recently used entries are evicted. Raising this raises the
    /// memory the info-fetcher sidecar can use, which is capped by its memory limit.
    #[serde(default = "default_max_entries")]
    pub max_entries: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entry_time_to_live: DEFAULT_CACHE_ENTRY_TIME_TO_LIVE,
            max_entries: DEFAULT_CACHE_MAX_ENTRIES,
        }
    }
}

impl Cache {
    pub fn apply_settings_to_cache_builder<K, V, R>(
        &self,
        cache_builder: CacheBuilder<K, V, R>,
    ) -> CacheBuilder<K, V, R> {
        cache_builder
            .time_to_live(*self.entry_time_to_live)
            .max_capacity(self.max_entries)
    }
}

const fn default_entry_time_to_live() -> Duration {
    DEFAULT_CACHE_ENTRY_TIME_TO_LIVE
}

const fn default_max_entries() -> u64 {
    DEFAULT_CACHE_MAX_ENTRIES
}
