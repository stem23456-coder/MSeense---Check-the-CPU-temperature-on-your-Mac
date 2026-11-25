#!/usr/bin/env bash
set -euo pipefail

# Free local/open-source MSense.app builder.
# No Apple Developer account is required. The result uses an ad-hoc signature.
# Run this script from anywhere; keep it in the root of the Cargo project.

APP_NAME="${APP_NAME:-MSense}"
BUNDLE_ID="${BUNDLE_ID:-io.github.yourname.msense}"
VERSION="${VERSION:-0.3.2}"
BIN_NAME="msense-menu"
# Optional override. If empty, the builder auto-detects AppIcon.icns or a
# square 1024x1024 AppIcon.png next to Cargo.toml (and then in assets/).
ICON_SOURCE="${ICON_SOURCE:-}"

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_DIR="$SCRIPT_DIR"
DIST_DIR="$PROJECT_DIR/dist"
FINAL_APP="$DIST_DIR/$APP_NAME.app"
FINAL_ZIP="$DIST_DIR/$APP_NAME-$VERSION-macOS-arm64.zip"
FINAL_DMG="$DIST_DIR/$APP_NAME-$VERSION-macOS-arm64.dmg"
FINAL_DMG_ZIP="$DIST_DIR/$APP_NAME-$VERSION-macOS-arm64-dmg.zip"
RELEASE_DIR_NAME="$APP_NAME-$VERSION-macOS-arm64"

fail() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

install_app_icon() {
    local staged_app="$1"
    local stage_root="$2"
    local icon_source="$ICON_SOURCE"
    local candidate
    local iconset
    local width
    local height

    if [[ -z "$icon_source" ]]; then
        for candidate in \
            "$PROJECT_DIR/AppIcon.icns" \
            "$PROJECT_DIR/AppIcon.png" \
            "$PROJECT_DIR/assets/AppIcon.icns" \
            "$PROJECT_DIR/assets/AppIcon.png"
        do
            if [[ -f "$candidate" ]]; then
                icon_source="$candidate"
                break
            fi
        done
    fi

    if [[ -z "$icon_source" ]]; then
        printf 'No app icon found. Add AppIcon.icns or a 1024x1024 AppIcon.png next to Cargo.toml.\n'
        return 0
    fi
    [[ -f "$icon_source" ]] || fail "Icon file not found: $icon_source"

    case "$icon_source" in
        *.icns|*.ICNS)
            /usr/bin/install -m 644 \
                "$icon_source" "$staged_app/Contents/Resources/AppIcon.icns"
            ;;
        *.png|*.PNG)
            width="$(/usr/bin/sips -g pixelWidth "$icon_source" 2>/dev/null \
                | /usr/bin/awk '/pixelWidth:/{print $2}')"
            height="$(/usr/bin/sips -g pixelHeight "$icon_source" 2>/dev/null \
                | /usr/bin/awk '/pixelHeight:/{print $2}')"
            [[ "$width" =~ ^[0-9]+$ && "$height" =~ ^[0-9]+$ ]] \
                || fail "Could not read icon dimensions: $icon_source"
            [[ "$width" -eq "$height" ]] \
                || fail "AppIcon.png must be square; got ${width}x${height}."
            [[ "$width" -ge 1024 ]] \
                || fail "AppIcon.png must be at least 1024x1024; got ${width}x${height}."

            iconset="$stage_root/AppIcon.iconset"
            /bin/mkdir -p "$iconset"
            /usr/bin/sips -z 16 16 "$icon_source" --out "$iconset/icon_16x16.png" >/dev/null
            /usr/bin/sips -z 32 32 "$icon_source" --out "$iconset/icon_16x16@2x.png" >/dev/null
            /usr/bin/sips -z 32 32 "$icon_source" --out "$iconset/icon_32x32.png" >/dev/null
            /usr/bin/sips -z 64 64 "$icon_source" --out "$iconset/icon_32x32@2x.png" >/dev/null
            /usr/bin/sips -z 128 128 "$icon_source" --out "$iconset/icon_128x128.png" >/dev/null
            /usr/bin/sips -z 256 256 "$icon_source" --out "$iconset/icon_128x128@2x.png" >/dev/null
            /usr/bin/sips -z 256 256 "$icon_source" --out "$iconset/icon_256x256.png" >/dev/null
            /usr/bin/sips -z 512 512 "$icon_source" --out "$iconset/icon_256x256@2x.png" >/dev/null
            /usr/bin/sips -z 512 512 "$icon_source" --out "$iconset/icon_512x512.png" >/dev/null
            /usr/bin/sips -z 1024 1024 "$icon_source" --out "$iconset/icon_512x512@2x.png" >/dev/null
            /usr/bin/iconutil -c icns "$iconset" \
                -o "$staged_app/Contents/Resources/AppIcon.icns"
            ;;
        *)
            fail "Unsupported icon format: $icon_source (use .icns or .png)."
            ;;
    esac

    APP_ICON_INSTALLED=1
    printf 'App icon: %s\n' "$icon_source"
}

