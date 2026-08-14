#!/bin/bash
#
# Build and install EMBR.app — the bundle that gives .embr files their icon in
# Finder and makes double-clicking one extract it.
#
# macOS will not associate an icon with a bare file extension. The icon has to
# be exported by an installed application that declares the type via a Uniform
# Type Identifier. That is the only reason this app exists; the extraction
# behaviour comes along nearly free once the bundle is there.
#
# Deliberately NOT a self-extracting executable. Those fail on a recipient's
# machine under Gatekeeper and notarization, which is exactly when an archive
# most needs to work.
#
#   ./macos/build-app.sh                 # install to /Applications
#   ./macos/build-app.sh ~/Applications  # or somewhere else
#
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${1:-/Applications}"
APP="$DEST/EMBR.app"
BUNDLE_ID="xyz.embr.opener"
UTI="xyz.embr.archive"

say() { printf '\033[1m==>\033[0m %s\n' "$1"; }

# --- the binary the app will carry ----------------------------------------
BIN="$REPO/target/release/embr"
if [ ! -x "$BIN" ]; then
    say "building embr (release)"
    ( cd "$REPO" && cargo build --release )
fi

# --- icon -----------------------------------------------------------------
ICONSET="$REPO/assets/embr.iconset"
if [ ! -d "$ICONSET" ]; then
    say "generating iconset"
    python3 "$REPO/assets/logo-candidates/make_logos.py" >/dev/null
fi
say "building embr.icns"
iconutil -c icns "$ICONSET" -o "$REPO/assets/embr.icns"

# --- compile the droplet --------------------------------------------------
say "compiling EMBR.app"
# A previously installed copy may still be running — and if an older build hung
# (a cross-app AppleEvent waiting on a permission grant will do it), macOS
# treats the app as already launched and re-activates the stale process instead
# of running the new code. The reinstall then looks like it did nothing.
pkill -9 -f "EMBR.app/Contents/MacOS/droplet" 2>/dev/null || true
rm -rf "$APP"
mkdir -p "$DEST"
osacompile -o "$APP" "$REPO/macos/embr-opener.applescript"

# --- resources ------------------------------------------------------------
cp "$REPO/assets/embr.icns" "$APP/Contents/Resources/embr.icns"
cp "$BIN" "$APP/Contents/Resources/embr"
chmod +x "$APP/Contents/Resources/embr"

# osacompile ships its generic droplet icon two ways: a loose .icns and an
# asset catalog referenced by CFBundleIconName. Modern macOS prefers the
# catalog over CFBundleIconFile, so leaving it here silently overrides our
# icon — the app and every .embr file keep the stock artwork. Both have to go.
rm -f "$APP/Contents/Resources/applet.icns" \
      "$APP/Contents/Resources/droplet.icns" \
      "$APP/Contents/Resources/Assets.car"

# --- Info.plist -----------------------------------------------------------
# UTExportedTypeDeclarations is what teaches macOS that .embr is a real type
# and which icon belongs to it. CFBundleDocumentTypes is what makes this app
# the handler for double-clicks. Both are needed; neither works alone.
PLIST="$APP/Contents/Info.plist"
P=/usr/libexec/PlistBuddy

# osacompile's plist has only a handful of keys, so every write has to cope
# with the entry being absent. Add first, fall back to Set if it is already there.
pset() { # pset <key> <type> <value>
    $P -c "Add :$1 $2 $3" "$PLIST" 2>/dev/null || $P -c "Set :$1 $3" "$PLIST"
}

say "declaring the .embr file type"
# Must go before CFBundleIconFile can take effect; see the rm above.
$P -c "Delete :CFBundleIconName" "$PLIST" 2>/dev/null || true
pset CFBundleIdentifier          string "$BUNDLE_ID"
pset CFBundleName                string EMBR
pset CFBundleDisplayName         string EMBR
pset CFBundleIconFile            string embr
pset CFBundleShortVersionString  string 0.1.0
pset CFBundleVersion             string 0.1.0
pset NSHumanReadableCopyright    string "MIT"

# Exported type declaration: this is OUR format, so we export rather than import.
$P -c "Delete :UTExportedTypeDeclarations"            "$PLIST" 2>/dev/null || true
$P -c "Add :UTExportedTypeDeclarations array"         "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0 dict"        "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeIdentifier string $UTI" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeDescription string 'EMBR Archive'" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeIconFile string embr" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeConformsTo array" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeConformsTo:0 string public.data" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeConformsTo:1 string public.archive" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification dict" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension array" "$PLIST"
$P -c "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:0 string embr" "$PLIST"

# Document type: makes this app the owner of the type, so double-click works.
$P -c "Delete :CFBundleDocumentTypes"                 "$PLIST" 2>/dev/null || true
$P -c "Add :CFBundleDocumentTypes array"              "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0 dict"             "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0:CFBundleTypeName string 'EMBR Archive'" "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0:CFBundleTypeRole string Editor" "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0:CFBundleTypeIconFile string embr" "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0:LSHandlerRank string Owner" "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0:LSItemContentTypes array" "$PLIST"
$P -c "Add :CFBundleDocumentTypes:0:LSItemContentTypes:0 string $UTI" "$PLIST"

# --- re-sign --------------------------------------------------------------
# osacompile ad-hoc signs the bundle, and every PlistBuddy write above breaks
# that seal. macOS ignores type declarations coming from a bundle whose
# signature does not verify, so this has to happen after the plist is final —
# without it the app registers as a handler but .embr keeps a generic icon.
say "re-signing the bundle"
codesign --force --deep --sign - "$APP"
codesign --verify --deep "$APP" && echo "    signature ok"

# --- register -------------------------------------------------------------
# Touch the bundle so Launch Services notices it changed, then register.
touch "$APP"
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
say "registering with Launch Services"
"$LSREG" -f "$APP"

# Finder caches icons aggressively, and Icon Services keeps its own store on
# top of that. Both have to be poked or the change shows up only after a logout.
say "refreshing icon caches"
"$LSREG" -kill -r -domain local -domain system -domain user >/dev/null 2>&1 || true
killall iconservicesagent 2>/dev/null || true
killall Dock 2>/dev/null || true
killall Finder 2>/dev/null || true

# --- prime the type declaration -------------------------------------------
# macOS marks an app's exported type declarations "untrusted" until the app has
# been launched at least once, and it ignores the icon of an untrusted
# declaration. Verified via `lsregister -dump`: the xyz.embr.archive entry
# flips from "untrusted" to "trusted" the first time the bundle runs. So run it
# once and quit it.
say "priming the type declaration"
open -a "$APP" 2>/dev/null || true
sleep 2
osascript -e 'tell application "EMBR" to quit' 2>/dev/null || true
"$LSREG" -f "$APP"

# Test for "untrusted", not "trusted" — the latter is a substring of the
# former and would match either way.
if "$LSREG" -dump 2>/dev/null | grep -A3 "type id: *$UTI" | grep -q "untrusted"; then
    echo "    warning: type declaration still untrusted; the icon will not show"
else
    echo "    type declaration is trusted"
fi

# --- Finder Quick Action --------------------------------------------------
say "installing the Compress with EMBR quick action"
python3 "$REPO/macos/install-quick-action.py" | sed 's/^/    /'

say "installed $APP"
echo
echo "  .embr files should now show the flame icon in Finder."
echo "  Double-clicking one extracts it into a folder beside it."
echo "  Right-click any file or folder -> Quick Actions -> Compress with EMBR."
echo
echo "  If the icon has not appeared yet, log out and back in — Finder's icon"
echo "  cache is the slowest part of this and nothing else forces it."
