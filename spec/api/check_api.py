#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Static checks for spec/api/aether-api-v0.openrpc.json: structure, unique
method names, resolvable $refs, required lists naming real properties."""
import json
import pathlib
import re
import sys

doc = json.loads((pathlib.Path(__file__).parent / "aether-api-v0.openrpc.json").read_text())
schemas = doc["components"]["schemas"]
errors = []


def walk(node, where):
    if isinstance(node, dict):
        if "$ref" in node:
            m = re.fullmatch(r"#/components/schemas/(\w+)", node["$ref"])
            if not m or m.group(1) not in schemas:
                errors.append(f"{where}: unresolved $ref {node['$ref']}")
        if node.get("type") == "object" and "required" in node:
            for r in node["required"]:
                if r not in node.get("properties", {}):
                    errors.append(f"{where}: required '{r}' not in properties")
        for k, v in node.items():
            walk(v, f"{where}/{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            walk(v, f"{where}[{i}]")


assert doc["openrpc"].startswith("1.3"), "not OpenRPC 1.3"
names = [m["name"] for m in doc["methods"]]
dups = {n for n in names if names.count(n) > 1}
if dups:
    errors.append(f"duplicate methods: {sorted(dups)}")
for m in doc["methods"]:
    if not re.fullmatch(r"[a-z]+\.[a-zA-Z]+", m["name"]):
        errors.append(f"bad method name {m['name']}")
    pn = [p["name"] for p in m["params"]]
    if len(pn) != len(set(pn)):
        errors.append(f"{m['name']}: duplicate params")
    for p in m["params"]:
        if "schema" not in p:
            errors.append(f"{m['name']}.{p['name']}: no schema")
walk(doc, "#")
used = set(re.findall(r'#/components/schemas/(\w+)', json.dumps(doc)))
unused = sorted(set(schemas) - used)
print(f"{len(names)} methods, {len(schemas)} schemas, {len(doc['x-events'])} events; unused schemas: {unused or 'none'}")
for e in errors:
    print("ERROR", e)
sys.exit(1 if errors else 0)
