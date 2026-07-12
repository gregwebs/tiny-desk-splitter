#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v hurl >/dev/null 2>&1; then
    echo "test-hurl: 'hurl' is not installed or not on PATH. See https://hurl.dev/docs/installation.html" >&2
    exit 1
fi
exec node scripts/hurl-test.js