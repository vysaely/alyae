# Alyae

An account manager for Roblox and other games, on Linux and Windows. It keeps your accounts'
cookies, passwords and 2FA secrets encrypted, and starts several Roblox accounts at once. On Linux
each account runs in its own [Mocktail](https://github.com/komaruworld/mocktail).

![Alyae](assets/preview.png)

## Windows

Download [Alyae-portable.zip](https://github.com/vysaely/alyae/releases/tag/portable), unzip it
anywhere and run `alyae.exe`. Your accounts are kept in the `data` folder next to it.

You need the Roblox app installed. Windows may say the publisher is unknown: click
More info, then Run anyway.

## Linux

Download [Alyae-linux-x86_64.tar.gz](https://github.com/vysaely/alyae/releases/tag/portable),
unpack it and run `./alyae` (needs `webkit2gtk-4.1`; see `README-linux.txt` inside). Mocktail is
not in the archive: build it with `modules/roblox-linux/mocktail/build.sh` inside the folder.

Or build Alyae from source. Install the dependencies (Arch):

```bash
sudo pacman -S --needed rust webkit2gtk-4.1 base-devel git cmake ninja pkgconf lld sdl3 sdl3_ttf \
    curl openssl nlohmann-json libyaml libelf minizip capstone gtk4 libadwaita webkitgtk-6.0 \
    libutf8proc fontconfig libglvnd libplacebo vulkan-headers vulkan-icd-loader zlib
```

On other distros, install Rust, webkit2gtk-4.1 and the packages from Mocktail's
[Build from source](https://github.com/komaruworld/mocktail#build-from-source) section.

Then:

```bash
git clone https://github.com/vysaely/alyae.git
cd alyae
./install.sh
modules/roblox-linux/mocktail/build.sh
```

`build.sh` builds Mocktail with Alyae's patches. With a Mocktail installed otherwise Join Server
works, but the shift-lock cursor, Anti-AFK, a main's low-memory profile and the camera-drag
cursor fix need this build; Alyae says so when it starts a game on another one. To update, run
`git pull` and both scripts again.

To uninstall, run `./install.sh --uninstall`. Your accounts in `~/.local/share/alyae` are kept.

## Moving your accounts

**Accounts file** (bottom right) saves every account into one file locked with a password. Open it
on another PC with Add Account → From an accounts file.

## What Roblox sees

Every Roblox web call and the sign-in window say they are Chrome on Windows, on Linux too: one
User-Agent for every account, with the client-hint headers (`sec-ch-ua`) Chrome sends and your
system language as `Accept-Language`. A cookie is then made and used by the same "browser". The
Chrome version is one constant in `crates/alyae-roblox/src/browser.rs`, to raise as Chrome moves
on. What this cannot change: the connection itself (TLS and HTTP/2 details) is not Chrome's, and
the sign-in window is WebKitGTK on Linux and WebView2 on Windows underneath, whose own scripts
and client hints can still tell.

## License

[GPL-3.0](LICENSE). The patches in `modules/roblox-linux/mocktail/` change Mocktail, which is
Apache-2.0, and are Apache-2.0 as well.
