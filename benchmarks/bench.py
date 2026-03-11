#!/usr/bin/env python3
"""Benchmark pycg-rs against competitor tools on real-world corpora.

Measures wall-clock time for each tool on vendored Python packages.
Tools that are not installed are silently skipped.

    python3 benchmarks/bench.py
    python3 benchmarks/bench.py --tools pycg-rs,pycg --corpora requests,flask
"""

from __future__ import annotations

import argparse
import json
import shutil
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENV_BIN = REPO_ROOT / "benchmarks" / ".venv" / "bin"

# corpus name -> subdirectory containing the Python package source
SOURCE_HINTS: dict[str, str] = {
    "requests": "src/requests",
    "flask": "src/flask",
    "black": "src/black",
    "httpx": "httpx",
    "rich": "rich",
    "click": "src/click",
    "pytest": "src",
    "pydantic": "pydantic",
    "fastapi": "fastapi",
}


# ── tool definitions ──────────────────────────────────────────────────────


@dataclass
class Tool:
    name: str
    binary: str | None = None
    build_cmd: list[str] = field(default_factory=list)

    # Subclasses override this
    def command(self, source_dir: Path, corpus_name: str) -> list[str]:
        raise NotImplementedError

    def resolve_binary(self) -> str | None:
        if self.binary:
            # Try exact path first, then PATH lookup
            p = Path(self.binary)
            if p.is_file():
                return str(p.resolve())
            found = shutil.which(self.binary)
            if found:
                return found
        # Try in venv
        venv_path = VENV_BIN / self.name
        if venv_path.is_file():
            return str(venv_path)
        return shutil.which(self.name)

    def is_available(self) -> bool:
        return self.resolve_binary() is not None


class PycgRs(Tool):
    def __init__(self, binary: str | None = None):
        super().__init__(
            name="pycg-rs",
            binary=binary or str(REPO_ROOT / "target" / "release" / "pycg"),
        )

    def command(self, source_dir: Path, corpus_name: str) -> list[str]:
        return [self.resolve_binary(), "analyze", str(source_dir)]


class PycgOriginal(Tool):
    """Original PyCG (Python, archived). Invoked via python -m pycg."""

    def __init__(self):
        super().__init__(name="pycg")
        self._python = str(VENV_BIN / "python3")

    def is_available(self) -> bool:
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

    def resolve_binary(self) -> str | None:
        return self._python if self.is_available() else None

    def command(self, source_dir: Path, corpus_name: str) -> list[str]:
        py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
        return [self._python, "-m", "pycg", "--package", corpus_name, *py_files]


class Pyan3(Tool):
    def __init__(self):
        super().__init__(name="pyan3", binary=str(VENV_BIN / "pyan3"))

    def command(self, source_dir: Path, corpus_name: str) -> list[str]:
        py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
        return [self.resolve_binary(), *py_files, "--uses", "--no-defines", "--dot"]


class Code2Flow(Tool):
    def __init__(self):
        super().__init__(name="code2flow", binary=str(VENV_BIN / "code2flow"))

    def command(self, source_dir: Path, corpus_name: str) -> list[str]:
        py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
        return [self.resolve_binary(), *py_files, "-o", "/tmp/code2flow_bench.dot", "-q"]


class JarvisCG(Tool):
    def __init__(self):
        super().__init__(name="jarviscg", binary=str(VENV_BIN / "jarviscg"))

    def command(self, source_dir: Path, corpus_name: str) -> list[str]:
        py_files = sorted(str(f) for f in source_dir.rglob("*.py"))
        return [
            self.resolve_binary(),
            "--package",
            corpus_name,
            "-o",
            "/tmp/jarviscg_bench.json",
            *py_files,
        ]


ALL_TOOLS = [PycgRs(), PycgOriginal(), Pyan3(), Code2Flow(), JarvisCG()]


# ── benchmark logic ──────────────────────────────────────────────────────


def find_source_dir(corpus_root: Path, corpus_name: str) -> Path:
    hint = SOURCE_HINTS.get(corpus_name, corpus_name)
    candidate = corpus_root / corpus_name / hint
    if not candidate.is_dir():
        raise FileNotFoundError(f"source dir not found: {candidate}")
    return candidate


def count_py_files(source_dir: Path) -> int:
    return sum(1 for _ in source_dir.rglob("*.py"))


def time_command(
    command: list[str],
    rounds: int,
    warmups: int,
    timeout: int = 300,
) -> dict:
    """Run a command multiple times and collect timing samples."""
    samples: list[float] = []
    errors: list[str] = []

    for i in range(rounds + warmups):
        try:
            start = time.perf_counter()
            completed = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            elapsed_ms = (time.perf_counter() - start) * 1000.0

            if completed.returncode != 0:
                errors.append(completed.stderr[:200])
                continue

            if i >= warmups:
                samples.append(elapsed_ms)

        except subprocess.TimeoutExpired:
            errors.append(f"timeout after {timeout}s")
            break

    if not samples:
        return {"success": False, "errors": errors[:3]}

    return {
        "success": True,
        "mean_ms": round(statistics.mean(samples), 2),
        "median_ms": round(statistics.median(samples), 2),
        "min_ms": round(min(samples), 2),
        "max_ms": round(max(samples), 2),
        "samples": len(samples),
    }


