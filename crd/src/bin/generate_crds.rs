use stackable_opa_crd::OpenPolicyAgent;
use stackable_operator::crd::CustomResourceExt;

fn main() {
    let target_file = "deploy/crd/opa.crd.yaml";
    OpenPolicyAgent::write_yaml_schema(target_file).unwrap();
    println!("Wrote CRD to [{}]", target_file);
}
