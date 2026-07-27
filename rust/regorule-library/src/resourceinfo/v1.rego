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

# Each resource type has its own `GET /metadata/<type>` endpoint; the parameters are passed as a URL
# query string. `urlquery.encode_object` URL-encodes the values (e.g. the `:`, `(` and `,` in a raw
# DataHub URN). `id` is stringified first because the query encoder only accepts string values.
resourceInfo(endpoint, params) := http.send({
  "method": "GET",
  "url": sprintf("http://127.0.0.1:9477/metadata/%s?%s", [endpoint, urlquery.encode_object(params)]),
  "raise_error": true
}).body
