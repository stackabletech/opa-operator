#!/usr/bin/env python
"""Populate DataHub with the test users, groups, tags, domains, data products and assignments
that the resource-info-fetcher (RIF) test asserts on.

The metadata is attached to the entities ingested in 31/32/33: the Trino `tpch.sf1`
catalog/schema/tables, the Kafka topics and the Superset chart/dashboard. Writes go to the GMS
OpenAPI v3 entity endpoint as UPSERTs (createIfNotExists=false), so the script is idempotent /
re-runnable. async=false makes a rejected write fail loudly.

Every resource type gets a *distinct* set of tags/domain/owners on purpose: RIF answers with an
empty record when a URN resolves to nothing, so assertions on all-empty metadata would pass even
for a wrong URN. Distinguishable metadata is what makes the URN derivation in
resource_to_urn_mapping.rs actually testable.

The model (see the RIF response contract in rust/resource-info-fetcher/src/backend/data_hub):
  - 3 groups own the two halves of the TPC-H model plus the shared reference layer.
  - 5 users are members of those groups.
  - tag `pii` on customer+supplier, `public` on nation+region (rest untagged for contrast).
  - domain Sales (customer/orders/lineitem) and Supply Chain (supplier/part/partsupp).
  - data products Order Analytics (Sales) and Supplier 360 (Supply Chain).
  - ownership at catalog + schema (technical) and table (business) level; the schema mixes a
    group *and* a user owner so the RIF's owner-by-type resolution is fully exercised.
  - the Kafka topic `orders` and the Superset chart/dashboard each get their own combination;
    the topic `page-views` is deliberately left bare as the "no metadata" contrast.
"""

import hashlib
import json
import os
import sys
import time

import requests

