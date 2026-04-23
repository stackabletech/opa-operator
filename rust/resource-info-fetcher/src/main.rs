use snafu::Snafu;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const APP_NAME: &str = "opa-resource-info-fetcher";

#[derive(Snafu, Debug)]
enum StartupError {}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    println!("opa-resource-info-fetcher starting (stub)");
    Ok(())
}
