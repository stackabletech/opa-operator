use std::collections::HashMap;

use hyper::StatusCode;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use stackable_operator::commons::authentication::oidc;

use crate::{http_error, util::send_json_request, Credentials, UserInfo, UserInfoRequest};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: reqwest::Error },

    #[snafu(display("failed to search for user"))]
    SearchForUser { source: reqwest::Error },

    #[snafu(display("unable to find user with id {user_id:?}"))]
    UserNotFoundById {
        source: reqwest::Error,
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
        username: String,
        user_id: String,
        source: reqwest::Error,
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
    attributes: HashMap<String, Vec<String>>,
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
    config: &crd::KeycloakBackend,
) -> Result<UserInfo, Error> {
    let crd::KeycloakBackend {
        client_credentials_secret: _,
        admin_realm,
        user_realm,
        hostname,
        port,
        root_path,
        tls,
    } = config;

    // We re-use existent functionality from operator-rs, besides it being a bit of miss-use.
    // Some attributes (such as principal_claim) are irrelevant, and will not be read by the code-flow we trigger.
    let wrapping_auth_provider = oidc::AuthenticationProvider::new(
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
        http.post(
            keycloak_url
                .join(&format!(
                    "realms/{admin_realm}/protocol/openid-connect/token"
                ))
                .context(ConstructOidcEndpointPathSnafu)?,
        )
        .basic_auth(&credentials.client_id, Some(&credentials.client_secret))
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
                http.get(
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
                http.get(users_url).bearer_auth(&authn.access_token),
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
        http.get(
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
