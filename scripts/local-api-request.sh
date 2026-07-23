#!/usr/bin/env bash
# Issue an HTTP request to concert-web running on loopback.
#
# Usage: ./scripts/local-api-request.sh PORT PATH [METHOD [BODY_FILE]]
#
# Before every real request, this script probes `GET /health` on the same
# port and requires the response to self-identify as `concert-web` (first
# text line, exact match). This script's stable prefix is allow-listed to run
# without a permission prompt, and it now accepts any absolute path, method,
# and body — without the identity check, that allow-listing would let an
# agent repurpose it to fire ad-hoc requests at *any* loopback service, not
# just concert-web. The check is a self-declared identity handshake, a
# misdirection guardrail — NOT authentication: any process could return
# "concert-web", so it only confines this script to concert-web instances, it
# does not prove a hostile process on loopback isn't lying. See
# `concert-tracker/src/web/handlers.rs`'s `SERVICE_IDENTITY`/`health` doc
# comment for the server side; the literal "concert-web" below is kept in
# sync with that constant by hand (no shared symbol crosses the language
# boundary). Fails closed: unreachable, non-2xx, or a mismatched identity all
# abort before the real request, with a dedicated exit code (see
# IDENTITY_MISMATCH_EXIT below) distinct from curl's own exit codes.
#
# TOCTOU caveat: the probe and the real request are two separate connections,
# so in principle the port's owner could change between them. Not engineered
# around — negligible for a persistent loopback dev server, and a
# single-connection guarantee isn't worth the complexity here.
#
# A concert-web build that predates `/health` will 404 the probe and this
# script will report an identity mismatch, not a helpful "upgrade your
# binary" message — acceptable for same-repo dev tooling.
set -euo pipefail

readonly MIN_PORT=1
readonly MAX_PORT=65535
readonly MAX_PORT_DIGITS=${#MAX_PORT}
readonly SERVICE_IDENTITY="concert-web"
# Distinct from curl's own exit codes (1-94; curl reserves 3 for "URL
# malformed") so a caller can tell "target isn't concert-web" apart from a
# curl failure on the real request, which passes its own exit code through.
readonly IDENTITY_MISMATCH_EXIT=100

usage() {
  echo "usage: $0 PORT PATH [METHOD [BODY_FILE]]" >&2
  echo "       PATH must begin with /" >&2
  echo "       METHOD defaults to GET and must contain uppercase letters only" >&2
  echo "       BODY_FILE is optional and sent as application/json" >&2
  echo "       host fixed to 127.0.0.1" >&2
  echo "       stable approval prefix: ./scripts/local-api-request.sh" >&2
  echo "       before every request, GET /health must identify as $SERVICE_IDENTITY (exit $IDENTITY_MISMATCH_EXIT otherwise)" >&2
}

invalid_port() {
  echo "error: PORT must be an integer from $MIN_PORT through $MAX_PORT" >&2
  exit 2
}

identity_mismatch() {
  echo "error: 127.0.0.1:${port_number} did not identify as $SERVICE_IDENTITY (GET /health)" >&2
  exit "$IDENTITY_MISMATCH_EXIT"
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

if [ "$#" -lt 2 ] || [ "$#" -gt 4 ]; then
  usage
  exit 2
fi

port="$1"
request_path="$2"
method="${3:-GET}"
body_file="${4:-}"

case "$port" in
  ''|*[!0-9]*)
    invalid_port
    ;;
esac

normalized_port="$port"
while [ "${#normalized_port}" -gt 1 ] && [ "${normalized_port#0}" != "$normalized_port" ]; do
  normalized_port="${normalized_port#0}"
done

if [ "${#normalized_port}" -gt "$MAX_PORT_DIGITS" ]; then
  invalid_port
fi

port_number=$((10#$normalized_port))
if [ "$port_number" -lt "$MIN_PORT" ] || [ "$port_number" -gt "$MAX_PORT" ]; then
  invalid_port
fi

case "$request_path" in
  /*)
    ;;
  *)
    echo "error: PATH must begin with /" >&2
    exit 2
    ;;
esac

case "$request_path" in
  *[[:space:]]*|*[[:cntrl:]]*)
    echo "error: PATH must not contain whitespace or control characters" >&2
    exit 2
    ;;
esac

case "$method" in
  ''|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZ]*)
    echo "error: METHOD must contain uppercase letters only" >&2
    exit 2
    ;;
esac

if [ -n "$body_file" ] && [ ! -r "$body_file" ]; then
  echo "error: BODY_FILE must be a readable file" >&2
  exit 2
fi

if [ -x /opt/homebrew/opt/curl/bin/curl ]; then
  curl_bin=/opt/homebrew/opt/curl/bin/curl
else
  curl_bin=curl
fi

# Identity probe: confirm 127.0.0.1:$port_number is concert-web before
# issuing the caller's real request. Capture into a variable rather than
# piping curl into `head -1` — under `set -o pipefail` above, `head` closing
# early after one line can SIGPIPE curl and trip pipefail, turning a
# *successful* probe into a spurious failure.
if ! health_response=$("$curl_bin" -fsS -H "Accept: text/plain" -- \
  "http://127.0.0.1:${port_number}/health" 2>/dev/null); then
  identity_mismatch
fi
health_first_line=${health_response%%$'\n'*}
health_first_line=${health_first_line%$'\r'}
if [ "$health_first_line" != "$SERVICE_IDENTITY" ]; then
  identity_mismatch
fi

if [ -n "$body_file" ]; then
  exec "$curl_bin" -fsS -X "$method" \
    -H "Content-Type: application/json" \
    --data-binary "@${body_file}" \
    -- "http://127.0.0.1:${port_number}${request_path}"
fi

exec "$curl_bin" -fsS -X "$method" -- "http://127.0.0.1:${port_number}${request_path}"
