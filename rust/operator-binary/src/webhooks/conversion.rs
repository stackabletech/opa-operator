use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    kube::{Client, core::crd::MergeError},
    webhook::{
        WebhookServer, WebhookServerError, WebhookServerOptions,
        webhooks::{ConversionWebhook, ConversionWebhookOptions},
    },
};

use crate::crd::{FIELD_MANAGER, OpaCluster, OpaClusterVersion};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to merge CRD"))]
    MergeCrd { source: MergeError },

    #[snafu(display("failed to create conversion webhook server"))]
    CreateWebhookServer { source: WebhookServerError },
}

pub async fn create_webhook_server(
    operator_environment: &OperatorEnvironmentOptions,
    disable_crd_maintenance: bool,
    client: Client,
) -> Result<WebhookServer, Error> {
    let crds_and_handlers = vec![(
        OpaCluster::merged_crd(OpaClusterVersion::V1Alpha2).context(MergeCrdSnafu)?,
        OpaCluster::try_convert,
    )];

    let conversion_webhook_options = ConversionWebhookOptions {
        field_manager: FIELD_MANAGER.to_owned(),
        disable_crd_maintenance,
    };

    let (conversion_webhook, _initial_reconcile_rx) =
        ConversionWebhook::new(crds_and_handlers, client, conversion_webhook_options);

    let webhook_server_options = WebhookServerOptions {
        webhook_service_name: operator_environment.operator_service_name.to_owned(),
        webhook_namespace: operator_environment.operator_namespace.to_owned(),
        socket_addr: WebhookServer::DEFAULT_SOCKET_ADDRESS,
    };

    WebhookServer::new(vec![Box::new(conversion_webhook)], webhook_server_options)
        .await
        .context(CreateWebhookServerSnafu)
}
