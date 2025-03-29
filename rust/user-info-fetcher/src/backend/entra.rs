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
    username: String,
    #[serde(default)]
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    path: String,
}

pub(crate) async fn get_user_info(
    req: &UserInfoRequest,
    http: &reqwest::Client,
    credentials: &Credentials,
    config: &v1alpha1::EntraBackend,
) -> Result<UserInfo, Error> {
    Ok(UserInfo::default())
}
