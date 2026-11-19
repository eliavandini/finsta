#!/bin/bash
# Package target/release/finsta (built with --no-default-features) as
# build/Finsta-macos-arm64.zip holding Finsta.app. Run on macOS with the
# Homebrew packages from the release workflow.
set -euo pipefail
cd "$(dirname "$0")/.."

brew="$(brew --prefix)"
app=build/Finsta.app
c="$app/Contents"
res="$c/Resources"
rm -rf "$app" build/Finsta-macos-arm64.zip
mkdir -p "$c/MacOS" "$c/Frameworks" "$res/lib/gstreamer-1.0" "$res/lib/gio/modules" "$res/share/glib-2.0" "$res/share/icons"
cp target/release/finsta "$c/MacOS/finsta-bin"

# Only the GStreamer plugins the player uses (Homebrew has all of them).
for p in coreelements playback typefindfunctions app videoconvertscale audioconvert audioresample \
         volume autodetect osxaudio isomp4 soup libav audioparsers videoparsersbad; do
    cp -L "$brew"/lib/gstreamer-1.0/libgst$p.* "$res/lib/gstreamer-1.0/"
done
cp -RL "$brew/lib/gdk-pixbuf-2.0" "$res/lib/"
cp -L "$brew"/lib/gio/modules/*.so "$res/lib/gio/modules/"
cp -RL "$brew/share/glib-2.0/schemas" "$res/share/glib-2.0/"
glib-compile-schemas "$res/share/glib-2.0/schemas"
cp -RL "$brew/share/icons/Adwaita" "$brew/share/icons/hicolor" "$res/share/icons/"
# Opened by GStreamer's souphttpsrc at run time.
cp -L "$brew/opt/libsoup/lib/libsoup-3.0.0.dylib" "$c/Frameworks/"

# The image loader list, with the bundle's place filled in at launch. Made
# from Homebrew's loaders (the copies only load once bundled); same names.
loaders="$brew/lib/gdk-pixbuf-2.0/2.10.0/loaders"
GDK_PIXBUF_MODULEDIR="$loaders" gdk-pixbuf-query-loaders | sed "s#$loaders#@LOADERS@#g" > "$res/loaders.cache.in"
grep -q '@LOADERS@.*svg' "$res/loaders.cache.in" || { echo "no SVG loader in the loader list" >&2; exit 1; }

# Copy every Homebrew library these need into Frameworks and point them there.
modules=()
while IFS= read -r f; do modules+=(-x "$f"); done < <(find "$res/lib" "$c/Frameworks" -name '*.so' -o -name '*.dylib')
# -s: where @rpath libraries are (the SVG loader's librsvg, for one). It asks on
# the terminal for any it can't find, so no input: fail instead of waiting.
dylibbundler -of -cd -b -s "$brew/lib" -d "$c/Frameworks" -p @executable_path/../Frameworks/ \
    -x "$c/MacOS/finsta-bin" "${modules[@]}" < /dev/null


cat > "$c/MacOS/Finsta" <<'LAUNCH'
#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
res="$here/../Resources"
export XDG_DATA_DIRS="$res/share"
export GSETTINGS_SCHEMA_DIR="$res/share/glib-2.0/schemas"
export GIO_MODULE_DIR="$res/lib/gio/modules"
export GST_PLUGIN_SYSTEM_PATH="$res/lib/gstreamer-1.0"
export GST_REGISTRY="$HOME/Library/Caches/io.github.eliavandini.Finsta/gstreamer-registry.bin"
export DYLD_FALLBACK_LIBRARY_PATH="$here/../Frameworks"
cache="${TMPDIR:-/tmp}/io.github.eliavandini.Finsta-loaders.cache"
sed "s#@LOADERS@#$res/lib/gdk-pixbuf-2.0/2.10.0/loaders#g" "$res/loaders.cache.in" > "$cache"
export GDK_PIXBUF_MODULE_FILE="$cache"
exec "$here/finsta-bin" "$@"
LAUNCH
chmod +x "$c/MacOS/Finsta"

# The icon, from the SVG.
iconset=build/Finsta.iconset
rm -rf "$iconset" && mkdir -p "$iconset"
for s in 16 32 128 256 512; do
    rsvg-convert -w $s -h $s data/icons/io.github.eliavandini.Finsta.svg -o "$iconset/icon_${s}x${s}.png"
    rsvg-convert -w $((s * 2)) -h $((s * 2)) data/icons/io.github.eliavandini.Finsta.svg -o "$iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$iconset" -o "$res/Finsta.icns"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
cat > "$c/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Finsta</string>
  <key>CFBundleDisplayName</key><string>Finsta</string>
  <key>CFBundleIdentifier</key><string>io.github.eliavandini.Finsta</string>
  <key>CFBundleExecutable</key><string>Finsta</string>
  <key>CFBundleIconFile</key><string>Finsta</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>$(sw_vers -productVersion | cut -d. -f1).0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Not notarized: an ad-hoc signature, enough to run on Apple silicon.
codesign --force --deep --sign - "$app"
ditto -c -k --keepParent "$app" build/Finsta-macos-arm64.zip
