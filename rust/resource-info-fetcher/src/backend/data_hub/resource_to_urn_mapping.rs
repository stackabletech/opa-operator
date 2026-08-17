use std::collections::BTreeMap;

use serde::Serialize;
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1;

use crate::{
    api::{
        Chart, Dashboard, Database, ParamValue, RawIdentifier, ResourceInfoRequest, Schema, Stream,
        Table,
    },
    backend::data_hub::{Error, InvalidResourceNameSnafu, Urn},
};

/// Maps a request to the URN of the DataHub entity that holds the resource's metadata.
///
/// `env` is DataHub's fabric (e.g. `PROD`) and comes from the backend configuration rather than from
/// the request - see [`v1alpha1::DataHubBackend::env`] for why. It is part of every dataset URN, so
/// it has to match the `env` the metadata was ingested with, otherwise the URN does not resolve.
pub fn urn_for_request(
    request: &ResourceInfoRequest,
    env: &v1alpha1::FabricType,
) -> Result<Urn, Error> {
    let urn = match request {
        ResourceInfoRequest::Database(Database {
            system,
            instance,
            database,
        }) => {
            reject_urn_delimiters(&[system, instance, database])?;
            container_urn(&BTreeMap::from([
                ("platform", system),
                ("instance", instance),
                ("database", database),
            ]))
        }
        ResourceInfoRequest::Schema(Schema {
            system,
            instance,
            database,
            schema,
        }) => {
            reject_urn_delimiters(&[system, instance, database, schema])?;
            container_urn(&BTreeMap::from([
                ("platform", system),
                ("instance", instance),
                ("database", database),
                ("schema", schema),
            ]))
        }
        ResourceInfoRequest::Table(Table {
            system,
            instance,
            database,
            schema,
            table,
        }) => {
            reject_urn_delimiters(&[system, instance, database, schema, table])?;
            format!(
                "urn:li:dataset:(urn:li:dataPlatform:{system},{instance}.{database}.{schema}.{table},{env})"
            )
        }
        ResourceInfoRequest::Stream(Stream {
            system,
            instance,
            queue,
        }) => {
            reject_urn_delimiters(&[system, instance, queue])?;
            format!("urn:li:dataset:(urn:li:dataPlatform:{system},{instance}.{queue},{env})")
        }
        ResourceInfoRequest::Dashboard(Dashboard {
            system,
            instance,
            id,
        }) => {
            reject_urn_delimiters(&[system, instance, id])?;
            format!("urn:li:dashboard:({system},{instance}.{id})")
        }
        ResourceInfoRequest::Chart(Chart {
            system,
            instance,
            id,
        }) => {
            reject_urn_delimiters(&[system, instance, id])?;
            format!("urn:li:chart:({system},{instance}.{id})")
        }
        // Deliberately unchecked: a raw identifier *is* a URN, so it necessarily contains the
        // delimiters that are rejected above.
        ResourceInfoRequest::RawIdentifier(RawIdentifier { identifier }) => identifier.to_string(),
    };

    Ok(Urn(urn))
}

/// The characters DataHub's URN grammar uses to delimit the parts of a URN.
///
/// A resource name containing one of these cannot be expressed as a URN at all - DataHub's own parser
/// would split the name apart - so no URN we could build for it would ever resolve.
const URN_DELIMITERS: [char; 3] = [',', '(', ')'];

/// Fails if any of `names` contains a [`URN_DELIMITERS`] character.
///
/// This is checked before querying DataHub rather than after: the query is guaranteed to fail, and any
/// caller who can name a resource could otherwise turn every such name into a round trip to DataHub
/// plus a log line - for a Trino table, `SELECT * FROM tpch.sf1."a,PROD)"` is enough.
fn reject_urn_delimiters(names: &[&ParamValue]) -> Result<(), Error> {
    for name in names {
        if let Some(delimiter) = name.as_ref().find(URN_DELIMITERS) {
            let name = name.to_string();
            let delimiter = name[delimiter..]
                .chars()
                .next()
                .expect("the match starts at a character boundary");

            return InvalidResourceNameSnafu { name, delimiter }.fail();
        }
    }

    Ok(())
}

