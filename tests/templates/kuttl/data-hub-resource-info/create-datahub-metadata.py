#!/usr/bin/env python
"""Populate DataHub with the users, groups, tags, domains, data products and assignments that the
resource-info-fetcher (RIF) test asserts on (see test-regorule.py).

The metadata is attached to the entities ingested in 31/32/33: the Trino `tpch.sf1`
catalog/schema/tables, the Kafka topics and the Superset chart/dashboard. Writes go to the GMS
OpenAPI v3 entity endpoint as UPSERTs (createIfNotExists=false), so the script is re-runnable;
async=false makes a rejected write fail loudly.

Every resource type gets a *distinct* set of tags/domain/owners on purpose: RIF answers with an
empty record when a URN resolves to nothing, so assertions on all-empty metadata would pass even
for a wrong URN. Distinguishable metadata is what makes the URN derivation in
resource_to_urn_mapping.rs testable.
"""

import hashlib
import json
import os
import sys
import time

import requests

GMS = os.environ.get("GMS", "http://datahub-datahub-gms:8080")
SESSION = requests.Session()
SESSION.headers.update(
    {
        "Authorization": f"Bearer {os.environ['DATAHUB_GMS_TOKEN']}",
        "Content-Type": "application/json",
    }
)

BUSINESS = "urn:li:ownershipType:__system__business_owner"
TECHNICAL = "urn:li:ownershipType:__system__technical_owner"

# The ingestion recipes in 31/32/33 set a `platform_instance: ${POD_NAMESPACE}/<cluster>`, so the
# DataHub `instance` is that value (not the `env`, which is PROD throughout) and it appears in the
# container GUIDs and as the entity-name prefix. The namespace is only known at runtime, so every
# URN is computed here rather than hardcoded.
NAMESPACE = os.environ["POD_NAMESPACE"]
TRINO = f"{NAMESPACE}/my-trino"
KAFKA = f"{NAMESPACE}/test-kafka"
SUPERSET = f"{NAMESPACE}/my-superset"


def group(gid):
    return f"urn:li:corpGroup:{gid}"


def user(username):
    return f"urn:li:corpuser:{username}"


def tag(tid):
    return f"urn:li:tag:{tid}"


def domain(did):
    return f"urn:li:domain:{did}"


def dataset(table):
    return f"urn:li:dataset:(urn:li:dataPlatform:trino,{TRINO}.tpch.sf1.{table},PROD)"


def topic(name):
    """A Kafka topic is a dataset too, just without the database/schema levels."""
    return f"urn:li:dataset:(urn:li:dataPlatform:kafka,{KAFKA}.{name},PROD)"


def container_urn(container_key):
    """Reproduce DataHub's `datahub_guid` (mirrors `container_urn` in resource_to_urn_mapping.rs):
    serialize the container key to compact, key-sorted JSON and MD5-hash it."""
    key_json = json.dumps(container_key, sort_keys=True, separators=(",", ":"))
    return f"urn:li:container:{hashlib.md5(key_json.encode()).hexdigest()}"


CATALOG = container_urn({"platform": "trino", "instance": TRINO, "database": "tpch"})
SCHEMA = container_urn(
    {"platform": "trino", "instance": TRINO, "database": "tpch", "schema": "sf1"}
)

# The Superset seed in 23-install-superset creates exactly one chart and one dashboard in a fresh
# Superset, so both get id 1. test-regorule.py asserts on the same ids.
CHART = f"urn:li:chart:(superset,{SUPERSET}.1)"
DASHBOARD = f"urn:li:dashboard:(superset,{SUPERSET}.1)"


def entity(urn, **aspects):
    """An UPSERT object: `{urn, <aspect name>: {"value": <aspect value>}, ...}`."""
    return {"urn": urn, **{name: {"value": value} for name, value in aspects.items()}}


def ownership(owners):
    """An `ownership` aspect value from a list of (owner urn, ownership type urn) pairs."""
    owner_types = {}
    for owner_urn, type_urn in owners:
        owner_types.setdefault(type_urn, []).append(owner_urn)
    return {
        "owners": [
            {"owner": o, "typeUrn": t, "type": "NONE", "source": {"type": "MANUAL"}}
            for o, t in owners
        ],
        "ownerTypes": owner_types,
        "lastModified": {"actor": user("datahub"), "time": 0},
    }


