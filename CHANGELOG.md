# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- Added PartialEq trait to `OpaReference` ([#103]).

### Changed

- Renamed crd/util to crd::discovery and added deprecated reexport for backwards compatibility ([#103]).

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