bundle_executable_name() {
    local app_path="$1"
    local plist="$app_path/Contents/Info.plist"

    [[ -f "$plist" ]] || fail "Info.plist not found: $plist"
    /usr/bin/plutil -lint "$plist" >/dev/null
    /usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist" 2>/dev/null \
        || fail "CFBundleExecutable is missing from $plist"
}

inspect_app() {
    local app_path="$1"
    local plist="$app_path/Contents/Info.plist"
    local executable_name=""
    local executable=""
    local report="${TMPDIR:-/tmp}/MSense-inspection.txt"

    [[ "$(/usr/bin/uname -s)" == "Darwin" ]] || fail "Run inspection on macOS."
    [[ -d "$app_path" ]] || fail "App not found: $app_path"

    # Inspection is deliberately read-only. In particular, do not clear the
    # quarantine attribute or re-sign before collecting this report.
    set +e
    {
        printf 'MSense downloaded-copy inspection\n'
        printf '==================================\n'
        /usr/bin/date
        /usr/bin/sw_vers
        /usr/bin/uname -a
        printf '\nAPP: %s\n' "$app_path"

        printf '\n== Bundle metadata ==\n'
        /usr/bin/plutil -lint "$plist"
        /usr/bin/plutil -p "$plist"
        executable_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist" 2>/dev/null)"
        executable="$app_path/Contents/MacOS/$executable_name"
        printf 'Resolved executable: %s\n' "$executable"

        printf '\n== Files and permissions ==\n'
        /bin/ls -ldeO@ "$app_path" "$app_path/Contents" "$app_path/Contents/MacOS" "$executable"
        /usr/bin/file "$executable"
        /usr/bin/otool -L "$executable"

        printf '\n== Extended attributes (quarantine evidence) ==\n'
        /usr/bin/xattr -lr "$app_path"

        printf '\n== Code signature verification ==\n'
        /usr/bin/codesign --verify --deep --strict --verbose=4 "$app_path"
        printf 'codesign verify exit: %s\n' "$?"
        /usr/bin/codesign --display --verbose=4 "$app_path"

        printf '\n== Gatekeeper assessment ==\n'
        /usr/sbin/spctl --assess --type execute --verbose=4 "$app_path"
        printf 'spctl assessment exit: %s\n' "$?"

        printf '\n== Finder-style launch attempt ==\n'
        /usr/bin/open -n "$app_path"
        printf 'open exit: %s\n' "$?"
        /bin/sleep 3
        /usr/bin/pgrep -fl "$executable_name"

        printf '\n== Relevant macOS launch/security log ==\n'
        /usr/bin/log show --last 2m --style compact \
            --predicate '(process == "syspolicyd") OR (process == "amfid") OR (process == "runningboardd") OR (process == "launchservicesd")' \
            | /usr/bin/tail -n 120
    } 2>&1 | /usr/bin/tee "$report"
    set -e

    printf '\nRead-only report saved to: %s\n' "$report"
    printf 'Send the complete report; do not run --repair or --diagnose on this copy first.\n'
}

repair_app() {
    local app_path="$1"
    local executable_name
    local executable

    [[ -d "$app_path" ]] || fail "App not found: $app_path"
    executable_name="$(bundle_executable_name "$app_path")"
    executable="$app_path/Contents/MacOS/$executable_name"
    [[ -f "$executable" ]] || fail "Executable not found: $executable"

    /bin/chmod 755 "$executable"
    /usr/bin/xattr -dr com.apple.quarantine "$app_path" 2>/dev/null || true
    /usr/bin/codesign --force --deep --sign - "$app_path"
    /usr/bin/codesign --verify --deep --strict --verbose=2 "$app_path"

    printf '\nRepaired: %s\n' "$app_path"
    printf 'Executable: '
    /usr/bin/file "$executable"
    /usr/bin/open "$app_path"
}

