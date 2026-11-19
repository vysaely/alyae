Alyae for Linux (x86_64)

Install the WebKitGTK 4.1 runtime (used by the Roblox Log in and Open Browser windows):
  Arch:          sudo pacman -S webkit2gtk-4.1
  Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-0
  Fedora:        sudo dnf install webkit2gtk4.1

Run ./alyae from this folder (keep alyae-roblox, alyae-roblox-login and modules/ beside it).
Accounts and settings are kept in ~/.local/share/alyae. To keep them in ./data instead,
create an empty file named "portable" beside alyae.

To launch Roblox on Linux, Alyae uses Mocktail. Build and install Mocktail with Alyae's
patches into ~/.local by running:
  modules/roblox-linux/mocktail/build.sh
If Mocktail is already installed, Join Server works without running build.sh, but the
shift-lock cursor, Anti-AFK, a main's low-memory profile and the camera-drag cursor fix need
Alyae's build.
