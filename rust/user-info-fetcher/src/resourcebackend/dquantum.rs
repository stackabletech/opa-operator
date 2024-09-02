use axum::http::StatusCode;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::util::send_json_request;
use crate::{http_error, Credentials, ResourceInfo, ResourceInfoRequest};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: crate::util::Error },
    #[snafu(display("failed to construct search endpoint path"))]
    ConstructSearchEndpointPath { source: url::ParseError },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        StatusCode::NOT_IMPLEMENTED
    }
}

#[derive(Clone, Deserialize, Debug)]
struct SearchResponse {
    total: u8,
    forbidden: u8,
    offset: u8,
    limit: u8,
    entities: Vec<Entity>,
}

#[derive(Clone, Deserialize, Debug)]
struct Property {
    name: String,
    value: String,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Entity {
    uid: String,
    draft: bool,
    name: String,
    entity_type_id: u8,
    entity_type_name: String,
    archived: bool,
    properties: Vec<Property>,
    created: String,
    created_user: String,
    modified: String,
    modified_user: String,
}

pub(crate) async fn get_resource_info(
    req: &ResourceInfoRequest,
    http: &reqwest::Client,
    credentials: &Credentials,
    config: &crd::DQuantumBackend,
) -> Result<ResourceInfo, crate::resourcebackend::dquantum::Error> {
    let crd::DQuantumBackend {
        url,
        tls,
        client_credentials_secret,
        hierarchy,
    } = config;

    match req {
        ResourceInfoRequest::TrinoTableInfoRequest(request) => {
            let catalog = &request.catalog;
            let schema = &request.schema;
            let table = &request.table;
            let mut headers = HeaderMap::new();
            let request_url =
                format!("{url}/entity/search/146/Name?propertyValue={catalog}/{schema}/{table}");
            let username = &credentials.client_id;
            let password = &credentials.client_secret;
            println!("username: {username}, password: {password} ");
            println!("got request for {catalog}/{schema}/{table} - retrieving url {request_url}");
            headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
            headers.insert(ACCEPT, "application/json".parse().unwrap());
            //headers.insert("X-XSRF-TOKEN", "f434e8e8-081c-4963-9df6-3046bf8bdeb9".parse().unwrap());
            //headers.insert(AUTHORIZATION, "Basic Zmphc2luc2tpOmZqYXNpbnNraQ==".parse().unwrap());

            let table_response =
                send_json_request::<crate::resourcebackend::dquantum::SearchResponse>(
                    http.get(request_url)
                        .headers(headers)
                        .basic_auth("sliebau", Some("sliebau")),
                )
                .await
                .context(AccessTokenSnafu)?;
            println!("Got response from dquantum: {:?}", table_response);
        }
    };

    Ok(ResourceInfo::TrinoTableInfo(Default::default()))
}
