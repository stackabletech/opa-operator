use snafu::Snafu;
use stackable_opa_crd::user_info_fetcher as crd;
use crate::{Credentials, ResourceInfo, ResourceInfoRequest};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get access_token"))]
    AccessToken { source: crate::util::Error },
}

pub(crate) async fn get_resource_info(
    _req: &ResourceInfoRequest,
    _http: &reqwest::Client,
    _credentials: &Credentials,
    config: &crd::DatahubBackend,

) -> Result<ResourceInfo, Error> {
    let crd::DatahubBackend {
        hostname, port, tls, bearer_token_secret
    } = config;

    Ok(ResourceInfo::TrinoTableInfo(Default::default()))
}

