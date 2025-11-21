use std::{
    collections::HashMap,
    fmt::Display,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{Json, Router, extract::State, routing::post};
use clap::Parser;
use futures::{FutureExt, future, pin_mut};
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use stackable_operator::{cli::CommonOptions, telemetry::Tracing};
use tokio::net::TcpListener;

mod backend;
mod http_error;
mod utils;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const APP_NAME: &str = "opa-user-info-fetcher";

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
    user_info_cache: Cache<UserInfoRequest, UserInfo>,
}

/// Backend with resolved credentials.
///
/// This enum wraps backend-specific implementations that have already loaded their credentials
/// and initialized their HTTP clients.
enum ResolvedBackend {
    None,
    Keycloak(backend::keycloak::ResolvedKeycloakBackend),
    ExperimentalXfscAas(backend::xfsc_aas::ResolvedXfscAasBackend),
    ActiveDirectory {
        ldap_server: String,
        tls: stackable_operator::commons::tls_verification::TlsClientDetails,
        base_distinguished_name: String,
        custom_attribute_mappings: std::collections::BTreeMap<String, String>,
        additional_group_attribute_filters: std::collections::BTreeMap<String, String>,
    },
    Entra(backend::entra::ResolvedEntraBackend),
    OpenLdap(backend::openldap::ResolvedOpenLdapBackend),
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

    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit {
        source: stackable_operator::telemetry::tracing::Error,
    },

    #[snafu(display("failed to resolve Keycloak backend"))]
    ResolveKeycloakBackend { source: backend::keycloak::Error },

    #[snafu(display("failed to resolve Entra backend"))]
    ResolveEntraBackend { source: backend::entra::Error },

    #[snafu(display("failed to resolve OpenLDAP backend"))]
    ResolveOpenLdapBackend { source: backend::openldap::Error },

    #[snafu(display("failed to resolve XFSC AAS backend"))]
    ResolveXfscAasBackend { source: backend::xfsc_aas::Error },
}

async fn read_config_file(path: &Path) -> Result<String, StartupError> {
    tokio::fs::read_to_string(path)
        .await
        .context(ReadConfigFileSnafu { path })
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
        v1alpha1::Backend::None {} => Ok(ResolvedBackend::None),
        v1alpha1::Backend::Keycloak(config) => {
            let resolved =
                backend::keycloak::ResolvedKeycloakBackend::resolve(config, credentials_dir)
                    .await
                    .context(ResolveKeycloakBackendSnafu)?;
            Ok(ResolvedBackend::Keycloak(resolved))
        }
        v1alpha1::Backend::ExperimentalXfscAas(config) => {
            let resolved = backend::xfsc_aas::ResolvedXfscAasBackend::resolve(config)
                .context(ResolveXfscAasBackendSnafu)?;
            Ok(ResolvedBackend::ExperimentalXfscAas(resolved))
        }
        v1alpha1::Backend::ActiveDirectory(config) => Ok(ResolvedBackend::ActiveDirectory {
            ldap_server: config.ldap_server,
            tls: config.tls,
            base_distinguished_name: config.base_distinguished_name,
            custom_attribute_mappings: config.custom_attribute_mappings,
            additional_group_attribute_filters: config.additional_group_attribute_filters,
        }),
        v1alpha1::Backend::Entra(config) => {
            let resolved = backend::entra::ResolvedEntraBackend::resolve(config, credentials_dir)
                .await
                .context(ResolveEntraBackendSnafu)?;
            Ok(ResolvedBackend::Entra(resolved))
        }
        v1alpha1::Backend::OpenLdap(config) => {
            let resolved = backend::openldap::ResolvedOpenLdapBackend::resolve(config)
                .await
                .context(ResolveOpenLdapBackendSnafu)?;
            Ok(ResolvedBackend::OpenLdap(resolved))
        }
    }
}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    // NOTE (@NickLarsenNZ): Before stackable-telemetry was used:
    // - The console log level was set by `OPA_OPERATOR_LOG`, and is now `CONSOLE_LOG` (when using Tracing::pre_configured).
    // - The file log level was set by `OPA_OPERATOR_LOG`, and is now set via `FILE_LOG` (when using Tracing::pre_configured).
    // - The file log directory was set by `OPA_OPERATOR_LOG_DIRECTORY`, and is now set by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
    let _tracing_guard = Tracing::pre_configured(built_info::PKG_NAME, args.common.telemetry)
        .init()
        .context(TracingInitSnafu)?;

    tracing::info!(
        built_info.pkg_version = built_info::PKG_VERSION,
        built_info.git_version = built_info::GIT_VERSION,
        built_info.target = built_info::TARGET,
        built_info.built_time_utc = built_info::BUILT_TIME_UTC,
        built_info.rustc_version = built_info::RUSTC_VERSION,
        "Starting user-info-fetcher",
    );

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

    let config: v1alpha1::Config =
        serde_json::from_str(&read_config_file(&args.config).await?).context(ParseConfigSnafu)?;

    let backend = Arc::new(resolve_backend(config.backend, &args.credentials_dir).await?);

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
            backend,
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

    #[snafu(display("failed to get user information from Entra"))]
    Entra { source: backend::entra::Error },

    #[snafu(display("failed to get user information from OpenLDAP"))]
    OpenLdap { source: backend::openldap::Error },
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
            Self::Entra { source } => source.status_code(),
            Self::OpenLdap { source } => source.status_code(),
        }
    }
}

async fn get_user_info(
    State(state): State<AppState>,
    Json(req): Json<UserInfoRequest>,
) -> Result<Json<UserInfo>, http_error::JsonResponse<Arc<GetUserInfoError>>> {
    let AppState {
        backend,
        user_info_cache,
    } = state;
    Ok(Json(
        user_info_cache
            .try_get_with_by_ref(&req, async {
                match backend.as_ref() {
                    ResolvedBackend::None => {
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
                    ResolvedBackend::Keycloak(keycloak) => keycloak
                        .get_user_info(&req)
                        .await
                        .context(get_user_info_error::KeycloakSnafu),
                    ResolvedBackend::ExperimentalXfscAas(aas) => aas
                        .get_user_info(&req)
                        .await
                        .context(get_user_info_error::ExperimentalXfscAasSnafu),
                    ResolvedBackend::ActiveDirectory {
                        ldap_server,
                        tls,
                        base_distinguished_name,
                        custom_attribute_mappings,
                        additional_group_attribute_filters,
                    } => backend::active_directory::get_user_info(
                        &req,
                        ldap_server,
                        tls,
                        base_distinguished_name,
                        custom_attribute_mappings,
                        additional_group_attribute_filters,
                    )
                    .await
                    .context(get_user_info_error::ActiveDirectorySnafu),
                    ResolvedBackend::Entra(entra) => entra
                        .get_user_info(&req)
                        .await
                        .context(get_user_info_error::EntraSnafu),
                    ResolvedBackend::OpenLdap(openldap) => openldap
                        .get_user_info(&req)
                        .await
                        .context(get_user_info_error::OpenLdapSnafu),
                }
            })
            .await?,
    ))
}
