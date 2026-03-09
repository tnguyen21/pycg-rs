#!/usr/bin/env bash
# bootstrap-corpora.sh — clone or refresh local reference repos and corpora
#
# Usage:
#   ./scripts/bootstrap-corpora.sh          # clone/update everything
#   ./scripts/bootstrap-corpora.sh --only-refs   # only pyan + PyCG reference repos
#   ./scripts/bootstrap-corpora.sh --only-corpora # only benchmarks/corpora packages
#
# These directories are intentionally ignored by .gitignore and are never
# committed.  Normal `cargo test` skips corpus tests when these dirs are
# absent, so this script must be run explicitly before those tests are
# useful.  It is safe to run repeatedly — it only fetches, never mutates
# project source files.
#
# Invariants:
#   - Never modifies any tracked file.
#   - All network activity is opt-in (requires running this script).
#   - Idempotent: running twice leaves the same result as running once.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORPORA_DIR="$REPO_ROOT/benchmarks/corpora"

# ── helpers ────────────────────────────────────────────────────────────────

clone_or_update() {
    local url="$1"
    local dest="$2"
    local label="$3"

    if [ -d "$dest/.git" ]; then
        echo "  [update] $label"
        git -C "$dest" fetch --quiet origin
        git -C "$dest" reset --quiet --hard origin/HEAD
    else
        echo "  [clone]  $label → $(basename "$dest")"
        git clone --quiet --depth 1 "$url" "$dest"
    fi
}

# ── reference repos ────────────────────────────────────────────────────────

bootstrap_refs() {
    echo "==> Reference repos"

    clone_or_update \
        "https://github.com/Technologicat/pyan.git" \
        "$REPO_ROOT/pyan" \
        "pyan (reference Python call-graph tool)"

    clone_or_update \
        "https://github.com/vitsalis/PyCG.git" \
        "$REPO_ROOT/PyCG" \
        "PyCG (micro-benchmark suite)"
}

# ── corpora packages ───────────────────────────────────────────────────────

bootstrap_corpora() {
    echo "==> Corpora packages  (→ $CORPORA_DIR)"
    mkdir -p "$CORPORA_DIR"

    clone_or_update \
        "https://github.com/psf/requests.git" \
        "$CORPORA_DIR/requests" \
        "requests"

    clone_or_update \
        "https://github.com/Textualize/rich.git" \
        "$CORPORA_DIR/rich" \
        "rich"

    clone_or_update \
        "https://github.com/pallets/flask.git" \
        "$CORPORA_DIR/flask" \
        "flask"

    clone_or_update \
        "https://github.com/encode/httpx.git" \
        "$CORPORA_DIR/httpx" \
        "httpx"

    clone_or_update \
        "https://github.com/psf/black.git" \
        "$CORPORA_DIR/black" \
        "black"
}

# ── entry point ────────────────────────────────────────────────────────────

MODE="${1:-}"

case "$MODE" in
    --only-refs)
        bootstrap_refs
        ;;
    --only-corpora)
        bootstrap_corpora
        ;;
    "")
        bootstrap_refs
        bootstrap_corpora
        ;;
    *)
        echo "Usage: $0 [--only-refs | --only-corpora]" >&2
        exit 1
        ;;
esac

echo ""
echo "Done.  Run \`cargo test\` to include corpus smoke tests."
