#!/bin/sh
# Builds Ludae and installs it for this user: ~/.local/bin, ~/.local/share/ludae/modules, a launcher entry.
# ./install.sh --uninstall removes the program (accounts in ~/.local/share/ludae are kept).
set -eu
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
SHARE="$HOME/.local/share"
if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/ludae" "$BIN/ludae-roblox" "$BIN/ludae-roblox-login" "$SHARE/applications/ludae.desktop"
    rm -rf "$SHARE/ludae/modules"
    echo "Ludae removed; accounts kept in $SHARE/ludae"
    exit 0
fi
cargo build --release
mkdir -p "$BIN" "$SHARE/ludae/modules" "$SHARE/applications"
install -m755 target/release/ludae target/release/ludae-roblox "$BIN/"
# Roblox's Log in window uses the system web view
if pkg-config --exists webkit2gtk-4.1; then
    cargo build --release -p ludae-roblox-login
    install -m755 target/release/ludae-roblox-login "$BIN/"
else
    echo "Roblox Log in needs webkit2gtk-4.1 (Arch: sudo pacman -S webkit2gtk-4.1); the rest works without it"
fi
for m in modules/*/; do
    name=$(basename "$m")
    case "$name" in *-windows) continue ;; esac
    mkdir -p "$SHARE/ludae/modules/$name"
    install -m644 "$m/module.toml" "$SHARE/ludae/modules/$name/module.toml"
done
cat > "$SHARE/applications/ludae.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=Ludae
Comment=Game accounts
Exec=$BIN/ludae
Icon=applications-games
Categories=Game;Utility;
StartupWMClass=ludae
DESK
echo "Ludae installed: $BIN/ludae"
