//! Ensures that `Pod`s are configured and running for each [`v1alpha2::OpaCluster`].
//!
//! This is the controller driver: it runs the `validate -> build -> apply -> update_status`
//! pipeline. The validated cluster type and the individual steps live under the
//! [`crate::controller`] module tree; this file is kept next to `main.rs` for consistency with
//! the other Stackable operators.

use std::{str::FromStr, sync::Arc};

use const_format::concatcp;
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    cluster_resources::ClusterResourceApplyStrategy,
    constant,
    kube::{
        Resource,
        core::{DeserializeGuard, error_boundary},
        runtime::controller::Action,
    },
    logging::controller::ReconcilerError,
    shared::time::Duration,
    utils::cluster_info::KubernetesClusterInfo,
    v2::types::operator::{ControllerName, OperatorName, ProductName},
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    controller::{
        apply::{self, Applier},
        build,
        update_status::{self, update_status},
        validate,
    },
    crd::{APP_NAME, OPA_OPERATOR_NAME, v1alpha2},
};

pub const OPA_CONTROLLER_NAME: &str = "opacluster";
pub const OPA_FULL_CONTROLLER_NAME: &str = concatcp!(OPA_CONTROLLER_NAME, '.', OPA_OPERATOR_NAME);

constant!(pub(crate) PRODUCT_NAME: ProductName = APP_NAME);
constant!(pub(crate) OPERATOR_NAME: OperatorName = OPA_OPERATOR_NAME);
constant!(pub(crate) CONTROLLER_NAME: ControllerName = OPA_CONTROLLER_NAME);

pub(crate) const CONTAINER_IMAGE_BASE_NAME: &str = "opa";

pub const OPA_STACKABLE_SERVICE_NAME: &str = "stackable";

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub opa_bundle_builder_image: String,
    pub user_info_fetcher_image: String,
    pub cluster_info: KubernetesClusterInfo,
    pub operator_environment: OperatorEnvironmentOptions,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
pub enum Error {
    #[snafu(display("OpaCluster object is invalid"))]
    InvalidOpaCluster {
        // boxed because otherwise Clippy warns about a large enum variant
        #[snafu(source(from(error_boundary::InvalidObject, Box::new)))]
        source: Box<error_boundary::InvalidObject>,
    },

    #[snafu(display("failed to validate cluster"))]
    ValidateCluster { source: validate::Error },

    #[snafu(display("failed to build the Kubernetes resources"))]
    BuildResources { source: build::Error },

    #[snafu(display("failed to apply the Kubernetes resources"))]
    ApplyResources { source: apply::Error },

    #[snafu(display("failed to update the cluster status"))]
    UpdateStatus { source: update_status::Error },
}
type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_opa(
    opa: Arc<DeserializeGuard<v1alpha2::OpaCluster>>,
    ctx: Arc<Ctx>,
) -> Result<Action> {
    tracing::info!("Starting reconcile");

    if opa.meta().deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }

    let opa = opa
        .0
        .as_ref()
        .map_err(error_boundary::InvalidObject::clone)
        .context(InvalidOpaClusterSnafu)?;

    let client = &ctx.client;

    let validated_cluster =
        validate::validate(opa, &ctx.operator_environment).context(ValidateClusterSnafu)?;

    let resources = build::build(
        &validated_cluster,
        &ctx.opa_bundle_builder_image,
        &ctx.user_info_fetcher_image,
        &ctx.cluster_info,
    )
    .context(BuildResourcesSnafu)?;

    let applied = Applier::new(
        client,
        &validated_cluster,
        ClusterResourceApplyStrategy::from(&opa.spec.cluster_operation),
        &opa.spec.object_overrides,
    )
    .apply(resources)
    .await
    .context(ApplyResourcesSnafu)?;

    update_status(client, opa, &applied)
        .await
        .context(UpdateStatusSnafu)?;

    Ok(Action::await_change())
}

pub fn error_policy(
    _obj: Arc<DeserializeGuard<v1alpha2::OpaCluster>>,
    error: &Error,
    _ctx: Arc<Ctx>,
) -> Action {
    match error {
        // root object is invalid, will be requeued when modified anyway
        Error::InvalidOpaCluster { .. } => Action::await_change(),

        _ => Action::requeue(*Duration::from_secs(10)),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use stackable_operator::{
        client::Client,
        commons::networking::DomainName,
        kube::{Client as KubeClient, Config},
    };

    use super::*;

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *PRODUCT_NAME;
        let _ = *OPERATOR_NAME;
        let _ = *CONTROLLER_NAME;
    }

    /// The client points at a closed port, so any API call would fail the reconciliation: an `Ok`
    /// proves that a cluster being deleted returns before the reconciler touches the Kubernetes
    /// API, and because the spec is invalid, before the [`DeserializeGuard`] is unwrapped.
    #[test]
    fn reconcile_exits_early_for_deleted_cluster() {
        // Building the kube client initialises rustls. kube enables its `ring` backend,
        // but the direct `rustls` dependency also enables the default `aws-lc-rs`
        // backend - with two candidates rustls refuses to auto-select one and panics.
        // Install one explicitly, exactly as main() does.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let opa = serde_yaml::from_str(
            r#"
apiVersion: opa.stackable.tech/v1alpha2
kind: OpaCluster
metadata:
  name: opa
  namespace: default
  deletionTimestamp: "2026-08-14T12:00:00Z"
spec: {}
"#,
        )
        .expect("YAML parses; the invalid spec is captured inside the DeserializeGuard");

        let action = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime")
            .block_on(async {
                let cluster_info = KubernetesClusterInfo {
                    cluster_domain: DomainName::from_str("cluster.local")
                        .expect("valid cluster domain"),
                };
                let ctx = Arc::new(Ctx {
                    client: Client::new(
                        KubeClient::try_from(Config::new(
                            "http://127.0.0.1:1".parse().expect("valid static URI"),
                        ))
                        .expect("client from static config"),
                        None,
                        "default".to_owned(),
                        cluster_info.clone(),
                    ),
                    opa_bundle_builder_image: "opa-bundle-builder".to_owned(),
                    user_info_fetcher_image: "user-info-fetcher".to_owned(),
                    cluster_info,
                    operator_environment: OperatorEnvironmentOptions {
                        operator_namespace: "stackable-operators".to_owned(),
                        operator_service_name: "opa-operator".to_owned(),
                        image_repository: "oci.stackable.tech/sdp".to_owned(),
                    },
                });

                reconcile_opa(Arc::new(opa), ctx).await
            })
            .expect("a deleted cluster reconciles without any API call");

        assert_eq!(action, Action::await_change());
    }
}
