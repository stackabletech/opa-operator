use std::{collections::BTreeMap, path::Path, sync::Arc};

use futures::future::try_join_all;
use hyper::StatusCode;
use info_fetcher_commons::{
    http_error,
    utils::{self, http::send_json_request},
};
use moka::future::Cache;
use reqwest::{ClientBuilder, Url};
use serde::Serialize;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1::{self, DataHubBackend};
use strum::IntoEnumIterator;
use tracing::{debug, instrument, trace};

use crate::{
    api::{GetDataHubUserSnafu, GetResourceInfoError, ResourceInfoBackend, ResourceInfoRequest},
    backend::data_hub::{
        resource_to_urn_mapping::urn_for_request,
        upstream_api::{
            AspectsCorpGroupInfoValue, AspectsCorpUserInfoValue, AspectsOwnershipValueOwnerType,
            DataHubEntityResponse, OwnerTypeUrn, RawOwnerType, Tag, Urn,
        },
    },
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
        urn: Urn,
    },

    #[snafu(display(
        "the entity information send by DataHub for the tag with the URN {urn:?} must contain tag properties"
    ))]
    EntityResponseMustContainTagProperties { urn: Urn },

    #[snafu(display(
        "the entity information send by DataHub for the user with the URN {urn:?} must contain user information"
    ))]
    EntityResponseMustContainUserInfo { urn: Urn },

    #[snafu(display(
        "the entity information send by DataHub for the group with the URN {urn:?} must contain group information"
    ))]
    EntityResponseMustContainGroupInfo { urn: Urn },
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
            Self::EntityResponseMustContainTagProperties { .. } => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::EntityResponseMustContainUserInfo { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::EntityResponseMustContainGroupInfo { .. } => StatusCode::INTERNAL_SERVER_ERROR,
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

    tag_cache: Cache<Urn, Tag>,
    user_cache: Cache<Urn, AspectsCorpUserInfoValue>,
    group_cache: Cache<Urn, AspectsCorpGroupInfoValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataHubResourceInfoResponse {
    owners: BTreeMap<OwnerType, DataHubResourceInfoResponseOwners>,
    tags: Vec<Tag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord, strum::EnumIter)]
#[serde(rename_all = "camelCase")]
pub enum OwnerType {
    BusinessOwner,
    TechnicalOwner,
    DataSteward,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataHubResourceInfoResponseOwners {
    users: Vec<AspectsCorpUserInfoValue>,
    groups: Vec<AspectsCorpGroupInfoValue>,
}

impl From<&AspectsOwnershipValueOwnerType> for OwnerType {
    fn from(value: &AspectsOwnershipValueOwnerType) -> Self {
        match value {
            AspectsOwnershipValueOwnerType::Urn { type_urn } => match type_urn {
                OwnerTypeUrn::BusinessOwner => Self::BusinessOwner,
                OwnerTypeUrn::TechnicalOwner => Self::TechnicalOwner,
                OwnerTypeUrn::DataSteward => Self::DataSteward,
                OwnerTypeUrn::None => Self::None,
            },
            AspectsOwnershipValueOwnerType::Raw {
                type_: RawOwnerType::TechnicalOwner,
            } => Self::TechnicalOwner,
        }
    }
}

impl ResolvedDataHubBackend {
    /// Resolves a DataHub backend by loading credentials from the filesystem.
    #[instrument(skip_all)]
    pub async fn resolve(
        config: v1alpha1::DataHubBackend,
        cache: &stackable_opa_operator::crd::cache::Cache,
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

        let tag_cache = Cache::builder()
            .time_to_live(*cache.entry_time_to_live)
            .name("tag-info")
            .build();
        let user_cache = Cache::builder()
            .time_to_live(*cache.entry_time_to_live)
            .name("user-info")
            .build();
        let group_cache = Cache::builder()
            .time_to_live(*cache.entry_time_to_live)
            .name("group-info")
            .build();

        Ok(Self {
            config,
            client_id,
            client_secret,
            http_client,
            tag_cache,
            user_cache,
            group_cache,
        })
    }
}

impl ResourceInfoBackend for ResolvedDataHubBackend {
    type Response = DataHubResourceInfoResponse;

