use std::path::Path;

use snafu::{ResultExt, Snafu};
use url::Url;

/// DataHub auth method resolved from the mounted credentials directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataHubAuthMethod {
    Basic { username: String, password: String },
    Bearer { token: String, actor: String },
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to read credential file {path:?}"))]
    ReadCredential {
        source: std::io::Error,
        path: std::path::PathBuf,
    },

    #[snafu(display(
        "credentials secret must contain either `username`+`password` or `token`+`actor`"
    ))]
    NoCredentials,
}

/// Inspects `credentials_dir` for credential files and returns the resolved auth method.
///
/// Precedence: if `username` and `password` both exist, Basic wins, ignoring any
/// token files. If only `token` and `actor` exist, Bearer is used. Otherwise
/// `Error::NoCredentials` is returned.
pub async fn detect_auth_method(credentials_dir: &Path) -> Result<DataHubAuthMethod, Error> {
    let username_path = credentials_dir.join("username");
    let password_path = credentials_dir.join("password");
    let token_path = credentials_dir.join("token");
    let actor_path = credentials_dir.join("actor");

    if username_path.exists() && password_path.exists() {
        let username = read_trim(&username_path).await?;
        let password = read_trim(&password_path).await?;
        return Ok(DataHubAuthMethod::Basic { username, password });
    }
    if token_path.exists() && actor_path.exists() {
        let token = read_trim(&token_path).await?;
        let actor = read_trim(&actor_path).await?;
        return Ok(DataHubAuthMethod::Bearer { token, actor });
    }
    Err(Error::NoCredentials)
}

async fn read_trim(path: &Path) -> Result<String, Error> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .with_context(|_| ReadCredentialSnafu {
            path: path.to_path_buf(),
        })?;
    Ok(raw.trim().to_owned())
}

/// Builds the dataset URN `urn:li:dataset:(urn:li:dataPlatform:{platform},{id},{environment})`
/// used by DataHub's GraphQL `dataset(urn: …)` query.
pub fn build_dataset_urn(platform: &str, id: &str, environment: &str) -> String {
    format!("urn:li:dataset:(urn:li:dataPlatform:{platform},{id},{environment})")
}

/// Parses and returns the GraphQL endpoint URL configured for the backend.
pub fn parse_graphql_endpoint(endpoint: &str) -> Result<Url, url::ParseError> {
    Url::parse(endpoint)
}

#[cfg(test)]
mod cred_tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn detects_basic_auth() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("username"), "sys_user").await.unwrap();
        fs::write(dir.path().join("password"), "s3cret").await.unwrap();

        let auth = detect_auth_method(dir.path()).await.unwrap();
        match auth {
            DataHubAuthMethod::Basic { username, password } => {
                assert_eq!(username, "sys_user");
                assert_eq!(password, "s3cret");
            }
            _ => panic!("expected Basic"),
        }
    }

    #[tokio::test]
    async fn detects_bearer_auth() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("token"), "tok123").await.unwrap();
        fs::write(dir.path().join("actor"), "bot_account").await.unwrap();

        let auth = detect_auth_method(dir.path()).await.unwrap();
        match auth {
            DataHubAuthMethod::Bearer { token, actor } => {
                assert_eq!(token, "tok123");
                assert_eq!(actor, "bot_account");
            }
            _ => panic!("expected Bearer"),
        }
    }

    #[tokio::test]
    async fn basic_wins_when_both_sets_present() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("username"), "u").await.unwrap();
        fs::write(dir.path().join("password"), "p").await.unwrap();
        fs::write(dir.path().join("token"), "t").await.unwrap();
        fs::write(dir.path().join("actor"), "a").await.unwrap();

        assert!(matches!(
            detect_auth_method(dir.path()).await.unwrap(),
            DataHubAuthMethod::Basic { .. }
        ));
    }

    #[tokio::test]
    async fn error_when_no_credentials() {
        let dir = tempdir().unwrap();
        let err = detect_auth_method(dir.path()).await.unwrap_err();
        assert!(matches!(err, Error::NoCredentials));
    }
}

