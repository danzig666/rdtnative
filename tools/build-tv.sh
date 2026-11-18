#!/bin/sh
# Cross-build for the TV from Linux/macOS: needs `rustup target add armv7-unknown-linux-gnueabi`
# and `python3 -m pip install ziglang==0.13.0`. Usage: tools/build-tv.sh [cargo build args...]
set -e
root=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER="$root/tools/zig-cc-armv7.sh"
export CC_armv7_unknown_linux_gnueabi="$root/tools/zig-cc-armv7.sh"
export AR_armv7_unknown_linux_gnueabi="$root/tools/zig-ar.sh"
exec cargo build --release --target armv7-unknown-linux-gnueabi "$@"
