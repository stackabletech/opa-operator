use std::sync::Arc;

use hyper::StatusCode;
use info_fetcher_commons::http_error;
use serde::{Deserialize, Serialize};
use snafu::Snafu;

use crate::backend;

pub trait ResourceInfoBackend {
    type Response: Serialize;

    async fn get_resource_info(
        &self,
        request: &ResourceInfoRequest,
    ) -> Result<Self::Response, GetResourceInfoError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfoRequest {
    // Global arguments shared between all resources
    pub stacklet: String,

    #[serde(flatten)]
    pub resource: ResourceInfoRequestResource,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceInfoRequestResource {
    TrinoTable {
        catalog: String,
        schema: String,
        table: String,
    },
    TrinoSchema {
        catalog: String,
        schema: String,
    },
    TrinoCatalog {
        catalog: String,
    },

    SupersetChart(String),
    SupersetDashboard(String),

    RawDataHubUrn(String),
}

#[derive(Snafu, Debug)]
pub enum GetResourceInfoError {
    #[snafu(display("failed to serialize response as JSON"))]
    SerializeResponseAsJson { source: serde_json::Error },

    #[snafu(
        context(false),
        display("failed to get resource information from DataHub")
    )]
    DataHub { source: backend::data_hub::Error },

    #[snafu(
        visibility(pub(crate)),
        display("failed to get user information from DataHub")
    )]
    GetDataHubUser {
        source: Arc<backend::data_hub::Error>,
    },
}

impl http_error::Error for GetResourceInfoError {
    fn status_code(&self) -> StatusCode {
        // todo: the warn here loses context about the scope in which the error occurred, eg: stackable_opa_resource_info_fetcher::backend::DATA_HUB
        // Also, we should make the log level (warn vs error) more dynamic in the backend's impl `http_error::Error for Error`
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::SerializeResponseAsJson { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::DataHub { source } => source.status_code(),
            Self::GetDataHubUser { source } => source.status_code(),
        }
    }
}
