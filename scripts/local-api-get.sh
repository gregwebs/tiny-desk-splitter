#!/usr/bin/env bash
# Issue a GET request to a concert-web API running on loopback.
#
# Usage: ./scripts/local-api-get.sh PORT API_PATH
set -euo pipefail

readonly MIN_PORT=1
readonly MAX_PORT=65535
readonly MAX_PORT_DIGITS=${#MAX_PORT}

usage() {
  echo "usage: $0 PORT API_PATH" >&2
  echo "       API_PATH must be /api or begin with /api/" >&2
  echo "       GET only; host fixed to 127.0.0.1" >&2
  echo "       stable approval prefix: ./scripts/local-api-get.sh" >&2
}

invalid_port() {
  echo "error: PORT must be an integer from $MIN_PORT through $MAX_PORT" >&2
  exit 2
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

if [ "$#" -ne 2 ]; then
  usage
  exit 2
fi

port="$1"
api_path="$2"

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

case "$api_path" in
  /api|/api/*)
    ;;
  *)
    echo "error: API_PATH must be /api or begin with /api/" >&2
    exit 2
    ;;
esac

case "$api_path" in
  *[[:space:]]*|*[[:cntrl:]]*)
    echo "error: API_PATH must not contain whitespace or control characters" >&2
    exit 2
    ;;
esac

if [ -x /opt/homebrew/opt/curl/bin/curl ]; then
  curl_bin=/opt/homebrew/opt/curl/bin/curl
else
  curl_bin=curl
fi

exec "$curl_bin" -fsS -- "http://127.0.0.1:${port_number}${api_path}"
