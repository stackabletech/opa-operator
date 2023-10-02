use serde::{Deserialize, Serialize};
use stackable_operator::schemars::{self, JsonSchema};

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub backend: Backend,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Backend {
    None {},
    Keycloak(KeycloakBackend),
}

impl Default for Backend {
    fn default() -> Self {
        Self::None {}
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeycloakBackend {
    pub url: String,
    pub credentials_secret_name: String,
    pub admin_realm: String,
    pub user_realm: String,
    pub client_id: String,
}
