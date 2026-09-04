#!/bin/sh
#
# Registers this copy of obs-rs with the desktop, so it has a name and an icon
# in the launcher and in the task bar.
#
# This is not how the application finds its own files — it runs perfectly well
# without ever being installed. It exists because of one thing Wayland does not
# do: a client cannot set its own window icon there. The protocol has no call
# for it, and `winit`'s Wayland backend is an empty function accordingly. A
# compositor finds the icon by matching the surface's `app_id` against an
# installed desktop entry instead, which is what this writes.
#
# X11 does not need it — the icon is set directly on the window — but the entry
# is worth having there too, for the launcher.
#
# Everything goes under $HOME. Nothing needs root, and nothing is written
# outside the two directories named below.

set -eu

APP_ID=obs-rs
PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_DIR="$PREFIX/applications"
ICON_DIR="$PREFIX/icons/hicolor/256x256/apps"
DESKTOP_FILE="$DESKTOP_DIR/$APP_ID.desktop"
ICON_FILE="$ICON_DIR/$APP_ID.png"

# The directory this script is in, which is the directory the archive was
# unpacked into. Resolved rather than assumed, so it works whether it was run
# as ./install-desktop-entry.sh or by its full path from somewhere else.
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

usage() {
    echo "usage: $0 [--uninstall]" >&2
    exit 2
}

uninstall() {
    rm -f "$DESKTOP_FILE" "$ICON_FILE"
    echo "removed $DESKTOP_FILE"
    echo "removed $ICON_FILE"
    refresh
    exit 0
}

# Both are best-effort: most desktops read these directories directly, and the
# ones that keep a cache are the reason to try. A missing tool is not a failure.
refresh() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -f -t "$PREFIX/icons/hicolor" >/dev/null 2>&1 || true
    fi
}

case "${1:-}" in
    --uninstall) uninstall ;;
    "") ;;
    *) usage ;;
esac

[ -x "$HERE/obs-rs" ] || {
    echo "no obs-rs executable beside this script (looked in $HERE)" >&2
    exit 1
}
[ -f "$HERE/obs-rs.png" ] || {
    echo "no obs-rs.png beside this script (looked in $HERE)" >&2
    exit 1
}

mkdir -p "$DESKTOP_DIR" "$ICON_DIR"
cp "$HERE/obs-rs.png" "$ICON_FILE"

# `StartupWMClass` is what an X11 task bar matches a window to this entry by;
# Wayland matches the surface's `app_id`. The application sets that itself —
# see `with_app_id` in `main.rs`, which it has to do explicitly, since nothing
# derives it from the application's name. Both strings are `obs-rs`, so one
# entry serves both.
cat > "$DESKTOP_FILE" <<DESKTOP
[Desktop Entry]
Type=Application
Version=1.0
Name=obs-rs
GenericName=Screen Recorder
Comment=Scene compositor and screen recorder
Exec=$HERE/obs-rs
Icon=$APP_ID
Terminal=false
Categories=AudioVideo;Video;Recorder;
StartupWMClass=$APP_ID
DESKTOP

chmod 644 "$DESKTOP_FILE" "$ICON_FILE"
refresh

echo "installed $DESKTOP_FILE"
echo "installed $ICON_FILE"
echo
echo "Moving this folder breaks the entry — it records where the executable is."
echo "Run this again from the new location, or $0 --uninstall to remove it."
