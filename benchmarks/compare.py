#!/usr/bin/env python3
"""Compare pycg-rs accuracy against competitor call-graph tools.

Runs each available tool on the same fixture files, checks expectations
from accuracy_cases.json, and produces a comparative scorecard.

For tools that share PyCG's naming convention (pycg-rs, PyCG original,
jarviscg), we also do direct edge-set comparison on real corpora.

Usage:
    python3 benchmarks/compare.py
    python3 benchmarks/compare.py --tools pycg-rs,pycg
    python3 benchmarks/compare.py --corpus-compare         # also compare edges on corpora
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
VENV_BIN = REPO_ROOT / "benchmarks" / ".venv" / "bin"
DEFAULT_MANIFEST = REPO_ROOT / "tests" / "fixtures" / "accuracy_cases.json"

SOURCE_HINTS: dict[str, str] = {
    "requests": "src/requests",
    "flask": "src/flask",
    "black": "src/black",
    "httpx": "httpx",
    "rich": "rich",
}


# ── tool adapters ─────────────────────────────────────────────────────────


@dataclass
class ToolResult:
    """Normalized output from a call-graph tool."""

    success: bool
    edges: set[tuple[str, str]] = field(default_factory=set)
    node_count: int = 0
    edge_count: int = 0
    error: str | None = None
    raw: Any = None


class ToolAdapter:
    name: str
    caveats: str = ""

    def is_available(self) -> bool:
        raise NotImplementedError

    def run(self, files: list[str], root: str | None = None) -> ToolResult:
        raise NotImplementedError

    def run_on_package(self, source_dir: Path, package_name: str) -> ToolResult:
        """Run on a full package directory. Default: glob *.py and call run()."""
        py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
        return self.run(py_files, root=str(source_dir.parent))


class PycgRsAdapter(ToolAdapter):
    name = "pycg-rs"
    caveats = ""

    def __init__(self, binary: str | None = None):
        self.binary = binary or str(REPO_ROOT / "target" / "release" / "pycg")

    def is_available(self) -> bool:
        return Path(self.binary).is_file()

    def run(self, files: list[str], root: str | None = None) -> ToolResult:
        cmd = [self.binary, "analyze", *files, "--format", "json"]
        if root:
            cmd.extend(["--root", root])
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=120, cwd=REPO_ROOT)
        except subprocess.TimeoutExpired:
            return ToolResult(success=False, error="timeout")

        if result.returncode != 0:
            return ToolResult(success=False, error=result.stderr[:300])

        try:
            data = json.loads(result.stdout)
        except json.JSONDecodeError as e:
            return ToolResult(success=False, error=f"bad JSON: {e}")

        return self._parse_json(data)

    def _parse_json(self, data: dict) -> ToolResult:
        node_names = {n["id"]: n["canonical_name"] for n in data.get("nodes", [])}
        edges = set()
        for edge in data.get("edges", []):
            if edge["kind"] != "uses":
                continue
            src = node_names.get(edge["source"])
            tgt = node_names.get(edge["target"])
            if src and tgt:
                edges.add((src, tgt))
        return ToolResult(
            success=True,
            edges=edges,
            node_count=len(node_names),
            edge_count=len(edges),
            raw=data,
        )


class PycgOriginalAdapter(ToolAdapter):
    """Original PyCG (vitsalis/PyCG). Archived, unmaintained since 2021."""

    name = "pycg"
    caveats = "Archived since 2021. Crashes on Python 3.12+ (importlib changes)."

    def __init__(self):
        self._python = str(VENV_BIN / "python3")

    def is_available(self) -> bool:
        """Check if the pycg module is importable."""
        try:
            r = subprocess.run(
                [self._python, "-c", "from pycg import pycg; print('ok')"],
                capture_output=True,
                text=True,
                timeout=10,
            )
            return r.returncode == 0
        except Exception:
            return False

    def _base_cmd(self) -> list[str]:
        return [self._python, "-m", "pycg"]

    def run(self, files: list[str], root: str | None = None) -> ToolResult:
        cmd = self._base_cmd()
        if root:
            cmd.extend(["--package", Path(root).name])
        cmd.extend(files)

        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=120, cwd=REPO_ROOT)
        except subprocess.TimeoutExpired:
            return ToolResult(success=False, error="timeout")

        if result.returncode != 0:
            return ToolResult(success=False, error=result.stderr[:300])

        try:
            data = json.loads(result.stdout)
        except json.JSONDecodeError as e:
            return ToolResult(success=False, error=f"bad JSON: {e}")

        return self._parse_json(data)

    def _parse_json(self, data: dict) -> ToolResult:
        """PyCG output: {caller_fqn: [callee_fqn, ...], ...}"""
        edges = set()
        for caller, callees in data.items():
            for callee in callees:
                edges.add((caller, callee))
        # Count unique nodes
        all_nodes = set()
        for caller, callees in data.items():
            all_nodes.add(caller)
            all_nodes.update(callees)
        return ToolResult(
            success=True,
            edges=edges,
            node_count=len(all_nodes),
            edge_count=len(edges),
            raw=data,
        )

    def run_on_package(self, source_dir: Path, package_name: str) -> ToolResult:
        py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
        cmd = [*self._base_cmd(), "--package", package_name, *py_files]
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=300, cwd=REPO_ROOT)
        except subprocess.TimeoutExpired:
            return ToolResult(success=False, error="timeout (300s)")

        if result.returncode != 0:
            return ToolResult(success=False, error=result.stderr[:300])

        try:
            data = json.loads(result.stdout)
        except json.JSONDecodeError as e:
            return ToolResult(success=False, error=f"bad JSON: {e}")

        return self._parse_json(data)


class JarvisCGAdapter(ToolAdapter):
    """JARVIS/jarviscg — improved PyCG fork. Claims higher precision + recall."""

    name = "jarviscg"
    caveats = "Active fork of PyCG with flow-sensitive analysis (ICSE 2025)."

    def __init__(self):
        self.binary = str(VENV_BIN / "jarviscg")

    def is_available(self) -> bool:
        return Path(self.binary).is_file()

    def run(self, files: list[str], root: str | None = None) -> ToolResult:
        with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as f:
            out_path = f.name
        try:
            cmd = [self.binary]
            if root:
                cmd.extend(["--package", Path(root).name])
            cmd.extend(["-o", out_path, *files])

            result = subprocess.run(cmd, capture_output=True, text=True, timeout=120, cwd=REPO_ROOT)
            if result.returncode != 0:
                return ToolResult(success=False, error=result.stderr[:300])

            data = json.loads(Path(out_path).read_text())
        except subprocess.TimeoutExpired:
            return ToolResult(success=False, error="timeout")
        except (json.JSONDecodeError, FileNotFoundError) as e:
            return ToolResult(success=False, error=str(e))
        finally:
            try:
                os.unlink(out_path)
            except OSError:
                pass

        # jarviscg uses same output format as PyCG
        edges = set()
        all_nodes = set()
        for caller, callees in data.items():
            all_nodes.add(caller)
            for callee in callees:
                all_nodes.add(callee)
                edges.add((caller, callee))
        return ToolResult(
            success=True,
            edges=edges,
            node_count=len(all_nodes),
            edge_count=len(edges),
            raw=data,
        )

    def run_on_package(self, source_dir: Path, package_name: str) -> ToolResult:
        with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as f:
            out_path = f.name
        try:
            py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
            cmd = [self.binary, "--package", package_name, "-o", out_path, *py_files]
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=300, cwd=REPO_ROOT)
            if result.returncode != 0:
                return ToolResult(success=False, error=result.stderr[:300])
            data = json.loads(Path(out_path).read_text())
        except subprocess.TimeoutExpired:
            return ToolResult(success=False, error="timeout (300s)")
        except (json.JSONDecodeError, FileNotFoundError) as e:
            return ToolResult(success=False, error=str(e))
        finally:
            try:
                os.unlink(out_path)
            except OSError:
                pass

        edges = set()
        all_nodes = set()
        for caller, callees in data.items():
            all_nodes.add(caller)
            for callee in callees:
                all_nodes.add(callee)
                edges.add((caller, callee))
        return ToolResult(
            success=True,
            edges=edges,
            node_count=len(all_nodes),
            edge_count=len(edges),
            raw=data,
        )


class Pyan3Adapter(ToolAdapter):
    """pyan3 — AST + symtable, two-pass. Shallow but fast."""

    name = "pyan3"
    caveats = "No cross-module type inference. Different naming convention."

    def __init__(self):
        self.binary = str(VENV_BIN / "pyan3")

    def is_available(self) -> bool:
        return Path(self.binary).is_file()

    def run(self, files: list[str], root: str | None = None) -> ToolResult:
        cmd = [self.binary, *files, "--uses", "--no-defines", "--tgf"]
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=120, cwd=REPO_ROOT)
        except subprocess.TimeoutExpired:
            return ToolResult(success=False, error="timeout")

        if result.returncode != 0:
            return ToolResult(success=False, error=result.stderr[:300])

        return self._parse_tgf(result.stdout)

    def _parse_tgf(self, tgf: str) -> ToolResult:
        """Parse Trivial Graph Format: nodes, then #, then edges."""
        lines = tgf.strip().split("\n")
        nodes: dict[str, str] = {}
        edges: set[tuple[str, str]] = set()
        past_separator = False

        for line in lines:
            line = line.strip()
            if not line:
                continue
            if line == "#":
                past_separator = True
                continue
            if not past_separator:
                parts = line.split(None, 1)
                if len(parts) >= 2:
                    nodes[parts[0]] = parts[1]
                elif len(parts) == 1:
                    nodes[parts[0]] = parts[0]
            else:
                parts = line.split(None, 2)
                if len(parts) >= 2:
                    src_name = nodes.get(parts[0], parts[0])
                    tgt_name = nodes.get(parts[1], parts[1])
                    edges.add((src_name, tgt_name))

        return ToolResult(
            success=True,
            edges=edges,
            node_count=len(nodes),
            edge_count=len(edges),
        )


