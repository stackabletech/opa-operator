mod http_error;

use std::{
    collections::HashMap,
    net::AddrParseError,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use futures::FutureExt;
use hyper::StatusCode;
use reqwest::RequestBuilder;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;

pub const APP_NAME: &str = "opa-user-info-fetcher";

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
    config: Arc<crd::Config>,
    http: reqwest::Client,
    credentials: Arc<Credentials>,
}

struct Credentials {
    username: String,
    password: String,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("unable to read config file from {path:?}"))]
    ReadConfigFile {
        source: std::io::Error,
        path: PathBuf,
    },
    #[snafu(display("unable to parse config file"))]
    ParseConfig { source: serde_json::Error },
    #[snafu(display("failed to parse listen address"))]
    ParseListenAddr { source: AddrParseError },
    #[snafu(display("failed to run server"))]
    RunServer { source: hyper::Error },
}

async fn read_config_file(path: &Path) -> Result<String, StartupError> {
    tokio::fs::read_to_string(path)
        .await
        .context(ReadConfigFileSnafu { path })
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    stackable_operator::logging::initialize_logging(
        "OPA_OPERATOR_LOG",
        APP_NAME,
        args.common.tracing_target,
    );

    let config = Arc::new(
        serde_json::from_str(&read_config_file(&args.config).await?).context(ParseConfigSnafu)?,
    );
    let credentials = Arc::new(Credentials {
        username: read_config_file(&args.credentials_dir.join("username")).await?,
        password: read_config_file(&args.credentials_dir.join("password")).await?,
    });
    let http = reqwest::Client::default();
    let app = Router::new()
        .route("/user", post(get_user_info))
        .with_state(AppState {
            config,
            http,
            credentials,
        });
    axum::Server::bind(&"127.0.0.1:9476".parse().context(ParseListenAddrSnafu)?)
        .serve(app.into_make_service())
        .with_graceful_shutdown(tokio::signal::ctrl_c().map(Result::unwrap))
        .await
        .context(RunServerSnafu)
}

#[derive(Deserialize)]
struct OAuthResponse {
    access_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserMetadata {
    id: String,
    #[serde(default)]
    attributes: HashMap<String, Vec<String>>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembership {
    path: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleMembership {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembershipRequest {
    username: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    groups: Vec<GroupMembership>,
    roles: Vec<RoleMembership>,
    custom_attributes: HashMap<String, String>,
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum GetUserInfoError {
    #[snafu(display("failed to get user information from Keycloak"))]
    Keycloak { source: KeycloakError },
}
impl http_error::Error for GetUserInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        match self {
            Self::Keycloak { source } => source.status_code(),
        }
    }
}

async fn send_json_request<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, reqwest::Error> {
    req.send().await?.error_for_status()?.json().await
}

async fn get_user_info(
    State(state): State<AppState>,
    Json(req): Json<GroupMembershipRequest>,
) -> Result<Json<UserInfo>, http_error::JsonResponse<GetUserInfoError>> {
    let AppState {
        config,
        http,
        credentials,
    } = state;
    Ok(Json(match &config.backend {
        crd::Backend::None {} => UserInfo {
            groups: vec![],
            roles: vec![],
            custom_attributes: HashMap::new(),
        },
        crd::Backend::Keycloak(keycloak) => {
            keycloak_get_user_info(req, &http, &credentials, keycloak)
                .await
                .context(get_user_info_error::KeycloakSnafu)?
        }
    }))
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum KeycloakError {
    #[snafu(display("unable to log in (expired credentials?)"))]
    LogIn { source: reqwest::Error },
    #[snafu(display("unable to search for user"))]
    SearchForUser { source: reqwest::Error },
    #[snafu(display("user {username:?} was not found"))]
    UserNotFound { username: String },
    #[snafu(display("unable to request groups for user"))]
    RequestUserGroups { source: reqwest::Error },
    #[snafu(display("unable to request roles for user"))]
    RequestUserRoles { source: reqwest::Error },
}
impl http_error::Error for KeycloakError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::LogIn { .. } => StatusCode::BAD_GATEWAY,
            Self::SearchForUser { .. } => StatusCode::BAD_GATEWAY,
            Self::UserNotFound { .. } => StatusCode::NOT_FOUND,
            Self::RequestUserGroups { .. } => StatusCode::BAD_GATEWAY,
            Self::RequestUserRoles { .. } => StatusCode::BAD_GATEWAY,
        }
    }
}

async fn keycloak_get_user_info(
    req: GroupMembershipRequest,
    http: &reqwest::Client,
    credentials: &Credentials,
    config: &crd::KeycloakBackend,
) -> Result<UserInfo, KeycloakError> {
    use keycloak_error::*;
    let crd::KeycloakBackend {
        url: keycloak_url,
        admin_realm,
        user_realm,
        credentials_secret_name: _,
        client_id,
    } = config;
    let user_realm_url = format!("{keycloak_url}/admin/realms/{user_realm}");
    let authn = send_json_request::<OAuthResponse>(
        http.post(format!(
            "{keycloak_url}/realms/{admin_realm}/protocol/openid-connect/token"
        ))
        .form(&[
            ("grant_type", "password"),
            ("client_id", client_id),
            ("username", &credentials.username),
            ("password", &credentials.password),
        ]),
    )
    .await
    .context(LogInSnafu)?;
    let users = send_json_request::<Vec<UserMetadata>>(
        http.get(format!("{user_realm_url}/users"))
            .query(&[("exact", "true"), ("username", &req.username)])
            .bearer_auth(&authn.access_token),
    )
    .await
    .context(SearchForUserSnafu)?;
    let user = users.into_iter().next().context(UserNotFoundSnafu {
        username: req.username,
    })?;
    let user_id = &user.id;
    let groups = send_json_request::<Vec<GroupMembership>>(
        http.get(format!("{user_realm_url}/users/{user_id}/groups"))
            .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserGroupsSnafu)?;
    let roles = send_json_request::<Vec<RoleMembership>>(
        http.get(format!(
            "{user_realm_url}/users/{user_id}/role-mappings/realm/composite"
        ))
        .bearer_auth(&authn.access_token),
    )
    .await
    .context(RequestUserRolesSnafu)?;
    Ok(UserInfo {
        groups,
        roles,
        custom_attributes: user
            .attributes
            .into_iter()
            // FIXME: why does keycloak support multiple values? do we need to support this? doesn't seem to be exposed in gui
            .filter_map(|(k, v)| Some((k, v.into_iter().next()?)))
            .collect(),
    })
}
