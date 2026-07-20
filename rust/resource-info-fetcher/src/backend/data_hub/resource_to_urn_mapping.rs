use std::collections::BTreeMap;

use crate::{
    api::{ResourceInfoRequest, ResourceInfoRequestResource},
    backend::data_hub::upstream_api::Urn,
};

pub fn urn_for_request(request: &ResourceInfoRequest, env: &str) -> Urn {
    let stacklet = &request.stacklet;
    let urn = match &request.resource {
        ResourceInfoRequestResource::TrinoTable {
            catalog,
            schema,
            table,
        } => {
            format!(
                "urn:li:dataset:(urn:li:dataPlatform:{stacklet},{catalog}.{schema}.{table},{env})"
            )
        }
        // Trino catalogs and schemas are modelled as DataHub `Container`s (subtypes `Database` and
        // `Schema` respectively). Unlike datasets, their URN is not human-readable but a GUID derived
        // from the container key - see `container_urn`.
        ResourceInfoRequestResource::TrinoCatalog { catalog } => container_urn(&BTreeMap::from([
            ("platform", stacklet.as_str()),
            ("instance", env),
            ("database", catalog.as_str()),
        ])),
        ResourceInfoRequestResource::TrinoSchema { catalog, schema } => {
            container_urn(&BTreeMap::from([
                ("platform", stacklet.as_str()),
                ("instance", env),
                ("database", catalog.as_str()),
                ("schema", schema.as_str()),
            ]))
        }
        ResourceInfoRequestResource::SupersetChart(_) => todo!(),
        ResourceInfoRequestResource::SupersetDashboard(_) => todo!(),
        ResourceInfoRequestResource::RawDataHubUrn(urn) => urn.to_owned(),
    };

    Urn(urn)
}

/// Reproduces DataHub's `datahub_guid`: the container key is serialized to compact, key-sorted JSON
/// and MD5-hashed. A [`BTreeMap`] yields sorted keys and `serde_json` emits no whitespace, which
/// matches Python's `json.dumps(key, sort_keys=True, separators=(",", ":"))`.
///
/// Note that the SQL ingestion source sets `backcompat_env_as_instance`, so the configured `env`
/// (e.g. `PROD`) ends up in the `instance` field and no `env` field is present in the key. The
/// `platform` is the bare platform name (e.g. `trino`), *not* the `urn:li:dataPlatform:` form.
fn container_urn(container_key: &BTreeMap<&str, &str>) -> String {
    let key_json = serde_json::to_string(container_key)
        .expect("serializing a BTreeMap<&str, &str> cannot fail");
    format!("urn:li:container:{:x}", md5::compute(key_json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verified against a live DataHub: the schema `customer_analytics` in catalog `lakehouse`.
    #[test]
    fn trino_schema_urn_matches_data_hub() {
        let request = ResourceInfoRequest {
            stacklet: "trino".to_owned(),
            resource: ResourceInfoRequestResource::TrinoSchema {
                catalog: "lakehouse".to_owned(),
                schema: "customer_analytics".to_owned(),
            },
        };

        assert_eq!(
            urn_for_request(&request, "PROD").0,
            "urn:li:container:c8531e5a52cacf56768d0bf77ca8787c"
        );
    }

    /// Verified against a live DataHub: the catalog `lakehouse`.
    #[test]
    fn trino_catalog_urn_matches_data_hub() {
        let request = ResourceInfoRequest {
            stacklet: "trino".to_owned(),
            resource: ResourceInfoRequestResource::TrinoCatalog {
                catalog: "lakehouse".to_owned(),
            },
        };

        assert_eq!(
            urn_for_request(&request, "PROD").0,
            "urn:li:container:39967cd09b38e2d4736d1eb604cd5247"
        );
    }
}
