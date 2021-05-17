//! Import and use OpaReference in the operator CRDs. For now, it only works with Java based products
//! (e.g. Kafka) and an adapted product package (https://github.com/Bisnode/opa-kafka-plugin).
//! The the opa-authorizer*.jar must be provided in the lib directory of the package
//! e.g. check https://github.com/Bisnode/opa-kafka-plugin/releases/

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The property to be set for the full class name of the supplied authorizer
/// (must be included in the product package manually)
/// Refers to OpaReference.authorizer_class_name
pub const AUTHORIZER_CLASS_NAME_PROPERTY: &str = "authorizer.class.name";
/// The property to be set for the OPA authorizer url
/// Refers to OpaReference.opa_authorizer_url
pub const OPA_AUTHORIZER_URL_PROPERTY: &str = "opa.authorizer.url";

/// Contains all necessary information to connect to a Stackable managed
/// Open Policy Agent (OPA).
/// The main purpose for this struct is for other operators that need to reference
/// OPA to use in their CRDs.
/// This has the benefit of keeping references to OPA consistent
/// throughout the entire stack.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaReference {
    pub authorizer_class_name: String,
    pub opa_authorizer_url: String,
}
