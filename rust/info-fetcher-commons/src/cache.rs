//! The response cache shared by the info-fetchers, including the short-lived caching of failed
//! lookups.

use std::{hash::Hash, sync::Arc, time::Duration};

use moka::{Expiry, future::Cache};
use stackable_opa_operator::crd::cache;

/// How long a failed lookup is remembered.
///
/// Without this, a lookup that keeps failing queries the backend again on every single request. That
/// is reachable by anyone who can name a user or a resource, and it is at its worst exactly when the
/// backend is already in trouble.
///
/// Deliberately far shorter than the configured time-to-live for successful lookups: a stale success
/// is merely out of date, while a stale failure keeps denying (or, for a rule keyed on the absence of
/// a group or a tag, keeps granting) after the backend has recovered. If the configured
/// time-to-live is shorter than this, it wins, because moka evicts at the earliest of the two.
const FAILURE_TIME_TO_LIVE: Duration = Duration::from_secs(5);

/// What a lookup produced, as held in the cache.
///
/// Failures are cached as well as successes, so the variants share one key space and one capacity
/// limit, and so that moka coalesces concurrent lookups of a failing key just as it does a
/// successful one.
pub enum CachedResponse<T, E> {
    /// The information to answer with.
    Found(T),

    /// The lookup failed. Held in an [`Arc`] because every caller that hits this entry is handed the
    /// same error.
    Failed(Arc<E>),
}

/// Written out rather than derived, as the derive would demand `E: Clone` even though the error is
/// only ever cloned through its [`Arc`].
impl<T: Clone, E> Clone for CachedResponse<T, E> {
    fn clone(&self) -> Self {
        match self {
            Self::Found(response) => Self::Found(response.clone()),
            Self::Failed(error) => Self::Failed(error.clone()),
        }
    }
}

/// A cache of what the backend answered for a request, keyed by the request.
pub type ResponseCache<K, T, E> = Cache<K, CachedResponse<T, E>>;

/// Builds the response cache from the cluster's cache configuration. `name` only labels the cache in
/// moka's own metrics.
pub fn build<K, T, E>(name: &str, config: &cache::Cache) -> ResponseCache<K, T, E>
where
    K: Hash + Eq + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    build_with_failure_time_to_live(name, config, FAILURE_TIME_TO_LIVE)
}

fn build_with_failure_time_to_live<K, T, E>(
    name: &str,
    config: &cache::Cache,
    failure_time_to_live: Duration,
) -> ResponseCache<K, T, E>
where
    K: Hash + Eq + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    config
        .apply_settings_to_cache_builder(Cache::builder().name(name))
        .expire_after(FailureExpiry {
            failure_time_to_live,
        })
        .build()
}

/// Expires [`CachedResponse::Failed`] entries early, leaving successful ones to the configured
/// time-to-live.
struct FailureExpiry {
    failure_time_to_live: Duration,
}

impl FailureExpiry {
    /// [`None`] leaves the entry to the cache's `time_to_live`, which is what successful lookups get.
    fn time_to_live_for<T, E>(&self, response: &CachedResponse<T, E>) -> Option<Duration> {
        match response {
            CachedResponse::Found(_) => None,
            CachedResponse::Failed(_) => Some(self.failure_time_to_live),
        }
    }
}

impl<K, T, E> Expiry<K, CachedResponse<T, E>> for FailureExpiry {
    fn expire_after_create(
        &self,
        _key: &K,
        response: &CachedResponse<T, E>,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        self.time_to_live_for(response)
    }

    fn expire_after_update(
        &self,
        _key: &K,
        response: &CachedResponse<T, E>,
        _updated_at: std::time::Instant,
        _duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        self.time_to_live_for(response)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use snafu::Snafu;

    use super::*;

    /// Stands in for a backend's error type, of which the cache only needs that it exists.
    #[derive(Debug, Snafu)]
    #[snafu(display("the lookup failed"))]
    struct LookupFailed;

    type TestCache = ResponseCache<String, String, LookupFailed>;

    /// A cache whose failed entries live for `failure_time_to_live`, and whose successful ones live
    /// for a time-to-live long enough not to interfere with any test.
    fn cache(failure_time_to_live: Duration) -> TestCache {
        let config = serde_json::from_value(json!({"entryTimeToLive": "10m"}))
            .expect("the cache config must be valid");

        build_with_failure_time_to_live("test", &config, failure_time_to_live)
    }

    fn failure() -> CachedResponse<String, LookupFailed> {
        CachedResponse::Failed(Arc::new(LookupFailed))
    }

    /// Counts how often the cache had to load a value, so the tests assert on cache hits rather than
    /// on timing.
    #[derive(Default)]
    struct Loads(AtomicUsize);

    impl Loads {
        async fn load(
            &self,
            response: CachedResponse<String, LookupFailed>,
        ) -> CachedResponse<String, LookupFailed> {
            self.0.fetch_add(1, Ordering::SeqCst);
            response
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// The point of the whole module: a failing lookup must not reach the backend again on the next
    /// request.
    #[tokio::test]
    async fn a_failed_lookup_is_served_from_the_cache() {
        let cache = cache(Duration::from_secs(60));
        let loads = Loads::default();
        let request = "broken".to_owned();

        for _ in 0..3 {
            cache.get_with_by_ref(&request, loads.load(failure())).await;
        }

        assert_eq!(loads.count(), 1);
    }

    /// A failure must not be held for the full time-to-live of a successful lookup: the backend may
    /// have recovered in the meantime, and until the entry goes away every request keeps failing.
    #[tokio::test]
    async fn a_failed_lookup_is_forgotten_again_quickly() {
        let cache = cache(Duration::from_millis(50));
        let loads = Loads::default();
        let request = "broken".to_owned();

        cache.get_with_by_ref(&request, loads.load(failure())).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        cache.run_pending_tasks().await;
        cache.get_with_by_ref(&request, loads.load(failure())).await;

        assert_eq!(loads.count(), 2);
    }

    /// The short expiry must apply to failures only. A successful lookup keeps the configured
    /// time-to-live, which the test's cache sets far beyond the failure one.
    #[tokio::test]
    async fn a_successful_lookup_keeps_the_configured_time_to_live() {
        let cache = cache(Duration::from_millis(50));
        let loads = Loads::default();
        let request = "known".to_owned();
        let found = CachedResponse::Found("the answer".to_owned());

        cache
            .get_with_by_ref(&request, loads.load(found.clone()))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        cache.run_pending_tasks().await;
        cache.get_with_by_ref(&request, loads.load(found)).await;

        assert_eq!(loads.count(), 1);
    }
}
