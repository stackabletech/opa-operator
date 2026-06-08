//! Per-config-file build steps assembled into the rolegroup `ConfigMap`.
//!
//! Unlike trino/hdfs (which emit several key-value `.properties` / `.xml` files), OPA only emits
//! JSON documents, so each builder returns a serialized `String` and there is no key-value
//! `writer`. The structure (one module per file + the [`ConfigFileName`] enum) mirrors the other
//! operators.

pub mod config_json;
pub mod logging;
pub mod user_info_fetcher;

/// The names of the config files assembled into the rolegroup `ConfigMap`.
///
/// The Vector config file is intentionally not listed here; like in hdfs it is added via the
/// `stackable_operator::product_logging::framework::VECTOR_CONFIG_FILE` constant.
#[derive(Clone, Copy, Debug, strum::Display)]
pub enum ConfigFileName {
    #[strum(serialize = "config.json")]
    ConfigJson,
    #[strum(serialize = "user-info-fetcher.json")]
    UserInfoFetcher,
}

#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::{Value, json};
    use stackable_operator::cli::OperatorEnvironmentOptions;

    use crate::{
        controller::validate::{ValidatedCluster, validate},
        crd::v1alpha2,
    };

    /// Builds an `OpaCluster` from the given `spec` JSON and runs the validate step, returning both
    /// the raw cluster (for owner references) and the [`ValidatedCluster`].
    pub fn validated_cluster_from_spec(spec: Value) -> (v1alpha2::OpaCluster, ValidatedCluster) {
        let opa: v1alpha2::OpaCluster = serde_json::from_value(json!({
            "apiVersion": "opa.stackable.tech/v1alpha2",
            "kind": "OpaCluster",
            "metadata": {
                "name": "test-opa",
                "namespace": "default",
                "uid": "c27b3971-ca72-42c1-80a4-abdfc1db0ddd",
            },
            "spec": spec,
        }))
        .expect("invalid test input");

        let operator_environment = OperatorEnvironmentOptions {
            operator_namespace: "stackable-operators".to_string(),
            operator_service_name: "opa-operator".to_string(),
            image_repository: "oci.stackable.tech/sdp".to_string(),
        };

        let validated = validate(&opa, &operator_environment)
            .expect("validation should succeed for the fixture");
        (opa, validated)
    }
}
