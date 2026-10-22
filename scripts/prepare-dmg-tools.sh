#!/bin/sh
# Run before importing signing credentials. Packaging itself never uses the network.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TOOLS=${DMG_TOOLS_DIR:-${BUILD_DIR:-"$ROOT/build"}/dmg-tools}
if [ "$(uname -s)" != Darwin ]; then
    echo "DMG tools require macOS." >&2
    exit 1
fi
# dmgbuild needs Python 3.10+; macOS's own python3 is older, so PYTHON can
# name another, e.g. PYTHON=python3.13.
PYTHON=${PYTHON:-python3}
if ! "$PYTHON" -c 'import sys; sys.exit(sys.version_info < (3, 10))'; then
    echo "DMG tools need Python 3.10 or newer; set PYTHON, e.g. PYTHON=python3.13." >&2
    exit 1
fi
"$PYTHON" -m venv "$TOOLS"
"$TOOLS/bin/python3" -m pip --isolated install --disable-pip-version-check \
    --require-hashes --only-binary=:all: -r "$ROOT/scripts/dmg-requirements.txt"
