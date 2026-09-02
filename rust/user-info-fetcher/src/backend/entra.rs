use std::{collections::HashMap, path::Path};

use hyper::StatusCode;
use info_fetcher_commons::utils::{
    self,
    credentials::FileCredential,
    http::{is_unauthorized, send_json_request},
    secret::Secret,
    token::{CachedToken, MintedToken, OAuthResponse},
};
use serde::Deserialize;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha2;
use stackable_operator::commons::{networking::HostName, tls_verification::TlsClientDetails};
use url::Url;

use crate::{UserInfo, UserInfoRequest, http_error};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: utils::http::Error },

    #[snafu(display("failed to search for user with username {username:?}"))]
    SearchForUser {
        source: utils::http::Error,
        username: String,
    },

    #[snafu(display("unable to find user with id {user_id:?}"))]
    UserNotFoundById { user_id: String },

    #[snafu(display("failed to search for user with id {user_id:?}"))]
    SearchForUserById {
        source: utils::http::Error,
        user_id: String,
    },

    #[snafu(display("unable to find user with username {username:?}"))]
    UserNotFoundByName { username: String },

    #[snafu(display(
        "failed to request groups for user with username {username:?} (user_id: {user_id:?})"
    ))]
    RequestUserGroups {
        source: utils::http::Error,
        username: String,
        user_id: String,
    },

    #[snafu(display("failed to to build entra endpoint for {endpoint}"))]
    BuildEntraEndpoint {
        source: url::ParseError,
        endpoint: String,
    },

    #[snafu(display("failed to construct HTTP client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to read the client ID"))]
    ReadClientId { source: utils::credentials::Error },

    #[snafu(display("failed to read the client secret"))]
    ReadClientSecret { source: utils::credentials::Error },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::AccessToken { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundById { .. } => StatusCode::NOT_FOUND,
            Self::SearchForUserById { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundByName { .. } => StatusCode::NOT_FOUND,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::BuildEntraEndpoint { .. } => StatusCode::BAD_REQUEST,
            Self::ConstructHttpClient { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ConfigureTls { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ReadClientId { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ReadClientSecret { .. } => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserMetadata {
    id: String,
    user_principal_name: String,
    #[serde(default)]
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembershipResponse {
    value: Vec<GroupMembership>,

    /// Set by Graph when further pages of group memberships are available.
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    display_name: Option<String>,
}

/// Upper bound on the number of `@odata.nextLink` pages followed for a single group lookup.
///
/// At Graph's maximum page size of 999 this covers ~99,900 memberships, hopefully well beyond any realistic
/// account.
/// It exists purely as a final protection against an infinite loop for whatever reason.
const MAX_GROUP_PAGES: usize = 100;

/// Entra backend with resolved credentials.
///
/// This struct combines the CRD configuration with credentials loaded from the filesystem.
/// Credentials and the HTTP client are initialized once at startup and stored internally.
pub struct ResolvedEntraBackend {
    config: v1alpha2::EntraBackend,
    client_id: String,

    /// The client secret, re-read from its mounted file when Entra rejects it, so that rotating it
    /// in the Secret takes effect without restarting the Pod.
    client_secret: FileCredential,
    http_client: reqwest::Client,

    /// The OAuth2 access token, minted on demand and reused until it is about to expire.
    access_token: CachedToken,
}

impl ResolvedEntraBackend {
    /// Resolves an Entra backend by loading credentials from the filesystem.
    ///
    /// Reads `clientId` and `clientSecret` from the credentials directory and initializes
    /// the HTTP client with appropriate TLS configuration.
    pub async fn resolve(
        config: v1alpha2::EntraBackend,
        credentials_dir: &Path,
    ) -> Result<Self, Error> {
        let client_id = utils::credentials::read_credential_file(&credentials_dir.join("clientId"))
            .await
            .context(ReadClientIdSnafu)?;
        let client_secret = FileCredential::load(credentials_dir.join("clientSecret"))
            .await
            .context(ReadClientSecretSnafu)?;

        let mut client_builder = utils::http::client_builder();
        client_builder = utils::tls::configure_reqwest(
            &TlsClientDetails {
                tls: config.tls.clone(),
            },
            client_builder,
        )
        .await
        .context(ConfigureTlsSnafu)?;
        let http_client = client_builder.build().context(ConstructHttpClientSnafu)?;

        Ok(Self {
            config,
            client_id,
            client_secret,
            http_client,
            access_token: CachedToken::new(),
        })
    }

    pub(crate) async fn get_user_info(&self, req: &UserInfoRequest) -> Result<UserInfo, Error> {
        let v1alpha2::EntraBackend {
            client_credentials_secret: _,
            token_hostname,
            user_info_hostname,
            port,
            tenant_id,
            tls,
        } = &self.config;

        let entra_backend = EntraBackend::try_new(
            token_hostname,
            user_info_hostname,
            *port,
            tenant_id,
            TlsClientDetails { tls: tls.clone() }.uses_tls(),
        )?;

        self.access_token
            .get_with_retry(
                "Entra",
                || self.mint_access_token(&entra_backend),
                |access_token| self.get_user_info_with(req, &entra_backend, access_token),
            )
            .await
    }

    /// Mints an access token through the `client_credentials` grant of the tenant.
    ///
    /// A rejected client secret is re-read from disk and the mint retried once, so that rotating it
    /// in the Secret takes effect without restarting the Pod, see [`FileCredential`].
    async fn mint_access_token(&self, entra_backend: &EntraBackend) -> Result<MintedToken, Error> {
        self.client_secret
            .use_with_retry(
                "Entra",
                |error| is_unauthorized(error),
                |client_secret| self.mint_access_token_with(entra_backend, client_secret),
            )
            .await
    }

    /// The mint itself, made with an already-read `client_secret`.
    async fn mint_access_token_with(
        &self,
        entra_backend: &EntraBackend,
        client_secret: Secret,
    ) -> Result<MintedToken, Error> {
        let response = send_json_request::<OAuthResponse>(
            self.http_client.post(entra_backend.oauth2_token()).form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", client_secret.expose()),
                ("scope", "https://graph.microsoft.com/.default"),
                ("grant_type", "client_credentials"),
            ]),
        )
        .await
        .context(AccessTokenSnafu)?;

        Ok(response.into())
    }

    /// Looks the user up with an already-obtained `access_token`, so the caller can retry with a fresh
    /// one if this token turns out to be rejected.
    async fn get_user_info_with(
        &self,
        req: &UserInfoRequest,
        entra_backend: &EntraBackend,
        access_token: Secret,
    ) -> Result<UserInfo, Error> {
        let user_info = match req {
            UserInfoRequest::UserInfoRequestById(req) => {
                let user_id = req.id.clone();
                send_json_request::<UserMetadata>(
                    self.http_client
                        .get(entra_backend.user_info(&user_id))
                        .bearer_auth(access_token.expose()),
                )
                .await
                .map_err(|source| match source.status() {
                    // Graph answers a user it does not have with `404`. Any other failure (an
                    // outage, a rejected token, a body we cannot read) says nothing about whether
                    // the user exists, so reporting it as "not found" would tell a policy that a
                    // user is absent whenever Entra is merely unwell.
                    Some(StatusCode::NOT_FOUND) => Error::UserNotFoundById { user_id },
                    _ => Error::SearchForUserById { source, user_id },
                })?
            }
            UserInfoRequest::UserInfoRequestByName(req) => {
                let username = req.username.clone();
                send_json_request::<UserMetadata>(
                    self.http_client
                        .get(entra_backend.user_info(&username))
                        .bearer_auth(access_token.expose()),
                )
                .await
                .map_err(|source| match source.status() {
                    // The same split as above. Without it a username Entra does not have was
                    // reported as a failed lookup (`502`) rather than as a user that is not there.
                    Some(StatusCode::NOT_FOUND) => Error::UserNotFoundByName { username },
                    _ => Error::SearchForUser { source, username },
                })?
            }
        };

        let mut groups = Vec::new();
        let mut next_url = Some(entra_backend.group_info(&user_info.id));
        let mut pages_remaining = MAX_GROUP_PAGES;

        while let Some(url) = next_url {
            // Bound the loop so we can't run into an infinite loop for any reason
            if pages_remaining == 0 {
                tracing::warn!(
                    user.id = %user_info.id,
                    max_pages = MAX_GROUP_PAGES,
                    "reached the maximum number of Entra group membership pages; \
                     the resolved group list may be incomplete"
                );

                break;
            }
            pages_remaining -= 1;

            let response = send_json_request::<GroupMembershipResponse>(
                self.http_client.get(url).bearer_auth(access_token.expose()),
            )
            .await
            .with_context(|_| RequestUserGroupsSnafu {
                username: user_info.user_principal_name.clone(),
                user_id: user_info.id.clone(),
            })?;

            groups.extend(response.value.into_iter().filter_map(|g| g.display_name));

            next_url = response
                .next_link
                .map(|next_link| entra_backend.next_page(&next_link))
                .transpose()?;
        }

        Ok(UserInfo {
            id: Some(user_info.id),
            username: Some(user_info.user_principal_name),
            groups,
            custom_attributes: user_info.attributes,
        })
    }
}

struct EntraBackend {
    token_endpoint_url: Url,
    user_info_endpoint_url: Url,
}

impl EntraBackend {
    pub fn try_new(
        token_endpoint: &HostName,
        user_info_endpoint: &HostName,
        port: Option<u16>,
        tenant_id: &str,
        uses_tls: bool,
    ) -> Result<Self, Error> {
        let schema = if uses_tls { "https" } else { "http" };
        let port = port.unwrap_or(if uses_tls { 443 } else { 80 });

        let token_endpoint =
            format!("{schema}://{token_endpoint}:{port}/{tenant_id}/oauth2/v2.0/token");
        let token_endpoint_url = Url::parse(&token_endpoint).context(BuildEntraEndpointSnafu {
            endpoint: token_endpoint,
        })?;

        let user_info_endpoint = format!("{schema}://{user_info_endpoint}:{port}");
        let user_info_endpoint_url =
            Url::parse(&user_info_endpoint).context(BuildEntraEndpointSnafu {
                endpoint: user_info_endpoint,
            })?;

        Ok(Self {
            token_endpoint_url,
            user_info_endpoint_url,
        })
    }

    pub fn oauth2_token(&self) -> Url {
        self.token_endpoint_url.clone()
    }

    // Works both with id/oid and userPrincipalName
    pub fn user_info(&self, user: &str) -> Url {
        let mut user_info_url = self.user_info_endpoint_url.clone();
        user_info_url.set_path(&format!("/v1.0/users/{user}"));
        user_info_url
    }

    /// Requests the first page of the user's directory memberships.
    ///
    /// `memberOf` returns every directory object the user belongs to: groups as well as directory
    /// roles and administrative units.
    pub fn group_info(&self, user: &str) -> Url {
        let mut user_info_url = self.user_info_endpoint_url.clone();
        user_info_url.set_path(&format!("/v1.0/users/{user}/memberOf"));
        // 999 is the largest page size Graph accepts, and keeps the number of round-trips down.
        user_info_url.set_query(Some("$select=displayName&$top=999"));
        user_info_url
    }

    /// Rebases an `@odata.nextLink` onto the configured user info endpoint.
    ///
    /// Graph reports the link against its public `graph.microsoft.com` host, so following it
    /// verbatim would ignore a configured endpoint and send the access token to whichever host
    /// the response names. Only the path and query are taken from the link itself.
    pub fn next_page(&self, next_link: &str) -> Result<Url, Error> {
        let next_link_url = Url::parse(next_link).context(BuildEntraEndpointSnafu {
            endpoint: next_link,
        })?;

        let mut next_page_url = self.user_info_endpoint_url.clone();
        next_page_url.set_path(next_link_url.path());
        next_page_url.set_query(next_link_url.query());
        Ok(next_page_url)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use stackable_operator::v2::types::kubernetes::SecretName;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    use super::*;
    use crate::UserInfoRequestById;

    const TENANT_ID: &str = "1234-5678-1234-5678";
    const USER_ID: &str = "8765-4321-8765-4321";

    /// Builds a backend pointing at `mock_server`, bypassing [`ResolvedEntraBackend::resolve`]
    /// so that no credentials have to be read from disk.
    fn backend_for(mock_server: &MockServer) -> ResolvedEntraBackend {
        let host = HostName::from_str(&mock_server.address().ip().to_string()).unwrap();

        ResolvedEntraBackend {
            config: v1alpha2::EntraBackend {
                token_hostname: host.clone(),
                user_info_hostname: host,
                port: Some(mock_server.address().port()),
                tenant_id: TENANT_ID.to_owned(),
                tls: None,
                client_credentials_secret: SecretName::from_str("entra-credentials").unwrap(),
            },
            client_id: "client-id".to_owned(),
            client_secret: FileCredential::fixed("client-secret"),
            access_token: CachedToken::new(),
            http_client: reqwest::Client::new(),
        }
    }

    /// Mounts Entra's token endpoint, see [`crate::backend::test_mocks::mock_token`].
    async fn mock_token(mock_server: &MockServer, expires_in: Option<u64>, expected_calls: u64) {
        crate::backend::test_mocks::mock_token(
            mock_server,
            format!("/{TENANT_ID}/oauth2/v2.0/token"),
            expires_in,
            Some(expected_calls),
        )
        .await;
    }

    /// Mocks the OAuth2 token and user metadata endpoints, which every `get_user_info` call hits
    /// before it gets to the group memberships we actually care about.
    async fn mock_token_and_user(mock_server: &MockServer) {
        crate::backend::test_mocks::mock_token(
            mock_server,
            format!("/{TENANT_ID}/oauth2/v2.0/token"),
            None,
            None,
        )
        .await;

        Mock::given(method("GET"))
            .and(path(format!("/v1.0/users/{USER_ID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": USER_ID,
                "userPrincipalName": "alice@example.com",
            })))
            .mount(mock_server)
            .await;
    }

    /// Mounts the token endpoint plus a user endpoint answering `status`, so that a lookup can be
    /// driven into a chosen failure. `key` is whatever the request addresses the user by, which for
    /// Graph is the id for one lookup and the principal name for the other.
    async fn mock_token_and_failing_user(mock_server: &MockServer, key: &str, status: u16) {
        crate::backend::test_mocks::mock_token(
            mock_server,
            format!("/{TENANT_ID}/oauth2/v2.0/token"),
            None,
            None,
        )
        .await;

        Mock::given(method("GET"))
            .and(path(format!("/v1.0/users/{key}")))
            .respond_with(ResponseTemplate::new(status))
            .mount(mock_server)
            .await;
    }

    async fn look_up_by_id(backend: &ResolvedEntraBackend) -> Result<UserInfo, super::Error> {
        backend
            .get_user_info(&UserInfoRequest::UserInfoRequestById(UserInfoRequestById {
                id: USER_ID.to_owned(),
            }))
            .await
    }

    async fn look_up_by_name(
        backend: &ResolvedEntraBackend,
        username: &str,
    ) -> Result<UserInfo, super::Error> {
        backend
            .get_user_info(&UserInfoRequest::UserInfoRequestByName(
                crate::UserInfoRequestByName {
                    username: username.to_owned(),
                },
            ))
            .await
    }

    /// Graph answers a user it does not have with `404`, and that is the only case in which the
    /// lookup may report the user as absent.
    #[tokio::test]
    async fn entra_reports_a_user_it_does_not_have_as_not_found() {
        let mock_server = MockServer::start().await;
        mock_token_and_failing_user(&mock_server, USER_ID, 404).await;

        let error = look_up_by_id(&backend_for(&mock_server))
            .await
            .expect_err("a user Entra does not have cannot be looked up");

        assert!(matches!(error, Error::UserNotFoundById { .. }), "{error}");
        assert_eq!(
            http_error::Error::status_code(&error),
            StatusCode::NOT_FOUND
        );
    }

    /// Every other failure says nothing about whether the user exists. Reporting one as "not found"
    /// hands a policy the same answer for "Entra is unwell" as for "this user is not there", which
    /// is the difference an authorization decision most needs.
    #[tokio::test]
    async fn entra_reports_a_failed_lookup_as_a_failed_lookup() {
        for status in [500, 502, 403] {
            let mock_server = MockServer::start().await;
            mock_token_and_failing_user(&mock_server, USER_ID, status).await;

            let error = look_up_by_id(&backend_for(&mock_server))
                .await
                .expect_err("a failing lookup surfaces as an error");

            assert!(
                matches!(error, Error::SearchForUserById { .. }),
                "for {status} got {error}"
            );
            assert_eq!(
                http_error::Error::status_code(&error),
                StatusCode::BAD_GATEWAY
            );
        }
    }

    /// The by-name lookup had the opposite problem: it reported every failure as a failed lookup, so
    /// a username Entra genuinely does not have came back as `502` rather than as a missing user.
    #[tokio::test]
    async fn entra_reports_a_username_it_does_not_have_as_not_found() {
        let mock_server = MockServer::start().await;
        mock_token_and_failing_user(&mock_server, "nobody@example.com", 404).await;

        let error = look_up_by_name(&backend_for(&mock_server), "nobody@example.com")
            .await
            .expect_err("a username Entra does not have cannot be looked up");

        assert!(matches!(error, Error::UserNotFoundByName { .. }), "{error}");
        assert_eq!(
            http_error::Error::status_code(&error),
            StatusCode::NOT_FOUND
        );
    }

    async fn get_user_info_by_id(backend: &ResolvedEntraBackend) -> UserInfo {
        backend
            .get_user_info(&UserInfoRequest::UserInfoRequestById(UserInfoRequestById {
                id: USER_ID.to_owned(),
            }))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_entra_follows_paginated_group_memberships() {
        let mock_server = MockServer::start().await;
        mock_token_and_user(&mock_server).await;

        let group_path = format!("/v1.0/users/{USER_ID}/memberOf");
        let next_link_path = "/v1.0/paged-groups";

        // First page: two groups, plus a link to the second page.
        Mock::given(method("GET"))
            .and(path(group_path))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "value": [
                        {"displayName": "group-1"},
                        {"displayName": "group-2"},
                    ],
                    "@odata.nextLink": format!("https://graph.microsoft.com{next_link_path}?$skiptoken=abc"),
                })),
            )
            .mount(&mock_server)
            .await;

        // Second (final) page: one more group and no further link.
        Mock::given(method("GET"))
            .and(path(next_link_path))
            .and(query_param("$skiptoken", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {"displayName": "group-3"},
                ],
            })))
            .mount(&mock_server)
            .await;

        let user_info = get_user_info_by_id(&backend_for(&mock_server)).await;

        assert_eq!(user_info.groups, vec!["group-1", "group-2", "group-3"]);
    }

    #[tokio::test]
    async fn test_entra_group_pagination_is_bounded() {
        let mock_server = MockServer::start().await;
        mock_token_and_user(&mock_server).await;

        // Both the first page and every subsequent page link onwards, so without the page cap this
        // would page forever. `next_page` rebases the link onto the mock host, so the loop path is
        // what actually gets requested after the first page.
        let loop_path = "/v1.0/looping-groups";
        let next_link = format!("https://graph.microsoft.com{loop_path}");

        Mock::given(method("GET"))
            .and(path(format!("/v1.0/users/{USER_ID}/memberOf")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{"displayName": "group"}],
                "@odata.nextLink": next_link,
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(loop_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{"displayName": "group"}],
                "@odata.nextLink": next_link,
            })))
            .mount(&mock_server)
            .await;

        let user_info = get_user_info_by_id(&backend_for(&mock_server)).await;

        // One group per page, stopping once the cap is hit instead of looping forever.
        assert_eq!(user_info.groups.len(), MAX_GROUP_PAGES);
    }

    /// Mounts the user metadata and (empty) group endpoints, so a lookup gets all the way through.
    async fn mock_user_and_groups(mock_server: &MockServer, user_status: u16) {
        Mock::given(method("GET"))
            .and(path(format!("/v1.0/users/{USER_ID}")))
            .respond_with(
                ResponseTemplate::new(user_status).set_body_json(serde_json::json!({
                    "id": USER_ID,
                    "userPrincipalName": "alice@example.com",
                })),
            )
            .mount(mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/v1.0/users/{USER_ID}/memberOf")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": []})),
            )
            .mount(mock_server)
            .await;
    }

    /// The access token was minted for every single lookup, doubling the round trips. Entra reports
    /// how long it is valid for, so it only has to be minted once.
    #[tokio::test]
    async fn test_entra_reuses_a_cached_access_token_across_lookups() {
        let mock_server = MockServer::start().await;
        mock_token(&mock_server, Some(3600), 1).await;
        mock_user_and_groups(&mock_server, 200).await;

        let backend = backend_for(&mock_server);
        get_user_info_by_id(&backend).await;
        get_user_info_by_id(&backend).await;

        // The token endpoint's `.expect(1)` is verified when `mock_server` is dropped.
    }

    /// A token can stop being accepted before it expires, e.g. by being revoked. The rejection is the
    /// only way to find that out, so it has to trigger exactly one re-authentication. Not none
    /// (the lookup would keep failing until the token expired), and not a retry loop.
    #[tokio::test]
    async fn test_entra_reauthenticates_once_when_the_token_is_rejected() {
        let mock_server = MockServer::start().await;
        mock_token(&mock_server, Some(3600), 2).await;
        mock_user_and_groups(&mock_server, 401).await;

        let error = backend_for(&mock_server)
            .get_user_info(&UserInfoRequest::UserInfoRequestById(UserInfoRequestById {
                id: USER_ID.to_owned(),
            }))
            .await
            .expect_err("a permanently rejected token must surface as an error");

        assert!(utils::http::is_unauthorized(&error), "{error}");
        // The token endpoint's `.expect(2)` is verified when `mock_server` is dropped.
    }

    #[test]
    fn test_entra_defaults_id() {
        let tenant_id = "1234-5678-1234-5678";
        let user = "1234-5678-1234-5678";

        let entra = EntraBackend::try_new(
            &HostName::from_str("login.microsoft.com").unwrap(),
            &HostName::from_str("graph.microsoft.com").unwrap(),
            None,
            tenant_id,
            true,
        )
        .unwrap();

        assert_eq!(
            entra.oauth2_token(),
            Url::parse(&format!(
                "https://login.microsoft.com/{tenant_id}/oauth2/v2.0/token"
            ))
            .unwrap()
        );
        assert_eq!(
            entra.user_info(user),
            Url::parse(&format!("https://graph.microsoft.com/v1.0/users/{user}")).unwrap()
        );
        assert_eq!(
            entra.group_info(user),
            Url::parse(&format!(
                "https://graph.microsoft.com/v1.0/users/{user}/memberOf?$select=displayName&$top=999"
            ))
            .unwrap()
        );
        assert_eq!(
            entra
                .next_page("https://graph.microsoft.com/v1.0/paged-groups?$skiptoken=abc")
                .unwrap(),
            Url::parse("https://graph.microsoft.com/v1.0/paged-groups?$skiptoken=abc").unwrap()
        );
    }

    #[test]
    fn test_entra_custom_id() {
        let tenant_id = "1234-5678-1234-5678";
        let user = "1234-5678-1234-5678";

        let entra = EntraBackend::try_new(
            &HostName::from_str("login.mock.com").unwrap(),
            &HostName::from_str("graph.mock.com").unwrap(),
            Some(8080),
            tenant_id,
            false,
        )
        .unwrap();

        assert_eq!(
            entra.oauth2_token(),
            Url::parse(&format!(
                "http://login.mock.com:8080/{tenant_id}/oauth2/v2.0/token"
            ))
            .unwrap()
        );
        assert_eq!(
            entra.user_info(user),
            Url::parse(&format!("http://graph.mock.com:8080/v1.0/users/{user}")).unwrap()
        );
        assert_eq!(
            entra.group_info(user),
            Url::parse(&format!(
                "http://graph.mock.com:8080/v1.0/users/{user}/memberOf?$select=displayName&$top=999"
            ))
            .unwrap()
        );
        // The link Graph returns names its own public host, but the configured endpoint wins.
        assert_eq!(
            entra
                .next_page("https://graph.microsoft.com/v1.0/paged-groups?$skiptoken=abc")
                .unwrap(),
            Url::parse("http://graph.mock.com:8080/v1.0/paged-groups?$skiptoken=abc").unwrap()
        );
    }
}
