# Security Policy

## Threat model

`yt-tui` is a local terminal client for YouTube Music.

**In scope**

- Protecting the signed-in YouTube session (cookies) stored in `auth.json` (`0600`, app data directory)
- The optional MCP control bridge: listens on `127.0.0.1` only and requires a random per-launch token from `control.json` (`0600`)
- Not logging cookies, session tokens or the control token to disk or the terminal
- Running YouTube's player JavaScript only inside the embedded, sandboxed QuickJS engine (no filesystem or network access)

**Out of scope**

- Compromised OS user account / malware that can read the user's files or process memory
- Changes on YouTube's side that break playback or sign-in
- Other local processes running as the same user

## Reporting

Please open a private security advisory on the repository or email the maintainers.
Do not file public issues that include cookies, session data or exploit details.

## Hardening expectations

- HTTPS only for all YouTube and lyrics requests
- File permissions `0600` for session and control files
- Logs go to a file, never to the terminal, and never contain cookies or tokens
- Panic / suspend should restore the terminal
