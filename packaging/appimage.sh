#!/bin/bash
# Package target/release/finsta (built with --no-default-features) as
# build/Finsta-x86_64.AppImage. Run on Debian 13 or newer (see the release
# workflow for the packages it needs).
set -euo pipefail
cd "$(dirname "$0")/.."

tools="$PWD/build/tools"
mkdir -p "$tools"
fetch() {
    [[ -e "$tools/$2" ]] || curl -fsSL -o "$tools/$2" "$1"
    chmod +x "$tools/$2"
}
fetch https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage linuxdeploy-x86_64.AppImage
fetch https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage linuxdeploy-plugin-appimage-x86_64.AppImage
fetch https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh linuxdeploy-plugin-gtk.sh
fetch https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gstreamer/master/linuxdeploy-plugin-gstreamer.sh linuxdeploy-plugin-gstreamer.sh
# No FUSE in containers.
export PATH="$tools:$PATH" APPIMAGE_EXTRACT_AND_RUN=1 DEPLOY_GTK_VERSION=4

appdir=build/AppDir
rm -rf "$appdir" build/Finsta-x86_64.AppImage
lib="/usr/lib/$(uname -m)-linux-gnu"
mkdir -p "$appdir/usr/lib/gio/modules" "$appdir/usr/share/icons" "$appdir/apprun-hooks"
# HTTPS for videos (GStreamer) is a GIO module, loaded at run time.
cp "$lib"/gio/modules/libgiognutls.so "$appdir/usr/lib/gio/modules/"
# Icons the app uses, for desktops without the Adwaita theme.
cp -r /usr/share/icons/Adwaita "$appdir/usr/share/icons/"
# Runs after the GTK plugin's hook (alphabetical order).
cat > "$appdir/apprun-hooks/zz-finsta.sh" <<'HOOK'
export GIO_MODULE_DIR="$APPDIR/usr/lib/gio/modules"
# The GTK plugin forces X11 and a theme; libadwaita styles itself and runs on Wayland.
unset GDK_BACKEND GTK_THEME
HOOK

# libsoup is opened by GStreamer's souphttpsrc at run time, so name it.
OUTPUT=Finsta-x86_64.AppImage linuxdeploy-x86_64.AppImage \
    --appdir "$appdir" \
    --executable target/release/finsta \
    --desktop-file data/io.github.eliavandini.Finsta.desktop \
    --icon-file data/icons/io.github.eliavandini.Finsta.svg \
    --library "$lib/libsoup-3.0.so.0" \
    --deploy-deps-only "$appdir/usr/lib/gio/modules" \
    --plugin gtk --plugin gstreamer \
    --output appimage
mv Finsta-x86_64.AppImage build/
