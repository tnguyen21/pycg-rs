# Query Contracts

## Purpose

This document sketches the planned query-oriented CLI surface for `pycg-rs`.

The existing graph JSON contract in [`json-contract.md`](json-contract.md)
describes whole-graph export. This document covers a different concern:

- narrow, machine-readable queries over analyzed code
- stable result shapes for CLI subcommands
- query-resolution semantics that agents and scripts can rely on

The goal is to make `pycg-rs` useful as a queryable analysis primitive, not
just a graph dumper.

Planned JSON Schemas for the initial query surface live in
[`docs/json-schema/`](json-schema/).

## Scope

This document is intentionally pre-implementation. It defines direction and
defaults, not a frozen command set.

It does not try to answer:

- caching or indexing design
- revision-to-revision diff format
- every possible future query

It does answer:

- what kinds of queries we want
- how query matching should work
- what error and ambiguity behavior should be
- how query outputs should relate to the graph contract

## High-Level Direction

The CLI should evolve toward a small set of focused queries that answer common
code-understanding questions cheaply:

- what symbols exist here?
- what does this symbol call?
- who calls this symbol?
- what is adjacent to this symbol?
- what path connects these two symbols?
- what modules or symbols should I inspect next?

The first tranche should stay small and stable. A good initial surface is:

- `analyze`
- `symbols-in`
- `callees`
- `callers`
- `neighbors`
- `path`
- `summary`

## Core Design Rules

### Separate Whole-Graph and Query Contracts

The existing whole-graph JSON contract should remain its own thing.

Query commands should return query-specific result shapes rather than forcing
every result into the same top-level graph document.

That means:

- `analyze --format json` returns a graph document
- `callees --format json` returns a query result document
- `path --format json` returns a path result document

This keeps the machine surface clean and avoids one oversized schema trying to
model everything.

### Use `canonical_name` as the Public Query Key

Queries should primarily target `canonical_name`.

Reasoning:

- it is stable and human-readable
- it matches the public graph contract
- it avoids making callers depend on opaque node IDs

Opaque `id` values are for joins within one emitted graph document, not for
human or agent-facing query input.

### Keep Diagnostics in Query Results

Query outputs should still include diagnostics where they matter.

If a caller asks for `callees foo.bar.baz`, the response should be able to say:

- query resolution was exact
- query resolution was ambiguous
- analysis for this result included unresolved or approximated edges

This matters for refactoring and routing decisions.

## Decisions

This section answers the main open questions directly.

### Exact Match Only or Suffix Match Too?

Default: exact match only.

Recommendation:

- exact `canonical_name` match should be the default and the contract path
- suffix matching, fuzzy matching, or prefix matching should only happen when
  explicitly requested with a flag such as `--match suffix`

Reasoning:

- exact matching is deterministic and script-safe
- agents should not guess unless the caller asked for that behavior

Planned match modes:

- `exact`

Possible later modes:

- suffix matching is useful for interactive use, but dangerous as a default
- `suffix`
- `prefix`
- `glob`

But those should not be part of the first query tranche unless they prove
necessary.

### Symbol Graph Only, or Module Graph Too?

Default: symbol graph first, module graph where it is naturally meaningful.

Recommendation:

- first implement query commands against the symbol graph
- allow selected commands to support `--modules` when the query naturally makes
  sense at the module level

Practical split:

- `symbols-in`: symbol-first, can also support `--modules`
- `summary`: symbol and module modes both make sense
- `callers`, `callees`, `neighbors`, `path`: symbol-first

Reasoning:

- symbol-level results are more useful for code navigation and refactors
- module-level graph queries are useful, but lower leverage at the start
- forcing every query to support both modes immediately adds complexity without
  clear payoff

### What Happens on 0 Matches or Multiple Matches?

Recommendation:

- `0` matches: non-zero exit, structured error payload in JSON mode
- `1` match: success
- `>1` matches under exact mode: non-zero exit, structured ambiguity payload
- `>1` matches under suffix mode: non-zero exit by default unless a future flag
  explicitly asks for all matches

This should be strict.

Reasoning:

- silent fallback is dangerous for automation
- query resolution must be deterministic by default
- if a query string is ambiguous, the caller should choose more precisely

Suggested JSON error shape:

```json
{
  "schema_version": "1",
  "query_kind": "callees",
  "status": "error",
  "error": {
    "code": "symbol_not_found",
    "message": "No symbol matched query",
    "query": {
      "value": "foo.bar",
      "match_mode": "exact"
    }
  }
}
```

Suggested JSON ambiguity shape:

```json
{
  "schema_version": "1",
  "query_kind": "callees",
  "status": "error",
  "error": {
    "code": "ambiguous_query",
    "message": "Query matched multiple symbols",
    "query": {
      "value": "make",
      "match_mode": "suffix"
    },
    "matches": ["pkg.a.make", "pkg.b.make"]
  }
}
```

Later, if there is demand, a flag like `--allow-multiple` could return a list
of per-match results. That should not be the default.

### Should Ambiguous Query Resolution Itself Be Surfaced in Diagnostics?

