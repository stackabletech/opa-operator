use stackable_opa_crd::OpaCluster;
use stackable_operator::crd::CustomResourceExt;

fn main() -> Result<(), stackable_operator::error::Error> {
    built::write_built_file().expect("Failed to acquire build-time information");

    OpaCluster::write_yaml_schema("../../deploy/crd/opacluster.crd.yaml")?;

    Ok(())
}
