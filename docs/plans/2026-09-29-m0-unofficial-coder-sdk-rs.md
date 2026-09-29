# M0: unofficial-coder-sdk-rs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `unofficial-coder-sdk-rs`, a Rust SDK for the Coder API that is regenerated from coder/coder's Swagger spec by a repeatable pipeline, plus the hand-written layer scuttle needs: session discovery, typed errors, and the chat stream and watch WebSockets.

**Architecture:** A Cargo workspace with three members. `coder-api-gen` holds progenitor output that is checked in and never edited by hand. `coder-sdk` wraps it with hand-written code. `xtask` runs progenitor. A shell script drives the pipeline (fetch spec, list raw JSON fields with a Go tool, convert to OpenAPI 3, patch with a Python script, generate), and GitHub Actions runs that script for any coder/coder ref.

**Tech Stack:** Rust 2024 edition, progenitor 0.15, reqwest 0.13, reqwest-websocket 0.6, tokio, serde, secrecy 0.10, thiserror 2, wiremock and tokio-tungstenite for tests; Go (stdlib only) for the raw-field lister; Python 3 (stdlib only) for the patch script; Node's `npx swagger2openapi@7` for conversion; Docker for smoke tests.

**Spec:** `~/git/nickvigilante/scuttle/docs/specs/2026-09-28-scuttle-design.md`, sections 1, 5 (session and authentication), 7, 8, and 9.

## Global Constraints

- Repo path: `~/git/nickvigilante/unofficial-coder-sdk-rs`, created locally with `git init`. Do not create a GitHub repo or push; the author does that.
- Every crate sets `publish = false`. Nothing is published to crates.io.
- The README states in its first sentence that the project is unofficial and not supported by Coder.
- No secrets in any committed file, fixture, or log. Test tokens are obviously fake strings such as `test-token-not-real`.
- `coder-api-gen/src/generated.rs` is only ever written by `cargo xtask generate`. Never edit it by hand.
- Commit messages use Conventional Commits and end with the trailer `Assisted-by: AI`. Never name a model or vendor.
- Markdown files use one sentence per line. Do not use em dashes, en dashes, or spaced double hyphens as punctuation.
- GitHub Actions workflows set top-level `permissions: {}` and grant only what each job needs.
- Rust edition 2024, stable toolchain pinned in `rust-toolchain.toml` to `1.98`.
- The generation baseline for development is the local coder/coder checkout at `<coder checkout>`, commit `d1597a583b`.

## Deviation from the spec, called out for review

Spec section 7 did not decide how to handle enums.
The SDK spike showed progenitor generates closed Rust enums, which fail to deserialize a whole response when the server sends a value the SDK has never seen.
The author's Coder deployment runs ahead of any tagged release, so that would happen routinely.
This plan therefore adds an `open-enums` patch rule (Task 3) that turns every enum in the spec into its base type with the allowed values listed in the description, and adds hand-written open enums with an `Unknown` variant in `coder-sdk` for the three types scuttle matches on (Task 8).
This rule also makes the spike's "drop narrowing enums" and "dedupe enum values" rules unnecessary, so they are omitted.

## Review Focus

1. A server newer than the SDK sends an unknown stream event type, part type, or chat status: the SDK must yield an `Unknown` value for that event and keep going, not fail the frame. Pinned by `unknown_event_type_is_yielded_not_fatal` in Task 8 and by the open-enums rule tests in Task 3.
1. A WebSocket closes abnormally mid-stream or the server restarts: the stream must yield one `Err(Error::StreamClosed { .. })` and then end, never hang. Pinned by `abnormal_close_yields_error_then_ends` in Task 8.
1. The session URL in the `coder` CLI's config has a trailing slash, uppercase host, explicit port, or trailing newline, and the token file has a trailing newline: discovery must still find the right token. Pinned by `normalizes_host_and_trims_files` in Task 7.
1. A WebSocket upgrade is rejected with `401`: the caller must get `Error::Unauthorized`, not an opaque handshake error, so scuttle can tell the user to run `coder login`. Pinned by `upgrade_401_maps_to_unauthorized` in Task 8.
1. A frame carries the maximum 256 events and exceeds 1 MiB: every event must be yielded in order. Pinned by `large_batched_frame_yields_all_events_in_order` in Task 8.

---

## File Structure

```text
unofficial-coder-sdk-rs/
  Cargo.toml                         workspace manifest and shared dependency versions
  rust-toolchain.toml                toolchain pin
  README.md                          unofficial disclaimer, layout, how to regenerate
  .gitignore
  crates/
    coder-api-gen/
      Cargo.toml
      src/lib.rs                     includes generated.rs with lint allowances
      src/generated.rs               progenitor output (generated, checked in)
    coder-sdk/
      Cargo.toml
      src/lib.rs                     public API re-exports
      src/error.rs                   Error type and progenitor error conversion
      src/session.rs                 session discovery from the coder CLI
      src/client.rs                  Client construction and server_version
      src/enums.rs                   open enums for ChatStatus, part type, event type
      src/stream.rs                  stream_chat and watch_chats
      tests/errors.rs                wiremock tests for error mapping
      tests/stream.rs                local WebSocket server tests
      tests/smoke.rs                 ignored tests against a real coderd
  xtask/
    Cargo.toml
    src/main.rs                      `cargo xtask generate`
  tools/
    rawfields/go.mod
    rawfields/main.go                lists json.RawMessage and []byte fields
    rawfields/main_test.go
    rawfields/testdata/sample/sample.go
    patch_spec.py                    OpenAPI 3 patch rules with a change log
    test_patch_spec.py
  scripts/
    regenerate.sh                    full pipeline for a coder/coder ref or local path
    smoke.sh                         starts coderd in Docker and runs smoke tests
  spec/
    swagger.json                     fetched input (generated)
    rawfields.json                   raw field list (generated)
    openapi.patched.json             progenitor input (generated)
    openapi3.json                    converted spec before patching (generated)
    patches.log                      one line per patch applied (generated)
    coder-ref.txt                    ref and commit the spec came from (generated)
  .github/workflows/
    ci.yml                           fmt, clippy, tests for Rust, Go, and Python
    regenerate.yml                   dispatch and scheduled regeneration with a PR
```

---

### Task 1: Scaffold the workspace

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `README.md`
- Create: `crates/coder-api-gen/Cargo.toml`, `crates/coder-api-gen/src/lib.rs`, `crates/coder-api-gen/src/generated.rs`
- Create: `crates/coder-sdk/Cargo.toml`, `crates/coder-sdk/src/lib.rs`
- Create: `xtask/Cargo.toml`, `xtask/src/main.rs`

**Interfaces:**
- Produces: workspace members `coder-api-gen`, `coder-sdk`, `xtask`; the `[workspace.dependencies]` versions every later task uses.

- [ ] **Step 1: Create the repo**

```bash
mkdir -p ~/git/nickvigilante/unofficial-coder-sdk-rs && cd ~/git/nickvigilante/unofficial-coder-sdk-rs && git init -b main
```

- [ ] **Step 2: Write the workspace manifest**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/coder-api-gen", "crates/coder-sdk", "xtask"]

[workspace.package]
edition = "2024"
publish = false
repository = "https://github.com/nickvigilante/unofficial-coder-sdk-rs"

[workspace.dependencies]
base64 = "0.22"
chrono = { version = "0.4", features = ["serde"] }
futures = "0.3"
prettyplease = "0.3"
progenitor = "0.15"
progenitor-client = "0.15"
reqwest = { version = "0.13", features = ["json", "stream"] }
reqwest-websocket = "0.6"
secrecy = "0.10"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
syn = "3"
thiserror = "2"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "net", "time"] }
tokio-tungstenite = "0.30"
url = "2"
uuid = { version = "1", features = ["serde", "v4"] }
wiremock = "0.6"
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.98"
components = ["rustfmt", "clippy"]
```

`.gitignore`:

```text
/target
__pycache__/
/.coder-src/
```

- [ ] **Step 3: Create the three crates**

`crates/coder-api-gen/Cargo.toml`:

```toml
[package]
name = "coder-api-gen"
version = "0.0.0"
edition.workspace = true
publish.workspace = true
description = "Unofficial generated client for the Coder API. Not supported by Coder."

[dependencies]
chrono.workspace = true
futures.workspace = true
progenitor-client.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
uuid.workspace = true
```

`crates/coder-api-gen/src/lib.rs`:

```rust
//! Unofficial client for the Coder API, generated by progenitor from coder/coder's Swagger spec.
//!
//! Do not edit `generated.rs`; run `scripts/regenerate.sh` instead.
#![allow(clippy::all, clippy::pedantic, missing_docs, unused_imports, dead_code)]

include!("generated.rs");
```

`crates/coder-api-gen/src/generated.rs`:

```rust
// Placeholder until `cargo xtask generate` runs in Task 5.
```

`crates/coder-sdk/Cargo.toml`:

```toml
[package]
name = "coder-sdk"
version = "0.0.0"
edition.workspace = true
publish.workspace = true
description = "Unofficial hand-written layer over coder-api-gen. Not supported by Coder."

[dependencies]
base64.workspace = true
coder-api-gen = { path = "../coder-api-gen" }
futures.workspace = true
progenitor-client.workspace = true
reqwest.workspace = true
reqwest-websocket.workspace = true
secrecy.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
url.workspace = true
uuid.workspace = true

