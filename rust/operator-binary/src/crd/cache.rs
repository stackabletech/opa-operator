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
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entry_time_to_live: DEFAULT_CACHE_ENTRY_TIME_TO_LIVE,
        }
    }
}

const fn default_entry_time_to_live() -> Duration {
    DEFAULT_CACHE_ENTRY_TIME_TO_LIVE
}
