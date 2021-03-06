# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Changed

- Include chart name when installing with a custom release name ([#313], [#314]).
- `operator-rs` `0.15.0` -> `0.22.0` ([#315]).

[#313]: https://github.com/stackabletech/trino-operator/pull/313
[#314]: https://github.com/stackabletech/trino-operator/pull/314
[#315]: https://github.com/stackabletech/trino-operator/pull/315

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

- `operator-rs` `0.8.0` ??? `0.10.0` ([#202]), ([#218]).
- fixed outdated docs ([#218]).

[#202]: https://github.com/stackabletech/opa-operator/pull/202
[#218]: https://github.com/stackabletech/opa-operator/pull/218

## [0.7.0] - 2022-01-27

### Changed

- BREAKING: STFU rework ([#146]).
- BREAKING: regoRuleReference in config now optional ([#188]).
- Version now a String instead of enum ([#156]).
- `operator-rs` `0.6.0` ??? `0.8.0` ([#177]).
- Custom resource example now points to regorule-operator service ([#177]).
- `snafu` `0.6.0` ??? `0.7.0` ([#188]).

### Removed

- Configurable Port from code and product config ([#188]).

[#146]: https://github.com/stackabletech/opa-operator/pull/146
[#156]: https://github.com/stackabletech/opa-operator/pull/156
[#177]: https://github.com/stackabletech/opa-operator/pull/177
[#188]: https://github.com/stackabletech/opa-operator/pull/188

## [0.6.0] - 2021-12-06

## [0.5.0] - 2021-11-12

### Changed

- `operator-rs` `0.3.0` ??? `0.4.0` ([#119]).
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

- `kube-rs`: `0.58` ??? `0.60` ([#88]).
- `k8s-openapi` `0.12` ??? `0.13` and features: `v1_21` ??? `v1_22` ([#88]).
- `operator-rs` `0.2.1` ??? `0.2.2` ([#88]).

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
