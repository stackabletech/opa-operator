use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{FromRequestParts, Query, State},
    http::request::Parts,
    routing::get,
};
use clap::Parser;
use futures::{FutureExt, future};
use hyper::StatusCode;
use info_fetcher_commons::{
    config::{ConfigError, read_config_file},
    http_error,
};
use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1::{self};
use stackable_operator::{cli::CommonOptions, telemetry::Tracing};
use tokio::net::TcpListener;

use crate::{
    api::{GetResourceInfoError, ResourceInfoBackend, ResourceInfoRequest},
    cache::{CachedResponse, ResourceInfoCache},
};

mod api;
mod backend;
mod cache;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const APP_NAME: &str = "opa-resource-info-fetcher";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(flatten)]
    common: CommonOptions,

    #[clap(long, env)]
    config: PathBuf,

    #[clap(long, env)]
    credentials_dir: PathBuf,
}

#[derive(Clone)]
struct AppState {
    backend: Arc<ResolvedBackend>,
    // Note: Although we might not talk JSON to the underlying backend, we always return JSON as a
    // result to the caller, so we can cache that.
    resource_info_cache: ResourceInfoCache,
}

/// Backend with resolved credentials.
///
/// This enum wraps backend-specific implementations that have already loaded their credentials
/// and initialized their HTTP clients.
enum ResolvedBackend {
    DataHub(backend::data_hub::ResolvedDataHubBackend),
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit {
        source: stackable_operator::telemetry::tracing::Error,
    },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("unable to parse config file from {path:?}"))]
    ParseConfigFile { source: ConfigError, path: PathBuf },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to resolve DataHub backend"))]
    ResolveDataHubBackend {
        source: backend::data_hub::ResolveError,
    },
}

/// Resolves a backend configuration by loading credentials and creating the appropriate backend implementation.
///
/// This function reads credentials from the filesystem once at startup and returns a backend that
/// contains both the configuration and the resolved credentials.
async fn resolve_backend(
    backend: v1alpha1::Backend,
    credentials_dir: &Path,
) -> Result<ResolvedBackend, StartupError> {
    match backend {
        v1alpha1::Backend::DataHub(config) => {
            let resolved =
                backend::data_hub::ResolvedDataHubBackend::resolve(config, credentials_dir)
                    .await
                    .context(ResolveDataHubBackendSnafu)?;
            Ok(ResolvedBackend::DataHub(resolved))
        }
    }
}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    let _tracing_guard = Tracing::pre_configured(built_info::PKG_NAME, args.common.telemetry)
        .init()
        .context(TracingInitSnafu)?;

    tracing::info!(
        built_info.pkg_version = built_info::PKG_VERSION,
        built_info.git_version = built_info::GIT_VERSION,
        built_info.target = built_info::TARGET,
        built_info.built_time_utc = built_info::BUILT_TIME_UTC,
        built_info.rustc_version = built_info::RUSTC_VERSION,
        "Starting resource-info-fetcher",
    );

    let shutdown_requested = tokio::signal::ctrl_c().map(|_| ());
    #[cfg(unix)]
    let shutdown_requested = {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context(RegisterSigtermSnafu)?;
        async move {
            use futures::pin_mut;

            let sigterm = sigterm.recv().map(|_| ());
            pin_mut!(shutdown_requested, sigterm);
            future::select(shutdown_requested, sigterm).await;
        }
    };

    let config: v1alpha1::Config = read_config_file(&args.config)
        .with_context(|_| ParseConfigFileSnafu { path: args.config })?;
    let backend = Arc::new(resolve_backend(config.backend, &args.credentials_dir).await?);
    let resource_info_cache = cache::build(&config.cache);
    // One GET endpoint per resource type. They all share the same generic `metadata` handler; only
    // the query-parameter struct (and thus the resulting `ResourceInfoRequest` variant) differs.
    let app = Router::new()
        .route("/metadata/database", get(metadata::<api::Database>))
        .route("/metadata/schema", get(metadata::<api::Schema>))
        .route("/metadata/table", get(metadata::<api::Table>))
        .route("/metadata/stream", get(metadata::<api::Stream>))
        .route("/metadata/dashboard", get(metadata::<api::Dashboard>))
        .route("/metadata/chart", get(metadata::<api::Chart>))
        .route(
            "/metadata/rawIdentifier",
            get(metadata::<api::RawIdentifier>),
        )
        .with_state(AppState {
            backend,
            resource_info_cache,
        });
    let listener = TcpListener::bind("127.0.0.1:9477")
        .await
        .context(BindListenerSnafu)?;

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)?;
    Ok(())
}

