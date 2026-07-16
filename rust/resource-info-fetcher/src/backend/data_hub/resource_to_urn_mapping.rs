use crate::api::{ResourceInfoRequest, ResourceInfoRequestResource};


pub fn urn_for_request(request: &ResourceInfoRequest, env: &str) -> String {
    let stacklet = &request.stacklet;
    match &request.resource {
        ResourceInfoRequestResource::TrinoTable {
            catalog,
            schema,
            table,
        } => {
            format!(
                "urn:li:dataset:(urn:li:dataPlatform:{stacklet},{catalog}.{schema}.{table},{env})"
            )
        }
    }
}
