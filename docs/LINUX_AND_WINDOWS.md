# Roblox on Linux and on Windows: what each has

Compared 2026-11-16 from the code (rows updated 2026-11-19), after merging the Windows
session's work (it was built on Windows from a copy of main and never pushed)
(`crates/alyae-roblox/src/linux.rs`, `windows.rs`, `modules/roblox-*/module.toml`). Windows is
still untested on a real PC.

| | Linux (Mocktail) | Windows (official client) |
|---|---|---|
| Several accounts at once | own profile folder per account | `ROBLOX_singletonMutex` held by the helper |
| One account starting at a time | **now yes** (borrowed, `launch.lock`, 5 s) | yes (`Local\AlyaeRobloxLaunch`, 5 s) |
| Join place / job / private server | `roblox://experiences/start` URI | PlaceLauncher URL with an auth ticket |
| Roblox settings per account | the profile's own `GlobalBasicSettings_13.xml` | own copy swapped in before start, taken back at exit |
| FPS cap, alt quality 1 and muted | yes | yes |
| FastFlags (texture budget, no DRS) | yes | no (Roblox ignores them on Windows now) |
| Alts at low CPU priority, own CPUs, memory cap | yes (systemd scope, own CPUs, nice 5; a main is not pinned) | below-normal priority and low memory priority (a main: high priority), memory trimmed with Memory "trim"; no own CPUs |
| Anti-AFK | yes (Mocktail patches 08 and 11) | no |
| Reset files | yes, per account | yes, for all accounts at once (they share Roblox's caches), with every Roblox closed |
| Removing an account deletes its files | yes (`on_remove = "forget"`) | nothing to delete (shared client) |
| Renewed cookie saved after play | yes, also when Alyae was closed while the game ran | no (Alyae does not read the official client's stored sign-in) |
| Add account: browser sign-in (click), Import cookies and Add manually (right-click) | yes | yes |
| Open in browser (roblox.com signed in as the account) | yes | yes |
| Follow joins by user (reserved servers, sub-places) | yes (`userId` in the launch URI, with the place presence shows, else Place ID) | yes (RequestFollowUser, the user alone) |
| Online watch backs off while Roblox rate limits | yes | yes |
| Colours from the desktop | Infynitae's theme; **now** any desktop's light/dark and accent through the portal (borrowed) | registry light/dark and accent |

## Borrowed from Windows into Linux (this change)

1. **One start at a time.** Starting ten alts at once ran ten Mocktail setups together (disk,
   CPU, the desktop stutters). Each now waits for the one before it to have run 5 s, as on
   Windows. Its status reads "waiting for another account to start". A helper that dies frees
   the turn with its lock file.
2. **Desktop colours on any Linux desktop.** Off Infynitae, Alyae used egui's fixed dark look;
   on Windows it follows the system. It now reads `org.freedesktop.appearance` (color-scheme,
   accent-color) through the desktop portal, which GNOME, KDE and Infynitae's portal answer.

## Not borrowed, and why

- **Settings copy swapped in at start:** Windows needs it because every account shares one
  settings file; Linux profiles each have their own.
- **PlaceLauncher URL:** Mocktail takes the `roblox://` URI; both carry place, job and private
  server code.
- **PID file for "is it running":** Linux already finds each account's own program by name.

## Linux has, Windows could get later

Anti-AFK (needs input injection: `SendInput` to the account's window), alts on their own CPUs
(a job object), renewed cookie capture.
