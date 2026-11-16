# Security policy

## Reporting a vulnerability

Please report security problems privately via
[GitHub security advisories](https://github.com/ymihere/seatswap/security/advisories/new), not
in public issues. You will get an answer within a week.

## Safety model

seatswap handles Claude login tokens, so it is built to keep them where they belong:

- **Tokens stay local.** Stored logins live in the system keyring (Secret Service on Linux,
  Credential Manager on Windows). Without a keyring they go into files in the data folder, and
  the pop-up warns about it. On Linux those are `0600` files in a `0700` folder; Windows has no
  such modes, so there they are protected only by the permissions of the user's own folder.
- **Each token goes only to its own provider, over HTTPS.** Claude logins go only to the usage
  endpoint (`api.anthropic.com`) and Claude Code's token endpoint (`platform.claude.com`). With
  the Codex card on, Codex's own access token goes only to `chatgpt.com`, read from Codex's
  `auth.json`, which seatswap never writes or refreshes. The Ollama card asks the Ollama server
  (by default the local one) with no secret.
- **The other network calls carry nothing about you.** While *Look up prices and the dollar rate*
  is on, seatswap fetches API prices from `models.dev` and the dollar rate from
  `api.frankfurter.dev`. Those requests send a User-Agent and no account, token or machine data;
  they follow no redirect. With the switch off, neither site is contacted.
- **No token in output.** Logs, error messages and command output never contain a token; at most
  the last four characters of a refresh token.
- **Minimal writes to Claude Code's files.** A switch rewrites only the login entries of
  `.credentials.json` and the `oauthAccount` entry of `.claude.json`. *Use in Claude Code* (the
  status line) writes only the `statusLine` key of Claude Code's `settings.json`. Every other key
  stays exactly as it was, in the same order. Every write is atomic (temporary file, then rename),
  and the first write to each file keeps a `.seatswap-backup` copy.
- **Reading sessions, not conversations.** To show running sessions and count tokens, seatswap
  reads Claude Code's session files and transcripts (on by default; *Read Claude Code's session
  files* turns it off). It keeps model names, token counts, times, folders and session ids; it
  never stores, logs or sends message text, and it never opens the secret `.key` files that sit
  next to the session files.
- **What a shared history folder holds.** The history (usage readings, switches, tokens per
  project and hour) and the live report other hosts read (`sessions-now.json`: session names,
  folders, models and token counts, never an agent's task text) are written to the history folder.
  If you point that folder at a synced service, those records travel with it. No token and no
  message text is ever written there.
- **Locks.** A switch holds Claude Code's own lock files and its own app lock, so it never
  interleaves with a token refresh or a settings write by Claude Code.
- **No shell.** The app runs no commands. The autostart entry is a plain desktop file on Linux
  and one value under the current user's `Run` registry key on Windows. The status line command
  that *Use in Claude Code* writes is run by Claude Code, not by seatswap.

## Scope

Bugs in Claude Code itself, or in Anthropic's, OpenAI's or Ollama's endpoints, are out of scope;
report those to their makers.
