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
