#!/usr/bin/env bash
# Build and install Finsta.
#
#   ./install.sh                 install for the current user (~/.local)
#   ./install.sh --system        install for all users (/usr/local, uses sudo)
#   ./install.sh --prefix DIR    install into DIR/bin, DIR/share/...
#   ./install.sh --no-webkit     build without the embedded sign-in browser
#   ./install.sh --uninstall     remove an installation (same prefix options)
#
# DESTDIR is honoured for packaging. Your Instagram session in
# ~/.config/finsta is never touched, not even by --uninstall.

set -euo pipefail

APP_ID="io.github.eliavandini.Finsta"
SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

prefix="$HOME/.local"
webkit=1
uninstall=0
system=0

usage() {
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --system) system=1; prefix="/usr/local" ;;
        --prefix) [[ $# -ge 2 ]] || { echo "--prefix needs a directory" >&2; exit 1; }
                  prefix="$2"; shift ;;
        --prefix=*) prefix="${1#--prefix=}" ;;
        --no-webkit) webkit=0 ;;
        --uninstall) uninstall=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift
done

prefix="${prefix%/}"
dest="${DESTDIR:-}"
bindir="$prefix/bin"
appsdir="$prefix/share/applications"
icondir="$prefix/share/icons/hicolor"
files=(
    "$bindir/finsta"
    "$appsdir/$APP_ID.desktop"
    "$icondir/scalable/apps/$APP_ID.svg"
)

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Use sudo only for the install steps, and only if the prefix isn't writable.
SUDO=""
needs_root() {
    local dir="$dest$prefix"
    while [[ ! -e "$dir" ]]; do dir="$(dirname "$dir")"; done
    [[ ! -w "$dir" ]]
}
if needs_root; then
    [[ $EUID -eq 0 ]] || SUDO="sudo"
    if [[ -n "$SUDO" ]] && ! command -v sudo >/dev/null; then
        die "$prefix is not writable and sudo is not available"
    fi
fi
if [[ $EUID -eq 0 && $system -eq 0 && -z "$dest" && "$prefix" == "$HOME/.local" ]]; then
    warn "running as root installs into root's home; use --system for a system-wide install"
fi

refresh_caches() {
    [[ -n "$dest" ]] && return 0 # packagers run these on the target system
    if command -v update-desktop-database >/dev/null; then
        $SUDO update-desktop-database -q "$appsdir" 2>/dev/null || true
    fi
    if [[ -f "$icondir/index.theme" ]]; then
        if command -v gtk4-update-icon-cache >/dev/null; then
            $SUDO gtk4-update-icon-cache -q -t -f "$icondir" 2>/dev/null || true
        elif command -v gtk-update-icon-cache >/dev/null; then
            $SUDO gtk-update-icon-cache -q -t -f "$icondir" 2>/dev/null || true
        fi
    fi
}

if [[ $uninstall -eq 1 ]]; then
    bold "Removing Finsta from $dest$prefix"
    for f in "${files[@]}"; do
        if [[ -e "$dest$f" ]]; then
            $SUDO rm -f "$dest$f"
            echo "  removed $dest$f"
        fi
    done
    refresh_caches
    echo "Your session in ~/.config/finsta was left in place; delete it to sign out completely."
    exit 0
fi

# ----- Dependencies ---------------------------------------------------------

distro_hint() {
    local id=""
    if [[ -r /etc/os-release ]]; then
        # shellcheck disable=SC1091
        id="$(. /etc/os-release; echo "${ID:-} ${ID_LIKE:-}")"
    fi
    case " $id " in
        *" arch "*)
            echo "sudo pacman -S --needed base-devel rustup gtk4 libadwaita webkitgtk-6.0 gstreamer gst-plugins-good gst-libav gst-plugin-gtk4" ;;
        *" debian "*|*" ubuntu "*)
            echo "sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libgstreamer1.0-dev gstreamer1.0-plugins-good gstreamer1.0-libav gstreamer1.0-gtk4" ;;
        *" fedora "*)
            echo "sudo dnf install gcc pkgconf gtk4-devel libadwaita-devel webkitgtk6.0-devel gstreamer1-devel gstreamer1-plugins-good gstreamer1-plugin-libav gstreamer1-plugin-gtk4" ;;
        *" opensuse"*|*" suse "*)
            echo "sudo zypper install gcc pkgconf gtk4-devel libadwaita-devel webkit2gtk4-devel gstreamer-devel gstreamer-plugins-good gstreamer-plugins-libav gstreamer-plugins-rs" ;;
        *) echo "" ;;
    esac
}

