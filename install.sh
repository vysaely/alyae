#!/bin/sh
# Builds Alyae and installs it for this user: ~/.local/bin, ~/.local/share/alyae/modules, a launcher entry.
# ./install.sh --uninstall removes the program (accounts in ~/.local/share/alyae are kept).
set -eu
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
SHARE="$HOME/.local/share"
if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/alyae" "$BIN/alyae-roblox" "$BIN/alyae-roblox-login" "$SHARE/applications/alyae.desktop"
    rm -rf "$SHARE/alyae/modules"
    echo "Alyae removed; accounts kept in $SHARE/alyae"
    exit 0
fi
cargo build --release
mkdir -p "$BIN" "$SHARE/alyae/modules" "$SHARE/applications"
install -m755 target/release/alyae target/release/alyae-roblox "$BIN/"
# Roblox's Log in window uses the system web view
if pkg-config --exists webkit2gtk-4.1; then
    cargo build --release -p alyae-roblox-login
    install -m755 target/release/alyae-roblox-login "$BIN/"
else
    echo "Roblox Log in needs webkit2gtk-4.1 (Arch: sudo pacman -S webkit2gtk-4.1); the rest works without it"
fi
for m in modules/*/; do
    name=$(basename "$m")
    case "$name" in *-windows) continue ;; esac
    mkdir -p "$SHARE/alyae/modules/$name"
    install -m644 "$m/module.toml" "$SHARE/alyae/modules/$name/module.toml"
done
cat > "$SHARE/applications/alyae.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=Alyae
Comment=Game accounts
Exec=$BIN/alyae
Icon=applications-games
Categories=Game;Utility;
StartupWMClass=alyae
DESK
echo "Alyae installed: $BIN/alyae"
