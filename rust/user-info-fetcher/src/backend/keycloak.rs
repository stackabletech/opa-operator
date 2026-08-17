use std::{collections::HashMap, path::Path, time::Duration};

use hyper::StatusCode;
use info_fetcher_commons::utils::{
    self,
    http::send_json_request,
    token::{CachedToken, MintedToken},
};
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha2;
use stackable_operator::crd::authentication::oidc;
use url::Url;

use crate::{UserInfo, UserInfoRequest, http_error};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: utils::http::Error },

    #[snafu(display("failed to search for user"))]
    SearchForUser { source: utils::http::Error },

    #[snafu(display("unable to find user with id {user_id:?}"))]
    UserNotFoundById {
        source: utils::http::Error,
        user_id: String,
    },

    #[snafu(display("unable to find user with username {username:?}"))]
    UserNotFoundByName { username: String },

    #[snafu(display("more than one user was returned when there should be one or none"))]
    TooManyUsersReturned,

    #[snafu(display(
        "failed to request groups for user with username {username:?} (user_id: {user_id:?})"
    ))]
    RequestUserGroups {
        source: utils::http::Error,
        username: String,
        user_id: String,
    },

    #[snafu(display("failed to parse OIDC endpoint url"))]
    ParseOidcEndpointUrl { source: oidc::v1alpha1::Error },

    #[snafu(display("failed to construct OIDC endpoint path"))]
    ConstructOidcEndpointPath { source: url::ParseError },

    #[snafu(display("failed to construct HTTP client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to read client ID from {path:?}"))]
    ReadClientId {
        source: std::io::Error,
        path: String,
    },

    #[snafu(display("failed to read client secret from {path:?}"))]
    ReadClientSecret {
        source: std::io::Error,
        path: String,
    },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::AccessToken { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundById { .. } => StatusCode::NOT_FOUND,
            Self::UserNotFoundByName { .. } => StatusCode::NOT_FOUND,
            Self::TooManyUsersReturned {} => StatusCode::INTERNAL_SERVER_ERROR,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::ParseOidcEndpointUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ConstructOidcEndpointPath { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ConstructHttpClient { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ConfigureTls { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ReadClientId { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ReadClientSecret { .. } => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

#[derive(Deserialize)]
struct OAuthResponse {
    access_token: String,

    /// How many seconds the token stays valid, which lets us cache it rather than mint one per
    /// lookup. Treated as optional so a response without it degrades to minting per lookup.
    expires_in: Option<u64>,
}

/// The minimal structure of [UserRepresentation] that is returned by [`/users`][users] and [`/users/{id}`][user-by-id].
/// <div class="warning">Some fields, such as `groups` are never present. See [keycloak/keycloak#20292][issue-20292]</div>
///
/// [users]: https://www.keycloak.org/docs-api/22.0.1/rest-api/index.html#_get_adminrealmsrealmusers
/// [user-by-id]: https://www.keycloak.org/docs-api/22.0.1/rest-api/index.html#_get_adminrealmsrealmusersid
/// [UserRepresentation]: https://www.keycloak.org/docs-api/22.0.1/rest-api/index.html#UserRepresentation
/// [issue-20292]: https://github.com/keycloak/keycloak/issues/20294
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserMetadata {
    id: String,
    username: String,
    #[serde(default)]
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    path: String,
}

/// Keycloak backend with resolved credentials.
///
/// This struct combines the CRD configuration with credentials loaded from the filesystem.
/// Credentials and the HTTP client are initialized once at startup and stored internally.
pub struct ResolvedKeycloakBackend {
    config: v1alpha2::KeycloakBackend,
    client_id: String,
    client_secret: String,
    http_client: reqwest::Client,

    /// The OAuth2 access token, minted on demand and reused until it is about to expire.
    access_token: CachedToken,
}

impl ResolvedKeycloakBackend {
    /// Resolves a Keycloak backend by loading credentials from the filesystem.
    ///
    /// Reads `clientId` and `clientSecret` from the credentials directory and initializes
    /// the HTTP client with appropriate TLS configuration.
    pub async fn resolve(
        config: v1alpha2::KeycloakBackend,
        credentials_dir: &Path,
    ) -> Result<Self, Error> {
        let client_id_path = credentials_dir.join("clientId");
        let client_secret_path = credentials_dir.join("clientSecret");

        let client_id =
            tokio::fs::read_to_string(&client_id_path)
                .await
                .context(ReadClientIdSnafu {
                    path: client_id_path.display().to_string(),
                })?;
        let client_secret = tokio::fs::read_to_string(&client_secret_path)
            .await
            .context(ReadClientSecretSnafu {
                path: client_secret_path.display().to_string(),
            })?;

        let mut client_builder = utils::http::client_builder();
        client_builder = utils::tls::configure_reqwest(&config.tls, client_builder)
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
        let keycloak_url = self.keycloak_url()?;

        let access_token = self.access_token(&keycloak_url).await?;
        match self
            .get_user_info_with(req, &keycloak_url, &access_token)
            .await
        {
            Err(error) if utils::http::is_unauthorized(&error) => {
                // The token was accepted when it was minted, so it has stopped being valid ahead of
                // its stated expiry - it was revoked, or the issuer's and our clock disagree. Drop it
                // and give the lookup exactly one more go with a fresh one.
                tracing::warn!(
                    error = &error as &dyn std::error::Error,
                    "Keycloak rejected the cached access token; re-authenticating and retrying once"
                );
                self.access_token.invalidate().await;

                let access_token = self.access_token(&keycloak_url).await?;
                self.get_user_info_with(req, &keycloak_url, &access_token)
                    .await
            }
            result => result,
        }
    }

    /// The base URL of the configured Keycloak.
    fn keycloak_url(&self) -> Result<Url, Error> {
        let v1alpha2::KeycloakBackend {
            hostname,
            port,
            root_path,
            tls,
            ..
        } = &self.config;

        // We re-use existent functionality from operator-rs, besides it being a bit of miss-use.
        // Some attributes (such as principal_claim) are irrelevant, and will not be read by the code-flow we trigger.
        let wrapping_auth_provider = oidc::v1alpha1::AuthenticationProvider::new(
            hostname.clone(),
            *port,
            root_path.clone(),
            tls.clone(),
            String::new(),
            Vec::new(),
            None,
        );

        wrapping_auth_provider
            .endpoint_url()
            .context(ParseOidcEndpointUrlSnafu)
    }

    /// The cached access token, minting one if there is none or it is about to expire.
    async fn access_token(&self, keycloak_url: &Url) -> Result<String, Error> {
        let admin_realm = &self.config.admin_realm;

        self.access_token
            .get(|| async {
                let response = send_json_request::<OAuthResponse>(
                    self.http_client
                        .post(
                            keycloak_url
                                .join(&format!(
                                    "realms/{admin_realm}/protocol/openid-connect/token"
                                ))
                                .context(ConstructOidcEndpointPathSnafu)?,
                        )
                        .basic_auth(&self.client_id, Some(&self.client_secret))
                        .form(&[("grant_type", "client_credentials")]),
                )
                .await
                .context(AccessTokenSnafu)?;

                Ok(MintedToken {
                    token: response.access_token,
                    lifetime: response.expires_in.map(Duration::from_secs),
                })
            })
            .await
    }

    /// Looks the user up with an already-obtained `access_token`, so the caller can retry with a fresh
    /// one if this token turns out to be rejected.
    async fn get_user_info_with(
        &self,
        req: &UserInfoRequest,
        keycloak_url: &Url,
        access_token: &str,
    ) -> Result<UserInfo, Error> {
        let user_realm = &self.config.user_realm;

        let users_base_url = keycloak_url
            .join(&format!("admin/realms/{user_realm}/users/"))
            .context(ConstructOidcEndpointPathSnafu)?;

        let user_info = match req {
            UserInfoRequest::UserInfoRequestById(req) => {
                let user_id = req.id.clone();
                send_json_request::<UserMetadata>(
                    self.http_client
                        .get(
                            users_base_url
                                .join(&req.id)
                                .context(ConstructOidcEndpointPathSnafu)?,
                        )
                        .bearer_auth(access_token),
                )
                .await
                .context(UserNotFoundByIdSnafu { user_id })?
            }
            UserInfoRequest::UserInfoRequestByName(req) => {
                let username = &req.username;
                let users_url = users_base_url
                    .join(&format!("?username={username}&exact=true"))
                    .context(ConstructOidcEndpointPathSnafu)?;

                let users = send_json_request::<Vec<UserMetadata>>(
                    self.http_client.get(users_url).bearer_auth(access_token),
                )
                .await
                .context(SearchForUserSnafu)?;

                if users.len() > 1 {
                    return TooManyUsersReturnedSnafu.fail();
                }

                users
                    .first()
                    .cloned()
                    .context(UserNotFoundByNameSnafu { username })?
            }
        };

        let groups = send_json_request::<Vec<GroupMembership>>(
            self.http_client
                .get(
                    users_base_url
                        .join(&format!("{}/groups", user_info.id))
                        .context(ConstructOidcEndpointPathSnafu)?,
                )
                .bearer_auth(access_token),
        )
        .await
        .context(RequestUserGroupsSnafu {
            username: user_info.username.clone(),
            user_id: user_info.id.clone(),
        })?;

        Ok(UserInfo {
            id: Some(user_info.id),
            username: Some(user_info.username),
            groups: groups.into_iter().map(|g| g.path).collect(),
            custom_attributes: user_info.attributes,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use stackable_operator::{
        commons::{networking::HostName, tls_verification::TlsClientDetails},
        v2::types::kubernetes::SecretName,
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::{UserInfoRequestById, backend::keycloak::ResolvedKeycloakBackend};

    const ADMIN_REALM: &str = "master";
    const USER_REALM: &str = "my-realm";
    const USER_ID: &str = "8765-4321-8765-4321";

    /// Builds a backend pointing at `mock_server`, bypassing [`ResolvedKeycloakBackend::resolve`] so
    /// that no credentials have to be read from disk.
    fn backend_for(mock_server: &MockServer) -> ResolvedKeycloakBackend {
        ResolvedKeycloakBackend {
            config: v1alpha2::KeycloakBackend {
                hostname: HostName::from_str(&mock_server.address().ip().to_string()).unwrap(),
                port: Some(mock_server.address().port()),
                root_path: "/".to_owned(),
                tls: TlsClientDetails { tls: None },
                client_credentials_secret: SecretName::from_str("keycloak-credentials").unwrap(),
                admin_realm: ADMIN_REALM.to_owned(),
                user_realm: USER_REALM.to_owned(),
            },
            client_id: "client-id".to_owned(),
            client_secret: "client-secret".to_owned(),
            http_client: reqwest::Client::new(),
            access_token: CachedToken::new(),
        }
    }

    /// Mounts the token endpoint, answering with a token valid for `expires_in` seconds.
    /// `expected_calls` is verified by wiremock when the server is dropped.
    async fn mock_token(mock_server: &MockServer, expires_in: Option<u64>, expected_calls: u64) {
        let mut body = serde_json::json!({"access_token": "access-token"});
        if let Some(expires_in) = expires_in {
            body["expires_in"] = expires_in.into();
        }

        Mock::given(method("POST"))
            .and(path(format!(
                "/realms/{ADMIN_REALM}/protocol/openid-connect/token"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(expected_calls)
            .mount(mock_server)
            .await;
    }

    /// Mounts the user metadata and (empty) group endpoints, so a lookup gets all the way through.
    async fn mock_user_and_groups(mock_server: &MockServer, user_status: u16) {
        Mock::given(method("GET"))
            .and(path(format!("/admin/realms/{USER_REALM}/users/{USER_ID}")))
            .respond_with(
                ResponseTemplate::new(user_status).set_body_json(serde_json::json!({
                    "id": USER_ID,
                    "username": "alice",
                })),
            )
            .mount(mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/admin/realms/{USER_REALM}/users/{USER_ID}/groups"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(mock_server)
            .await;
    }

    async fn get_user_info_by_id(
        backend: &ResolvedKeycloakBackend,
    ) -> Result<UserInfo, super::Error> {
        backend
            .get_user_info(&UserInfoRequest::UserInfoRequestById(UserInfoRequestById {
                id: USER_ID.to_owned(),
            }))
            .await
    }

    /// The access token was minted for every single lookup, doubling the round trips. Keycloak reports
    /// how long it is valid for, so it only has to be minted once.
    #[tokio::test]
    async fn keycloak_reuses_a_cached_access_token_across_lookups() {
        let mock_server = MockServer::start().await;
        mock_token(&mock_server, Some(3600), 1).await;
        mock_user_and_groups(&mock_server, 200).await;

        let backend = backend_for(&mock_server);
        get_user_info_by_id(&backend).await.expect("lookup works");
        get_user_info_by_id(&backend).await.expect("lookup works");

        // The token endpoint's `.expect(1)` is verified when `mock_server` is dropped.
    }

    /// A token can stop being accepted before it expires, e.g. by being revoked. The rejection is the
    /// only way to find that out, so it has to trigger exactly one re-authentication - not none
    /// (the lookup would keep failing until the token expired) and not a retry loop.
    #[tokio::test]
    async fn keycloak_reauthenticates_once_when_the_token_is_rejected() {
        let mock_server = MockServer::start().await;
        mock_token(&mock_server, Some(3600), 2).await;
        mock_user_and_groups(&mock_server, 401).await;

        let error = get_user_info_by_id(&backend_for(&mock_server))
            .await
            .expect_err("a permanently rejected token must surface as an error");

        assert!(utils::http::is_unauthorized(&error), "{error}");
        // The token endpoint's `.expect(2)` is verified when `mock_server` is dropped.
    }
}