ALL_ADAPTERS: list[ToolAdapter] = [
    PycgRsAdapter(),
    PycgOriginalAdapter(),
    JarvisCGAdapter(),
    Pyan3Adapter(),
]


# ── expectation checking ──────────────────────────────────────────────────


def match_name(actual: str, expected: str, matcher: str) -> bool:
    """Check if an actual FQN matches an expected name using the given strategy."""
    if matcher in {"short", "concrete_short"}:
        return actual.rsplit(".", 1)[-1] == expected
    if matcher in {"full", "concrete_full"}:
        return actual == expected
    raise ValueError(f"unknown matcher: {matcher}")


def check_expectation(edges: set[tuple[str, str]], nodes_by_name: dict, expectation: dict) -> dict:
    """Check a single expectation against a set of edges.

    For pycg-rs, nodes_by_name maps canonical_name -> node_id from JSON.
    For other tools, edges are already (fqn, fqn) tuples.
    """
    source_match = expectation.get("source_match", "short")
    target_match = expectation.get("target_match", "short")
    source_pat = expectation["source"]
    target_pat = expectation["target"]

    matching_targets = set()
    for src, tgt in edges:
        if match_name(src, source_pat, source_match) and match_name(tgt, target_pat, target_match):
            matching_targets.add(tgt)

    matched_count = len(matching_targets)
    required = expectation.get("min_matches", 1)
    present = expectation["present"]
    passed = matched_count >= required if present else matched_count == 0

    return {
        "passed": passed,
        "matched_count": matched_count,
        "required": required,
        "expected_present": present,
    }


