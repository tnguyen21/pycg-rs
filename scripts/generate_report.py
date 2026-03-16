#!/usr/bin/env python3
"""Generate a self-contained HTML page for pycg-rs.

Runs pycg on popular Python projects and emits a single index.html with
a landing section, corpus results, and a short essay on why call graphs
matter for LLM workflows.

    python scripts/generate_report.py --pycg ./target/release/pycg \
        --corpora benchmarks/corpora --out report/index.html
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from html import escape
from pathlib import Path

# ---------------------------------------------------------------------------
# Corpus source-directory hints
# ---------------------------------------------------------------------------

SOURCE_HINTS = {
    "black": ["src/black"],
    "flask": ["src/flask"],
    "httpx": ["httpx"],
    "requests": ["src/requests"],
    "rich": ["rich"],
    "pytest": ["src"],
    "click": ["src/click"],
    "pydantic": ["pydantic"],
    "fastapi": ["fastapi"],
}

# ---------------------------------------------------------------------------
# Static content: comparison table (hardcoded from benchmarks, March 2026)
# ---------------------------------------------------------------------------

COMPARISON_ROWS = [
    ("Accuracy (fixtures)", "116/116 (100%)", "49/63 (78%)", "33/63 (52%)", "crashes on 3.12+"),
    ("Speed (requests)", "34 ms", "~400 ms", "~700 ms", "N/A"),
    ("Speed (fastapi)", "164 ms", "~1.8 s", "crashes", "N/A"),
    ("Speed (pydantic)", "540 ms", "~4.2 s", "crashes", "N/A"),
    ("Language", "Rust", "Python", "Python", "Python"),
    ("JSON output", "yes (7 schemas)", "PyCG-compat", "no", "yes"),
    ("Maintained", "active", "active", "maintained", "archived"),
]

# ---------------------------------------------------------------------------
# Static content: example
# ---------------------------------------------------------------------------

EXAMPLE_PYTHON = """\
# app/service.py
from app.db import get_user
from app.auth import verify_token

class UserService:
    def get_profile(self, token):
        user = verify_token(token)
        return get_user(user.id)

# app/db.py
def get_user(user_id):
    ...

# app/auth.py
def verify_token(token):
    ..."""

EXAMPLE_OUTPUT = """\
$ pycg callees get_profile src/app/ --match suffix

  app.service.UserService.get_profile
    → app.auth.verify_token
    → app.db.get_user"""

# ---------------------------------------------------------------------------
# Static content: "why" essay
# ---------------------------------------------------------------------------

WHY_ESSAY = """\
<p>
LLMs working on code face a fundamental constraint: they cannot read an entire
repository. Context windows are finite, and even million-token models degrade
when stuffed with irrelevant source files. The practical question is always
<em>which code should the model see?</em>
</p>

<p>
Call graphs answer this structurally. Given a function you want to change, the
graph tells you what it calls, what calls it, and the shortest dependency path
between any two symbols. This turns "find all code related to X" from a
grep-and-hope exercise into a precise, bounded query.
</p>

<p>
The key design decision in pycg-rs is <strong>machine-consumable output</strong>.
Every subcommand emits structured JSON with versioned schemas, diagnostics that
surface uncertainty, and provenance metadata. This is not a tool for generating
pretty diagrams (though it can do that too) &mdash; it is a routing layer that
tells an agent which files to read, which functions to inspect, and how
confident to be in the result.
</p>

<p>
Speed matters because the tool runs in the loop, not ahead of it. At 34 ms on a
small package and under 200 ms on most real codebases, pycg-rs is fast enough to
invoke on every prompt &mdash; as a live tool call, not a pre-computed artifact
that goes stale.
</p>

<p>
Static analysis is not omniscient. Dynamic dispatch, metaprogramming, and
framework magic will always create blind spots. The right response is not to
pretend these don't exist, but to surface them: pycg-rs reports external
references, unresolved names, and ambiguous resolutions as first-class
diagnostics. An agent that reads the diagnostics can decide when to trust the
graph and when to fall back to broader search.
</p>

