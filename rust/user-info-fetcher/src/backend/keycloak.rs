use std::{collections::HashMap, path::Path};

use hyper::StatusCode;
use reqwest::ClientBuilder;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha2;
use stackable_operator::crd::authentication::oidc;

use crate::{
    UserInfo, UserInfoRequest, http_error,
    utils::{self, http::send_json_request},
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: crate::utils::http::Error },

    #[snafu(display("failed to search for user"))]
    SearchForUser { source: crate::utils::http::Error },

    #[snafu(display("unable to find user with id {user_id:?}"))]
    UserNotFoundById {
        source: crate::utils::http::Error,
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
        source: crate::utils::http::Error,
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

        let mut client_builder = ClientBuilder::new();
        client_builder = utils::tls::configure_reqwest(&config.tls, client_builder)
            .await
            .context(ConfigureTlsSnafu)?;
        let http_client = client_builder.build().context(ConstructHttpClientSnafu)?;

        Ok(Self {
            config,
            client_id,
            client_secret,
            http_client,
        })
    }

    pub(crate) async fn get_user_info(&self, req: &UserInfoRequest) -> Result<UserInfo, Error> {
        let v1alpha2::KeycloakBackend {
            client_credentials_secret: _,
            admin_realm,
            user_realm,
            hostname,
            port,
            root_path,
            tls,
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
        let keycloak_url = wrapping_auth_provider
            .endpoint_url()
            .context(ParseOidcEndpointUrlSnafu)?;

        let authn = send_json_request::<OAuthResponse>(
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
                        .bearer_auth(&authn.access_token),
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
                    self.http_client
                        .get(users_url)
                        .bearer_auth(&authn.access_token),
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
                .bearer_auth(&authn.access_token),
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
