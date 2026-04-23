use std::collections::BTreeMap;
use std::path::Path;

use reqwest::{ClientBuilder, header};
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1;
use stackable_operator::commons::tls_verification::TlsClientDetails;
use url::Url;

use crate::{
    ResourceInfoRequest,
    utils::{self, http::send_json_request},
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to read credential file {path:?}"))]
    ReadCredential {
        source: std::io::Error,
        path: std::path::PathBuf,
    },

    #[snafu(display("credentials secret must contain a `token` key"))]
    MissingToken,

    #[snafu(display("failed to build HTTP client"))]
    BuildHttpClient { source: reqwest::Error },

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: crate::utils::tls::Error },

    #[snafu(display("failed to parse OpenMetadata endpoint URL"))]
    ParseEndpoint { source: url::ParseError },

    #[snafu(display("OpenMetadata request failed"))]
    Request { source: crate::utils::http::Error },
}

impl crate::http_error::Error for Error {
    fn status_code(&self) -> hyper::StatusCode {
        use hyper::StatusCode;
        match self {
            Self::ReadCredential { .. } | Self::MissingToken => StatusCode::SERVICE_UNAVAILABLE,
            Self::BuildHttpClient { .. }
            | Self::ConfigureTls { .. }
            | Self::ParseEndpoint { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Request { .. } => StatusCode::BAD_GATEWAY,
        }
    }
}

pub struct ResolvedOpenMetadataBackend {
    endpoint: url::Url,
    http_client: reqwest::Client,
    token: String,
}

impl ResolvedOpenMetadataBackend {
    pub async fn resolve(
        config: v1alpha1::OpenMetadataBackend,
        credentials_dir: &Path,
    ) -> Result<Self, Error> {
        let token_path = credentials_dir.join("token");
        if !token_path.exists() {
            return Err(Error::MissingToken);
        }
        let token = tokio::fs::read_to_string(&token_path)
            .await
            .with_context(|_| ReadCredentialSnafu {
                path: token_path.clone(),
            })?
            .trim()
            .to_owned();

        let mut builder = ClientBuilder::new();
        builder = utils::tls::configure_reqwest(
            &TlsClientDetails {
                tls: config.tls.clone(),
            },
            builder,
        )
        .await
        .context(ConfigureTlsSnafu)?;
        let http_client = builder.build().context(BuildHttpClientSnafu)?;

        let endpoint = url::Url::parse(&config.endpoint).context(ParseEndpointSnafu)?;

        Ok(Self {
            endpoint,
            http_client,
            token,
        })
    }

    pub async fn get_resource_info(
        &self,
        req: &ResourceInfoRequest,
    ) -> Result<crate::ResourceInfo, Error> {
        let url = build_table_by_fqn_url(&self.endpoint, &req.id);

        let request = self
            .http_client
            .get(url)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));

        let table: TableResponse = send_json_request(request).await.context(RequestSnafu)?;
        Ok(table.into_resource_info())
    }
}

