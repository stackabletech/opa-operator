//! XFSC AAS backend.
//!
//!
//! Endpoint definition:
//! https://gitlab.eclipse.org/eclipse/xfsc/authenticationauthorization/-/blob/main/service/src/main/java/eu/xfsc/aas/controller/CipController.java
//!
//! Look at the endpoint defintion for the API path, required parameters and the type of the returned object.
use std::collections::HashMap;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use stackable_operator::commons::authentication::oidc;

use crate::{UserInfo, UserInfoRequest};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: crate::util::Error },

    #[snafu(display("failed to search for user"))]
    SearchForUser { source: crate::util::Error },

    #[snafu(display("unable to find user with id {user_id:?}"))]
    UserNotFoundById {
        source: crate::util::Error,
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
        source: crate::util::Error,
        username: String,
        user_id: String,
    },

    #[snafu(display("failed to parse OIDC endpoint url"))]
    ParseOidcEndpointUrl { source: oidc::Error },

    #[snafu(display("failed to construct OIDC endpoint path"))]
    ConstructOidcEndpointPath { source: url::ParseError },
}

type UserClaims = HashMap<String, serde_json::Value>;

impl From<UserClaims> for UserInfo {
    fn from(value: UserClaims) -> Self {
        // TODO fix unwraps
        let sub = value.get("sub").unwrap().as_str().unwrap().to_owned();
        let attributes = value
            .into_iter()
            .map(|(k, v)| (k, vec![v.to_string()]))
            .collect();
        UserInfo {
            id: Some(sub.clone()),
            username: Some(sub),
            groups: vec![],
            custom_attributes: attributes,
        }
    }
}

pub(crate) async fn get_user_info(
    req: &UserInfoRequest,
    http: &reqwest::Client,
    config: &crd::AasBackend,
) -> Result<UserInfo, Error> {
    let crd::AasBackend { hostname, port } = config;

    let port = port.unwrap_or(5000);

    let endpoint = "/cip/claims";

    let url = format!("http://{hostname}:{port}{endpoint}");

    // the AAS has no id/username distinction, we treat them both the same.
    let sub = match req {
        UserInfoRequest::UserInfoRequestById(r) => &r.id,
        UserInfoRequest::UserInfoRequestByName(r) => &r.username,
    }
    .as_ref();

    let params = [("sub", sub), ("scope", "openid")];

    let x = http.get(url).form(&params).send().await.unwrap();

    let user_claims: UserClaims = x.json().await.unwrap();

    Ok(user_claims.into())
}
