# Stackable library of shared regorules

This contains regorules that are shipped by the Stackable Data Platform (SDP) as libraries to help simplify writing authorization rules.

## What this is not

This library should *not* contain rules that only concern one SDP product. Those are the responsibility of their individual operators.

## Versioning

All regorules exposed by this library should be versioned, according to Kubernetes conventions.

This version covers *breaking changes to the interface*, not the implementation. If a proposed change breaks existing clients,
add a new version. Otherwise, change the latest version inline.

Ideally, old versions should be implemented on top of newer versions, rather than carry independent implementations.
