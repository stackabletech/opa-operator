//! Caching of the bearer tokens the info-fetcher backends authenticate with.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use tokio::sync::RwLock;

/// How long before its stated expiry a token stops being handed out.
///
/// A token is minted, then travels to the backend and is validated there, so handing out one that is
/// about to expire risks it being rejected mid-request. Refreshing slightly early avoids that without
/// needing to know anything about the backend's clock.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(30);

/// A freshly minted bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedToken {
    pub token: String,

    /// How long the token remains valid, as reported by whoever issued it (for OAuth: `expires_in`).
    ///
    /// [`None`] when the issuer does not say. The token is then used but not cached, because we have
    /// no basis for deciding when it goes stale.
    pub lifetime: Option<Duration>,
}

/// A bearer token that is minted on demand and kept until shortly before it expires.
///
/// Backends previously minted a token for every single request, which doubled the round trips per
/// lookup and threw the issuer's `expires_in` away. This caches the token for the lifetime the issuer
/// reported, and lets the caller drop it early via [`CachedToken::invalidate`] when the backend
/// rejects it. A token can stop working before its stated expiry, for example by being revoked.
#[derive(Debug, Default)]
pub struct CachedToken {
    cached: RwLock<Option<Entry>>,
}

#[derive(Debug)]
struct Entry {
    token: String,

    /// The stated expiry, already brought forward by [`EXPIRY_MARGIN`].
    usable_until: Instant,
}

impl CachedToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a usable token, minting one with `mint` if the cache has none or the cached one is
    /// within [`EXPIRY_MARGIN`] of expiring.
    ///
    /// Concurrent callers that all miss the cache do not each mint: the first one to get the write
    /// lock mints while the others wait, and they then find its result in the cache. Without that, a
    /// burst of requests arriving just after a token expired would produce a burst of token requests.
    pub async fn get<F, Fut, E>(&self, mint: F) -> Result<String, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<MintedToken, E>>,
    {
        if let Some(token) = Self::usable_token(&*self.cached.read().await) {
            return Ok(token);
        }

        // The read lock is released above, so another caller may have minted in between. Hence the
        // second look before minting ourselves.
        let mut cached = self.cached.write().await;
        if let Some(token) = Self::usable_token(&cached) {
            return Ok(token);
        }

        let MintedToken { token, lifetime } = mint().await?;

        // Only cache a token we know is still usable for a worthwhile amount of time. A failed mint
        // returns above, so nothing is cached in that case either.
        *cached = lifetime
            .and_then(|lifetime| lifetime.checked_sub(EXPIRY_MARGIN))
            .map(|usable_for| Entry {
                token: token.clone(),
                usable_until: Instant::now() + usable_for,
            });

        Ok(token)
    }

    /// Drops the cached token, so the next [`CachedToken::get`] mints a fresh one.
    ///
    /// Call this when the backend rejects the token, which is the only way to find out that it stopped
    /// being valid ahead of its stated expiry.
    pub async fn invalidate(&self) {
        *self.cached.write().await = None;
    }

    /// The cached token, if there is one and it is not within [`EXPIRY_MARGIN`] of expiring.
    fn usable_token(cached: &Option<Entry>) -> Option<String> {
        let entry = cached.as_ref()?;

        (entry.usable_until > Instant::now()).then(|| entry.token.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    /// Counts how often a token was minted, so the tests can assert on cache hits rather than on
    /// timing.
    struct Minter {
        calls: AtomicUsize,
        lifetime: Option<Duration>,
    }

    impl Minter {
        fn new(lifetime: Option<Duration>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                lifetime,
            }
        }

        async fn mint(&self) -> Result<MintedToken, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);

            Ok(MintedToken {
                token: format!("token-{call}"),
                lifetime: self.lifetime,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    /// A lifetime comfortably longer than [`EXPIRY_MARGIN`], so the token stays usable for the whole
    /// test without any waiting.
    fn long_lifetime() -> Option<Duration> {
        Some(EXPIRY_MARGIN + Duration::from_secs(600))
    }

    #[tokio::test]
    async fn token_is_minted_once_and_then_served_from_the_cache() {
        let minter = Minter::new(long_lifetime());
        let cached = CachedToken::new();

        let first = cached.get(|| minter.mint()).await.expect("minting works");
        let second = cached.get(|| minter.mint()).await.expect("minting works");

        assert_eq!(first, "token-0");
        assert_eq!(second, "token-0");
        assert_eq!(minter.calls(), 1);
    }

    #[tokio::test]
    async fn invalidating_forces_the_next_get_to_mint() {
        let minter = Minter::new(long_lifetime());
        let cached = CachedToken::new();

        let first = cached.get(|| minter.mint()).await.expect("minting works");
        cached.invalidate().await;
        let second = cached.get(|| minter.mint()).await.expect("minting works");

        assert_eq!(first, "token-0");
        assert_eq!(second, "token-1");
        assert_eq!(minter.calls(), 2);
    }

    /// Without a stated lifetime we have no idea how long the token is good for, so we must not keep
    /// it. That degrades to minting per request, which is what the backends did before caching.
    #[tokio::test]
    async fn a_token_without_a_lifetime_is_not_cached() {
        let minter = Minter::new(None);
        let cached = CachedToken::new();

        cached.get(|| minter.mint()).await.expect("minting works");
        cached.get(|| minter.mint()).await.expect("minting works");

        assert_eq!(minter.calls(), 2);
    }

    /// A token that expires within the safety margin has no usable lifetime left, so caching it would
    /// only hand out a token that is about to be rejected.
    #[tokio::test]
    async fn a_token_expiring_within_the_margin_is_not_cached() {
        let minter = Minter::new(Some(EXPIRY_MARGIN));
        let cached = CachedToken::new();

        cached.get(|| minter.mint()).await.expect("minting works");
        cached.get(|| minter.mint()).await.expect("minting works");

        assert_eq!(minter.calls(), 2);
    }

    /// Concurrent lookups that all miss the cache must not each mint a token: the backend would see a
    /// burst of token requests every time the cached one expires.
    #[tokio::test]
    async fn concurrent_gets_mint_only_once() {
        let minter = Minter::new(long_lifetime());
        let cached = CachedToken::new();

        let tokens = futures::future::join_all((0..20).map(|_| {
            cached.get(|| async {
                // Yield inside the critical section, so the tasks actually overlap.
                tokio::time::sleep(Duration::from_millis(20)).await;
                minter.mint().await
            })
        }))
        .await;

        for token in tokens {
            assert_eq!(token.expect("minting works"), "token-0");
        }
        assert_eq!(minter.calls(), 1);
    }

    /// A failed mint must not be cached, and must not poison a later attempt.
    #[tokio::test]
    async fn a_failed_mint_is_not_cached() {
        let minter = Minter::new(long_lifetime());
        let cached = CachedToken::new();

        let failed = cached
            .get(|| async { Err::<MintedToken, String>("no token for you".to_owned()) })
            .await;
        let succeeded = cached.get(|| minter.mint()).await;

        assert_eq!(failed, Err("no token for you".to_owned()));
        assert_eq!(succeeded, Ok("token-0".to_owned()));
    }
}
