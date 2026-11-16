# Alyae

Game account manager for Linux and Windows, AEAM's successor. Run several Roblox accounts at once
(on Linux each in its own [Mocktail](https://github.com/komaruworld/mocktail)), or keep any game's accounts.

- Encrypted vault: passwords, `.ROBLOSECURITY` cookies, 2FA codes
- Join by Place ID / Job ID or follow a user; many accounts at once
- Add Account signs in on Roblox's page; or import cookies, or fill in by hand
- Groups, aliases, descriptions, per-account mode and FPS, anti-AFK
- One password-sealed `.alyae` file to move accounts to another PC

## Windows

Download [Alyae-portable.zip](https://github.com/vysaely/alyae/releases/tag/portable), unzip anywhere, run `alyae.exe`.

## Linux

Any desktop (KDE, GNOME, Hyprland, ...). Needs [Rust](https://rustup.rs), and `webkit2gtk-4.1` for the sign-in window.

```bash
git clone https://github.com/vysaely/alyae.git
cd alyae
./install.sh
modules/roblox-linux/mocktail/build.sh   # Mocktail with Alyae's patches (AEAM's Mocktail works too, without anti-AFK)
```

Coming from AEAM: the first start offers to bring its accounts in (AEAM is left as it is).

Uninstall: `./install.sh --uninstall` (accounts in `~/.local/share/alyae` are kept).

## License

[GPL-3.0](LICENSE). The patches in `modules/roblox-linux/mocktail/` modify Mocktail (Apache-2.0) and are Apache-2.0 too.
