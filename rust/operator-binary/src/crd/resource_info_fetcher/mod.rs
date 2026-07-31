use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::{networking::HostName, tls_verification::TlsClientDetails},
    schemars::{self, JsonSchema},
    v2::types::kubernetes::SecretName,
    versioned::versioned,
};

use crate::crd::cache::Cache;

#[versioned(version(name = "v1alpha1"))]
pub mod versioned {
    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Config {
        /// The backend directory service to use.
        pub backend: Backend,

        /// Caching configuration.
        #[serde(default)]
        pub cache: Cache,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Backend {
        /// Backend that fetches resource information from DataHub.
        DataHub(DataHubBackend),
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DataHubBackend {
        /// Hostname of DataHub
        pub hostname: HostName,

        /// Port of DataHub. If TLS is used defaults to `443`, otherwise to `80`.
        pub port: Option<u16>,

        /// Root HTTP path of DataHub, e.g. `/datahub` if DataHub is served below that path by a
        /// reverse proxy. The GraphQL endpoint is expected at `api/graphql` below this path.
        /// Defaults to `/`.
        #[serde(default = "default_root_path")]
        pub root_path: String,

        /// Use a TLS connection. If not specified then no TLS will be used.
        #[serde(flatten)]
        pub tls: TlsClientDetails,

        /// Name of a Secret containing a DataHub Personal Access Token (PAT) that is authorized
        /// to read resource metadata.
        ///
        /// Must contain the field `token`.
        pub credentials_secret_name: SecretName,

        /// The DataHub environment (in DataHub terms: the fabric) the resources live in.
        ///
        /// This must match the `env` of the DataHub ingestion source that produced the metadata,
        /// because `env` is part of the dataset URN (and of the container key that database and
        /// schema URNs are hashed from). A mismatch is not reported as an error: the constructed URN
        /// simply does not resolve and the resource-info-fetcher returns empty metadata.
        ///
        /// This is configured here rather than passed in by the Rego rule on purpose. `env`
        /// describes how the catalog was populated, not the resource that is being authorized, and
        /// the Rego rule has no way of knowing it. It is also DataHub-specific: no other metadata
        /// catalog we know of has this concept.
        ///
        /// `env` is largely superseded by DataHub's platform instance, which the Rego rules already
        /// pass as `instance` — within a single DataHub, a platform instance belongs to exactly one
        /// fabric. Should a single resource-info-fetcher ever need to serve multiple fabrics, we
        /// would add per-instance `envOverrides` here instead of moving `env` back into the API.
        #[serde(default)]
        pub env: FabricType,
    }

    /// DataHub's `FabricType`: the environments (in DataHub terms: fabrics) a resource can live in.
    ///
    /// The variants serialize exactly the way DataHub spells them, so a value can be copied from an
    /// ingestion recipe or the DataHub UI as-is.
    ///
    /// See the [`FabricType` definition] in DataHub's metadata model.
    ///
    /// [`FabricType` definition]: https://github.com/datahub-project/datahub/blob/master/li-utils/src/main/pegasus/com/linkedin/common/FabricType.pdl
    #[derive(
        Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize, strum::Display,
    )]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    #[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
    pub enum FabricType {
        /// Development fabrics.
        Dev,

        /// Testing fabrics.
        Test,

        /// Quality assurance fabrics.
        Qa,

        /// User acceptance testing fabrics.
        Uat,

        /// Early-integration fabrics.
        Ei,

        /// Pre-production fabrics.
        Pre,

        /// Staging fabrics.
        Stg,

        /// Non-production fabrics.
        NonProd,

        /// Production fabrics. DataHub's ingestion sources default to this fabric, so we do as well.
        #[default]
        Prod,

        /// Corporation fabrics.
        Corp,

        /// Review fabrics.
        Rvw,

        /// Alternative spelling of [`Self::Prod`], which DataHub accepts as well.
        Prd,

        /// Alternative spelling of [`Self::Test`], which DataHub accepts as well.
        Tst,

        /// System integration testing fabrics.
        Sit,

        /// Alternative spelling of [`Self::Sandbox`], which DataHub accepts as well.
        Sbx,

        /// Sandbox fabrics.
        Sandbox,

        /// Certification fabrics.
        Cert,
    }
}

fn default_root_path() -> String {
    "/".to_string()
}