def upsert(entity_type, objects):
    resp = SESSION.post(
        f"{GMS}/openapi/v3/entity/{entity_type}",
        params={"async": "false", "createIfNotExists": "false"},
        json=objects,
    )
    if not resp.ok:
        sys.exit(f"ERROR upserting {entity_type}: HTTP {resp.status_code}: {resp.text}")


def wait_for(what, check, attempts=60, delay=5):
    for attempt in range(attempts):
        if check():
            print(f"  {what}: ready")
            return
        print(
            f"  {what}: not ready (attempt {attempt + 1}/{attempts}), retrying in {delay}s"
        )
        time.sleep(delay)
    sys.exit(f"{what}: not ready in time")


def gms_healthy():
    try:
        return SESSION.get(f"{GMS}/health", timeout=10).ok
    except requests.RequestException:
        return False


def data_product_edge_indexed(asset_urn):
    query = (
        '{ entity(urn: "%s") { relationships(input: {types: ["DataProductContains"], '
        "direction: INCOMING, count: 10}) { total } } }" % asset_urn
    )
    resp = SESSION.post(f"{GMS}/api/graphql", json={"query": query}, timeout=20)
    if not resp.ok:
        return False
    node = resp.json().get("data") or {}
    for key in ("entity", "relationships"):
        node = node.get(key) or {}
    return bool(node.get("total"))


# --- The test fixture ----------------------------------------------------------------------------

# group id -> display name
GROUPS = {
    "sales-analytics": "Sales Analytics",
    "procurement": "Procurement",
    "data-platform": "Data Platform",
}

# username -> (full name, group id)
USERS = {
    "alice.turner": ("Alice Turner", "sales-analytics"),
    "bob.ramirez": ("Bob Ramirez", "sales-analytics"),
    "carla.nowak": ("Carla Nowak", "procurement"),
    "david.okoye": ("David Okoye", "procurement"),
    "erin.fischer": ("Erin Fischer", "data-platform"),
}

# tag id -> display name
TAGS = {"pii": "PII", "public": "Public"}

# domain id -> (display name, description)
DOMAINS = {
    "sales": ("Sales", "Sales and order data"),
    "supply-chain": ("Supply Chain", "Suppliers, parts and procurement data"),
}

# data product id -> (display name, description, domain id, [asset tables])
DATA_PRODUCTS = {
    "order-analytics": (
        "Order Analytics",
        "Curated order analytics datasets",
        "sales",
        ["customer", "orders", "lineitem"],
    ),
    "supplier-360": (
        "Supplier 360",
        "Supplier, part and procurement datasets",
        "supply-chain",
        ["supplier", "part", "partsupp"],
    ),
}

# table -> (tag ids, domain id or None, [(owner urn, ownership type urn)])
TABLES = {
    "customer": (["pii"], "sales", [(group("sales-analytics"), BUSINESS)]),
    "orders": ([], "sales", [(group("sales-analytics"), BUSINESS)]),
    "lineitem": ([], "sales", [(group("sales-analytics"), BUSINESS)]),
    "supplier": (["pii"], "supply-chain", [(group("procurement"), BUSINESS)]),
    "part": ([], "supply-chain", [(group("procurement"), BUSINESS)]),
    "partsupp": ([], "supply-chain", [(group("procurement"), BUSINESS)]),
    "nation": (["public"], None, [(group("data-platform"), TECHNICAL)]),
    "region": (["public"], None, [(group("data-platform"), TECHNICAL)]),
}

# Kafka topic -> same shape as TABLES. The topics come from the `kafka-seed` Job in
# 21-install-kafka; `page-views` is intentionally absent so the test has an ingested-but-unannotated
# resource to compare against.
TOPICS = {
    "orders": (["pii"], "sales", [(group("sales-analytics"), BUSINESS)]),
}

# Superset chart/dashboard, again the same shape. The dashboard mixes a group and a user owner (like
# the Trino schema does), the chart carries no tags and no domain - each resource type ends up with
# a combination no other one has, so a wrong URN cannot accidentally satisfy the assertions.
DASHBOARD_METADATA = (
    ["public"],
    "supply-chain",
    [(group("procurement"), BUSINESS), (user("carla.nowak"), BUSINESS)],
)
CHART_METADATA = ([], None, [(group("data-platform"), TECHNICAL)])