[dev-dependencies]
tokio-tungstenite.workspace = true
wiremock.workspace = true
```

`crates/coder-sdk/src/lib.rs`:

```rust
//! Unofficial hand-written layer over the generated Coder API client.
```

`xtask/Cargo.toml`:

```toml
[package]
name = "xtask"
version = "0.0.0"
edition.workspace = true
publish.workspace = true

[dependencies]
prettyplease.workspace = true
progenitor.workspace = true
serde_json.workspace = true
syn.workspace = true
```

`xtask/src/main.rs`:

```rust
fn main() {
    eprintln!("usage: cargo xtask generate <patched-spec> <out-file> <ref-file>");
    std::process::exit(2);
}
```

Add a Cargo alias so `cargo xtask` works, in `.cargo/config.toml`:

```toml
[alias]
xtask = "run --quiet --package xtask --"
```

- [ ] **Step 4: Write the README**

`README.md`:

```markdown
# unofficial-coder-sdk-rs

This is an unofficial Rust SDK for the Coder API, and it is not built, supported, or endorsed by Coder.

It exists to support scuttle, a personal terminal client for Coder Agents.

## Layout

- `crates/coder-api-gen` is generated from coder/coder's Swagger spec by progenitor and is never edited by hand.
- `crates/coder-sdk` is the hand-written layer: session discovery, errors, and the chat WebSocket streams.
- `xtask` runs the generator.
- `tools` holds the raw-field lister and the spec patch script.

## Regenerating

Run `scripts/regenerate.sh <coder-ref-or-path>` with a coder/coder tag, branch, commit, or local checkout path.
The script records what it applied in `spec/patches.log` and the source commit in `spec/coder-ref.txt`.
```

- [ ] **Step 5: Verify the workspace builds**

Run: `cargo check --workspace`
Expected: finishes with no errors.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "chore: scaffold the unofficial-coder-sdk-rs workspace

Assisted-by: AI"
```

---

### Task 2: Raw JSON field lister (Go)

The Swagger spec describes `json.RawMessage` and `[]byte` fields as integer arrays, and the spec alone cannot distinguish them from real integer arrays such as `deleted_message_ids`.
This tool reads the Go source instead.

**Files:**
- Create: `tools/rawfields/go.mod`, `tools/rawfields/main.go`, `tools/rawfields/main_test.go`, `tools/rawfields/testdata/sample/sample.go`

**Interfaces:**
- Produces: CLI `go run ./tools/rawfields <coder-checkout-dir>` printing a JSON array to stdout of `{"definition": "<pkg>.<Type>", "property": "<json name>", "kind": "json" | "bytes"}`, sorted by definition then property. Task 3 consumes this file as `spec/rawfields.json`.

- [ ] **Step 1: Write the test fixture**

`tools/rawfields/testdata/sample/sample.go`:

```go
package sample

import "encoding/json"

type Part struct {
	Args      json.RawMessage  `json:"args,omitempty"`
	Result    *json.RawMessage `json:"result"`
	Data      []byte           `json:"data"`
	Documented json.RawMessage `json:"documented" swaggertype:"object"`
	Hidden    json.RawMessage  `json:"-"`
	IDs       []int64          `json:"ids"`
	Untagged  json.RawMessage
}
```

- [ ] **Step 2: Write the failing test**

`tools/rawfields/main_test.go`:

```go
package main

import (
	"reflect"
	"testing"
)

func TestScanFindsRawFields(t *testing.T) {
	t.Parallel()
	got, err := scan("testdata")
	if err != nil {
		t.Fatal(err)
	}
	want := []field{
		{Definition: "sample.Part", Property: "Untagged", Kind: "json"},
		{Definition: "sample.Part", Property: "args", Kind: "json"},
		{Definition: "sample.Part", Property: "data", Kind: "bytes"},
		{Definition: "sample.Part", Property: "result", Kind: "json"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("scan() = %#v, want %#v", got, want)
	}
}
```

`tools/rawfields/go.mod`:

```text
module example.com/rawfields

go 1.26
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd tools/rawfields && go test ./...`
Expected: FAIL with `undefined: scan` and `undefined: field`.

- [ ] **Step 4: Implement the scanner**

`tools/rawfields/main.go`:

```go
// Command rawfields lists struct fields typed json.RawMessage or []byte in a
// Go source tree, keyed the way swaggo names Swagger definitions.
package main

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
)

type field struct {
	Definition string `json:"definition"`
	Property   string `json:"property"`
	Kind       string `json:"kind"`
}

var skipDirs = map[string]bool{"node_modules": true, "vendor": true, "site": true, ".git": true}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: rawfields <source-dir>")
		os.Exit(2)
	}
	fields, err := scan(os.Args[1])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(fields); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func scan(root string) ([]field, error) {
	fields := []field{}
	fset := token.NewFileSet()
	err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			if skipDirs[d.Name()] {
				return filepath.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return nil
		}
		file, err := parser.ParseFile(fset, path, nil, parser.SkipObjectResolution)
		if err != nil {
			return fmt.Errorf("parse %s: %w", path, err)
		}
		fields = append(fields, fileFields(file)...)
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Slice(fields, func(i, j int) bool {
		if fields[i].Definition != fields[j].Definition {
			return fields[i].Definition < fields[j].Definition
		}
		return fields[i].Property < fields[j].Property
	})
	return fields, nil
}

func fileFields(file *ast.File) []field {
	var out []field
	pkg := file.Name.Name
	for _, decl := range file.Decls {
		gen, ok := decl.(*ast.GenDecl)
		if !ok || gen.Tok != token.TYPE {
			continue
		}
		for _, spec := range gen.Specs {
			ts := spec.(*ast.TypeSpec)
			st, ok := ts.Type.(*ast.StructType)
			if !ok {
				continue
			}
			for _, f := range st.Fields.List {
				if len(f.Names) == 0 {
					continue
				}
				kind := rawKind(f.Type)
				if kind == "" {
					continue
				}
				tag := reflect.StructTag("")
				if f.Tag != nil {
					tag = reflect.StructTag(strings.Trim(f.Tag.Value, "`"))
				}
				if tag.Get("swaggertype") != "" {
					continue
				}
				name := strings.Split(tag.Get("json"), ",")[0]
				if name == "-" {
					continue
				}
				if name == "" {
					name = f.Names[0].Name
				}
				out = append(out, field{Definition: pkg + "." + ts.Name.Name, Property: name, Kind: kind})
			}
		}
	}
	return out
}