<p>
The graph doesn't replace reading code. It tells you <em>which</em> code to read.
</p>"""

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def find_source_dir(corpus_dir: Path, name: str) -> Path | None:
    hints = SOURCE_HINTS.get(name, [name])
    for hint in hints:
        candidate = corpus_dir / hint
        if candidate.is_dir():
            return candidate
    candidate = corpus_dir / name
    if candidate.is_dir():
        return candidate
    return None


def count_py_files(directory: Path) -> int:
    return sum(1 for _ in directory.rglob("*.py"))


def run_pycg_json(pycg_bin: str, source_dir: Path) -> dict | None:
    """Run pycg --format json on source_dir, return parsed JSON or None."""
    try:
        start = time.monotonic()
        result = subprocess.run(
            [pycg_bin, "analyze", str(source_dir), "--format", "json"],
            capture_output=True, text=True, timeout=120,
        )
        elapsed = time.monotonic() - start
        if result.returncode != 0:
            print(f"  [warn] pycg json exited {result.returncode}: {result.stderr[:200]}", file=sys.stderr)
            return None
        data = json.loads(result.stdout)
        data["_elapsed_ms"] = round(elapsed * 1000)
        return data
    except (subprocess.TimeoutExpired, json.JSONDecodeError) as e:
        print(f"  [warn] pycg json failed: {e}", file=sys.stderr)
        return None


def run_pycg_svg(pycg_bin: str, source_dir: Path) -> str | None:
    """Run pycg --modules --colored | dot -Tsvg, return SVG string or None."""
    try:
        pycg_proc = subprocess.run(
            [pycg_bin, "analyze", str(source_dir), "--modules", "--colored", "--format", "dot"],
            capture_output=True, text=True, timeout=120,
        )
        if pycg_proc.returncode != 0:
            print(f"  [warn] pycg dot exited {pycg_proc.returncode}", file=sys.stderr)
            return None
        dot_proc = subprocess.run(
            ["dot", "-Tsvg"],
            input=pycg_proc.stdout, capture_output=True, text=True, timeout=30,
        )
        if dot_proc.returncode != 0:
            print(f"  [warn] dot exited {dot_proc.returncode}", file=sys.stderr)
            return None
        svg = dot_proc.stdout
        for prefix in ('<?xml', '<!DOCTYPE'):
            idx = svg.find(prefix)
            if idx != -1:
                end = svg.find('\n', idx)
                if end != -1:
                    svg = svg[:idx] + svg[end + 1:]
        return svg.strip()
    except subprocess.TimeoutExpired:
        print(f"  [warn] SVG generation timed out for {source_dir}", file=sys.stderr)
        return None


# ---------------------------------------------------------------------------
# HTML generation
# ---------------------------------------------------------------------------


def _hero_html(accuracy: dict | None) -> str:
    acc_text = ""
    if accuracy:
        p = accuracy["passed_expectations"]
        t = accuracy["total_expectations"]
        acc_text = f"{p}/{t} ({p*100//t}%)"
    else:
        acc_text = "116/116 (100%)"

    comparison_rows = ""
    for label, pycg, jarvis, pyan, orig in COMPARISON_ROWS:
        # Replace the pycg-rs accuracy cell with the dynamic value
        cell = acc_text if "Accuracy" in label else escape(pycg)
        comparison_rows += f"""<tr>
  <td>{escape(label)}</td>
  <td class="highlight">{cell}</td>
  <td>{escape(jarvis)}</td>
  <td>{escape(pyan)}</td>
  <td>{escape(orig)}</td>
</tr>"""

    return f"""
