use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::{networking::HostName, tls_verification::TlsClientDetails},
    schemars::{self, JsonSchema},
    time::Duration,
};
use stackable_versioned::versioned;

#[versioned(version(name = "v1alpha1"))]
pub mod versioned {
    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Config {
        /// The backend directory service to use.
        #[serde(default)]
        pub backend: v1alpha1::Backend,

        /// Caching configuration.
        #[serde(default)]
        pub cache: v1alpha1::Cache,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Backend {
        /// Dummy backend that adds no extra user information.
        None {},

        /// Backend that fetches user information from Keycloak.
        Keycloak(v1alpha1::KeycloakBackend),

        /// Backend that fetches user information from the Gaia-X
        /// Cross Federation Services Components (XFSC) Authentication & Authorization Service.
        ExperimentalXfscAas(v1alpha1::AasBackend),

        /// Backend that fetches user information from Active Directory
        #[serde(rename = "experimentalActiveDirectory")]
        ActiveDirectory(v1alpha1::ActiveDirectoryBackend),
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct KeycloakBackend {
        /// Hostname of the identity provider, e.g. `my.keycloak.corp`.
        pub hostname: HostName,

        /// Port of the identity provider. If TLS is used defaults to `443`, otherwise to `80`.
        pub port: Option<u16>,

        /// Root HTTP path of the identity provider. Defaults to `/`.
        #[serde(default = "default_root_path")]
        pub root_path: String,

        /// Use a TLS connection. If not specified no TLS will be used.
        #[serde(flatten)]
        pub tls: TlsClientDetails,

        /// Name of a Secret that contains client credentials of a Keycloak account with permission to read user metadata.
        ///
        /// Must contain the fields `clientId` and `clientSecret`.
        pub client_credentials_secret: String,

        /// The Keycloak realm that OPA's Keycloak account (as specified by `credentialsSecretName` exists in).
        ///
        /// Typically `master`.
        pub admin_realm: String,

        /// The Keycloak realm that user metadata should be resolved from.
        pub user_realm: String,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AasBackend {
        /// Hostname of the identity provider, e.g. `my.aas.corp`.
        pub hostname: String,

        /// Port of the identity provider. Defaults to port 5000.
        #[serde(default = "aas_default_port")]
        pub port: u16,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ActiveDirectoryBackend {
        /// Hostname of the domain controller, e.g. `ad-ds-1.contoso.com`.
        pub ldap_server: String,

        /// The root Distinguished Name (DN) where users and groups are located.
        pub base_distinguished_name: String,

        /// The name of the Kerberos SecretClass.
        pub kerberos_secret_class_name: String,

        /// Use a TLS connection. If not specified then no TLS will be used.
        #[serde(flatten)]
        pub tls: TlsClientDetails,

        /// Custom attributes, and their LDAP attribute names.
        #[serde(default)]
        pub custom_attribute_mappings: BTreeMap<String, String>,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize, Derivative)]
    #[serde(rename_all = "camelCase")]
    pub struct Cache {
        /// How long metadata about each user should be cached for.
        #[serde(default = "v1alpha1::Cache::default_entry_time_to_live")]
        pub entry_time_to_live: Duration,
    }
}

impl Default for v1alpha1::Backend {
    fn default() -> Self {
        Self::None {}
    }
}

fn default_root_path() -> String {
    "/".to_string()
}

fn aas_default_port() -> u16 {
    5000
}

impl v1alpha1::Cache {
    const fn default_entry_time_to_live() -> Duration {
        Duration::from_minutes_unchecked(1)
    }
}

impl Default for v1alpha1::Cache {
    fn default() -> Self {
        Self {
            entry_time_to_live: Self::default_entry_time_to_live(),
        }
    }
}