if ! command -v cargo >/dev/null; then
    die "cargo was not found. Install Rust from https://rustup.rs (or your distribution's rustup package) and run this again."
fi
if ! command -v pkg-config >/dev/null; then
    die "pkg-config was not found. Install it (and a C compiler) and run this again."
fi

missing=()
for lib in "gtk4 >= 4.18" "libadwaita-1 >= 1.6" "gstreamer-1.0 >= 1.20"; do
    pkg-config --exists "$lib" || missing+=("$lib")
done
if [[ $webkit -eq 1 ]] && ! pkg-config --exists "webkitgtk-6.0"; then
    missing+=("webkitgtk-6.0")
fi
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "Missing development libraries: ${missing[*]}" >&2
    hint="$(distro_hint)"
    [[ -n "$hint" ]] && echo "Install them with:  $hint" >&2
    if [[ " ${missing[*]} " == *" webkitgtk-6.0 "* && ${#missing[@]} -eq 1 ]]; then
        echo "Or build without the embedded sign-in browser:  $0 --no-webkit" >&2
    fi
    exit 1
fi

# Videos play through GStreamer; they need HTTP and H.264 support at runtime.
if command -v gst-inspect-1.0 >/dev/null; then
    gst_has() { gst-inspect-1.0 "$1" >/dev/null 2>&1; }
    if ! gst_has souphttpsrc; then
        warn "GStreamer's souphttpsrc is missing (gst-plugins-good); videos won't play"
    fi
    if ! { gst_has avdec_h264 || gst_has openh264dec || gst_has vah264dec || gst_has nvh264dec || gst_has v4l2slh264dec; }; then
        warn "no GStreamer H.264 decoder found (e.g. gst-libav); videos won't play"
    fi
    if ! gst_has gtk4paintablesink; then
        warn "GStreamer's GTK 4 sink is missing (gst-plugin-gtk4); stories show videos as still images"
    fi
else
    warn "GStreamer tools not found; videos may not play"
fi

# ----- Build ----------------------------------------------------------------

bold "Building Finsta (release)"
# Run cargo from the source tree so rustup picks up rust-toolchain.toml.
cd "$SRC_DIR"
cargo_args=(build --release --locked)
[[ $webkit -eq 0 ]] && cargo_args+=(--no-default-features)
cargo "${cargo_args[@]}"

target_dir="$(cargo metadata --format-version 1 --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
binary="${target_dir:-$SRC_DIR/target}/release/finsta"
[[ -x "$binary" ]] || die "build finished but $binary was not found"

# ----- Install --------------------------------------------------------------

bold "Installing into $dest$prefix"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Point the launcher at the installed binary so it works even when $bindir
# isn't on the desktop session's PATH.
sed "s|^Exec=.*|Exec=$bindir/finsta|" "$SRC_DIR/data/$APP_ID.desktop" > "$tmp/$APP_ID.desktop"

$SUDO install -Dm755 "$binary" "$dest$bindir/finsta"
$SUDO install -Dm644 "$tmp/$APP_ID.desktop" "$dest$appsdir/$APP_ID.desktop"
$SUDO install -Dm644 "$SRC_DIR/data/icons/$APP_ID.svg" "$dest$icondir/scalable/apps/$APP_ID.svg"
for f in "${files[@]}"; do
    echo "  $dest$f"
done
refresh_caches

echo
bold "Finsta is installed."
echo "Start it from your app launcher, or run: $bindir/finsta"
if [[ -z "$dest" ]] && [[ ":$PATH:" != *":$bindir:"* ]]; then
    echo "Note: $bindir is not on your PATH, so typing 'finsta' alone won't work in a terminal."
fi
echo "To remove it later: $0 --uninstall$([[ $system -eq 1 ]] && echo " --system")$([[ $system -eq 0 && "$prefix" != "$HOME/.local" ]] && echo " --prefix $prefix")"