func rawKind(expr ast.Expr) string {
	if star, ok := expr.(*ast.StarExpr); ok {
		expr = star.X
	}
	switch t := expr.(type) {
	case *ast.SelectorExpr:
		if id, ok := t.X.(*ast.Ident); ok && id.Name == "json" && t.Sel.Name == "RawMessage" {
			return "json"
		}
	case *ast.ArrayType:
		if id, ok := t.Elt.(*ast.Ident); ok && t.Len == nil && id.Name == "byte" {
			return "bytes"
		}
	}
	return ""
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd tools/rawfields && go test ./...`
Expected: `ok  example.com/rawfields`.

- [ ] **Step 6: Run it against coder/coder and check a known field**

Run: `cd tools/rawfields && go run . <coder checkout> | python3 -c "import json,sys; f=json.load(sys.stdin); print(len(f)); print([x for x in f if x['definition']=='codersdk.ChatMessagePart'])"`
Expected: a count in the dozens, and the list includes `args`, `result`, and `provider_metadata` with kind `json`. `codersdk.EditChatMessageResponse` must not appear.

- [ ] **Step 7: Commit**

```bash
git add tools/rawfields && git commit -m "feat(tools): list raw json and byte fields from go source

Assisted-by: AI"
```

---

### Task 3: Spec patch script (Python)

**Files:**
- Create: `tools/patch_spec.py`, `tools/test_patch_spec.py`

**Interfaces:**
- Consumes: `spec/rawfields.json` from Task 2.
- Produces: CLI `python3 tools/patch_spec.py <openapi3.json> <rawfields.json> <out.json> <log>`; each log line is `<rule>\t<location>\t<detail>`. Rule names: `multi-media-request`, `remap-media`, `array-format`, `multi-success`, `raw-field`, `open-enums`.

- [ ] **Step 1: Write the failing tests**

`tools/test_patch_spec.py`:

```python
import unittest

import patch_spec


def op(**kw):
    return {"paths": {"/x": {"post": {"operationId": "x", **kw}}}, "components": {"schemas": {}}}


class PatchSpecTest(unittest.TestCase):
    def test_multi_media_request_collapses_to_binary(self):
        spec = op(requestBody={"content": {"image/png": {}, "text/plain": {}}}, responses={})
        log = patch_spec.apply(spec, [])
        body = spec["paths"]["/x"]["post"]["requestBody"]["content"]
        self.assertEqual(body, {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}})
        self.assertEqual(log[0][0], "multi-media-request")

    def test_remap_star_media_type(self):
        spec = op(responses={"404": {"content": {"*/*": {"schema": {"type": "object"}}}}})
        patch_spec.apply(spec, [])
        content = spec["paths"]["/x"]["post"]["responses"]["404"]["content"]
        self.assertEqual(content, {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}})

    def test_array_format_moves_to_items(self):
        spec = op(parameters=[{"name": "ids", "in": "query", "schema": {"type": "array", "format": "uuid", "items": {"type": "string"}}}], responses={})
        patch_spec.apply(spec, [])
        schema = spec["paths"]["/x"]["post"]["parameters"][0]["schema"]
        self.assertNotIn("format", schema)
        self.assertEqual(schema["items"]["format"], "uuid")

    def test_identical_success_responses_merge_to_2xx(self):
        body = {"content": {"application/json": {"schema": {"type": "object"}}}}
        spec = op(responses={"200": dict(body), "201": dict(body)})
        patch_spec.apply(spec, [])
        self.assertEqual(list(spec["paths"]["/x"]["post"]["responses"]), ["2XX"])

    def test_different_success_responses_keep_first_and_log_lossy(self):
        spec = op(responses={"200": {"content": {"application/json": {"schema": {"type": "object"}}}}, "204": {}})
        log = patch_spec.apply(spec, [])
        self.assertEqual(list(spec["paths"]["/x"]["post"]["responses"]), ["200"])
        self.assertIn("LOSSY", log[0][2])

    def test_raw_fields_become_free_form_or_base64(self):
        spec = {"paths": {}, "components": {"schemas": {"codersdk.Part": {"type": "object", "properties": {
            "args": {"type": "array", "items": {"type": "integer"}, "description": "Args."},
            "data": {"type": "array", "items": {"type": "integer"}},
            "ids": {"type": "array", "items": {"type": "integer"}},
        }}}}}
        raw = [
            {"definition": "codersdk.Part", "property": "args", "kind": "json"},
            {"definition": "codersdk.Part", "property": "data", "kind": "bytes"},
            {"definition": "codersdk.Missing", "property": "x", "kind": "json"},
        ]
        patch_spec.apply(spec, raw)
        props = spec["components"]["schemas"]["codersdk.Part"]["properties"]
        self.assertEqual(props["args"], {"description": "Args."})
        self.assertEqual(props["data"], {"type": "string", "format": "byte"})
        self.assertEqual(props["ids"], {"type": "array", "items": {"type": "integer"}})

    def test_open_enums_removes_enum_and_documents_values(self):
        spec = {"paths": {}, "components": {"schemas": {
            "codersdk.ChatStatus": {"type": "string", "enum": ["waiting", "running"], "x-enum-varnames": ["A", "B"]},
            "codersdk.User": {"type": "object", "properties": {"status": {"enum": ["active"], "allOf": [{"$ref": "#/components/schemas/codersdk.UserStatus"}]}}},
        }}}
        patch_spec.apply(spec, [])
        status = spec["components"]["schemas"]["codersdk.ChatStatus"]
        self.assertEqual(status, {"type": "string", "description": "Known values: `waiting`, `running`."})
        user_status = spec["components"]["schemas"]["codersdk.User"]["properties"]["status"]
        self.assertNotIn("enum", user_status)

    def test_apply_is_idempotent(self):
        spec = op(requestBody={"content": {"image/png": {}, "text/plain": {}}}, responses={"200": {}, "201": {}})
        patch_spec.apply(spec, [])
        self.assertEqual(patch_spec.apply(spec, []), [])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd tools && python3 -m unittest test_patch_spec -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'patch_spec'`.

- [ ] **Step 3: Implement the patch script**

`tools/patch_spec.py`:

```python
"""Patch coder/coder's OpenAPI 3 spec so progenitor can generate a usable Rust client.

Every change is returned as a (rule, location, detail) tuple and written to the log,
so a regeneration PR shows exactly how the upstream spec differs from what progenitor accepts.
"""

import json
import sys

BINARY = {"type": "string", "format": "binary"}
REMAP = {"*/*": "application/octet-stream", "application/scim+json": "application/json", "text/event-stream": "application/octet-stream"}
METHODS = ("get", "put", "post", "patch", "delete")


def operations(spec):
    for path, item in spec.get("paths", {}).items():
        for method in METHODS:
            if method in item:
                yield f"{method.upper()} {path}", item[method]


def remap_content(content, where, log):
    for media in list(content):
        if media in REMAP:
            target = REMAP[media]
            body = content.pop(media)
            content[target] = {"schema": dict(BINARY)} if target == "application/octet-stream" else body
            log.append(("remap-media", where, f"{media} -> {target}"))


def fix_operations(spec, log):
    for where, op in operations(spec):
        body = op.get("requestBody")
        if body and len(body.get("content", {})) > 1:
            count = len(body["content"])
            body["content"] = {"application/octet-stream": {"schema": dict(BINARY)}}
            log.append(("multi-media-request", where, f"{count} media types -> application/octet-stream"))
        elif body:
            remap_content(body.get("content", {}), where, log)
        responses = op.get("responses", {})
        for code, response in responses.items():
            remap_content(response.get("content", {}), f"{where} {code}", log)
        success = sorted(c for c in responses if c.startswith("2") and c != "2XX")
        if len(success) < 2:
            continue
        bodies = {json.dumps(responses[c].get("content"), sort_keys=True) for c in success}
        if len(bodies) == 1:
            merged = responses[success[0]]
            for code in success:
                del responses[code]
            responses["2XX"] = merged
            log.append(("multi-success", where, f"merged identical {success} into 2XX"))
        else:
            for code in success[1:]:
                del responses[code]
            log.append(("multi-success", where, f"kept {success[0]} of {success} (LOSSY)"))


def walk(node, where, visit):
    if isinstance(node, dict):
        visit(node, where)
        for key, value in node.items():
            walk(value, f"{where}.{key}", visit)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            walk(value, f"{where}[{index}]", visit)


def fix_array_format(spec, log):
    def visit(node, where):
        if node.get("type") == "array" and "format" in node and isinstance(node.get("items"), dict):
            fmt = node.pop("format")
            node["items"].setdefault("format", fmt)
            log.append(("array-format", where, f"moved format {fmt} to items"))

    walk(spec, "$", visit)


def fix_raw_fields(spec, raw_fields, log):
    schemas = spec.get("components", {}).get("schemas", {})
    for entry in raw_fields:
        props = schemas.get(entry["definition"], {}).get("properties", {})
        prop = props.get(entry["property"])
        if prop is None or prop.get("type") != "array":
            continue
        description = {"description": prop["description"]} if "description" in prop else {}
        if entry["kind"] == "json":
            props[entry["property"]] = description
        else:
            props[entry["property"]] = {"type": "string", "format": "byte", **description}
        log.append(("raw-field", f"{entry['definition']}.{entry['property']}", entry["kind"]))


def open_enums(spec, log):
    def visit(node, where):
        values = node.get("enum")
        if not isinstance(values, list):
            return
        del node["enum"]
        for key in [k for k in node if k.startswith("x-enum-")]:
            del node[key]
        if "allOf" not in node and "$ref" not in node:
            known = ", ".join(f"`{v}`" for v in values)
            existing = node.get("description", "")
            node["description"] = f"{existing} Known values: {known}.".strip()
        log.append(("open-enums", where, f"{len(values)} values"))

    walk(spec, "$", visit)


def apply(spec, raw_fields):
    log = []
    fix_operations(spec, log)
    fix_array_format(spec, log)
    fix_raw_fields(spec, raw_fields, log)
    open_enums(spec, log)
    return log