def evaluate_fixture(
    adapter: ToolAdapter,
    case: dict,
) -> dict:
    """Run a tool on a fixture case and check all expectations."""
    files = [str((REPO_ROOT / f).resolve()) for f in case["files"]]
    root = str((REPO_ROOT / case["root"]).resolve()) if case.get("root") else None

    result = adapter.run(files, root)

    if not result.success:
        return {
            "id": case["id"],
            "category": case["category"],
            "success": False,
            "error": result.error,
            "passed": 0,
            "total": len(case["expectations"]),
            "failures": [],
        }

    passed = 0
    failures = []
    for exp in case["expectations"]:
        # Filter to the right edge kind for pycg-rs (already filtered to "uses" in adapter)
        # For other tools, all edges are call edges
        check = check_expectation(result.edges, {}, exp)
        if check["passed"]:
            passed += 1
        else:
            failures.append(
                {
                    "source": exp["source"],
                    "target": exp["target"],
                    "expected": "present" if exp["present"] else "absent",
                    "matched": check["matched_count"],
                }
            )

    return {
        "id": case["id"],
        "category": case["category"],
        "success": True,
        "passed": passed,
        "total": len(case["expectations"]),
        "failures": failures,
    }


# ── corpus edge comparison ────────────────────────────────────────────────