def print_results_table(results: list[dict]) -> None:
    """Print a human-readable comparison table."""
    # Collect all tool names that have results
    tool_names = set()
    for r in results:
        for tool_name in r.get("tools", {}):
            tool_names.add(tool_name)
    tool_names = sorted(tool_names)

    if not tool_names:
        print("No results to display.")
        return

    # Header
    tool_cols = "".join(f" │ {t:>12s}" for t in tool_names)
    header = f"{'Corpus':<12s} │ {'Files':>5s}{tool_cols}"
    separator = "─" * len(header)

    print()
    print(separator)
    print(header)
    print(separator)

    for r in results:
        cols = ""
        pycg_rs_ms = None
        for t in tool_names:
            tool_data = r.get("tools", {}).get(t, {})
            if tool_data.get("success"):
                ms = tool_data["median_ms"]
                if t == "pycg-rs":
                    pycg_rs_ms = ms
                cols += f" │ {ms:>9.0f} ms"
            else:
                cols += f" │ {'—':>12s}"

        print(f"{r['corpus']:<12s} │ {r['py_files']:>5d}{cols}")

    print(separator)

    # Speedup summary vs pycg-rs
    if "pycg-rs" in tool_names and len(tool_names) > 1:
        print()
        print("Speedup vs pycg-rs (median):")
        for r in results:
            pycg_rs_data = r.get("tools", {}).get("pycg-rs", {})
            if not pycg_rs_data.get("success"):
                continue
            pycg_rs_ms = pycg_rs_data["median_ms"]
            speedups = []
            for t in tool_names:
                if t == "pycg-rs":
                    continue
                tool_data = r.get("tools", {}).get(t, {})
                if tool_data.get("success"):
                    ratio = tool_data["median_ms"] / pycg_rs_ms
                    speedups.append(f"{t}: {ratio:.1f}x")
            if speedups:
                print(f"  {r['corpus']:<12s}  {', '.join(speedups)}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--pycg-rs",
        default=None,
        help="Path to pycg-rs binary (default: ./target/release/pycg)",
    )
    parser.add_argument(
        "--corpora",
        default="benchmarks/corpora",
        help="Path to vendored corpora",
    )
    parser.add_argument(
        "--results-dir",
        default="benchmarks/results",
        help="Directory for JSON results",
    )
    parser.add_argument(
        "--tools",
        default=None,
        help="Comma-separated list of tools to benchmark (default: all available)",
    )
    parser.add_argument(
        "--only",
        default=None,
        help="Comma-separated list of corpora to benchmark (default: all)",
    )
    parser.add_argument("--rounds", type=int, default=5, help="Measured rounds per tool")
    parser.add_argument("--warmups", type=int, default=1, help="Warmup rounds per tool")
    parser.add_argument("--timeout", type=int, default=300, help="Per-run timeout (seconds)")
    args = parser.parse_args()

    corpora_root = Path(args.corpora)
    results_dir = Path(args.results_dir)
    results_dir.mkdir(parents=True, exist_ok=True)

    # Resolve tools
    tools = list(ALL_TOOLS)
    if args.pycg_rs:
        tools[0] = PycgRs(binary=args.pycg_rs)

    if args.tools:
        requested = {t.strip() for t in args.tools.split(",")}
        tools = [t for t in tools if t.name in requested]

    available_tools = []
    for tool in tools:
        if tool.is_available():
            available_tools.append(tool)
            print(f"  [ok]   {tool.name} → {tool.resolve_binary()}", file=sys.stderr)
        else:
            print(f"  [skip] {tool.name} (not found)", file=sys.stderr)

    if not available_tools:
        print("No tools available. Run benchmarks/setup.sh first.", file=sys.stderr)
        sys.exit(1)

    # Resolve corpora
    corpus_names = list(SOURCE_HINTS.keys())
    if args.only:
        requested = {c.strip() for c in args.only.split(",")}
        corpus_names = [c for c in corpus_names if c in requested]

    # Run benchmarks
    results: list[dict] = []
    for corpus_name in corpus_names:
        try:
            source_dir = find_source_dir(corpora_root, corpus_name)
        except FileNotFoundError:
            print(f"  [skip] {corpus_name}: corpus not found", file=sys.stderr)
            continue

        py_files = count_py_files(source_dir)
        print(f"\n  Benchmarking {corpus_name} ({py_files} .py files)...", file=sys.stderr)

        corpus_result: dict = {
            "corpus": corpus_name,
            "py_files": py_files,
            "tools": {},
        }

        for tool in available_tools:
            try:
                cmd = tool.command(source_dir, corpus_name)
            except Exception as e:
                corpus_result["tools"][tool.name] = {"success": False, "errors": [str(e)]}
                continue

            print(f"    {tool.name}...", end="", file=sys.stderr, flush=True)
            timing = time_command(cmd, args.rounds, args.warmups, args.timeout)
            corpus_result["tools"][tool.name] = timing

            if timing.get("success"):
                print(f" {timing['median_ms']:.0f} ms", file=sys.stderr)
            else:
                print(" FAILED", file=sys.stderr)

        results.append(corpus_result)

    # Output
    output = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "rounds": args.rounds,
        "warmups": args.warmups,
        "timeout_s": args.timeout,
        "tools": [t.name for t in available_tools],
        "results": results,
    }

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    out_path = results_dir / f"bench-{timestamp}.json"
    out_path.write_text(json.dumps(output, indent=2) + "\n")

    print_results_table(results)
    print(f"\nWrote {out_path}")


if __name__ == "__main__":
    main()
