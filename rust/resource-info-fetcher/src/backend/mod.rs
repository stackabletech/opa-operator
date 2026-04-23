pub mod datahub;

use snafu::Snafu;

use crate::{ResourceInfo, ResourceInfoRequest};

#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum GetResourceInfoError {
    #[snafu(display("unsupported resource kind {kind:?}; only \"dataset\" is supported in v1"))]
    UnsupportedKind { kind: String },
}

impl crate::http_error::Error for GetResourceInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::UnsupportedKind { .. } => hyper::StatusCode::BAD_REQUEST,
        }
    }
}

pub enum ResolvedBackend {
    None,
}

impl ResolvedBackend {
    pub async fn get_resource_info(
        &self,
        _req: &ResourceInfoRequest,
    ) -> Result<ResourceInfo, GetResourceInfoError> {
        match self {
            Self::None => Ok(ResourceInfo::default()),
        }
    }
}
