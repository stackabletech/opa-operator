pub mod active_directory;
pub mod entra;
pub mod keycloak;
pub mod openldap;
pub mod xfsc_aas;

#[cfg(test)]
mod test_mocks {
    use percent_encoding::percent_decode_str;
    use wiremock::{
        Mock, MockServer, Request, ResponseTemplate,
        matchers::{method, path},
    };

    /// User ids and names that address something other than the user when put into a URL as text:
    /// another host, another path or extra query parameters.
    pub(super) const HOSTILE_USERS: &[&str] = &[
        "https://evil.example.com/",
        "//evil.example.com",
        "../../master",
        "..%2F..%2Fmaster",
        "%2e%2e/%2e%2e/master",
        "..\\..\\master",
        "alice/groups",
        "alice?briefRepresentation=true",
        "alice&exact=false",
        "alice#fragment",
        "alice;x=1",
        "alice@example.com",
    ];

    /// The `GET` requests `mock_server` received, leaving out the token requests.
    pub(super) async fn received_lookups(mock_server: &MockServer) -> Vec<Request> {
        let requests = mock_server
            .received_requests()
            .await
            .expect("request recording is enabled by default");

        requests
            .into_iter()
            .filter(|request| request.method.as_str() == "GET")
            .collect()
    }

    /// Asserts that `mock_server` received a single lookup, addressed to exactly one path segment
    /// below `parent_path` that decodes to `user`.
    pub(super) async fn assert_looked_up_below(
        mock_server: &MockServer,
        parent_path: &str,
        user: &str,
    ) {
        let lookups = received_lookups(mock_server).await;
        let [lookup] = lookups.as_slice() else {
            panic!("{user:?} was not looked up exactly once: {lookups:?}");
        };

        let path = lookup.url.path();
        let segment = path
            .strip_prefix(parent_path)
            .unwrap_or_else(|| panic!("{user:?} was looked up at {path}, outside {parent_path}"));

        assert!(
            !segment.contains('/'),
            "{user:?} was looked up at {path}, more than one segment below {parent_path}"
        );
        assert_eq!(
            percent_decode_str(segment).decode_utf8_lossy(),
            user,
            "{user:?} was looked up at {path}"
        );
    }

    /// Mounts an OAuth2 `client_credentials` token endpoint at `token_path`, answering with a token
    /// that is valid for `expires_in` seconds. Without an `expires_in` the token is deliberately not
    /// cached, see [`info_fetcher_commons::utils::token::CachedToken`].
    ///
    /// `expected_calls` asserts how often the endpoint is hit; wiremock verifies it when the server
    /// is dropped. Pass [`None`] to place no expectation on it.
    pub(super) async fn mock_token(
        mock_server: &MockServer,
        token_path: String,
        expires_in: Option<u64>,
        expected_calls: Option<u64>,
    ) {
        let mut body = serde_json::json!({"access_token": "access-token"});
        if let Some(expires_in) = expires_in {
            body["expires_in"] = expires_in.into();
        }

        let mut mock = Mock::given(method("POST"))
            .and(path(token_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(body));

        if let Some(expected_calls) = expected_calls {
            mock = mock.expect(expected_calls);
        }

        mock.mount(mock_server).await;
    }
}
