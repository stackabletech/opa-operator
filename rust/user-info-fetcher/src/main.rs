use std::{
    collections::HashMap,
    fmt::Display,
    ops::Deref as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{Json, Router, extract::State, routing::post};
use clap::Parser;
use futures::{FutureExt, future, pin_mut};
use moka::future::Cache;
use reqwest::ClientBuilder;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use stackable_operator::cli::RollingPeriod;
use stackable_telemetry::{Tracing, tracing::settings::Settings};
use tokio::net::TcpListener;
use tracing::level_filters::LevelFilter;

mod backend;
mod http_error;
mod utils;

pub const APP_NAME: &str = "opa-user-info-fetcher";

// TODO (@NickLarsenNZ): Change the variable to `CONSOLE_LOG`
pub const ENV_VAR_CONSOLE_LOG: &str = "OPA_OPERATOR_LOG";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(long, env)]
    config: PathBuf,
    #[clap(long, env)]
    credentials_dir: PathBuf,
    #[clap(flatten)]
    common: stackable_operator::cli::ProductOperatorRun,
}

#[derive(Clone)]
struct AppState {
    config: Arc<v1alpha1::Config>,
    http: reqwest::Client,
    credentials: Arc<Credentials>,
    user_info_cache: Cache<UserInfoRequest, UserInfo>,
}

struct Credentials {
    // TODO: Find a better way of sharing behavior between different backends
    client_id: String,
    client_secret: String,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("unable to read config file from {path:?}"))]
    ReadConfigFile {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("failed to parse config file"))]
    ParseConfig { source: serde_json::Error },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to construct http client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit {
        source: stackable_telemetry::tracing::Error,
    },
}

async fn read_config_file(path: &Path) -> Result<String, StartupError> {
    tokio::fs::read_to_string(path)
        .await
        .context(ReadConfigFileSnafu { path })
}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    let _tracing_guard = Tracing::builder()
        .service_name("user-info-fetcher")
        .with_console_output((
            ENV_VAR_CONSOLE_LOG,
            LevelFilter::INFO,
            !args.common.telemetry_arguments.no_console_output,
        ))
        // NOTE (@NickLarsenNZ): Before stackable-telemetry was used, the log directory was
        // set via an env: `OPA_OPERATOR_LOG_DIRECTORY`.
        // See: https://github.com/stackabletech/operator-rs/blob/f035997fca85a54238c8de895389cc50b4d421e2/crates/stackable-operator/src/logging/mod.rs#L40
        // Now it will be `ROLLING_LOGS` (or via `--rolling-logs <DIRECTORY>`).
        .with_file_output(
            args.common
                .telemetry_arguments
                .rolling_logs
                .map(|log_directory| {
                    let rotation_period = args
                        .common
                        .telemetry_arguments
                        .rolling_logs_period
                        .unwrap_or(RollingPeriod::Never)
                        .deref()
                        .clone();

                    Settings::builder()
                        .with_environment_variable(ENV_VAR_CONSOLE_LOG)
                        .with_default_level(LevelFilter::INFO)
                        .file_log_settings_builder(log_directory, "tracing-rs.log")
                        .with_rotation_period(rotation_period)
                        .build()
                }),
        )
        .with_otlp_log_exporter((
            "OTLP_LOG",
            LevelFilter::DEBUG,
            args.common.telemetry_arguments.otlp_logs,
        ))
        .with_otlp_trace_exporter((
            "OTLP_TRACE",
            LevelFilter::DEBUG,
            args.common.telemetry_arguments.otlp_traces,
        ))
        .build()
        .init()
        .context(TracingInitSnafu)?;

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

    let config = Arc::<v1alpha1::Config>::new(
        serde_json::from_str(&read_config_file(&args.config).await?).context(ParseConfigSnafu)?,
    );
    let credentials = Arc::new(match &config.backend {
        // TODO: factor this out into each backend (e.g. when we add LDAP support)
        v1alpha1::Backend::None {} => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        v1alpha1::Backend::Keycloak(_) => Credentials {
            client_id: read_config_file(&args.credentials_dir.join("clientId")).await?,
            client_secret: read_config_file(&args.credentials_dir.join("clientSecret")).await?,
        },
        v1alpha1::Backend::ExperimentalXfscAas(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        v1alpha1::Backend::ActiveDirectory(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
    });

    let mut client_builder = ClientBuilder::new();

    // TODO: I'm not so sure we should be doing all this keycloak specific stuff here.
    // We could factor it out in the provider specific implementation (e.g. when we add LDAP support).
    // I know it is for setting up the client, but an idea: make a trait for implementing backends
    // The trait can do all this for a genric client using an implementation on the trait (eg: get_http_client() which will call self.uses_tls())
    if let v1alpha1::Backend::Keycloak(keycloak) = &config.backend {
        client_builder = utils::tls::configure_reqwest(&keycloak.tls, client_builder)
            .await
            .context(ConfigureTlsSnafu)?;
    }
    let http = client_builder.build().context(ConstructHttpClientSnafu)?;

    let user_info_cache = {
        let v1alpha1::Cache { entry_time_to_live } = config.cache;
        Cache::builder()
            .name("user-info")
            .time_to_live(*entry_time_to_live)
            .build()
    };
    let app = Router::new()
        .route("/user", post(get_user_info))
        .with_state(AppState {
            config,
            http,
            credentials,
            user_info_cache,
        });
    let listener = TcpListener::bind("127.0.0.1:9476")
        .await
        .context(BindListenerSnafu)?;

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase", untagged)]
enum UserInfoRequest {
    UserInfoRequestById(UserInfoRequestById),
    UserInfoRequestByName(UserInfoRequestByName),
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct UserInfoRequestById {
    id: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct UserInfoRequestByName {
    username: String,
}

/// Renders [`UserInfoRequest`] for use in error messages.
///
/// An independent type rather than an impl on [`UserInfoRequest`], since it is
/// not suitable for use in other contexts.
#[derive(Debug, Clone)]
struct ErrorRenderUserInfoRequest(UserInfoRequest);
impl Display for ErrorRenderUserInfoRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            UserInfoRequest::UserInfoRequestById(UserInfoRequestById { id }) => {
                write!(f, "with id {id:?}")
            }
            UserInfoRequest::UserInfoRequestByName(UserInfoRequestByName { username }) => {
                write!(f, "with username {username:?}")
            }
        }
    }
}
impl From<&UserInfoRequest> for ErrorRenderUserInfoRequest {
    fn from(value: &UserInfoRequest) -> Self {
        Self(value.clone())
    }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    /// This might be null in case the id is not known (e.g. the backend does not have this info).
    id: Option<String>,
    /// This might be null in case the username is not known (e.g. the backend does not have this info).
    username: Option<String>,
    groups: Vec<String>,
    custom_attributes: HashMap<String, serde_json::Value>,
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum GetUserInfoError {
    #[snafu(display("failed to get user information from Keycloak"))]
    Keycloak { source: backend::keycloak::Error },

    #[snafu(display(
        "failed to get user information from the XFSC Authentication & Authorization Service"
    ))]
    ExperimentalXfscAas { source: backend::xfsc_aas::Error },

    #[snafu(display("failed to get user information from Active Directory"))]
    ActiveDirectory {
        source: backend::active_directory::Error,
    },
}

