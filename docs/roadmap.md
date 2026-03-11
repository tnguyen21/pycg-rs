# Roadmap

## Intent

This roadmap assumes `pycg-rs` is primarily a CLI and is being built as a
static-analysis primitive inside broader LLM-augmented engineering workflows.

The project should not optimize for feature breadth by default. It should
optimize for a narrow claim:

- Faster than stale alternatives.
- Better accuracy and coverage on realistic Python code.
- More useful as a machine-consumable code-understanding tool.

## Current Phase

`pycg-rs` is past the "prove the core idea" stage and approaching maintenance
mode.

It has:

- A working analyzer with 41 accuracy cases (86 expectations across 18
  categories) and 3 corpus smoke tests against real-world repos.
- Multiple output formats (DOT, TGF, text, JSON) with versioned JSON schemas
  for all 7 subcommands (`docs/json-schema/`).
- 6 focused query commands: `callees`, `callers`, `neighbors`, `path`,
  `summary`, `symbols-in`.
- CI running lint, format, tests, and corpus tests (with corpora cloned).
- A multi-tool comparison harness (`benchmarks/compare.py`) benchmarking
  against jarviscg, pyan3, code2flow, and PyCG original.
- A published report site with corpus analysis results and module dependency
  graphs.
- Honest limitations documentation (`docs/limitations.md`).

The right mode is:

- Maintenance for generic call-graph functionality.
- Active development for workflow-critical capabilities.

## Priority Order

## 1. Accuracy and Coverage

This is the most important investment because it supports the project's core
claim and directly affects downstream trust.

Done:

- 41 accuracy cases with 86 expectations (81 presence, 5 absence) across 18
  categories: aliasing, async, branch_join, builtins, chained_calls, closures,
  containers, decorators, definitions, destructuring, higher_order, imports,
  inheritance, multi_return, precision, protocols, returns, unknowns.
- Reproducible comparison harness (`benchmarks/compare.py`) against jarviscg
  (78%), pyan3 (52%), and PyCG (crashes on Python 3.12+). pycg-rs scores
  100% on all 86 expectations.
- Corpus smoke tests on requests, rich, flask with semantic edge assertions.
- `docs/limitations.md` documents strengths, partial areas, and weak/unsupported
  patterns honestly.

Remaining:

- Expand expected-absence fixtures (currently 5 of 86; more would strengthen
  precision claims).
- Add accuracy cases for partial/weak areas (framework decorators, attribute-heavy
  OO, dynamic dispatch) to track improvement over time.
- Track regressions via scheduled `compare.py` runs.

## 2. Performance on Real Workloads

Performance matters because this tool is meant to sit inside repeated analysis
loops.

Done:

- `benchmarks/bench.py` measures 9 real-world corpora against 4 competitor
  tools. pycg-rs is 3-17x faster across all benchmarks.
- Analysis completes in 18-228ms on representative packages.

Remaining:

- Profile-driven optimization on hot paths.
- Partial/incremental analysis for repeated invocation workflows.
- Perf regression gate in CI (no baseline tracked yet).

## 3. Query-Oriented CLI Design

Whole-graph export is useful, but it is not the highest-leverage interface for
agentic systems.

Done:

- 6 focused query commands: `callees`, `callers`, `neighbors`, `path`,
  `summary`, `symbols-in`.
- `--match suffix` for fuzzy symbol lookup.
- JSON and text output for all queries.

Remaining:

- Symbol/module/path filter flags on `analyze` (e.g. "edges touching this
  module only").
- Impact-analysis and revision-diff workflows.
- Structural summaries for routing downstream reasoning.

## 4. JSON Contract and Provenance

If other tools depend on `pycg-rs`, the machine-readable interface becomes the
real API.

Done:

- Versioned JSON schemas for all 7 subcommands in `docs/json-schema/`.
- `schema_version`, `tool`, `analysis`, `diagnostics`, `stats` fields in output.
- Provenance fields (tool version, analyzed files, graph mode) included
  consistently.

Remaining:

- Documented deterministic ordering guarantees.
- Explicit metadata for skipped or uncertain cases in query results.
- Versioning policy documentation.

## 5. Testing and Trust Signals

Testing should evolve from "is the graph non-degenerate?" toward "does this
analysis support the claims we want to make?"

Done:

- Corpus tests use semantic edge assertions (not just count thresholds).
- Corpus tests marked `#[ignore]` (visible, not silently passing) and run in CI
  with real repos cloned.
- 3 regression tests (issues #2, #3, #5) verify structural defines edges.
- 205+ tests total.

Remaining:

- CLI snapshot tests for stable output behavior.
- More regression cases for tricky patterns (metaclasses, `__init_subclass__`,
  complex decorator stacks).
- Expand corpus test suite beyond requests/rich/flask.

## 6. Maintenance and Refactoring

Refactoring remains important, but it should support correctness and velocity,
not become its own project.

Recommended work:

- Split oversized modules only when they are actively slowing development.
- Keep internal boundaries crisp where they support testing and profiling.
- Avoid workspace/crate decomposition unless a boundary is genuinely becoming
  public or independently reusable.

## Things Not Worth Heavy Investment Right Now

- Large amounts of visual presentation work.
- Many new export formats.
- Broad library API design.
- Packaging/distribution work beyond practical CLI use.
- Premature architectural decomposition of small modules.

## Suggested Allocation

For the next development tranche, a reasonable effort split is:

- 40% accuracy and coverage work.
- 30% query-oriented CLI work and JSON contract quality.
- 20% performance work driven by measurement.
- 10% maintenance, refactors, and documentation.

## Exit Criteria for Maintenance Mode

The project can move into a truer maintenance mode once these conditions are
mostly satisfied:

- Accuracy claims are backed by reproducible evidence.
- Performance is consistently good on the repositories that matter.
- The CLI exposes a small, stable set of query workflows.
- JSON output is stable enough for downstream tool integration.
- Known limitations are documented and surfaced honestly.

At that point, new work can become narrower and more selective:

- Fix regressions.
- Extend coverage for real encountered cases.
- Improve performance where it materially affects workflows.
- Add only those features that directly improve the primitive.
