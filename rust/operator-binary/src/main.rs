mod controller;
mod discovery;

use clap::Parser;
use futures::StreamExt;
use stackable_opa_crd::{OpaCluster, APP_NAME};
use stackable_operator::cli::ProductOperatorRun;
use stackable_operator::namespace::WatchNamespace;
use stackable_operator::{
    cli::Command,
    client,
    client::Client,
    error,
    error::OperatorResult,
    k8s_openapi::api::{
        apps::v1::DaemonSet,
        core::v1::{ConfigMap, Service},
    },
    kube::{api::ListParams, runtime::Controller, Api, CustomResourceExt},
    logging::controller::report_controller_reconciled,
    product_config::ProductConfigManager,
};
use std::sync::Arc;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[derive(Parser)]
#[clap(about = built_info::PKG_DESCRIPTION, author = stackable_operator::cli::AUTHOR)]
struct Opts {
    #[clap(subcommand)]
    cmd: Command<OpaRun>,
}

#[derive(clap::Parser)]
struct OpaRun {
    #[clap(long, env)]
    opa_bundle_builder_clusterrole: String,
    #[clap(flatten)]
    common: ProductOperatorRun,
}

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    let opts = Opts::parse();
    match opts.cmd {
        Command::Crd => println!("{}", serde_yaml::to_string(&OpaCluster::crd())?),
        Command::Run(OpaRun {
            opa_bundle_builder_clusterrole: opa_builder_clusterrole,
            common:
                ProductOperatorRun {
                    product_config,
                    watch_namespace,
                    tracing_target,
                },
        }) => {
            stackable_operator::logging::initialize_logging(
                "OPA_OPERATOR_LOG",
                APP_NAME,
                tracing_target,
            );

            stackable_operator::utils::print_startup_string(
                built_info::PKG_DESCRIPTION,
                built_info::PKG_VERSION,
                built_info::GIT_VERSION,
                built_info::TARGET,
                built_info::BUILT_TIME_UTC,
                built_info::RUSTC_VERSION,
            );
            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/opa-operator/config-spec/properties.yaml",
            ])?;
            let client = client::create_client(Some("opa.stackable.tech".to_string())).await?;
            create_controller(
                client,
                product_config,
                watch_namespace,
                opa_builder_clusterrole,
            )
            .await?;
        }
    };

    Ok(())
}

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
async fn create_controller(
    client: Client,
    product_config: ProductConfigManager,
    watch_namespace: WatchNamespace,
    opa_bundle_builder_clusterrole: String,
) -> OperatorResult<()> {
    let opa_api: Api<OpaCluster> = watch_namespace.get_api(&client);
    let daemonsets_api: Api<DaemonSet> = watch_namespace.get_api(&client);
    let configmaps_api: Api<ConfigMap> = watch_namespace.get_api(&client);
    let services_api: Api<Service> = watch_namespace.get_api(&client);

    let controller = Controller::new(opa_api, ListParams::default())
        .owns(daemonsets_api, ListParams::default())
        .owns(configmaps_api, ListParams::default())
        .owns(services_api, ListParams::default());

    controller
        .run(
            controller::reconcile_opa,
            controller::error_policy,
            Arc::new(controller::Ctx {
                client: client.clone(),
                product_config,
                opa_bundle_builder_clusterrole,
            }),
        )
        .map(|res| {
            report_controller_reconciled(&client, "openpolicyagents.opa.stackable.tech", &res)
        })
        .collect::<()>()
        .await;

    Ok(())
}
