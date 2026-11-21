#!/usr/bin/env bash
# Builds Mocktail with Alyae's patches (patches/*.patch, in order) at the upstream commit in
# patches/UPSTREAM_COMMIT, and installs it under ~/.local (bin/mocktail, lib/mocktail/).
# What was installed there before is backed up first.
#   build.sh            build, then install (refused while a Roblox runs; the build is kept)
#   build.sh --build    only build; the finished build is marked in its folder
#   build.sh --install  only install the marked build (Alyae runs this once no game runs)
# Missing build tools or packages: one line naming them, exit code 2.
#
# The patches:
#   01 fps-uncap               Roblox's Android build caps at 240 FPS; raise it to the FPS setting
#   03 cpu-and-texture-loading texture writes on up to 8 threads; challengeCompleted closes the
#                              challenge page; the login window gets 5 s to start, not 3
#   04 shiftlock-icon          MOCKTAIL_SHIFTLOCK_CURSOR: a PNG in place of Roblox's shift-lock icon
#   05 chat-enter              the Enter that sends a chat message also reaches the game
#   06 shiftlock-zoom          the scroll wheel zooms in shift lock
#   07 browser-tracker-cookie  the account's device ID reaches Roblox with its sign-in
#   08 anti-afk                MOCKTAIL_ANTI_AFK_SECONDS: a 1 px nudge after that long without input
#   09 pointer-release         the right-button camera drag ends where it started, not at the window edge
#   10 low-memory-profile      MOCKTAIL_LOW_MEMORY_PROFILE=0 (Alyae's mains): no phone settings (memory
#                              caps, mip bias, Luau GC caps, mesh compression); Roblox's defaults
#   11 nudge-now               MOCKTAIL_NUDGE_FILE: writing that file asks for one nudge now (Anti-AFK)
#   12 captcha-device-and-close the challenge window sends the game's device ID and closes 2 s
#                              after success, so Roblox reads the result and retries the join
#   13 fps-file                MOCKTAIL_FPS_FILE: a number written there (1-1000, 0 = 1000) becomes
#                              the running game's frame-rate limit, read once a second
#   14 challenge-frames        the challenge window lets a check build its inner frames from
#                              about:srcdoc (the press-and-hold check); nothing loads from the network
set -euo pipefail

MODE="${1:-all}"
case "$MODE" in
    all | --build | --install) ;;
    *) echo "usage: build.sh [--build | --install]"; exit 1 ;;
esac
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCHES="$HERE/patches"
UPSTREAM_URL="https://github.com/komaruworld/mocktail.git"
UPSTREAM_COMMIT="$(tr -d '[:space:]' < "$PATCHES/UPSTREAM_COMMIT")"
PREFIX="$HOME/.local"
SRC="${ALYAE_MOCKTAIL_SRC:-$HOME/.cache/alyae/mocktail-src}"
BUILD="$SRC-build"
MARKER="$PREFIX/lib/mocktail/alyae-mocktail-patch"

MARK="$UPSTREAM_COMMIT $(cat "$PATCHES"/*.patch | sha256sum | cut -c1-16)"
BUILT="$BUILD/alyae-mocktail-patch"
PKGS=(base-devel cmake git ninja pkgconf lld sdl3 sdl3_ttf curl openssl nlohmann-json libyaml libelf minizip
    capstone gtk4 libadwaita webkitgtk-6.0 libutf8proc fontconfig libglvnd libplacebo vulkan-headers
    vulkan-icd-loader zlib)

deps() {
    echo "Install Mocktail's build dependencies, then run this again:"
    echo "  Arch: sudo pacman -S --needed ${PKGS[*]}"
    echo "  Others: the 'Build from source' section of ${UPSTREAM_URL%.git}"
}

if [ "$MODE" != --install ]; then
    if command -v pacman > /dev/null; then
        # the packages hold the tools too; pacman -T prints the ones not installed (exit 127)
        mapfile -t missing < <(pacman -T "${PKGS[@]}" || true)
        if [ ${#missing[@]} -gt 0 ]; then
            echo "Missing build tools: ${missing[*]}. Install them: sudo pacman -S --needed ${missing[*]}"
            exit 2
        fi
    else
        missing=()
        for tool in git cmake ninja c++ pkg-config; do
            command -v "$tool" > /dev/null || missing+=("$tool")
        done
        if [ ${#missing[@]} -gt 0 ]; then
            echo "Missing build tools: ${missing[*]}."
            deps
            exit 2
        fi
    fi
fi

install_build() {
    if [ "$(cat "$BUILT" 2> /dev/null)" != "$MARK" ]; then
        echo "No finished build of these patches in $BUILD: run build.sh --build first."
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
    echo "$MARK" > "$MARKER"
    echo "Installed: $PREFIX/bin/mocktail"
}

if [ "$MODE" = --install ]; then
    install_build
    exit 0
fi

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
# a build that fails or is stopped is not marked finished
rm -f "$BUILT"
if ! { cmake -S "$SRC" -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTING=OFF \
        -DCMAKE_INSTALL_PREFIX="$PREFIX" && cmake --build "$BUILD"; } > "$BUILD.log" 2>&1; then
    echo "The build failed. Last lines of $BUILD.log:"
    tail -n 15 "$BUILD.log"
    deps
    exit 1
fi

echo "$MARK" > "$BUILT"
echo "Built: $BUILD"
[ "$MODE" = --build ] || install_build
