use std::collections::HashMap;

use hyper::StatusCode;
use serde::Deserialize;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use stackable_operator::commons::{networking::HostName, tls_verification::TlsClientDetails};

use crate::{http_error, utils::http::send_json_request, Credentials, UserInfo, UserInfoRequest};

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

    #[snafu(display(
        "failed to request groups for user with username {username:?} (user_id: {user_id:?})"
    ))]
    RequestUserGroups {
        source: crate::utils::http::Error,
        username: String,
        user_id: String,
    },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::AccessToken { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundById { .. } => StatusCode::NOT_FOUND,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
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
    user_principal_name: String,
    #[serde(default)]
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    display_name: String,
}

struct EntraEndpoint {
    hostname: HostName,
    port: u16,
    tenant_id: String,
    protocol: String,
}

pub(crate) async fn get_user_info(
    req: &UserInfoRequest,
    http: &reqwest::Client,
    credentials: &Credentials,
    config: &v1alpha1::EntraBackend,
) -> Result<UserInfo, Error> {
    let v1alpha1::EntraBackend {
        client_credentials_secret: _,
        hostname,
        port,
        tenant_id,
        tls,
    } = config;

    let entra_endpoint = EntraEndpoint::new(hostname.clone(), port.clone(), tenant_id.clone(), tls);
    let token_url = entra_endpoint.oauth2_token();

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
            let user_id = req.id.clone();
            send_json_request::<UserMetadata>(
                http.get(entra_endpoint.user_info(&user_id))
                    .bearer_auth(&authn.access_token),
            )
            .await
            .context(UserNotFoundByIdSnafu { user_id })?
        }
        UserInfoRequest::UserInfoRequestByName(req) => {
            let username = &req.username;
            send_json_request::<UserMetadata>(
                http.get(entra_endpoint.user_info(&username))
                    .bearer_auth(&authn.access_token),
            )
            .await
            .context(SearchForUserSnafu)?
        }
    };

    let groups = send_json_request::<Vec<GroupMembership>>(
        http.get(entra_endpoint.group_info(&user_info.id))
            .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserGroupsSnafu {
        username: user_info.user_principal_name.clone(),
        user_id: user_info.id.clone(),
    })?;

    Ok(UserInfo {
        id: Some(user_info.id),
        username: Some(user_info.user_principal_name),
        groups: groups.into_iter().map(|g| g.display_name).collect(),
        custom_attributes: user_info.attributes,
    })
}

impl EntraEndpoint {
    pub fn new(hostname: HostName, port: u16, tenant_id: String, tls: &TlsClientDetails) -> Self {
        Self {
            hostname,
            port,
            tenant_id,
            protocol: if tls.uses_tls() {
                "https".to_string()
            } else {
                "http".to_string()
            },
        }
    }

    pub fn oauth2_token(&self) -> String {
        format!(
            "{base_url}/{tenant_id}/oauth2/v2.0/token",
            base_url = self.base_url(),
            tenant_id = self.tenant_id
        )
    }

    // Works both with id/oid and userPrincipalName
    pub fn user_info(&self, user: &str) -> String {
        format!("{base_url}/v1.0/users/{user}", base_url = self.base_url())
    }

    pub fn group_info(&self, user: &str) -> String {
        format!(
            "{base_url}/v1.0/users/{user}/memberOf",
            base_url = self.base_url()
        )
    }

    fn base_url(&self) -> String {
        format!(
            "{protocol}://{hostname}{opt_port}",
            opt_port = if self.port == 443 || self.port == 80 {
                "".to_string()
            } else {
                format!(":{port}", port = self.port)
            },
            hostname = self.hostname,
            protocol = self.protocol
        )
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use stackable_operator::commons::tls_verification::{
        CaCert, Tls, TlsServerVerification, TlsVerification,
    };

    use super::*;

    #[test]
    fn test_defaults() {
        let entra_endpoint = EntraEndpoint::new(
            HostName::from_str("login.microsoft.com").expect("Could not parse hostname"),
            443,
            "1234-5678".to_string(),
            &TlsClientDetails {
                tls: Some(Tls {
                    verification: TlsVerification::Server(TlsServerVerification {
                        ca_cert: CaCert::WebPki {},
                    }),
                }),
            },
        );

        assert_eq!(
            entra_endpoint.oauth2_token(),
            "https://login.microsoft.com/1234-5678/oauth2/v2.0/token"
        );
        assert_eq!(
            entra_endpoint.user_info("0000-0000"),
            "https://login.microsoft.com/v1.0/users/0000-0000"
        );
        assert_eq!(
            entra_endpoint.group_info("0000-0000"),
            "https://login.microsoft.com/v1.0/users/0000-0000/memberOf"
        );
    }

    #[test]
    fn test_non_defaults_tls() {
        let entra_endpoint = EntraEndpoint::new(
            HostName::from_str("login.myentra.com").expect("Could not parse hostname"),
            8443,
            "1234-5678".to_string(),
            &TlsClientDetails {
                tls: Some(Tls {
                    verification: TlsVerification::Server(TlsServerVerification {
                        ca_cert: CaCert::WebPki {},
                    }),
                }),
            },
        );

        assert_eq!(
            entra_endpoint.oauth2_token(),
            "https://login.myentra.com:8443/1234-5678/oauth2/v2.0/token"
        );
    }

    #[test]
    fn test_non_defaults_non_tls() {
        let entra_endpoint = EntraEndpoint::new(
            HostName::from_str("login.myentra.com").expect("Could not parse hostname"),
            8080,
            "1234-5678".to_string(),
            &TlsClientDetails { tls: None },
        );

        assert_eq!(
            entra_endpoint.oauth2_token(),
            "http://login.myentra.com:8080/1234-5678/oauth2/v2.0/token"
        );
    }
}
