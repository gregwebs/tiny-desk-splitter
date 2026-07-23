#!/usr/bin/env bash
# Issue an HTTP request to concert-web running on loopback.
#
# Usage: ./scripts/local-api-request.sh PORT PATH [METHOD [BODY_FILE]]
set -euo pipefail

readonly MIN_PORT=1
readonly MAX_PORT=65535
readonly MAX_PORT_DIGITS=${#MAX_PORT}

usage() {
  echo "usage: $0 PORT PATH [METHOD [BODY_FILE]]" >&2
  echo "       PATH must begin with /" >&2
  echo "       METHOD defaults to GET and must contain uppercase letters only" >&2
  echo "       BODY_FILE is optional and sent as application/json" >&2
  echo "       host fixed to 127.0.0.1" >&2
  echo "       stable approval prefix: ./scripts/local-api-request.sh" >&2
}

invalid_port() {
  echo "error: PORT must be an integer from $MIN_PORT through $MAX_PORT" >&2
  exit 2
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

if [ -n "$body_file" ]; then
  exec "$curl_bin" -fsS -X "$method" \
    -H "Content-Type: application/json" \
    --data-binary "@${body_file}" \
    -- "http://127.0.0.1:${port_number}${request_path}"
fi

exec "$curl_bin" -fsS -X "$method" -- "http://127.0.0.1:${port_number}${request_path}"
