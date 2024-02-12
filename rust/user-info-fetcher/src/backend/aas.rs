//! XFSC AAS backend.
//!
//!
//! Endpoint definition:
//! https://gitlab.eclipse.org/eclipse/xfsc/authenticationauthorization/-/blob/main/service/src/main/java/eu/xfsc/aas/controller/CipController.java
//!
//! Look at the endpoint defintion for the API path, required parameters and the type of the returned object.
use std::collections::HashMap;

use hyper::StatusCode;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use stackable_operator::{commons::authentication::oidc, k8s_openapi::apimachinery::pkg::util};
use url::Url;

use crate::{http_error, util::send_json_request, UserInfo, UserInfoRequest};

static API_PATH: &str = "/cip/claims";

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

    #[snafu(display("failed to parse AAS endpoint url"))]
    ParseAasEndpointUrl {
        source: url::ParseError,
        hostname: String,
        port: u16,
    },

    #[snafu(display("request failed"))]
    Request { source: crate::util::Error },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::AccessToken { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFoundById { .. } => StatusCode::NOT_FOUND,
            Self::UserNotFoundByName { .. } => StatusCode::NOT_FOUND,
            Self::ParseAasEndpointUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Request { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

type UserClaims = HashMap<String, serde_json::Value>;

impl From<UserClaims> for UserInfo {
    fn from(value: UserClaims) -> Self {
        // TODO fix unwraps. What if the sub isn't there? Is it always there?
        println!("value: {:?}", value);
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

fn get_request_url(hostname: &str, port: &u16) -> Result<Url, Error> {
    Url::parse(&format!("http://{hostname}:{port}{API_PATH}")).context(ParseAasEndpointUrlSnafu {
        hostname,
        port: port.to_owned(),
    })
}

fn get_request_query(req: &UserInfoRequest) -> Result<HashMap<&str, &str>, Error> {
    // the AAS has no id/username distinction, we treat them both the same.
    let sub = match req {
        UserInfoRequest::UserInfoRequestById(r) => &r.id,
        UserInfoRequest::UserInfoRequestByName(r) => &r.username,
    }
    .as_ref();

    let mut query = HashMap::new();
    query.insert("sub", sub);
    query.insert("scope", "openid"); // always request the openid scope

    Ok(query)
}

pub(crate) async fn get_user_info(
    req: &UserInfoRequest,
    http: &reqwest::Client,
    config: &crd::AasBackend,
) -> Result<UserInfo, Error> {
    let crd::AasBackend { hostname, port } = config;

    let url = get_request_url(hostname, port)?;

    let args = get_request_query(req)?;

    let user_claims: UserClaims = send_json_request(http.get(url).query(&args))
        .await
        .context(RequestSnafu)?;

    Ok(user_claims.into())
}