diagnose_app() {
    local app_path="$1"
    local plist="$app_path/Contents/Info.plist"
    local executable_name
    local executable
    local file_description
    local launch_log="${TMPDIR:-/tmp}/MSense-direct-launch.log"
    local launch_pid
    local launch_status
    local lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

    [[ "$(/usr/bin/uname -s)" == "Darwin" ]] || fail "Run diagnostics on macOS."
    [[ -d "$app_path" ]] || fail "App not found: $app_path"

    printf '== Bundle ==\n'
    printf 'Path: %s\n' "$app_path"
    /usr/bin/plutil -lint "$plist"
    executable_name="$(bundle_executable_name "$app_path")"
    executable="$app_path/Contents/MacOS/$executable_name"
    printf 'CFBundleExecutable: %s\n' "$executable_name"
    [[ -f "$executable" ]] || fail "Info.plist points to a missing file: $executable"

    printf '\n== Executable ==\n'
    /bin/ls -l "$executable"
    file_description="$(/usr/bin/file "$executable")"
    printf '%s\n' "$file_description"
    case "$file_description" in
        *Mach-O*arm64*) ;;
        *) fail "This is not an arm64 Mach-O executable. Build the app on an Apple Silicon Mac." ;;
    esac
    /usr/bin/otool -L "$executable"

    printf '\n== Repairing this copy ==\n'
    /bin/chmod 755 "$executable"
    /usr/bin/xattr -dr com.apple.quarantine "$app_path" 2>/dev/null || true
    /usr/bin/codesign --force --deep --sign - "$app_path"
    /usr/bin/codesign --verify --deep --strict --verbose=4 "$app_path"
    printf 'Executable permission, quarantine and ad-hoc signature: OK\n'

    printf '\n== Direct launch (the useful test) ==\n'
    : > "$launch_log"
    "$executable" >"$launch_log" 2>&1 &
    launch_pid=$!
    /bin/sleep 4

    if /bin/kill -0 "$launch_pid" 2>/dev/null; then
        printf 'The executable stayed alive for 4 seconds: the binary itself works.\n'
        /bin/kill "$launch_pid" 2>/dev/null || true
        wait "$launch_pid" 2>/dev/null || true

        /usr/bin/touch "$app_path"
        if [[ -x "$lsregister" ]]; then
            "$lsregister" -f "$app_path" >/dev/null 2>&1 || true
        fi
        printf 'Launch Services registration refreshed. Opening the app bundle…\n'
        /usr/bin/open -n "$app_path"
        printf 'If the menu-bar icon appeared, MSense.app is repaired.\n'
        exit 0
    fi

    set +e
    wait "$launch_pid"
    launch_status=$?
    set -e
    printf 'The executable exited with status %s. This is the real failure:\n' "$launch_status"
    if [[ -s "$launch_log" ]]; then
        /bin/cat "$launch_log"
    else
        printf '(The program produced no terminal output.)\n'
    fi
    printf '\nDiagnostic log: %s\n' "$launch_log"
    fail "The Rust program exits during startup; this is not a Gatekeeper prompt."
}

