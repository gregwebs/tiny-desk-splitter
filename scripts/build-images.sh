#!/usr/bin/env bash
# scripts/build-images.sh — build base, dev, and release OCI images
#
# Works with Docker or Podman.  Set ENGINE to override auto-detection:
#   ENGINE=podman ./scripts/build-images.sh
#
# Build a single target:
#   ./scripts/build-images.sh base
#   ./scripts/build-images.sh dev
#   ./scripts/build-images.sh release
#
# By default all three targets are built in dependency order.

set -euo pipefail

# ── engine detection ──────────────────────────────────────────────────────────
ENGINE="${ENGINE:-}"
if [ -z "$ENGINE" ]; then
    if command -v docker &>/dev/null; then
        ENGINE=docker
    elif command -v podman &>/dev/null; then
        ENGINE=podman
    else
        echo "error: neither docker nor podman found on PATH" >&2
        exit 1
    fi
fi
echo "Using engine: $ENGINE"

# ── image tags ────────────────────────────────────────────────────────────────
TAG_BASE="${TAG_BASE:-tiny-desk-base}"
TAG_DEV="${TAG_DEV:-tiny-desk-dev}"
TAG_RELEASE="${TAG_RELEASE:-tiny-desk}"

# ── select targets ────────────────────────────────────────────────────────────
TARGETS=("${@:-base dev release}")
if [ "$#" -gt 0 ]; then
    TARGETS=("$@")
else
    TARGETS=(base dev release)
fi

# ── build ─────────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

build_target() {
    local target="$1"
    local tag
    case "$target" in
        base)    tag="$TAG_BASE" ;;
        dev)     tag="$TAG_DEV" ;;
        release) tag="$TAG_RELEASE" ;;
        *)       echo "Unknown target: $target" >&2; exit 1 ;;
    esac
    echo
    echo "═══ Building --target $target  →  $tag ═══"
    "$ENGINE" build \
        --target "$target" \
        --tag "$tag" \
        --file "$REPO_ROOT/Containerfile" \
        "$REPO_ROOT"
}

for target in "${TARGETS[@]}"; do
    build_target "$target"
done

echo
echo "Done.  Images built:"
for target in "${TARGETS[@]}"; do
    case "$target" in
        base)    echo "  base:    $TAG_BASE" ;;
        dev)     echo "  dev:     $TAG_DEV" ;;
        release) echo "  release: $TAG_RELEASE" ;;
    esac
done
echo
echo "Quick-start:"
echo "  $ENGINE run --rm -p 3000:3000 -v tiny-desk-data:/data $TAG_RELEASE"
