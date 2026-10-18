#!/bin/sh
# Minimal NDJSON peer for client tests.
echo "fake sidecar starting" >&2
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"session.start"'*)
      printf '{"id":"s-%s","reply_to":"%s","type":"session.started","claude_session_id":"sess"}\n' "$id" "$id" ;;
    *'"type":"session.turn"'*)
      printf 'not json\n'
      printf '{"id":"r-%s","type":"raw.event","seq":1,"kind":"assistant","json":{"echo":true}}\n' "$id" ;;
    *'"type":"session.stop"'*)
      echo "stopping" >&2
      exit 3 ;;
  esac
done
