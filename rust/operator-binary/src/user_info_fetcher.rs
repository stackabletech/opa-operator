use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use stackable_opa_crd::user_info_fetcher as crd;

#[derive(clap::Parser)]
pub struct Args {
    #[clap(long, env)]
    config: PathBuf,
}

#[derive(Clone)]
struct AppState {
    config: Arc<crd::Config>,
    http: reqwest::Client,
}

pub async fn run(args: Args) {
    let config =
        Arc::new(serde_json::from_slice(&tokio::fs::read(args.config).await.unwrap()).unwrap());
    let http = reqwest::Client::default();
    let app = Router::new()
        .route("/user", post(get_user_info))
        .with_state(AppState { config, http });
    axum::Server::bind(&"127.0.0.1:9476".parse().unwrap())
        .serve(app.into_make_service())
        .with_graceful_shutdown(tokio::signal::ctrl_c().map(Result::unwrap))
        .await
        .unwrap();
}

#[derive(Deserialize)]
struct OAuthResponse {
    access_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BriefUserMetadata {
    id: String,
    username: String,
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
}

async fn get_user_info(
    State(state): State<AppState>,
    Json(req): Json<GroupMembershipRequest>,
) -> Json<UserInfo> {
    let AppState { config, http } = state;
    match &config.backend {
        crd::Backend::None {} => Json(UserInfo {
            groups: vec![],
            roles: vec![],
        }),
        crd::Backend::Keycloak(crd::KeycloakBackend {
            url: keycloak_url,
            admin_realm,
            user_realm,
            credentials_secret_name: _,
            client_id,
        }) => {
            let authn = http
                .post(format!(
                    "{keycloak_url}/realms/{admin_realm}/protocol/openid-connect/token"
                ))
                .form(&[
                    ("client_id", &**client_id),
                    ("grant_type", "password"),
                    ("username", "admin"),
                    ("password", "admin"),
                ])
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<OAuthResponse>()
                .await
                .unwrap();
            let users = http
                .get(format!("{keycloak_url}/admin/realms/{user_realm}/users"))
                .query(&[("briefRepresentation", "true"), ("username", &req.username)])
                .bearer_auth(&authn.access_token)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Vec<BriefUserMetadata>>()
                .await
                .unwrap();
            // Search endpoint allows partial match, only allow users that match exactly
            let BriefUserMetadata { id: user_id, .. } = users
                .into_iter()
                .find(|user| user.username == req.username)
                .unwrap();
            let groups = http
                .get(format!(
                    "{keycloak_url}/admin/realms/{user_realm}/users/{user_id}/groups"
                ))
                .bearer_auth(&authn.access_token)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Vec<GroupMembership>>()
                .await
                .unwrap();
            let roles = http
                .get(format!(
                    "{keycloak_url}/admin/realms/{user_realm}/users/{user_id}/role-mappings/realm/composite"
                ))
                .bearer_auth(&authn.access_token)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Vec<RoleMembership>>()
                .await
                .unwrap();
            Json(UserInfo { groups, roles })
        }
    }
}