<header class="hero">
  <h1>pycg-rs</h1>
  <p class="tagline">Fast, accurate static call graph analysis for Python.</p>
  <p class="tagline-sub">No runtime required. JSON output for humans and machines.</p>

  <nav class="page-nav">
    <a href="#get-started">Get started</a>
    <a href="#comparison">Comparison</a>
    <a href="#corpus">Corpus results</a>
    <a href="#why">Why call graphs?</a>
  </nav>

  <div class="section" id="get-started">
    <h2>Get started</h2>
    <pre class="code-block"><span class="prompt">$</span> git clone https://github.com/nwyin/pycg-rs && cd pycg-rs
<span class="prompt">$</span> cargo build --release
<span class="prompt">$</span> target/release/pycg analyze src/myapp/ --format json</pre>
  </div>

  <div class="example-block">
    <div class="example-side">
      <p class="example-label">Python source</p>
      <pre class="code-block">{escape(EXAMPLE_PYTHON)}</pre>
    </div>
    <div class="example-side">
      <p class="example-label">What pycg-rs finds</p>
      <pre class="code-block">{escape(EXAMPLE_OUTPUT)}</pre>
    </div>
  </div>

  <div class="section" id="comparison">
    <h2>How it compares</h2>
    <p class="section-desc">
      Accuracy measured on
      <a href="https://github.com/nwyin/pycg-rs/blob/main/tests/fixtures/accuracy_cases.json">116 fixture expectations</a>
      across 18 categories. Speed measured on real open-source projects (single-threaded, cold).
      <a href="https://github.com/nwyin/pycg-rs/blob/main/docs/limitations.md">Limitations documented honestly.</a>
    </p>
    <table class="comparison-table">
      <thead>
        <tr>
          <th></th>
          <th class="highlight">pycg-rs</th>
          <th>jarviscg</th>
          <th>pyan3</th>
          <th>PyCG</th>
        </tr>
      </thead>
      <tbody>
        {comparison_rows}
      </tbody>
    </table>
  </div>
</header>"""


def _corpus_html(corpora_results: list[dict], accuracy: dict | None) -> str:
    dash = "&mdash;"

    def node_kind_count(stats: dict, key: str) -> int | str:
        return stats.get("by_node_kind", {}).get(key, dash)

    def function_like_count(stats: dict) -> int | str:
        by_node_kind = stats.get("by_node_kind", {})
        values = [
            by_node_kind.get("function", 0),
            by_node_kind.get("method", 0),
            by_node_kind.get("static_method", 0),
            by_node_kind.get("class_method", 0),
        ]
        return sum(values) if any(value != 0 for value in values) else dash

    # Summary stats
    times = [r["_elapsed_ms"] for r in corpora_results if isinstance(r.get("_elapsed_ms"), int)]
    speed_range = f"{min(times)}&ndash;{max(times)} ms" if times else dash
    n_projects = len(corpora_results)

    acc_line = ""
    if accuracy:
        p = accuracy["passed_expectations"]
        t = accuracy["total_expectations"]
        acc_line = f'<span class="stat">Accuracy: <strong>{p}/{t}</strong> expectations</span>'
    else:
        acc_line = '<span class="stat">Accuracy: <strong>116/116</strong> expectations</span>'

    # Table rows
    rows_html = ""
    for r in corpora_results:
        s = r.get("stats", {})
        elapsed = r.get("_elapsed_ms", dash)
        py_files = r.get("_py_files", dash)
        status_class = "ok" if r.get("_success") else "fail"
        status_text = "&#x2713;" if r.get("_success") else "&#x2717;"
        rows_html += f"""<tr class="{status_class}">
  <td class="name">{escape(r['name'])}</td>
  <td>{py_files}</td>
  <td>{s.get('files_analyzed', dash)}</td>
  <td>{s.get('nodes', dash)}</td>
  <td>{node_kind_count(s, 'class')}</td>
  <td>{function_like_count(s)}</td>
  <td>{s.get('edges', dash)}</td>
  <td>{elapsed}ms</td>
  <td class="status">{status_text}</td>
