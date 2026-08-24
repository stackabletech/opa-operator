pub mod active_directory;
pub mod entra;
pub mod keycloak;
pub mod openldap;
pub mod xfsc_aas;

#[cfg(test)]
mod test_mocks {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

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
