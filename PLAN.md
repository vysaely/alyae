# Ludae plan

Ludae is AEAM's successor: a small game account manager for Linux and Windows. The core knows
nothing about any game; each game is a module. AEAM keeps working untouched until Ludae
replaces it.

## Shape

- **Core (`ludae`)**: one Rust program with an egui window. It opens on a game picker and
  remembers the last game. Each game uses AEAM's layout:
  - group picker, account table (Username, Alias, Group, Mode, Description);
  - launch boxes and the main button on the right, with the active account below;
  - a right-click menu: the module's buttons, Stop, Mode, Move to group, Copy, Remove.
- **Storage**: a 32-byte key file plus secretbox (AEAM's format), and SQLite holding plain fields
  and one sealed blob of secret fields per account.
- **Module**: a folder holding `module.toml`.
  - `[[field]]`: what an account of the game stores (secret, `totp`, `choice`).
  - `[[action]]`: its buttons. An action runs a command template (`{login}` ...) or the
    module's helper. `primary` makes it the big button, and `runs` means the account counts
    as running while the action lasts.
- **Helper protocol**: Ludae runs `helper <action>`.
  - stdin: one JSON line, `{id, account, inputs, data_dir}`.
  - stdout: JSON lines, `{"set":{...}}`, `{"status":"..."}`, `{"error":"..."}`.
  - A `stop` line on stdin ends the game. Closing Ludae leaves games running.
- **Modules shipped**:
  - `generic`: login, password, 2FA, a launch command.
  - `roblox-linux` / `roblox-windows`: both use the game id `roblox`, so they share accounts.
    The helper is `ludae-roblox`, with the actions Join Server, Follow and Refresh.

## Status

1. Core, generic module: done.
2. Roblox on Linux (Mocktail, main/alt, isolated profiles): done. Tested on one alt: sign-in
   check, launch, stop, cookie cleanup, saving a renewed cookie. A main, Join by place/job and
   Follow are not tested.
3. AEAM import: done. Accounts keep AEAM's profile folders. Test: the real AEAM data reads
   and decrypts.
4. Windows: the code is written but not built. It needs `mingw-w64-gcc` here or a GitHub build,
   then a run on a Windows PC.
5. Still to do: README.

## Left out on purpose (AEAM had them)

Each can come back as a module action if missed:

- Batch password change, unblock, friends, privacy and sign-out tools.
- Server browser, anti-AFK, self-heal, updater, the FastFlags editor.
- The in-app web login. Roblox accounts are added by cookie.
