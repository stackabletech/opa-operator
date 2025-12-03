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
use reqwest::ClientBuilder;
use serde::Deserialize;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use url::Url;

use crate::{UserInfo, UserInfoRequest, http_error, utils::http::send_json_request};

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
    Request { source: crate::utils::http::Error },

    #[snafu(display("the XFSC AAS does not support querying by username, only by user ID"))]
    UserInfoByUsernameNotSupported {},

    #[snafu(display("failed to construct HTTP client"))]
    ConstructHttpClient { source: reqwest::Error },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ParseAasEndpointUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Request { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::UserInfoByUsernameNotSupported { .. } => StatusCode::NOT_IMPLEMENTED,
            Self::ConstructHttpClient { .. } => StatusCode::SERVICE_UNAVAILABLE,
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
        Ok(UserInfo {
            id: Some(claims.sub),
            username: None,
            groups: vec![],
            custom_attributes: claims.other,
        })
    }
}

/// Request user info from the AAS REST API by querying the
/// ClaimsInformationPoint (CIP) of the AAS.
///
/// Endpoint definition:
/// `<https://gitlab.eclipse.org/eclipse/xfsc/authenticationauthorization/-/blob/main/service/src/main/java/eu/xfsc/aas/controller/CipController.java>`
///
/// This struct combines the CRD configuration with an HTTP client initialized at startup.
pub struct ResolvedXfscAasBackend {
    config: v1alpha1::AasBackend,
    http_client: reqwest::Client,
}

impl ResolvedXfscAasBackend {
    /// Resolves an XFSC AAS backend by initializing the HTTP client.
    pub fn resolve(config: v1alpha1::AasBackend) -> Result<Self, Error> {
        let http_client = ClientBuilder::new()
            .build()
            .context(ConstructHttpClientSnafu)?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Only `UserInfoRequestById` is supported because the endpoint has no username concept.
    pub(crate) async fn get_user_info(&self, req: &UserInfoRequest) -> Result<UserInfo, Error> {
        let v1alpha1::AasBackend { hostname, port } = &self.config;

        let cip_endpoint_raw = format!("http://{hostname}:{port}{API_PATH}");
        let cip_endpoint = Url::parse(&cip_endpoint_raw).context(ParseAasEndpointUrlSnafu {
            url: cip_endpoint_raw,
        })?;

        let subject_id = match req {
            UserInfoRequest::UserInfoRequestById(r) => &r.id,
            UserInfoRequest::UserInfoRequestByName(_) => {
                UserInfoByUsernameNotSupportedSnafu.fail()?
            }
        }
        .as_ref();

        let query_parameters: HashMap<&str, &str> = [
            (SUB_CLAIM, subject_id),
            (SCOPE_CLAIM, OPENID_SCOPE), // we only request the openid scope because that is the only scope that the AAS supports
        ]
        .into();

        let user_claims: UserClaims =
            send_json_request(self.http_client.get(cip_endpoint).query(&query_parameters))
                .await
                .context(RequestSnafu)?;

        user_claims.try_into()
    }
}
