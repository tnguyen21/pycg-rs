# JSON Contract

## Purpose

This document defines the intended machine-readable JSON contract for
`pycg-rs`.

The JSON output is treated as a product surface for downstream tooling,
including LLM-augmented code-understanding and refactoring workflows. The goal
is not simply to dump internal analyzer state. The goal is to provide:

- A stable graph representation for machine consumers.
- Explicit symbolic identity and source location metadata.
- Deterministic ordering and field semantics.
- Structured diagnostics about uncertainty and analysis limits.

This document describes the target contract before implementation is finalized.

The corresponding JSON Schema lives at
[`docs/json-schema/pycg-graph-v1.schema.json`](json-schema/pycg-graph-v1.schema.json).

## Design Goals

The contract should be:

- Stable enough to version and depend on.
- Deterministic enough for snapshots, caching, and automated consumers.
- Concrete enough to answer graph queries without reconstructing analyzer
  internals.
- Honest enough to expose uncertainty that matters for refactors and larger
  structural changes.

## Non-Goals

The contract is not intended to:

- Expose every internal analyzer node or temporary intermediate state.
- Preserve exact in-memory representation details.
- Encode all future query results in a single schema.

The base graph and the diagnostics surface should remain clean and focused.

## Top-Level Shape

The intended top-level structure is:

```json
{
  "schema_version": "1",
  "tool": {
    "name": "pycg-rs",
    "version": "0.1.0",
    "commit": "36969ec"
  },
  "graph_mode": "symbol",
  "analysis": {
    "root": "tests",
    "inputs": [
      "tests/test_code/accuracy_factory.py"
    ],
    "node_inclusion_policy": "defined_only",
    "path_kind": "root_relative"
  },
  "stats": {
    "nodes": 5,
    "edges": 8,
    "files_analyzed": 1,
    "by_node_kind": {
      "module": 1,
      "class": 1,
      "function": 2,
      "method": 1
    },
    "by_edge_kind": {
      "defines": 4,
      "uses": 4
    }
  },
  "nodes": [],
  "edges": [],
  "diagnostics": {
    "summary": {
      "warnings": 0,
      "unresolved_references": 0,
      "ambiguous_resolutions": 0,
      "external_references": 0,
      "approximations": 0
    },
    "warnings": [],
    "unresolved_references": [],
    "ambiguous_resolutions": [],
    "external_references": [],
    "approximations": []
  }
}
```

## Field Semantics

### `schema_version`

String identifying the JSON contract version.

Rules:

- Required.
- Must change when a backward-incompatible schema change is introduced.
- Should remain stable across pure analyzer-quality improvements that do not
  change the contract shape or semantics.

## `tool`

Structured information about the producer.

Fields:

- `name`: tool name, expected to be `pycg-rs`
- `version`: project version
- `commit`: optional VCS revision when available

This is producer metadata, not analysis metadata.

## `graph_mode`

Identifies the graph entity mode.

Initial values:

- `symbol`
- `module`

Meaning:

- `symbol`: nodes represent modules, classes, functions, and methods
- `module`: nodes represent modules/packages only

Consumers should not infer graph semantics solely from node shapes. They should
read `graph_mode`.

## `analysis`

Describes how the graph was produced.

Initial fields:

- `root`: analysis root used for module-name resolution, when present
- `inputs`: ordered list of input files/directories supplied to the CLI
- `node_inclusion_policy`: how nodes were selected for emission
- `path_kind`: how `location.path` values are encoded

Initial expected values:

- `node_inclusion_policy`: `defined_only`
- `path_kind`: `root_relative`, `input_relative`, or `absolute`

This section describes contract semantics that matter to consumers. It should
not become a dump of every internal analysis knob.

## `stats`

Derived summary information for dashboards, reports, and quick inspection.

Initial fields:

- `nodes`
- `edges`
- `files_analyzed`
- `by_node_kind`
- `by_edge_kind`

Rules:

- `stats` is convenience metadata, not the authoritative graph.
- Consumers must be able to function without trusting `stats`.
- Counts must be consistent with emitted `nodes` and `edges`.

## `nodes`

Array of emitted graph nodes.

Each node should have the shape:

```json
{
  "id": "n5",
  "kind": "method",
  "canonical_name": "tests.test_code.accuracy_factory.Product.make",
  "name": "make",
  "namespace": "tests.test_code.accuracy_factory.Product",
  "location": {
    "path": "tests/test_code/accuracy_factory.py",
    "line": 16
  }
}
```

### Node fields

#### `id`

Stable machine-facing identifier within the emitted graph.

Rules:

- Required.
- Must be unique within a document.
- Must be the field used by edges to reference nodes.
- Does not need to equal `canonical_name`.
- May be opaque.

Rationale:

- This leaves room for future naming changes without breaking edge identity.
- It also leaves room for non-symbolic or synthetic nodes if they are ever
  included later.

#### `kind`

Public node kind.

Initial values:

- `module`
- `class`
- `function`
- `method`
- `static_method`
- `class_method`

