#!/usr/bin/env bash
# setup.sh — bootstrap benchmark environment with all competitor tools
#
# Installs:
#   - PyCG      (original, archived — pip install pycg)
#   - pyan3     (AST + symtable — pip install pyan3)
#   - code2flow (AST, multi-language — pip install code2flow)
#   - jarviscg  (JARVIS fork — install from git)
#
# Also clones benchmark corpora via bootstrap-corpora.sh.
#
# Usage:
#   ./benchmarks/setup.sh              # full setup
#   ./benchmarks/setup.sh --only-tools # skip corpora bootstrap

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENV_DIR="$REPO_ROOT/benchmarks/.venv"
VENV_PY="$VENV_DIR/bin/python3"
MODE="${1:-}"

# ── corpora ───────────────────────────────────────────────────────────────

if [ "$MODE" != "--only-tools" ]; then
    "$REPO_ROOT/scripts/bootstrap-corpora.sh" --only-corpora
fi

# ── venv + tools ──────────────────────────────────────────────────────────

if [ ! -d "$VENV_DIR" ]; then
    echo "Creating venv at $VENV_DIR (Python 3.12)"
    uv venv --python 3.12 "$VENV_DIR"
fi

echo ""
echo "==> Installing competitor tools"

install_tool() {
    local name="$1"
    local spec="$2"
    local check_mod="${3:-$name}"

    if "$VENV_PY" -c "import importlib; importlib.import_module('$check_mod')" 2>/dev/null; then
        echo "  [ok]   $name (already installed)"
    else
        echo "  [install] $name"
        if uv pip install --python "$VENV_PY" $spec >/dev/null 2>&1; then
            echo "  [ok]   $name"
        else
            echo "  [FAIL] $name — install failed, will be skipped in benchmarks"
        fi
    fi
}

# PyCG — original static call graph generator (archived, may fail on newer Python)
# Install from GitHub since PyPI package has broken case (PyCG vs pycg).
install_tool "pycg" "pycg @ git+https://github.com/vitsalis/PyCG.git" "pycg"

# Fix PyCG packaging: installs as PyCG/ but uses `from pycg import ...` internally.
# On case-insensitive filesystems (macOS) we rename to lowercase.
SITE_PKGS="$VENV_DIR/lib/python3.12/site-packages"
if [ -d "$SITE_PKGS/PyCG" ] && ! "$VENV_PY" -c "from pycg import pycg" 2>/dev/null; then
    echo "  [fix]  Renaming PyCG → pycg (case fix)"
    cp -r "$SITE_PKGS/PyCG" /tmp/_pycg_fix && rm -rf "$SITE_PKGS/PyCG" && mv /tmp/_pycg_fix "$SITE_PKGS/pycg"
fi

# PyCG needs pkg_resources from setuptools (removed in setuptools>=75)
install_tool "setuptools" "setuptools<75" "pkg_resources"

# pyan3 — AST + symtable based call graph
install_tool "pyan3" "pyan3" "pyan"

# code2flow — lightweight AST call graph for dynamic languages
install_tool "code2flow" "code2flow" "code2flow"

# jarviscg — JARVIS fork, improved precision over PyCG
echo "  [install] jarviscg"
if uv pip install --python "$VENV_PY" "jarviscg @ git+https://github.com/nuanced-dev/jarviscg" >/dev/null 2>&1; then
    echo "  [ok]   jarviscg"
else
    echo "  [FAIL] jarviscg — install failed, will be skipped in benchmarks"
fi

# ── build pycg-rs ─────────────────────────────────────────────────────────

echo ""
echo "==> Building pycg-rs (release)"
(cd "$REPO_ROOT" && cargo build --release --quiet)

# ── summary ───────────────────────────────────────────────────────────────

echo ""
echo "Benchmark environment ready."
echo ""
echo "Available tools:"
for tool in pycg pyan3 code2flow jarviscg; do
    if [ -x "$VENV_DIR/bin/$tool" ]; then
        echo "  ✓ $tool"
    else
        echo "  ✗ $tool (not found)"
    fi
done
echo "  ✓ pycg-rs (./target/release/pycg)"
echo ""
echo "Run:"
echo "  python3 benchmarks/bench.py                          # performance"
echo "  python3 benchmarks/compare.py                        # accuracy comparison"
echo "  python3 benchmarks/compare.py --corpus-compare       # + edge set comparison on corpora"
