use std::collections::HashMap;

use hyper::StatusCode;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use stackable_operator::commons::authentication::oidc;

use crate::{http_error, util::send_json_request, Credentials, UserInfo, UserInfoRequest};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("unable to log in (expired credentials?)"))]
    LogIn { source: reqwest::Error },

    #[snafu(display("unable to search for user"))]
    SearchForUser { source: reqwest::Error },

    #[snafu(display("user with userId {user_id:?} was not found"))]
    UserNotFoundById {
        source: reqwest::Error,
        user_id: String,
    },

    #[snafu(display("user with userName {user_name:?} was not found"))]
    UserNotFoundByName { user_name: String },

    #[snafu(display("unable to request groups for user"))]
    RequestUserGroups { source: reqwest::Error },

    #[snafu(display("unable to request roles for user"))]
    RequestUserRoles { source: reqwest::Error },

    #[snafu(display("failed to parse OIDC endpoint url"))]
    ParseOidcEndpointUrl { source: oidc::Error },

    #[snafu(display("failed to construct OIDC endpoint path"))]
    ConstructOidcEndpointPath { source: url::ParseError },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::LogIn { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundById { .. } => StatusCode::NOT_FOUND,
            Self::UserNotFoundByName { .. } => StatusCode::NOT_FOUND,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::RequestUserRoles { .. } => StatusCode::BAD_GATEWAY,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoleMembership {
    name: String,
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
    let wrapping_auth_provider = oidc::AuthenticationProvider::new(
        hostname.clone(),
        *port,
        root_path.clone(),
        tls.clone(),
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
        .basic_auth(&credentials.username, Some(&credentials.password))
        .form(&[("grant_type", "client_credentials")]),
    )
    .await
    .context(LogInSnafu)?;

    let users_base_url = keycloak_url
        .join(&format!("admin/realms/{user_realm}/users/"))
        .context(ConstructOidcEndpointPathSnafu)?;

    let (user_id, user) = match req {
        UserInfoRequest::UserInfoRequestById(req) => {
            let user_id = &req.user_id;

            let user = send_json_request::<UserMetadata>(
                http.get(
                    users_base_url
                        .join(&user_id.to_string())
                        .context(ConstructOidcEndpointPathSnafu)?,
                )
                .bearer_auth(&authn.access_token),
            )
            .await
            .context(UserNotFoundByIdSnafu { user_id })?;

            (user_id.clone(), user)
        }
        UserInfoRequest::UserInfoRequestByName(req) => {
            let user_name = &req.user_name;
            let users_url = users_base_url
                .join(&format!("?username={user_name}&exact=true"))
                .context(ConstructOidcEndpointPathSnafu)?;

            let user = send_json_request::<Vec<UserMetadata>>(
                http.get(users_url).bearer_auth(&authn.access_token),
            )
            .await
            .context(SearchForUserSnafu)?
            .first() // todo: we should probably fail if there are more than one record
            .cloned()
            .context(UserNotFoundByNameSnafu { user_name })?;

            let user_id = &user.id;

            (user_id.clone(), user)
        }
    };

    let user_url = users_base_url
        .join(&format!("{user_id}/"))
        .context(ConstructOidcEndpointPathSnafu)?;

    let groups = send_json_request::<Vec<GroupMembership>>(
        http.get(
            user_url
                .join("groups")
                .context(ConstructOidcEndpointPathSnafu)?,
        )
        .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserGroupsSnafu)?;

    let roles = send_json_request::<Vec<RoleMembership>>(
        http.get(
            user_url
                .join("role-mappings/realm/composite")
                .context(ConstructOidcEndpointPathSnafu)?,
        )
        .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserRolesSnafu)?;

    Ok(UserInfo {
        id: user_id,
        username: user.username,
        groups: groups
            .into_iter()
            .map(|group| crate::GroupRef { name: group.path })
            .collect(),
        roles: roles
            .into_iter()
            .map(|role| crate::RoleRef { name: role.name })
            .collect(),
        custom_attributes: user.attributes,
    })
}
