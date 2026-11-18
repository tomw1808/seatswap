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
  On Windows, saved Codex logins are always private files in the same `secrets` folder, beside
  the keyring: a Codex login is larger than one Credential Manager entry, and seatswap copies a
  login it finds into that folder as Codex keeps its own `auth.json`.
- **Each token goes only to its own provider, over HTTPS.** Claude logins go only to the usage
  endpoint (`api.anthropic.com`) and Claude Code's token endpoint (`platform.claude.com`). With
  the Codex card on (or a price set for an OpenAI subscription), each saved Codex login's access
  token goes only to `chatgpt.com`, and its refresh token only to OpenAI's own token endpoint
  (`auth.openai.com`), to renew a saved login that is not live; a login from any other issuer is
  never renewed. seatswap never renews Codex's live login, and writes Codex's `auth.json` only to
  switch to another saved login. Saved Codex logins are kept like Claude logins (keyring, or the
  `0600` files; on Windows always the private files above). While the card is off and no OpenAI
  subscription has a price, the tray app still reads a Codex login file it finds in the Codex
  folder, saves that login and may poll it (§7.9); it reads nothing when no login file is there.
  Only `seatswap codex list` or `codex switch`, run by you, read it otherwise. The Sessions area
  is separate: with *Read Claude Code's session files*
  on, it reads Codex's rollout files whatever the card says. The Ollama card asks the Ollama server
  (by default the local one) with no secret; when the `ollama` program is on `PATH` or this host
  has an Ollama subscription, seatswap still asks that server while the card is off.
- **A found provider is read, not trusted.** The Codex login found in the Codex folder is copied
  into seatswap's store so it can be counted and, if needed, switched to; it is never written back
  to Codex except by an explicit switch. When the `ollama` program is found on `PATH`, the server
  at `ollamaUrl` is asked only for its balance (`/api/balance`) and the last 24 h's usage
  (`/api/usage?range=24h`), with no login and no secret.
- **The other network calls carry nothing about you.** While *Look up prices and the dollar rate*
  is on, seatswap fetches API prices from `models.dev` and the dollar rate from
  `api.frankfurter.dev`. Those requests send a User-Agent and no account, token or machine data;
  they follow no redirect. With the switch off, neither site is contacted.
- **No token in output.** Logs, error messages and command output never contain a token; at most
  the last four characters of a refresh token.
- **Minimal writes to Claude Code's files.** A switch rewrites only the login entries of
  `.credentials.json` and the `oauthAccount` entry of `.claude.json`. *Use in Claude Code* (the
  status line) writes only the `statusLine` key of Claude Code's `settings.json`. A Codex switch
  writes the saved login's whole `auth.json` object back as Codex wrote it, holding Codex's own
  locks. Every other key stays exactly as it was, in the same order. Every write is atomic (temporary file, then rename),
  and the first write to each file keeps a `.seatswap-backup` copy.
- **Reading sessions, not conversations.** To show running sessions and count tokens, seatswap
  reads Claude Code's session files and transcripts (on by default; *Read Claude Code's session
  files* turns it off). It also reads DeepSeek Harness's session files (`$DSH_HOME/sessions` or
  `~/.dsh/sessions`), flash-agents' job reports (`$FLASH_DATA_DIR`; else on Windows
  `%LOCALAPPDATA%\flash-agents`, elsewhere `$XDG_STATE_HOME/flash-agents` or
  `~/.local/state/flash-agents`) and Codex's rollout files
  (`<codex folder>/sessions/YYYY/MM/DD/rollout-*.jsonl`) and thread names
  (`<codex folder>/session_index.jsonl`). From a Harness session it takes the
  session id, its parent, folder, model, token counts and times; from a job report only the job's
  `sessionId`, to link the job to its Claude Code session; from a Codex rollout the session id,
  folder, model, token counts and times; from Codex's thread index only each thread's id, name and
  time. A Claude Code session's title (the `aiTitle` of its
  transcript's `ai-title` lines) is stored in the history and the live report; a DeepSeek Harness
  session that is a row of its own (not a flash job) carries its last `session/title`, and a Codex
  row its thread name, in the live report. Message text and a
  worker's title or task text are never
  stored, logged or sent; a worker's title is shown in the window and cleared before
  the report other hosts read. No file among them is ever written. It never opens the secret `.key`
  files that sit next to Claude Code's session files.
- **What a shared history folder holds.** The history (usage readings, switches, tokens per
  project, model and 5 minutes, session records with their titles) and the live report other hosts read
  (`sessions-now.json`: session names and titles, folders, models and token counts, never an agent's
  task text) are written to the history folder.
  If you point that folder at a synced service, those records travel with it. No token and no
  message text is ever written there.
- **Locks.** A switch holds Claude Code's own lock files and its own app lock, so it never
  interleaves with a token refresh or a settings write by Claude Code.
- **No shell.** The app runs no commands. The autostart entry is a plain desktop file on Linux
  and one value under the current user's `Run` registry key on Windows; seatswap also writes its
  own menu entry and icon under `$XDG_DATA_HOME/applications` and `$XDG_DATA_HOME/icons`, and
  leaves a symbolic link there alone, and an entry it did not make. The status line command
  that *Use in Claude Code* writes is run by Claude Code, not by seatswap.

## Scope

Bugs in Claude Code itself, or in Anthropic's, OpenAI's or Ollama's endpoints, are out of scope;
report those to their makers.
