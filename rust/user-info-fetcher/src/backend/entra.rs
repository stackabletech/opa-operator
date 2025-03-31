use std::collections::HashMap;

use hyper::StatusCode;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use stackable_operator::commons::authentication::oidc;

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
    ParseOidcEndpointUrl { source: oidc::Error },

    #[snafu(display("failed to construct OIDC endpoint path"))]
    ConstructOidcEndpointPath { source: url::ParseError },
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
    displayName: String,
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

    // TODO: tls
    let host_port = port.unwrap_or(443);
    let token_url = format!("http://{hostname}:{host_port}/{tenant_id}/oauth2/v2.0/token");

    // -H "Content-Type: application/x-www-form-urlencoded" \
    // -d "client_id=${CLIENT_ID}" \
    // -d "client_secret=${CLIENT_SECRET}" \
    // -d "scope=https://graph.microsoft.com/.default" \
    // -d "grant_type=client_credentials" | jq -r .access_token)

    let authn = send_json_request::<OAuthResponse>(http.post(token_url).form(&[
        ("client_id", &credentials.client_id),
        ("client_secret", &credentials.client_secret),
        ("scope", &"https://graph.microsoft.com/.default".to_string()),
        ("grant_type", &"client_credentials".to_string()),
    ]))
    .await
    .context(AccessTokenSnafu)?;

    let tok = &authn.access_token;
    tracing::warn!("Got token: {tok}");

    let users_base_url = format!("http://{hostname}:{host_port}/v1.0/users");

    let user_info = match req {
        UserInfoRequest::UserInfoRequestById(req) => {
            let user_id = req.id.clone();
            send_json_request::<UserMetadata>(
                http.get(format!("{users_base_url}/{user_id}"))
                    .bearer_auth(&authn.access_token),
            )
            .await
            .context(UserNotFoundByIdSnafu { user_id })?
        }
        UserInfoRequest::UserInfoRequestByName(req) => {
            let username = &req.username;
            let users_url = format!("{users_base_url}/{username}");
            send_json_request::<UserMetadata>(http.get(users_url).bearer_auth(&authn.access_token))
                .await
                .context(SearchForUserSnafu)?
        }
    };

    let groups = send_json_request::<Vec<GroupMembership>>(
        http.get(format!("{users_base_url}/{id}/memberOf", id = user_info.id))
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
        groups: groups.into_iter().map(|g| g.displayName).collect(),
        custom_attributes: user_info.attributes,
    })
}
