# User info fetcher

Fetches user metadata from a directory service, and exposes it in a normalized format for OPA rules to read.

It is deployed by the Stackable OPA Operator, and is not recommended for standalone use.

## Supported backends

- `none` - Dummy backend
- `keycloak` - Keycloak