/// Builds the OpenMetadata "get table by FQN" URL for a given server endpoint
/// and fully-qualified table name. The FQN is URL-encoded into the path so
/// dots stay intact but other special characters (spaces, `/`, etc.) are
/// percent-encoded.
pub fn build_table_by_fqn_url(endpoint: &Url, fqn: &str) -> Url {
    let mut url = endpoint.clone();
    url.path_segments_mut()
        .expect("endpoint must have a base")
        .pop_if_empty()
        .push("api")
        .push("v1")
        .push("tables")
        .push("name")
        .push(fqn);
    url.query_pairs_mut().append_pair(
        "fields",
        "tags,owners,columns,domain,dataProducts,extension,glossaryTerm",
    );
    url
}

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TableResponse {
    pub fully_qualified_name: String,
    #[serde(default)]
    pub tags: Vec<TagLabel>,
    #[serde(default)]
    pub glossary_terms: Vec<GlossaryTermLabel>,
    #[serde(default)]
    pub owners: Vec<Owner>,
    pub domain: Option<DomainRef>,
    #[serde(default)]
    pub data_products: Vec<DataProductRef>,
    #[serde(default)]
    pub extension: Option<serde_json::Value>,
    #[serde(default)]
    pub columns: Vec<ColumnResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TagLabel {
    #[serde(rename = "tagFQN")]
    pub tag_fqn: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GlossaryTermLabel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Owner {
    #[serde(rename = "type")]
    pub type_: String, // "user" | "team"
    pub name: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomainRef {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DataProductRef {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ColumnResponse {
    pub name: String,
    pub data_type: String,
    #[serde(default)]
    pub tags: Vec<TagLabel>,
    #[serde(default)]
    pub glossary_terms: Vec<GlossaryTermLabel>,
}

use crate::{FieldInfo, ResourceInfo};

impl TableResponse {
    pub fn into_resource_info(self) -> ResourceInfo {
        let tags = self.tags.into_iter().map(|t| t.tag_fqn).collect();
        let glossary_terms = self.glossary_terms.into_iter().map(|g| g.name).collect();

        let owners = self
            .owners
            .into_iter()
            .map(|o| {
                let prefix = match o.type_.as_str() {
                    "user" => "user",
                    "team" => "group",
                    other => other,
                };
                format!("{prefix}:{}", o.name)
            })
            .collect();

        let domain = self.domain.map(|d| d.name);
        let data_products = self.data_products.into_iter().map(|dp| dp.name).collect();

        let custom_properties: BTreeMap<String, serde_json::Value> = match self.extension {
            Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
            _ => BTreeMap::new(),
        };

        let fields = self
            .columns
            .into_iter()
            .map(|c| {
                (
                    c.name,
                    FieldInfo {
                        type_: c.data_type,
                        tags: c.tags.into_iter().map(|t| t.tag_fqn).collect(),
                        glossary_terms: c.glossary_terms.into_iter().map(|g| g.name).collect(),
                    },
                )
            })
            .collect();

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
mod response_tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/openmetadata_table.json");

    #[test]
    fn deserialize_full_fixture() {
        let t: TableResponse = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(t.fully_qualified_name, "mysql.mydb.public.orders");
        assert_eq!(t.tags.len(), 2);
        assert_eq!(t.glossary_terms.len(), 1);
        assert_eq!(t.owners.len(), 2);
        assert_eq!(t.domain.as_ref().unwrap().name, "Finance");
        assert_eq!(t.data_products.len(), 1);
        assert!(t.extension.as_ref().is_some());
        assert_eq!(t.columns.len(), 2);
    }
}

#[cfg(test)]
mod transform_tests_om {
    use super::*;
    use crate::ResourceInfo;

    const FIXTURE: &str = include_str!("fixtures/openmetadata_table.json");

    #[test]
    fn fixture_transforms_to_expected_resource_info() {
        let t: TableResponse = serde_json::from_str(FIXTURE).unwrap();
        let info: ResourceInfo = t.into_resource_info();

        assert_eq!(info.tags, vec!["PII.Sensitive", "GDPR.Personal"]);
        assert_eq!(info.glossary_terms, vec!["CustomerPII"]);
        assert_eq!(
            info.owners,
            vec!["user:alice", "group:data-platform-team"]
        );
        assert_eq!(info.domain.as_deref(), Some("Finance"));
        assert_eq!(info.data_products, vec!["Customer360"]);
        assert_eq!(
            info.custom_properties.get("sensitivityLevel"),
            Some(&serde_json::Value::String("High".to_owned()))
        );
        // linkedEntity preserved as native JSON object
        assert!(info.custom_properties["linkedEntity"].is_object());

        let cid = info.fields.get("customer_id").unwrap();
        assert_eq!(cid.type_, "VARCHAR");
        assert_eq!(cid.tags, vec!["PII.Sensitive"]);
        assert_eq!(cid.glossary_terms, vec!["CustomerIdentifier"]);

        let sts = info.fields.get("signup_ts").unwrap();
        assert_eq!(sts.type_, "TIMESTAMP");
        assert!(sts.tags.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fqn_url_simple() {
        let endpoint = Url::parse("http://om:8585").unwrap();
        let url = build_table_by_fqn_url(&endpoint, "mysql.mydb.public.orders");
        assert_eq!(
            url.as_str(),
            "http://om:8585/api/v1/tables/name/mysql.mydb.public.orders?fields=tags%2Cowners%2Ccolumns%2Cdomain%2CdataProducts%2Cextension%2CglossaryTerm"
        );
    }

    #[test]
    fn fqn_url_with_trailing_slash_endpoint() {
        let endpoint = Url::parse("http://om:8585/").unwrap();
        let url = build_table_by_fqn_url(&endpoint, "svc.db.sch.tbl");
        assert!(url.path().starts_with("/api/v1/tables/name/svc.db.sch.tbl"));
    }

    #[test]
    fn fqn_url_encodes_spaces() {
        let endpoint = Url::parse("http://om:8585").unwrap();
        let url = build_table_by_fqn_url(&endpoint, "svc.db.sch.my table");
        assert!(url.path().contains("my%20table"));
    }
}
