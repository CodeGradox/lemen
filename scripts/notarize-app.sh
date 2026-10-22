#!/bin/sh
# Explicit release operation: imports supplied credentials and contacts Apple.
set -eu
cd "$(dirname "$0")/.."
: "${RUNNER_TEMP:?Run on an ephemeral release runner}"
: "${BUILD_DIR:?}"
: "${LEMEN_CERTIFICATE_BASE64:?}"
: "${LEMEN_CERTIFICATE_PASSWORD:?}"
: "${LEMEN_SIGNING_IDENTITY:?}"
: "${LEMEN_NOTARY_KEY_BASE64:?}"
: "${LEMEN_NOTARY_KEY_ID:?}"
: "${LEMEN_NOTARY_ISSUER_ID:?}"
case "$LEMEN_SIGNING_IDENTITY" in
    "Developer ID Application:"*) ;;
    *) echo "An Apple-issued Developer ID Application identity is required." >&2; exit 1 ;;
esac

umask 077
credentials="$(mktemp -d "$RUNNER_TEMP/lemur-signing.XXXXXX")"
LEMEN_SIGNING_KEYCHAIN="$credentials/release.keychain-db"
export LEMEN_SIGNING_KEYCHAIN
set_keychain_search_list() {
    # Keep paths as separate arguments, including spaces, quotes and Unicode.
    # Extra arguments are prepended; no arguments restores the saved list.
    python3 - "$credentials/keychain-search-list.json" "$@" <<'PY'
import json, subprocess, sys
from pathlib import Path
original = json.loads(Path(sys.argv[1]).read_text())
subprocess.run(
    ["security", "list-keychains", "-d", "user", "-s", *sys.argv[2:], *original],
    check=True,
)
PY
}
cleanup() {
    if [ -f "$credentials/keychain-search-list.json" ]; then
        set_keychain_search_list >/dev/null 2>&1 ||
            printf 'Warning: could not restore the runner keychain search list.\n' >&2
    fi
    security delete-keychain "$LEMEN_SIGNING_KEYCHAIN" >/dev/null 2>&1 || true
    rm -rf "$credentials"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
export LEMEN_CREDENTIALS_DIRECTORY="$credentials"
python3 - <<'PY'
import base64, json, os, shlex, subprocess
from pathlib import Path
directory = Path(os.environ["LEMEN_CREDENTIALS_DIRECTORY"])
for variable, filename in [
    ("LEMEN_CERTIFICATE_BASE64", "certificate.p12"),
    ("LEMEN_NOTARY_KEY_BASE64", "notary.p8"),
]:
    # A pasted secret commonly ends in a newline, or is wrapped. Whitespace is not data, but
    # anything else outside the alphabet still aborts the run.
    encoded = "".join(os.environ[variable].split())
    (directory / filename).write_bytes(base64.b64decode(encoded, validate=True))
# Snapshot before create-keychain, which may itself change the search list.
# Do not create the snapshot file if inspecting the original list fails.
original = shlex.split(subprocess.check_output(
    ["security", "list-keychains", "-d", "user"], text=True,
))
(directory / "keychain-search-list.json").write_text(json.dumps(original))
PY
keychain_password="$(openssl rand -hex 32)"
security create-keychain -p "$keychain_password" "$LEMEN_SIGNING_KEYCHAIN"
security set-keychain-settings -lut 21600 "$LEMEN_SIGNING_KEYCHAIN"
security unlock-keychain -p "$keychain_password" "$LEMEN_SIGNING_KEYCHAIN"
security import "$credentials/certificate.p12" -k "$LEMEN_SIGNING_KEYCHAIN" \
    -P "$LEMEN_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
    -k "$keychain_password" "$LEMEN_SIGNING_KEYCHAIN" >/dev/null
# codesign needs the identity's keychain on the user search list even when an
# explicit --keychain limits certificate selection. Keep existing chain sources.
set_keychain_search_list "$LEMEN_SIGNING_KEYCHAIN"
# The credential directory and imported files are already private (0700/0600).
# Public app/image contents must remain readable when installed by another user.
umask 022
CONFIGURATION=release sh scripts/build-app.sh
app="$BUILD_DIR/lemur.app"
codesign --verify --deep --strict --verbose=2 "$app"
codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'Authority=Developer ID Application:'
codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'runtime'
codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'Timestamp='
# Submit the final distribution once; Apple also checks the signed app inside.
dmg="$BUILD_DIR/lemur.dmg"
sh scripts/build-dmg.sh "$app" "$dmg"
codesign --force --sign "$LEMEN_SIGNING_IDENTITY" --keychain "$LEMEN_SIGNING_KEYCHAIN" \
    --timestamp "$dmg"
codesign --verify --strict --verbose=2 "$dmg"
codesign -dv --verbose=4 "$dmg" 2>&1 | grep -F 'Authority=Developer ID Application:'
codesign -dv --verbose=4 "$dmg" 2>&1 | grep -F 'Timestamp='
xcrun notarytool submit "$dmg" \
    --key "$credentials/notary.p8" --key-id "$LEMEN_NOTARY_KEY_ID" \
    --issuer "$LEMEN_NOTARY_ISSUER_ID" --wait --output-format json > "$credentials/result.json"
python3 - "$credentials/result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1]))
print("Notarization:", result.get("id"), result.get("status"))
if result.get("status") != "Accepted":
    raise SystemExit("Apple did not accept this submission; no release artifact will be uploaded.")
PY
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
