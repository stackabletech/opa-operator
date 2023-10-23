use std::collections::HashMap;

use hyper::StatusCode;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;

use crate::{http_error, util::send_json_request, Credentials, UserInfo, UserInfoRequest};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("unable to log in (expired credentials?)"))]
    LogIn { source: reqwest::Error },
    #[snafu(display("unable to search for user"))]
    SearchForUser { source: reqwest::Error },
    #[snafu(display("user {username:?} was not found"))]
    UserNotFound { username: String },
    #[snafu(display("unable to request groups for user"))]
    RequestUserGroups { source: reqwest::Error },
    #[snafu(display("unable to request roles for user"))]
    RequestUserRoles { source: reqwest::Error },
}
impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::LogIn { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFound { .. } => StatusCode::NOT_FOUND,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::RequestUserRoles { .. } => StatusCode::BAD_GATEWAY,
        }
    }
}

#[derive(Deserialize)]
struct OAuthResponse {
    access_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserMetadata {
    id: String,
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
        url: keycloak_url,
        admin_realm,
        user_realm,
        credentials_secret_name: _,
        client_id,
    } = config;
    let user_realm_url = format!("{keycloak_url}/admin/realms/{user_realm}");
    let authn = send_json_request::<OAuthResponse>(
        http.post(format!(
            "{keycloak_url}/realms/{admin_realm}/protocol/openid-connect/token"
        ))
        .form(&[
            ("grant_type", "password"),
            ("client_id", client_id),
            ("username", &credentials.username),
            ("password", &credentials.password),
        ]),
    )
    .await
    .context(LogInSnafu)?;
    let users = send_json_request::<Vec<UserMetadata>>(
        http.get(format!("{user_realm_url}/users"))
            .query(&[("exact", "true"), ("username", &req.username)])
            .bearer_auth(&authn.access_token),
    )
    .await
    .context(SearchForUserSnafu)?;
    let user = users.into_iter().next().context(UserNotFoundSnafu {
        username: &req.username,
    })?;
    let user_id = &user.id;
    let groups = send_json_request::<Vec<GroupMembership>>(
        http.get(format!("{user_realm_url}/users/{user_id}/groups"))
            .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserGroupsSnafu)?;
    let roles = send_json_request::<Vec<RoleMembership>>(
        http.get(format!(
            "{user_realm_url}/users/{user_id}/role-mappings/realm/composite"
        ))
        .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserRolesSnafu)?;
    Ok(UserInfo {
        groups: groups
            .into_iter()
            .map(|group| crate::GroupRef { name: group.path })
            .collect(),
        roles: roles
            .into_iter()
            .map(|role| crate::RoleRef { name: role.name })
            .collect(),
        custom_attributes: user
            .attributes
            .into_iter()
            // FIXME: why does keycloak support multiple values? do we need to support this? doesn't seem to be exposed in gui
            .filter_map(|(k, v)| Some((k, v.into_iter().next()?)))
            .collect(),
    })
}
