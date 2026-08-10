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

/// The resource whose metadata was requested, in a backend-agnostic form.
///
/// There is one HTTP endpoint per variant (e.g. `GET /metadata/trinoTable`). Each endpoint
/// deserializes its query parameters into the variant's payload struct and the backend then maps
/// the request to whatever the concrete backend needs (for DataHub: a URN, see [`urn_for_request`]).
///
/// Each variant wraps its own parameter struct rather than inlining the fields, so a single struct
/// serves as both the HTTP query-parameter target ([`axum::extract::Query`]) and the enum payload —
/// there is no second copy of the field list to keep in sync.
///
/// [`urn_for_request`]: crate::backend::data_hub::resource_to_urn_mapping::urn_for_request
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceInfoRequest {
    Database(Database),
    Schema(Schema),
    Table(Table),
    Stream(Stream),
    Dashboard(Dashboard),
    Chart(Chart),

    /// Generic fallback to support arbitrary identifiers, e.g. URNs in the case of DataHub.
    RawIdentifier(RawIdentifier),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Database {
    pub system: String,
    pub instance: String,
    pub database: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Schema {
    pub system: String,
    pub instance: String,
    pub database: String,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Table {
    pub system: String,
    pub instance: String,
    pub database: String,
    pub schema: String,
    pub table: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Stream {
    pub system: String,
    pub instance: String,

    /// AKA topic
    pub queue: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Dashboard {
    pub system: String,
    pub instance: String,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Chart {
    pub system: String,
    pub instance: String,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct RawIdentifier {
    pub identifier: String,
}

/// Generates the trivial `From<Params> for ResourceInfoRequest` conversions, so each HTTP handler
/// can turn its deserialized query parameters into a [`ResourceInfoRequest`] via `.from()`. Adding a
/// resource type means adding its struct above and one entry here — no hand-written conversion.
macro_rules! impl_into_resource_info_request {
    ($($variant:ident),+ $(,)?) => {
        $(
            impl From<$variant> for ResourceInfoRequest {
                fn from(params: $variant) -> Self {
                    Self::$variant(params)
                }
            }
        )+
    };
}

impl_into_resource_info_request!(
    Database,
    Schema,
    Table,
    Stream,
    Dashboard,
    Chart,
    RawIdentifier
);

#[derive(Snafu, Debug)]
pub enum GetResourceInfoError {
    #[snafu(display("failed to serialize response as JSON"))]
    SerializeResponseAsJson { source: serde_json::Error },

    #[snafu(
        context(false),
        display("failed to get resource information from DataHub")
    )]
    DataHub { source: backend::data_hub::Error },
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
        }
    }
}