if [[ "${1:-}" == "--repair" ]]; then
    [[ $# -eq 2 ]] || fail "Usage: $0 --repair /path/to/MSense.app"
    repair_app "$2"
    exit 0
fi

if [[ "${1:-}" == "--inspect" ]]; then
    [[ $# -eq 2 ]] || fail "Usage: $0 --inspect /path/to/downloaded/MSense.app"
    inspect_app "$2"
    exit 0
fi

if [[ "${1:-}" == "--diagnose" ]]; then
    [[ $# -eq 2 ]] || fail "Usage: $0 --diagnose /path/to/MSense.app"
    diagnose_app "$2"
    exit 0
fi

[[ $# -eq 0 ]] || fail "Usage: $0  OR  $0 --inspect /path/to/downloaded/MSense.app  OR  $0 --repair /path/to/MSense.app  OR  $0 --diagnose /path/to/MSense.app"
[[ "$(/usr/bin/uname -s)" == "Darwin" ]] || fail "Run this script on macOS."
[[ "$(/usr/bin/uname -m)" == "arm64" ]] || fail "MSense requires an Apple Silicon Mac."
[[ -f "$PROJECT_DIR/Cargo.toml" ]] || fail "Put this script next to Cargo.toml."
command -v cargo >/dev/null 2>&1 || fail "cargo is not installed or not in PATH."

printf 'Building release binary…\n'
cd "$PROJECT_DIR"
cargo build --locked --release --bin "$BIN_NAME"

SOURCE_BIN="$PROJECT_DIR/target/release/$BIN_NAME"
[[ -f "$SOURCE_BIN" ]] || fail "Cargo did not create $SOURCE_BIN"
/bin/chmod 755 "$SOURCE_BIN"

/bin/mkdir -p "$DIST_DIR"
STAGE_ROOT="$(/usr/bin/mktemp -d "$DIST_DIR/.msense-build.XXXXXX")"
STAGED_APP="$STAGE_ROOT/$APP_NAME.app"
trap '/bin/rm -rf "$STAGE_ROOT"' EXIT

/bin/mkdir -p "$STAGED_APP/Contents/MacOS" "$STAGED_APP/Contents/Resources"
/usr/bin/install -m 755 "$SOURCE_BIN" "$STAGED_APP/Contents/MacOS/$BIN_NAME"
APP_ICON_INSTALLED=0
install_app_icon "$STAGED_APP" "$STAGE_ROOT"

PLIST="$STAGED_APP/Contents/Info.plist"
/usr/bin/plutil -create xml1 "$PLIST"
/usr/bin/plutil -insert CFBundleDevelopmentRegion -string en "$PLIST"
/usr/bin/plutil -insert CFBundleDisplayName -string "$APP_NAME" "$PLIST"
/usr/bin/plutil -insert CFBundleExecutable -string "$BIN_NAME" "$PLIST"
/usr/bin/plutil -insert CFBundleIdentifier -string "$BUNDLE_ID" "$PLIST"
/usr/bin/plutil -insert CFBundleInfoDictionaryVersion -string 6.0 "$PLIST"
/usr/bin/plutil -insert CFBundleName -string "$APP_NAME" "$PLIST"
/usr/bin/plutil -insert CFBundlePackageType -string APPL "$PLIST"
/usr/bin/plutil -insert CFBundleShortVersionString -string "$VERSION" "$PLIST"
/usr/bin/plutil -insert CFBundleVersion -string 5 "$PLIST"
if [[ "$APP_ICON_INSTALLED" -eq 1 ]]; then
    /usr/bin/plutil -insert CFBundleIconFile -string AppIcon "$PLIST"
fi
/usr/bin/plutil -insert LSUIElement -bool YES "$PLIST"
/usr/bin/plutil -insert NSHighResolutionCapable -bool YES "$PLIST"
/usr/bin/plutil -lint "$PLIST"

# Clear accidental quarantine metadata before signing. Sign last: changing any
# file inside the app after this point invalidates its signature.
/usr/bin/xattr -cr "$STAGED_APP" 2>/dev/null || true
/usr/bin/codesign --force --deep --sign - "$STAGED_APP"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$STAGED_APP"

[[ -x "$STAGED_APP/Contents/MacOS/$BIN_NAME" ]] || fail "Executable bit is missing."

# Preserve an older build instead of deleting it.
if [[ -e "$FINAL_APP" ]]; then
    BACKUP_APP="$DIST_DIR/$APP_NAME-backup-$(/bin/date +%Y%m%d-%H%M%S).app"
    /bin/mv "$FINAL_APP" "$BACKUP_APP"
    printf 'Previous build moved to: %s\n' "$BACKUP_APP"
fi
/bin/mv "$STAGED_APP" "$FINAL_APP"

if [[ -e "$FINAL_ZIP" ]]; then
    BACKUP_ZIP="$FINAL_ZIP.backup-$(/bin/date +%Y%m%d-%H%M%S)"
    /bin/mv "$FINAL_ZIP" "$BACKUP_ZIP"
fi

# Build a release folder, not a loose .app. A downloaded archive is quarantined
# by macOS. Without paid Developer ID notarization that cannot be prevented by
# the publisher, so the package also contains a small, auditable local installer.
RELEASE_ROOT="$STAGE_ROOT/$RELEASE_DIR_NAME"
/bin/mkdir -p "$RELEASE_ROOT"
/usr/bin/ditto "$FINAL_APP" "$RELEASE_ROOT/$APP_NAME.app"

/bin/cat > "$RELEASE_ROOT/install-msense.sh" <<'INSTALLER'
#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
SOURCE_APP="$(/usr/bin/find "$SCRIPT_DIR" -maxdepth 1 -type d -name '*.app' -print -quit)"
[[ -n "$SOURCE_APP" ]] || fail "No .app was found next to this installer."

APP_BASENAME="$(/usr/bin/basename "$SOURCE_APP")"
PLIST="$SOURCE_APP/Contents/Info.plist"
[[ -f "$PLIST" ]] || fail "Info.plist is missing from $APP_BASENAME."
EXECUTABLE_NAME="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST" 2>/dev/null)" \
    || fail "CFBundleExecutable is missing from Info.plist."
SOURCE_EXECUTABLE="$SOURCE_APP/Contents/MacOS/$EXECUTABLE_NAME"
[[ -f "$SOURCE_EXECUTABLE" ]] || fail "The app executable is missing."

[[ "$(/usr/bin/uname -s)" == "Darwin" ]] || fail "This installer requires macOS."
[[ "$(/usr/bin/uname -m)" == "arm64" ]] || fail "This MSense build requires an Apple Silicon Mac."

printf 'Verifying the downloaded package…\n'
if [[ -f "$SCRIPT_DIR/SHA256SUMS" ]]; then
    (cd "$SCRIPT_DIR" && /usr/bin/shasum -a 256 -c SHA256SUMS)
fi
/usr/bin/codesign --verify --deep --strict --verbose=2 "$SOURCE_APP" \
    || fail "The app changed after it was packaged. Download the release again."

TARGET_ROOT="$HOME/Applications"
TARGET_APP="$TARGET_ROOT/$APP_BASENAME"
/bin/mkdir -p "$TARGET_ROOT"
INSTALL_STAGE="$(/usr/bin/mktemp -d "$TARGET_ROOT/.msense-install.XXXXXX")"
STAGED_APP="$INSTALL_STAGE/$APP_BASENAME"
trap '/bin/rm -rf "$INSTALL_STAGE"' EXIT

/usr/bin/ditto "$SOURCE_APP" "$STAGED_APP"
/bin/chmod 755 "$STAGED_APP/Contents/MacOS/$EXECUTABLE_NAME"
/usr/bin/xattr -dr com.apple.quarantine "$STAGED_APP" 2>/dev/null || true
/usr/bin/codesign --force --deep --sign - "$STAGED_APP"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$STAGED_APP"

if [[ -e "$TARGET_APP" ]]; then
    BACKUP_APP="$TARGET_ROOT/${APP_BASENAME%.app}-backup-$(/bin/date +%Y%m%d-%H%M%S).app"
    /bin/mv "$TARGET_APP" "$BACKUP_APP"
    printf 'Previous version saved as: %s\n' "$BACKUP_APP"
fi
/bin/mv "$STAGED_APP" "$TARGET_APP"

LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
if [[ -x "$LSREGISTER" ]]; then
    "$LSREGISTER" -f "$TARGET_APP" >/dev/null 2>&1 || true
fi

printf 'Installed: %s\n' "$TARGET_APP"
/usr/bin/open -n "$TARGET_APP"
printf 'MSense is running in the menu bar.\n'
INSTALLER
/bin/chmod 755 "$RELEASE_ROOT/install-msense.sh"

/bin/cat > "$RELEASE_ROOT/README.txt" <<'README'
MSense for Apple Silicon
========================

DOWNLOAD NOTE
Send users a GitHub Release link and have them download this package in Safari,
Chrome, or Firefox. Do not send the release itself as a Telegram attachment:
Telegram for macOS can apply a hard App Sandbox quarantine that has no
Open Anyway button for an unnotarized app.

NORMAL OPENING
1. Move MSense.app to Applications and try opening it once.
2. If macOS blocks it, open System Settings > Privacy & Security and choose
   Open Anyway. Apple normally shows that button for about one hour.

RELIABLE FREE INSTALL (NO APPLE DEVELOPER ACCOUNT)
1. Open Terminal.
2. Type: bash followed by one space.
3. Drag install-msense.sh from this folder into Terminal and press Return.

The installer first verifies the binary and its package signature. It then
installs MSense only for your account in ~/Applications, clears quarantine only
from that verified local copy, applies an ad-hoc signature, and opens it. It
does not disable Gatekeeper or SIP.

РУССКАЯ ИНСТРУКЦИЯ
1. Сначала перенесите MSense.app в «Программы» и попробуйте открыть.
2. Если macOS блокирует запуск, откройте «Системные настройки» >
   «Конфиденциальность и безопасность» > «Открыть всё равно».
3. Если кнопка не появилась: откройте Терминал, напишите bash и один пробел,
   перетащите install-msense.sh в окно Терминала и нажмите Return.
README

(cd "$RELEASE_ROOT" && \
    /usr/bin/shasum -a 256 "$APP_NAME.app/Contents/MacOS/$BIN_NAME" > SHA256SUMS)
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$RELEASE_ROOT" "$FINAL_ZIP"

# A disk image is the recommended no-Terminal release. Unlike an extracted ZIP,
# it keeps the app bundle, Unix permissions and code signature in a filesystem
# image while it is being uploaded, downloaded and forwarded by messengers.
if [[ -e "$FINAL_DMG" ]]; then
    BACKUP_DMG="$FINAL_DMG.backup-$(/bin/date +%Y%m%d-%H%M%S)"
    /bin/mv "$FINAL_DMG" "$BACKUP_DMG"
fi
DMG_ROOT="$STAGE_ROOT/dmg"
/bin/mkdir -p "$DMG_ROOT"
/usr/bin/ditto "$FINAL_APP" "$DMG_ROOT/$APP_NAME.app"
/bin/ln -s /Applications "$DMG_ROOT/Applications"
/bin/cat > "$DMG_ROOT/README.txt" <<'DMG_README'
MSense — installation without Terminal
======================================

IMPORTANT: distribute this DMG through GitHub Releases and download it in a web
browser. For Telegram, send the outer *-dmg.zip produced by the MSense builder,
not this bare DMG. The extra ZIP layer avoids Telegram applying its hard App
Sandbox quarantine directly to the disk image contents.

1. Drag MSense.app onto the Applications shortcut in this window.
2. Open Applications and double-click MSense.
3. If macOS blocks the first launch, immediately open System Settings >
   Privacy & Security, scroll to Security and click Open Anyway for MSense.
4. Confirm Open. This is a one-time action.

MSense is a menu-bar application. It does not show a Dock icon or a main window.

MSense — установка без Терминала
===============================

ВАЖНО: для GitHub Releases используйте этот DMG. Для отправки вложением в
Telegram используйте созданный сборщиком внешний архив *-dmg.zip, внутри
которого лежит DMG. Не отправляйте голый DMG: Telegram может поставить ему
жёсткий sandbox-карантин без кнопки «Открыть всё равно».

1. Перетащите MSense.app на ярлык Applications в этом окне.
2. Откройте «Программы» и дважды нажмите MSense.
3. Если macOS заблокирует первый запуск, сразу откройте «Системные настройки» >
   «Конфиденциальность и безопасность», прокрутите до раздела «Безопасность»
   и нажмите «Открыть всё равно» для MSense.
4. Подтвердите открытие. Это потребуется только один раз.

MSense работает в строке меню, поэтому значка в Dock и главного окна нет.
DMG_README
/usr/bin/codesign --verify --deep --strict --verbose=2 "$DMG_ROOT/$APP_NAME.app"
/usr/bin/hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$DMG_ROOT" \
    -ov \
    -format UDZO \
    "$FINAL_DMG"

if [[ -e "$FINAL_DMG_ZIP" ]]; then
    BACKUP_DMG_ZIP="$FINAL_DMG_ZIP.backup-$(/bin/date +%Y%m%d-%H%M%S)"
    /bin/mv "$FINAL_DMG_ZIP" "$BACKUP_DMG_ZIP"
fi
/usr/bin/zip -q -j "$FINAL_DMG_ZIP" "$FINAL_DMG"

printf '\nBuilt successfully:\n  %s\n  %s\n  %s\n  %s\n' \
    "$FINAL_APP" "$FINAL_ZIP" "$FINAL_DMG" "$FINAL_DMG_ZIP"
printf '\nGitHub Releases / browser download:\n  %s\n' "$FINAL_DMG"
printf '\nTelegram attachment (ZIP containing the DMG):\n  %s\n' "$FINAL_DMG_ZIP"
if [[ "${OPEN_AFTER_BUILD:-1}" == "1" ]]; then
    printf '\nOpening the app…\n'
    if ! /usr/bin/open "$FINAL_APP"; then
        printf 'Finder could not launch it. Run this to see the real error:\n'
        printf '  "%s/Contents/MacOS/%s"\n' "$FINAL_APP" "$BIN_NAME"
    fi
fi

printf '\nIf a downloaded copy is blocked on another Mac, the owner can run:\n'
printf '  bash "/path/to/%s/install-msense.sh"\n' "$RELEASE_DIR_NAME"
printf '\nOr try opening once, then use System Settings > Privacy & Security > Open Anyway.\n'
