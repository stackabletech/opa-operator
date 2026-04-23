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