Yes, but only for successful query results where ambiguity remains relevant.

Recommendation:

- failed query resolution should be represented as top-level `error`, not only
  as a diagnostic
- successful query results may include diagnostics describing ambiguity or
  approximation inside the returned result set

Reasoning:

- if the query target itself cannot be resolved uniquely, that is not merely a
  warning; the query failed
- if the target resolved successfully but the returned graph neighborhood
  contains ambiguous edges or approximations, diagnostics are the right place

So the split should be:

- query-resolution ambiguity: top-level error
- analysis ambiguity inside the resolved result: diagnostics

## Proposed Query Set

### `analyze`

Purpose:

- whole-graph export
- the existing graph contract

Notes:

- already exists in spirit through the current CLI
- may remain the default command for raw graph output

### `symbols-in <path-or-module>`

Purpose:

- list the symbols defined in a file, directory, or module

Why it matters:

- cheap inventory step for both humans and agents
- useful first routing primitive before deeper graph traversal

Suggested result shape:

```json
{
  "schema_version": "1",
  "query_kind": "symbols_in",
  "status": "ok",
  "query": {
    "target": "pkg/subpkg/mod.py",
    "target_kind": "path"
  },
  "symbols": [
    {
      "canonical_name": "pkg.subpkg.mod.Helper",
      "kind": "class",
      "location": {
        "path": "pkg/subpkg/mod.py",
        "line": 10
      }
    }
  ],
  "diagnostics": {
    "summary": {
      "warnings": 0
    },
    "warnings": []
  }
}
```

### `summary <path-or-module>`

Purpose:

- return a compact summary of a file, module, or package

Likely contents:

- symbol counts by kind
- top-level symbols
- maybe inbound/outbound edge counts
- maybe diagnostic summary

Why it matters:

- cheap orientation primitive
- useful for deciding what to inspect next

### `callees <canonical-name>`

Purpose:

- return outgoing neighbors from one symbol

Suggested default:

- direct outgoing `uses` neighbors only

Optional later flags:

- `--defines`
- `--depth N`
- `--include-self`

Suggested result shape:

```json
{
  "schema_version": "1",
  "query_kind": "callees",
  "status": "ok",
  "query": {
    "symbol": "pkg.mod.fn",
    "match_mode": "exact"
  },
  "node": {
    "canonical_name": "pkg.mod.fn",
    "kind": "function",
    "location": {
      "path": "pkg/mod.py",
      "line": 42
    }
  },
  "edges": [
    {
      "kind": "uses",
      "target": {
        "canonical_name": "pkg.other.helper",
        "kind": "function",
        "location": {
          "path": "pkg/other.py",
          "line": 10
        }
      }
    }
  ],
  "diagnostics": {
    "summary": {
      "warnings": 0,
      "ambiguous_resolutions": 1,
      "unresolved_references": 0,
      "external_references": 0,
      "approximations": 1
    }
  }
}
```

### `callers <canonical-name>`

Purpose:

- return incoming `uses` neighbors for one symbol

Why it matters:

- impact analysis
- refactor planning
- test targeting

This is one of the highest-value commands after `callees`.

### `neighbors <canonical-name>`

Purpose:

- return both incoming and outgoing neighbors in one call

Why it matters:

- cheap local graph context
- simpler than calling `callers` and `callees` separately in some workflows

This may be a better first query than implementing both `callers` and
`callees` separately if implementation effort is high.

### `path <src> <dst>`

Purpose:

- find one or more graph paths between two symbols

Why it matters:

- dependency tracing
- why-is-this-connected exploration
- impact and explanation workflows

Notes:

- probably start with shortest path
- probably cap path count and path length
- diagnostics matter here because missing edges can produce false negatives

## Query Result Conventions

All query result documents should share a few conventions.

### Common top-level fields

- `schema_version`
- `query_kind`
- `status`
- `query`

When `status == "ok"`, the document contains query-specific result fields.

When `status == "error"`, the document contains:

- `error.code`
- `error.message`

### Determinism

Query results should be deterministic:

- stable ordering of matches
- stable ordering of returned nodes/edges
- stable diagnostics ordering

This matters for snapshots, agents, and cached downstream processing.

### Output Modes

Human-readable output can exist, but JSON should be treated as the primary
query contract.

For the first tranche, a reasonable rule is:

- human-readable text for interactive use
- JSON for stable downstream use

## Implementation Order

Recommended order:

1. `symbols-in`
2. `callees`
3. `callers` or `neighbors`
4. `summary`
5. `path`

Reasoning:

- `symbols-in` gives a cheap inventory primitive
- `callees` is highly valuable and naturally aligned with current graph data
- `callers` and `neighbors` are straightforward once symbol lookup is solid
- `path` is useful, but depends on stable symbol resolution and traversal rules

## Non-Goals for the First Query Tranche

- fuzzy query languages
- complex multi-symbol batch queries
- revision diff results
- full explanation/provenance trees
- incremental index storage design

Those may come later, but they should not block the first useful query
surface.