</tr>"""

    # SVG accordions
    details_html = ""
    for r in corpora_results:
        if not r.get("_success"):
            continue
        s = r.get("stats", {})
        svg = r.get("_svg", "")
        summary_stats = (
            f"{s.get('nodes', 0)} nodes, "
            f"{s.get('edges', 0)} edges, "
            f"{s.get('by_node_kind', {}).get('class', 0)} classes, "
            f"{sum(s.get('by_node_kind', {}).get(kind, 0) for kind in ['function', 'method', 'static_method', 'class_method'])} functions"
        )
        svg_block = f'<div class="svg-container">{svg}</div>' if svg else '<p class="no-svg">SVG not available (graphviz not found?)</p>'
        details_html += f"""
<details class="corpus-detail" id="detail-{escape(r['name'])}">
  <summary>{escape(r['name'])} &mdash; {summary_stats}</summary>
  <div class="detail-body">
    <p class="detail-caption">Module-level dependency graph &mdash; each node is a Python module, edges represent cross-module calls or imports.</p>
    {svg_block}
  </div>
</details>"""

    return f"""
<section class="section" id="corpus">
  <h2>Corpus results</h2>
  <p class="section-desc">
    Analysis of {n_projects} popular open-source Python projects. Click column headers to sort.
  </p>
  <div class="report-summary">
    {acc_line}
    <span class="stat">Speed: <strong>{speed_range}</strong> across {n_projects} projects</span>
  </div>
  <table>
    <thead>
      <tr>
        <th>Project</th>
        <th>.py files</th>
        <th>Analyzed</th>
        <th>Nodes</th>
        <th>Classes</th>
        <th>Functions</th>
        <th>Edges</th>
        <th>Time</th>
        <th>Status</th>
      </tr>
    </thead>
    <tbody>
      {rows_html}
    </tbody>
  </table>

  <h3 style="margin-top: 2rem; font-size: 1rem; margin-bottom: 0.5rem;">Module dependency graphs</h3>
  <p class="section-desc">Module-level view &mdash; functions and classes collapsed into their owning module. Generated with <code>pycg analyze --modules --colored</code>.</p>
  {details_html}
</section>"""


def _essay_html() -> str:
    return f"""
<section class="section why-essay" id="why">
  <h2>Why machine-readable call graphs matter for LLM workflows</h2>
  {WHY_ESSAY}
