#!/bin/sh
set -eu

# Keep the six distributed brain databases bounded without coupling cleanup to
# the HTTP server process. The GC exits quietly when NEON_DB_* is not set.
/usr/local/bin/storage-gc &
GC_PID=$!

cleanup() {
  kill "$GC_PID" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

exec /usr/local/bin/realssa-engine
