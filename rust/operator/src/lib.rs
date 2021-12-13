mod discovery;
mod opa_controller;

use futures::StreamExt;
use stackable_opa_crd::{OpenPolicyAgent, APP_NAME};
use stackable_operator::client::Client;
use stackable_operator::error::OperatorResult;
use stackable_operator::k8s_openapi::api::apps::v1::DaemonSet;
use stackable_operator::k8s_openapi::api::core::v1::{ConfigMap, Service};
use stackable_operator::kube::api::ListParams;
use stackable_operator::kube::runtime::controller::Context;
use stackable_operator::kube::runtime::Controller;
use stackable_operator::kube::Api;
use stackable_operator::product_config::ProductConfigManager;

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
pub async fn create_controller(
    client: Client,
    product_config: ProductConfigManager,
) -> OperatorResult<()> {
    let opa_api: Api<OpenPolicyAgent> = client.get_all_api();
    let daemonsets_api: Api<DaemonSet> = client.get_all_api();
    let configmaps_api: Api<ConfigMap> = client.get_all_api();
    let services_api: Api<Service> = client.get_all_api();

    let controller = Controller::new(opa_api, ListParams::default())
        .owns(daemonsets_api, ListParams::default())
        .owns(configmaps_api, ListParams::default())
        .owns(services_api, ListParams::default());

    controller
        .run(
            opa_controller::reconcile_opa,
            opa_controller::error_policy,
            Context::new(opa_controller::Ctx {
                client,
                product_config,
            }),
        )
        .for_each(|res| async {
            match res {
                Ok((obj, _)) => tracing::info!(object = %obj, "Reconciled object"),
                Err(err) => {
                    tracing::error!(
                        error = &err as &dyn std::error::Error,
                        "Failed to reconcile object",
                    )
                }
            }
        })
        .await;

    Ok(())
}
