//! Cross Federation Service Components (XFSC) Authentication and Authorization Service (AAS) backend.
//! The AAS provides context information for authorization decisions in the form of claims.
//! The endpoint is the CIP - ClaimsInformationPoint.
//! Claims are requested for a subject and scope, and are returned as a semi-structured object.
//!
//! Endpoint definition:
//! `<https://gitlab.eclipse.org/eclipse/xfsc/authenticationauthorization/-/blob/main/service/src/main/java/eu/xfsc/aas/controller/CipController.java>`
//!
//! Look at the endpoint definition for the API path, required parameters and the type of the returned object.
//!
//! This backend is currently in a minimal PoC state, it does not support TLS or authenticating at the endpoint.
//! This is because the AAS is also still in an early development stage and is likely to change.
use std::collections::HashMap;

use hyper::StatusCode;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use url::Url;

use crate::{http_error, util::send_json_request, UserInfo, UserInfoRequest};

static API_PATH: &str = "/cip/claims";
static SUB_CLAIM: &str = "sub";
static SCOPE_CLAIM: &str = "scope";
static OPENID_SCOPE: &str = "openid";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to parse AAS endpoint: {url:?} as URL"))]
    ParseAasEndpointUrl {
        source: url::ParseError,
        url: String,
    },

    #[snafu(display("request failed"))]
    Request { source: crate::util::Error },

    #[snafu(display("claims response has no 'sub' claim"))]
    SubClaimMissing {},

    #[snafu(display("The 'sub' claim value is not a string: {sub:?}"))]
    SubClaimValueNotAString { sub: serde_json::Value },

    #[snafu(display("the XFSC AAS does not support querying by username, only by user ID"))]
    UserInfoByUsernameNotSupported {},
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ParseAasEndpointUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Request { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::SubClaimMissing { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::SubClaimValueNotAString { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::UserInfoByUsernameNotSupported { .. } => StatusCode::NOT_IMPLEMENTED,
        }
    }
}

/// The return type of the CIP API endpoint.
#[derive(Deserialize)]
struct UserClaims {
    sub: String,
    #[serde(flatten)]
    other: HashMap<String, serde_json::Value>,
}

impl TryFrom<UserClaims> for UserInfo {
    type Error = Error;

    fn try_from(claims: UserClaims) -> Result<Self, Error> {
        let subject_id = claims
            .get(SUB_CLAIM)
            .context(SubClaimMissingSnafu)?
            .as_str()
            .context(SubClaimValueNotAStringSnafu)?
            .to_owned();
        Ok(UserInfo {
            id: Some(subject_id),
            username: None,
            groups: vec![],
            custom_attributes: claims,
        })
    }
}

/// Request user info from the AAS REST API by querying the
/// ClaimsInformationPoint (CIP) of the AAS.
///
/// Endpoint definition:
/// `<https://gitlab.eclipse.org/eclipse/xfsc/authenticationauthorization/-/blob/main/service/src/main/java/eu/xfsc/aas/controller/CipController.java>`
///
/// Only `UserInfoRequestById` is supported because the enpoint has no username concept.
pub(crate) async fn get_user_info(
    req: &UserInfoRequest,
    http: &reqwest::Client,
    config: &crd::AasBackend,
) -> Result<UserInfo, Error> {
    let crd::AasBackend { hostname, port } = config;

    let cip_endpoint_raw = format!("http://{hostname}:{port}{API_PATH}");
    let cip_endpoint = Url::parse(&cip_endpoint_raw).context(ParseAasEndpointUrlSnafu {
        url: cip_endpoint_raw,
    })?;

    let subject_id = match req {
        UserInfoRequest::UserInfoRequestById(r) => &r.id,
        UserInfoRequest::UserInfoRequestByName(_) => UserInfoByUsernameNotSupportedSnafu.fail()?,
    }
    .as_ref();

    let query_parameters: HashMap<&str, &str> = [
        (SUB_CLAIM, subject_id),
        (SCOPE_CLAIM, OPENID_SCOPE), // we only request the openid scope because that is the only scope that the AAS supports
    ]
    .into();

    let user_claims: UserClaims =
        send_json_request(http.get(cip_endpoint).query(&query_parameters))
            .await
            .context(RequestSnafu)?;

    user_claims.try_into()
}
