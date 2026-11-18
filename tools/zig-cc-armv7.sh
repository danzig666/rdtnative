#!/bin/sh
# Linux/macOS twin of zig-cc-armv7.cmd (see zigcc.py).
exec python3 "$(dirname "$0")/zigcc.py" "$@"
