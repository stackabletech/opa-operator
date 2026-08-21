use moka::future::CacheBuilder;
use serde::{Deserialize, Serialize};
use stackable_operator::{
    schemars::{self, JsonSchema},
    shared::time::Duration,
};

/// Default time-to-live for cached responses.
pub(crate) const DEFAULT_CACHE_ENTRY_TIME_TO_LIVE: Duration = Duration::from_minutes_unchecked(1);

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    /// How long responses should be cached for.
    #[serde(default = "default_entry_time_to_live")]
    pub entry_time_to_live: Duration,

    /// Maximum number of entries to cache.
    ///
    /// By default there is no limit.
    #[serde(default)]
    pub max_entries: Option<u64>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entry_time_to_live: DEFAULT_CACHE_ENTRY_TIME_TO_LIVE,
            max_entries: None,
        }
    }
}

impl Cache {
    pub fn apply_settings_to_cache_builder<K, V, R>(
        &self,
        mut cache_builder: CacheBuilder<K, V, R>,
    ) -> CacheBuilder<K, V, R> {
        cache_builder = cache_builder.time_to_live(*self.entry_time_to_live);

        if let Some(max_entries) = self.max_entries {
            cache_builder = cache_builder.max_capacity(max_entries);
        }

        cache_builder
    }
}

const fn default_entry_time_to_live() -> Duration {
    DEFAULT_CACHE_ENTRY_TIME_TO_LIVE
}
