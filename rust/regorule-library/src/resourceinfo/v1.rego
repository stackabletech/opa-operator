package stackable.opa.resourceinfo.v1

# Fetch resource metadata (tags, glossary terms, owners, domain, data products,
# custom properties, per-field type + tags + glossary) from the configured
# catalog backend. Returns the body of the HTTP response directly.
#
# Example:
#   info := stackable.opa.resourceinfo.v1.resourceInfo(
#     "dataset",
#     "hive.db.table",
#     {"platform": "trino", "environment": "PROD"}
#   )
#   allow { not "pii" in info.tags }
resourceInfo(kind, id, attributes) := http.send({
  "method": "POST",
  "url": "http://127.0.0.1:9477/resource",
  "body": {"kind": kind, "id": id, "attributes": attributes},
  "headers": {"Content-Type": "application/json"},
  "raise_error": true,
}).body
