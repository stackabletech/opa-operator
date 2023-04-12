use std::collections::HashMap;

use axum::{
    extract::Query,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::{Deserialize, Serialize};

pub async fn run() {
    let app = Router::new().route("/user", post(get_user_info));
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembershipRequest {
    username: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    groups: Vec<GroupMembership>,
}

async fn get_user_info(Json(req): Json<GroupMembershipRequest>) -> Json<UserInfo> {
    let http = reqwest::Client::default();
    const KEYCLOAK: &str = "http://192.168.122.1:8080";
    const ADMIN_REALM: &str = "master";
    const REALM: &str = "master";
    let authn = http
        .post(format!(
            "{KEYCLOAK}/realms/{ADMIN_REALM}/protocol/openid-connect/token"
        ))
        .form(&[
            ("client_id", "admin-cli"),
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
        .get(format!("{KEYCLOAK}/admin/realms/{REALM}/users"))
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
            "{KEYCLOAK}/admin/realms/{REALM}/users/{user_id}/groups"
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
    Json(UserInfo { groups })
}
