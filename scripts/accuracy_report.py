#!/usr/bin/env python3
"""Evaluate semantic accuracy fixtures against the pycg CLI.

This reads the declarative fixture manifest in tests/fixtures/accuracy_cases.json,
runs pycg in JSON mode for each fixture set, and prints a compact summary.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = REPO_ROOT / "tests" / "fixtures" / "accuracy_cases.json"


@dataclass(frozen=True)
class GraphKey:
    files: tuple[str, ...]
    root: str | None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--pycg",
        help="Path to the pycg binary. Defaults to `cargo run --quiet --bin pycg --`.",
    )
    parser.add_argument(
        "--manifest",
        default=str(DEFAULT_MANIFEST),
        help="Path to the accuracy fixture manifest.",
    )
    parser.add_argument(
        "--out",
        help="Optional path to write the JSON summary.",
    )
    return parser.parse_args()


def load_manifest(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text())


def resolve_pycg_cmd(pycg: str | None) -> list[str]:
    if pycg:
        return [pycg]
    return ["cargo", "run", "--quiet", "--bin", "pycg", "--"]


def run_pycg_json(
    pycg_cmd: list[str],
    files: list[str],
    root: str | None,
) -> dict[str, Any]:
    cmd = [*pycg_cmd, *files, "--format", "json"]
    if root:
        cmd.extend(["--root", root])

    result = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"pycg exited {result.returncode} for {' '.join(files)}:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def match_name(actual: str, expected: str, matcher: str) -> bool:
    if matcher in {"short", "concrete_short"}:
        return actual.rsplit(".", 1)[-1] == expected
    if matcher in {"full", "concrete_full"}:
        return actual == expected
    raise ValueError(f"unknown matcher: {matcher}")


def evaluate_expectation(graph: dict[str, Any], expectation: dict[str, Any]) -> dict[str, Any]:
    edge_kind = expectation["kind"]
    source_match = expectation.get("source_match", "short")
    target_match = expectation.get("target_match", "short")

    matching_sources = {
        edge["source"]
        for edge in graph["edges"]
        if edge["kind"] == edge_kind
        and match_name(edge["source"], expectation["source"], source_match)
    }
    matching_targets = sorted(
        {
            edge["target"]
            for edge in graph["edges"]
            if edge["kind"] == edge_kind
            and edge["source"] in matching_sources
            and match_name(edge["target"], expectation["target"], target_match)
        }
    )
    matched_count = len(matching_targets)
    required_matches = expectation.get("min_matches", 1)
    present = expectation["present"]
    passed = matched_count >= required_matches if present else matched_count == 0
    return {
        "passed": passed,
        "matched_count": matched_count,
        "matched_targets": matching_targets,
        "required_matches": required_matches,
    }


def print_summary(summary: dict[str, Any]) -> None:
    print(
        "Accuracy fixtures:"
        f" {summary['passed_expectations']}/{summary['total_expectations']} expectations passed"
        f" across {summary['passed_cases']}/{summary['total_cases']} cases"
    )
    print()

    for category, counts in sorted(summary["categories"].items()):
        print(
            f"- {category}: {counts['passed_expectations']}/{counts['total_expectations']}"
            f" expectations, {counts['passed_cases']}/{counts['total_cases']} cases"
        )

    failed_cases = [case for case in summary["cases"] if not case["passed"]]
    if not failed_cases:
        return

    print()
    print("Failed cases:")
    for case in failed_cases:
        print(f"- {case['id']} [{case['category']}]")
        for failure in case["failed_expectations"]:
            print(
                "  "
                f"{failure['kind']} {failure['source']} -> {failure['target']} "
                f"expected {'present' if failure['present'] else 'absent'}; "
                f"matched {failure['matched_count']}: "
                f"{', '.join(failure['matched_targets']) or '<none>'}"
            )


def main() -> int:
    args = parse_args()
    manifest_path = Path(args.manifest)
    manifest = load_manifest(manifest_path)
    pycg_cmd = resolve_pycg_cmd(args.pycg)

    cache: dict[GraphKey, dict[str, Any]] = {}
    categories: dict[str, dict[str, int]] = defaultdict(
        lambda: {
            "total_cases": 0,
            "passed_cases": 0,
            "total_expectations": 0,
            "passed_expectations": 0,
        }
    )
    case_summaries = []
    total_expectations = 0
    passed_expectations = 0

    for case in manifest["cases"]:
        files = [str((REPO_ROOT / file).resolve()) for file in case["files"]]
        root = str((REPO_ROOT / case["root"]).resolve()) if case.get("root") else None
        key = GraphKey(tuple(files), root)
        graph = cache.get(key)
        if graph is None:
            graph = run_pycg_json(pycg_cmd, files, root)
            cache[key] = graph

        category = categories[case["category"]]
        category["total_cases"] += 1
        failed_expectations = []
        case_total = 0
        case_passed = 0

        for expectation in case["expectations"]:
            case_total += 1
            total_expectations += 1
            category["total_expectations"] += 1
            result = evaluate_expectation(graph, expectation)
            if result["passed"]:
                case_passed += 1
                passed_expectations += 1
                category["passed_expectations"] += 1
            else:
                failed_expectations.append(
                    {
                        **expectation,
                        **result,
                    }
                )

        case_summary = {
            "id": case["id"],
            "category": case["category"],
            "passed": not failed_expectations,
            "total_expectations": case_total,
            "passed_expectations": case_passed,
            "failed_expectations": failed_expectations,
        }
        case_summaries.append(case_summary)
        if case_summary["passed"]:
            category["passed_cases"] += 1

    summary = {
        "total_cases": len(case_summaries),
        "passed_cases": sum(1 for case in case_summaries if case["passed"]),
        "total_expectations": total_expectations,
        "passed_expectations": passed_expectations,
        "categories": dict(categories),
        "cases": case_summaries,
    }

    print_summary(summary)

    if args.out:
        out_path = Path(args.out)
        out_path.write_text(json.dumps(summary, indent=2) + "\n")

    return 0 if passed_expectations == total_expectations else 1


if __name__ == "__main__":
    sys.exit(main())