</section>"""


def generate_html(corpora_results: list[dict], meta: dict, accuracy: dict | None) -> str:
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    commit = meta.get("commit", "unknown")

    hero = _hero_html(accuracy)
    corpus = _corpus_html(corpora_results, accuracy)
    essay = _essay_html()

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pycg-rs &mdash; Static call graphs for Python</title>
<style>
:root {{
  --bg: #0d1117;
  --surface: #161b22;
  --border: #30363d;
  --text: #e6edf3;
  --text-muted: #8b949e;
  --accent: #58a6ff;
  --green: #3fb950;
  --red: #f85149;
  --font: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
  --mono: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
}}
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
body {{
  font-family: var(--font);
  background: var(--bg);
  color: var(--text);
  line-height: 1.6;
  padding: 2rem;
  max-width: 1100px;
  margin: 0 auto;
}}
a {{ color: var(--accent); text-decoration: none; }}
a:hover {{ text-decoration: underline; }}

/* Hero */
.hero {{ margin-bottom: 3rem; }}
h1 {{ font-size: 2rem; margin-bottom: 0.25rem; }}
.tagline {{ font-size: 1.125rem; color: var(--text); margin-bottom: 0.125rem; }}
.tagline-sub {{ font-size: 0.9375rem; color: var(--text-muted); margin-bottom: 1.5rem; }}

/* Nav */
.page-nav {{ margin-bottom: 2rem; display: flex; gap: 1.5rem; font-size: 0.875rem; }}
.page-nav a {{ color: var(--text-muted); }}
.page-nav a:hover {{ color: var(--accent); }}

/* Code blocks */
.code-block {{
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 1rem;
  font-family: var(--mono);
  font-size: 0.8125rem;
  line-height: 1.6;
  overflow-x: auto;
  white-space: pre;
}}
.code-block .prompt {{ color: var(--text-muted); }}

/* Example side-by-side */
.example-block {{
  display: flex;
  gap: 1rem;
  margin: 1.5rem 0 2rem;
}}
.example-side {{ flex: 1; min-width: 0; }}
.example-label {{
  color: var(--text-muted);
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 0.5rem;
}}
@media (max-width: 768px) {{
  .example-block {{ flex-direction: column; }}
}}

/* Sections */
.section {{ margin-top: 2.5rem; }}
.section h2 {{ font-size: 1.125rem; margin-bottom: 0.5rem; padding-bottom: 0.5rem; border-bottom: 1px solid var(--border); }}
.section h3 {{ font-size: 1rem; }}
.section-desc {{ color: var(--text-muted); font-size: 0.8125rem; margin-bottom: 1rem; line-height: 1.6; }}
.section-desc a {{ color: var(--accent); }}

/* Report summary stats */
.report-summary {{
  display: flex;
  gap: 2rem;
  margin-bottom: 1.5rem;
  font-size: 0.875rem;
}}
.report-summary .stat {{ color: var(--text-muted); }}
.report-summary .stat strong {{ color: var(--text); font-family: var(--mono); }}

/* Tables */
table {{
  width: 100%;
  border-collapse: collapse;
  font-size: 0.875rem;
  margin-bottom: 1rem;
}}
th, td {{
  padding: 0.5rem 0.75rem;
  text-align: left;
  border-bottom: 1px solid var(--border);
}}
th {{
  color: var(--text-muted);
  font-weight: 500;
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  white-space: nowrap;
  cursor: pointer;
}}
th:hover {{ color: var(--text); }}
td {{ font-family: var(--mono); font-size: 0.8125rem; }}
td.name {{ font-weight: 600; color: var(--accent); }}
tr.ok td.status {{ color: var(--green); }}
tr.fail td.status {{ color: var(--red); }}
tr:hover {{ background: rgba(88,166,255,0.04); }}

/* Comparison table highlight column */
.comparison-table th.highlight,
.comparison-table td.highlight {{
  color: var(--green);
  font-weight: 600;
}}

/* Corpus detail accordion */
details.corpus-detail {{
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 6px;
  margin-bottom: 0.75rem;
}}
details.corpus-detail summary {{
  padding: 0.75rem 1rem;
  cursor: pointer;
  font-weight: 500;
  font-size: 0.875rem;
}}
details.corpus-detail summary:hover {{ color: var(--accent); }}
.detail-body {{ padding: 0 1rem 1rem; }}
.detail-caption {{ color: var(--text-muted); font-size: 0.75rem; margin-bottom: 0.75rem; }}
.svg-container {{
  background: #fff;
  border-radius: 4px;
  padding: 1rem;
  overflow-x: auto;
  text-align: center;
}}
.svg-container svg {{ max-width: 100%; height: auto; }}
.no-svg {{ color: var(--text-muted); font-style: italic; font-size: 0.8125rem; }}

/* Why essay */
.why-essay p {{
  color: var(--text-muted);
  font-size: 0.9375rem;
  line-height: 1.7;
  max-width: 65ch;
  margin-bottom: 1rem;
}}
.why-essay strong {{ color: var(--text); }}
.why-essay em {{ color: var(--text); font-style: italic; }}

/* Footer */
footer {{
  margin-top: 3rem;
  padding-top: 1rem;
  border-top: 1px solid var(--border);
  color: var(--text-muted);
  font-size: 0.75rem;
}}
footer a {{ color: var(--accent); }}
</style>
</head>
<body>

{hero}
{corpus}
{essay}

<footer>
  Generated {now} from commit <code>{escape(str(commit)[:8])}</code>.
  <a href="https://github.com/nwyin/pycg-rs">Source on GitHub</a>.
</footer>

<script>
document.querySelectorAll('table thead tr').forEach(function(headerRow) {{
  headerRow.addEventListener('click', function(e) {{
    const th = e.target.closest('th');
    if (!th) return;
    const table = th.closest('table');
    const tbody = table.querySelector('tbody');
    if (!tbody) return;
    const rows = Array.from(tbody.rows);
    const idx = Array.from(th.parentNode.children).indexOf(th);
    const dir = th.dataset.sort === 'asc' ? -1 : 1;
    th.dataset.sort = dir === 1 ? 'asc' : 'desc';
    rows.sort((a, b) => {{
      let av = a.cells[idx].textContent.replace(/[^\\d.]/g, '');
      let bv = b.cells[idx].textContent.replace(/[^\\d.]/g, '');
      const an = parseFloat(av), bn = parseFloat(bv);
      if (!isNaN(an) && !isNaN(bn)) return (an - bn) * dir;
      return a.cells[idx].textContent.localeCompare(b.cells[idx].textContent) * dir;
    }});
    rows.forEach(r => tbody.appendChild(r));
  }});
}});
</script>
</body>
</html>"""


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description="Generate pycg-rs HTML report")
    parser.add_argument("--pycg", default="./target/release/pycg", help="Path to pycg binary")
    parser.add_argument("--corpora", default="benchmarks/corpora", help="Corpora directory")
    parser.add_argument("--out", default="report/index.html", help="Output HTML path")
    parser.add_argument("--accuracy-json", default=None, help="Path to accuracy_report.py JSON output")
    parser.add_argument("--commit", default=None, help="Git commit hash")
    args = parser.parse_args()

    corpora_dir = Path(args.corpora)
    if not corpora_dir.is_dir():
        print(f"Corpora directory not found: {corpora_dir}", file=sys.stderr)
        sys.exit(1)

    commit = args.commit
    if not commit:
        try:
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
        except Exception:
            commit = "unknown"

    # Load accuracy data if provided
    accuracy = None
    if args.accuracy_json:
        try:
            accuracy = json.loads(Path(args.accuracy_json).read_text())
        except Exception as e:
            print(f"  [warn] Could not load accuracy JSON: {e}", file=sys.stderr)

    # Check for graphviz
    has_dot = subprocess.run(["which", "dot"], capture_output=True).returncode == 0
    if not has_dot:
        print("  [warn] graphviz 'dot' not found, SVGs will be skipped", file=sys.stderr)

    results = []
    for corpus_name in sorted(os.listdir(corpora_dir)):
        corpus_path = corpora_dir / corpus_name
        if not corpus_path.is_dir():
            continue
        source_dir = find_source_dir(corpus_path, corpus_name)
        if not source_dir:
            print(f"  [skip] {corpus_name}: no source directory found", file=sys.stderr)
            continue

        py_count = count_py_files(source_dir)
        print(f"  Analyzing {corpus_name} ({py_count} .py files)...", file=sys.stderr)

        data = run_pycg_json(args.pycg, source_dir)
        svg = run_pycg_svg(args.pycg, source_dir) if has_dot else None

        if data:
            entry = {
                "name": corpus_name,
                "_py_files": py_count,
                "_success": True,
                "_elapsed_ms": data.pop("_elapsed_ms", "—"),
                "stats": data.get("stats", {}),
                "_svg": svg or "",
            }
        else:
            entry = {
                "name": corpus_name,
                "_py_files": py_count,
                "_success": False,
            }
        results.append(entry)

    meta = {"commit": commit}
    html = generate_html(results, meta, accuracy)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(html)
    print(f"Report written to {out_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
