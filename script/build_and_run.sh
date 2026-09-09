#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
case "$MODE" in
  run|--verify|--build-only|--debug|--logs|--telemetry) ;;
  *) echo "usage: $0 [--verify|--build-only|--debug|--logs|--telemetry]" >&2; exit 2 ;;
esac
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v node >/dev/null 2>&1; then
  export PATH="$HOME/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin:$PATH"
fi
if ! command -v pnpm >/dev/null 2>&1; then
  export PATH="$HOME/.cache/codex-runtimes/codex-primary-runtime/dependencies/bin/fallback:$PATH"
fi
APP_BUNDLE="$ROOT_DIR/dist/Rozu.app"
CONFIGURATION="${ROZU_BUILD_CONFIGURATION:-release}"

if [[ "$MODE" != "--build-only" ]]; then
  pkill -x Rozu >/dev/null 2>&1 || true
fi
if [[ "${ROZU_USE_BUILT_UI:-0}" != "1" ]]; then
  cd "$ROOT_DIR/desktop/ui"
  pnpm install --frozen-lockfile
  pnpm build
fi
if [[ ! -f "$ROOT_DIR/desktop/ui/dist/index.html" ]]; then
  echo "Missing desktop/ui/dist/index.html. Build the frontend before using ROZU_USE_BUILT_UI=1." >&2
  exit 1
fi
cd "$ROOT_DIR"
swift build --package-path desktop -c "$CONFIGURATION"
BUILD_DIR="$(swift build --package-path desktop -c "$CONFIGURATION" --show-bin-path)"

mkdir -p "$ROOT_DIR/dist"
STAGING="$(mktemp -d "$ROOT_DIR/dist/.rozu-stage.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
CONTENTS="$STAGING/Rozu.app/Contents"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources/UI"
cp "$BUILD_DIR/Rozu" "$CONTENTS/MacOS/Rozu"
chmod +x "$CONTENTS/MacOS/Rozu"
cp -R "$ROOT_DIR/desktop/ui/dist/." "$CONTENTS/Resources/UI/"
cp "$ROOT_DIR/LICENSE" "$CONTENTS/Resources/LICENSE"
cp "$ROOT_DIR/desktop/THIRD-PARTY-NOTICES.md" "$CONTENTS/Resources/THIRD-PARTY-NOTICES.md"
swift "$ROOT_DIR/script/generate_icon.swift" "$STAGING/Rozu.iconset"
/usr/bin/iconutil -c icns "$STAGING/Rozu.iconset" -o "$CONTENTS/Resources/Rozu.icns"
cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>Rozu</string>
  <key>CFBundleIdentifier</key><string>com.rozu.desktop</string>
  <key>CFBundleName</key><string>Rozu</string>
  <key>CFBundleDisplayName</key><string>Rozu</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.1</string>
  <key>CFBundleVersion</key><string>2</string>
  <key>CFBundleIconFile</key><string>Rozu</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Rozu contributors. MIT License.</string>
</dict></plist>
PLIST
cat > "$STAGING/entitlements.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.app-sandbox</key><true/>
  <key>com.apple.security.network.client</key><true/>
</dict></plist>
PLIST
/usr/bin/plutil -lint "$CONTENTS/Info.plist"
/usr/bin/codesign --force --sign - --entitlements "$STAGING/entitlements.plist" "$STAGING/Rozu.app"
/usr/bin/codesign --verify --deep --strict "$STAGING/Rozu.app"
rm -rf "$APP_BUNDLE"
mv "$STAGING/Rozu.app" "$APP_BUNDLE"
echo "Built $APP_BUNDLE (ad-hoc signed; not Apple-notarized)"

case "$MODE" in
  --build-only) ;;
  run) /usr/bin/open -n "$APP_BUNDLE" ;;
  --verify)
    /usr/bin/open -n "$APP_BUNDLE"
    sleep 2
    pgrep -x Rozu >/dev/null
    echo "Rozu process is running."
    ;;
  --debug)
    /usr/bin/open -n "$APP_BUNDLE"
    sleep 1
    /usr/bin/lldb -p "$(pgrep -x Rozu | head -n 1)"
    ;;
  --logs|--telemetry)
    /usr/bin/open -n "$APP_BUNDLE"
    /usr/bin/log stream --info --style compact --predicate 'process == "Rozu"'
    ;;
esac