def normalize_edges(edges: set[tuple[str, str]], package_name: str) -> set[tuple[str, str]]:
    """Normalize edge names to package-relative form.

    jarviscg uses filesystem-absolute names like
        benchmarks.corpora.requests.src.requests.adapters
    while pycg-rs uses package-relative names like
        requests.adapters

    We normalize by finding and stripping the prefix before the package name.
    We also drop edges involving builtins/externals (e.g. <builtin>.*, logging.*).
    """
    normalized = set()
    for src, tgt in edges:
        src = _strip_to_package(src, package_name)
        tgt = _strip_to_package(tgt, package_name)
        # Skip edges involving external/builtin references
        if src.startswith("<") or tgt.startswith("<"):
            continue
        if not src.startswith(package_name) or not tgt.startswith(package_name):
            continue
        normalized.add((src, tgt))
    return normalized


def _strip_to_package(name: str, package_name: str) -> str:
    """Strip filesystem prefix to get package-relative name.

    Handles cases like:
      benchmarks.corpora.requests.src.requests.adapters → requests.adapters
    by finding the *last* occurrence of the package name as a dotted component.
    """
    # Find the last occurrence of package_name as a path component
    marker = f".{package_name}."
    idx = name.rfind(marker)
    if idx >= 0:
        return name[idx + 1 :]
    # Check if it starts with package_name
    marker_start = f"{package_name}."
    if name.startswith(marker_start) or name == package_name:
        return name
    return name


def compare_edge_sets(
    name_a: str,
    edges_a: set[tuple[str, str]],
    name_b: str,
    edges_b: set[tuple[str, str]],
    package_name: str = "",
) -> dict:
    """Compare two edge sets and return overlap statistics.

    If package_name is given, normalizes both edge sets to package-relative
    names and filters to internal-only edges before comparison.
    """
    if package_name:
        edges_a = normalize_edges(edges_a, package_name)
        edges_b = normalize_edges(edges_b, package_name)

    common = edges_a & edges_b
    only_a = edges_a - edges_b
    only_b = edges_b - edges_a

    return {
        "tools": [name_a, name_b],
        f"{name_a}_total": len(edges_a),
        f"{name_b}_total": len(edges_b),
        "common": len(common),
        f"only_{name_a}": len(only_a),
        f"only_{name_b}": len(only_b),
        "jaccard": round(len(common) / len(edges_a | edges_b), 4) if edges_a | edges_b else 0,
    }


# ── output formatting ────────────────────────────────────────────────────


