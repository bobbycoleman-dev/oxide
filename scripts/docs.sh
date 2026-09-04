#!/bin/bash
# Serve the docs site locally and open it in the browser.
#
#   scripts/docs.sh            serve on http://localhost:8000 and open /docs/
#   PORT=9000 scripts/docs.sh  pick a port
#   NO_OPEN=1 scripts/docs.sh  just serve, don't open a browser
#
# The pages use root-absolute links (/docs/docs.css, /icon.png), so the
# docs/ folder is served as the site root, exactly as it deploys.
set -euo pipefail

cd "$(dirname "$0")/../docs"
PORT="${PORT:-8000}"
URL="http://localhost:$PORT/docs/"

if ! command -v python3 >/dev/null; then
    echo "python3 is required (it ships with the Xcode Command Line Tools)" >&2
    exit 1
fi

if [ "${NO_OPEN:-0}" != "1" ]; then
    # Give the server a moment to bind before the browser asks for the page.
    (sleep 0.5 && open "$URL") &
fi

echo "serving docs at $URL — ctrl-c to stop"
exec python3 -m http.server "$PORT" --bind 127.0.0.1
