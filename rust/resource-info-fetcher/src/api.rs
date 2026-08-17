use std::fmt;

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

/// The maximum length, in bytes, of a single query parameter value.
///
/// Every parameter value ends up in the response cache key, so without a bound any caller who can
/// name a resource can fill the cache with arbitrarily large keys. Real identifiers are nowhere near
/// this: even a fully qualified DataHub URN stays in the low hundreds of bytes.
const MAX_PARAM_VALUE_LENGTH: usize = 1024;

/// A query parameter value, bounded to [`MAX_PARAM_VALUE_LENGTH`] bytes.
///
/// The bound is enforced while deserializing, so an over-long value is reported through the same
/// `400` envelope as any other malformed parameter (see [`crate::MetadataQuery`]) and never reaches
/// the backend or the cache.
///
/// Serializes as a plain string, so the container key hashes in
/// [`urn_for_request`](crate::backend::data_hub::resource_to_urn_mapping::urn_for_request) are
/// unaffected by the wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ParamValue(String);

impl<'de> Deserialize<'de> for ParamValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;

        if value.len() > MAX_PARAM_VALUE_LENGTH {
            return Err(serde::de::Error::custom(format!(
                "value is {length} bytes long, but at most {MAX_PARAM_VALUE_LENGTH} are allowed",
                length = value.len(),
            )));
        }

        Ok(Self(value))
    }
}

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ParamValue {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Only available in tests: outside of them a [`ParamValue`] is always deserialized from a request,
/// which is where the length bound has to be enforced.
#[cfg(test)]
impl From<&str> for ParamValue {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Database {
    pub system: ParamValue,
    pub instance: ParamValue,
    pub database: ParamValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Schema {
    pub system: ParamValue,
    pub instance: ParamValue,
    pub database: ParamValue,
    pub schema: ParamValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Table {
    pub system: ParamValue,
    pub instance: ParamValue,
    pub database: ParamValue,
    pub schema: ParamValue,
    pub table: ParamValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Stream {
    pub system: ParamValue,
    pub instance: ParamValue,

    /// AKA topic
    pub queue: ParamValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Dashboard {
    pub system: ParamValue,
    pub instance: ParamValue,

    /// The dashboard's identifier within its product, treated as an opaque string.
    ///
    /// Superset numbers its dashboards, but other products (e.g. Looker or Tableau) identify them by
    /// name, so this must not be narrowed to an integer. It is only ever spliced into the URN.
    pub id: ParamValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Chart {
    pub system: ParamValue,
    pub instance: ParamValue,

    /// The chart's identifier within its product, treated as an opaque string. See
    /// [`Dashboard::id`].
    pub id: ParamValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct RawIdentifier {
    pub identifier: ParamValue,
}

/// Generates the trivial `From<Params> for ResourceInfoRequest` conversions, so each HTTP handler
/// can turn its deserialized query parameters into a [`ResourceInfoRequest`] via `.into()`. Adding a
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Deserializes a [`Table`] whose `table` parameter is `length` bytes long.
    fn table_with_name_of_length(length: usize) -> Result<Table, serde_json::Error> {
        serde_json::from_value(json!({
            "system": "trino",
            "instance": "my-trino",
            "database": "tpch",
            "schema": "sf1",
            "table": "a".repeat(length),
        }))
    }

    /// Parameter values end up in the response cache key, so an unbounded one lets any caller who can
    /// name a table fill the cache with megabyte-sized keys. They are bounded while deserializing,
    /// which is the same path that renders a `400` for any other malformed parameter.
    #[test]
    fn param_values_within_the_limit_are_accepted() {
        table_with_name_of_length(MAX_PARAM_VALUE_LENGTH)
            .expect("a value at the limit must be accepted");
    }

    #[test]
    fn param_values_over_the_limit_are_rejected() {
        let error = table_with_name_of_length(MAX_PARAM_VALUE_LENGTH + 1)
            .expect_err("a value over the limit must be rejected");

        assert!(
            error.to_string().contains("1025 bytes"),
            "the error should report the offending length, but was: {error}"
        );
    }

    /// The wrapper must stay invisible in the serialized form, because the container URNs are MD5
    /// hashes over the serialized parameters and have to keep matching DataHub's own hashes.
    #[test]
    fn param_values_serialize_as_plain_strings() {
        let serialized = serde_json::to_string(&ParamValue::from("tpch"))
            .expect("a param value must be serializable");

        assert_eq!(serialized, r#""tpch""#);
    }
}