def main(argv):
    if len(argv) != 5:
        print("usage: patch_spec.py <openapi3.json> <rawfields.json> <out.json> <log>", file=sys.stderr)
        return 2
    with open(argv[1]) as f:
        spec = json.load(f)
    with open(argv[2]) as f:
        raw_fields = json.load(f)
    log = apply(spec, raw_fields)
    with open(argv[3], "w") as f:
        json.dump(spec, f, indent=1, sort_keys=True)
        f.write("\n")
    with open(argv[4], "w") as f:
        for rule, where, detail in log:
            f.write(f"{rule}\t{where}\t{detail}\n")
    counts = {}
    for rule, _, _ in log:
        counts[rule] = counts.get(rule, 0) + 1
    print(json.dumps(counts, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd tools && python3 -m unittest test_patch_spec -v`
Expected: all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add tools/patch_spec.py tools/test_patch_spec.py && git commit -m "feat(tools): add openapi patch rules with a change log

Assisted-by: AI"
```

---

### Task 4: `cargo xtask generate`

**Files:**
- Modify: `xtask/src/main.rs`

**Interfaces:**
- Produces: `cargo xtask generate <patched-spec> <out-file> <ref-file>` writes `<out-file>` starting with the line `// @generated by cargo xtask generate from coder/coder <contents of ref-file>. Do not edit.`

- [ ] **Step 1: Write the failing test**

Add to the bottom of `xtask/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_a_minimal_spec_with_header() {
        let spec = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title": "t", "version": "1"},
            "paths": {"/api/v2/buildinfo": {"get": {
                "operationId": "build-info",
                "responses": {"200": {"description": "OK", "content": {"application/json": {
                    "schema": {"type": "object", "properties": {"version": {"type": "string"}}}
                }}}}
            }}}
        });
        let code = render(spec, "v0.0.0 (abc123)").expect("render");
        assert!(code.starts_with("// @generated by cargo xtask generate from coder/coder v0.0.0 (abc123). Do not edit.\n"));
        assert!(code.contains("pub async fn build_info"));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p xtask`
Expected: FAIL with `unresolved import super::render`.

- [ ] **Step 3: Implement generation**

Replace `xtask/src/main.rs` above the test module with:

```rust
use std::{env, fs, process::ExitCode};

fn render(spec: serde_json::Value, source: &str) -> Result<String, String> {
    let spec = serde_json::from_value(spec).map_err(|e| format!("parse spec: {e}"))?;
    let mut generator = progenitor::Generator::default();
    let tokens = generator.generate_tokens(&spec).map_err(|e| format!("generate: {e:?}"))?;
    let file = syn::parse2(tokens).map_err(|e| format!("parse tokens: {e}"))?;
    Ok(format!(
        "// @generated by cargo xtask generate from coder/coder {source}. Do not edit.\n{}",
        prettyplease::unparse(&file)
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let [command, spec_path, out_path, ref_path] = args.as_slice() else {
        eprintln!("usage: cargo xtask generate <patched-spec> <out-file> <ref-file>");
        return ExitCode::from(2);
    };
    if command != "generate" {
        eprintln!("unknown command {command}");
        return ExitCode::from(2);
    }
    let run = || -> Result<(), String> {
        let spec: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(spec_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let source = fs::read_to_string(ref_path).map_err(|e| e.to_string())?;
        let code = render(spec, source.trim())?;
        fs::write(out_path, code).map_err(|e| e.to_string())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p xtask`
Expected: `renders_a_minimal_spec_with_header ... ok`.

- [ ] **Step 5: Commit**

```bash
git add xtask && git commit -m "feat(xtask): generate the api client with progenitor

Assisted-by: AI"
```

---

### Task 5: The regeneration pipeline and the first generated crate

**Files:**
- Create: `scripts/regenerate.sh`
- Create (generated): `spec/swagger.json`, `spec/rawfields.json`, `spec/openapi3.json`, `spec/openapi.patched.json`, `spec/patches.log`, `spec/coder-ref.txt`, `crates/coder-api-gen/src/generated.rs`

**Interfaces:**
- Consumes: Tasks 2, 3, and 4.
- Produces: `scripts/regenerate.sh <ref-or-path>`; a compiling `coder-api-gen` exposing `coder_api_gen::Client::new_with_client(&str, reqwest::Client)` and `coder_api_gen::types::*` (names such as `CodersdkChatStreamEvent`, `CodersdkChatWatchEvent`, `CodersdkChat`, `CodersdkChatMessagePart`).

- [ ] **Step 1: Write the script**

`scripts/regenerate.sh`:

```bash
#!/usr/bin/env bash
# Regenerate coder-api-gen from coder/coder at a tag, branch, commit, or local checkout path.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: scripts/regenerate.sh <coder-ref-or-local-path>" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
target="$1"
src="$root/.coder-src"

if [[ -d "$target" ]]; then
  src="$(cd "$target" && pwd)"
  ref="local"
  sha="$(git -C "$src" rev-parse --short=10 HEAD)"
else
  rm -rf "$src"
  git init -q "$src"
  git -C "$src" remote add origin https://github.com/coder/coder.git
  git -C "$src" fetch -q --depth 1 origin "$target"
  git -C "$src" checkout -q FETCH_HEAD
  ref="$target"
  sha="$(git -C "$src" rev-parse --short=10 HEAD)"
fi

mkdir -p "$root/spec"
cp "$src/coderd/apidoc/swagger.json" "$root/spec/swagger.json"
(cd "$root/tools/rawfields" && go run . "$src") > "$root/spec/rawfields.json"
npx -y swagger2openapi@7 --patch --outfile "$root/spec/openapi3.json" "$root/spec/swagger.json"
python3 "$root/tools/patch_spec.py" "$root/spec/openapi3.json" "$root/spec/rawfields.json" \
  "$root/spec/openapi.patched.json" "$root/spec/patches.log"
echo "$ref ($sha)" > "$root/spec/coder-ref.txt"
(cd "$root" && cargo xtask generate spec/openapi.patched.json crates/coder-api-gen/src/generated.rs spec/coder-ref.txt)
(cd "$root" && cargo build -p coder-api-gen)
echo "generated from coder/coder $ref ($sha)"
```

Run: `chmod +x scripts/regenerate.sh`

- [ ] **Step 2: Run it against the local baseline**

Run: `scripts/regenerate.sh <coder checkout>`
Expected: the last line reads `generated from coder/coder local (d1597a583b)` and `cargo build -p coder-api-gen` succeeds.

If progenitor panics or the build fails, the spec has a construct the rules do not cover yet. Read the panic message, add a rule and a test for it to Task 3's files following the existing pattern, and rerun. The spike's notes in `~/git/nickvigilante/scuttle/docs/research/sdk-spike/` list every construct found so far.

- [ ] **Step 3: Check the raw-field fix reached the generated code**

Run: `rg -n 'pub args: ' crates/coder-api-gen/src/generated.rs`
Expected: the `CodersdkChatMessagePart` field is typed `::serde_json::Value` (possibly wrapped in `Option`), not `Vec<i64>`.

Run: `rg -c 'pub enum Codersdk' crates/coder-api-gen/src/generated.rs`
Expected: a small number (only enums typify creates for `oneOf` constructs), confirming `open-enums` ran.

- [ ] **Step 4: Commit the pipeline and its first output**

```bash
git add scripts/regenerate.sh spec crates/coder-api-gen/src/generated.rs && git commit -m "feat: regenerate coder-api-gen from coder/coder d1597a583b

Assisted-by: AI"
```

---

### Task 6: Errors and the client

**Files:**
- Create: `crates/coder-sdk/src/error.rs`, `crates/coder-sdk/src/client.rs`
- Modify: `crates/coder-sdk/src/lib.rs`
- Test: `crates/coder-sdk/tests/errors.rs`

**Interfaces:**
- Consumes: `coder_api_gen::Client::new_with_client(&str, reqwest::Client)`; `progenitor_client::Error<E>`.
- Produces:
  - `coder_sdk::Error` with variants `Unauthorized`, `Api { status: u16, message: String, detail: Option<String>, validations: Vec<Validation> }`, `Transport(String)`, `Decode(String)`, `NotLoggedIn(String)`, `StreamClosed { code: Option<u16>, reason: String }`, `InvalidToken`.
  - `coder_sdk::Validation { field: String, detail: String }`.
  - `coder_sdk::Result<T> = std::result::Result<T, Error>`.
  - `coder_sdk::Error::from_progenitor<E: serde::Serialize>(err: progenitor_client::Error<E>) -> Error` (async).
  - `coder_sdk::Client::new(session: &Session) -> Result<Client>`, `Client::api(&self) -> &coder_api_gen::Client`, `Client::http(&self) -> &reqwest::Client`, `Client::base_url(&self) -> &url::Url`, `Client::server_version(&self) -> Result<String>` (async).
  - `coder_sdk::Session { url: url::Url, token: secrecy::SecretString }` (defined here; Task 7 adds discovery).

- [ ] **Step 1: Write the failing tests**

`crates/coder-sdk/tests/errors.rs`:

```rust
use coder_sdk::{Client, Error, Session};
use secrecy::SecretString;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client(server: &MockServer) -> Client {
    let session = Session {
        url: server.uri().parse().unwrap(),
        token: SecretString::from("test-token-not-real"),
    };
    Client::new(&session).unwrap()
}

#[tokio::test]
async fn server_version_sends_token_and_parses_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/buildinfo"))
        .and(header("Coder-Session-Token", "test-token-not-real"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "v2.38.0+abc"})))
        .mount(&server)
        .await;
    assert_eq!(client(&server).await.server_version().await.unwrap(), "v2.38.0+abc");
}

#[tokio::test]
async fn unauthorized_maps_to_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v2/buildinfo"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"message": "no"})))
        .mount(&server)
        .await;
    assert!(matches!(client(&server).await.server_version().await, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn generated_call_errors_keep_server_message_and_validations() {
    let server = MockServer::start().await;
    let chat = uuid::Uuid::new_v4();
    Mock::given(path(format!("/api/v2/chats/{chat}")))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "message": "Invalid request.",
            "detail": "bad field",
            "validations": [{"field": "title", "detail": "too long"}]
        })))
        .mount(&server)
        .await;
    let c = client(&server).await;
    let err = match c.api().get_chat_by_id(&chat).await {
        Ok(_) => panic!("expected an error"),
        Err(e) => Error::from_progenitor(e).await,
    };
    match err {
        Error::Api { status, message, detail, validations } => {
            assert_eq!(status, 400);
            assert_eq!(message, "Invalid request.");
            assert_eq!(detail.as_deref(), Some("bad field"));
            assert_eq!(validations[0].field, "title");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn token_never_appears_in_debug_output() {
    let session = Session { url: "https://example.com".parse().unwrap(), token: SecretString::from("test-token-not-real") };
    assert!(!format!("{session:?}").contains("test-token-not-real"));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coder-sdk --test errors`
Expected: FAIL with unresolved imports `coder_sdk::Client`, `coder_sdk::Error`, `coder_sdk::Session`.

- [ ] **Step 3: Implement errors**

`crates/coder-sdk/src/error.rs`:

```rust
use serde::Deserialize;

/// One field-level validation message from the server.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Validation {
    pub field: String,
    pub detail: String,
}

#[derive(Debug, Deserialize, Default)]
struct ApiBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    validations: Vec<Validation>,
}

/// Errors returned by coder-sdk.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the session token was rejected; run `coder login`")]
    Unauthorized,
    #[error("{message}")]
    Api { status: u16, message: String, detail: Option<String>, validations: Vec<Validation> },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("could not decode response: {0}")]
    Decode(String),
    #[error("not logged in: {0}")]
    NotLoggedIn(String),
    #[error("stream closed: {reason}")]
    StreamClosed { code: Option<u16>, reason: String },
    #[error("the session token contains characters that cannot be sent in a header")]
    InvalidToken,
}

/// Convenience alias for coder-sdk results.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Builds an error from an HTTP status and a response body in codersdk.Response shape.
    pub fn from_status(status: u16, body: &[u8]) -> Error {
        if status == 401 {
            return Error::Unauthorized;
        }
        let parsed: ApiBody = serde_json::from_slice(body).unwrap_or_default();
        let message = if parsed.message.is_empty() { format!("HTTP {status}") } else { parsed.message };
        Error::Api { status, message, detail: parsed.detail.filter(|d| !d.is_empty()), validations: parsed.validations }
    }

    /// Converts a generated-client error, reading the response body when it is still available.
    pub async fn from_progenitor<E: serde::Serialize>(err: progenitor_client::Error<E>) -> Error {
        use progenitor_client::Error as P;
        match err {
            P::ErrorResponse(value) => {
                let status = value.status().as_u16();
                let body = serde_json::to_vec(&value.into_inner()).unwrap_or_default();
                Error::from_status(status, &body)
            }
            P::UnexpectedResponse(response) => {
                let status = response.status().as_u16();
                let body = response.bytes().await.unwrap_or_default();
                Error::from_status(status, &body)
            }
            P::InvalidResponsePayload(_, e) => Error::Decode(e.to_string()),
            other => Error::Transport(other.to_string()),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.status().map(|s| s.as_u16()) == Some(401) {
            return Error::Unauthorized;
        }
        Error::Transport(e.to_string())
    }
}
```

If `progenitor_client::Error` in 0.15 names a variant differently, check `cargo doc -p progenitor-client --open` and adjust the match arms; the test in Step 1 is the contract.

- [ ] **Step 4: Implement the client**

`crates/coder-sdk/src/client.rs`:

```rust
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::{Error, Result};

/// A Coder deployment URL and the session token used to call it.
#[derive(Debug, Clone)]
pub struct Session {
    pub url: Url,
    pub token: SecretString,
}

/// An authenticated Coder API client.
#[derive(Clone)]
pub struct Client {
    base: Url,
    http: reqwest::Client,
    api: coder_api_gen::Client,
}

impl Client {
    /// Builds a client that sends the session token on every request, including WebSocket upgrades.
    pub fn new(session: &Session) -> Result<Client> {
        let mut token = HeaderValue::from_str(session.token.expose_secret()).map_err(|_| Error::InvalidToken)?;
        token.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("Coder-Session-Token", token);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(concat!("unofficial-coder-sdk-rs/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let base = session.url.clone();
        let api = coder_api_gen::Client::new_with_client(base.as_str().trim_end_matches('/'), http.clone());
        Ok(Client { base, http, api })
    }

    /// The generated client for any endpoint coder-sdk does not wrap.
    pub fn api(&self) -> &coder_api_gen::Client {
        &self.api
    }

    /// The underlying HTTP client, already carrying the session token.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The deployment URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// The server's version string from `/api/v2/buildinfo`.
    pub async fn server_version(&self) -> Result<String> {
        let url = self.base.join("/api/v2/buildinfo").map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http.get(url).send().await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if status != 200 {
            return Err(Error::from_status(status, &body));
        }
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))?;
        value["version"].as_str().map(str::to_owned).ok_or_else(|| Error::Decode("buildinfo has no version".into()))
    }
}
```

`crates/coder-sdk/src/lib.rs`:

```rust
//! Unofficial hand-written layer over the generated Coder API client.

mod client;
mod error;

pub use client::{Client, Session};
pub use coder_api_gen::types;
pub use error::{Error, Result, Validation};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p coder-sdk --test errors`
Expected: all 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/coder-sdk && git commit -m "feat(coder-sdk): add client construction and typed errors

Assisted-by: AI"
```

---

### Task 7: Session discovery from the `coder` CLI

The `coder` CLI stores its session as follows (verified in coder/coder `cli/sessionstore` and `cli/config` at `d1597a583b`):

- The config directory is `$CODER_CONFIG_DIR` if set, otherwise `~/Library/Application Support/coderv2` on macOS and `$XDG_CONFIG_HOME/coderv2` (default `~/.config/coderv2`) on Linux.
- The deployment URL is in `<config dir>/url`.
- On macOS the token is in the login keychain: service `coder-v2-credentials`, account `coder-login-credentials`, read with `/usr/bin/security find-generic-password -s coder-v2-credentials -wa coder-login-credentials`. The value is base64 of a JSON object mapping a lowercase `host[:port]` to `{"coder_url": ..., "api_token": ...}`.
- Otherwise, or when the keychain has no entry for the host, the token is in `<config dir>/session`.
- `CODER_URL` and `CODER_SESSION_TOKEN` override everything.

**Files:**
- Create: `crates/coder-sdk/src/session.rs`
- Modify: `crates/coder-sdk/src/lib.rs`

**Interfaces:**
- Consumes: `Session`, `Error::NotLoggedIn` from Task 6.
- Produces: `coder_sdk::discover_session() -> Result<Session>`; the testable core `coder_sdk::session::discover_with(env: &dyn SessionEnv) -> Result<Session>`; trait `SessionEnv { fn var(&self, key: &str) -> Option<String>; fn config_dir(&self) -> Option<PathBuf>; fn read_file(&self, path: &Path) -> Option<String>; fn keychain(&self) -> Option<String>; }`; `session::token_from_keychain(blob_b64: &str, url: &Url) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/coder-sdk/src/session.rs` with only the test module, then add the implementation above it in Step 3:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv {
        vars: HashMap<String, String>,
        files: HashMap<PathBuf, String>,
        keychain: Option<String>,
    }

    impl SessionEnv for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
        fn config_dir(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/cfg"))
        }
        fn read_file(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn keychain(&self) -> Option<String> {
            self.keychain.clone()
        }
    }

    fn keychain_blob(host: &str, token: &str) -> String {
        let json = serde_json::json!({host: {"coder_url": format!("https://{host}"), "api_token": token}});
        base64::engine::general_purpose::STANDARD.encode(json.to_string())
    }

    #[test]
    fn env_overrides_everything() {
        let mut env = FakeEnv::default();
        env.vars.insert("CODER_URL".into(), "https://env.example.com".into());
        env.vars.insert("CODER_SESSION_TOKEN".into(), "test-token-env".into());
        env.files.insert("/cfg/url".into(), "https://file.example.com\n".into());
        let s = discover_with(&env).unwrap();
        assert_eq!(s.url.as_str(), "https://env.example.com/");
        assert_eq!(s.token.expose_secret(), "test-token-env");
    }

    #[test]
    fn keychain_token_wins_over_session_file() {
        let mut env = FakeEnv::default();
        env.files.insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files.insert("/cfg/session".into(), "test-token-file".into());
        env.keychain = Some(keychain_blob("dev.coder.com", "test-token-keychain"));
        assert_eq!(discover_with(&env).unwrap().token.expose_secret(), "test-token-keychain");
    }

    #[test]
    fn falls_back_to_session_file_when_keychain_lacks_host() {
        let mut env = FakeEnv::default();
        env.files.insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files.insert("/cfg/session".into(), "test-token-file".into());
        env.keychain = Some(keychain_blob("other.example.com", "test-token-other"));
        assert_eq!(discover_with(&env).unwrap().token.expose_secret(), "test-token-file");
    }

    #[test]
    fn normalizes_host_and_trims_files() {
        let mut env = FakeEnv::default();
        env.files.insert("/cfg/url".into(), "  https://Dev.Coder.com:8443/ \n".into());
        env.keychain = Some(keychain_blob("dev.coder.com:8443", "test-token-port"));
        let s = discover_with(&env).unwrap();
        assert_eq!(s.url.host_str(), Some("dev.coder.com"));
        assert_eq!(s.token.expose_secret(), "test-token-port");

        let mut env = FakeEnv::default();
        env.files.insert("/cfg/url".into(), "https://dev.coder.com\n".into());
        env.files.insert("/cfg/session".into(), "test-token-file\n".into());
        assert_eq!(discover_with(&env).unwrap().token.expose_secret(), "test-token-file");
    }

    #[test]
    fn missing_login_says_run_coder_login() {
        let err = discover_with(&FakeEnv::default()).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn malformed_keychain_blob_is_ignored() {
        let url: Url = "https://dev.coder.com".parse().unwrap();
        assert_eq!(token_from_keychain("not base64!!", &url), None);
    }
}
```

Add `pub mod session;` and `pub use session::discover_session;` to `crates/coder-sdk/src/lib.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coder-sdk session`
Expected: FAIL with `cannot find trait SessionEnv` and `cannot find function discover_with`.

- [ ] **Step 3: Implement discovery**

Insert above the test module in `crates/coder-sdk/src/session.rs`:

```rust
//! Finds the deployment URL and session token the `coder` CLI stored at `coder login`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::{Error, Result, Session};

const KEYCHAIN_SERVICE: &str = "coder-v2-credentials";
const KEYCHAIN_ACCOUNT: &str = "coder-login-credentials";

/// Everything discovery reads from the machine, so tests can supply fakes.
pub trait SessionEnv {
    fn var(&self, key: &str) -> Option<String>;
    fn config_dir(&self) -> Option<PathBuf>;
    fn read_file(&self, path: &Path) -> Option<String>;
    /// The raw base64 value stored by the `coder` CLI in the OS keychain, if any.
    fn keychain(&self) -> Option<String>;
}

struct OsEnv;

impl SessionEnv for OsEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn config_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.var("CODER_CONFIG_DIR") {
            return Some(PathBuf::from(dir));
        }
        let home = PathBuf::from(self.var("HOME")?);
        if cfg!(target_os = "macos") {
            return Some(home.join("Library/Application Support/coderv2"));
        }
        let base = self.var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        Some(base.join("coderv2"))
    }

    fn read_file(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn keychain(&self) -> Option<String> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-wa", KEYCHAIN_ACCOUNT])
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

#[derive(Deserialize)]
struct Credential {
    api_token: String,
}

fn normalized_host(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Extracts the token for `url` from the `coder` CLI's keychain value.
pub fn token_from_keychain(blob_b64: &str, url: &Url) -> Option<String> {
    let json = base64::engine::general_purpose::STANDARD.decode(blob_b64.trim()).ok()?;
    let creds: HashMap<String, Credential> = serde_json::from_slice(&json).ok()?;
    creds.get(&normalized_host(url)?).map(|c| c.api_token.clone()).filter(|t| !t.is_empty())
}

/// Discovery against an injectable environment.
pub fn discover_with(env: &dyn SessionEnv) -> Result<Session> {
    let not_logged_in = || Error::NotLoggedIn("no Coder session found; run `coder login` first".into());
    let config_dir = env.config_dir();
    let url_text = env
        .var("CODER_URL")
        .or_else(|| config_dir.as_ref().and_then(|d| env.read_file(&d.join("url"))))
        .ok_or_else(not_logged_in)?;
    let url: Url = url_text.trim().parse().map_err(|_| Error::NotLoggedIn(format!("invalid Coder URL {:?}", url_text.trim())))?;
    let token = env
        .var("CODER_SESSION_TOKEN")
        .or_else(|| env.keychain().and_then(|blob| token_from_keychain(&blob, &url)))
        .or_else(|| config_dir.as_ref().and_then(|d| env.read_file(&d.join("session"))))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .ok_or_else(not_logged_in)?;
    Ok(Session { url, token: SecretString::from(token) })
}

/// Finds the session the `coder` CLI stored, honoring `CODER_URL` and `CODER_SESSION_TOKEN`.
pub fn discover_session() -> Result<Session> {
    discover_with(&OsEnv)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p coder-sdk session`
Expected: all 6 tests pass.

- [ ] **Step 5: Try it against the real machine**

Run: `cargo test -p coder-sdk --lib -- --ignored real_session` after adding this test to the test module:

```rust
    #[test]
    #[ignore = "reads the developer's real coder CLI session"]
    fn real_session() {
        let s = discover_session().expect("run `coder login` first");
        println!("found session for {}", s.url);
    }
```

Expected: prints the deployment URL the author is logged in to, and never the token. If it fails on macOS with a keychain prompt or error, record the exact message in the plan's execution notes; do not work around it by reading other keychain items.

- [ ] **Step 6: Commit**

```bash
git add crates/coder-sdk && git commit -m "feat(coder-sdk): discover the coder cli session

Assisted-by: AI"
```

---

### Task 8: Open enums, `stream_chat`, and `watch_chats`

**Files:**
- Create: `crates/coder-sdk/src/enums.rs`, `crates/coder-sdk/src/stream.rs`
- Modify: `crates/coder-sdk/src/lib.rs`
- Test: `crates/coder-sdk/tests/stream.rs`

**Interfaces:**
- Consumes: `Client`, `Error`, `coder_api_gen::types::CodersdkChatStreamEvent`, `coder_api_gen::types::CodersdkChatWatchEvent`.
- Produces:
  - `coder_sdk::ChatStatus`, `coder_sdk::StreamEventType`, `coder_sdk::PartType`: open enums, each with `Unknown(String)` and `fn parse(&str) -> Self`.
  - `coder_sdk::StreamEvent { kind: StreamEventType, event: Option<types::CodersdkChatStreamEvent>, raw: serde_json::Value }`; `event` is `None` when the payload failed to decode, so callers can still log `raw`.
  - `coder_sdk::WatchEvent { kind: String, event: Option<types::CodersdkChatWatchEvent>, raw: serde_json::Value }`.
  - `Client::stream_chat(&self, chat: uuid::Uuid, after_id: Option<i64>) -> Result<impl Stream<Item = Result<StreamEvent>>>` (async).
  - `Client::watch_chats(&self) -> Result<impl Stream<Item = Result<WatchEvent>>>` (async).

- [ ] **Step 1: Write the failing tests**

`crates/coder-sdk/tests/stream.rs`:

```rust
use coder_sdk::{Client, Error, Session, StreamEventType};
use futures::{SinkExt, StreamExt};
use secrecy::SecretString;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

/// Starts a one-connection WebSocket server that sends `frames`, then closes with `close`.
async fn serve(frames: Vec<String>, close: Option<CloseFrame>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        for frame in frames {
            ws.send(Message::text(frame)).await.unwrap();
        }
        match close {
            Some(frame) => ws.close(Some(frame)).await.unwrap(),
            None => drop(ws),
        }
    });
    format!("http://{addr}")
}

fn client(url: &str) -> Client {
    Client::new(&Session { url: url.parse().unwrap(), token: SecretString::from("test-token-not-real") }).unwrap()
}

fn normal_close() -> Option<CloseFrame> {
    Some(CloseFrame { code: CloseCode::Normal, reason: "".into() })
}

#[tokio::test]
async fn batched_frame_yields_each_event_in_order() {
    let frame = r#"[{"type":"status","status":{"status":"running"}},{"type":"preview_reset"},{"type":"status","status":{"status":"waiting"}}]"#;
    let url = serve(vec![frame.into()], normal_close()).await;
    let events: Vec<_> = client(&url).stream_chat(uuid::Uuid::new_v4(), None).await.unwrap().collect().await;
    let kinds: Vec<_> = events.into_iter().map(|e| e.unwrap().kind).collect();
    assert_eq!(kinds, vec![StreamEventType::Status, StreamEventType::PreviewReset, StreamEventType::Status]);
}

#[tokio::test]
async fn unknown_event_type_is_yielded_not_fatal() {
    let frame = r#"[{"type":"brand_new_event","whatever":1},{"type":"status","status":{"status":"a_status_from_the_future"}}]"#;
    let url = serve(vec![frame.into()], normal_close()).await;
    let events: Vec<_> = client(&url).stream_chat(uuid::Uuid::new_v4(), None).await.unwrap().collect().await;
    assert_eq!(events.len(), 2);
    let first = events[0].as_ref().unwrap();
    assert_eq!(first.kind, StreamEventType::Unknown("brand_new_event".into()));
    assert_eq!(first.raw["whatever"], 1);
    let second = events[1].as_ref().unwrap();
    assert_eq!(second.kind, StreamEventType::Status);
    assert!(second.event.is_some(), "an unknown status value must still decode");
}

#[tokio::test]
async fn abnormal_close_yields_error_then_ends() {
    let url = serve(vec![r#"[{"type":"preview_reset"}]"#.into()], None).await;
    let events: Vec<_> = client(&url).stream_chat(uuid::Uuid::new_v4(), None).await.unwrap().collect().await;
    assert_eq!(events.len(), 2);
    assert!(events[0].is_ok());
    assert!(matches!(events[1], Err(Error::StreamClosed { .. })));
}

#[tokio::test]
async fn large_batched_frame_yields_all_events_in_order() {
    let big_text = "x".repeat(5_000);
    let events: Vec<String> = (0..256)
        .map(|i| format!(r#"{{"type":"message_part","message_part":{{"role":"assistant","seq":{i},"part":{{"type":"text","text":"{big_text}"}}}}}}"#, i = i + 1))
        .collect();
    let frame = format!("[{}]", events.join(","));
    assert!(frame.len() > 1_000_000);
    let url = serve(vec![frame], normal_close()).await;
    let got: Vec<_> = client(&url).stream_chat(uuid::Uuid::new_v4(), None).await.unwrap().collect().await;
    assert_eq!(got.len(), 256);
    let seqs: Vec<i64> = got.iter().map(|e| e.as_ref().unwrap().raw["message_part"]["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, (1..=256).collect::<Vec<_>>());
}

#[tokio::test]
async fn upgrade_401_maps_to_unauthorized() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let result = client(&server.uri()).stream_chat(uuid::Uuid::new_v4(), None).await;
    assert!(matches!(result, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn watch_yields_one_event_per_frame_with_kind() {
    let url = serve(
        vec![r#"{"kind":"title_change","chat":{"id":"00000000-0000-0000-0000-000000000001","title":"Hi"}}"#.into(),
             r#"{"kind":"something_new"}"#.into()],
        normal_close(),
    )
    .await;
    let got: Vec<_> = client(&url).watch_chats().await.unwrap().collect().await;
    let kinds: Vec<_> = got.into_iter().map(|e| e.unwrap().kind).collect();
    assert_eq!(kinds, vec!["title_change".to_string(), "something_new".to_string()]);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coder-sdk --test stream`
Expected: FAIL with unresolved imports `coder_sdk::StreamEventType` and missing methods `stream_chat` and `watch_chats`.

- [ ] **Step 3: Implement the open enums**

`crates/coder-sdk/src/enums.rs`:

```rust
//! Open enums for the values scuttle matches on. Unknown values from a newer server are preserved.

macro_rules! open_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)+
            /// A value this SDK version does not know yet.
            Unknown(String),
        }

        impl $name {
            pub fn parse(value: &str) -> Self {
                match value {
                    $($text => $name::$variant,)+
                    other => $name::Unknown(other.to_owned()),
                }
            }

            pub fn as_str(&self) -> &str {
                match self {
                    $($name::$variant => $text,)+
                    $name::Unknown(other) => other,
                }
            }
        }
    };
}

open_enum!(
    /// `codersdk.ChatStatus`.
    ChatStatus {
        Waiting => "waiting",
        Running => "running",
        Interrupting => "interrupting",
        RequiresAction => "requires_action",
        Error => "error",
    }
);

open_enum!(
    /// `codersdk.ChatStreamEventType`.
    StreamEventType {
        MessagePart => "message_part",
        Message => "message",
        Status => "status",
        Error => "error",
        QueueUpdate => "queue_update",
        Retry => "retry",
        ActionRequired => "action_required",
        PreviewReset => "preview_reset",
        HistoryReset => "history_reset",
    }
);

open_enum!(
    /// `codersdk.ChatMessagePartType`.
    PartType {
        Text => "text",
        Reasoning => "reasoning",
        ToolCall => "tool-call",
        ToolResult => "tool-result",
        Source => "source",
        File => "file",
        FileReference => "file-reference",
        ContextFile => "context-file",
        Skill => "skill",
        WorkspaceFileReference => "workspace-file-reference",
        HookContext => "hook-context",
        HookNotice => "hook-notice",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_known_and_unknown_values() {
        assert_eq!(ChatStatus::parse("running"), ChatStatus::Running);
        assert_eq!(ChatStatus::parse("paused").as_str(), "paused");
        assert_eq!(PartType::parse("tool-call"), PartType::ToolCall);
    }
}
```

- [ ] **Step 4: Implement the streams**

`crates/coder-sdk/src/stream.rs`:

```rust
//! The chat stream and chat list WebSockets.

use futures::{Stream, StreamExt, stream};
use reqwest_websocket::{Message, RequestBuilderExt};

use crate::types::{CodersdkChatStreamEvent, CodersdkChatWatchEvent};
use crate::{Client, Error, Result, StreamEventType};

/// One event from `/api/v2/chats/{id}/stream`.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub kind: StreamEventType,
    /// The typed event, or `None` if this SDK version could not decode it.
    pub event: Option<CodersdkChatStreamEvent>,
    pub raw: serde_json::Value,
}

/// One event from `/api/v2/chats/watch`.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub kind: String,
    pub event: Option<CodersdkChatWatchEvent>,
    pub raw: serde_json::Value,
}

fn stream_event(raw: serde_json::Value) -> StreamEvent {
    let kind = StreamEventType::parse(raw["type"].as_str().unwrap_or_default());
    let event = match kind {
        StreamEventType::Unknown(_) => None,
        _ => serde_json::from_value(raw.clone()).ok(),
    };
    StreamEvent { kind, event, raw }
}

fn watch_event(raw: serde_json::Value) -> WatchEvent {
    let kind = raw["kind"].as_str().unwrap_or_default().to_owned();
    let event = serde_json::from_value(raw.clone()).ok();
    WatchEvent { kind, event, raw }
}

impl Client {
    async fn open(&self, path_and_query: &str) -> Result<reqwest_websocket::WebSocket> {
        let url = self.base_url().join(path_and_query).map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http().get(url).upgrade().send().await.map_err(|e| Error::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(Error::Unauthorized);
        }
        if status != 101 {
            return Err(Error::from_status(status, b""));
        }
        response.into_websocket().await.map_err(|e| Error::Transport(e.to_string()))
    }

    /// Streams a chat's events. Frames are JSON arrays; each element is yielded separately.
    /// An abnormal close yields one `Error::StreamClosed` and then the stream ends.
    pub async fn stream_chat(
        &self,
        chat: uuid::Uuid,
        after_id: Option<i64>,
    ) -> Result<impl Stream<Item = Result<StreamEvent>> + use<>> {
        let mut path = format!("/api/v2/chats/{chat}/stream");
        if let Some(id) = after_id {
            path.push_str(&format!("?after_id={id}"));
        }
        let socket = self.open(&path).await?;
        Ok(frames(socket).flat_map(|frame| {
            let items: Vec<Result<StreamEvent>> = match frame {
                Ok(text) => match serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                    Ok(values) => values.into_iter().map(|v| Ok(stream_event(v))).collect(),
                    Err(e) => vec![Err(Error::Decode(e.to_string()))],
                },
                Err(e) => vec![Err(e)],
            };
            stream::iter(items)
        }))
    }

    /// Streams chat list changes for the signed-in user, one event per frame.
    pub async fn watch_chats(&self) -> Result<impl Stream<Item = Result<WatchEvent>> + use<>> {
        let socket = self.open("/api/v2/chats/watch").await?;
        Ok(frames(socket).map(|frame| {
            let text = frame?;
            let raw: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Decode(e.to_string()))?;
            Ok(watch_event(raw))
        }))
    }
}

/// Text frames until a normal close. Anything else ends with one `StreamClosed` error.
fn frames(socket: reqwest_websocket::WebSocket) -> impl Stream<Item = Result<String>> {
    stream::unfold(Some(socket), |state| async move {
        let mut socket = state?;
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return Some((Ok(text.to_string()), Some(socket))),
                Some(Ok(Message::Close { code, reason })) => {
                    let code = u16::from(code);
                    if code == 1000 {
                        return None;
                    }
                    return Some((Err(Error::StreamClosed { code: Some(code), reason }), None));
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Some((Err(Error::StreamClosed { code: None, reason: e.to_string() }), None)),
                None => return Some((Err(Error::StreamClosed { code: None, reason: "connection ended without a close frame".into() }), None)),
            }
        }
    })
}
```

The `reqwest-websocket` 0.6 names used above are `RequestBuilderExt::upgrade`, `UpgradeResponse::status`, `UpgradeResponse::into_websocket`, `WebSocket` as a `Stream` of `Result<Message>`, and `Message::Close { code, reason }`. If any of them differ in the resolved version, check `cargo doc -p reqwest-websocket --open` and adapt the code, keeping the tests unchanged.

Add to `crates/coder-sdk/src/lib.rs`:

```rust
mod enums;
mod stream;

pub use enums::{ChatStatus, PartType, StreamEventType};
pub use stream::{StreamEvent, WatchEvent};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p coder-sdk`
Expected: all tests in `enums`, `session`, `errors`, and `stream` pass.

If `unknown_event_type_is_yielded_not_fatal` fails because the second event's `event` is `None`, the generated `CodersdkChatStreamStatus` still has a closed enum: confirm `open-enums` ran by checking `spec/patches.log` for `codersdk.ChatStatus`, and fix the rule rather than the test.

- [ ] **Step 6: Commit**

```bash
git add crates/coder-sdk && git commit -m "feat(coder-sdk): add chat stream and watch websockets with open enums

Assisted-by: AI"
```

---

### Task 9: Smoke tests against a real coderd

**Files:**
- Create: `scripts/smoke.sh`
- Create: `crates/coder-sdk/tests/smoke.rs`

**Interfaces:**
- Consumes: `Client`, `Session`, `stream_chat`, `watch_chats`, `server_version`.
- Produces: `scripts/smoke.sh [image]`, which starts `coderd`, bootstraps an owner, a provider, and a default model, exports `CODER_URL`, `CODER_SESSION_TOKEN`, and `CODER_SMOKE_ORG`, runs `cargo test -p coder-sdk --test smoke -- --ignored`, and removes the container.

- [ ] **Step 1: Write the smoke tests**

`crates/coder-sdk/tests/smoke.rs`:

```rust
//! Run with `scripts/smoke.sh`, which starts a coderd container and sets the environment.

use coder_sdk::{Client, StreamEventType, discover_session};
use futures::StreamExt;
use std::time::Duration;

fn client() -> Client {
    Client::new(&discover_session().expect("CODER_URL and CODER_SESSION_TOKEN")).unwrap()
}

fn org() -> String {
    std::env::var("CODER_SMOKE_ORG").expect("CODER_SMOKE_ORG")
}

async fn create_idle_chat(c: &Client) -> uuid::Uuid {
    let url = c.base_url().join("/api/v2/chats").unwrap();
    let response = c.http().post(url).json(&serde_json::json!({"organization_id": org(), "content": []})).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 201, "{}", response.text().await.unwrap());
    let chat: serde_json::Value = response.json().await.unwrap();
    chat["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn server_version_is_reported() {
    let version = client().server_version().await.unwrap();
    assert!(version.starts_with('v'), "{version}");
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn generated_list_chats_decodes_real_response() {
    let c = client();
    create_idle_chat(&c).await;
    let chats = c.api().list_chats(None, None, None, None, None).await.expect("list_chats");
    assert!(!chats.into_inner().is_empty());
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn stream_snapshot_reports_waiting_status() {
    let c = client();
    let chat = create_idle_chat(&c).await;
    let mut events = c.stream_chat(chat, None).await.unwrap();
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.next().await {
            let event = event.unwrap();
            if event.kind == StreamEventType::Status {
                return event.raw["status"]["status"].as_str().map(str::to_owned);
            }
        }
        None
    })
    .await
    .expect("status event within 10s");
    assert_eq!(found.as_deref(), Some("waiting"));
}

#[tokio::test]
#[ignore = "needs scripts/smoke.sh"]
async fn watch_reports_created_chat() {
    let c = client();
    let mut events = c.watch_chats().await.unwrap();
    let chat = create_idle_chat(&c).await;
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.next().await {
            let event = event.unwrap();
            if event.kind == "created" && event.raw["chat"]["id"].as_str() == Some(&chat.to_string()) {
                return true;
            }
        }
        false
    })
    .await
    .expect("created event within 10s");
    assert!(seen);
}
```

`list_chats` takes one `Option` per query parameter in the generated client; if its parameter count differs, match the signature in `crates/coder-api-gen/src/generated.rs` (search `pub async fn list_chats`) and pass `None` for each.

- [ ] **Step 2: Write the smoke script**

`scripts/smoke.sh`:

```bash
#!/usr/bin/env bash
# Start coderd in Docker, bootstrap an owner and a default chat model, and run the ignored smoke tests.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ref="$(cut -d' ' -f1 < "$root/spec/coder-ref.txt")"
if [[ $# -ge 1 ]]; then
  image="$1"
elif [[ "$ref" == v* ]]; then
  image="ghcr.io/coder/coder:$ref"
else
  image="ghcr.io/coder/coder-preview:latest"
fi
name="coder-sdk-smoke-$$"
port=37123
url="http://127.0.0.1:$port"

cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker run -d --name "$name" -p "$port:3000" \
  -e CODER_HTTP_ADDRESS=0.0.0.0:3000 -e CODER_ACCESS_URL="$url" -e CODER_TELEMETRY_ENABLE=false \
  "$image" server >/dev/null

for _ in $(seq 1 120); do
  curl -fsS "$url/healthz" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "$url/healthz" >/dev/null

email="smoke@example.com"
password="SmokeTest-Only-$$-Password"
curl -fsS -X POST "$url/api/v2/users/first" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$email\",\"username\":\"smoke\",\"password\":\"$password\",\"trial\":false}" >/dev/null
token="$(curl -fsS -X POST "$url/api/v2/users/login" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$email\",\"password\":\"$password\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session_token"])')"
org="$(curl -fsS "$url/api/v2/users/me/organizations" -H "Coder-Session-Token: $token" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"

provider="$(curl -fsS -X POST "$url/api/v2/ai/providers" -H "Coder-Session-Token: $token" -H 'Content-Type: application/json' \
  -d '{"type":"openai-compat","name":"smoke","enabled":true,"base_url":"http://127.0.0.1:9/v1","api_keys":["smoke-key-not-real"]}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
curl -fsS -X POST "$url/api/v2/organizations/$org/chats/models" -H "Coder-Session-Token: $token" -H 'Content-Type: application/json' \
  -d "{\"ai_provider_id\":\"$provider\",\"model\":\"gpt-4o-mini\",\"context_limit\":4096,\"is_default\":true}" >/dev/null

(cd "$root" && CODER_URL="$url" CODER_SESSION_TOKEN="$token" CODER_SMOKE_ORG="$org" \
  cargo test -p coder-sdk --test smoke -- --ignored --test-threads=1)
```

Run: `chmod +x scripts/smoke.sh`

The smoke run never calls the model, because it only creates idle chats; the unreachable provider URL is deliberate.

- [ ] **Step 3: Run the smoke tests**

Run: `colima status || colima start` and then `scripts/smoke.sh`
Expected: 4 smoke tests pass.

If `POST /api/v2/ai/providers` rejects the `openai-compat` type in the image, rerun with `"type":"openai"` in the script. If an endpoint returns `404`, the image predates it; pass a newer image explicitly, for example `scripts/smoke.sh ghcr.io/coder/coder-preview:latest`, and note the image used in the commit message.

- [ ] **Step 4: Commit**

```bash
git add scripts/smoke.sh crates/coder-sdk/tests/smoke.rs && git commit -m "test(coder-sdk): add smoke tests against a real coderd

Assisted-by: AI"
```

---

### Task 10: CI and scheduled regeneration

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/regenerate.yml`

**Interfaces:**
- Consumes: `scripts/regenerate.sh`, `scripts/smoke.sh`, the Go and Python test suites.
- Produces: a CI check on every push and pull request; a regeneration workflow that opens one pull request per new coder/coder release or manual ref.

- [ ] **Step 1: Write the CI workflow**

`.github/workflows/ci.yml`:

```yaml
name: ci
on:
  push:
    branches: [main]
  pull_request:
permissions: {}
jobs:
  test:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-go@v5
        with:
          go-version: "1.26"
      - run: rustup show
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --exclude coder-api-gen --all-targets -- -D warnings
      - run: cargo test --workspace
      - run: cd tools/rawfields && go test ./...
      - run: cd tools && python3 -m unittest test_patch_spec -v
```

- [ ] **Step 2: Write the regeneration workflow**

`.github/workflows/regenerate.yml`:

```yaml
name: regenerate
on:
  workflow_dispatch:
    inputs:
      ref:
        description: coder/coder tag, branch, or commit
        required: true
  schedule:
    - cron: "17 6 * * *"
permissions: {}
jobs:
  regenerate:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-go@v5
        with:
          go-version: "1.26"
      - uses: actions/setup-node@v4
        with:
          node-version: "22"
      - run: rustup show
      - name: Pick the ref
        id: ref
        env:
          GH_TOKEN: ${{ github.token }}
          INPUT_REF: ${{ inputs.ref }}
        run: |
          if [ -n "$INPUT_REF" ]; then
            echo "ref=$INPUT_REF" >> "$GITHUB_OUTPUT"
          else
            latest="$(gh api repos/coder/coder/releases/latest --jq .tag_name)"
            current="$(cut -d' ' -f1 < spec/coder-ref.txt)"
            if [ "$latest" = "$current" ]; then
              echo "Already at $latest"
              echo "ref=" >> "$GITHUB_OUTPUT"
            else
              echo "ref=$latest" >> "$GITHUB_OUTPUT"
            fi
          fi
      - name: Regenerate
        if: steps.ref.outputs.ref != ''
        env:
          REF: ${{ steps.ref.outputs.ref }}
        run: |
          cut -f1 spec/patches.log | sort | uniq -c > /tmp/rules-before.txt
          scripts/regenerate.sh "$REF"
          cut -f1 spec/patches.log | sort | uniq -c > /tmp/rules-after.txt
          cargo test --workspace
          scripts/smoke.sh
      - name: Open a pull request
        if: steps.ref.outputs.ref != ''
        env:
          GH_TOKEN: ${{ github.token }}
          REF: ${{ steps.ref.outputs.ref }}
        run: |
          if git diff --quiet; then
            echo "No changes"
            exit 0
          fi
          branch="regenerate-$(echo "$REF" | tr '/.' '--')"
          git config user.name "github-actions[bot]"
          git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
          git switch -c "$branch"
          git add -A
          git commit -m "feat: regenerate coder-api-gen from coder/coder $REF" -m "Assisted-by: AI"
          git push -u origin "$branch"
          {
            echo "## Summary"
            echo
            echo "Regenerates coder-api-gen from coder/coder \`$(cat spec/coder-ref.txt)\`."
            echo
            echo "## Patch rules"
            echo
            echo "Before:"
            echo '```text'
            cat /tmp/rules-before.txt
            echo '```'
            echo "After:"
            echo '```text'
            cat /tmp/rules-after.txt
            echo '```'
            if ! diff -q /tmp/rules-before.txt /tmp/rules-after.txt >/dev/null; then
              echo
              echo "**The set or count of patch rules changed. Review spec/patches.log.**"
            fi
            echo
            echo "---"
            echo "🤖 Built with AI assistance."
          } > /tmp/body.md
          gh pr create --title "feat: regenerate coder-api-gen from coder/coder $REF" --body-file /tmp/body.md
```

- [ ] **Step 3: Lint the workflows**

Run: `docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest -color`
Expected: no findings. Colima only shares the home directory, which contains this repo, so the volume mount works.

- [ ] **Step 4: Run the CI steps locally**

Run: `cargo fmt --all --check && cargo clippy --workspace --exclude coder-api-gen --all-targets -- -D warnings && cargo test --workspace && (cd tools/rawfields && go test ./...) && (cd tools && python3 -m unittest test_patch_spec)`
Expected: every command succeeds.

- [ ] **Step 5: Commit**

```bash
git add .github && git commit -m "ci: add checks and scheduled regeneration

Assisted-by: AI"
```

---

## Handoff notes for the author

- The repo is local only. Creating `nickvigilante/unofficial-coder-sdk-rs` on GitHub and pushing is the author's call; the regeneration workflow needs "Allow GitHub Actions to create pull requests" enabled in the repo settings.
- The `open-enums` rule is the one deliberate deviation from the spec (see the top of this plan).
- The spec's open question 1 (session storage) is resolved by Task 7; update the spec's open questions list when this plan is done.
- Deferred from spec section 7: the wrapper that maps `GET .../templateversions/{name}/previous` returning `204` to `None`. scuttle never calls that endpoint, so it waits until something does.
- Deferred to scuttle M1: comparing `server_version()` with `spec/coder-ref.txt` to show the version-skew warning. The SDK only provides both values.
