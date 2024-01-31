use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;
use snafu::Snafu;

// TODO write an error type in here

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to execute request"))]
    HttpRequest { source: reqwest::Error },

    #[snafu(display("failed to parse json response"))]
    ParseJson { source: reqwest::Error },

    #[snafu(display("response was an http error"))]
    HttpErrorResponse { source: reqwest::Error },

    #[snafu(display("response was an http error: {text}"))]
    Something { text: String },
}

pub async fn send_json_request<T: DeserializeOwned>(
    req: RequestBuilder,
) -> Result<T, reqwest::Error> {
    // TODO check for differen error sources and send informative errors from here
    let response =  req.send().await?;
    response.error_for_status()?.json().await
}
