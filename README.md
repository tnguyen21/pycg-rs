# pyan-rs

A Rust reimplementation of [pyan3](https://github.com/Technologicat/pyan) — a static call graph generator for Python programs.

Parses Python source files and produces a directed graph of defines/uses relationships between modules, classes, functions, and methods. No Python runtime required — uses [ruff's parser](https://github.com/astral-sh/ruff) for AST parsing.

## Usage

```bash
# Analyze Python files, output DOT format (default)
pyan-rs src/**/*.py

# Output as plain text dependency list
pyan-rs mypackage/ --format text

# Show both defines and uses edges, colored by file
pyan-rs mypackage/ -d -u --colored --grouped

# Pipe DOT to graphviz for SVG
pyan-rs mypackage/ | dot -Tsvg -o callgraph.svg
```

### Options

```
pyan-rs [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Python source files or directories to analyze

Options:
  -d, --defines          Draw defines edges
  -u, --uses             Draw uses edges
  -c, --colored          Color nodes by file
  -g, --grouped          Group nodes by namespace
  -a, --annotated        Annotate nodes with file:line info
  -r, --root <ROOT>      Root directory for module name resolution
      --format <FORMAT>  Output format: dot, tgf, text [default: dot]
      --rankdir <DIR>    GraphViz rank direction [default: TB]
  -v, --verbose          Enable verbose logging (-vv for debug)
```

If neither `--defines` nor `--uses` is specified, uses edges are shown by default.

## Output formats

- **dot** — GraphViz DOT format, suitable for rendering with `dot`, `neato`, etc.
- **tgf** — Trivial Graph Format
- **text** — Plain text dependency list with `[D]`/`[U]` tags

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
- For-loop bindings, comprehensions, lambdas
- Match statement patterns (Python 3.10+)
- Type alias statements (Python 3.12+)

## Building

```bash
cargo build --release
```

## Testing

```bash
cargo test
```

35 tests: 18 unit tests covering graph construction, coloring, and output formatting; 17 integration tests using Python fixture files covering core analysis, decorators, inheritance, output formats, and regression cases.

## Performance

~7x faster than the Python pyan3 on real-world codebases. On the pyan3 source tree (10 files, ~4200 LOC):

| | Median |
|---|---|
| Python pyan3 | 220ms |
| pyan-rs | 31ms (wall clock incl. process startup) |
| pyan-rs (library, no startup) | ~10ms |

## Differences from pyan3

This is an MVP port. Not yet implemented:

- PyO3 Python bindings
- SVG/HTML output (pipe DOT to graphviz instead)
- yEd GraphML output
- Module-level dependency analysis mode
- Sphinx documentation plugin
- Context manager protocol edges (`__enter__`/`__exit__`)
- Iterator protocol edges (`__iter__`/`__next__`)
- `super()` resolution
- `del` statement protocol edges

## License

GPL-2.0-or-later (same as pyan3)