def print_fixture_scorecard(tool_results: dict[str, list[dict]]) -> None:
    """Print a comparative scorecard for fixture accuracy."""
    tool_names = list(tool_results.keys())

    print()
    print("=" * 72)
    print("  ACCURACY SCORECARD — Fixture Expectations")
    print("=" * 72)

    # Per-tool summary
    header = f"  {'Tool':<14s} │ {'Pass':>5s} │ {'Total':>5s} │ {'Rate':>6s} │ {'Errors':>6s} │ Caveats"
    print(header)
    print("  " + "─" * 68)

    for name in tool_names:
        cases = tool_results[name]
        total_pass = sum(c["passed"] for c in cases)
        total_exp = sum(c["total"] for c in cases)
        errors = sum(1 for c in cases if not c["success"])
        rate = f"{total_pass / total_exp * 100:.0f}%" if total_exp > 0 else "—"

        adapter = next((a for a in ALL_ADAPTERS if a.name == name), None)
        caveat = (adapter.caveats[:30] + "...") if adapter and len(adapter.caveats) > 30 else (adapter.caveats if adapter else "")

        print(f"  {name:<14s} │ {total_pass:>5d} │ {total_exp:>5d} │ {rate:>6s} │ {errors:>6d} │ {caveat}")

    # Per-category breakdown
    print()
    categories: dict[str, dict[str, dict]] = {}
    for name in tool_names:
        for case in tool_results[name]:
            cat = case["category"]
            if cat not in categories:
                categories[cat] = {}
            if name not in categories[cat]:
                categories[cat][name] = {"passed": 0, "total": 0}
            categories[cat][name]["passed"] += case["passed"]
            categories[cat][name]["total"] += case["total"]

    print(f"  {'Category':<20s}", end="")
    for name in tool_names:
        print(f" │ {name:>12s}", end="")
    print()
    print("  " + "─" * (22 + 15 * len(tool_names)))

    for cat in sorted(categories):
        print(f"  {cat:<20s}", end="")
        for name in tool_names:
            data = categories[cat].get(name, {"passed": 0, "total": 0})
            if data["total"] > 0:
                pct = data["passed"] / data["total"] * 100
                cell = f"{data['passed']}/{data['total']} ({pct:.0f}%)"
            else:
                cell = "—"
            print(f" │ {cell:>12s}", end="")
        print()

    # Failed expectations detail
    print()
    for name in tool_names:
        failures = [c for c in tool_results[name] if c["failures"] or not c["success"]]
        if not failures:
            continue
        print(f"  Failures for {name}:")
        for case in failures:
            if not case["success"]:
                print(f"    {case['id']}: ERROR — {case.get('error', '?')[:60]}")
            else:
                for f in case["failures"]:
                    print(f"    {case['id']}: {f['source']} → {f['target']} expected {f['expected']}, matched {f['matched']}")
        print()


def print_corpus_comparison(comparisons: list[dict]) -> None:
    """Print corpus-level edge set comparison."""
    if not comparisons:
        return

    print()
    print("=" * 72)
    print("  EDGE SET COMPARISON — Real-World Corpora")
    print("=" * 72)

    for comp in comparisons:
        pair = comp["tools"]
        print(f"\n  {comp['corpus']}  ({pair[0]} vs {pair[1]})")
        print(f"    {pair[0]:>12s} edges: {comp[f'{pair[0]}_total']:>6d}")
        print(f"    {pair[1]:>12s} edges: {comp[f'{pair[1]}_total']:>6d}")
        print(f"    {'Common':>12s}:       {comp['common']:>6d}")
        print(f"    {'Only ' + pair[0]:>12s}:       {comp[f'only_{pair[0]}']:>6d}")
        print(f"    {'Only ' + pair[1]:>12s}:       {comp[f'only_{pair[1]}']:>6d}")
        print(f"    {'Jaccard':>12s}:       {comp['jaccard']:.3f}")


