//! Per-config-file build steps assembled into the rolegroup `ConfigMap`.

pub mod config_json;
pub mod product_logging;
pub mod user_info_fetcher;

/// The names of the config files assembled into the rolegroup `ConfigMap`.
///
/// The Vector config file is intentionally not listed here; it is added separately via the
/// `VECTOR_CONFIG_FILE` constant.
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
        controller::{ValidatedCluster, validate::validate},
        crd::v1alpha2,
    };

    /// The expected `app.kubernetes.io/version` label value for the given product version.
    ///
    /// The `-stackable` suffix carries the operator's own version, which is `0.0.0-dev` on main
    /// but rewritten by the release process — so tests must derive it rather than hardcode it,
    /// or they fail on release branches.
    pub fn app_version_label(product_version: &str) -> String {
        format!(
            "{product_version}-stackable{}",
            crate::built_info::PKG_VERSION
        )
    }

    /// Builds an `OpaCluster` from the given `spec` JSON and runs the validate step, returning the
    /// resulting [`ValidatedCluster`].
    ///
    /// The cluster name (`test-opa`) deliberately differs from the product name (`opa`), so tests
    /// asserting recommended labels catch swapped `name`/`instance` values.
    pub fn validated_cluster_from_spec(spec: Value) -> ValidatedCluster {
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

        validate(&opa, &operator_environment).expect("validation should succeed for the fixture")
    }
}
