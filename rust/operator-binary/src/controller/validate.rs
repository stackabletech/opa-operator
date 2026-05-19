//! The validate step in the OpaCluster controller
//!
//! Synchronously validates inputs that don't require a Kubernetes client. Produces
//! [`ValidatedInputs`], consumed by the rest of `reconcile_opa`.

use product_config::{ProductConfigManager, types::PropertyNameKind};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection::{self, ResolvedProductImage},
    product_config_utils::{
        ValidatedRoleConfigByPropertyKind, transform_all_roles_to_config,
        validate_all_roles_and_groups_config,
    },
};

use crate::{
    controller::dereference::DereferencedObjects,
    crd::{OpaRole, v1alpha2},
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to resolve product image"))]
    ResolveProductImage {
        source: product_image_selection::Error,
    },

    #[snafu(display("failed to transform configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::product_config_utils::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Synchronous inputs the rest of `reconcile_opa` needs after dereferencing.
pub struct ValidatedInputs {
    pub image: ResolvedProductImage,
    pub validated_role_config: ValidatedRoleConfigByPropertyKind,
}

/// Validates the cluster spec and the dereferenced inputs.
pub fn validate(
    opa: &v1alpha2::OpaCluster,
    _dereferenced_objects: &DereferencedObjects,
    operator_environment: &OperatorEnvironmentOptions,
    product_config: &ProductConfigManager,
) -> Result<ValidatedInputs> {
    let image = opa
        .spec
        .image
        .resolve(
            super::CONTAINER_IMAGE_BASE_NAME,
            &operator_environment.image_repository,
            crate::built_info::PKG_VERSION,
        )
        .context(ResolveProductImageSnafu)?;

    let validated_role_config = validate_all_roles_and_groups_config(
        &image.product_version,
        &transform_all_roles_to_config(
            opa,
            &[(
                OpaRole::Server.to_string(),
                (
                    vec![
                        PropertyNameKind::File(super::CONFIG_FILE.to_string()),
                        PropertyNameKind::Env,
                        PropertyNameKind::Cli,
                    ],
                    opa.spec.servers.clone(),
                ),
            )]
            .into(),
        )
        .context(ProductConfigTransformSnafu)?,
        product_config,
        false,
        false,
    )
    .context(InvalidProductConfigSnafu)?;

    Ok(ValidatedInputs {
        image,
        validated_role_config,
    })
}