# Container ownership is technical; the schema deliberately mixes a group and a user owner.
CATALOG_OWNERS = [(group("data-platform"), TECHNICAL)]
SCHEMA_OWNERS = CATALOG_OWNERS + [(user("erin.fischer"), TECHNICAL)]

# `admins`/`members`/`groups` are required by corpGroupInfo, membership is set on the user instead.
EMPTY_MEMBERSHIP = {"admins": [], "members": [], "groups": []}


def group_object(gid, name):
    info = {"displayName": name, "description": f"{name} team", **EMPTY_MEMBERSHIP}
    return entity(group(gid), corpGroupInfo=info)


def user_object(username, name, gid):
    email = f"{username}@example.com"
    info = {"active": True, "displayName": name, "fullName": name, "email": email}
    return entity(
        user(username),
        corpUserInfo=info,
        corpUserEditableInfo={"email": email},
        groupMembership={"groups": [group(gid)]},
    )


def data_product_object(pid, name, desc, domain_id, tables):
    assets = [{"destinationUrn": dataset(table)} for table in tables]
    return entity(
        f"urn:li:dataProduct:{pid}",
        dataProductProperties={"name": name, "description": desc, "assets": assets},
        domains={"domains": [domain(domain_id)]},
    )


def asset_object(urn, tags, domain_id, owners):
    """The aspects RIF reads off an asset (dataset, chart or dashboard). Tags and domain are only
    sent when set, so an asset can deliberately have none."""
    aspects = {"ownership": ownership(owners)}
    if tags:
        aspects["globalTags"] = {"tags": [{"tag": tag(tid)} for tid in tags]}
    if domain_id:
        aspects["domains"] = {"domains": [domain(domain_id)]}
    return entity(urn, **aspects)


def main():
    print("==> Waiting for DataHub GMS to be healthy")
    wait_for("GMS", gms_healthy)

    print("==> Groups, users, tags and domains")
    upsert("corpgroup", [group_object(gid, name) for gid, name in GROUPS.items()])
    upsert("corpuser", [user_object(name, *info) for name, info in USERS.items()])
    upsert(
        "tag", [entity(tag(tid), tagProperties={"name": n}) for tid, n in TAGS.items()]
    )
    upsert(
        "domain",
        [
            entity(domain(did), domainProperties={"name": name, "description": desc})
            for did, (name, desc) in DOMAINS.items()
        ],
    )

    # The data product `assets` are what creates the DataProductContains edges the RIF reads.
    print("==> Data products")
    upsert(
        "dataproduct",
        [data_product_object(pid, *info) for pid, info in DATA_PRODUCTS.items()],
    )

    print(
        "==> Tags / domains / ownership of tables, topics, chart, dashboard, containers"
    )
    # Trino tables and Kafka topics are both `dataset` entities, so they go in one call.
    upsert(
        "dataset",
        [asset_object(dataset(table), *metadata) for table, metadata in TABLES.items()]
        + [asset_object(topic(name), *metadata) for name, metadata in TOPICS.items()],
    )
    upsert("chart", [asset_object(CHART, *CHART_METADATA)])
    upsert("dashboard", [asset_object(DASHBOARD, *DASHBOARD_METADATA)])
    upsert(
        "container",
        [
            entity(CATALOG, ownership=ownership(CATALOG_OWNERS)),
            entity(SCHEMA, ownership=ownership(SCHEMA_OWNERS)),
        ],
    )

    # DataProductContains edges are graph-indexed asynchronously (unlike the aspects above), so wait
    # until at least one asset per data product resolves the incoming edge. This makes the metadata
    # state deterministic for the RIF assertions in the next step.
    print("==> Waiting for data-product membership edges to index")
    for pid, (_, _, _, tables) in DATA_PRODUCTS.items():
        probe = dataset(tables[0])
        wait_for(f"{pid} edge", lambda: data_product_edge_indexed(probe), attempts=24)

    print(
        "==> Successfully created all DataHub users, groups, tags, domains, data products"
    )
    print("    and attached them to the Trino, Kafka and Superset resources")


if __name__ == "__main__":
    main()
