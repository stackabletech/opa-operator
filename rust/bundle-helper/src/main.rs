use clap::Parser;
use futures::{StreamExt, TryStreamExt};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::cli::Command;
use stackable_operator::client;
use stackable_operator::error;
use stackable_operator::k8s_openapi::api::core::v1::ConfigMap;
use stackable_operator::kube::api::ListParams;
use stackable_operator::kube::runtime::utils::try_flatten_applied;
use stackable_operator::kube::runtime::watcher;
use stackable_operator::kube::Api;
use stackable_operator::namespace::WatchNamespace;
use std::env::VarError;
use std::path::Path;
use tokio::fs::create_dir_all;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub struct Ctx {
    pub client: stackable_operator::client::Client,
}

#[derive(Parser)]
#[clap(about = built_info::PKG_DESCRIPTION, author = stackable_operator::cli::AUTHOR)]
struct Opts {
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object defines no version"))]
    ObjectHasNoVersion,
    #[snafu(display("opa bundle has no name"))]
    OpaBundleHasNoName,
    #[snafu(display("opa bundle dir error"))]
    OpaBundleDir { source: std::io::Error },
    #[snafu(display("missing namespace to watch"))]
    MissingWatchNamespace,
}

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_BUNDLE_HELPER_LOG");

    // TODO: verify this
    stackable_operator::utils::print_startup_string(
        built_info::PKG_DESCRIPTION,
        built_info::PKG_VERSION,
        built_info::GIT_VERSION,
        built_info::TARGET,
        built_info::BUILT_TIME_UTC,
        built_info::RUSTC_VERSION,
    );

    let client = client::create_client(Some("opa.stackable.tech".to_string())).await?;
    match stackable_operator::namespace::get_watch_namespace()? {
        WatchNamespace::One(namespace) => {
            let opa_bundle_api: Api<ConfigMap> = client.get_namespaced_api(namespace.as_str());
            let mut watcher =
                try_flatten_applied(watcher(opa_bundle_api, ListParams::default().labels("opa.stackable.tech/bundle=true"))).boxed_local();
            while let Ok(Some(cm)) = watcher.try_next().await {
                // TODO: can we handle errors ?
                tracing::debug!("Applied ConfigMap name [{:?}]", cm.metadata.name);
                if let Err(e) = update_bundle(Path::new("/bundles"), &cm).await {
                    tracing::error!("{}", e);
                }
            }

            Ok(())
        }
        WatchNamespace::All => {
            // TODO: need to return an enum variant that is defined in operator-rs and this seems the best choice.
            // Is there a better way ?
            tracing::error!(
                "Missing namespace to watch. Env var [{}] is probably not defined.",
                stackable_operator::namespace::WATCH_NAMESPACE_ENV
            );
            Err(error::Error::EnvironmentVariableError {
                source: VarError::NotPresent,
            })
        }
    }
}

pub async fn update_bundle(root: impl AsRef<Path>, bundle: &ConfigMap) -> Result<(), Error> {
    let name = bundle
        .metadata
        .name
        .as_ref()
        .context(OpaBundleHasNoNameSnafu)?;
    let full_path = root.as_ref().join(Path::new(name.as_str()));
    create_dir_all(full_path)
        .await
        .with_context(|_| OpaBundleDirSnafu)?;

    Ok(())
}
