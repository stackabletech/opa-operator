pub const REGORULES: &[(&str, &str)] = &[
    (
        "stackable/opa/userinfo/v1.rego",
        include_str!("userinfo/v1.rego"),
    ),
    (
        "stackable/opa/resourceinfo/v1.rego",
        include_str!("resourceinfo/v1.rego"),
    ),
];
