package stackable.opa.resourceinfo.v1

# Trino catalog
resourceInfoTrinoCatalog(stacklet, catalog) := resourceInfoJson(
    {"stacklet": stacklet, "trinoCatalog": {"catalog": catalog}}
)

# Trino schema
resourceInfoTrinoSchema(stacklet, catalog, schema) := resourceInfoJson(
    {"stacklet": stacklet, "trinoSchema": {"catalog": catalog, "schema": schema}}
)

# Trino table
resourceInfoTrinoTable(stacklet, catalog, schema, table) := resourceInfoJson(
    {"stacklet": stacklet, "trinoTable": {"catalog": catalog, "schema": schema, "table": table}}
)

# Superset chart
resourceInfoSupersetChart(stacklet, id) := resourceInfoJson(
    {"stacklet": stacklet, "supersetChart": {"id": id}}
)

# Superset dashboard
resourceInfoSupersetDashboard(stacklet, id) := resourceInfoJson(
    {"stacklet": stacklet, "supersetDashboard": {"id": id}}
)

# Kafka topic
resourceInfoKafkaTopic(stacklet, topic) := resourceInfoJson(
    {"stacklet": stacklet, "kafkaTopic": {"topic": topic}}
)

# Raw DataHub urn
resourceInfoDataHubUrn(urn) := resourceInfoJson(
    {"stacklet": "dummy", "dataHubUrn": urn}
)

resourceInfoJson(json) := http.send({
  "method": "POST",
  "url": "http://127.0.0.1:9477/resource",
  "body": json,
  "headers": {"Content-Type": "application/json"},
  "raise_error": true
}).body
