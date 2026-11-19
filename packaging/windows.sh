#!/bin/bash
# Package target/release/finsta.exe (built with --no-default-features) as
# build/Finsta-windows-x86_64.zip: a folder with finsta.exe and everything
# it needs. Run in an MSYS2 UCRT64 shell (see the release workflow).
set -euo pipefail
cd "$(dirname "$0")/.."

prefix=/ucrt64
out=build/Finsta
rm -rf "$out" build/Finsta-windows-x86_64.zip
mkdir -p "$out/lib/gio/modules" "$out/share/glib-2.0" "$out/share/icons" "$out/libexec/gstreamer-1.0"
cp target/release/finsta.exe "$out/"
# GLib, GTK and GStreamer find these next to their DLLs (lib/, share/, libexec/).
cp -r "$prefix/lib/gstreamer-1.0" "$prefix/lib/gdk-pixbuf-2.0" "$out/lib/"
find "$out/lib" -name '*.a' -delete
cp "$prefix"/lib/gio/modules/*.dll "$out/lib/gio/modules/"
cp "$prefix"/libexec/gstreamer-1.0/gst-plugin-scanner.exe "$out/libexec/gstreamer-1.0/"
cp -r "$prefix/share/glib-2.0/schemas" "$out/share/glib-2.0/"
glib-compile-schemas "$out/share/glib-2.0/schemas"
cp -r "$prefix/share/icons/Adwaita" "$prefix/share/icons/hicolor" "$out/share/icons/"
# Loader paths relative to the folder, wherever it's unpacked.
sed -i -E 's#"[^"]*(lib[/\\]+gdk-pixbuf-2\.0)#"\1#' "$out"/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache
# Opened by GStreamer's souphttpsrc at run time.
cp "$prefix/bin/libsoup-3.0-0.dll" "$out/"
# Every DLL from MSYS2 that any of these need, next to finsta.exe.
find "$out" \( -name '*.exe' -o -name '*.dll' \) -exec ldd {} + \
    | grep -o "$prefix/bin/[^ ]*\.dll" | sort -u | xargs -I{} cp -n {} "$out/"
(cd build && zip -qr Finsta-windows-x86_64.zip Finsta)
