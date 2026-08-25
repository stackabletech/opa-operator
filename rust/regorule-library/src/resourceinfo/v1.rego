package stackable.opa.resourceinfo.v1

# Database
databaseResourceInfo(system, instance, database) := resourceInfo(
    "database",
    {"system": system, "instance": instance, "database": database}
)

# Schema
schemaResourceInfo(system, instance, database, schema) := resourceInfo(
    "schema",
    {"system": system, "instance": instance, "database": database, "schema": schema}
)

# Table
tableResourceInfo(system, instance, database, schema, table) := resourceInfo(
    "table",
    {
        "system": system,
        "instance": instance,
        "database": database,
        "schema": schema,
        "table": table
    }
)

# Stream
streamResourceInfo(system, instance, queue) := resourceInfo(
    "stream",
    {"system": system, "instance": instance, "queue": queue}
)

# Dashboard
dashboardResourceInfo(system, instance, id) := resourceInfo(
    "dashboard",
    {"system": system, "instance": instance, "id": sprintf("%v", [id])}
)

# Chart
chartResourceInfo(system, instance, id) := resourceInfo(
    "chart",
    {"system": system, "instance": instance, "id": sprintf("%v", [id])}
)

# Raw identifier
rawIdentifierResourceInfo(identifier) := resourceInfo(
    "rawIdentifier",
    {"identifier": identifier}
)

# How long to wait for the resource-info-fetcher before giving up.
#
# Set explicitly rather than inherited, because it is part of the contract: an authorization decision
# has to be answered promptly, so this is deliberately far below the fetcher's own 60s budget for
# talking to the data catalog. A catalog that is slower than this leaves the caller without an answer
# while the fetcher finishes and caches the result, so the next lookup is served from the cache.
requestTimeout := "5s"

# Each resource type has its own `GET /metadata/<type>` endpoint; the parameters are passed as a URL
# query string. `urlquery.encode_object` URL-encodes the values (e.g. the `:`, `(` and `,` in a raw
# DataHub URN). `id` is stringified first because the query encoder only accepts string values.
#
# Only a `200` yields a value. The fetcher answers a failed lookup with an error envelope
# (`{"error": {...}}`), which must never reach a policy as if it were metadata. A policy that
# defaults a missing field (for example `object.get(info, "tags", [])`) would otherwise read a
# failed lookup as "this resource has no tags" and allow what it should deny. Requiring a `200`
# makes the whole call undefined instead, which such a policy cannot silently succeed on.
#
# An undefined call is still not a denial. OPA turns a builtin error (the fetcher being unreachable,
# or slower than `requestTimeout`) into an undefined value rather than aborting the query, unless
# the query sets `strict-builtin-errors`. Write policies so that absent resource information denies
# rather than allows, and set `strict-builtin-errors` where the product's OPA client supports it, so
# that a failed lookup fails the decision instead of quietly dropping out of it.
resourceInfo(endpoint, params) := response.body if {
    response := http.send({
        "method": "GET",
        "url": sprintf("http://127.0.0.1:9477/metadata/%s?%s", [endpoint, urlquery.encode_object(params)]),
        "raise_error": true,
        "timeout": requestTimeout
    })
    response.status_code == 200
}
