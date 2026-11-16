# Security review (2026-11-16)

Read from the code at main 0d39a67 plus this branch. `cargo audit`: no known vulnerabilities in
the dependencies; three warnings in Linux-only GUI libraries pulled in by the login window
(zvariant and zvariant_derive unmaintained, glib 0.18 VariantStrIter unsound; Alyae does not
call either directly).

## What protects the accounts

- Secrets (cookie, password, 2FA secret) are sealed in the database with XSalsa20-Poly1305 under
  a random 32-byte key in `vault.key`; plain fields (names, groups) are not secret.
- `vault.key`, the database and the log are owner-only on Linux (0600, folder 0700). On Windows
  they sit in the user's own `%APPDATA%\Alyae` (or the portable `data` folder).
- Helpers get secrets on stdin, never on the command line. The online watch sends only user IDs
  and asks Roblox anonymously.
- The Log in window gives the saved password only to `https://www.roblox.com`'s own top page and
  will not navigate elsewhere.
- Roblox calls go only to fixed `https://*.roblox.com` addresses.
- Accounts files (`.alyae`) are sealed with Argon2id (64 MiB, 3 passes) + XSalsa20-Poly1305; the
  header is inside the sealed part, so a file edited to weaker settings refuses to open.

## Fixed on this branch

1. **Command injection in action templates (high for shared modules or imported files).** A
   template like `run {login}` put the value into a shell command as it was: a login of
   `x; anything` ran `anything`. Values are now one quoted argument each (Windows refuses
   `" % ! ^`, which cmd cannot quote). A template that is one field alone (`{launch}` in Any
   game) stays the person's own command.
2. **Imported launch commands.** A `.alyae` file from someone else could carry a launch command;
   bringing it in now names every account that has one, to check before pressing Launch.
3. **Command text in error messages** could show a password placed in a template; errors no
   longer repeat the command.
4. **Private server link codes** from a pasted link are checked to be letters, digits, `-`, `_`
   before they go into the launch address.

## Left as they are (know them)

- **Anyone with both `vault.key` and the database has every secret.** That is the price of
  starting without a password (same as AEAM). On a shared or work PC, or a USB stick, the
  folder is the key. A master password (Argon2id, asked at start) is the fix if wanted.
- **Copied secrets stay in the clipboard**, and Windows' clipboard history (Win+V) and cloud
  clipboard keep them. Turn clipboard history off at work, or copy 2FA codes only.
- **Roblox on Linux:** Mocktail needs the cookie in a file (owner-only) while the account plays;
  it is removed when the game ends. A killed helper leaves it until the next start.
- **Windows build is unsigned:** SmartScreen warns; a copy from anywhere but the release page
  cannot be told apart. Check the zip came from github.com/vysaely/alyae.
- **Modules are trusted code.** A module folder can name any helper program; only put modules
  from yourself in the modules folders.
- Not checked: whether ureq passes the Cookie header on a cross-site redirect (all addresses are
  Roblox's own, so a redirect would have to come from Roblox).
