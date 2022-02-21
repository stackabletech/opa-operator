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
use std::fs::create_dir_all;
use std::fs::rename;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;

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
    #[snafu(display("config map [{name}] is empty"))]
    EmptyConfigMap { name: String },
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
            let mut watcher = try_flatten_applied(watcher(
                opa_bundle_api,
                ListParams::default().labels("opa.stackable.tech/bundle=true"),
            ))
            .boxed_local();
            while let Ok(Some(cm)) = watcher.try_next().await {
                // TODO: can we handle errors ?
                tracing::debug!("Applied ConfigMap name [{:?}]", cm.metadata.name);
                if let Err(e) = update_bundle(
                    Path::new("/bundles/active"),
                    Path::new("/bundles/incomming"),
                    &cm,
                ) {
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

/// Writes bundle.data under `root`.
pub fn update_bundle(root: &Path, incomming: &Path, bundle: &ConfigMap) -> Result<(), Error> {
    let name = bundle
        .metadata
        .name
        .as_ref()
        .context(OpaBundleHasNoNameSnafu)?;

    match bundle.data.as_ref() {
        Some(rules) => {
            let temp_full_path = incomming.join(Path::new(name.as_str()));
            create_dir_all(&temp_full_path).with_context(|_| OpaBundleDirSnafu)?;

            for (k, v) in rules.iter() {
                let rego_file_path = temp_full_path.clone().join(Path::new(k));

                File::create(&rego_file_path)
                    .and_then(|mut file| file.write_all(v.as_bytes()))
                    .context(OpaBundleDirSnafu)?;
            }

            let dest_path = root.join(Path::new(name));
            rename(&temp_full_path, &dest_path).context(OpaBundleDirSnafu)
        }
        None => Err(Error::EmptyConfigMap { name: name.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::update_bundle;

    use std::fs::create_dir;
    use std::fs::read_to_string;

    use stackable_operator::builder::{ConfigMapBuilder, ObjectMetaBuilder};
    use tempdir::TempDir;

    #[test]
    pub fn test_update_bundle() {
        let tmp = TempDir::new("test-bundle-helper").unwrap();
        let active = tmp.path().join("active");
        let incomming = tmp.path().join("incomming");

        create_dir(&active).unwrap();
        create_dir(&incomming).unwrap();

        let config_map = ConfigMapBuilder::new()
            .metadata(ObjectMetaBuilder::new().name("test-bundle-helper").build())
            .add_data(String::from("roles.rego"), String::from("allow user true"))
            .build()
            .unwrap();

        update_bundle(&active, &incomming, &config_map).unwrap();

        assert_eq!(
            String::from("allow user true"),
            read_to_string(active.join("test-bundle-helper/roles.rego")).unwrap()
        );
    }
}
