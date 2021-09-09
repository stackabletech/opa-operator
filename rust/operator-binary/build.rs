use stackable_opa_crd::OpenPolicyAgent;
use stackable_operator::crd::CustomResourceExt;

fn main() -> Result<(), stackable_operator::error::Error> {
    built::write_built_file().expect("Failed to acquire build-time information");

    OpenPolicyAgent::write_yaml_schema("../../deploy/crd/openpolicyagent.crd.yaml")?;

    Ok(())
}
