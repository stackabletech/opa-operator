use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use snafu::Snafu;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

mod http_error;
mod utils;

pub const APP_NAME: &str = "opa-resource-info-fetcher";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfoRequest {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub glossary_terms: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub data_products: Vec<String>,
    #[serde(default)]
    pub custom_properties: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub custom_attributes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub glossary_terms: Vec<String>,
}

#[derive(Snafu, Debug)]
enum StartupError {}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    println!("opa-resource-info-fetcher starting (stub)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_info_request_deserialize() {
        let json = serde_json::json!({
            "kind": "dataset",
            "id": "hive.db.table",
            "attributes": {"platform": "trino", "environment": "PROD"},
        });
        let req: ResourceInfoRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.kind, "dataset");
        assert_eq!(req.id, "hive.db.table");
        assert_eq!(req.attributes.get("platform").map(String::as_str), Some("trino"));
    }

    #[test]
    fn resource_info_serialize_roundtrip() {
        let mut info = ResourceInfo::default();
        info.tags.push("pii".to_owned());
        info.owners.push("user:alice@example.com".to_owned());
        info.fields.insert(
            "customer_id".to_owned(),
            FieldInfo {
                type_: "STRING".to_owned(),
                tags: vec!["pii".to_owned()],
                glossary_terms: vec![],
            },
        );
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["tags"], serde_json::json!(["pii"]));
        assert_eq!(json["owners"], serde_json::json!(["user:alice@example.com"]));
        assert_eq!(json["fields"]["customer_id"]["type"], serde_json::json!("STRING"));
    }
}
