use std::collections::HashMap;

use hyper::StatusCode;
use serde::Deserialize;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use stackable_operator::commons::{networking::HostName, tls_verification::TlsClientDetails};
use url::Url;

use crate::{Credentials, UserInfo, UserInfoRequest, http_error, utils::http::send_json_request};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: crate::utils::http::Error },

    #[snafu(display("failed to search for user with username {username:?}"))]
    SearchForUser {
        source: crate::utils::http::Error,
        username: String,
    },

    #[snafu(display("failed to search for user with id {user_id:?}"))]
    UserNotFoundById {
        source: crate::utils::http::Error,
        user_id: String,
    },

    #[snafu(display(
        "failed to request groups for user with username {username:?} (user_id: {user_id:?})"
    ))]
    RequestUserGroups {
        source: crate::utils::http::Error,
        username: String,
        user_id: String,
    },

    #[snafu(display("failed to to build entra endpoint for {endpoint}"))]
    BuildEntraEndpointFailed {
        source: url::ParseError,
        endpoint: String,
    },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::AccessToken { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundById { .. } => StatusCode::NOT_FOUND,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::BuildEntraEndpointFailed { .. } => StatusCode::BAD_REQUEST,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    display_name: Option<String>,
}

pub(crate) async fn get_user_info(
    req: &UserInfoRequest,
    http: &reqwest::Client,
    credentials: &Credentials,
    config: &v1alpha1::EntraBackend,
) -> Result<UserInfo, Error> {
    let v1alpha1::EntraBackend {
        client_credentials_secret: _,
        token_hostname,
        user_info_hostname,
        port,
        tenant_id,
        tls,
    } = config;

    let entra_backend = EntraBackend::try_new(
        token_hostname,
        user_info_hostname,
        *port,
        tenant_id,
        TlsClientDetails { tls: tls.clone() }.uses_tls(),
    )?;

    let token_url = entra_backend.oauth2_token();
    let authn = send_json_request::<OAuthResponse>(http.post(token_url).form(&[
        ("client_id", credentials.client_id.as_str()),
        ("client_secret", credentials.client_secret.as_str()),
        ("scope", "https://graph.microsoft.com/.default"),
        ("grant_type", "client_credentials"),
    ]))
    .await
    .context(AccessTokenSnafu)?;

    let user_info = match req {
        UserInfoRequest::UserInfoRequestById(req) => {
            let user_id = &req.id;
            send_json_request::<UserMetadata>(
                http.get(entra_backend.user_info(user_id))
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
                http.get(entra_backend.user_info(username))
                    .bearer_auth(&authn.access_token),
            )
            .await
            .with_context(|_| SearchForUserSnafu {
                username: username.clone(),
            })?
        }
    };

    let groups = send_json_request::<GroupMembershipResponse>(
        http.get(entra_backend.group_info(&user_info.id))
            .bearer_auth(&authn.access_token),
    )
    .await
    .with_context(|_| RequestUserGroupsSnafu {
        username: user_info.user_principal_name.clone(),
        user_id: user_info.id.clone(),
    })?
    .value;

    Ok(UserInfo {
        id: Some(user_info.id),
        username: Some(user_info.user_principal_name),
        groups: groups.into_iter().filter_map(|g| g.display_name).collect(),
        custom_attributes: user_info.attributes,
    })
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
        let token_endpoint_url =
            Url::parse(&token_endpoint).context(BuildEntraEndpointFailedSnafu {
                endpoint: token_endpoint,
            })?;

        let user_info_endpoint = format!("{schema}://{user_info_endpoint}:{port}");
        let user_info_endpoint_url =
            Url::parse(&user_info_endpoint).context(BuildEntraEndpointFailedSnafu {
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

    pub fn group_info(&self, user: &str) -> Url {
        let mut user_info_url = self.user_info_endpoint_url.clone();
        user_info_url.set_path(&format!("/v1.0/users/{user}/memberOf"));
        user_info_url
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

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
                "https://graph.microsoft.com/v1.0/users/{user}/memberOf"
            ))
            .unwrap()
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
                "http://graph.mock.com:8080/v1.0/users/{user}/memberOf"
            ))
            .unwrap()
        );
    }
}