This is external vocabulary. It should not drift casually with internal enum
renames.

#### `canonical_name`

The canonical symbolic path for the node, when such a path exists.

Examples:

- `tests.test_code.accuracy_factory`
- `tests.test_code.accuracy_factory.consumer`
- `tests.test_code.accuracy_factory.Product.make`

Rules:

- Intended for symbolic queries and human-readable downstream references.
- Must be deterministic.
- Should be unique among emitted concrete symbolic nodes.
- May evolve more slowly than internal analyzer naming rules.

#### `name`

Short local name of the entity.

Examples:

- `accuracy_factory`
- `consumer`
- `make`

#### `namespace`

Parent symbolic namespace, when present.

Examples:

- `tests.test_code`
- `tests.test_code.accuracy_factory`
- `tests.test_code.accuracy_factory.Product`

The invariant should be:

- if `namespace` is present, `canonical_name` should be reconstructible as
  `namespace + "." + name`
- otherwise `canonical_name == name`

#### `location`

Source location metadata.

Initial fields:

- `path`
- `line`

Future-compatible additions may include:

- `column`
- `end_line`
- `end_column`

Rules:

- `path` semantics must match `analysis.path_kind`
- location should be omitted only when unavailable

## `edges`

Array of concrete graph edges.

Each edge should have the shape:

```json
{
  "kind": "uses",
  "source": "n3",
  "target": "n4"
}
```

### Edge fields

#### `kind`

Initial values:

- `uses`
- `defines`

#### `source`

Node ID for the source node.

#### `target`

Node ID for the target node.

Rules:

- `source` and `target` must refer to emitted nodes in `nodes`
- edge ordering must be deterministic

## `diagnostics`

Structured uncertainty and analysis-quality metadata.

This section exists because the graph alone is not enough for safe downstream
refactoring and orientation workflows. Consumers need to know where analysis
was incomplete, widened, ambiguous, or external.

The diagnostics section is not the primary graph. It is a structured companion
to the graph.

Initial shape:

```json
{
  "summary": {
    "warnings": 0,
    "unresolved_references": 0,
    "ambiguous_resolutions": 0,
    "external_references": 0,
    "approximations": 0
  },
  "warnings": [],
  "unresolved_references": [],
  "ambiguous_resolutions": [],
  "external_references": [],
  "approximations": []
}
```

### `warnings`

Structured noteworthy events that do not naturally fit the other categories.

Proposed shape:

```json
{
  "code": "unsupported_dynamic_import",
  "message": "Could not statically resolve importlib.import_module(name)",
  "path": "pkg/loader.py",
  "line": 42
}
```

Guidelines:

- `code` should be stable enough for automation
- `message` is primarily for humans

### `unresolved_references`

References observed during analysis that could not be resolved to a concrete
emitted node.

Proposed shape:

```json
{
  "kind": "call",
  "source": "n17",
  "symbol": "do_work",
  "path": "pkg/tasks.py",
  "line": 18
}
```

### `ambiguous_resolutions`

References for which multiple targets remained plausible.

Proposed shape:

```json
{
  "kind": "call",
  "source": "n22",
  "symbol": "method",
  "candidate_targets": ["n30", "n31"],
  "path": "pkg/flow.py",
  "line": 51
}
```

### `external_references`

References to symbols outside the analyzed input set.

Proposed shape:

```json
{
  "kind": "import",
  "source": "n4",
  "canonical_name": "requests.sessions.Session",
  "path": "pkg/client.py",
  "line": 3
}
```

### `approximations`

Cases where the analyzer intentionally widened or approximated.

Proposed shape:

```json
{
  "kind": "wildcard_expansion",
  "source": "n12",
  "symbol": "do_work",
  "reason": "expanded_from_unknown_receiver",
  "candidate_targets": ["n40", "n41"]
}
```

## Emission Policy

The main graph should stay focused on concrete, emitted nodes and edges.

Current intended policy:

- emit concrete graph nodes according to `node_inclusion_policy`
- emit concrete edges between emitted nodes
- report uncertainty and omissions in `diagnostics`

This keeps the graph traversal surface clean while still surfacing analysis
confidence information for higher-level tooling.

## Determinism Requirements

The contract should guarantee deterministic output ordering.

At minimum:

- top-level object keys should be stable
- `nodes` ordering should be deterministic
- `edges` ordering should be deterministic
- diagnostic arrays should be deterministic

Determinism matters for:

- snapshot testing
- cache keys
- diffing
- repeated LLM tool calls

## Compatibility Rules

When evolving the schema:

- additive fields should preserve `schema_version`
- incompatible renames or semantic meaning changes should increment
  `schema_version`
- internal analyzer changes should not casually alter public field semantics

## Open Questions

These are intentionally left open for implementation work:

- exact `id` generation strategy
- exact path normalization policy across all invocation modes
- whether location should include columns in v1
- which diagnostics can be populated immediately versus added gradually

Those can evolve during implementation without changing the overall shape of
the contract described here.
