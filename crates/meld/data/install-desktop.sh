#!/bin/sh
# Installs the desktop entry and dark icon for the Rust build into the
# user's data dir, so Wayland compositors can map the window's app id
# (org.gnome.Meld.Rust) to an icon.
#
# Usage: install-desktop.sh [path/to/meld]   (default: target/release/meld)
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
exec_path=$(realpath "${1:-$root/target/release/meld}")
data=${XDG_DATA_HOME:-$HOME/.local/share}
app_id=org.gnome.Meld.Rust

install -Dm644 "$root/data/icons/hicolor/scalable/apps/org.gnome.Meld-dark.svg" \
    "$data/icons/hicolor/scalable/apps/$app_id.svg"
mkdir -p "$data/applications"
sed "s|@exec@|$exec_path|" "$root/crates/meld/data/$app_id.desktop.in" \
    > "$data/applications/$app_id.desktop"

command -v gtk-update-icon-cache >/dev/null &&
    gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
command -v update-desktop-database >/dev/null &&
    update-desktop-database -q "$data/applications" || true

echo "Installed $app_id (Exec=$exec_path)"
