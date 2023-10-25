use std::{
    net::AddrParseError,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{extract::State, response::IntoResponse, routing::get, Router};
use clap::Parser;
use flate2::write::GzEncoder;
use futures::{
    future::{self, BoxFuture},
    pin_mut, FutureExt, StreamExt,
};
use snafu::{futures::TryFutureExt as _, ResultExt, Snafu};
use stackable_operator::{
    k8s_openapi::api::core::v1::ConfigMap,
    kube::{
        self,
        runtime::{
            reflector::{self, ObjectRef, Store},
            watcher,
        },
    },
};
use tracing::{error, info};

const OPERATOR_NAME: &str = "opa.stackable.tech";
pub const APP_NAME: &str = "opa-bundle-builder";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(flatten)]
    common: stackable_operator::cli::ProductOperatorRun,
}

#[derive(Clone)]
struct AppState {
    bundle: Arc<Mutex<future::Shared<BoxFuture<'static, Vec<u8>>>>>,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("unable to read config file from {path:?}"))]
    ReadConfigFile {
        source: std::io::Error,
        path: PathBuf,
    },
    #[snafu(display("failed to parse listen address"))]
    ParseListenAddr { source: AddrParseError },
    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },
    #[snafu(display("failed to run server"))]
    RunServer { source: hyper::Error },
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    stackable_operator::logging::initialize_logging(
        "OPA_OPERATOR_LOG",
        APP_NAME,
        args.common.tracing_target,
    );

    let kube = kube::Client::try_default().await.unwrap();

    let (store, store_w) = reflector::store();
    let bundle = Arc::new(Mutex::new(build_bundle(store.clone()).boxed().shared()));
    let reflector = std::pin::pin!(reflector::reflector(
        store_w,
        watcher(
            kube::Api::<ConfigMap>::default_namespaced(kube),
            watcher::Config::default().labels(&format!("{OPERATOR_NAME}/bundle")),
        ),
    )
    .for_each(|ev| async {
        match ev {
            Ok(watcher::Event::Applied(o)) => {
                info!(object = %ObjectRef::from_obj(&o), "saw updated object")
            }
            Ok(watcher::Event::Deleted(o)) => {
                info!(object = %ObjectRef::from_obj(&o), "saw deleted object")
            }
            Ok(watcher::Event::Restarted(os)) => {
                let objects = os
                    .iter()
                    .map(ObjectRef::from_obj)
                    .map(|o| o.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                info!(objects, "restarted reflector")
            }
            Err(error) => {
                error!(
                    error = &error as &dyn std::error::Error,
                    "failed to update reflector"
                )
            }
        }
        *bundle.lock().unwrap() = build_bundle(store.clone()).boxed().shared();
    })
    .map(Ok));

    let shutdown_requested = tokio::signal::ctrl_c().map(|_| ());
    #[cfg(unix)]
    let shutdown_requested = {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context(RegisterSigtermSnafu)?;
        async move {
            let sigterm = sigterm.recv().map(|_| ());
            pin_mut!(shutdown_requested, sigterm);
            future::select(shutdown_requested, sigterm).await;
        }
    };

    let app = Router::new()
        .route("/opa/v1/opa/bundle.tar.gz", get(get_bundle))
        .with_state(AppState {
            bundle: bundle.clone(),
        });
    let server = std::pin::pin!(axum::Server::bind(
        &"127.0.0.1:9477".parse().context(ParseListenAddrSnafu)?
    )
    .serve(app.into_make_service())
    .with_graceful_shutdown(shutdown_requested)
    .context(RunServerSnafu));

    future::select(reflector, server).await.factor_first().0
}

async fn build_bundle(store: Store<ConfigMap>) -> Vec<u8> {
    fn file_header(data: &[u8]) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len().try_into().unwrap());
        header.set_entry_type(tar::EntryType::Regular);
        header
    }

    info!("building bundle");
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), flate2::Compression::default()));
    for cm in store.state() {
        let cm_name = cm.metadata.name.as_deref().unwrap();
        for (file_name, data) in cm.data.iter().flatten() {
            let mut header = file_header(data.as_bytes());
            tar.append_data(
                &mut header,
                format!("configmap/{cm_name}/{file_name}"),
                data.as_bytes(),
            )
            .unwrap();
        }
    }
    let tar = tar.into_inner().unwrap().finish().unwrap();
    info!("finished building bundle");
    tar
}

async fn get_bundle(State(state): State<AppState>) -> impl IntoResponse {
    let bundle = future::Shared::clone(&*state.bundle.lock().unwrap());
    (
        [(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/gzip"),
        )],
        bundle.await,
    )
}
