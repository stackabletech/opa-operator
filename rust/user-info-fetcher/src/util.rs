use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;

pub async fn send_json_request<T: DeserializeOwned>(
    req: RequestBuilder,
) -> Result<T, reqwest::Error> {
    req.send().await?.error_for_status()?.json().await
}
