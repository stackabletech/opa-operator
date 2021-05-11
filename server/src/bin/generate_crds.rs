use stackable_opa_crd::OpenPolicyAgent;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!(
        "OpenPolicyAgent CRD:\n{}\n",
        serde_yaml::to_string(&OpenPolicyAgent::crd())?
    );
    Ok(())
}