/// The single handler backing every `GET /metadata/<type>` route. It deserializes the request's
/// query parameters into the per-type params struct `P`, converts them into a [`ResourceInfoRequest`]
/// and delegates to [`get_resource_info`]. Adding a resource type is therefore just a new params
/// struct (in [`crate::api`]) plus one route line — there is no per-type handler body.
async fn metadata<P>(
    State(state): State<AppState>,
    MetadataQuery(params): MetadataQuery<P>,
) -> Result<Json<serde_json::Value>, http_error::JsonResponse<Arc<GetResourceInfoError>>>
where
    P: DeserializeOwned + Into<ResourceInfoRequest>,
{
    get_resource_info(state, params.into()).await
}

/// Like [`axum::extract::Query`], but renders deserialization failures (a missing or malformed query
/// parameter) through the same JSON error envelope as the rest of the API instead of axum's default
/// plain-text `400` response.
struct MetadataQuery<P>(P);

impl<P, S> FromRequestParts<S> for MetadataQuery<P>
where
    P: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = http_error::JsonResponse<InvalidQueryParameters>;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(params) =
            Query::<P>::from_request_parts(parts, state)
                .await
                .map_err(|rejection| InvalidQueryParameters {
                    message: rejection.body_text(),
                })?;
        Ok(Self(params))
    }
}

/// Returned when a request's query parameters cannot be deserialized into the endpoint's params
/// struct, e.g. a required parameter is missing or a numeric one is malformed.
#[derive(Snafu, Debug)]
#[snafu(display("invalid query parameters: {message}"))]
struct InvalidQueryParameters {
    message: String,
}

impl http_error::Error for InvalidQueryParameters {
    fn status_code(&self) -> StatusCode {
        StatusCode::BAD_REQUEST
    }
}

/// Shared request handling: serve the resource info from cache, or query the backend and cache the
/// resulting JSON. Independent of how the request was parsed, so all endpoints reuse it.
async fn get_resource_info(
    state: AppState,
    request: ResourceInfoRequest,
) -> Result<Json<serde_json::Value>, http_error::JsonResponse<Arc<GetResourceInfoError>>> {
    let AppState {
        backend,
        resource_info_cache,
    } = state;
    // A failed lookup is cached as well, see [`CachedResponse`], so nothing fails here: the error is
    // part of the cached value rather than something the cache passes through.
    let cached = resource_info_cache
        .get_with_by_ref(&request, async {
            match fetch_resource_info(&backend, &request).await {
                Ok(resource_info) => CachedResponse::Found(resource_info),
                Err(error) => CachedResponse::Failed(Arc::new(error)),
            }
        })
        .await;

    match cached {
        CachedResponse::Found(resource_info) => Ok(Json(resource_info)),
        CachedResponse::Failed(error) => Err(error.into()),
    }
}

/// Queries the backend for a single resource and serializes the answer into the JSON we return.
async fn fetch_resource_info(
    backend: &ResolvedBackend,
    request: &ResourceInfoRequest,
) -> Result<serde_json::Value, GetResourceInfoError> {
    match backend {
        ResolvedBackend::DataHub(data_hub) => {
            let response = data_hub.get_resource_info(request).await?;

            serde_json::to_value(&response)
                .map_err(|source| GetResourceInfoError::SerializeResponseAsJson { source })
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::api::RawIdentifier;

    /// State whose backend queries `mock_server`, with the cache a cluster gets by default.
    fn state_for(mock_server: &MockServer) -> AppState {
        let graphql_url = format!("{}/api/graphql", mock_server.uri())
            .parse()
            .expect("the mock server's address must be a valid URL");

        AppState {
            backend: Arc::new(ResolvedBackend::DataHub(
                backend::data_hub::ResolvedDataHubBackend::for_tests(graphql_url),
            )),
            resource_info_cache: cache::build(&Default::default()),
        }
    }

    fn request() -> ResourceInfoRequest {
        ResourceInfoRequest::RawIdentifier(RawIdentifier {
            identifier: "urn:li:dataset:(urn:li:dataPlatform:trino,broken,PROD)".into(),
        })
    }

    /// A lookup that keeps failing must not query the backend again on every request. Anyone who can
    /// name a resource could otherwise amplify their requests against DataHub, and worst of all
    /// exactly while DataHub is already unwell.
    #[tokio::test]
    async fn a_failing_lookup_queries_the_backend_only_once() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .respond_with(ResponseTemplate::new(500).set_body_string("GMS is having a bad day"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let state = state_for(&mock_server);
        for _ in 0..5 {
            let result = get_resource_info(state.clone(), request()).await;
            assert!(result.is_err(), "the lookup must keep failing");
        }

        // The mock's `.expect(1)` is verified when `mock_server` is dropped.
    }
}
