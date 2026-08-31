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

/// The page size of the `DataProductContains` relationship query, passed to DataHub as the
/// `$dataProductsCount` variable.
///
/// DataHub paginates the `relationships` resolver, so some page size has to be picked. An asset
/// normally belongs to one or two data products, so this is deliberately set far above anything
/// realistic rather than at a plausible maximum: exceeding it fails the request (see
/// [`Entity::data_products_truncation`]), and failing a lookup that should have succeeded is the
/// worse outcome. It stays a single request at any size, and only costs DataHub more when an asset
/// really does have that many relationships.
///
/// We do not paginate. That would mean one round trip per page for a case that should not occur,
/// whereas overshooting the page size costs nothing until it is actually needed.
const DATA_PRODUCTS_PAGE_SIZE: u32 = 1000;

/// A single query covering every entity kind we build URNs for. We use the generic `entity(urn:)`
/// resolver plus per-type inline fragments, because a request can target a dataset (Trino table or
/// Kafka topic), a container (Trino catalog or schema), a chart, a dashboard or something else.
const RESOURCE_INFO_QUERY: &str = r#"
query ResourceInfo($urn: String!, $dataProductsCount: Int!) {
  entity(urn: $urn) {
    ...ResourceInfo
  }
}
fragment ResourceInfo on Entity {
  # The concrete type DataHub resolved the URN to. Only the types with an inline fragment below carry
  # tags, owners and a domain in our response, so this lets us tell an entity we cannot read from one
  # that genuinely has no metadata, see `Entity::uncovered_type`.
  __typename

  # DataHub has no direct "dataProduct" field on assets; membership is a graph edge that points from
  # the data product to its assets. From the asset's side it is therefore an INCOMING relationship.
  # `total` is the number of edges DataHub has, which we compare against the number we received to
  # detect that the page size was too small.
  dataProducts: relationships(input: {types: ["DataProductContains"], direction: INCOMING, count: $dataProductsCount}) {
    total
    relationships { ...DataProduct }
  }
  # `exists` and `status` answer whether the resource is in the catalog at all, see
  # `Entity::in_catalog`. Both sit on the concrete types rather than on the `Entity` interface, so
  # they have to be repeated per fragment alongside the metadata fields.
  ... on Dataset   { exists status { removed } tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
  ... on Container { exists status { removed } tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
  ... on Chart     { exists status { removed } tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
  ... on Dashboard { exists status { removed } tags { ...Tags } ownership { ...Owners } domain { ...Domain } }
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
        variables: Variables {
            urn: &urn.0,
            data_products_count: DATA_PRODUCTS_PAGE_SIZE,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct GraphQlRequest<'a> {
    query: &'static str,
    variables: Variables<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Variables<'a> {
    urn: &'a str,
    data_products_count: u32,
}

/// Reads an absent or `null` list as an empty one.
///
/// GraphQL answers a field whose resolver failed with `null` rather than omitting it, so a list
/// field can legitimately arrive as `null`. `#[serde(default)]` alone only covers the absent case.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

/// A GraphQL server answers `200 OK` even when the query fails; the failures are reported in
/// `errors`. Callers must therefore inspect `errors` explicitly rather than relying on the HTTP
/// status code.
#[derive(Debug, Deserialize)]
pub struct GraphQlResponse {
    /// Kept unparsed until `errors` has been inspected. A failing resolver nulls out the field it
    /// failed on, which can leave `data` in a shape [`ResponseData`] cannot describe. Parsing it
    /// eagerly would surface such a response as a JSON error and discard `errors`, the only part
    /// of the response that says what actually went wrong.
    pub data: Option<serde_json::Value>,

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
    /// The concrete DataHub type the URN resolved to, e.g. `Dataset`. [`None`] for the substituted
    /// "no metadata" entity, and for a DataHub that does not report it.
    #[serde(rename = "__typename")]
    typename: Option<String>,

    /// Whether DataHub holds any aspect for this URN, see [`Entity::in_catalog`] for what that does
    /// and does not say. [`None`] for an entity type [`RESOURCE_INFO_QUERY`] has no inline fragment
    /// for, as the field is declared on the concrete types rather than on the `Entity` interface.
    exists: Option<bool>,

    /// The entity's lifecycle status, whose `removed` flag marks a soft delete. [`None`] when the
    /// entity has no status aspect, which means it was never deleted.
    status: Option<Status>,

    tags: Option<GlobalTags>,
    ownership: Option<Ownership>,
    domain: Option<DomainAssociation>,
    data_products: Option<EntityRelationships>,
}

#[derive(Debug, Deserialize)]
struct Status {
    removed: bool,
}

/// The entity types [`RESOURCE_INFO_QUERY`] has an inline fragment for, and whose tags, owners and
/// domain we therefore read.
///
/// Anything else still resolves, because `rawIdentifier` accepts any URN, but only the fields common
/// to every `Entity` come back. The response then looks just like that of a resource with no metadata.
const COVERED_ENTITY_TYPES: &[&str] = &["Dataset", "Container", "Chart", "Dashboard"];

#[derive(Debug, Deserialize)]
struct GlobalTags {
    #[serde(default, deserialize_with = "null_as_default")]
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
    /// The total number of matching relationships DataHub has, which can exceed the number of
    /// `relationships` actually returned, as those are capped by [`DATA_PRODUCTS_PAGE_SIZE`].
    total: Option<u32>,

    #[serde(default, deserialize_with = "null_as_default")]
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
    #[serde(default, deserialize_with = "null_as_default")]
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

/// A truncated data product list: DataHub has more `DataProductContains` edges for the entity than
/// [`DATA_PRODUCTS_PAGE_SIZE`] allowed us to fetch.
#[derive(Debug)]
pub struct DataProductsTruncation {
    /// The number of data products DataHub reported in total.
    pub total: u32,

    /// The number of data products we actually received, i.e. [`DATA_PRODUCTS_PAGE_SIZE`].
    pub received: u32,
}

impl Entity {
    /// Whether the resource is present in the catalog and has not been deleted.
    ///
    /// This is the one signal that tells a resource DataHub has never heard of apart from one that
    /// is catalogued but carries no tags, owners or data products. Both look identical otherwise,
    /// because `entity(urn:)` answers with an entity built from the URN alone for *any* well-formed
    /// URN, rather than with `null` for one it does not know.
    ///
    /// Deliberately not DataHub's `exists` on its own. That field is resolved with
    /// `includeSoftDelete = true`, so it stays `true` for an asset someone deleted in DataHub, which
    /// is the normal delete there (it sets `status.removed`, it does not purge the aspects). A
    /// policy keyed on `exists` alone would therefore keep treating a deleted resource as present,
    /// which is exactly backwards for an authorization decision. Both fields come from the one query
    /// we already send, so requiring both costs nothing.
    ///
    /// `false` when the fields are missing, which happens for an entity type
    /// [`RESOURCE_INFO_QUERY`] has no inline fragment for (see [`Entity::uncovered_type`]). We
    /// cannot confirm such a resource is in the catalog, and for an authorization input the honest
    /// answer to "cannot confirm" is the one that denies.
    pub fn in_catalog(&self) -> bool {
        let soft_deleted = self.status.as_ref().is_some_and(|status| status.removed);

        self.exists == Some(true) && !soft_deleted
    }

    /// The entity's DataHub type, if [`RESOURCE_INFO_QUERY`] does not cover it.
    ///
    /// [`None`] means the type is covered, or that DataHub did not report one. In the latter case
    /// there is nothing to compare against, so we must not report a problem we cannot substantiate.
    ///
    /// Callers should surface this: the response for an uncovered type is empty, and a policy has no
    /// way to distinguish that from a resource that carries no tags, owners or domain at all.
    pub fn uncovered_type(&self) -> Option<&str> {
        let typename = self.typename.as_deref()?;

        (!COVERED_ENTITY_TYPES.contains(&typename)).then_some(typename)
    }

    /// Checks whether the data product list was truncated by [`DATA_PRODUCTS_PAGE_SIZE`].
    ///
    /// Callers must turn this into an error rather than serving the truncated list: a policy that
    /// evaluates data product membership cannot distinguish an incomplete list from a complete one,
    /// so it would silently make decisions on partial metadata.
    pub fn data_products_truncation(&self) -> Option<DataProductsTruncation> {
        let data_products = self.data_products.as_ref()?;
        let total = data_products.total?;

        (total as usize > data_products.relationships.len()).then_some(DataProductsTruncation {
            total,
            received: data_products.relationships.len() as u32,
        })
    }

    /// Flattens the GraphQL response into the crate's public [`DataHubResourceInfoResponse`].
    pub fn into_response(self, urn: Urn) -> DataHubResourceInfoResponse {
        // Read before `self` is taken apart below.
        let in_catalog = self.in_catalog();

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
            in_catalog,
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

/// The username part of a `corpuser` URN, or the whole URN if it is not one.
///
/// Strips exactly one prefix: `trim_start_matches` would strip it repeatedly, mangling the name of a
/// user who is unluckily called `urn:li:corpuser:alice`.
fn strip_user_urn(urn: &Urn) -> String {
    urn.0
        .strip_prefix("urn:li:corpuser:")
        .unwrap_or(&urn.0)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    /// Deserializes an entity whose `dataProducts` result reports `total` and contains
    /// `returned` relationships, mirroring what DataHub answers.
    fn entity(total: Option<u32>, returned: u32) -> Entity {
        let relationships = (0..returned)
            .map(|index| json!({"entity": {"urn": format!("urn:li:dataProduct:{index}")}}))
            .collect::<Vec<_>>();

        serde_json::from_value(json!({
            "dataProducts": {"total": total, "relationships": relationships},
        }))
        .expect("test entity must be a valid GraphQL entity payload")
    }

    #[rstest]
    #[case::no_data_products(Some(0), 0)]
    #[case::single_data_product(Some(1), 1)]
    #[case::full_page(Some(DATA_PRODUCTS_PAGE_SIZE), DATA_PRODUCTS_PAGE_SIZE)]
    // DataHub not reporting a total leaves us nothing to compare against, so we must not fail.
    #[case::no_total(None, DATA_PRODUCTS_PAGE_SIZE)]
    fn complete_data_products(#[case] total: Option<u32>, #[case] returned: u32) {
        assert!(entity(total, returned).data_products_truncation().is_none());
    }

    /// An entity without any `dataProducts` result at all, e.g. the "no metadata" entity we
    /// substitute for URNs DataHub does not know.
    #[test]
    fn missing_data_products_are_complete() {
        assert!(Entity::default().data_products_truncation().is_none());
    }

    /// Deserializes an entity of the given DataHub type, as reported by `__typename`.
    fn entity_of_type(typename: Option<&str>) -> Entity {
        serde_json::from_value(json!({"__typename": typename}))
            .expect("test entity must be a valid GraphQL entity payload")
    }

    /// An entity type the query has an inline fragment for is read normally.
    #[rstest]
    #[case::dataset("Dataset")]
    #[case::container("Container")]
    #[case::chart("Chart")]
    #[case::dashboard("Dashboard")]
    fn covered_entity_types(#[case] typename: &str) {
        assert_eq!(entity_of_type(Some(typename)).uncovered_type(), None);
    }

    /// Any other type deserializes into an entity with no tags, owners or domain, which a policy
    /// cannot tell apart from a resource that genuinely has none, so it has to be reported.
    #[rstest]
    #[case::data_job("DataJob")]
    #[case::data_flow("DataFlow")]
    #[case::notebook("Notebook")]
    #[case::ml_model("MLModel")]
    fn uncovered_entity_types(#[case] typename: &str) {
        assert_eq!(
            entity_of_type(Some(typename)).uncovered_type(),
            Some(typename)
        );
    }

    /// Without a `__typename` there is nothing to check against, so we must not cry wolf.
    #[test]
    fn entities_without_a_typename_are_not_reported() {
        assert_eq!(entity_of_type(None).uncovered_type(), None);
        assert_eq!(Entity::default().uncovered_type(), None);
    }

    /// Deserializes an entity with the `exists`/`status` pair DataHub answers with, both optional
    /// because an uncovered entity type is answered without either.
    fn entity_with_presence(exists: Option<bool>, removed: Option<bool>) -> Entity {
        let mut payload = json!({});
        if let Some(exists) = exists {
            payload["exists"] = json!(exists);
        }
        if let Some(removed) = removed {
            payload["status"] = json!({"removed": removed});
        }

        entity_from(payload)
    }

    /// A resource DataHub holds and nobody deleted. The only combination that may answer "yes".
    #[rstest]
    #[case::never_deleted(Some(true), None)]
    #[case::explicitly_not_removed(Some(true), Some(false))]
    fn a_catalogued_resource_is_reported_as_in_the_catalog(
        #[case] exists: Option<bool>,
        #[case] removed: Option<bool>,
    ) {
        assert!(entity_with_presence(exists, removed).in_catalog());
    }

    /// The reason we do not use DataHub's `exists` on its own. Deleting an asset in DataHub is a
    /// soft delete: it sets `status.removed` and leaves the aspects in place, and `exists` is
    /// resolved with `includeSoftDelete = true`, so it keeps answering `true`. A policy keyed on
    /// `exists` alone would go on treating the deleted resource as present.
    #[test]
    fn a_soft_deleted_resource_is_not_in_the_catalog() {
        assert!(!entity_with_presence(Some(true), Some(true)).in_catalog());
    }

    /// A URN DataHub has never seen. It still answers with an entity built from the URN alone,
    /// which is why `exists` is the only thing that tells this apart from a resource with no
    /// metadata.
    #[test]
    fn an_unknown_resource_is_not_in_the_catalog() {
        assert!(!entity_with_presence(Some(false), None).in_catalog());
    }

    /// An entity type [`RESOURCE_INFO_QUERY`] has no inline fragment for is answered without
    /// `exists`, so we cannot confirm anything. For an authorization input that has to deny.
    #[rstest]
    #[case::no_fields(None, None)]
    #[case::status_only(None, Some(false))]
    fn a_resource_we_cannot_check_is_not_in_the_catalog(
        #[case] exists: Option<bool>,
        #[case] removed: Option<bool>,
    ) {
        assert!(!entity_with_presence(exists, removed).in_catalog());
    }

    /// The substituted "no metadata" entity carries no presence fields either.
    #[test]
    fn the_default_entity_is_not_in_the_catalog() {
        assert!(!Entity::default().in_catalog());
    }

    /// The wire name is part of the contract: the rego rule library and the docs both spell it
    /// `inCatalog`, and a rule reading a differently spelled field is silently undefined rather
    /// than wrong in any visible way.
    #[test]
    fn the_response_spells_the_presence_field_in_catalog() {
        let response = entity_with_presence(Some(true), Some(false)).into_response(urn());

        let body = serde_json::to_value(&response).expect("the response must serialize");

        assert_eq!(body["inCatalog"], json!(true));
    }

    #[test]
    fn truncated_data_products_are_detected() {
        let truncation = entity(Some(DATA_PRODUCTS_PAGE_SIZE + 1), DATA_PRODUCTS_PAGE_SIZE)
            .data_products_truncation()
            .expect("more data products than the page size must be reported as truncated");

        assert_eq!(truncation.total, DATA_PRODUCTS_PAGE_SIZE + 1);
        assert_eq!(truncation.received, DATA_PRODUCTS_PAGE_SIZE);
    }

    fn urn() -> Urn {
        Urn("urn:li:dataset:(urn:li:dataPlatform:trino,my-trino.tpch.sf1.customer,PROD)".to_owned())
    }

    /// Deserializes an entity from the payload DataHub would return.
    fn entity_from(payload: serde_json::Value) -> Entity {
        serde_json::from_value(payload).expect("test entity must be a valid GraphQL entity payload")
    }

    /// The mapping when DataHub populated every `properties` aspect, i.e. the case the fallbacks below
    /// are fallbacks for.
    #[test]
    fn a_fully_populated_entity_maps_every_field() {
        let response = entity_from(json!({
            "__typename": "Dataset",
            "exists": true,
            "status": {"removed": false},
            "tags": {"tags": [{"tag": {"urn": "urn:li:tag:PII", "properties": {"name": "PII"}}}]},
            "domain": {"domain": {"urn": "urn:li:domain:finance", "properties": {
                "name": "Finance", "description": "Financial data",
            }}},
            "dataProducts": {"total": 1, "relationships": [{"entity": {
                "urn": "urn:li:dataProduct:orders",
                "properties": {"name": "Orders", "description": "Order data"},
            }}]},
            "ownership": {"owners": [
                {
                    "owner": {
                        "__typename": "CorpUser",
                        "urn": "urn:li:corpuser:alice",
                        "properties": {
                            "fullName": "Alice Example", "displayName": "Alice",
                            "email": "alice@example.com", "active": true,
                        },
                    },
                    "ownershipType": {
                        "urn": "urn:li:ownershipType:__system__technical_owner",
                        "info": {"name": "Technical Owner"},
                    },
                },
                {
                    "owner": {
                        "__typename": "CorpGroup",
                        "urn": "urn:li:corpGroup:analytics",
                        "properties": {"displayName": "Analytics", "description": "The team"},
                    },
                    "ownershipType": {
                        "urn": "urn:li:ownershipType:__system__technical_owner",
                        "info": {"name": "Technical Owner"},
                    },
                },
            ]},
        }))
        .into_response(urn());

        assert!(response.in_catalog);
        assert_eq!(
            response.tags,
            vec![Tag {
                urn: Urn("urn:li:tag:PII".to_owned()),
                name: "PII".to_owned(),
            }]
        );
        assert_eq!(
            response.domain,
            Some(Domain {
                urn: Urn("urn:li:domain:finance".to_owned()),
                name: "Finance".to_owned(),
                description: Some("Financial data".to_owned()),
            })
        );
        assert_eq!(
            response.data_products,
            vec![DataProduct {
                urn: Urn("urn:li:dataProduct:orders".to_owned()),
                name: "Orders".to_owned(),
                description: Some("Order data".to_owned()),
            }]
        );

        // Both owners share an ownership type, so they land in the same bucket rather than two.
        let technical_owner = Urn("urn:li:ownershipType:__system__technical_owner".to_owned());
        assert_eq!(response.owners.len(), 1);
        let owners = &response.owners[&technical_owner];
        assert_eq!(
            owners.ownership_type_name.as_deref(),
            Some("Technical Owner")
        );
        assert_eq!(
            owners.users,
            vec![User {
                urn: Urn("urn:li:corpuser:alice".to_owned()),
                full_name: Some("Alice Example".to_owned()),
                display_name: "Alice".to_owned(),
                email: Some("alice@example.com".to_owned()),
                active: true,
            }]
        );
        assert_eq!(
            owners.groups,
            vec![Group {
                urn: Urn("urn:li:corpGroup:analytics".to_owned()),
                display_name: "Analytics".to_owned(),
                description: Some("The team".to_owned()),
            }]
        );
    }

    /// An entity whose referenced tag, domain and data product have no `properties` aspect. There is no
    /// name to show, so each falls back to its URN rather than to an empty string, which would render
    /// as a nameless entry in a policy decision.
    #[test]
    fn entities_without_properties_fall_back_to_urns() {
        let response = entity_from(json!({
            "tags": {"tags": [{"tag": {"urn": "urn:li:tag:PII"}}]},
            "domain": {"domain": {"urn": "urn:li:domain:finance"}},
            "dataProducts": {
                "total": 1,
                "relationships": [{"entity": {"urn": "urn:li:dataProduct:orders"}}],
            },
        }))
        .into_response(urn());

        assert_eq!(response.tags[0].name, "urn:li:tag:PII");

        let domain = response.domain.expect("the domain is present");
        assert_eq!(domain.name, "urn:li:domain:finance");
        assert_eq!(domain.description, None);

        assert_eq!(response.data_products[0].name, "urn:li:dataProduct:orders");
        assert_eq!(response.data_products[0].description, None);
    }

    /// Owners whose `properties` aspect is missing entirely, and owners where it exists but carries no
    /// display name. A user's display name is derived from the URN; a group's falls back to the whole
    /// URN, as there is no group-name convention to strip.
    #[rstest]
    #[case::no_properties_aspect(json!({"__typename": "CorpUser", "urn": "urn:li:corpuser:alice"}))]
    #[case::no_display_name(
        json!({"__typename": "CorpUser", "urn": "urn:li:corpuser:alice", "properties": {}})
    )]
    fn users_without_a_display_name_are_named_after_their_urn(#[case] owner: serde_json::Value) {
        let response =
            entity_from(json!({"ownership": {"owners": [{"owner": owner}]}})).into_response(urn());

        let owners = response
            .owners
            .values()
            .next()
            .expect("the owner is present");
        assert_eq!(owners.users[0].display_name, "alice");
        assert_eq!(owners.users[0].full_name, None);
        assert_eq!(owners.users[0].email, None);
        // Absent `active` means we must not report the user as deactivated.
        assert!(owners.users[0].active);
    }

    #[rstest]
    #[case::no_properties_aspect(
        json!({"__typename": "CorpGroup", "urn": "urn:li:corpGroup:analytics"})
    )]
    #[case::no_display_name(
        json!({"__typename": "CorpGroup", "urn": "urn:li:corpGroup:analytics", "properties": {}})
    )]
    fn groups_without_a_display_name_are_named_after_their_urn(#[case] owner: serde_json::Value) {
        let response =
            entity_from(json!({"ownership": {"owners": [{"owner": owner}]}})).into_response(urn());

        let owners = response
            .owners
            .values()
            .next()
            .expect("the owner is present");
        assert_eq!(owners.groups[0].display_name, "urn:li:corpGroup:analytics");
        assert_eq!(owners.groups[0].description, None);
    }

    /// Owners predating ownership type entities only carry the legacy `type` enum, and some carry
    /// neither. The key has to stay stable either way, because it is what a policy looks owners up by.
    #[rstest]
    #[case::legacy_type_only(json!({"type": "TECHNICAL_OWNER"}), "TECHNICAL_OWNER")]
    #[case::no_type_at_all(json!({}), "unknown")]
    fn owners_without_an_ownership_type_entity_fall_back_to_the_legacy_type(
        #[case] extra_owner_fields: serde_json::Value,
        #[case] expected_key: &str,
    ) {
        let mut owner = json!({
            "owner": {"__typename": "CorpUser", "urn": "urn:li:corpuser:alice"},
        });
        owner
            .as_object_mut()
            .expect("the owner is a JSON object")
            .extend(
                extra_owner_fields
                    .as_object()
                    .expect("the extra fields are a JSON object")
                    .clone(),
            );

        let response = entity_from(json!({"ownership": {"owners": [owner]}})).into_response(urn());

        let (key, owners) = response.owners.iter().next().expect("the owner is present");
        assert_eq!(key.0, expected_key);
        // There is no ownership type entity, so there is no human-readable name for it either.
        assert_eq!(owners.ownership_type_name, None);
    }

    /// The "no metadata" entity we substitute for a URN DataHub does not know must map to a response
    /// that is empty rather than one that fails to build.
    #[test]
    fn the_default_entity_maps_to_an_empty_response() {
        let response = Entity::default().into_response(urn());

        assert_eq!(response.urn, urn());
        assert!(response.tags.is_empty());
        assert_eq!(response.domain, None);
        assert!(response.data_products.is_empty());
        assert!(response.owners.is_empty());
    }

    /// The display name we fall back to is the URN with its prefix removed exactly once, so that a
    /// username that happens to look like a URN itself survives intact.
    #[rstest]
    #[case::plain_user("urn:li:corpuser:alice", "alice")]
    #[case::username_looking_like_a_urn(
        "urn:li:corpuser:urn:li:corpuser:alice",
        "urn:li:corpuser:alice"
    )]
    #[case::not_a_user_urn("urn:li:corpGroup:analytics", "urn:li:corpGroup:analytics")]
    fn the_user_urn_prefix_is_stripped_once(#[case] urn: &str, #[case] expected: &str) {
        assert_eq!(strip_user_urn(&Urn(urn.to_owned())), expected);
    }

    /// A resolver that fails nulls out the list it was resolving, which must read as "nothing here"
    /// rather than failing the whole payload.
    #[test]
    fn null_lists_are_read_as_empty() {
        let response = entity_from(json!({
            "tags": {"tags": null},
            "ownership": {"owners": null},
            "dataProducts": {"total": 0, "relationships": null},
        }))
        .into_response(urn());

        assert!(response.tags.is_empty());
        assert!(response.owners.is_empty());
        assert!(response.data_products.is_empty());
    }

    /// `errors` is the only part of the response that says why a query failed, so it has to survive
    /// a `data` payload we cannot read (here an owner where an object was expected).
    #[test]
    fn errors_survive_an_unreadable_payload() {
        let response: GraphQlResponse = serde_json::from_value(json!({
            "data": {"entity": {"ownership": {"owners": [{"owner": 42}]}}},
            "errors": [{"message": "Failed to resolve ownership"}],
        }))
        .expect("the errors must be readable regardless of the payload");

        let messages = response
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, ["Failed to resolve ownership"]);
    }
}