    // The individual URN lookups already run concurrently via `try_join_all`. On top of that we
    // overlap the coarse-grained groups as well: tags are fetched at the same time as the owners,
    // all owner types are fetched concurrently, and within each owner type the user and group
    // lookups run at the same time.
    //
    // TODO: This is unbounded concurrency. For entities with many owners/tags this can burst a lot
    // of simultaneous requests at DataHub. If that becomes a problem, bound the concurrency (e.g.
    // via `futures::stream::StreamExt::buffer_unordered` or a shared `tokio::sync::Semaphore`).
    #[instrument(skip(self))]
    async fn get_resource_info(
        &self,
        request: &ResourceInfoRequest,
    ) -> Result<Self::Response, GetResourceInfoError> {
        let urn = urn_for_request(request, &self.config.env);
        let entity_response: DataHubEntityResponse = self.query_entity(&urn).await?;
        let entity_response = &entity_response;

        // Fetch the tags of the entity.
        let tags_fut = async move {
            let tag_urns = entity_response.tag_urns();
            try_join_all(
                tag_urns
                    .iter()
                    .map(|urn| async { self.query_tag(urn).await }),
            )
            .await
            .context(GetDataHubUserSnafu)
        };

        // Fetch the owners of every type.
        let owners_fut = try_join_all(OwnerType::iter().map(|owner_type| async move {
            let (user_urns, users_without_urn, group_urns) =
                entity_response.owners_for_type(&owner_type);

            let (mut users, groups) = futures::try_join!(
                try_join_all(
                    user_urns
                        .iter()
                        .map(|urn| async { self.query_user(urn).await }),
                ),
                try_join_all(
                    group_urns
                        .iter()
                        .map(|urn| async { self.query_group(urn).await }),
                ),
            )
            .context(GetDataHubUserSnafu)?;

            users.extend(
                users_without_urn
                    .iter()
                    .map(|username| AspectsCorpUserInfoValue {
                        full_name: None,
                        display_name: username.trim_start_matches("urn:li:corpuser").to_owned(),
                        email: None,
                        active: true,
                        data_hub_user: false,
                    }),
            );

            Ok::<_, GetResourceInfoError>((
                owner_type,
                DataHubResourceInfoResponseOwners { users, groups },
            ))
        }));

        let (tags, owners) = futures::try_join!(tags_fut, owners_fut)?;

        Ok(DataHubResourceInfoResponse {
            tags,
            owners: owners.into_iter().collect(),
        })
    }
}

impl ResolvedDataHubBackend {
    #[instrument(skip(self))]
    async fn query_entity(&self, urn: &Urn) -> Result<DataHubEntityResponse, Error> {
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
            ..
        } = &self;

        let schema = if tls.uses_tls() { "https" } else { "http" };
        let port = port.unwrap_or(if tls.uses_tls() { 443 } else { 80 });

        let entity = format!(
            "{schema}://{hostname}:{port}/entitiesV2/{url_encoded_urn}",
            url_encoded_urn = urlencoding::encode(&urn.0)
        );
        let entity_url =
            Url::parse(&entity).with_context(|_| BuildDataHubEndpointSnafu { endpoint: entity })?;

        trace!(%entity_url, "Sending request to DataHub's entity API");

        let entity = send_json_request(
            http_client
                .get(entity_url.clone())
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
        .with_context(|_| QueryForUrnSnafu { urn: urn.clone() })?;

        debug!(%urn, %entity_url, "Fetched entity from DataHub");
        trace!(?entity, "DataHub entity payload");

        Ok(entity)
    }

    #[instrument(skip(self))]
    async fn query_tag(&self, urn: &Urn) -> Result<Tag, Arc<Error>> {
        self.tag_cache
            .try_get_with_by_ref(urn, async {
                let tag = self
                    .query_entity(urn)
                    .await?
                    .tag_properties()
                    .with_context(|| EntityResponseMustContainTagPropertiesSnafu {
                        urn: urn.clone(),
                    })?
                    .name
                    .clone();
                debug!(%urn, %tag, "Fetched tag from DataHub");

                Ok(tag)
            })
            .await
    }

    #[instrument(skip(self))]
    async fn query_user(&self, urn: &Urn) -> Result<AspectsCorpUserInfoValue, Arc<Error>> {
        self.user_cache
            .try_get_with_by_ref(urn, async {
                let user = self
                    .query_entity(urn)
                    .await?
                    .user_info()
                    .with_context(|| EntityResponseMustContainUserInfoSnafu { urn: urn.clone() })?
                    .clone();
                debug!(%urn, ?user, "Fetched user from DataHub");

                Ok(user)
            })
            .await
    }

    #[instrument(skip(self))]
    async fn query_group(&self, urn: &Urn) -> Result<AspectsCorpGroupInfoValue, Arc<Error>> {
        self.group_cache
            .try_get_with_by_ref(urn, async {
                let group = self
                    .query_entity(urn)
                    .await?
                    .group_info()
                    .with_context(|| EntityResponseMustContainGroupInfoSnafu { urn: urn.clone() })?
                    .clone();
                debug!(%urn, ?group, "Fetched group from DataHub");

                Ok(group)
            })
            .await
    }
}
