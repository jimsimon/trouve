#!/usr/bin/env bash
# Install (or refresh) the trouve desktop entry and icon for the local user.
#
# Wayland compositors (KDE, GNOME) resolve taskbar/titlebar icons through a
# desktop file whose name matches the window's app id ("trouve"), not
# through the in-window icon — so dev builds need this once per machine.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
icon_src="$repo_root/crates/trouve-app/ui/assets/trouve.png"

apps_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"

mkdir -p "$apps_dir" "$icon_dir/512x512/apps" "$icon_dir/256x256/apps" \
    "$icon_dir/128x128/apps" "$icon_dir/64x64/apps" "$icon_dir/48x48/apps" \
    "$icon_dir/32x32/apps"

cp "$here/trouve.desktop" "$apps_dir/trouve.desktop"

# Drop leftovers from the pre-rename ("trouve-app") install, if any.
rm -f "$apps_dir/trouve-app.desktop" "$icon_dir"/*/apps/trouve-app.png

for size in 512 256 128 64 48 32; do
    if command -v magick >/dev/null; then
        magick "$icon_src" -resize "${size}x${size}" \
            "$icon_dir/${size}x${size}/apps/trouve.png"
    else
        cp "$icon_src" "$icon_dir/${size}x${size}/apps/trouve.png"
    fi
done

command -v update-desktop-database >/dev/null && update-desktop-database "$apps_dir" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$icon_dir" || true

echo "installed: $apps_dir/trouve.desktop"
echo "installed: $icon_dir/{512,256,128,64,48,32}x*/apps/trouve.png"
echo "restart trouve (and possibly plasmashell) to see the new icon"
