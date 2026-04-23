use url::Url;

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
