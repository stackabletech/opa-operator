//! DataHub GraphQL query and the types used to (de)serialize it.
//!
//! A single `POST /api/graphql` fetches the entity together with its tags and owners, and DataHub
//! resolves the referenced tag/user/group and ownership-type entities server-side. The [`Entity`]
//! response is then flattened into the crate's public [`DataHubResourceInfoResponse`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::backend::data_hub::{
    DataHubResourceInfoResponse, DataProduct, Domain, Group, Owners, Tag, Urn, User,
};

/// A single query covering every entity kind we build URNs for. We use the generic `entity(urn:)`
/// resolver plus per-type inline fragments, because a request can target a dataset (Trino table or
/// Kafka topic), a container (Trino catalog or schema), a chart, a dashboard or something else.
const RESOURCE_INFO_QUERY: &str = r#"
query ResourceInfo($urn: String!) {
  entity(urn: $urn) {
    ...ResourceInfo
  }
}
fragment ResourceInfo on Entity {
  # DataHub has no direct "dataProduct" field on assets; membership is a graph edge that points from
  # the data product to its assets. From the asset's side it is therefore an INCOMING relationship.
  dataProducts: relationships(input: {types: ["DataProductContains"], direction: INCOMING, count: 10}) {
    relationships { ...DataProduct }
  }
  ... on Dataset   { tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
  ... on Container { tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
  ... on Chart     { tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
  ... on Dashboard { tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
}
fragment Tags on GlobalTags {
  tags { tag { urn properties { name } } }
}
fragment Domain on DomainAssociation {
  domain { urn properties { name description } }
}
fragment DataProduct on EntityRelationship {
  entity {
    urn
    ... on DataProduct { properties { name description } }
  }
}
fragment Owners on Ownership {
  owners {
    owner {
      __typename
      ... on CorpUser  { urn properties { fullName displayName email active } }
      ... on CorpGroup { urn properties { displayName description } }
    }
    ownershipType { urn info { name } }
    type
  }
}
"#;

/// Builds the request body for the [`RESOURCE_INFO_QUERY`], parameterized by the entity's URN.
pub fn request(urn: &Urn) -> GraphQlRequest<'_> {
    GraphQlRequest {
        query: RESOURCE_INFO_QUERY,
        variables: Variables { urn: &urn.0 },
    }
}

#[derive(Debug, Serialize)]
pub struct GraphQlRequest<'a> {
    query: &'static str,
    variables: Variables<'a>,
}

#[derive(Debug, Serialize)]
struct Variables<'a> {
    urn: &'a str,
}

/// A GraphQL server answers `200 OK` even when the query fails; the failures are reported in
/// `errors`. Callers must therefore inspect `errors` explicitly rather than relying on the HTTP
/// status code.
#[derive(Debug, Deserialize)]
pub struct GraphQlResponse {
    pub data: Option<ResponseData>,

    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct ResponseData {
    pub entity: Option<Entity>,
}

/// The resolved entity. `tags` and `ownership` come from the per-type inline fragments; GraphQL
/// merges them onto the entity object, so a single flat struct reads them regardless of the
/// concrete entity type. [`Default`] yields the "no metadata" entity used when DataHub does not
/// return an entity for a URN.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    tags: Option<GlobalTags>,
    ownership: Option<Ownership>,
    domain: Option<DomainAssociation>,
    data_products: Option<EntityRelationships>,
}

#[derive(Debug, Deserialize)]
struct GlobalTags {
    tags: Vec<TagAssociation>,
}

#[derive(Debug, Deserialize)]
struct TagAssociation {
    tag: TagNode,
}

#[derive(Debug, Deserialize)]
struct TagNode {
    urn: Urn,
    properties: Option<TagProperties>,
}

#[derive(Debug, Deserialize)]
struct TagProperties {
    name: String,
}

#[derive(Debug, Deserialize)]
struct DomainAssociation {
    domain: DomainNode,
}

#[derive(Debug, Deserialize)]
struct DomainNode {
    urn: Urn,
    properties: Option<DomainProperties>,
}

#[derive(Debug, Deserialize)]
struct DomainProperties {
    name: String,
    description: Option<String>,
}

/// The result of the `DataProductContains` relationship query. Each relationship's related entity is
/// a data product the resource belongs to.
#[derive(Debug, Deserialize)]
struct EntityRelationships {
    relationships: Vec<EntityRelationship>,
}

#[derive(Debug, Deserialize)]
struct EntityRelationship {
    entity: DataProductNode,
}

#[derive(Debug, Deserialize)]
struct DataProductNode {
    urn: Urn,
    properties: Option<DataProductProperties>,
}

#[derive(Debug, Deserialize)]
struct DataProductProperties {
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Ownership {
    owners: Vec<Owner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    owner: OwnerEntity,

    /// The modern ownership type entity, e.g. `urn:li:ownershipType:__system__technical_owner`.
    ownership_type: Option<OwnershipType>,

    /// The legacy ownership type enum, e.g. `TECHNICAL_OWNER`. Used as a fallback key for owners
    /// that predate ownership type entities.
    #[serde(rename = "type")]
    legacy_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OwnershipType {
    urn: Urn,
    info: Option<OwnershipTypeInfo>,
}

#[derive(Debug, Deserialize)]
struct OwnershipTypeInfo {
    name: String,
}

/// The resolved owner. DataHub only ever resolves owners to users or groups.
#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
enum OwnerEntity {
    CorpUser {
        urn: Urn,
        properties: Option<CorpUserProperties>,
    },
    CorpGroup {
        urn: Urn,
        properties: Option<CorpGroupProperties>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpUserProperties {
    full_name: Option<String>,
    display_name: Option<String>,
    email: Option<String>,
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpGroupProperties {
    display_name: Option<String>,
    description: Option<String>,
}

impl Entity {
    /// Flattens the GraphQL response into the crate's public [`DataHubResourceInfoResponse`].
    pub fn into_response(self, urn: Urn) -> DataHubResourceInfoResponse {
        let tags = self
            .tags
            .into_iter()
            .flat_map(|global_tags| global_tags.tags)
            .map(|association| {
                let TagNode { urn, properties } = association.tag;
                // A tag without a properties aspect has no name; fall back to its URN.
                let name = properties
                    .map(|properties| properties.name)
                    .unwrap_or_else(|| urn.0.clone());
                Tag { urn, name }
            })
            .collect();

        let domain = self.domain.map(|association| {
            let DomainNode { urn, properties } = association.domain;
            // A domain without a properties aspect has no name; fall back to its URN.
            let (name, description) = match properties {
                Some(properties) => (properties.name, properties.description),
                None => (urn.0.clone(), None),
            };
            Domain {
                urn,
                name,
                description,
            }
        });

        let data_products = self
            .data_products
            .into_iter()
            .flat_map(|relationships| relationships.relationships)
            .map(|relationship| {
                let DataProductNode { urn, properties } = relationship.entity;
                // A data product without a properties aspect has no name; fall back to its URN.
                let (name, description) = match properties {
                    Some(properties) => (properties.name, properties.description),
                    None => (urn.0.clone(), None),
                };
                DataProduct {
                    urn,
                    name,
                    description,
                }
            })
            .collect();

        let mut owners: BTreeMap<Urn, Owners> = BTreeMap::new();
        for owner in self
            .ownership
            .into_iter()
            .flat_map(|ownership| ownership.owners)
        {
            let (type_urn, type_name) = owner_type(owner.ownership_type, owner.legacy_type);
            let bucket = owners.entry(type_urn).or_default();
            if bucket.ownership_type_name.is_none() {
                bucket.ownership_type_name = type_name;
            }
            match owner.owner {
                OwnerEntity::CorpUser { urn, properties } => {
                    bucket.users.push(user(urn, properties))
                }
                OwnerEntity::CorpGroup { urn, properties } => {
                    bucket.groups.push(group(urn, properties))
                }
            }
        }

        DataHubResourceInfoResponse {
            urn,
            tags,
            domain,
            data_products,
            owners,
        }
    }
}

/// Resolves the map key and human-readable name for an owner's ownership type, preferring the
/// modern ownership type entity and falling back to the legacy `type` enum. This handles arbitrary
/// (including user-defined) ownership types instead of assuming a fixed enum.
fn owner_type(
    ownership_type: Option<OwnershipType>,
    legacy_type: Option<String>,
) -> (Urn, Option<String>) {
    match ownership_type {
        Some(ownership_type) => (
            ownership_type.urn,
            ownership_type.info.map(|info| info.name),
        ),
        // Fallback for owners where DataHub only populated the legacy `type` field.
        None => (
            Urn(legacy_type.unwrap_or_else(|| "unknown".to_owned())),
            None,
        ),
    }
}

fn user(urn: Urn, properties: Option<CorpUserProperties>) -> User {
    match properties {
        Some(properties) => User {
            full_name: properties.full_name,
            display_name: properties
                .display_name
                .unwrap_or_else(|| strip_user_urn(&urn)),
            email: properties.email,
            active: properties.active.unwrap_or(true),
            urn,
        },
        // An owner reference without a corpUserInfo aspect: derive a display name from the URN.
        None => User {
            full_name: None,
            display_name: strip_user_urn(&urn),
            email: None,
            active: true,
            urn,
        },
    }
}

fn group(urn: Urn, properties: Option<CorpGroupProperties>) -> Group {
    let (display_name, description) = match properties {
        Some(properties) => (
            properties.display_name.unwrap_or_else(|| urn.0.clone()),
            properties.description,
        ),
        None => (urn.0.clone(), None),
    };

    Group {
        urn,
        display_name,
        description,
    }
}

fn strip_user_urn(urn: &Urn) -> String {
    urn.0.trim_start_matches("urn:li:corpuser:").to_owned()
}
