use std::path::Path;

use hyper::StatusCode;
use info_fetcher_commons::{
    http_error,
    utils::{self, http::send_json_request},
};
use reqwest::{ClientBuilder, Url};
use serde::Serialize;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1::{self, DataHubBackend};

use crate::{
    api::{
        GetResourceInfoError, ResourceInfoBackend, ResourceInfoRequest,
    },
    backend::data_hub::{resource_to_urn_mapping::urn_for_request, upstream_api::DataHubEntityResponse},
};

mod resource_to_urn_mapping;
mod upstream_api;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to read client ID from {path:?}"))]
    ReadClientId {
        source: std::io::Error,
        path: String,
    },

    #[snafu(display("failed to read client secret from {path:?}"))]
    ReadClientSecret {
        source: std::io::Error,
        path: String,
    },

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to construct HTTP client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to to build DataHub endpoint for {endpoint}"))]
    BuildDataHubEndpoint {
        source: url::ParseError,
        endpoint: String,
    },

    #[snafu(display("failed to query for entity with URN {urn:?}"))]
    QueryForUrn {
        source: utils::http::Error,
        urn: String,
    },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ReadClientId { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ReadClientSecret { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ConfigureTls { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::ConstructHttpClient { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::BuildDataHubEndpoint { .. } => StatusCode::BAD_REQUEST,
            Self::QueryForUrn { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// This struct combines the CRD configuration with credentials loaded from the filesystem.
/// Credentials and the HTTP client are initialized once at startup and stored internally.
pub struct ResolvedDataHubBackend {
    config: v1alpha1::DataHubBackend,
    client_id: String,
    client_secret: String,
    http_client: reqwest::Client,
    // TODO: Think about a cache for tag names?
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataHubResourceInfoResponse {
    owners: Vec<String>,
    tags: Vec<String>,
}

impl ResolvedDataHubBackend {
    /// Resolves a DataHub backend by loading credentials from the filesystem.
    pub async fn resolve(
        config: v1alpha1::DataHubBackend,
        credentials_dir: &Path,
    ) -> Result<Self, Error> {
        let client_id_path = credentials_dir.join("clientId");
        let client_secret_path = credentials_dir.join("clientSecret");

        let client_id = tokio::fs::read_to_string(&client_id_path)
            .await
            .with_context(|_| ReadClientIdSnafu {
                path: client_id_path.display().to_string(),
            })?;
        let client_secret = tokio::fs::read_to_string(&client_secret_path)
            .await
            .with_context(|_| ReadClientSecretSnafu {
                path: client_secret_path.display().to_string(),
            })?;

        let mut client_builder = ClientBuilder::new();
        client_builder = utils::tls::configure_reqwest(&config.tls, client_builder)
            .await
            .context(ConfigureTlsSnafu)?;
        let http_client = client_builder.build().context(ConstructHttpClientSnafu)?;

        Ok(Self {
            config,
            client_id,
            client_secret,
            http_client,
        })
    }
}

impl ResourceInfoBackend for ResolvedDataHubBackend {
    type Response = DataHubResourceInfoResponse;

    async fn get_resource_info(
        &self,
        request: &ResourceInfoRequest,
    ) -> Result<Self::Response, GetResourceInfoError> {
        let urn = urn_for_request(request, &self.config.env);
        let entity_response = self.query_entity(&urn).await?;

        dbg!(&entity_response);
        let tags = entity_response.tag_urns();
        let owners = entity_response.owner_urns();

        Ok(DataHubResourceInfoResponse { tags, owners })
    }
}

impl ResolvedDataHubBackend {
    async fn query_entity(&self, urn: &str) -> Result<DataHubEntityResponse, Error> {
        let Self {
            config:
                DataHubBackend {
                    hostname,
                    port,
                    tls,
                    ..
                },
            client_id,
            client_secret,
            http_client,
        } = &self;

        let schema = if tls.uses_tls() { "https" } else { "http" };
        let port = port.unwrap_or(if tls.uses_tls() { 443 } else { 80 });

        let entity = format!(
            "{schema}://{hostname}:{port}/entitiesV2/{url_encoded_urn}",
            url_encoded_urn = urlencoding::encode(urn)
        );
        let entity_url =
            Url::parse(&entity).with_context(|_| BuildDataHubEndpointSnafu { endpoint: entity })?;

        send_json_request(
            http_client
                .get(entity_url)
                // DataHub's system authenticator strips the leading "Basic " and compares the rest
                // VERBATIM against "<id>:<secret>" — it is NOT standard RFC 7617 Basic auth. So do
                // NOT use reqwest's .basic_auth(), which base64-encodes the credentials; set the
                // header manually so the value goes out unencoded.
                .header(
                    reqwest::header::AUTHORIZATION,
                    format!("Basic {client_id}:{client_secret}"),
                ),
        )
        .await
        .with_context(|_| QueryForUrnSnafu { urn })
    }
}
