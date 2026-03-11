# pycg-rs

[![CI](https://github.com/tnguyen21/pycg-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/tnguyen21/pycg-rs/actions/workflows/ci.yml)
[![Analysis Report](https://github.com/tnguyen21/pycg-rs/actions/workflows/report.yml/badge.svg)](https://github.com/tnguyen21/pycg-rs/actions/workflows/report.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fast static Python call graphs in Rust.

Parses Python source files and produces a directed graph of defines/uses relationships between modules, classes, functions, and methods. No Python runtime required — uses [ruff's parser](https://github.com/astral-sh/ruff) for AST parsing.

- CLI for DOT, TGF, text, and JSON output
- Module-level and symbol-level graph modes
- Real-world corpus smoke testing and a published GitHub Pages report

GitHub Pages report: <https://tnguyen21.github.io/pycg-rs/>

## Installation

```bash
cargo install --git https://github.com/tnguyen21/pycg-rs --bin pycg
```

For local development from a checkout:

```bash
cargo install --path . --force
```

This installs the CLI as `pycg`, typically at `~/.cargo/bin/pycg`.

If `pycg` is not on your `PATH`, add this to your shell config:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

## Quickstart

```bash
# Analyze a package and print a plain-text dependency list
pycg mypackage/ --format text

# Emit machine-readable JSON
pycg mypackage/ --format json > graph.json

# Render an SVG with GraphViz
pycg mypackage/ --defines --uses --grouped --annotated mypackage/ | dot -Tsvg -o callgraph.svg
```

## Usage

```bash
# Analyze Python files, output DOT format (default)
pycg src/**/*.py

# Output as plain text dependency list
pycg mypackage/ --format text

# Analyze a package and keep module names rooted at the repo src dir
pycg mypackage/ --root .

# Show both defines and uses edges, colored by file
pycg mypackage/ -d -u --colored --grouped

# Pipe DOT to graphviz for SVG
pycg mypackage/ | dot -Tsvg -o callgraph.svg
```

### Options

```
pycg [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Python source files or directories to analyze

Options:
  -d, --defines          Draw defines edges
  -u, --uses             Draw uses edges
  -m, --modules          Show module-level import dependencies instead of
                         symbol-level call graph
  -c, --colored          Color nodes by file
  -g, --grouped          Group nodes by namespace
  -a, --annotated        Annotate nodes with file:line info
  -r, --root <ROOT>      Root directory for module name resolution
      --format <FORMAT>  Output format: dot, tgf, text, json [default: dot]
      --rankdir <DIR>    GraphViz rank direction [default: TB]
  -v, --verbose          Enable verbose logging (-vv for debug)
```

If neither `--defines` nor `--uses` is specified, uses edges are shown by default.

### Typical workflows

```bash
# Inspect call dependencies in a package
pycg src/

# Show only defines edges
pycg src/ --defines

# Render a grouped, annotated SVG
pycg src/ --root . --defines --uses --grouped --annotated | dot -Tsvg -o callgraph.svg

# Module-level import dependency graph
pycg src/ --modules | dot -Tsvg -o imports.svg

# Debug analyzer decisions
pycg src/ -vv
```

## Output formats

- **dot** — GraphViz DOT format, suitable for rendering with `dot`, `neato`, etc.
- **tgf** — Trivial Graph Format
- **text** — Plain text dependency list with `[D]`/`[U]` tags
- **json** — Machine-readable graph output with nodes, edges, stats, and diagnostics for unresolved, external, ambiguous, and approximated analysis results

Example JSON workflow:

```bash
pycg mypackage/ --format json > graph.json
jq '.stats' graph.json
```

The machine-readable contract is documented in
[`docs/json-contract.md`](docs/json-contract.md).
The corresponding JSON Schema lives at
[`docs/json-schema/pycg-graph-v1.schema.json`](docs/json-schema/pycg-graph-v1.schema.json).

## How it works

1. Walks the given files/directories collecting `.py` files
2. Parses each file into an AST using `ruff_python_parser`
3. Runs two-pass analysis:
   - **Pass 1**: Collect all definitions (modules, classes, functions), track name bindings, imports, and attribute access
   - **Between passes**: Resolve base classes, compute MRO (C3 linearization)
   - **Pass 2**: Re-analyze with full inheritance info to resolve forward references
4. Postprocessing: expand wildcard references, resolve imports, contract undefined nodes, cull inherited edges, collapse inner scopes
5. Build visual graph and write to the selected output format

### What gets tracked

- Module, class, function/method definitions and nesting
- Function calls (including class instantiation → `__init__`)
- Attribute access through `self` and resolved objects
- Imports (absolute and relative, with aliases)
- Decorator analysis (`@staticmethod`, `@classmethod`, `@property`)
- Inheritance with MRO-aware attribute lookup
- Assignments, augmented assignments, annotated assignments
- Return-value propagation across calls, including multi-return cases
- Tuple/list destructuring and shallow list/dict subscript flow for statically-known literals
- For-loop bindings, comprehensions, lambdas
- Context-manager, iterator, `str`, `repr`, `del obj.attr`, and `del obj[key]` protocol edges
- Match statement patterns (Python 3.10+)
- Type alias statements (Python 3.12+)

## Building

```bash
cargo build --release
```

## Testing

```bash
cargo test --all-targets
```

Semantic accuracy fixtures are tracked separately from the broader integration
suite. To run the declarative fixture report against the CLI:

```bash
python3 scripts/accuracy_report.py --pycg ./target/release/pycg
```

Corpus-scale smoke tests run the full analysis pipeline over vendored real-world packages (`requests`, `flask`, `rich`) and assert non-degenerate graph statistics. They skip automatically when the corpora are absent (e.g. a fresh clone), so `cargo test --all-targets` stays green without them.

To clone the corpora (and the `pyan`/`PyCG` reference repos) locally:

```bash
./scripts/bootstrap-corpora.sh
```

This is idempotent and safe to re-run. After cloning, `cargo test` will include the corpus smoke tests.

## Performance

Wall-clock comparison against [code2flow](https://github.com/scottrogowski/code2flow) (v2.5.1) on real-world codebases. Both tools produce DOT output; timings include process startup. Measured on Apple M4 Pro, 5 runs after 1 warmup, interleaved tool order per round.

| Corpus | Files | pycg | code2flow | Speedup |
|--------|------:|-----:|----------:|--------:|
| requests | 18 | 14ms | 74ms | 5.4x |
| flask | 24 | 20ms | 94ms | 4.8x |
| black | 25 | 33ms | 161ms | 4.9x |
| httpx | 23 | 23ms | 111ms | 4.8x |
| rich | 78 | 63ms | 434ms | 6.9x |

These tools are not equivalent — pycg performs deeper static analysis (MRO resolution, return-value propagation, protocol edges) while code2flow does lightweight control-flow extraction. The comparison is wall-clock only and says nothing about output quality.

To reproduce:

```bash
./benchmarks/setup.sh
python3 benchmarks/bench.py --pycg ./target/release/pycg
```

The benchmark harness bootstraps the same corpora used by the smoke tests. Results are intended as trend data, not as a semantic correctness claim.

## Current limitations

- This is still narrower than `pyan3` in some dynamic-language corner cases.
- Corpus runs are smoke tests, not full semantic validation against a gold standard.
- Benchmark numbers are wall-clock comparisons and should be treated as directional.

## License

MIT
