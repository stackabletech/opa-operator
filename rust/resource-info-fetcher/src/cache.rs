//! The response cache, including the short-lived caching of failed lookups.

use std::{sync::Arc, time::Duration};

use moka::{Expiry, future::Cache};
use stackable_opa_operator::crd::cache;

use crate::api::{GetResourceInfoError, ResourceInfoRequest};

/// How long a failed lookup is remembered.
///
/// Without this, a lookup that keeps failing queries the backend again on every single request. That
/// is reachable by anyone who can name a resource, and it is at its worst exactly when the backend is
/// already in trouble.
///
/// Deliberately far shorter than the configured time-to-live for successful lookups: a stale success
/// is merely out of date, while a stale failure keeps denying (or, for a rule keyed on the absence of
/// a tag, keeps granting) after the backend has recovered. If the configured time-to-live is shorter
/// than this, it wins, because moka evicts at the earliest of the two.
const FAILURE_TIME_TO_LIVE: Duration = Duration::from_secs(5);

/// What a lookup produced, as held in the cache.
///
/// Failures are cached as well as successes, so the variants share one key space and one capacity
/// limit, and so that moka coalesces concurrent lookups of a failing key just as it does a
/// successful one.
#[derive(Clone)]
pub enum CachedResponse {
    /// The metadata to answer with, already serialized to the JSON we return.
    Found(serde_json::Value),

    /// The lookup failed. Held in an [`Arc`] because every caller that hits this entry is handed the
    /// same error.
    Failed(Arc<GetResourceInfoError>),
}

pub type ResourceInfoCache = Cache<ResourceInfoRequest, CachedResponse>;

/// Builds the response cache from the cluster's cache configuration.
pub fn build(config: &cache::Cache) -> ResourceInfoCache {
    build_with_failure_time_to_live(config, FAILURE_TIME_TO_LIVE)
}

fn build_with_failure_time_to_live(
    config: &cache::Cache,
    failure_time_to_live: Duration,
) -> ResourceInfoCache {
    config
        .apply_settings_to_cache_builder(Cache::builder().name("resource-info"))
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
    fn time_to_live_for(&self, response: &CachedResponse) -> Option<Duration> {
        match response {
            CachedResponse::Found(_) => None,
            CachedResponse::Failed(_) => Some(self.failure_time_to_live),
        }
    }
}

impl Expiry<ResourceInfoRequest, CachedResponse> for FailureExpiry {
    fn expire_after_create(
        &self,
        _key: &ResourceInfoRequest,
        response: &CachedResponse,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        self.time_to_live_for(response)
    }

    fn expire_after_update(
        &self,
        _key: &ResourceInfoRequest,
        response: &CachedResponse,
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

    use super::*;
    use crate::api::RawIdentifier;

    /// A cache whose failed entries live for `failure_time_to_live`, and whose successful ones live
    /// for a time-to-live long enough not to interfere with any test.
    fn cache(failure_time_to_live: Duration) -> ResourceInfoCache {
        let config = serde_json::from_value(json!({"entryTimeToLive": "10m"}))
            .expect("the cache config must be valid");

        build_with_failure_time_to_live(&config, failure_time_to_live)
    }

    fn request(identifier: &str) -> ResourceInfoRequest {
        ResourceInfoRequest::RawIdentifier(RawIdentifier {
            identifier: identifier.into(),
        })
    }

    fn failure() -> CachedResponse {
        let error = GetResourceInfoError::SerializeResponseAsJson {
            source: serde_json::from_str::<serde_json::Value>("not json")
                .expect_err("the input is not valid JSON"),
        };

        CachedResponse::Failed(Arc::new(error))
    }

    /// Counts how often the cache had to load a value, so the tests assert on cache hits rather than
    /// on timing.
    #[derive(Default)]
    struct Loads(AtomicUsize);

    impl Loads {
        async fn load(&self, response: CachedResponse) -> CachedResponse {
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
        let request = request("urn:li:dataset:(urn:li:dataPlatform:trino,broken,PROD)");

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
        let request = request("urn:li:dataset:(urn:li:dataPlatform:trino,broken,PROD)");

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
        let request = request("urn:li:container:fb46bf1f985e130eeceeee8a51317cd9");
        let found = CachedResponse::Found(json!({"tags": []}));

        cache
            .get_with_by_ref(&request, loads.load(found.clone()))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        cache.run_pending_tasks().await;
        cache.get_with_by_ref(&request, loads.load(found)).await;

        assert_eq!(loads.count(), 1);
    }
}
