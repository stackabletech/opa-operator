use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    api::{
        DataHubUrn, KafkaTopic, ResourceInfoRequest, SupersetChart, SupersetDashboard,
        TrinoCatalog, TrinoSchema, TrinoTable,
    },
    backend::data_hub::Urn,
};

pub fn urn_for_request(request: &ResourceInfoRequest) -> Urn {
    let urn = match request {
        ResourceInfoRequest::TrinoTable(TrinoTable {
            env,
            stacklet,
            catalog,
            schema,
            table,
        }) => {
            format!(
                "urn:li:dataset:(urn:li:dataPlatform:{stacklet},{catalog}.{schema}.{table},{env})"
            )
        }
        // Trino catalogs and schemas are modelled as DataHub `Container`s (subtypes `Database` and
        // `Schema` respectively). Unlike datasets, their URN is not human-readable but a GUID derived
        // from the container key - see `container_urn`.
        ResourceInfoRequest::TrinoCatalog(TrinoCatalog {
            env,
            stacklet,
            catalog,
        }) => container_urn(&BTreeMap::from([
            ("platform", stacklet),
            ("instance", env),
            ("database", catalog),
        ])),
        ResourceInfoRequest::TrinoSchema(TrinoSchema {
            env,
            stacklet,
            catalog,
            schema,
        }) => container_urn(&BTreeMap::from([
            ("platform", stacklet),
            ("instance", env),
            ("database", catalog),
            ("schema", schema),
        ])),
        ResourceInfoRequest::SupersetChart(SupersetChart { stacklet, id }) => {
            format!("urn:li:chart:({stacklet},{id})")
        }
        ResourceInfoRequest::SupersetDashboard(SupersetDashboard { stacklet, id }) => {
            format!("urn:li:dashboard:({stacklet},{id})")
        }
        ResourceInfoRequest::KafkaTopic(KafkaTopic {
            env,
            stacklet,
            topic,
        }) => {
            format!("urn:li:dataset:(urn:li:dataPlatform:{stacklet},{topic},{env})")
        }
        ResourceInfoRequest::DataHubUrn(DataHubUrn { urn }) => urn.to_owned(),
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
fn container_urn(
    container_key: &BTreeMap<impl AsRef<str> + Serialize, impl AsRef<str> + Serialize>,
) -> String {
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
        let request = ResourceInfoRequest::TrinoSchema(TrinoSchema {
            env: "PROD".to_owned(),
            stacklet: "trino".to_owned(),
            catalog: "lakehouse".to_owned(),
            schema: "customer_analytics".to_owned(),
        });

        assert_eq!(
            urn_for_request(&request).0,
            "urn:li:container:c8531e5a52cacf56768d0bf77ca8787c"
        );
    }

    /// Verified against a live DataHub: the catalog `lakehouse`.
    #[test]
    fn trino_catalog_urn_matches_data_hub() {
        let request = ResourceInfoRequest::TrinoCatalog(TrinoCatalog {
            env: "PROD".to_owned(),
            stacklet: "trino".to_owned(),
            catalog: "lakehouse".to_owned(),
        });

        assert_eq!(
            urn_for_request(&request).0,
            "urn:li:container:39967cd09b38e2d4736d1eb604cd5247"
        );
    }
}
