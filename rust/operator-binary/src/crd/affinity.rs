//! The default [`StackableAffinityFragment`] of an OPA role.

use stackable_operator::{
    commons::affinity::{StackableAffinityFragment, affinity_between_role_pods},
    k8s_openapi::api::core::v1::PodAntiAffinity,
};

use crate::crd::{APP_NAME, OpaRole};

/// Weight of the anti-affinity that spreads the Pods of a role across nodes.
///
/// The absolute value only matters once a second, competing term exists; see the `PreferSameNode`
/// note on [`get_affinity`].
const ANTI_AFFINITY_BETWEEN_ROLE_PODS_WEIGHT: i32 = 70;

/// The default affinity of `role`: prefer to spread its Pods across nodes.
///
/// Soft (`preferred`), so it can never leave a Pod unschedulable, and inert for a `DaemonSet`, which
/// already places exactly one Pod per node. It matters in `Deployment` mode only.
//
// TODO: Revisit once our minimum supported Kubernetes version is 1.35 and the role Service can use
// `trafficDistribution: PreferSameNode` instead of `internalTrafficPolicy` (see
// `controller::build::resource::service`).
//
// The chance: with node-local routing that degrades gracefully, an affinity attracting OPA Pods
// towards the products that query them would genuinely pay off, because traffic would prefer a
// node-local OPA Pod without the current risk of failing outright when there is none. 
//
// The concerns:
//
// * `PreferSameNode` falls back to other nodes only when there is no *ready* local endpoint, never
//   because the local one is busy. A request-heavy client (Trino, Kafka, depending on their
//   config) would keep hitting its local Pod while the others idle.
//
// * Field experience points the other way: spreading the load across Pods outperformed avoiding the
//   network hop by a wide margin.
//
// * The scheduler scores `podAffinity` and `podAntiAffinity` on one scale, so the two weights would
//   compete. Keeping this one at 70 above a lower attraction weight encodes "spreading wins", where 
//   equal weights would cancel out.
pub fn get_affinity(cluster_name: &str, role: &OpaRole) -> StackableAffinityFragment {
    StackableAffinityFragment {
        pod_affinity: None,
        pod_anti_affinity: Some(PodAntiAffinity {
            preferred_during_scheduling_ignored_during_execution: Some(vec![
                affinity_between_role_pods(
                    APP_NAME,
                    cluster_name,
                    &role.to_string(),
                    ANTI_AFFINITY_BETWEEN_ROLE_PODS_WEIGHT,
                ),
            ]),
            required_during_scheduling_ignored_during_execution: None,
        }),
        node_affinity: None,
        node_selector: None,
    }
}

#[cfg(test)]
mod tests {
    use stackable_operator::k8s_openapi::{
        api::core::v1::{PodAffinityTerm, WeightedPodAffinityTerm},
        apimachinery::pkg::apis::meta::v1::LabelSelector,
    };

    use super::*;

    /// Locks the shape of the default: a soft, per-node anti-affinity selecting the whole role
    /// (so across every role group), which is what makes replicas spread instead of piling up.
    #[test]
    fn default_affinity_spreads_the_role_across_nodes() {
        let affinity = get_affinity("simple-opa", &OpaRole::Server);

        assert_eq!(affinity.pod_affinity, None);
        assert_eq!(affinity.node_affinity, None);
        assert_eq!(affinity.node_selector, None);

        let anti_affinity = affinity.pod_anti_affinity.expect("is always set");
        // Soft only: a `required` term would leave Pods Pending once the replica count exceeds the
        // number of schedulable nodes.
        assert_eq!(
            anti_affinity.required_during_scheduling_ignored_during_execution,
            None
        );
        assert_eq!(
            anti_affinity.preferred_during_scheduling_ignored_during_execution,
            Some(vec![WeightedPodAffinityTerm {
                weight: ANTI_AFFINITY_BETWEEN_ROLE_PODS_WEIGHT,
                pod_affinity_term: PodAffinityTerm {
                    label_selector: Some(LabelSelector {
                        match_expressions: None,
                        match_labels: Some(
                            [
                                ("app.kubernetes.io/name", "opa"),
                                ("app.kubernetes.io/instance", "simple-opa"),
                                ("app.kubernetes.io/component", "server"),
                            ]
                            .map(|(key, value)| (key.to_string(), value.to_string()))
                            .into()
                        ),
                    }),
                    topology_key: "kubernetes.io/hostname".to_string(),
                    ..PodAffinityTerm::default()
                },
            }])
        );
    }
}
