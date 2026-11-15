# Ludae plan

Ludae is AEAM's successor: a small game account manager. The core knows nothing about any game;
games come as modules. Linux and Windows. AEAM keeps working untouched until Ludae replaces it.

## Shape

- **Core (`ludae`)**: one Rust program with an egui window.
  - Vault: a 32-byte key file plus secretbox, the same format as AEAM.
  - Accounts in SQLite: game, name, group, notes, plain fields, encrypted secret fields.
  - Search, groups, copy buttons, live 2FA codes.
  - Runs module actions and shows which accounts are running.
- **Module**: a folder holding `module.toml`.
  - `[[field]]` lists what an account of this game stores (secret, 2FA, choices).
  - `[[action]]` lists its buttons. An action either runs a command template (`{login}` ...) or
    calls the module's helper program.
- **Helper protocol** (when a game needs code): Ludae runs `helper <action>`.
  - stdin, one JSON line: `{account, inputs, data_dir}`.
  - stdout, JSON lines: `{"set":{...}}` updates fields (a new name, a refreshed cookie);
    `{"status":"..."}`; `{"error":"..."}`.
  - The helper lives as long as the game. Ludae sends `stop` on stdin to close it.
  - Closing Ludae leaves running games alone.
- **Modules shipped**:
  - `generic` (any game): login, password, 2FA, notes, a launch command.
  - `roblox`: the helper `ludae-roblox`.
    - Linux: Mocktail, one isolated profile per account, main or alt role.
    - Windows: the official client through an auth ticket, plus the multi-instance mutex.
    - Actions: Play, Join (place / job / private link / follow user), Refresh.

## Stages

1. **Core**: vault, store, 2FA, module loading, the window, the generic module.
2. **Roblox on Linux**: the helper with Mocktail launch, check cookie, main/alt.
3. **AEAM import**: read `~/.local/share/aeam` (key, DB, `mains.txt`) and copy the accounts in.
   AEAM itself is not changed.
4. **Windows**:
   - Build the core and the helper for Windows.
   - Roblox through `roblox-player:` with a ticket.
   - Data in `%APPDATA%\Ludae`.
   - Checked with a cross-build here; a real run needs a Windows PC.
5. **Polish**: colours from the system theme (Infynitae `LOOK_*`), a launcher entry,
   install script, README.

## Left out on purpose (AEAM had them)

These are not in the core. Each can come back later as a Roblox module action:

- Batch password change, unblock, friends, privacy and sign-out tools.
- Server browser, anti-AFK, self-heal, updater, the FastFlags editor.
- The in-app web login. Roblox accounts are added by cookie for now.

## Checks

- Unit tests: vault (AEAM blobs open), 2FA (RFC vector), store, module parsing.
- The Roblox launch is compared against AEAM's on one alt account before anything is switched.
