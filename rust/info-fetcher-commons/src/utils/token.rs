//! Caching of the bearer tokens the info-fetcher backends authenticate with.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use serde::Deserialize;
use tokio::sync::RwLock;

use crate::utils::{http, secret::Secret};

/// How long before its stated expiry a token stops being handed out.
///
/// A token is minted, then travels to the backend and is validated there, so handing out one that is
/// about to expire risks it being rejected mid-request. Refreshing slightly early avoids that without
/// needing to know anything about the backend's clock.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(30);

/// A freshly minted bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedToken {
    pub token: Secret,

    /// How long the token remains valid, as reported by whoever issued it (for OAuth: `expires_in`).
    ///
    /// [`None`] when the issuer does not say. The token is then used but not cached, because we have
    /// no basis for deciding when it goes stale.
    pub lifetime: Option<Duration>,
}

/// The parts of an OAuth2 token endpoint's answer to a `client_credentials` grant that we use.
///
/// Both the Keycloak and the Entra backend authenticate this way, so they read the same response.
#[derive(Debug, Deserialize)]
pub struct OAuthResponse {
    access_token: Secret,

    /// How many seconds the token stays valid, which lets us cache it rather than mint one per
    /// lookup. Treated as optional so a response without it degrades to minting per lookup.
    expires_in: Option<u64>,
}

impl From<OAuthResponse> for MintedToken {
    fn from(response: OAuthResponse) -> Self {
        Self {
            token: response.access_token,
            lifetime: response.expires_in.map(Duration::from_secs),
        }
    }
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
    token: Secret,

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
    pub async fn get<F, Fut, E>(&self, mint: F) -> Result<Secret, E>
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

    /// Runs `use_token` with a token from the cache, retrying exactly once with a freshly minted one
    /// if the backend rejects it with `401 Unauthorized`.
    ///
    /// A token that was accepted when it was minted can stop being valid ahead of its stated expiry:
    /// it was revoked, or the issuer's and our clock disagree. Being rejected is the only way we find
    /// that out, so the rejection is what drives the re-mint. `issuer` only names who did the
    /// rejecting in the log message.
    pub async fn get_with_retry<M, MFut, U, UFut, T, E>(
        &self,
        issuer: &str,
        mint: M,
        use_token: U,
    ) -> Result<T, E>
    where
        M: Fn() -> MFut,
        MFut: Future<Output = Result<MintedToken, E>>,
        U: Fn(Secret) -> UFut,
        UFut: Future<Output = Result<T, E>>,
        E: std::error::Error + 'static,
    {
        let token = self.get(&mint).await?;

        match use_token(token).await {
            Err(error) if http::is_unauthorized(&error) => {
                tracing::warn!(
                    error = &error as &dyn std::error::Error,
                    issuer,
                    "the issuer rejected the cached access token; re-authenticating and retrying once"
                );
                self.invalidate().await;

                let token = self.get(&mint).await?;
                use_token(token).await
            }
            result => result,
        }
    }

    /// Drops the cached token, so the next [`CachedToken::get`] mints a fresh one.
    ///
    /// Call this when the backend rejects the token, which is the only way to find out that it stopped
    /// being valid ahead of its stated expiry.
    pub async fn invalidate(&self) {
        *self.cached.write().await = None;
    }

    /// The cached token, if there is one and it is not within [`EXPIRY_MARGIN`] of expiring.
    fn usable_token(cached: &Option<Entry>) -> Option<Secret> {
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
                token: format!("token-{call}").into(),
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

        assert_eq!(first.expose(), "token-0");
        assert_eq!(second.expose(), "token-0");
        assert_eq!(minter.calls(), 1);
    }

    #[tokio::test]
    async fn invalidating_forces_the_next_get_to_mint() {
        let minter = Minter::new(long_lifetime());
        let cached = CachedToken::new();

        let first = cached.get(|| minter.mint()).await.expect("minting works");
        cached.invalidate().await;
        let second = cached.get(|| minter.mint()).await.expect("minting works");

        assert_eq!(first.expose(), "token-0");
        assert_eq!(second.expose(), "token-1");
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
            assert_eq!(token.expect("minting works").expose(), "token-0");
        }
        assert_eq!(minter.calls(), 1);
    }

    /// The error a backend reports when the identity provider rejected the token it was given, wrapped
    /// the way a backend wraps it.
    #[derive(Debug, snafu::Snafu)]
    #[snafu(display("the lookup failed"))]
    struct LookupFailed {
        source: http::Error,
    }

    /// Mints a distinct, long-lived token per call, counting the calls in `mints`. Unlike
    /// [`Minter`] it fails with the error type of the lookup, which [`CachedToken::get_with_retry`]
    /// requires the two to share.
    async fn mint_for_lookup(mints: &AtomicUsize) -> Result<MintedToken, LookupFailed> {
        let call = mints.fetch_add(1, Ordering::SeqCst);

        Ok(MintedToken {
            token: format!("token-{call}").into(),
            lifetime: long_lifetime(),
        })
    }

    fn rejected_with(status: hyper::StatusCode) -> LookupFailed {
        LookupFailed {
            source: http::Error::HttpErrorResponse {
                status,
                url: "https://idp.example.com/users/alice".to_owned(),
                text: "denied".to_owned(),
            },
        }
    }

    /// A token can be revoked before its stated expiry, and the rejection is the only way we find out.
    /// The lookup must then be retried once with a freshly minted token.
    #[tokio::test]
    async fn a_rejected_token_is_reminted_and_the_call_retried_once() {
        let cached = CachedToken::new();
        let mints = AtomicUsize::new(0);
        let attempts = AtomicUsize::new(0);

        let token = cached
            .get_with_retry(
                "the issuer",
                || mint_for_lookup(&mints),
                |token| async {
                    // Only the first attempt, made with `token-0`, is rejected.
                    match attempts.fetch_add(1, Ordering::SeqCst) {
                        0 => Err(rejected_with(hyper::StatusCode::UNAUTHORIZED)),
                        _ => Ok(token),
                    }
                },
            )
            .await
            .expect("the retry with the re-minted token succeeds");

        assert_eq!(token.expose(), "token-1");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(mints.load(Ordering::SeqCst), 2);
    }

    /// Any other failure says nothing about the token, so re-minting it would only cost a round trip.
    #[tokio::test]
    async fn other_failures_are_not_retried() {
        let cached = CachedToken::new();
        let mints = AtomicUsize::new(0);
        let attempts = AtomicUsize::new(0);

        let error = cached
            .get_with_retry(
                "the issuer",
                || mint_for_lookup(&mints),
                |_token| async {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err::<Secret, _>(rejected_with(hyper::StatusCode::FORBIDDEN))
                },
            )
            .await
            .expect_err("a 403 is not a rejected token");

        assert_eq!(error.to_string(), "the lookup failed");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(mints.load(Ordering::SeqCst), 1);
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

        assert_eq!(
            failed.map(|token| token.expose().to_owned()),
            Err("no token for you".to_owned())
        );
        assert_eq!(succeeded.expect("minting works").expose(), "token-0");
    }
}
