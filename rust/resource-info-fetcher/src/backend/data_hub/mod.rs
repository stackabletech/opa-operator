use std::{
    collections::BTreeMap,
    fmt::{self, Display},
    path::Path,
};

use hyper::StatusCode;
use info_fetcher_commons::{
    http_error,
    utils::{self, http::send_json_request},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1;
use tracing::{debug, instrument, trace, warn};

use crate::{
    api::{GetResourceInfoError, ResourceInfoBackend, ResourceInfoRequest},
    backend::data_hub::resource_to_urn_mapping::urn_for_request,
};

mod graphql;
pub(crate) mod resource_to_urn_mapping;

/// Errors that can occur while resolving the backend, which happens once at startup.
///
/// Kept apart from [`Error`] because these never reach a caller: a failure here means the process
/// does not come up at all, so (unlike [`Error`]) they have no HTTP status code to map to.
#[derive(Snafu, Debug)]
pub enum ResolveError {
    #[snafu(display("failed to read DataHub token from {path:?}"))]
    ReadToken {
        source: std::io::Error,
        path: String,
    },

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to construct HTTP client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to build DataHub GraphQL endpoint {endpoint:?}"))]
    BuildDataHubEndpoint {
        source: url::ParseError,
        endpoint: String,
    },
}

/// Errors that can occur while answering a request, and which are therefore rendered as an HTTP
/// response to the caller.
#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to execute GraphQL query for URN {urn:?}"))]
    ExecuteGraphQlQuery {
        source: utils::http::Error,
        urn: Urn,
    },

    #[snafu(display("DataHub returned GraphQL errors for URN {urn:?}: {messages}"))]
    GraphQlErrors { messages: String, urn: Urn },

    #[snafu(display("failed to parse the GraphQL response data for URN {urn:?}"))]
    ParseGraphQlData { source: serde_json::Error, urn: Urn },

    #[snafu(display(
        "the resource name {name:?} contains {delimiter:?}, which DataHub uses to delimit the parts \
        of a URN. No URN containing it can resolve, so the request is rejected without querying \
        DataHub."
    ))]
    InvalidResourceName { name: String, delimiter: char },

    #[snafu(display(
        "DataHub reported {total} data products for URN {urn:?}, but the GraphQL query only fetches \
        up to {received} of them. Refusing to answer with a truncated list of data products, as \
        policies evaluating data product membership cannot tell it apart from a complete one. \
        We expect assets to only have a single data product, please report this problem."
    ))]
    TruncatedDataProducts { urn: Urn, total: u32, received: u32 },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            // We could not talk to DataHub at all, which is not something the caller can fix.
            Self::ExecuteGraphQlQuery { .. } => StatusCode::INTERNAL_SERVER_ERROR,

            // The URN is built entirely from the caller's parameters, so a URN DataHub refuses to
            // parse or resolve is a bad request rather than a server fault. Any user who can name a
            // table can reach this, e.g. through Trino's `SELECT * FROM tpch.sf1."a,PROD)"`.
            Self::GraphQlErrors { .. } => StatusCode::BAD_REQUEST,

            // DataHub answered with a body we cannot read, which the caller cannot do anything about.
            Self::ParseGraphQlData { .. } => StatusCode::INTERNAL_SERVER_ERROR,

            // The caller named a resource that cannot be expressed as a URN at all.
            Self::InvalidResourceName { .. } => StatusCode::BAD_REQUEST,

            // A limitation of our own query, see the variant's message.
            Self::TruncatedDataProducts { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// A DataHub URN, e.g. `urn:li:corpuser:alice` or `urn:li:tag:pii`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Urn(pub String);

impl Display for Urn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The information we return for a single resource. Assembled from a single DataHub GraphQL query
/// that resolves the resource's tags and owners (with their user/group details) server-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataHubResourceInfoResponse {
    urn: Urn,
    tags: Vec<Tag>,

    /// The domain the resource belongs to, e.g. `urn:li:domain:marketing`. A resource is assigned to
    /// at most one domain, so this is an [`Option`] rather than a list. [`None`] if the resource is
    /// not assigned to any domain.
    domain: Option<Domain>,

    /// The data products the resource is part of, e.g. `urn:li:dataProduct:orders`. Modelled as a
    /// list because DataHub allows an asset to belong to more than one data product.
    data_products: Vec<DataProduct>,

    /// Owners grouped by their ownership type URN, e.g.
    /// `urn:li:ownershipType:__system__technical_owner`. DataHub has no fixed set of ownership
    /// types — users can define custom ones — so this is an open map, not an enum.
    owners: BTreeMap<Urn, Owners>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Owners {
    /// Human-readable name of the ownership type, e.g. "Technical Owner". [`None`] if DataHub did
    /// not resolve a name for the type.
    ownership_type_name: Option<String>,
    users: Vec<User>,
    groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    urn: Urn,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Domain {
    urn: Urn,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataProduct {
    urn: Urn,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    urn: Urn,
    full_name: Option<String>,
    display_name: String,
    email: Option<String>,
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    urn: Urn,
    display_name: String,
    description: Option<String>,
}

/// This struct combines the CRD configuration with credentials loaded from the filesystem.
/// Credentials, the HTTP client and the GraphQL endpoint are initialized once at startup and stored
/// internally.
pub struct ResolvedDataHubBackend {
    token: String,
    http_client: reqwest::Client,
    graphql_url: Url,

    /// The DataHub fabric the requested resources live in, see [`v1alpha1::DataHubBackend::env`].
    env: v1alpha1::FabricType,
}

impl ResolvedDataHubBackend {
    /// Resolves a DataHub backend by loading credentials from the filesystem.
    #[instrument(skip_all)]
    pub async fn resolve(
        config: v1alpha1::DataHubBackend,
        credentials_dir: &Path,
    ) -> Result<Self, ResolveError> {
        let token_path = credentials_dir.join("token");

        // Trim trailing whitespace/newlines so the value is safe to use in an HTTP header.
        let token = tokio::fs::read_to_string(&token_path)
            .await
            .with_context(|_| ReadTokenSnafu {
                path: token_path.display().to_string(),
            })?
            .trim()
            .to_owned();

        let mut client_builder = utils::http::client_builder();
        client_builder = utils::tls::configure_reqwest(&config.tls, client_builder)
            .await
            .context(ConfigureTlsSnafu)?;
        let http_client = client_builder.build().context(ConstructHttpClientSnafu)?;

        Ok(Self {
            token,
            http_client,
            graphql_url: build_graphql_url(&config)?,
            env: config.env,
        })
    }

    /// Fetches a resource, its tags and its owners (including the owning users' and groups' details)
    /// in a single DataHub GraphQL query. DataHub resolves the referenced tag/user/group entities
    /// server-side, so there is no client-side fan-out.
    #[instrument(skip(self))]
    async fn query_entity(&self, urn: &Urn) -> Result<graphql::Entity, Error> {
        trace!(%urn, %self.graphql_url, "Sending GraphQL query to DataHub");

        let response: graphql::GraphQlResponse = send_json_request(
            self.http_client
                .post(self.graphql_url.clone())
                // Authenticate with a DataHub Personal Access Token (a bearer JWT). DataHub's
                // Metadata Service Authentication verifies it and resolves it to the token's actor.
                .bearer_auth(&self.token)
                .json(&graphql::request(urn)),
        )
        .await
        .with_context(|_| ExecuteGraphQlQuerySnafu { urn: urn.clone() })?;

        // GraphQL reports genuine problems (unresolvable URNs, resolver failures) in `errors` while
        // still returning `200 OK`. These are actual errors — not a missing resource — so we fail
        // the request rather than returning incomplete data.
        if !response.errors.is_empty() {
            let messages = response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return GraphQlErrorsSnafu {
                messages,
                urn: urn.clone(),
            }
            .fail();
        }

        // Only now that `errors` is known to be empty is the payload parsed, so that a response we
        // cannot read is still reported with the GraphQL error explaining it.
        let data = response
            .data
            .map(serde_json::from_value::<graphql::ResponseData>)
            .transpose()
            .with_context(|_| ParseGraphQlDataSnafu { urn: urn.clone() })?;

        // A resource that is unknown to DataHub is reported as a null entity (with no errors). It
        // simply has no metadata, so we return an empty response rather than failing the request.
        let Some(entity) = data.and_then(|data| data.entity) else {
            debug!(%urn, "DataHub returned no entity; responding with empty resource info");
            return Ok(graphql::Entity::default());
        };

        // The query only reads tags, owners and domains off the entity types it has inline fragments
        // for. Anything else (reachable through `rawIdentifier`, which accepts any URN) resolves to
        // a response that looks just like that of a resource with no metadata, so say so rather than
        // letting a policy silently decide on an empty record.
        if let Some(entity_type) = entity.uncovered_type() {
            warn!(
                %urn,
                entity_type,
                "DataHub resolved this URN to an entity type the resource-info-fetcher cannot read \
                metadata from; answering with empty tags, owners and data products. A policy cannot \
                tell this apart from a resource that has no metadata, so do not rely on it"
            );
        }

        // Fail loudly instead of serving a partial list of data products: a policy that keys off data
        // product membership would otherwise silently decide based on incomplete metadata.
        if let Some(truncation) = entity.data_products_truncation() {
            return TruncatedDataProductsSnafu {
                urn: urn.clone(),
                total: truncation.total,
                received: truncation.received,
            }
            .fail();
        }

        debug!(%urn, "Fetched entity from DataHub via GraphQL");
        trace!(?entity, "DataHub entity payload");

        Ok(entity)
    }
}

#[cfg(test)]
impl ResolvedDataHubBackend {
    /// A backend querying `graphql_url`, bypassing [`ResolvedDataHubBackend::resolve`] so that no
    /// credentials have to be read from disk.
    pub fn for_tests(graphql_url: Url) -> Self {
        Self {
            token: "not-a-real-token".to_owned(),
            http_client: reqwest::Client::new(),
            graphql_url,
            env: v1alpha1::FabricType::Prod,
        }
    }
}

/// Builds the DataHub GraphQL endpoint from the backend configuration.
fn build_graphql_url(config: &v1alpha1::DataHubBackend) -> Result<Url, ResolveError> {
    let schema = if config.tls.uses_tls() {
        "https"
    } else {
        "http"
    };
    let port = config
        .port
        .unwrap_or(if config.tls.uses_tls() { 443 } else { 80 });

    let base = format!("{schema}://{hostname}:{port}", hostname = config.hostname);
    let mut graphql_url =
        Url::parse(&base).with_context(|_| BuildDataHubEndpointSnafu { endpoint: base })?;

    // Appending the segments one by one - instead of formatting them into the path - normalizes
    // however the user spelled the root path: leading, trailing and repeated slashes are dropped.
    graphql_url
        .path_segments_mut()
        .expect("an http(s) URL can always be used as a base")
        .clear()
        .extend(
            config
                .root_path
                .split('/')
                .filter(|segment| !segment.is_empty()),
        )
        .extend(["api", "graphql"]);

    Ok(graphql_url)
}

impl ResourceInfoBackend for ResolvedDataHubBackend {
    type Response = DataHubResourceInfoResponse;

    #[instrument(skip(self))]
    async fn get_resource_info(
        &self,
        request: &ResourceInfoRequest,
    ) -> Result<Self::Response, GetResourceInfoError> {
        let urn = urn_for_request(request, &self.env)?;
        let entity = self.query_entity(&urn).await?;

        Ok(entity.into_response(urn))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;
    use snafu::IntoError;

    use super::*;

    const HOSTNAME: &str = "datahub-gms.default.svc.cluster.local";

    fn tls() -> serde_json::Value {
        json!({"verification": {"none": {}}})
    }

    /// Deserializes a DataHub backend configured with the mandatory fields plus `config_fields`.
    ///
    /// The configuration is deserialized rather than constructed, so that the CRD defaults - most
    /// importantly the default root path - are applied the same way they are for a real OpaCluster.
    fn config(config_fields: serde_json::Value) -> v1alpha1::DataHubBackend {
        let mut config = json!({
            "hostname": HOSTNAME,
            "credentialsSecretName": "datahub-credentials",
        });
        config
            .as_object_mut()
            .expect("the mandatory fields are a JSON object")
            .extend(
                config_fields
                    .as_object()
                    .expect("the config fields of every test case are a JSON object")
                    .clone(),
            );

        serde_json::from_value(config)
            .expect("test config must be a valid DataHub backend configuration")
    }

    /// [`Url`] omits the port whenever it is the default port of the scheme, hence the expected
    /// endpoints of the defaulted cases carry no port.
    fn urn() -> Urn {
        Urn("urn:li:dataset:(urn:li:dataPlatform:trino,a,PROD)".to_owned())
    }

    /// The status code tells the caller whose problem a failure is. The URN we query is built
    /// entirely from the caller's parameters, so a URN DataHub refuses to parse or resolve is a bad
    /// request. Any user who can name a table can reach it, for example through Trino's
    /// `SELECT * FROM tpch.sf1."a,PROD)"`. A backend we could not reach at all, or a limitation of
    /// our own query, is not something the caller can do anything about.
    #[rstest]
    #[case::graphql_errors(
        GraphQlErrorsSnafu { messages: "Failed to parse urn", urn: urn() }.build(),
        StatusCode::BAD_REQUEST
    )]
    #[case::unreachable_backend(
        ExecuteGraphQlQuerySnafu { urn: urn() }.into_error(utils::http::Error::HttpErrorResponse {
            status: StatusCode::UNAUTHORIZED,
            url: "http://datahub-gms/api/graphql".to_owned(),
            text: "Unauthorized".to_owned(),
        }),
        StatusCode::INTERNAL_SERVER_ERROR
    )]
    #[case::truncated_data_products(
        TruncatedDataProductsSnafu { urn: urn(), total: 11u32, received: 10u32 }.build(),
        StatusCode::INTERNAL_SERVER_ERROR
    )]
    fn status_code(#[case] error: Error, #[case] expected_status_code: StatusCode) {
        assert_eq!(
            info_fetcher_commons::http_error::Error::status_code(&error),
            expected_status_code
        );
    }

    #[rstest]
    #[case::default_scheme_and_port(json!({}), format!("http://{HOSTNAME}/api/graphql"))]
    #[case::default_tls_scheme_and_port(
        json!({"tls": tls()}),
        format!("https://{HOSTNAME}/api/graphql")
    )]
    #[case::explicit_port(json!({"port": 8080}), format!("http://{HOSTNAME}:8080/api/graphql"))]
    #[case::explicit_tls_port(
        json!({"port": 8443, "tls": tls()}),
        format!("https://{HOSTNAME}:8443/api/graphql")
    )]
    #[case::empty_root_path(json!({"rootPath": ""}), format!("http://{HOSTNAME}/api/graphql"))]
    #[case::root_path_slash(json!({"rootPath": "/"}), format!("http://{HOSTNAME}/api/graphql"))]
    #[case::root_path_without_slashes(
        json!({"rootPath": "datahub"}),
        format!("http://{HOSTNAME}/datahub/api/graphql")
    )]
    #[case::root_path_leading_slash(
        json!({"rootPath": "/datahub"}),
        format!("http://{HOSTNAME}/datahub/api/graphql")
    )]
    #[case::root_path_trailing_slash(
        json!({"rootPath": "datahub/"}),
        format!("http://{HOSTNAME}/datahub/api/graphql")
    )]
    #[case::root_path_surrounding_slashes(
        json!({"rootPath": "/datahub/"}),
        format!("http://{HOSTNAME}/datahub/api/graphql")
    )]
    #[case::root_path_repeated_slashes(
        json!({"rootPath": "//datahub//"}),
        format!("http://{HOSTNAME}/datahub/api/graphql")
    )]
    #[case::nested_root_path(
        json!({"rootPath": "/proxy/datahub/"}),
        format!("http://{HOSTNAME}/proxy/datahub/api/graphql")
    )]
    #[case::root_path_with_tls(
        json!({"rootPath": "/datahub", "tls": tls()}),
        format!("https://{HOSTNAME}/datahub/api/graphql")
    )]
    fn graphql_url(#[case] config_fields: serde_json::Value, #[case] expected_url: String) {
        let graphql_url = build_graphql_url(&config(config_fields))
            .expect("GraphQL endpoint must be constructible from the test config");

        assert_eq!(graphql_url.as_str(), expected_url);
    }
}
