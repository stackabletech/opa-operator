use url::Url;

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