GMS = os.environ.get("GMS", "http://datahub-datahub-gms:8080")
TOKEN = os.environ["DATAHUB_GMS_TOKEN"]
SESSION = requests.Session()
SESSION.headers.update({"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})

BUSINESS = "urn:li:ownershipType:__system__business_owner"
TECHNICAL = "urn:li:ownershipType:__system__technical_owner"

# URNs from the ingestions in 31/32/33 (env=PROD throughout). Each recipe sets a
# `platform_instance: ${POD_NAMESPACE}/<cluster>`, so the DataHub `instance` is that value (not the
# `env`) and it appears in the container GUIDs and as the entity-name prefix. The namespace is only
# known at runtime, so every URN is computed here rather than hardcoded.
INSTANCE = f"{os.environ['POD_NAMESPACE']}/my-trino"
KAFKA_INSTANCE = f"{os.environ['POD_NAMESPACE']}/test-kafka"
SUPERSET_INSTANCE = f"{os.environ['POD_NAMESPACE']}/my-superset"


def container_urn(container_key):
    """Reproduce DataHub's `datahub_guid` (mirrors `container_urn` in resource_to_urn_mapping.rs):
    serialize the container key to compact, key-sorted JSON and MD5-hash it."""
    key_json = json.dumps(container_key, sort_keys=True, separators=(",", ":"))
    return f"urn:li:container:{hashlib.md5(key_json.encode()).hexdigest()}"


CATALOG = container_urn({"platform": "trino", "instance": INSTANCE, "database": "tpch"})  # tpch
SCHEMA = container_urn(
    {"platform": "trino", "instance": INSTANCE, "database": "tpch", "schema": "sf1"}
)  # tpch.sf1

# The Superset seed in 23-install-superset creates exactly one chart and one dashboard in a fresh
# Superset, so both get id 1. test-regorule.py asserts on the same ids.
CHART = f"urn:li:chart:(superset,{SUPERSET_INSTANCE}.1)"
DASHBOARD = f"urn:li:dashboard:(superset,{SUPERSET_INSTANCE}.1)"


def dataset(table):
    return f"urn:li:dataset:(urn:li:dataPlatform:trino,{INSTANCE}.tpch.sf1.{table},PROD)"


def topic(name):
    """A Kafka topic is a dataset too, just without the database/schema levels."""
    return f"urn:li:dataset:(urn:li:dataPlatform:kafka,{KAFKA_INSTANCE}.{name},PROD)"


def upsert(entity_type, objects):
    """UPSERT a list of {urn, <aspect>:{value:...}} objects to /openapi/v3/entity/<type>."""
    resp = SESSION.post(
        f"{GMS}/openapi/v3/entity/{entity_type}",
        params={"async": "false", "createIfNotExists": "false"},
        json=objects,
    )
    if not resp.ok:
        sys.exit(f"ERROR upserting {entity_type}: HTTP {resp.status_code}: {resp.text}")


def ownership_aspect(owners):
    """Build an `ownership` aspect value from a list of (owner_urn, ownership_type_urn) pairs."""
    owner_types = {}
    for owner_urn, type_urn in owners:
        owner_types.setdefault(type_urn, []).append(owner_urn)
    return {
        "value": {
            "owners": [
                {
                    "owner": owner_urn,
                    "typeUrn": type_urn,
                    "type": "NONE",
                    "source": {"type": "MANUAL"},
                }
                for owner_urn, type_urn in owners
            ],
            "ownerTypes": owner_types,
            "lastModified": {"actor": "urn:li:corpuser:datahub", "time": 0},
        }
    }


def wait_for_health():
    for attempt in range(60):
        try:
            if SESSION.get(f"{GMS}/health", timeout=10).ok:
                return
        except requests.RequestException:
            pass
        print(f"  GMS not ready yet (attempt {attempt + 1}/60), retrying in 5s")
        time.sleep(5)
    sys.exit("GMS did not become healthy in time")


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
    "customer": (["pii"], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
    "orders": ([], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
    "lineitem": ([], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
    "supplier": (["pii"], "supply-chain", [("urn:li:corpGroup:procurement", BUSINESS)]),
    "part": ([], "supply-chain", [("urn:li:corpGroup:procurement", BUSINESS)]),
    "partsupp": ([], "supply-chain", [("urn:li:corpGroup:procurement", BUSINESS)]),
    "nation": (["public"], None, [("urn:li:corpGroup:data-platform", TECHNICAL)]),
    "region": (["public"], None, [("urn:li:corpGroup:data-platform", TECHNICAL)]),
}

# Kafka topic -> same shape as TABLES. The topics come from the `kafka-seed` Job in
# 21-install-kafka; `page-views` is intentionally absent so the test has an ingested-but-unannotated
# resource to compare against.
TOPICS = {
    "orders": (["pii"], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
}

# Superset chart/dashboard, again the same shape. The dashboard mixes a group and a user owner (like
# the Trino schema does), the chart carries no tags and no domain - each resource type ends up with
# a combination no other one has, so a wrong URN cannot accidentally satisfy the assertions.
DASHBOARD_METADATA = (
    ["public"],
    "supply-chain",
    [
        ("urn:li:corpGroup:procurement", BUSINESS),
        ("urn:li:corpuser:carla.nowak", BUSINESS),
    ],
)
CHART_METADATA = ([], None, [("urn:li:corpGroup:data-platform", TECHNICAL)])


def asset_object(urn, tags, domain_id, owners):
    """Build the UPSERT object for an asset (dataset, chart or dashboard): the aspects RIF reads.
    Tags and domain are only sent when set, so an asset can deliberately have none."""
    obj = {"urn": urn, "ownership": ownership_aspect(owners)}
    if tags:
        obj["globalTags"] = {"value": {"tags": [{"tag": f"urn:li:tag:{t}"} for t in tags]}}
    if domain_id:
        obj["domains"] = {"value": {"domains": [f"urn:li:domain:{domain_id}"]}}
    return obj


def main():
    print("==> Waiting for DataHub GMS to be healthy")
    wait_for_health()

    print("==> Groups")
    upsert(
        "corpgroup",
        [
            {
                "urn": f"urn:li:corpGroup:{gid}",
                "corpGroupInfo": {
                    "value": {
                        "displayName": name,
                        "description": f"{name} team",
                        "admins": [],
                        "members": [],
                        "groups": [],
                    }
                },
            }
            for gid, name in GROUPS.items()
        ],
    )

    print("==> Users")
    upsert(
        "corpuser",
        [
            {
                "urn": f"urn:li:corpuser:{username}",
                "corpUserInfo": {
                    "value": {
                        "active": True,
                        "displayName": full_name,
                        "fullName": full_name,
                        "email": f"{username}@example.com",
                    }
                },
                "corpUserEditableInfo": {"value": {"email": f"{username}@example.com"}},
                "groupMembership": {"value": {"groups": [f"urn:li:corpGroup:{gid}"]}},
            }
            for username, (full_name, gid) in USERS.items()
        ],
    )

    print("==> Tags")
    upsert(
        "tag",
        [
            {"urn": f"urn:li:tag:{tid}", "tagProperties": {"value": {"name": name}}}
            for tid, name in TAGS.items()
        ],
    )

    print("==> Domains")
    upsert(
        "domain",
        [
            {
                "urn": f"urn:li:domain:{did}",
                "domainProperties": {"value": {"name": name, "description": desc}},
            }
            for did, (name, desc) in DOMAINS.items()
        ],
    )

    print("==> Data products (assets create the DataProductContains edges the RIF reads)")
    upsert(
        "dataproduct",
        [
            {
                "urn": f"urn:li:dataProduct:{pid}",
                "dataProductProperties": {
                    "value": {
                        "name": name,
                        "description": desc,
                        "assets": [{"destinationUrn": dataset(t)} for t in tables],
                    }
                },
                "domains": {"value": {"domains": [f"urn:li:domain:{did}"]}},
            }
            for pid, (name, desc, did, tables) in DATA_PRODUCTS.items()
        ],
    )

    # Trino tables and Kafka topics are both `dataset` entities, so they go in one call.
    print("==> Table / topic tags / domains / ownership")
    upsert(
        "dataset",
        [asset_object(dataset(table), *metadata) for table, metadata in TABLES.items()]
        + [asset_object(topic(name), *metadata) for name, metadata in TOPICS.items()],
    )

    print("==> Superset chart / dashboard tags / domains / ownership")
    upsert("chart", [asset_object(CHART, *CHART_METADATA)])
    upsert("dashboard", [asset_object(DASHBOARD, *DASHBOARD_METADATA)])

    print("==> Catalog / schema ownership")
    upsert(
        "container",
        [
            {"urn": CATALOG, "ownership": ownership_aspect([("urn:li:corpGroup:data-platform", TECHNICAL)])},
            # The schema deliberately mixes a group and a user owner (both technical).
            {
                "urn": SCHEMA,
                "ownership": ownership_aspect(
                    [
                        ("urn:li:corpGroup:data-platform", TECHNICAL),
                        ("urn:li:corpuser:erin.fischer", TECHNICAL),
                    ]
                ),
            },
        ],
    )

    # DataProductContains edges are graph-indexed asynchronously (unlike the aspects above), so
    # wait until at least one asset per data product resolves the incoming edge. This makes the
    # metadata state deterministic for the RIF assertions in the next step.
    print("==> Waiting for data-product membership edges to index")
    for pid, (_, _, _, tables) in DATA_PRODUCTS.items():
        probe = dataset(tables[0])
        query = (
            '{ entity(urn: "%s") { relationships(input: {types: ["DataProductContains"], '
            'direction: INCOMING, count: 10}) { total } } }' % probe
        )
        for attempt in range(24):
            resp = SESSION.post(f"{GMS}/api/graphql", json={"query": query}, timeout=20)
            total = (
                resp.json().get("data", {}).get("entity", {}).get("relationships", {}).get("total", 0)
                if resp.ok
                else 0
            )
            if total:
                print(f"  {pid}: edge indexed")
                break
            print(f"  {pid}: not indexed yet (attempt {attempt + 1}/24), retrying in 5s")
            time.sleep(5)
        else:
            sys.exit(f"data product {pid} membership edge never indexed")

    print("==> Successfully created all DataHub users, groups, tags, domains, data products")
    print("    and attached them to the Trino, Kafka and Superset resources")


if __name__ == "__main__":
    main()
