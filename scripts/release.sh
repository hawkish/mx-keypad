#!/bin/bash
# Build, sign, notarize and publish a release of mx-keypad.
#
#   scripts/release.sh dist      universal binary, Developer ID signed, zipped into dist/
#   scripts/release.sh notarize  dist, then submit the zip to Apple and wait
#   scripts/release.sh publish   notarize, then tag v$VERSION and create the GitHub release
#
# Runs on a Mac with Xcode, Homebrew rustup (both Apple targets) and a
# Developer ID Application identity in the keychain. Notarizing reads the
# keychain profile "mx-keypad", stored once with:
#
#   xcrun notarytool store-credentials mx-keypad --apple-id <id> --team-id <team>
#
# The Nix dev shell points xcrun at its own SDK and links against Nix's
# libiconv, so the build runs with a clean environment: the result loads only
# system libraries and runs on any Mac from MACOSX_DEPLOYMENT_TARGET up.

set -euo pipefail

STEP=${1:-dist}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

if [[ -z ${MX_KEYPAD_CLEAN_ENV:-} ]]; then
    exec env -i \
        HOME="$HOME" USER="${USER:-}" TERM="${TERM:-dumb}" \
        PATH="/opt/homebrew/opt/rustup/bin:/usr/bin:/bin:/usr/sbin:/sbin:/etc/profiles/per-user/${USER:-}/bin:/opt/homebrew/bin" \
        MX_KEYPAD_CLEAN_ENV=1 \
        "$0" "$@"
fi

LABEL=dk.mortenhogh.mx-keypad
export MACOSX_DEPLOYMENT_TARGET=12.0
VERSION=$(awk -F'"' '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^version/{print $2; exit}' Cargo.toml)
TAG=v$VERSION
DIST=dist
BIN=$DIST/mx-keypad
ZIP=$DIST/mx-keypad-$TAG-macos-universal.zip

die() { echo "release: $*" >&2; exit 1; }

identity() {
    security find-identity -v -p codesigning \
        | awk -F'"' '/Developer ID Application:/{split($1, a, " "); print a[2]; exit}'
}

dist() {
    local id
    id=$(identity)
    [[ -n $id ]] || die "no Developer ID Application identity in the keychain"
    rm -rf "$DIST" && mkdir -p "$DIST"
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release --locked --target "$target"
    done
    lipo -create -output "$BIN" \
        target/aarch64-apple-darwin/release/mx-keypad \
        target/x86_64-apple-darwin/release/mx-keypad
    # Anything outside the system would be missing on another Mac.
    if otool -L "$BIN" | grep -E '^\s' | grep -vE '^\s+(/System/Library/|/usr/lib/)'; then
        die "$BIN links a library outside the system"
    fi
    codesign --force --options runtime --timestamp \
        --identifier "$LABEL" --sign "$id" "$BIN"
    codesign --verify --strict --verbose=2 "$BIN"
    local stage=$DIST/mx-keypad-$TAG
    mkdir -p "$stage"
    cp "$BIN" README.md LICENSE-MIT LICENSE-APACHE NOTICE "$stage"/
    ditto -c -k --norsrc --noextattr --noqtn --keepParent "$stage" "$ZIP"
    shasum -a 256 "$ZIP" | tee "$ZIP.sha256"
    lipo -archs "$BIN"
}

notarize() {
    # A bare executable cannot be stapled; Gatekeeper checks the ticket
    # online the first time the downloaded binary runs.
    xcrun notarytool submit "$ZIP" --keychain-profile mx-keypad --wait
}

publish() {
    [[ -z $(git status --porcelain) ]] || die "the working tree is not clean"
    git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "$TAG already exists"
    git tag -a "$TAG" -m "mx-keypad $TAG"
    git push origin "$TAG"
    gh release create "$TAG" "$ZIP" "$ZIP.sha256" --title "mx-keypad $TAG" --generate-notes
}

case $STEP in
    dist) dist ;;
    notarize) dist && notarize ;;
    publish) dist && notarize && publish ;;
    *) die "unknown step $STEP (dist, notarize, publish)" ;;
esac