# ── main ──────────────────────────────────────────────────────────────────


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pycg-rs", default=None, help="Path to pycg-rs binary")
    parser.add_argument("--manifest", default=str(DEFAULT_MANIFEST), help="Fixture manifest path")
    parser.add_argument("--tools", default=None, help="Comma-separated tool list (default: all available)")
    parser.add_argument("--corpus-compare", action="store_true", help="Also compare edge sets on real corpora")
    parser.add_argument("--corpora", default="benchmarks/corpora", help="Path to corpora directory")
    parser.add_argument("--only-corpora", default=None, help="Comma-separated corpus list for edge comparison")
    parser.add_argument("--out", default=None, help="Write JSON results to file")
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    # Resolve adapters
    adapters = list(ALL_ADAPTERS)
    if args.pycg_rs:
        adapters[0] = PycgRsAdapter(binary=args.pycg_rs)

    if args.tools:
        requested = {t.strip() for t in args.tools.split(",")}
        adapters = [a for a in adapters if a.name in requested]

    available = []
    for adapter in adapters:
        if adapter.is_available():
            available.append(adapter)
            print(f"  [ok]   {adapter.name}", file=sys.stderr)
        else:
            print(f"  [skip] {adapter.name} (not found)", file=sys.stderr)

    if not available:
        print("No tools available. Run benchmarks/setup.sh first.", file=sys.stderr)
        return 1

    # ── fixture accuracy ──────────────────────────────────────────────────

    manifest = json.loads(Path(args.manifest).read_text())
    cases = manifest["cases"]

    print(f"\nEvaluating {len(cases)} fixture cases across {len(available)} tools...", file=sys.stderr)

    tool_results: dict[str, list[dict]] = {}
    for adapter in available:
        print(f"  Running {adapter.name}...", file=sys.stderr, end="", flush=True)
        results = []
        for case in cases:
            result = evaluate_fixture(adapter, case)
            results.append(result)
        tool_results[adapter.name] = results

        total_pass = sum(r["passed"] for r in results)
        total_exp = sum(r["total"] for r in results)
        errors = sum(1 for r in results if not r["success"])
        err_str = f" ({errors} errors)" if errors else ""
        print(f" {total_pass}/{total_exp}{err_str}", file=sys.stderr)

    print_fixture_scorecard(tool_results)

    # ── corpus edge comparison ────────────────────────────────────────────

    corpus_comparisons: list[dict] = []

    if args.corpus_compare:
        corpora_root = Path(args.corpora)
        corpus_names = list(SOURCE_HINTS.keys())
        if args.only_corpora:
            requested = {c.strip() for c in args.only_corpora.split(",")}
            corpus_names = [c for c in corpus_names if c in requested]

        # Only compare tools with compatible naming (PyCG-family)
        pycg_family = [a for a in available if a.name in {"pycg-rs", "pycg", "jarviscg"}]
        if len(pycg_family) >= 2:
            baseline = pycg_family[0]  # pycg-rs
            others = pycg_family[1:]

            print(f"\nComparing edge sets on corpora ({baseline.name} vs {', '.join(o.name for o in others)})...", file=sys.stderr)

            for corpus_name in corpus_names:
                hint = SOURCE_HINTS.get(corpus_name, corpus_name)
                source_dir = corpora_root / corpus_name / hint
                if not source_dir.is_dir():
                    print(f"  [skip] {corpus_name}: not found", file=sys.stderr)
                    continue

                print(f"  {corpus_name}...", file=sys.stderr, end="", flush=True)

                baseline_result = baseline.run_on_package(source_dir, corpus_name)
                if not baseline_result.success:
                    print(f" {baseline.name} failed", file=sys.stderr)
                    continue

                for other in others:
                    other_result = other.run_on_package(source_dir, corpus_name)
                    if not other_result.success:
                        print(f" {other.name} failed", file=sys.stderr, end="")
                        continue

                    comp = compare_edge_sets(
                        baseline.name,
                        baseline_result.edges,
                        other.name,
                        other_result.edges,
                        package_name=corpus_name,
                    )
                    comp["corpus"] = corpus_name
                    corpus_comparisons.append(comp)

                print(" done", file=sys.stderr)

        print_corpus_comparison(corpus_comparisons)

    # ── JSON output ───────────────────────────────────────────────────────

    output = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "tools": {a.name: {"available": True, "caveats": a.caveats} for a in available},
        "fixture_results": {
            name: {
                "total_expectations": sum(r["total"] for r in results),
                "passed_expectations": sum(r["passed"] for r in results),
                "total_cases": len(results),
                "passed_cases": sum(1 for r in results if r["passed"] == r["total"] and r["success"]),
                "errors": sum(1 for r in results if not r["success"]),
                "cases": results,
            }
            for name, results in tool_results.items()
        },
        "corpus_comparisons": corpus_comparisons,
    }

    if args.out:
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(output, indent=2) + "\n")
        print(f"\nWrote {out_path}")

    # Exit non-zero if pycg-rs has failures
    pycg_rs_results = tool_results.get("pycg-rs", [])
    if pycg_rs_results:
        all_passed = all(r["passed"] == r["total"] and r["success"] for r in pycg_rs_results)
        return 0 if all_passed else 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
