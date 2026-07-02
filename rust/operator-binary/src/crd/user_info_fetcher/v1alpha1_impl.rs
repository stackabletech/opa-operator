use stackable_operator::shared::time::Duration;

use crate::crd::user_info_fetcher::{DEFAULT_CACHE_ENTRY_TIME_TO_LIVE, v1alpha1};

// TODO (@Techassi): Most of these impls are the exact same across v1alpha1 and v1alpha2. Explore
// and design a more elegant solution for it.
impl Default for v1alpha1::Backend {
    fn default() -> Self {
        Self::None {}
    }
}

impl Default for v1alpha1::Cache {
    fn default() -> Self {
        Self {
            entry_time_to_live: Self::default_entry_time_to_live(),
        }
    }
}

impl v1alpha1::Cache {
    pub const fn default_entry_time_to_live() -> Duration {
        DEFAULT_CACHE_ENTRY_TIME_TO_LIVE
    }
}