/// Reproduces DataHub's `datahub_guid`: the container key is serialized to compact, key-sorted JSON
/// and MD5-hashed. A [`BTreeMap`] yields sorted keys and `serde_json` emits no whitespace, which
/// matches Python's `json.dumps(key, sort_keys=True, separators=(",", ":"))`.
///
/// Note that the SQL ingestion source sets `backcompat_env_as_instance`, so the `env` configured for
/// the ingestion (e.g. `PROD`) ends up in the `instance` field and no `env` field is present in the
/// key. This is why the callers below build container keys without an `env`, even though the
/// configured [`v1alpha1::FabricType`] is part of the dataset URNs. The `platform` is the bare
/// platform name (e.g. `trino`), *not* the `urn:li:dataPlatform:` form.
fn container_urn(
    container_key: &BTreeMap<impl AsRef<str> + Serialize, impl AsRef<str> + Serialize>,
) -> String {
    let key_json = serde_json::to_string(container_key)
        .expect("serializing a BTreeMap<&str, &str> cannot fail");
    format!("urn:li:container:{:x}", md5::compute(key_json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use hyper::StatusCode;
    use rstest::rstest;

    use super::*;

    /// Dashboard and chart ids are opaque to us. Superset happens to number them, but other
    /// platforms (e.g. Looker or Tableau) identify their dashboards by name, so neither the API nor
    /// the URN construction may assume an integer.
    ///
    /// Neither URN carries the fabric, so the configured [`v1alpha1::FabricType`] is irrelevant here.
    #[rstest]
    #[case::numeric_id("1", "urn:li:chart:(superset,my-superset.1)")]
    #[case::non_numeric_id(
        "orders-by-region",
        "urn:li:chart:(superset,my-superset.orders-by-region)"
    )]
    fn chart_urn(#[case] id: &str, #[case] expected_urn: &str) {
        let request = ResourceInfoRequest::Chart(Chart {
            system: "superset".into(),
            instance: "my-superset".into(),
            id: id.into(),
        });

        assert_eq!(urn_of(request).expect("the name is valid").0, expected_urn);
    }

    fn urn_of(request: ResourceInfoRequest) -> Result<Urn, Error> {
        urn_for_request(&request, &v1alpha1::FabricType::Prod)
    }

    fn table_named(table: &str) -> ResourceInfoRequest {
        ResourceInfoRequest::Table(Table {
            system: "trino".into(),
            instance: "my-trino".into(),
            database: "tpch".into(),
            schema: "sf1".into(),
            table: table.into(),
        })
    }

    /// A name containing a URN delimiter cannot be expressed as a DataHub URN at all, so querying
    /// DataHub with it is guaranteed to fail. Rejecting it here keeps a caller who can name a table -
    /// e.g. via Trino's `SELECT * FROM tpch.sf1."a,PROD)"` - from turning every such query into a
    /// round trip to DataHub.
    #[rstest]
    #[case::comma("a,PROD)")]
    #[case::opening_paren("a(b")]
    #[case::closing_paren("a)b")]
    fn names_containing_urn_delimiters_are_rejected(#[case] table: &str) {
        let error = urn_of(table_named(table))
            .expect_err("a name containing a URN delimiter must be rejected");

        assert_eq!(
            info_fetcher_commons::http_error::Error::status_code(&error),
            StatusCode::BAD_REQUEST
        );
    }

    /// Dots are not delimiters: they separate the segments of the dataset name, and a name that
    /// genuinely contains one resolves as long as it was ingested the same way.
    #[rstest]
    #[case::plain("customer")]
    #[case::dotted("a.b")]
    #[case::dashed_and_underscored("my_table-2")]
    #[case::colon("a:b")]
    fn ordinary_names_are_accepted(#[case] table: &str) {
        urn_of(table_named(table)).expect("an ordinary name must be accepted");
    }

    /// `rawIdentifier` is passed through verbatim, and for DataHub it *is* a URN - so it necessarily
    /// contains the very delimiters the other endpoints reject.
    #[test]
    fn raw_identifiers_may_contain_urn_delimiters() {
        let identifier = "urn:li:chart:(superset,my-superset.1)";

        let urn = urn_of(ResourceInfoRequest::RawIdentifier(RawIdentifier {
            identifier: identifier.into(),
        }))
        .expect("a raw identifier must be passed through as-is");

        assert_eq!(urn.0, identifier);
    }

    /// Guards the container hash, which DataHub computes independently on ingestion: it has to keep
    /// matching byte for byte, or every database and schema lookup silently stops resolving. The
    /// expected value was computed with Python's
    /// `json.dumps(key, sort_keys=True, separators=(",", ":"))` and `hashlib.md5`, mirroring what
    /// DataHub's `datahub_guid` does.
    #[test]
    fn schema_container_urn_matches_datahubs_guid() {
        let request = ResourceInfoRequest::Schema(Schema {
            system: "trino".into(),
            instance: "my-namespace/my-trino".into(),
            database: "tpch".into(),
            schema: "sf1".into(),
        });

        assert_eq!(
            urn_of(request).expect("the name is valid").0,
            "urn:li:container:fb46bf1f985e130eeceeee8a51317cd9"
        );
    }

    #[rstest]
    #[case::numeric_id("1", "urn:li:dashboard:(superset,my-superset.1)")]
    #[case::non_numeric_id("sales", "urn:li:dashboard:(superset,my-superset.sales)")]
    fn dashboard_urn(#[case] id: &str, #[case] expected_urn: &str) {
        let request = ResourceInfoRequest::Dashboard(Dashboard {
            system: "superset".into(),
            instance: "my-superset".into(),
            id: id.into(),
        });

        assert_eq!(urn_of(request).expect("the name is valid").0, expected_urn);
    }
}
