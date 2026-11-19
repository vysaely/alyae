#!/usr/bin/env bash
# Builds Mocktail with Alyae's patches (patches/*.patch, in order) at the upstream commit in
# patches/UPSTREAM_COMMIT, and installs it under ~/.local (bin/mocktail, lib/mocktail/).
# What was installed there before is backed up first.
#
# The patches:
#   01 fps-uncap               Roblox's Android build caps at 240 FPS; raise it to the FPS setting
#   02 input-polling           read input every 1 ms instead of 4 ms when frames are unthrottled
#   03 cpu-and-texture-loading less CPU between engine polls; faster texture conversion
#   04 shiftlock-icon          MOCKTAIL_SHIFTLOCK_CURSOR: a PNG in place of Roblox's shift-lock icon
#   05 chat-enter              the Enter that sends a chat message also reaches the game
#   06 shiftlock-zoom          the scroll wheel zooms in shift lock
#   07 browser-tracker-cookie  the account's device ID reaches Roblox with its sign-in
#   08 anti-afk                MOCKTAIL_ANTI_AFK_SECONDS: a 1 px nudge after that long without input
#   09 pointer-release         the right-button camera drag ends where it started, not at the window edge
#   10 low-memory-profile      MOCKTAIL_LOW_MEMORY_PROFILE=0: no phone-sized memory caps (Alyae's mains)
#   11 nudge-now               MOCKTAIL_NUDGE_FILE: writing that file asks for one nudge now (Anti-AFK)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCHES="$HERE/patches"
UPSTREAM_URL="https://github.com/komaruworld/mocktail.git"
UPSTREAM_COMMIT="$(tr -d '[:space:]' < "$PATCHES/UPSTREAM_COMMIT")"
PREFIX="$HOME/.local"
SRC="${ALYAE_MOCKTAIL_SRC:-$HOME/.cache/alyae/mocktail-src}"
BUILD="$SRC-build"
MARKER="$PREFIX/lib/mocktail/alyae-mocktail-patch"

deps() {
    echo "Install Mocktail's build dependencies, then run this again:"
    echo "  Arch: sudo pacman -S --needed base-devel cmake git ninja pkgconf lld sdl3 sdl3_ttf curl openssl \\"
    echo "        nlohmann-json libyaml libelf minizip capstone gtk4 libadwaita webkitgtk-6.0 libutf8proc \\"
    echo "        fontconfig libglvnd libplacebo vulkan-headers vulkan-icd-loader zlib"
    echo "  Others: the 'Build from source' section of ${UPSTREAM_URL%.git}"
}
for tool in git cmake ninja c++ pkg-config; do
    command -v "$tool" > /dev/null || { echo "'$tool' is missing."; deps; exit 1; }
done

echo "Mocktail ${UPSTREAM_COMMIT:0:7}: fetching"
[ -d "$SRC/.git" ] || { mkdir -p "$(dirname "$SRC")"; git clone --quiet "$UPSTREAM_URL" "$SRC"; }
git -C "$SRC" cat-file -e "$UPSTREAM_COMMIT^{commit}" 2> /dev/null || git -C "$SRC" fetch --quiet origin
# this checkout belongs to the script: earlier patching is discarded
git -C "$SRC" checkout --quiet --force "$UPSTREAM_COMMIT"
git -C "$SRC" clean --quiet -fdx
git -C "$SRC" submodule --quiet update --init --recursive
for p in "$PATCHES"/*.patch; do
    git -C "$SRC" apply --whitespace=nowarn "$p"
    echo "  applied $(basename "$p")"
done

echo "Building (a few minutes; log: $BUILD.log)"
if ! { cmake -S "$SRC" -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTING=OFF \
        -DCMAKE_INSTALL_PREFIX="$PREFIX" && cmake --build "$BUILD"; } > "$BUILD.log" 2>&1; then
    echo "The build failed. Last lines of $BUILD.log:"
    tail -n 15 "$BUILD.log"
    deps
    exit 1
fi

if pgrep -f '/mocktail(_alyae_|_acc_)?[^/]*$' > /dev/null 2>&1; then
    echo "Close every running Roblox first, then run this again (the build is kept)."
    exit 1
fi
if [ -e "$PREFIX/bin/mocktail" ] || [ -e "$PREFIX/lib/mocktail/mocktail" ]; then
    BACKUP="$PREFIX/lib/mocktail-backup/$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$BACKUP"
    [ -e "$PREFIX/bin/mocktail" ] && cp -a "$PREFIX/bin/mocktail" "$BACKUP/bin-mocktail"
    find "$PREFIX/lib/mocktail" -maxdepth 1 -type f ! -name 'mocktail_acc_*' ! -name 'mocktail_alyae_*' \
        -exec cp -a {} "$BACKUP/" \;
    echo "Previous Mocktail kept in $BACKUP"
    # a wrapper or link here would shadow the new program
    rm -f "$PREFIX/bin/mocktail"
fi
cmake --install "$BUILD" > /dev/null
echo "$UPSTREAM_COMMIT $(cat "$PATCHES"/*.patch | sha256sum | cut -c1-16)" > "$MARKER"
echo "Installed: $PREFIX/bin/mocktail"
