use std::sync::Arc;

use clap::{crate_description, crate_version, Parser};
use futures::StreamExt;
use product_config::ProductConfigManager;
use stackable_opa_crd::{OpaCluster, APP_NAME, OPERATOR_NAME};
use stackable_operator::{
    cli::{Command, ProductOperatorRun},
    client::{self, Client},
    error::{self, OperatorResult},
    k8s_openapi::api::{
        apps::v1::DaemonSet,
        core::v1::{ConfigMap, Service},
    },
    kube::{
        runtime::{watcher, Controller},
        Api,
    },
    logging::controller::report_controller_reconciled,
    namespace::WatchNamespace,
    CustomResourceExt,
};

use crate::controller::OPA_CONTROLLER_NAME;

mod controller;
mod discovery;
mod operations;
mod product_logging;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
    pub const TARGET_PLATFORM: Option<&str> = option_env!("TARGET");
    pub const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
}

#[derive(Parser)]
#[clap(about, author)]
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
        Command::Crd => {
            OpaCluster::print_yaml_schema(built_info::CARGO_PKG_VERSION)?;
        }
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
                crate_description!(),
                crate_version!(),
                built_info::GIT_VERSION,
                built_info::TARGET_PLATFORM.unwrap_or("unknown target"),
                built_info::BUILT_TIME_UTC,
                built_info::RUSTC_VERSION,
            );
            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/opa-operator/config-spec/properties.yaml",
            ])?;

            let client = client::create_client(Some(OPERATOR_NAME.to_string())).await?;
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

    let controller = Controller::new(opa_api, watcher::Config::default())
        .owns(daemonsets_api, watcher::Config::default())
        .owns(configmaps_api, watcher::Config::default())
        .owns(services_api, watcher::Config::default());

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
            report_controller_reconciled(
                &client,
                &format!("{OPA_CONTROLLER_NAME}.{OPERATOR_NAME}"),
                &res,
            )
        })
        .collect::<()>()
        .await;

    Ok(())
}
