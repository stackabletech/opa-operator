use flate2::write::GzEncoder;
use flate2::Compression;
use futures::Future;
use futures::StreamExt;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::client;
use stackable_operator::client::Client;
use stackable_operator::error;
use stackable_operator::error::OperatorResult;
use stackable_operator::k8s_openapi::api::core::v1::ConfigMap;
use stackable_operator::kube::api::ListParams;
use stackable_operator::kube::runtime::controller::Context;
use stackable_operator::kube::runtime::controller::ReconcilerAction;
use stackable_operator::kube::runtime::Controller;
use stackable_operator::kube::Api;
use stackable_operator::logging::controller::report_controller_reconciled;
use stackable_operator::logging::controller::ReconcilerError;
use std::env;
use std::fs::create_dir_all;
use std::fs::rename;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use strum::{EnumDiscriminants, IntoStaticStr};
use tar::Builder;
use warp::Filter;

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
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
    #[snafu(display("could not create [{path}]"))]
    CreateBundleFailed {
        source: std::io::Error,
        path: String,
    },
    #[snafu(display("could not create bundle tar"))]
    CreateBundleTarFailed { source: std::io::Error },
    #[snafu(display("could not append to bundle tar"))]
    AppendToBundleTarFailed { source: std::io::Error },
}

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}
pub struct Ctx {
    pub active: String,
    pub incoming: String,
    pub tmp: String,
}

const WATCH_NAMESPACE_ENV: &str = "WATCH_NAMESPACE";

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_BUNDLE_BUILDER_LOG");

    let client = client::create_client(Some("opa.stackable.tech".to_string())).await?;

    match env::var(WATCH_NAMESPACE_ENV) {
        Ok(namespace) => {
            create_controller_and_web_server(
                client,
                namespace,
                Ctx {
                    active: "/bundles/active".to_string(),
                    incoming: "/bundles/incoming".to_string(),
                    tmp: "/bundles/tmp".to_string(),
                },
            )
            .await?;
        }
        Err(_) => {
            tracing::error!(
                "Missing namespace to watch. Env var [{}] is probably not defined.",
                WATCH_NAMESPACE_ENV
            );
        }
    }

    Ok(())
}

async fn create_controller_and_web_server(
    client: Client,
    namespace: impl Into<String>,
    ctx: Ctx,
) -> OperatorResult<()> {
    let configmaps_api: Api<ConfigMap> = client.get_namespaced_api(namespace.into().as_ref());

    let controller = Controller::new(
        configmaps_api,
        ListParams::default().labels("opa.stackable.tech/bundle"),
    );

    let web_server = create_web_server(ctx.active.clone(), 3030);

    tokio::select! {
        _ = web_server => Ok(()),
    _ = controller
        .run(
            update_bundle,
            error_policy,
            Context::new(ctx),
        )
        .map(|res| {
            report_controller_reconciled(&client, "openpolicyagents.opa.stackable.tech", &res)
        })
        .collect::<()>() => Ok(()),
    }
}

/// Writes bundle.data under `root`.
async fn update_bundle(
    bundle: Arc<ConfigMap>,
    ctx: Context<Ctx>,
) -> Result<ReconcilerAction, Error> {
    let name = bundle
        .metadata
        .name
        .as_ref()
        .context(OpaBundleHasNoNameSnafu)?;

    match bundle.data.as_ref() {
        Some(rules) => {
            let incoming = ctx.get_ref().incoming.as_str();
            let active = ctx.get_ref().active.as_str();
            let tmp = ctx.get_ref().tmp.as_str();

            let temp_full_path = Path::new(incoming).join(Path::new(name.as_str()));
            create_dir_all(&temp_full_path).with_context(|_| OpaBundleDirSnafu)?;

            for (k, v) in rules.iter() {
                let rego_file_path = temp_full_path.clone().join(Path::new(k));

                File::create(&rego_file_path)
                    .and_then(|mut file| file.write_all(v.as_bytes()))
                    .context(OpaBundleDirSnafu)?;
            }

            let tmp_bundle_path = format!("{}/bundle.tar.gz", tmp);
            let tar_gz =
                File::create(&tmp_bundle_path).with_context(|_| CreateBundleFailedSnafu {
                    path: tmp_bundle_path.to_string(),
                })?;
            let gz_encoder = GzEncoder::new(tar_gz, Compression::best());
            let mut tar_builder = Builder::new(gz_encoder);

            tar_builder
                .append_dir_all("bundles", ctx.get_ref().incoming.as_str())
                .context(AppendToBundleTarFailedSnafu)?;
            tar_builder.finish().context(CreateBundleTarFailedSnafu)?;

            let dest_path = Path::new(active).join(Path::new("bundle.tar.gz"));
            rename(&Path::new(&tmp_bundle_path), &dest_path).context(OpaBundleDirSnafu)?;
        }
        None => tracing::error!("empty config map {}", name),
    }

    Ok(ReconcilerAction {
        requeue_after: None,
    })
}

pub fn error_policy(_error: &Error, _ctx: Context<Ctx>) -> ReconcilerAction {
    ReconcilerAction {
        requeue_after: Some(Duration::from_secs(5)),
    }
}

async fn create_web_server(active: String, port: u16) -> impl Future<Output = ()> {
    let bundle = warp::path!("opa" / "v1" / "opa" / "bundle.tar.gz")
        .and(warp::fs::file(format!("{}/bundle.tar.gz", active)));
    let bundle = bundle.with(warp::log("bundle"));
    warp::serve(bundle).run(([0, 0, 0, 0], port))
}

#[cfg(test)]
mod tests {
    use crate::Ctx;

    use super::update_bundle;

    use std::fs::create_dir;
    use std::fs::metadata;
    use std::sync::Arc;

    use stackable_operator::builder::{ConfigMapBuilder, ObjectMetaBuilder};
    use stackable_operator::kube::runtime::controller::Context;
    use tempdir::TempDir;

    #[test]
    pub fn test_update_bundle() {
        let tmp = TempDir::new("test-bundle-builder").unwrap();
        let active = tmp.path().join("active");
        let incoming = tmp.path().join("incoming");
        let tmp = tmp.path().join("tmp");

        create_dir(&active).unwrap();
        create_dir(&incoming).unwrap();
        create_dir(&tmp).unwrap();

        let config_map = ConfigMapBuilder::new()
            .metadata(ObjectMetaBuilder::new().name("test-bundle-builder").build())
            .add_data(String::from("roles.rego"), String::from("allow user true"))
            .build()
            .unwrap();

        let context = Context::new(Ctx {
            active: String::from(active.to_str().unwrap()),
            incoming: String::from(incoming.to_str().unwrap()),
            tmp: String::from(tmp.to_str().unwrap()),
        });

        match tokio_test::block_on(update_bundle(Arc::new(config_map), context)) {
            Ok(_) => assert!(metadata(active.join("bundle.tar.gz")).unwrap().is_file()),
            Err(e) => panic!("{:?}", e),
        }

        println!("bundle file {}/bundle.tar.gz", active.to_str().unwrap());
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}
