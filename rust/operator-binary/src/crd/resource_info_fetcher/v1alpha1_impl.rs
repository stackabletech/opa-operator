use stackable_operator::shared::time::Duration;

use crate::crd::resource_info_fetcher::v1alpha1;

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
        Duration::from_minutes_unchecked(1)
    }
}
