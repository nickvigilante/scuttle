"""Spec fixups for the SDK spike. Every patch is logged so the report can count them."""
import json
log = []

# Swagger 2.0 input for openapi-generator: header params may not carry `example`.
sw = json.load(open("swagger.json"))
for path, item in sw["paths"].items():
    for method, op in item.items():
        for prm in op.get("parameters", []) if isinstance(op, dict) else []:
            if prm.get("in") == "header" and "example" in prm:
                del prm["example"]
                log.append(f"swagger2: drop header example {method.upper()} {path} {prm['name']}")
for name, d in sw["definitions"].items():
    names = d.get("x-enum-varnames")
    if names and "_" in names:
        d["x-enum-varnames"] = ["Unspecified" if n == "_" else n for n in names]
        log.append(f"swagger2: rename enum varname '_' to Unspecified in {name}")
for path, item in sw["paths"].items():
    for method, op in item.items():
        if not isinstance(op, dict):
            continue
        keep = []
        for prm in op.get("parameters", []):
            if prm.get("in") == "path" and "{" + prm["name"] + "}" not in path:
                log.append(f"swagger2: drop undeclared path param {prm['name']} from {method.upper()} {path}")
                continue
            keep.append(prm)
        if "parameters" in op:
            op["parameters"] = keep
json.dump(sw, open("swagger.patched.json", "w"))

# OpenAPI 3 input for progenitor.
spec = json.load(open("openapi3.json"))
REMAP = {"*/*": "application/octet-stream", "application/scim+json": "application/json",
         "text/event-stream": "application/octet-stream"}
for path, item in spec["paths"].items():
    for method, op in item.items():
        if not isinstance(op, dict):
            continue
        oid = op.get("operationId")
        rb = op.get("requestBody")
        if rb:
            types = list(rb.get("content", {}))
            if len(types) > 1:
                keep = "application/octet-stream"
                rb["content"] = {keep: {"schema": {"type": "string", "format": "binary"}}}
                log.append(f"oas3: collapse {len(types)} request media types to {keep} for {oid}")
            else:
                for t in types:
                    if t in REMAP:
                        body = rb["content"][t]
                        if REMAP[t] == "application/octet-stream":
                            body = {"schema": {"type": "string", "format": "binary"}}
                        rb["content"] = {REMAP[t]: body}
                        log.append(f"oas3: request {t} -> {REMAP[t]} for {oid}")
        for code, r in op.get("responses", {}).items():
            for t in list(r.get("content", {})):
                if t in REMAP:
                    body = r["content"].pop(t)
                    if REMAP[t] == "application/octet-stream":
                        body = {"schema": {"type": "string", "format": "binary"}}
                    r["content"][REMAP[t]] = body
                    log.append(f"oas3: response {code} {t} -> {REMAP[t]} for {oid}")
def fix_array_format(node, where="$"):
    if isinstance(node, dict):
        if node.get("type") == "array" and "format" in node and isinstance(node.get("items"), dict):
            fmt = node.pop("format")
            node["items"].setdefault("format", fmt)
            log.append(f"oas3: move array format {fmt!r} to items at {where}")
        for k, v in node.items():
            fix_array_format(v, f"{where}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            fix_array_format(v, f"{where}[{i}]")
fix_array_format(spec)

def dedupe_enums(node, where="$"):
    if isinstance(node, dict):
        e = node.get("enum")
        if isinstance(e, list) and len(e) != len(set(map(json.dumps, e))):
            seen, out = set(), []
            for v in e:
                k = json.dumps(v)
                if k not in seen:
                    seen.add(k); out.append(v)
            node["enum"] = out
            log.append(f"oas3: dedupe enum ({len(e)} -> {len(out)}) at {where}")
        for k, v in node.items():
            dedupe_enums(v, f"{where}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            dedupe_enums(v, f"{where}[{i}]")
dedupe_enums(spec)

def drop_narrowing_enums(node, where="$"):
    if isinstance(node, dict):
        ao = node.get("allOf")
        if "enum" in node and isinstance(ao, list) and len(ao) == 1 and "$ref" in ao[0]:
            dropped = node.pop("enum")
            log.append(f"oas3: drop narrowing enum {dropped} over {ao[0]['$ref'].split('/')[-1]} at {where}")
        for k, v in node.items():
            drop_narrowing_enums(v, f"{where}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            drop_narrowing_enums(v, f"{where}[{i}]")
drop_narrowing_enums(spec)

for path, item in spec["paths"].items():
    for method, op in item.items():
        if not isinstance(op, dict):
            continue
        resp = op.get("responses", {})
        ok = sorted(c for c in resp if c.startswith("2"))
        if len(ok) < 2:
            continue
        schemas = {json.dumps(resp[c].get("content")) for c in ok}
        if len(schemas) == 1:
            merged = resp[ok[0]]
            for c in ok:
                del resp[c]
            resp["2XX"] = merged
            log.append(f"oas3: merge identical {ok} into 2XX for {method.upper()} {path}")
        else:
            for c in ok[1:]:
                del resp[c]
            log.append(f"oas3: keep only {ok[0]} of {ok} (LOSSY) for {method.upper()} {path}")
json.dump(spec, open("openapi3.patched.json", "w"))
open("patches.log", "w").write("\n".join(log) + "\n")
print(f"PATCHES {len(log)}")