impl http_error::Error for GetUserInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        // todo: the warn here loses context about the scope in which the error occurred, eg: stackable_opa_user_info_fetcher::backend::keycloak
        // Also, we should make the log level (warn vs error) more dynamic in the backend's impl `http_error::Error for Error`
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::Keycloak { source } => source.status_code(),
            Self::ExperimentalXfscAas { source } => source.status_code(),
            Self::ActiveDirectory { source } => source.status_code(),
        }
    }
}

async fn get_user_info(
    State(state): State<AppState>,
    Json(req): Json<UserInfoRequest>,
) -> Result<Json<UserInfo>, http_error::JsonResponse<Arc<GetUserInfoError>>> {
    let AppState {
        config,
        http,
        credentials,
        user_info_cache,
    } = state;
    Ok(Json(
        user_info_cache
            .try_get_with_by_ref(&req, async {
                match &config.backend {
                    v1alpha1::Backend::None {} => {
                        let user_id = match &req {
                            UserInfoRequest::UserInfoRequestById(UserInfoRequestById { id }) => {
                                Some(id)
                            }
                            _ => None,
                        };
                        let username = match &req {
                            UserInfoRequest::UserInfoRequestByName(UserInfoRequestByName {
                                username,
                            }) => Some(username),
                            _ => None,
                        };
                        Ok(UserInfo {
                            id: user_id.cloned(),
                            username: username.cloned(),
                            groups: vec![],
                            custom_attributes: HashMap::new(),
                        })
                    }
                    v1alpha1::Backend::Keycloak(keycloak) => {
                        backend::keycloak::get_user_info(&req, &http, &credentials, keycloak)
                            .await
                            .context(get_user_info_error::KeycloakSnafu)
                    }
                    v1alpha1::Backend::ExperimentalXfscAas(aas) => {
                        backend::xfsc_aas::get_user_info(&req, &http, aas)
                            .await
                            .context(get_user_info_error::ExperimentalXfscAasSnafu)
                    }
                    v1alpha1::Backend::ActiveDirectory(ad) => {
                        backend::active_directory::get_user_info(
                            &req,
                            &ad.ldap_server,
                            &ad.tls,
                            &ad.base_distinguished_name,
                            &ad.custom_attribute_mappings,
                            &ad.additional_group_attribute_filters,
                        )
                        .await
                        .context(get_user_info_error::ActiveDirectorySnafu)
                    }
                }
            })
            .await?,
    ))
}
