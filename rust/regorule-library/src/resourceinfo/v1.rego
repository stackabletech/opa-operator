package stackable.opa.resourceinfo.v1

# Trino catalog
resourceInfoTrinoCatalog(stacklet, catalog) := resourceInfo(
    "trinoCatalog",
    {"stacklet": stacklet, "catalog": catalog}
)

# Trino schema
resourceInfoTrinoSchema(stacklet, catalog, schema) := resourceInfo(
    "trinoSchema",
    {"stacklet": stacklet, "catalog": catalog, "schema": schema}
)

# Trino table
resourceInfoTrinoTable(stacklet, catalog, schema, table) := resourceInfo(
    "trinoTable",
    {"stacklet": stacklet, "catalog": catalog, "schema": schema, "table": table}
)

# Superset chart
resourceInfoSupersetChart(stacklet, id) := resourceInfo(
    "supersetChart",
    {"stacklet": stacklet, "id": sprintf("%v", [id])}
)

# Superset dashboard
resourceInfoSupersetDashboard(stacklet, id) := resourceInfo(
    "supersetDashboard",
    {"stacklet": stacklet, "id": sprintf("%v", [id])}
)

# Kafka topic
resourceInfoKafkaTopic(stacklet, topic) := resourceInfo(
    "kafkaTopic",
    {"stacklet": stacklet, "topic": topic}
)

# Raw DataHub urn
resourceInfoDataHubUrn(urn) := resourceInfo(
    "dataHubUrn",
    {"urn": urn}
)

# Each resource type has its own `GET /metadata/<type>` endpoint; the parameters are passed as a URL
# query string. `urlquery.encode_object` URL-encodes the values (e.g. the `:`, `(` and `,` in a raw
# DataHub URN). `id` is stringified first because the query encoder only accepts string values.
resourceInfo(endpoint, params) := http.send({
  "method": "GET",
  "url": sprintf("http://127.0.0.1:9477/metadata/%s?%s", [endpoint, urlquery.encode_object(params)]),
  "raise_error": true
}).body
