use stackable_opa_crd::OpenPolicyAgent;
use stackable_operator::crd::CustomResourceExt;

fn main() {
    let target_file = "deploy/crd/opa.crd.yaml";
    match OpenPolicyAgent::write_yaml_schema(target_file) {
        Ok(_) => println!("Wrote CRD to [{}]", target_file),
        Err(err) => println!("Could not write CRD to [{}]: {:?}", target_file, err),
    }
}
