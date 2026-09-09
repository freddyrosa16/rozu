#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$ROOT_DIR/script/build_and_run.sh" --build-only
APP_BUNDLE="$ROOT_DIR/dist/Rozu.app"
ARCHITECTURE="$(/usr/bin/lipo -archs "$APP_BUNDLE/Contents/MacOS/Rozu")"
case "$ARCHITECTURE" in
  arm64|x86_64) ;;
  *) echo "Unexpected app architecture: $ARCHITECTURE" >&2; exit 1 ;;
esac
ARCHIVE="$ROOT_DIR/dist/Rozu-0.1.1-macos-$ARCHITECTURE.zip"
/usr/bin/codesign --verify --deep --strict "$APP_BUNDLE"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" "$ARCHIVE"
/usr/bin/unzip -t -q "$ARCHIVE"
cd "$ROOT_DIR/dist"
/usr/bin/shasum -a 256 "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256"
/usr/bin/shasum -a 256 -c "$(basename "$ARCHIVE").sha256"
echo "Packaged $ARCHIVE"
cat "$(basename "$ARCHIVE").sha256"
echo "This preview is ad-hoc signed and is not Apple-notarized."
