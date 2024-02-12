//! XFSC AAS backend.
//!
//!
//! Endpoint definition:
//! `<https://gitlab.eclipse.org/eclipse/xfsc/authenticationauthorization/-/blob/main/service/src/main/java/eu/xfsc/aas/controller/CipController.java>`
//!
//! Look at the endpoint defintion for the API path, required parameters and the type of the returned object.
use std::collections::HashMap;

use hyper::StatusCode;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use url::Url;

use crate::{http_error, util::send_json_request, UserInfo, UserInfoRequest};

static API_PATH: &str = "/cip/claims";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to parse AAS endpoint url"))]
    ParseAasEndpointUrl {
        source: url::ParseError,
        hostname: String,
        port: u16,
    },

    #[snafu(display("request failed"))]
    Request { source: crate::util::Error },

    #[snafu(display("The 'sub' claim is missing from the response from the claims endpoint."))]
    SubClaimMissing {},

    #[snafu(display("The 'sub' claim value is not a string."))]
    SubClaimValueNotAString {},
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ParseAasEndpointUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Request { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::SubClaimMissing { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::SubClaimValueNotAString { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

type UserClaims = HashMap<String, serde_json::Value>;

impl TryFrom<UserClaims> for UserInfo {
    type Error = Error;

    fn try_from(value: UserClaims) -> Result<Self, Error> {
        // extract the sub key
        let sub = value
            .get("sub")
            .context(SubClaimMissingSnafu)?
            .as_str()
            .context(SubClaimValueNotAStringSnafu)?
            .to_owned();
        // the attributes can contain arbitrary objects, we convert them into the structure that keycloak provides for now.
        let attributes = value
            .into_iter()
            .map(|(k, v)| (k, vec![v.to_string()]))
            .collect();
        // assemble UserInfo object
        Ok(UserInfo {
            id: Some(sub.clone()),
            username: Some(sub),
            groups: vec![],
            custom_attributes: attributes,
        })
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

    user_claims.try_into()
}
