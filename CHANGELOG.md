# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- Log the startup event for bundle-builder and user-info-fetcher ([#703]).
- Support experimental user-info-fetcher Entra backend to fetch user groups ([#712]).

### Changed

- BREAKING: Replace stackable-operator `initialize_logging` with stackable-telemetry `Tracing` ([#703], [#710]).
  - operator-binary:
    - The console log level was set by `OPA_OPERATOR_LOG`, and is now set by `CONSOLE_LOG`.
    - The file log level was set by `OPA_OPERATOR_LOG`, and is now set by `FILE_LOG`.
    - The file log directory was set by `OPA_OPERATOR_LOG_DIRECTORY`, and is now set
      by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
  - bundle-builder:
    - The console log level was set by `OPA_BUNDLE_BUILDER_LOG`, and is now set by `CONSOLE_LOG`.
    - The file log level was set by `OPA_BUNDLE_BUILDER_LOG`, and is now set by `FILE_LOG`.
    - The file log directory was set by `OPA_BUNDLE_BUILDER_LOG_DIRECTORY`, and is now set
      by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
  - user-info-fetcher:
    - The console log level was set by `OPA_OPERATOR_LOG`, and is now set by `CONSOLE_LOG`.
    - The file log level was set by `OPA_OPERATOR_LOG`, and is now set by `FILE_LOG`.
    - The file log directory was set by `OPA_OPERATOR_LOG_DIRECTORY`, and is now set
      by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
  - Replace stackable-operator `print_startup_string` with `tracing::info!` with fields.
- BREAKING: Inject the vector aggregator address into the vector config using the env var `VECTOR_AGGREGATOR_ADDRESS` instead
    of having the operator write it to the vector config ([#707]).

### Fixed

- Use `json` file extension for log files ([#709]).

[#703]: https://github.com/stackabletech/opa-operator/pull/703
[#707]: https://github.com/stackabletech/opa-operator/pull/707
[#709]: https://github.com/stackabletech/opa-operator/pull/709
[#710]: https://github.com/stackabletech/opa-operator/pull/710
[#712]: https://github.com/stackabletech/opa-operator/pull/712

## [25.3.0] - 2025-03-21

### Added

- Run a `containerdebug` process in the background of each OPA container to collect debugging information ([#666]).
- Added support for OPA `1.0.x` ([#677]) and ([#687]).
- Aggregate emitted Kubernetes events on the CustomResources ([#675]).
- Added support for filtering groups searched by Active Directory ([#693]).

### Removed

- Removed support for OPA `0.66.0` ([#677]).

### Changed

- Bump `stackable-operator` to 0.87.0 and `stackable-versioned` to 0.6.0 ([#696]).
- Default to OCI for image metadata and product image selection ([#671]).
- Active Directory backend for user-info-fetcher now uses the `service={opacluster}` scope rather than `pod,node` ([#698]).

[#666]: https://github.com/stackabletech/opa-operator/pull/666
[#671]: https://github.com/stackabletech/opa-operator/pull/671
[#675]: https://github.com/stackabletech/opa-operator/pull/675
[#677]: https://github.com/stackabletech/opa-operator/pull/677
[#687]: https://github.com/stackabletech/opa-operator/pull/687
[#693]: https://github.com/stackabletech/opa-operator/pull/693
[#696]: https://github.com/stackabletech/opa-operator/pull/696
[#698]: https://github.com/stackabletech/opa-operator/pull/698

## [24.11.1] - 2025-01-10

### Fixed

- BREAKING: Use distinct ServiceAccounts for the Stacklets, so that multiple Stacklets can be
  deployed in one namespace. Existing Stacklets will use the newly created ServiceAccounts after
  restart ([#656]).

[#656]: https://github.com/stackabletech/opa-operator/pull/656

## [24.11.0] - 2024-11-18

### Added

- Added regorule library for accessing user-info-fetcher ([#580]).
- Added support for OPA 0.67.1 ([#616]).
- The operator can now run on Kubernetes clusters using a non-default cluster domain.
  Use the env var `KUBERNETES_CLUSTER_DOMAIN` or the operator Helm chart property `kubernetesClusterDomain` to set a non-default cluster domain ([#637]).
- Added Active Directory backend for user-info-fetcher ([#622]).

### Changed

- Rewrite of the OPA bundle builder ([#578]).
- Reduce CRD size from `468KB` to `42KB` by accepting arbitrary YAML input instead of the underlying schema for the following fields ([#621]):
  - `podOverrides`
  - `affinity`

### Fixed

- Bundle builder should no longer keep serving deleted rules until it is restarted ([#578]).
- Failing to parse one `OpaCluster` should no longer cause the whole operator to stop functioning ([#638]).

### Removed

- Remove support for OPA 0.61.0 ([#616]).

[#578]: https://github.com/stackabletech/opa-operator/pull/578
[#580]: https://github.com/stackabletech/opa-operator/pull/580
[#616]: https://github.com/stackabletech/opa-operator/pull/616
[#621]: https://github.com/stackabletech/opa-operator/pull/621
[#622]: https://github.com/stackabletech/opa-operator/pull/622
[#637]: https://github.com/stackabletech/opa-operator/pull/637
[#638]: https://github.com/stackabletech/opa-operator/pull/638

## [24.7.0] - 2024-07-24

### Added

- Support enabling decision logs ([#555]).

### Changed

- Bump `stackable-operator` to `0.70.0`, `product-config` to `0.7.0`, and other dependencies  ([#595]).

### Fixed

- Processing of corrupted log events fixed; If errors occur, the error
  messages are added to the log event ([#583]).

## Removed

- Dead code ([#596]).

[#555]: https://github.com/stackabletech/opa-operator/pull/555
[#583]: https://github.com/stackabletech/opa-operator/pull/583
[#595]: https://github.com/stackabletech/opa-operator/pull/595
[#596]: https://github.com/stackabletech/opa-operator/pull/596

## [24.3.0] - 2024-03-20

### Added

- Add user-info-fetcher to fetch user metadata from directory services ([#433]).
- Helm: support labels in values.yaml ([#507]).
- Added support for OPA 0.61.0 ([#518]).

### Changed

- [BREAKING]: Remove legacy `nodeSelector` on rolegroups. Use the field `affinity.nodeAffinity` instead ([#433]).

### Removed

- Removed support for OPA 0.51.0 ([#518]).

[#433]: https://github.com/stackabletech/opa-operator/pull/433
[#507]: https://github.com/stackabletech/opa-operator/pull/507
[#518]: https://github.com/stackabletech/opa-operator/pull/518

## [23.11.0] - 2023-11-24

### Added

- Default stackableVersion to operator version ([#467]).
- Document we don't create PodDisruptionBudgets ([#480]).
- Added support for 0.57.0 ([#482]).
- Support graceful shutdown ([#487]).
- Disable OPA telemetry ([#487]).

### Changed

- `vector` `0.26.0` -> `0.33.0` ([#465], [#482]).
- `operator-rs` `0.44.0` -> `0.55.0` ([#467], [#480], [#482]).

### Removed

- Removed support for versions 0.45.0, 0.41.0, 0.37.2, 0.28.0, 0.27.1 ([#482]).

[#465]: https://github.com/stackabletech/opa-operator/pull/465
[#467]: https://github.com/stackabletech/opa-operator/pull/467
[#480]: https://github.com/stackabletech/opa-operator/pull/480
[#482]: https://github.com/stackabletech/opa-operator/pull/482
[#487]: https://github.com/stackabletech/opa-operator/pull/487

## [23.7.0] - 2023-07-14

### Added

- Generate OLM bundle for Release 23.4.0 ([#442]).
- Missing CRD defaults for `status.conditions` field ([#443]).
- Support for OPA 0.51.0 ([#451]).
- Set explicit resources on all containers ([#453]).
- Support `podOverrides` ([#458]).

### Changed

- operator-rs: `0.40.1` -> `0.44.0` ([#440], [#460]).
- Use 0.0.0-dev product images for testing ([#441]).
- Use testing-tools 0.2.0 ([#441]).
- Added kuttl test suites ([#455]).
- Set explicit resources on all containers ([#453], [#456]).

### Fixed

- Migrate "opa-bundle-builder" container name from <= 23.1 releases ([#445]).
- Increase the size limit of the log volume ([#460]).

[#440]: https://github.com/stackabletech/opa-operator/pull/440
[#441]: https://github.com/stackabletech/opa-operator/pull/441
[#442]: https://github.com/stackabletech/opa-operator/pull/442
[#443]: https://github.com/stackabletech/opa-operator/pull/443
[#445]: https://github.com/stackabletech/opa-operator/pull/445
[#451]: https://github.com/stackabletech/opa-operator/pull/451
[#453]: https://github.com/stackabletech/opa-operator/pull/453
[#455]: https://github.com/stackabletech/opa-operator/pull/455
[#456]: https://github.com/stackabletech/opa-operator/pull/456
[#458]: https://github.com/stackabletech/opa-operator/pull/458
[#460]: https://github.com/stackabletech/opa-operator/pull/460

## [23.4.0] - 2023-04-17

### Added

- Cluster status conditions ([#428]).
- Extend cluster resources for status and cluster operation (paused, stopped) ([430]).

### Changed

- [BREAKING] Support specifying Service type.
  This enables us to later switch non-breaking to using `ListenerClasses` for the exposure of Services.
  This change is breaking, because - for security reasons - we default to the `cluster-internal` `ListenerClass`.
  If you need your cluster to be accessible from outside of Kubernetes you need to set `clusterConfig.listenerClass`
  to `external-unstable` or `external-stable` ([#432]).
- `operator-rs` `0.27.1` -> `0.40.1` ([#411], [#420], [#430], [#431]).
- Fragmented `OpaConfig` ([#411]).
- Bumped stackable image versions to `23.4.0-rc2` ([#420]).
- Enabled logging ([#420]).
- Openshift compatibility: extended roles ([#431]).
- Use operator-rs `build_rbac_resources` method ([#431]).

[#411]: https://github.com/stackabletech/opa-operator/pull/411
[#420]: https://github.com/stackabletech/opa-operator/pull/420
[#428]: https://github.com/stackabletech/opa-operator/pull/428
[#430]: https://github.com/stackabletech/opa-operator/pull/430
[#431]: https://github.com/stackabletech/opa-operator/pull/431
[#432]: https://github.com/stackabletech/opa-operator/pull/432

## [23.1.0] - 2023-01-23

### Changed

- Updated stackable image versions ([#374]).
- `operator-rs` `0.22.0` -> `0.27.1` ([#377]).
- Don't run init container as root and avoid chmod and chowning ([#382]).
- [BREAKING] Use Product image selection instead of version. `spec.version` has been replaced by `spec.image` ([#385]).
- Support offline mode ([#391]).
- Updated to new docker tags containing the opa-bundle builder ([#391]).

[#374]: https://github.com/stackabletech/opa-operator/pull/374
[#377]: https://github.com/stackabletech/opa-operator/pull/377
[#382]: https://github.com/stackabletech/opa-operator/pull/382
[#385]: https://github.com/stackabletech/opa-operator/pull/385
[#391]: https://github.com/stackabletech/opa-operator/pull/391

## [0.11.0] - 2022-11-07

### Added

- CPU and memory limits are now configurable ([#347]).
- Better documentation on the bundle builder ([#350])
- Support OPA 0.45.0 ([#360]).

[#347]: https://github.com/stackabletech/opa-operator/pull/347
[#350]: https://github.com/stackabletech/opa-operator/pull/350
[#360]: https://github.com/stackabletech/opa-operator/pull/360

## [0.10.0] - 2022-09-06

### Changed

- Include chart name when installing with a custom release name ([#313], [#314]).
- `operator-rs` `0.15.0` -> `0.22.0` ([#315]).

[#313]: https://github.com/stackabletech/opa-operator/pull/313
[#314]: https://github.com/stackabletech/opa-operator/pull/314
[#315]: https://github.com/stackabletech/opa-operator/pull/315

## [0.9.0] - 2022-06-30

### Added

- Reconciliation errors are now reported as Kubernetes events ([#241]).
- Bundle builder side car container that generates bundles from
    `ConfigMap` objects ([#244])
- The command line argument `--opa-builder-clusterrole` for the `run`
    subcommand or the environment variable `OPA_BUNDLE_BUILDER_CLUSTERROLE` to set up
    a service account for the OPA pods ([#244], [#252]).
- The command line argument `--watch-namespace` for the `run` subcommand or
  the environment variable `WATCH_NAMESPACE` can be used to instruct the
  operator to watch a particular namespace. ([#244])
- Added `kuttl` tests from `integration-test` repository ([#289])

### Changed

- `operator-rs` `0.10.0` -> `0.15.0` ([#241], [#244], [#273]).
- BREAKING: Renamed custom resource from `OpenPolicyAgent` to `OpaCluster` ([#244]).
- Replace the `tempdir` crate with `tempfile` ([#287]).
- [BREAKING] Specifying the product version has been changed to adhere to [ADR018](https://docs.stackable.tech/home/contributor/adr/ADR018-product_image_versioning.html) instead of just specifying the product version you will now have to add the Stackable image version as well, so `version: 3.5.8` becomes (for example) `version: 3.5.8-stackable0.1.0` ([#293])

### Removed

- `regoRuleReference` from OpaConfig and CRD respectively ([#273]).

[#241]: https://github.com/stackabletech/opa-operator/pull/241
[#244]: https://github.com/stackabletech/opa-operator/pull/244
[#252]: https://github.com/stackabletech/opa-operator/pull/252
[#273]: https://github.com/stackabletech/opa-operator/pull/273
[#287]: https://github.com/stackabletech/opa-operator/pull/287
[#289]: https://github.com/stackabletech/opa-operator/pull/289
[#293]: https://github.com/stackabletech/opa-operator/pull/293

## [0.8.0] - 2022-02-14

### Added

- monitoring scraping label `prometheus.io/scrape: true` ([#218]).
- supported versions ([#218]).

### Changed

- `operator-rs` `0.8.0` → `0.10.0` ([#202]), ([#218]).
- fixed outdated docs ([#218]).

[#202]: https://github.com/stackabletech/opa-operator/pull/202
[#218]: https://github.com/stackabletech/opa-operator/pull/218

## [0.7.0] - 2022-01-27

### Changed

- BREAKING: STFU rework ([#146]).
- BREAKING: regoRuleReference in config now optional ([#188]).
- Version now a String instead of enum ([#156]).
- `operator-rs` `0.6.0` → `0.8.0` ([#177]).
- Custom resource example now points to regorule-operator service ([#177]).
- `snafu` `0.6.0` → `0.7.0` ([#188]).

### Removed

- Configurable Port from code and product config ([#188]).

[#146]: https://github.com/stackabletech/opa-operator/pull/146
[#156]: https://github.com/stackabletech/opa-operator/pull/156
[#177]: https://github.com/stackabletech/opa-operator/pull/177
[#188]: https://github.com/stackabletech/opa-operator/pull/188

## [0.6.0] - 2021-12-06

## [0.5.0] - 2021-11-12

### Changed

- `operator-rs` `0.3.0` → `0.4.0` ([#119]).
- Adapted pod image and container command to docker image ([#119]).
- BREAKING CRD: Fixed typos `Reporule` to `Regorule` ([#119]).
- Adapted documentation to represent new workflow with docker images ([#119]).

### Removed

- BREAKING monitoring: container port `metrics` temporarily removed (cannot assign the same port to `client` and `metrics`). This will not work with the current monitoring approach ([#119]).

[#119]: https://github.com/stackabletech/opa-operator/pull/119

## [0.4.1] - 2021-10-27

### Added

- Added PartialEq trait to `OpaReference` ([#103]).

### Changed

- `operator-rs`: `0.3.0` ([#115]).
- Renamed crd/util to crd::discovery and added deprecated reexport for backwards compatibility ([#103]).

### Fixed

- Moved `wait_until_crds_present` to operator-binary (preparation for commands) ([#115]).

[#115]: https://github.com/stackabletech/opa-operator/pull/115
[#103]: https://github.com/stackabletech/opa-operator/pull/103

## [0.4.0] - 2021-09-21

### Changed

- `kube-rs`: `0.58` → `0.60` ([#88]).
- `k8s-openapi` `0.12` → `0.13` and features: `v1_21` → `v1_22` ([#88]).
- `operator-rs` `0.2.1` → `0.2.2` ([#88]).

### Removed

- `kube-runtime` dependency ([#88]).

[#88]: https://github.com/stackabletech/opa-operator/pull/88

## [0.3.0] - 2021-09-20

### Added

- Added versioning code from operator-rs for up and downgrades ([#86]).
- Added `ProductVersion` to status ([#86]).
- Added `Condition` to status ([#86]).

[#86]: https://github.com/stackabletech/opa-operator/pull/86

## [0.2.0] - 2021-09-14

### Changed

- **Breaking:** Repository structure was changed and the -server crate renamed to -binary. As part of this change the -server suffix was removed from both the package name for os packages and the name of the executable ([#72]).

## [0.1.0] - 2021.09.07

### Added

- Initial release
