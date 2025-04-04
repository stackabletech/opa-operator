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
    //username: String,
    mail: String,
    display_name: String,
    #[serde(default)]
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    id: String,
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
                http.get(entra_endpoint.users(&user_id))
                    .bearer_auth(&authn.access_token),
            )
            .await
            .context(UserNotFoundByIdSnafu { user_id })?
        }
        UserInfoRequest::UserInfoRequestByName(req) => {
            let username = &req.username;
            send_json_request::<UserMetadata>(
                http.get(entra_endpoint.users(&username))
                    .bearer_auth(&authn.access_token),
            )
            .await
            .context(SearchForUserSnafu)?
        }
    };

    let groups = send_json_request::<Vec<GroupMembership>>(
        http.get(entra_endpoint.member_of(&user_info.id))
            .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserGroupsSnafu {
        username: user_info.display_name.clone(),
        user_id: user_info.id.clone(),
    })?;

    Ok(UserInfo {
        id: Some(user_info.id),
        username: Some(user_info.display_name),
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

    pub fn users(&self, user: &str) -> String {
        format!("{base_url}/v1.0/users/{user}", base_url = self.base_url())
    }

    pub fn member_of(&self, user: &str) -> String {
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
