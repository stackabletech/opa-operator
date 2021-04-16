mod error;
use crate::error::Error;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::Pod;
use kube::api::ListParams;
use kube::Api;
use kube_runtime::controller::ReconcilerAction;
use stackable_opa_crd::{OpaServer, OpaServerSpec};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::pod_utils as operator_pod_utils;
use stackable_operator::reconcile;
use stackable_operator::reconcile::{
    ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use std::pin::Pin;
use std::time::Duration;
use tracing::{debug, error, info, trace};

type OpaReconcileResult = ReconcileResult<error::Error>;

struct OpaState {
    context: ReconciliationContext<OpaServer>,
}

impl OpaState {
    pub async fn test1(&mut self) -> OpaReconcileResult {
        Ok(ReconcileFunctionAction::Continue)
    }

    pub async fn test2(&mut self) -> OpaReconcileResult {
        Ok(ReconcileFunctionAction::Continue)
    }
}

impl ReconciliationState for OpaState {
    type Error = error::Error;

    fn reconcile(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<ReconcileFunctionAction, Self::Error>> + Send + '_>>
    {
        Box::pin(async move { self.test1().await?.then(self.test2()).await })
    }
}

#[derive(Debug)]
struct OpaStrategy {}

impl OpaStrategy {
    pub fn new() -> OpaStrategy {
        OpaStrategy {}
    }
}

#[async_trait]
impl ControllerStrategy for OpaStrategy {
    type Item = OpaServer;
    type State = OpaState;
    type Error = error::Error;

    fn error_policy(&self) -> ReconcilerAction {
        let reconcile_after_error_sec = 30;
        error!(
            "Reconciliation error: requeuing after {} seconds!",
            reconcile_after_error_sec
        );
        reconcile::create_requeuing_reconciler_action(Duration::from_secs(
            reconcile_after_error_sec,
        ))
    }

    async fn init_reconcile_state(
        &self,
        context: ReconciliationContext<Self::Item>,
    ) -> Result<Self::State, Error> {
        Ok(OpaState { context })
    }
}

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
pub async fn create_controller(client: Client) {
    let opa_api: Api<OpaServer> = client.get_all_api();
    let pods_api: Api<Pod> = client.get_all_api();

    let controller = Controller::new(opa_api).owns(pods_api, ListParams::default());

    let strategy = OpaStrategy::new();

    controller
        .run(client, strategy, Duration::from_secs(10))
        .await;
}