use serde::Deserialize;

/// GraphQL query covering every field required to populate the normalised
/// `ResourceInfo`. Field names match DataHub's `dataset` schema. Exact subfield
/// names for tags/glossary at column level (`globalTags` vs `tags`) may vary
/// between DataHub versions — cross-check during integration.
pub const DATASET_QUERY: &str = r#"
query Dataset($urn: String!) {
  dataset(urn: $urn) {
    tags { tags { tag { urn } } }
    glossaryTerms { terms { term { urn } } }
    ownership {
      owners {
        owner {
          __typename
          ... on CorpUser { urn properties { email } }
          ... on CorpGroup { urn properties { displayName } }
        }
      }
    }
    domain { domain { properties { name } } }
    dataProducts { dataProducts { properties { name } } }
    properties { customProperties { key value } }
    editableSchemaMetadata {
      editableSchemaFieldInfo {
        fieldPath
        tags { tags { tag { urn } } }
        glossaryTerms { terms { term { urn } } }
      }
    }
    schemaMetadata {
      fields {
        fieldPath
        type
        globalTags { tags { tag { urn } } }
        glossaryTerms { terms { term { urn } } }
      }
    }
  }
}
"#;

#[derive(Debug, Deserialize)]
pub(crate) struct GraphqlResponse {
    pub data: GraphqlData,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GraphqlData {
    pub dataset: Option<DatasetResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DatasetResponse {
    pub tags: Option<TagContainer>,
    pub glossary_terms: Option<GlossaryTermContainer>,
    pub ownership: Option<OwnershipContainer>,
    pub domain: Option<DomainContainer>,
    pub data_products: Option<DataProductsContainer>,
    pub properties: Option<DatasetProperties>,
    pub editable_schema_metadata: Option<EditableSchemaMetadata>,
    pub schema_metadata: Option<SchemaMetadata>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TagContainer {
    pub tags: Vec<TagAssociation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TagAssociation {
    pub tag: TagRef,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TagRef {
    pub urn: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GlossaryTermContainer {
    pub terms: Vec<GlossaryTermAssociation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GlossaryTermAssociation {
    pub term: TermRef,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TermRef {
    pub urn: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OwnershipContainer {
    pub owners: Vec<OwnerAssociation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OwnerAssociation {
    pub owner: OwnerRef,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub(crate) enum OwnerRef {
    CorpUser { urn: String, properties: Option<CorpUserProperties> },
    CorpGroup { urn: String, properties: Option<CorpGroupProperties> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CorpUserProperties {
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CorpGroupProperties {
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DomainContainer {
    pub domain: Option<DomainInner>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DomainInner {
    pub properties: DomainProperties,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DomainProperties {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DataProductsContainer {
    pub data_products: Vec<DataProductRef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DataProductRef {
    pub properties: DataProductProperties,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DataProductProperties {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DatasetProperties {
    pub custom_properties: Vec<CustomProperty>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CustomProperty {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditableSchemaMetadata {
    pub editable_schema_field_info: Vec<EditableSchemaFieldInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditableSchemaFieldInfo {
    pub field_path: String,
    pub tags: Option<TagContainer>,
    pub glossary_terms: Option<GlossaryTermContainer>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SchemaMetadata {
    pub fields: Vec<SchemaField>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SchemaField {
    pub field_path: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub global_tags: Option<TagContainer>,
    pub glossary_terms: Option<GlossaryTermContainer>,
}

#[cfg(test)]
mod graphql_tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/datahub_dataset.json");

    #[test]
    fn deserialize_full_fixture() {
        let envelope: GraphqlResponse = serde_json::from_str(FIXTURE).unwrap();
        let ds = envelope.data.dataset.expect("dataset present in fixture");

        assert_eq!(ds.tags.as_ref().unwrap().tags.len(), 2);
        assert_eq!(ds.glossary_terms.as_ref().unwrap().terms.len(), 1);
        assert_eq!(ds.ownership.as_ref().unwrap().owners.len(), 2);
        assert_eq!(
            ds.domain.as_ref().and_then(|d| d.domain.as_ref()).map(|d| d.properties.name.as_str()),
            Some("Finance")
        );
        assert_eq!(ds.data_products.as_ref().unwrap().data_products.len(), 1);
        assert_eq!(
            ds.properties.as_ref().unwrap().custom_properties.len(),
            2
        );
        assert_eq!(ds.schema_metadata.as_ref().unwrap().fields.len(), 2);
    }
}

use std::collections::BTreeMap;

use crate::{FieldInfo, ResourceInfo};

const TAG_URN_PREFIX: &str = "urn:li:tag:";
const GLOSSARY_URN_PREFIX: &str = "urn:li:glossaryTerm:";
const CORP_GROUP_URN_PREFIX: &str = "urn:li:corpGroup:";

fn strip(urn: &str, prefix: &str) -> String {
    urn.strip_prefix(prefix).unwrap_or(urn).to_owned()
}

fn tags_to_names(container: Option<&TagContainer>) -> Vec<String> {
    container
        .map(|c| {
            c.tags
                .iter()
                .map(|t| strip(&t.tag.urn, TAG_URN_PREFIX))
                .collect()
        })
        .unwrap_or_default()
}

fn glossary_to_names(container: Option<&GlossaryTermContainer>) -> Vec<String> {
    container
        .map(|c| {
            c.terms
                .iter()
                .map(|t| strip(&t.term.urn, GLOSSARY_URN_PREFIX))
                .collect()
        })
        .unwrap_or_default()
}

impl DatasetResponse {
    pub fn into_resource_info(self) -> ResourceInfo {
        // Dataset-level tags + glossary.
        let tags = tags_to_names(self.tags.as_ref());
        let glossary_terms = glossary_to_names(self.glossary_terms.as_ref());

        // Owners with user:/group: prefixes.
        let owners = self
            .ownership
            .as_ref()
            .map(|o| {
                o.owners
                    .iter()
                    .filter_map(|assoc| match &assoc.owner {
                        OwnerRef::CorpUser { urn, properties } => {
                            let id = properties
                                .as_ref()
                                .and_then(|p| p.email.as_deref())
                                .unwrap_or_else(|| urn.rsplit(':').next().unwrap_or(urn));
                            Some(format!("user:{id}"))
                        }
                        OwnerRef::CorpGroup { urn, properties } => {
                            let id = properties
                                .as_ref()
                                .and_then(|p| p.display_name.as_deref())
                                .unwrap_or_else(|| {
                                    urn.strip_prefix(CORP_GROUP_URN_PREFIX).unwrap_or(urn)
                                });
                            Some(format!("group:{id}"))
                        }
                        OwnerRef::Other => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Domain (first non-null).
        let domain = self
            .domain
            .and_then(|d| d.domain)
            .map(|inner| inner.properties.name);

        // Data products.
        let data_products = self
            .data_products
            .map(|dp| {
                dp.data_products
                    .into_iter()
                    .map(|p| p.properties.name)
                    .collect()
            })
            .unwrap_or_default();

        // Custom properties flat KV -> JSON strings.
        let custom_properties: BTreeMap<String, serde_json::Value> = self
            .properties
            .map(|p| {
                p.custom_properties
                    .into_iter()
                    .map(|cp| (cp.key, serde_json::Value::String(cp.value)))
                    .collect()
            })
            .unwrap_or_default();

        // Field-level: merge ingested (schemaMetadata) with editable (editableSchemaMetadata).
        let editable_by_path: BTreeMap<String, EditableSchemaFieldInfo> = self
            .editable_schema_metadata
            .map(|esm| {
                esm.editable_schema_field_info
                    .into_iter()
                    .map(|info| (info.field_path.clone(), info))
                    .collect()
            })
            .unwrap_or_default();

        let fields: BTreeMap<String, FieldInfo> = self
            .schema_metadata
            .map(|sm| {
                sm.fields
                    .into_iter()
                    .map(|f| {
                        let mut tag_set: std::collections::BTreeSet<String> = f
                            .global_tags
                            .as_ref()
                            .into_iter()
                            .flat_map(|c| c.tags.iter().map(|t| strip(&t.tag.urn, TAG_URN_PREFIX)))
                            .collect();
                        let mut gloss_set: std::collections::BTreeSet<String> = f
                            .glossary_terms
                            .as_ref()
                            .into_iter()
                            .flat_map(|c| {
                                c.terms.iter().map(|t| strip(&t.term.urn, GLOSSARY_URN_PREFIX))
                            })
                            .collect();
                        if let Some(ed) = editable_by_path.get(&f.field_path) {
                            if let Some(tc) = &ed.tags {
                                for t in &tc.tags {
                                    tag_set.insert(strip(&t.tag.urn, TAG_URN_PREFIX));
                                }
                            }
                            if let Some(gc) = &ed.glossary_terms {
                                for t in &gc.terms {
                                    gloss_set.insert(strip(&t.term.urn, GLOSSARY_URN_PREFIX));
                                }
                            }
                        }
                        (
                            f.field_path,
                            FieldInfo {
                                type_: f.type_,
                                tags: tag_set.into_iter().collect(),
                                glossary_terms: gloss_set.into_iter().collect(),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        ResourceInfo {
            tags,
            glossary_terms,
            owners,
            domain,
            data_products,
            custom_properties,
            custom_attributes: BTreeMap::new(),
            fields,
        }
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;
    use crate::{FieldInfo, ResourceInfo};

    const FIXTURE: &str = include_str!("fixtures/datahub_dataset.json");

    #[test]
    fn fixture_transforms_to_expected_resource_info() {
        let envelope: GraphqlResponse = serde_json::from_str(FIXTURE).unwrap();
        let ds = envelope.data.dataset.expect("dataset present");
        let info: ResourceInfo = ds.into_resource_info();

        assert_eq!(info.tags, vec!["pii", "gdpr"]);
        assert_eq!(info.glossary_terms, vec!["CustomerPII"]);
        assert_eq!(
            info.owners,
            vec![
                "user:alice@example.com",
                "group:data-platform-team"
            ]
        );
        assert_eq!(info.domain.as_deref(), Some("Finance"));
        assert_eq!(info.data_products, vec!["Customer360"]);
        assert_eq!(
            info.custom_properties.get("sensitivityLevel"),
            Some(&serde_json::Value::String("High".to_owned()))
        );
        assert_eq!(info.custom_properties.get("retentionYears"),
            Some(&serde_json::Value::String("7".to_owned())));
        assert!(info.custom_attributes.is_empty());

        // Fields
        let cid = info.fields.get("customer_id").expect("customer_id present");
        assert_eq!(cid.type_, "STRING");
        assert_eq!(cid.tags, vec!["pii"]);
        assert_eq!(cid.glossary_terms, vec!["CustomerIdentifier"]);

        let sts = info.fields.get("signup_ts").expect("signup_ts present");
        assert_eq!(sts.type_, "TIMESTAMP");
        assert!(sts.tags.is_empty());
        assert!(sts.glossary_terms.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_urn_trino_prod() {
        let urn = build_dataset_urn("trino", "hive.db.table", "PROD");
        assert_eq!(
            urn,
            "urn:li:dataset:(urn:li:dataPlatform:trino,hive.db.table,PROD)"
        );
    }

    #[test]
    fn dataset_urn_nested_id() {
        let urn = build_dataset_urn("hive", "my_catalog.my_schema.my_table", "DEV");
        assert_eq!(
            urn,
            "urn:li:dataset:(urn:li:dataPlatform:hive,my_catalog.my_schema.my_table,DEV)"
        );
    }

    #[test]
    fn graphql_endpoint_parse_ok() {
        let url = parse_graphql_endpoint("http://datahub-gms:8080/api/graphql").unwrap();
        assert_eq!(url.host_str(), Some("datahub-gms"));
        assert_eq!(url.path(), "/api/graphql");
    }
}
