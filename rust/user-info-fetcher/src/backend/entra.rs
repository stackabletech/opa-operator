use std::{collections::HashMap, path::Path};

use hyper::StatusCode;
use info_fetcher_commons::utils::{self, http::send_json_request};
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

    #[snafu(display("failed to search for user with id {user_id:?}"))]
    UserNotFoundById {
        source: utils::http::Error,
        user_id: String,
    },

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
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::BuildEntraEndpoint { .. } => StatusCode::BAD_REQUEST,
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
    client_secret: String,
    http_client: reqwest::Client,
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

        let token_url = entra_backend.oauth2_token();
        let authn = send_json_request::<OAuthResponse>(self.http_client.post(token_url).form(&[
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("scope", "https://graph.microsoft.com/.default"),
            ("grant_type", "client_credentials"),
        ]))
        .await
        .context(AccessTokenSnafu)?;

        let user_info = match req {
            UserInfoRequest::UserInfoRequestById(req) => {
                let user_id = &req.id;
                send_json_request::<UserMetadata>(
                    self.http_client
                        .get(entra_backend.user_info(user_id))
                        .bearer_auth(&authn.access_token),
                )
                .await
                .with_context(|_| UserNotFoundByIdSnafu {
                    user_id: user_id.clone(),
                })?
            }
            UserInfoRequest::UserInfoRequestByName(req) => {
                let username = &req.username;
                send_json_request::<UserMetadata>(
                    self.http_client
                        .get(entra_backend.user_info(username))
                        .bearer_auth(&authn.access_token),
                )
                .await
                .with_context(|_| SearchForUserSnafu {
                    username: username.clone(),
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
                self.http_client.get(url).bearer_auth(&authn.access_token),
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
            client_secret: "client-secret".to_owned(),
            http_client: reqwest::Client::new(),
        }
    }

    /// Mocks the OAuth2 token and user metadata endpoints, which every `get_user_info` call hits
    /// before it gets to the group memberships we actually care about.
    async fn mock_token_and_user(mock_server: &MockServer) {
        Mock::given(method("POST"))
            .and(path(format!("/{TENANT_ID}/oauth2/v2.0/token")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-token",
            })))
            .mount(mock_server)
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
