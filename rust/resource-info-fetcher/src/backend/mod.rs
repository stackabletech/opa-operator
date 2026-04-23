use snafu::{ResultExt, Snafu};

use crate::{ResourceInfo, ResourceInfoRequest};

pub mod datahub;

#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum GetResourceInfoError {
    #[snafu(display("unsupported resource kind {kind:?}; only \"dataset\" is supported in v1"))]
    UnsupportedKind { kind: String },

    #[snafu(display("failed to get resource info from DataHub"))]
    DataHub { source: datahub::Error },
}

impl crate::http_error::Error for GetResourceInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::UnsupportedKind { .. } => hyper::StatusCode::BAD_REQUEST,
            Self::DataHub { source } => crate::http_error::Error::status_code(source),
        }
    }
}

pub enum ResolvedBackend {
    None,
    DataHub(datahub::ResolvedDataHubBackend),
}

impl ResolvedBackend {
    pub async fn get_resource_info(
        &self,
        req: &ResourceInfoRequest,
    ) -> Result<ResourceInfo, GetResourceInfoError> {
        match self {
            Self::None => Ok(ResourceInfo::default()),
            Self::DataHub(b) => b
                .get_resource_info(req)
                .await
                .context(get_resource_info_error::DataHubSnafu),
        }
    }
}
