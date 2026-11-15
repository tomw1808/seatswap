---
name: seatswap — specification
status: Draft
created: 2026-11-10
updated: 2026-11-15
tags: [seatswap, spec, claude-code, tray, linux, macos, windows]
---

# seatswap — specification

A tray app that switches Claude Code between several Claude accounts (licences) with one click. All accounts share one `~/.claude`, so memory, skills, plugins, hooks, settings and history are the same whichever account is on. The tray icon shows which account is active and how much of its limit is used. A pop-up shows the usage of every stored account side by side.

## Provenance

Written 2026-11-10 on the Mint/Cinnamon ThinkPad with Claude Code 2.1.322.

**Checked on this machine:**
- Location and layout of the login files.
- The usage endpoint, called with the live login.
- The token endpoint and client ID, read from the Claude Code program.

**Read from the Claude Code program (second pass, same day):** how Claude Code itself refreshes, locks, re-reads and saves the login. These points are marked *(code)*. They describe version 2.1.322 and are read from the code.

**Tested in Phase 0 (same day, evening):** the points marked *(tested)*. The tests ran with two real accounts in a separate config folder, so the real login was never touched. Results, evidence and scripts are in `phase0/RESULTS.md`.

**Still assumed:** everything marked *(Phase 0)*.

The endpoints are undocumented and belong to Claude Code's own login. Anthropic can change them in any release. The design keeps them in one module so a change is a one-file fix.

---

## 1. Goals and non-goals

**Goals**
1. Switch the Claude Code account in one click from the tray, or with one command (`seatswap use B`).
2. Keep one shared `~/.claude`. Nothing besides the login changes on a switch.
3. Show the active account and its usage on the tray icon itself.
4. Show the usage of all stored accounts in a good-looking pop-up, including accounts that aren't active.
5. Give each account a short label of one or two characters, and a colour.
6. Run on Linux (as many desktops as possible), macOS and Windows.
7. Show when every account's login ends and warn in good time before it does. A login has a fixed end date that nothing can move (see §6.3).
8. Switch by itself, when auto-burn is switched on, so that as little work as possible is blocked, and say what it would do when it is off (see §7.4).

**Non-goals (v1)**
- Switching the **Claude Desktop** app. Its login is a Chromium cookie, stored encrypted in `~/.config/Claude` (Linux) or the matching folder elsewhere, so swapping files is not reliable there. Revisit later (see §12).
- Switching claude.ai in the browser. Browser profiles already do that.
- Choosing the account automatically **without being asked**. Auto-burn is a switch (§7.4); off, seatswap only *suggests* a switch.
- API-key accounts (Console billing). Only subscription logins (Pro, Max, Team, Enterprise) in v1.
- A different account per terminal. A switch is global: every Claude Code session on the machine follows it (§6.5). Claude Code has a built-in way to pin one terminal to one account (`claude setup-token` makes a 1-year token, used through the `CLAUDE_CODE_OAUTH_TOKEN` variable). Those tokens only carry the `user:inference` permission *(code)*, so the usage display would most likely not work with them. Not part of v1.

---

## 2. How Claude Code stores the login (observed)

### 2.1 Linux and Windows: `~/.claude/.credentials.json`

On Windows the file is `%USERPROFILE%\.claude\.credentials.json`. It is plain JSON, mode `0600` on Linux; on Windows it has the permissions it inherits from the profile folder (there are no mode bits). Top-level keys:

| Key | What it is | On a switch |
|---|---|---|
| `claudeAiOauth` | **The account login.** It holds `accessToken`, `refreshToken`, `expiresAt`, `refreshTokenExpiresAt` (both in ms since the epoch), `scopes`, `subscriptionType` (e.g. `team`), `subscriptionTypeSource` and `rateLimitTier` (e.g. `default_claude_max_5x`). | **swap** |
| `organizationUuid`, `trustedDeviceToken`, `trustedDeviceEnrolledAfterRefusalAtMs`, `enterpriseGateway`, `designOauth`, `cloudSessionsOauth` | **Other parts of the login.** Claude Code's logout deletes exactly these six together with `claudeAiOauth` *(code)*. On this machine only `organizationUuid` exists; it holds the account's organization. | **swap** |
| `mcpOAuth` | Logins to MCP servers (Linear, Notion, GitHub, Slack and others), keyed by server. | **keep**: these are shared |
| anything else | Unknown future keys. | **keep** |

The seven **login keys** are saved and restored as one set. A login key that the target account does not have is removed from the file, so nothing of the old account stays behind. A fresh `/login` and a renewal wrote only `claudeAiOauth` *(tested)*. The others appear later; the code that writes `organizationUuid` was not found.

Claude Code can add more fields inside `claudeAiOauth` (seen in the code: `clientId`, `subscriptionTypeVerifiedAt`, `loginEnded`). seatswap always stores and restores each login key whole and never picks single fields.

Lifetimes *(tested)*:
- The access token lives for 8 hours.
- **A login has a fixed end date.** `refreshTokenExpiresAt` is set at `/login` and no refresh moves it: four refreshes on the two accounts returned the same end date each time. Claude Code itself keeps the *earlier* of the old and the new date, and marks a login that reached its end with `loginEnded: "time_limit"` *(code)*.
- A fresh `/login` lasted 27.5 days (Acme, Team) and 29.4 days (private, Max). seatswap reads the date from the login and assumes no fixed length.
- **Every refresh uses up the old tokens.** The server returned a new refresh token each time. The old refresh token was rejected right away (`invalid_grant`), and so was the old access token (HTTP 401). The login itself stayed valid.
- Two logins of the same account exist side by side: a second `/login` in another config folder did not disturb the first.

### 2.2 macOS: Keychain

On macOS Claude Code keeps the same JSON in the login Keychain, not in a file:
- With the default config folder the service name is `Claude Code-credentials`. *(code)*
- With `CLAUDE_CONFIG_DIR` set it is `Claude Code-credentials-<h>`, where `<h>` is the first 8 hex characters of the SHA-256 of the folder path. *(code)*
- The Keychain account name is the user name (`$USER`). *(code)*
- Not tried on a Mac yet *(Phase 0)*.

### 2.3 Account identity: `~/.claude.json` → `oauthAccount`

This is a separate file next to the folder. It is large and Claude Code writes to it often. The `oauthAccount` object holds:
- `accountUuid`, `emailAddress`, `fullName`
- `organizationUuid`, `organizationName`, `organizationType`, `organizationRole`, `workspaceRole`
- `billingType`, `seatTier`, `userRateLimitTier`, `organizationRateLimitTier`
- `hasExtraUsageEnabled` and a few onboarding/trial fields

Claude Code uses it for display (`/status`) and for features tied to the organization. **A switch replaces `oauthAccount` and must leave every other key of the file alone.**

Claude Code itself sometimes updates this object *(code)*: when it renews a login and the profile details are incomplete, it fetches the profile and merges name, billing type, seat tier and similar fields into whatever `oauthAccount` is in the file. It does not touch `accountUuid`, the email or the organization. So the two files can disagree for a moment only if a switch lands in the middle of such a renewal; the locks in §6.2 prevent that.

The file also holds caches that belong to the account. Claude Code's logout clears 18 more keys *(code)*, among them `clientDataCacheSlots`, `cachedArtifactRoster`, `modelAccessCache` and `metricsStatusCacheByPrincipal`. seatswap leaves them alone: the ones seen in the tests carry the account or organization they were fetched for, and Claude Code replaced them by itself, a running session at its first request after a switch and a new session at start *(tested)*.

Claude Code writes this file under a lock of its own (§2.6).

### 2.4 Caches tied to the organization

`~/.claude/remote-settings.json` and `~/.claude/policy-limits.json`, each with `.signature*` / `.stamp*` siblings, are fetched per organization. **seatswap does not touch them.** Claude Code keeps them right by itself:

- Each file is signed for one organization and one account. Claude Code rejects a file signed for another *(code)*.
- A Team account checks them with the server at every start, and a running session fetched new ones 12 s after a switch to that account *(tested)*.
- A personal account (Pro, Max) leaves the other account's files on disk *(tested)*. Its log shows no sign that it reads them.

Not tested: a switch between two different Team or Enterprise organizations. There only the signature check keeps one organization's limits out of the other's session.

### 2.5 `CLAUDE_CONFIG_DIR`

If the environment variable is set, all paths above move into that folder, `.claude.json` included. seatswap honours the variable. A setting can also point it at a folder explicitly.

### 2.6 How Claude Code itself handles the login

seatswap shares the login files with every running Claude Code session, so it has to follow the same rules they use among themselves. Points marked *(code)* are read from the program. The locks were also watched in tests (`phase0/lockwatch.py`).

- **Sessions re-read the file when it changes.** *(code, tested)* Before a session uses its login, it looks at the modification time of `.credentials.json`. If the time changed, it drops its copy and reads the file again. So a running session picks up a switched login at its next request. No restart is needed.
- **A lock for renewals.** *(tested)* Before a session renews the login it takes `.oauth_refresh.lock` in the config folder, then a second lock next to the config folder (`<config folder>.lock`, the one older versions use), and writes an owner note `.oauth_refresh.lock.owner` (process ID and start time). It holds them until the new tokens are saved. A session that finds the lock taken tries 5 times, 1–2 s apart, and then gives up on the renewal for now *(code)*.
- **Two more renewal locks.** *(code, Claude Code 2.1.322)* Two secondary logins in `.credentials.json`, `designOauth` and `cloudSessionsOauth`, are renewed under their own lock folders in the config folder: `.design_oauth_refresh.lock` and `.cloud_sessions_oauth_refresh.lock`, abandoned after 60 s. The renewal follows the same pattern as the main login: read the key under the lock, call the server, save only if the key is unchanged. That its save also takes `.storage-write.lock` was read for the main login and is assumed for these two.
- **A lock for every write of `.credentials.json`.** *(tested)* `.storage-write.lock` in the config folder. Claude Code writes this file often, not only for renewals: one session start rewrote it about 20 times in 2.5 s while it added its MCP server entries. Each write reads the whole file, changes its part and writes the whole file back.
- **A lock for every write of `.claude.json`.** *(tested)* `.claude.json.lock` next to the file.
- **All of these locks are folders** made with `mkdir`. The holder keeps the folder's modification time fresh. Others treat a lock as abandoned when that time is older than 60 s (renewal), 15 s (`.storage-write.lock`) or 10 s (`.claude.json.lock`) *(code)*. The owner note only lets a session take the renewal lock over early, when the holder is proven dead *(code)*.
- **Claude Code waits for all three** (renewal, `.storage-write.lock`, `.claude.json.lock`) when someone else holds them *(tested)*.
- **Renewal uses the file, not memory.** *(code)* Under the lock the session reads the file again and sends the refresh token it finds *there*. A session never sends an old refresh token it remembers.
- **Save only if nothing changed.** *(code)* After a renewal the session writes the new tokens only if the file still holds the refresh token it sent. If something else changed the file meanwhile, it keeps the file as it is and adopts it. The renewed tokens are then lost.

What follows for seatswap:
- It must hold the renewal lock while it swaps the login (§6.2). Otherwise a switch can land in the middle of a renewal: the old account's refresh token was just used up, the new one is thrown away, and seatswap saves a dead login for that account. The same holds for the two secondary renewal locks: `designOauth` and `cloudSessionsOauth` are among the keys a switch swaps.
- It must hold `.storage-write.lock` while it rewrites `.credentials.json`, and `.claude.json.lock` while it rewrites `.claude.json`. Otherwise a write that Claude Code started just before the switch puts the old login back, or the switch wipes out an MCP login that Claude Code just saved.
- With these locks held, a running session cannot write an old account back over a switched login.
- Exceptions: sessions started with `CLAUDE_CODE_OAUTH_TOKEN`, and sessions started by another app that hands them a token (Claude Desktop), do not follow the file.

---

## 3. Endpoints (observed 2026-11-10)

| Purpose | Request | Status |
|---|---|---|
| Usage | `GET https://api.anthropic.com/api/oauth/usage` with headers `Authorization: Bearer <accessToken>` and `anthropic-beta: oauth-2025-04-20` | **Verified**, HTTP 200 |
| Token refresh | `POST https://platform.claude.com/v1/oauth/token`, header `Content-Type: application/json`, JSON body `{"grant_type":"refresh_token", "refresh_token":"…", "client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e", "scope":"<the login's scopes, separated by spaces>"}` | **Verified** *(tested)*, HTTP 200. The server rejects Python's default `User-Agent` with HTTP 403 ("error code: 1010"), so seatswap sends its own: `seatswap/<version>` |

The refresh response holds `access_token`, `expires_in` (seconds), `scope`, and optionally `refresh_token` and `refresh_token_expires_in`. If `refresh_token` is missing, the old one stays valid, so the code must handle both a rotated and an unchanged refresh token. In every test call the response carried a new `refresh_token` and a `refresh_token_expires_in` *(tested)*. The response also names the owner: `account.uuid`, `account.email_address` and `organization.uuid`. seatswap compares these with the slot it is refreshing and refuses to save on a mismatch.

Scopes on the live login: `user:inference`, `user:profile`, `user:file_upload`, `user:mcp_servers`, `user:plugins`, `user:sessions:claude_code`. Which of them the usage endpoint needs is not known *(Phase 0)*.

The usage response, trimmed to the parts we use:

```json
{
  "five_hour":  { "utilization": 97.0, "resets_at": "2026-11-10T13:40:00+00:00" },
  "seven_day":  { "utilization": 48.0, "resets_at": "2026-11-16T01:00:00+00:00" },
  "limits": [
    { "kind": "session",       "group": "session", "percent": 97, "severity": "critical",
      "resets_at": "2026-11-10T13:40:00+00:00", "scope": null, "is_active": true },
    { "kind": "weekly_all",    "group": "weekly",  "percent": 48, "severity": "normal",
      "resets_at": "2026-11-16T01:00:00+00:00", "scope": null, "is_active": false },
    { "kind": "weekly_scoped", "group": "weekly",  "percent": 43, "severity": "normal",
      "resets_at": "2026-11-16T01:00:00+00:00",
      "scope": { "model": { "display_name": "Fable" } }, "is_active": false }
  ],
  "extra_usage": { "is_enabled": false, "...": "..." },
  "spend": { "used": { "amount_minor": 0, "currency": "USD", "exponent": 2 }, "enabled": false }
}
```

Rules for reading it:
- **Use `limits[]` first.** It is already normalised, with a kind, a percentage, a severity (`normal` / `warning` / `critical`, the last two seen *(Phase 0)*), a reset time, an optional model scope, and `is_active` for the limit that binds right now.
- Fall back to `five_hour` / `seven_day` when `limits` is missing.
- **A limit can be missing.** The private account's reply had `five_hour: null` and no `session` entry in `limits[]`, only weekly limits *(tested)*. Show only the limits the reply holds.
- Ignore unknown keys. The response carries many code-named null fields that change between releases.
- Show `extra_usage` / `spend` only when `enabled` is true.

---

## 4. Architecture

```
┌──────────────────────────── seatswap (one Rust binary) ───────────────────────────┐
│                                                                                         │
│  core (library crate)                                                                   │
│   ├─ store      account list (no secrets) + secrets in the OS keyring                   │
│   ├─ live       read/write the live Claude Code login (file or macOS Keychain)          │
│   ├─ switcher   save current → load target, under Claude Code's locks, atomic writes    │
│   ├─ usage      poll /api/oauth/usage per account, cache, back off                      │
│   ├─ refresher  refresh logins of inactive accounts, track when each login ends         │
│   └─ watcher    notice when Claude Code changes the live login (renewal, /login)        │
│                                                                                         │
│  front ends                                                                             │
│   ├─ tray       icon + menu (tray-icon crate via Tauri 2)                               │
│   ├─ panel      pop-up window, HTML/CSS in the system webview (Tauri 2)                 │
│   └─ cli        seatswap list | use | next | add | rm | edit | status | panel           │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

**Stack: Tauri 2 (Rust core, web UI in the system webview).**
- One code base gives a tray, a pop-up window and a CLI on all three systems.
- Binaries are small, around 10 MB against about 150 MB for Electron.
- The pop-up is HTML/CSS, which is the easiest way to make it look good.
- The core is a plain Rust library, so the CLI works without any GUI. It can run over SSH and from a Claude Code status line.

Alternatives considered:
- **Python + PySide6.** Quicker to start and similar to repotray, but packaging for macOS and Windows is heavier.
- **Electron.** Too heavy for a tray app.
- **A native Cinnamon applet, GNOME extension or KDE plasmoid.** The best integration, but one per desktop. These are kept as optional extras (§9.3).

---

## 5. Data model

Config folder (Rust `dirs` crate):
- Linux: `~/.config/seatswap/`
- macOS: `~/Library/Application Support/seatswap/`
- Windows: `%APPDATA%\seatswap\`

**`accounts.json`** holds no secrets:

```json
{
  "version": 1,
  "accounts": [
    { "id": "6f1c…", "accountUuid": "…", "organizationUuid": "…",
      "label": "Acme", "abbr": "AC", "color": "#3b82f6",
      "email": "…", "orgName": "…", "plan": "Team",
      "oauthAccount": { "...": "copy of ~/.claude.json oauthAccount" },
      "addedAt": "2026-11-10T12:00:00Z", "lastUsedAt": "…" },
    { "id": "a92d…", "label": "Private", "abbr": "P", "color": "#f59e0b", "...": "…" }
  ],
  "settings": { "...": "see §10" }
}
```

**Secrets.** Each account's login keys (§2.1) go into the OS keyring as one entry:
- Linux: Secret Service (GNOME Keyring or KWallet).
- macOS: Keychain.
- Windows: Credential Manager.

They use the `keyring` crate, with service `seatswap` and the account `id` as the user. If no keyring is available (a headless Linux box, or a minimal window manager without Secret Service), the fallback is `secrets/<id>.json` with mode `0600` in a `0700` folder. The UI then shows a warning that the logins are stored unencrypted.

**Identity.** An account is identified by `accountUuid` + `organizationUuid`. The same person in two organizations counts as two accounts, which is exactly the two-licence case.

**Usage cache.** `usage-cache.json` holds the last response per account and when it was fetched. The pop-up opens at once from the cache and refreshes in the background.

**History.** seatswap keeps what it saw and did in a folder of its own: `<data folder>/history` (`historyDir` names another one). Every host writes only into its own subfolder `<host>/`, and only appends, so several hosts can share one synced folder without producing a conflict. `<host>/host.json` holds the id of the machine that writes there; when it names another machine, seatswap refuses to write into that subfolder and says why.

The history has six kinds of file, one JSON object per line, one file per month and kind: `samples-YYYY-MM.jsonl` (one line per usage answer the tray app receives, per account, when a number or a reset time changed, and at least once an hour while the app runs), `windows-YYYY-MM.jsonl` (a window that ended), `events-YYYY-MM.jsonl` (a switch; a limit crossing 100 %, as `blocked`, `unblocked`, `allBlocked` or `anyUsable`; an `accountAdded` or `accountRemoved` when the CLI or the tray app creates or removes an account; a `loginEnded` or `loginRenewed` when the tray app's poll sees an account's login end or work again; `live`; an `agentCall`; a wait), `tokens-YYYY-MM.jsonl` (what the sessions used, one line per session, hour and model), `sessions-YYYY-MM.jsonl` (a session's summary when it changes and when it closes, also for a session that is seen in a transcript but no longer runs), and `up-YYYY-MM.jsonl` (at most one line per 5 minutes while seatswap runs on this host). The CLI writes the account events (`accountAdded`, `accountRemoved`); the tray app's engine writes the login events (`loginEnded`, `loginRenewed`) with the poll's time. `planChanged` is defined but not written yet.

A line is at most 4096 bytes; a longer record is an error, never cut. Times are UTC, RFC 3339; a reset time is rounded up to the full minute before it is stored. An account is named by `acc` (its `accountUuid`) and `org` (its `organizationUuid`), never by seatswap's local account id. A token line carries the project's identity as it was found at that moment, so it stays right after a folder is moved or its transcript is deleted. The transcript offsets move past an answer only after its token line was written: a pass without a working history reads the same answers again instead of losing them.

Appending is one write of the whole line plus an fsync (`atomic::append_line`). Reading opens only the month files that overlap the asked range and merges the hosts in time order. A torn last line, or a line of a shape this version does not know, is skipped and named in a warning, never guessed; the app prints each warning once as `seatswap: history: <warning>` on standard error.

---

## 6. Behaviour

### 6.1 Adding an account

1. **First run:** the live login is read and the user is offered "Save as account 1".
   - Label is prefilled from `organizationName` or the email.
   - Abbreviation is prefilled from the label's initials (see §8.2).
   - A colour is picked from the palette.
2. **More accounts:** from the menu, "Add account…" explains the steps and waits:
   1. "Your current account (AC) is saved."
   2. "In a terminal run `claude`, then `/login` with the other account." A `/logout` first is usually not needed (experience from daily use; to confirm in Phase 0, item 2). The app does not run the OAuth flow itself in v1.
   3. The watcher sees the live login change to an unknown `accountUuid`/org and offers "Save as new account".
3. Later option *(v2)*: run the OAuth flow in-app (PKCE through the browser, the same flow Claude Code uses). Then adding an account doesn't touch Claude Code at all.

### 6.2 Switching (the core algorithm)

```
switch(target):
  if target is marked "login ended": stop, show the re-login steps (§6.1)
  lock  ~/.config/seatswap/switch.lock           (exclusive file lock, 5 s timeout;
                                                       keeps two seatswap processes apart)
  lock  Claude Code's locks (§2.6), in the order Claude Code takes them:
          1. <config folder>/.oauth_refresh.lock, then <config folder>.lock   (renewal)
          2. <config folder>/.design_oauth_refresh.lock,
             then <config folder>/.cloud_sessions_oauth_refresh.lock          (secondary renewals)
          3. <config folder>/.storage-write.lock                              (.credentials.json)
          4. .claude.json.lock next to .claude.json                           (.claude.json)
        wait up to 10 s for all of them, then stop with
        "Claude Code is busy with its login, try again"
  live  = read the login keys (§2.1) from the live login   (file or macOS Keychain)
  ident = read ~/.claude.json → oauthAccount
  cur   = account matching ident.accountUuid + organizationUuid
  if cur exists:
      save live → keyring[cur]                         (it is fresher than our copy)
      save ident → accounts[cur].oauthAccount
  else:
      unlock all, ask "The current login (email) isn't saved. Save it first?"
      [Save] [Discard] [Cancel], then start again from the top
  tgt = keyring[target]
  write live login:
      file:     read whole JSON, remove all login keys, add the target's, write temp file
                (0600) in the same folder, fsync, rename over the original
      Keychain: update the generic-password item in place (same service and account)
  write ~/.claude.json:
      read → replace oauthAccount only → write temp → rename
  unlock all, in reverse order
  update tray icon, notify "Switched to P — Private (12 % · 5 h)"
```

Notes:
- **Order of the two writes.** The login file is written first, the identity second. A session that reads between the two sees the new login with the old name for a moment. That affects the display only.
- **If a step fails,** the switch stops with an error and names the state it left ("login switched, identity not"). It never continues quietly. The next switch or the watcher (§6.4) repairs a half-done state by matching the live login to a known account.
- **If the live login is empty or marked as ended** (`refreshToken` is `""`), there is nothing to save for the current account. Its slot is marked "Logged out" and the switch goes on.
- **How seatswap holds Claude Code's locks.** Each lock is a folder made with `mkdir`. If the folder exists, the lock is taken: seatswap waits and tries again. While it holds the locks it sets their modification time anew every 2 s, so none of them looks abandoned (the shortest limit is 10 s). A switch itself takes milliseconds.
- **Abandoned locks.** A lock folder whose modification time is older than its limit (§2.6) was left behind by a crashed process. seatswap removes it and takes the lock, as Claude Code does.
- **No owner note.** seatswap does not write `.oauth_refresh.lock.owner`. Without one, Claude Code waits the normal way and never takes the lock over early *(code)*.
- **Nobody is asked anything while the locks are held.** The question about an unsaved login comes after the locks are released, because the locks count as abandoned after 10–60 s and Claude Code waits on them.

The access token of the target may have expired. That's fine: Claude Code refreshes it with the refresh token on its next call. **seatswap never refreshes the active account itself.** Claude Code owns the active login. A refresh uses up the old tokens at once (§2.1), so every session still holding them would be cut off.

### 6.3 Refreshing inactive logins

An account is **inactive** when its login is not the one in Claude Code's file. Since every session follows the file (§2.6), no session still uses an inactive account's refresh token, so seatswap can refresh it safely.

- **Why refresh at all:** the access token lasts 8 hours. To show the usage of an inactive account, seatswap needs a valid one. When the token has expired and a usage poll is due, seatswap refreshes it (request in §3).
- **Save first.** The old tokens are dead as soon as the server answers (§2.1). seatswap writes the new ones into the keyring before it does anything else with them. If that write fails, the account is logged out; the app says so and shows the steps from §6.1.
- Like Claude Code, seatswap keeps the *earlier* of the old and new `refreshTokenExpiresAt`.
- Refresh runs under seatswap's own lock (`switch.lock`), and only for accounts that are not live. It checks under the lock that the account is still not live.
- A refresh that fails with `invalid_grant` marks the account **"Logged out — log in again"**. It shows red in the pop-up, with the steps from §6.1. The account's old tokens are not used again.
- A network error or a 5xx is not a logout. The account keeps its state and the refresh is tried again with the back-off from §7.1.

**The end date of a login.** A login ends on a fixed date that no refresh moves *(tested, §2.1)*. seatswap cannot keep a login alive. It makes sure the end is never a surprise:
- The pop-up shows "login ends in N days" for every account.
- A notification comes 5 days and 1 day before the end, with the re-login steps from §6.1.

This is accepted (decided 2026-11-10): a `/login` per account about once a month is what is needed today without the app.

### 6.4 Noticing changes made by Claude Code

The watcher watches the live login:
- inotify on Linux, FSEvents on macOS, ReadDirectoryChanges on Windows.
- Keychain items can't be watched, so on macOS the login is polled every 30 s.

Cases:
- **Same account, new tokens** (Claude Code renewed them): nothing to do. The keyring copy is updated at the next switch. Optionally copy right away (setting `mirrorLive`, default on) so a crash never leaves a stale slot.
- **Different known account** (someone ran `/login` by hand): the tray follows it.
- **Unknown account:** the app offers to save it (§6.1).
- **Login gone or ended** (`/logout`, or Claude Code marked it as ended): the tray shows "no login" in grey. The account's slot is marked "Logged out".

The watcher reads the file only when Claude Code's lock is free, so it never mirrors a half-finished renewal.

### 6.5 Sessions that are already running

**A running session follows the switch at its next request** *(tested)*. It notices that the login file changed, reads it again and continues on the new account. It does not write the old account back. So a switch moves every open terminal session at once, including an agent that is in the middle of a long task.

The test: one open session (Claude Code 2.1.322 in a terminal), switched private → Acme → private → Acme. The last switch came between two tool calls of one turn. No restart was needed and no error appeared.

What this means in practice:
- This is what one wants when an account is nearly used up: switch, and the running work continues on the other account without a restart.
- A request that is already under way finishes on the old account. The next one goes to the new account.
- **The first request after a switch costs more** *(tested)*. The server keeps a copy of the conversation so far to make follow-up requests cheap, and that copy belongs to one organization. After a switch the conversation is written again for the new one: about 20k tokens in the test. Switching back to an account used a few minutes earlier only costs what was added meanwhile (3.9k tokens after 13 minutes).
- The pop-up counts the running `claude` processes and says before a switch: "3 sessions running — they move to P with their next request".
- Sessions that do not follow: those started with `CLAUDE_CODE_OAUTH_TOKEN`, and those started by Claude Desktop.

Not tried yet *(Phase 0)*:
- Sessions inside VS Code. On this machine the extension runs its own copy of Claude Code (2.1.321). Daily use suggests they follow a switch too. The test is postponed until no work sessions are running.
- A switch between two Team or Enterprise organizations (§2.4).
- macOS, where the login is in the Keychain and Claude Code keeps its copy for up to 30 s.

There is no special handling for an "old account written back" event. If the live login ever changes to another known account, whatever the cause, the rule in §6.4 covers it: the tray follows.

### 6.6 Agents

Every command Claude Code runs carries `CLAUDECODE=1` in its environment; an agent call also carries `AI_AGENT` (for example `claude-code_2-1-322_agent`). seatswap treats a call as an agent call when `CLAUDECODE` is exactly `1`, or `AI_AGENT` is set and not empty. A person's terminal has neither.

Two settings decide what an agent may do. `agentsRead` (on by default) lets agents run `list` and `status`, also with `--json`, `--refresh`, `--waybar` or `--i3blocks`. `agentsSwitch` (off by default) lets agents run `use <account>` and `next` without `--save-current` or `--discard-current`. `add`, `rm`, `edit`, `agent-skill`, `panel`, `use` or `next` with one of those two flags, and `seatswap` without a subcommand (which starts the tray app) can never be run by an agent, whatever the settings.

| Command | Agent may run it when |
|---|---|
| `list`, `status` | `agentsRead` is on; otherwise refused with `agents may not read usage (seatswap settings → Agents)` |
| `use`, `next` without `--save-current` / `--discard-current` | `agentsSwitch` is on; otherwise refused with `agents may not switch accounts (seatswap settings → Agents)` |
| `use` or `next` with one of those flags; `add`, `rm`, `edit`, `agent-skill`, `panel`; no subcommand | never; refused with `agents may not change seatswap's accounts or settings` (no subcommand is recorded as `(no subcommand)`) |

A refused call prints `seatswap: <text>` to stderr and exits with code 3; nothing is half done. Every call by an agent writes an `agentCall` event with the subcommand and its flags (without free-text values, e.g. `use PR` or `status --json --refresh`), the folder it was called from, and whether it was allowed. A switch by an agent is written with `by: agent` and the folder, and the tray always announces it: `An agent switched to PR (from ~/projects/atlas).` — or `An agent switched to PR.` when the folder is not known — whatever `notifyOnSwitch` says.

`seatswap agent-skill install` writes `<Claude config folder>/skills/seatswap/SKILL.md` from the bundled text; it refuses to overwrite a file that differs from it unless `--force`, and prints the path. `seatswap agent-skill uninstall` removes that file (with `--force` if it differs) and the `seatswap` folder when it is empty. Nothing else writes into Claude Code's config folder.

What the settings page says about the two switches:

> These two switches work through a marker that Claude Code puts on every command it runs. An agent runs as you. It can remove the marker, or change seatswap's files directly. The switches stop agents that follow the rules. They are not a lock. To block the command itself, add a deny rule for `seatswap` to Claude Code's permissions.

---

## 7. Usage

### 7.1 Polling

| Account | Interval | Also |
|---|---|---|
| Active | 2 min | right after a switch; when the pop-up opens (if older than 30 s) |
| Inactive | 15 min | when the pop-up opens (if older than 5 min) |

- On an HTTP 429 or 5xx, back off exponentially up to 30 min and show the time of the last fetch.
- On a 401: for the active account, read the live file again (Claude Code may have renewed it). For an inactive account, refresh (§6.3).
- An account that a limit blocks is polled once 30 s after that limit's reset time, whatever its interval, so a notification does not wait for the 15-minute poll of inactive accounts.
- While the active account's binding limit is at 90 % or more and below 100 %, it is polled every 30 s; at 100 % the reset poll above applies. The status line's feed (§7.6) can make the numbers newer still, without a request.
- No polling while the machine sleeps. Poll once on wake.

### 7.2 What the app derives from the response

For each account it shows:
- **Session (5 h):** percentage and time until reset (`1h12m`, `35m`). Only if the reply has a session limit (§3).
- **Week, all models:** percentage and the reset: more than 48 h away the weekday and clock time (`Sun 02:00`); otherwise the time until it, `1d18h` from 24 h on, `5h40m` below.
- One rule for every reset shown (pop-up, tray menu, CLI): under 24 h hours and minutes, 24–48 h days and hours, beyond 48 h weekday and clock time. Minutes round up, and a reset time is shown rounded up to the full minute: the server sends a moment just before the boundary (`00:59:59.59`), which is meant as `01:00`.
- **Week, scoped:** one row per scoped weekly limit (e.g. Fable), percentage only.
- **Binding limit:** the one with `is_active: true`, or else the highest percentage. This drives the tray icon.
- **Usable:** the login works, the usage is known, and no limit blocks the account. The week is below 100 %, the 5 h session below 100 %, and a per-model weekly limit below 100 % while a running session uses that model. A per-model limit at 100 % blocks the account only while a session on that model is open (busy or idle); with `readSessions` off, it blocks only while the server marks it as the binding one.
- **Which limit blocks:** when several limits are at 100 %, the one with the latest reset time is named; one without a reset time comes before one with a time. On a tie the week comes before a model, and a model before the 5 h session.
- **Extra usage / spend:** only if enabled.

### 7.3 Colours by load

| Load | Colour | Meaning |
|---|---|---|
| < 70 % | green `#22c55e` | fine |
| 70–89 % | amber `#f59e0b` | getting close |
| ≥ 90 % or `severity: critical` | red `#ef4444` | about to hit the limit |
| 100 % / locked | red + lock glyph | limit reached, shows the time until reset |

### 7.4 Auto-burn and switch suggestions

Auto-burn is one switch, `autoBurn`, off by default. On, seatswap switches by itself; off, with
`suggestSwitches` on (default), it says what it would do and why, with a [Switch] button. There are
no modes and no tuning settings: every threshold is a constant in the code (see below and
`docs/implementation.md`).

| # | Trigger | What happens |
|---|---|---|
| S1 | The active account is blocked, or there is none | switch at once to the usable account with the least slack |
| S2 | Another usable account has clearly less slack | only while early switching is on: switch to it at a quiet moment (below) |
| S3 | Nothing is usable | no switch; the "All accounts are full" notification of §7.5, and the reset poll of §7.1 |

- **Blocked** means the week or the 5 h window is at 100 % or more, the login has ended, or a
  per-model limit at 100 % blocks the account while a session on that model is open (§7.2).
- **Least slack** is the work the account can still take before its weekly reset minus the work
  needed to empty it. A capped account can only take a window's worth at a time, so its remaining
  5 h windows count, not just its percent. An account whose rates are not learned yet is played
  like one without rates: the fixed order is used instead, capped accounts with an open window
  first, then the others, each by the earlier weekly reset and then the account id.
- **S2** needs another usable account whose slack is at least 2 typical working hours below the
  active account's, and at least 30 minutes since the last switch of any kind. It switches only
  when the expected saved hours are worth it (below).
- **Seeing 100 % quickly:** the active account is polled every 30 s while its binding limit is at
  90 % or more and below 100 % (§7.1), and the status line feed (§7.6) brings the live numbers
  without a request.
- An account with `autoBurn` off is never a target and is never suggested. When it is the active
  account and gets blocked, S1 still leaves it (§7.4.4). While such an account is not blocked,
  auto-burn stays on it: S3 is reached only when the active account is blocked (or there is none)
  and `choose` finds no usable account. An active account auto-burn may not use is also not a
  reason for S2.

#### 7.4.1 The regime

Once an hour seatswap plans the next 7 days twice: the **lazy** plan stays on an account until it
is blocked and then takes the one with the least slack; the **eager** plan also takes the usable
account with the least slack when it has at least 2 typical working hours less slack, with at most
one switch per 30 minutes. Early switching (S2) is on only when all of these hold:

- every account auto-burn may use has learned rates, and the rhythm has a typical working hour;
- the measured cost of a switch has at least 3 samples;
- the eager plan blocks at least 0.5 typical working hours less than the lazy plan;
- those saved hours are at least twice the extra switches' cost (extra switches × the measured
  cost of one switch, in working hours).

While any of these is missing, auto-burn switches only when the active account is full. The
regime's words are `learning`, `no switch measured yet`, `switching before 100 % does not pay` or
`switching before 100 % pays`.

#### 7.4.2 Quiet moments

A switch makes a running session send its conversation again, and on some models it continues
without its earlier thinking (`thinking_tied` in `docs/implementation.md`). S2 therefore waits
while sessions on such a model are busy: the decision is a *waiting switch* to the target, with the
latest time at which the rhythm's work from now reaches the target's slack (the end of the 7-day
horizon when it never does). The pop-up
shows `A switch to AC is waiting: 2 sessions on a model that would lose its thinking are busy. At
the latest 18:40.` and the switch happens when those sessions are no longer busy, or at the latest
time. A target with no slack left is switched to at once instead of waiting. S1 and S3 never wait.
With `readSessions` off there is no session count and S2 does not wait.

#### 7.4.3 Working by hand

- **Manual switch while auto-burn is on.** A switch by a person (`seatswap use`, `next` or the
  pop-up) holds S2 back for 4 hours, or until the account switched to is blocked, whichever comes
  first. S1 is not held back.
- **Pause.** "Pause 4 h" in the pop-up writes an `autoBurn` event with `pausedUntil`; "Resume"
  writes one with no end. The pop-up shows `Auto-burn paused until 18:10 · Resume`. A pause holds
  S2 like a manual switch; a resume also ends a manual hold. Turning `autoBurn` on or off in the
  settings writes `autoBurnSet`, which is neither a pause nor a resume and leaves both alone.

#### 7.4.4 Per account

An account carries `autoBurn` ("Auto-burn may use this account"), default on. Off, auto-burn never
switches to it and never suggests it. If it is the active account and gets blocked, S1 still
leaves it; if every other account is also unusable, S3 applies.

#### 7.4.5 The texts

Times are local `HH:MM`, or `Mon HH:MM` when not today; durations are `35m`, `2h`, `2h05m`,
`1d3h`.

- **The line under the switch:** `On PR until its week is full, about 2h. Then AC.`, `On AC until
  its 5 h window is full, about 1h30m. Then LB.`, `On PR. No switch planned this week.` While
  rates are missing: `Learning how fast AC, LB are used. Until then: switch only when full.`
- **An automatic switch** (when `notifyOnSwitch` is on): `Auto-burn: now on PR. AC is full until
  14:40.` or `Auto-burn: now on PR: its week resets on Mon 02:00.`
- **A suggestion** (auto-burn off): `Switch to AC now: its 5 h window is open and its week resets
  on Mon 02:00.` When the lazy plan expects more than 1 % of the target's week to be left unused
  at its reset, it adds ` Staying on PR is expected to let 12 % of AC expire.` The pop-up shows
  the same sentence and a [Switch] button; the desktop notification is sent once per target and
  hour.
- **The reason of a switch:** `AC is full until 14:40`, `its 5 h window is open and its week
  resets on Mon 02:00`, `its week resets on Mon 02:00`, or `no account was in use`.
- While auto-burn is on, no suggestions are sent: it would ask for what auto-burn does anyway.

#### 7.4.6 How old a reading may be

A decision needs a reading at most 20 minutes old for every account whose login still works,
whether or not auto-burn may use it; the oldest of those readings is the one tested. An account
that auto-burn may use, or the active one, with a login but no reading at all also stops a
decision. The reading's age is the later of its
poll and its feed (`fetchedAt` in the cache). When the oldest is older than 20 minutes, auto-burn
does not switch or suggest: it asks for a poll, at most once a minute, and the engine polls only
the accounts whose last reading or attempt is older than 20 minutes. The status line feed (§7.6)
is a reading too, so a running session can keep the numbers fresh without a request.

### 7.5 Notifications about limits and waiting

- **Usable again.** When an account that a limit blocked is usable again and is not the active account, seatswap sends `PR is usable again: its 5 h window reset.` The reason is `its 5 h window reset`, `its week reset` or `its {model} week reset`. It is sent at most once per account and reset minute, and only while `notifyUsableAgain` is on. The poll 30 s after the reset time (§7.1) is what makes it timely. It is sent only when the block ended because its reset time passed (the previous `blocked_until` is at or before now): a per-model block whose last session on that model closed, or a block the server lifted early, sends nothing and does not use up the once-per-reset.
- **All accounts are full.** When every account whose login still works is blocked by a limit, and the previous snapshot had a usable account, seatswap sends `All accounts are full. Next: AC at 14:40.` once; without a next reset time it is `All accounts are full.` An account whose usage is not known yet is not usable but not blocked either, so it does not count as full. Only accounts whose login still works count.
- **Waiting for you.** A session that is still waiting 15 s after it entered `waiting` sends `acme-kb-dd is waiting for you (acme-kb, on atlas).` The label is the session's name, or `<project>-<first two hex digits of the session id>` when the session has no name. The host part is left out when this host's name cannot be read: `acme-kb-dd is waiting for you (acme-kb).` It is sent once per session and wait, and only while `notifyWaiting` is on. When the session stops waiting, its notification is withdrawn.
- A waiting notification does not time out where the desktop allows that (Linux). On Windows it follows the system's rules; the pop-up keeps the count either way. A click opens the pop-up.

### 7.6 `seatswap statusline` and the feed

`seatswap statusline` is the command Claude Code runs every 2 seconds instead of a shell script. It
reads Claude Code's JSON from standard input, seatswap's layout from its settings, and the usage
from its cache, and prints one line.

- **What it reads from Claude Code:** `workspace.current_dir`, `model.id`, `model.display_name`,
  `effort.level`, and `rate_limits.five_hour` and `.seven_day`, each with `used_percentage` and
  `resets_at` in seconds since the epoch. Unknown keys are ignored. A missing key, or a `null`
  whole object such as `"five_hour": null`, is absent; a `null` inside a limit makes the whole
  payload malformed. Anything that is not a JSON object of this shape is treated as an empty
  payload: a malformed payload must not hide the line.
- **What it prints:** the layout `statusline: {left, right, margin}`. The preset "My current line"
  is `sam@atlas:~/projects/atlas Fable 5.2 high` on the left and `AC 12% 4h45m  48% 3d16h` on the
  right. A segment without a value prints nothing, with its prefix and suffix. The right part is
  flush with the terminal's right edge minus the margin; when the line does not fit, segments are
  dropped by their `drop` number (1 first) until it does, and when nothing is left to drop it is
  printed anyway. Without a width the right part follows the left after two spaces.
- **The width** is the terminal of the nearest ancestor process that has one, else `$COLUMNS`,
  else none.
- **Never slow, never a failure:** it uses no network, takes no lock that can wait, does not build
  the engine or the HTTP client, and always exits 0. Any error becomes the line itself:
  `seatswap: <reason>`, because a non-zero exit makes Claude Code hide the whole line. `CLAUDECODE`
  may be set; the command is never gated and writes no `agentCall` event.
- **The preset and the segments:** the preset reproduces the old script. The layout can also hold
  the account (abbreviation in its colour), plan, 5 h percent and reset, week percent and reset,
  per-model percent, auto-burn, model, effort, folder (`~` short), git branch, `user@host`, clock,
  free text and a separator. Each segment has an optional colour rule (fixed, by load with two
  thresholds, by model family, by effort, or the account's colour), prefix, suffix and drop order;
  reset segments can use the compact time left, seatswap's own format (§7.2) or a clock time. The
  `autoBurn` segment has no value in this build: the next account is in `status --json` (`next`)
  and in the pop-up, not in the line.

**The feed.** When `statuslineFeed` is on (default) and the payload has `rate_limits` with a
`seven_day`, the command writes the live numbers to `<data folder>/feed.json`, atomic and mode
0600:

```json
{"at":"2026-11-15T09:41:12Z","fiveHour":{"percent":12.0,"resetsAt":"2026-11-15T14:30:00Z"},
 "sevenDay":{"percent":48.0,"resetsAt":"2026-11-16T01:00:00Z"},"model":"claude-fable-5-2"}
```

- It rewrites the file only when the numbers changed or the file is at least 10 seconds old, and
  never writes a partial feed: a payload without `seven_day` writes nothing.
- **Which account do the numbers belong to?** The payload does not name the account. A session
  that has not sent a request since a switch still carries the old account's numbers. The numbers
  are therefore accepted only for the one stored account whose cached weekly reset is within 60
  seconds of `sevenDay.resetsAt`; none or more than one match means they are dropped. They are
  never stored for the new account because a session still carries the old one.
- **The tray app takes it.** After a write the command sends `feed` on the existing single-instance
  socket; no tray app running is not an error, the file waits. The tray app reads the file, merges
  it into that account's cache entry only when `at` is later than both the poll's `fetchedAt` and
  the last feed, and writes a usage sample with `src: statusline` like a poll. The 5 h percent and
  its reset follow the feed when its window is new or the same window is not lower; the weekly
  percent follows it only when it is not lower, and the weekly reset stays the cache's. Per-model
  limits stay the cache's. The polls stay as they are: they bring the per-model limits and the
  inactive accounts.
- **Effect:** the active account's numbers are seconds old instead of up to 2 minutes, without an
  extra request to Anthropic; auto-burn sees 100 % sooner. With `statuslineFeed` off the command
  writes no feed file and sends nothing.

### 7.7 `seatswap burn plan` and `seatswap burn simulate`

Both commands only read; for an agent they follow `agentsRead` like `status` and `list`.

- **`seatswap burn plan [--json]`** prints the early-switching regime and the lazy plan, read the
  same way the tray app reads them (with no session count, so `tied_busy` is 0). The text form is
  the regime's words, then one line per segment: `Mon 08:00–Mon 10:00  AC`, or `… nothing usable`.
  `--json` prints one object with the regime (`early`, `why`, `blockedLazyH`, `blockedEagerH`,
  `extraSwitches`, `costH`), the segments (`account`, `from`, `to`; `account` is null when nothing
  is usable) and `leftAtReset` (week percent left at each account's first reset). An account
  auto-burn may use (or the active one) with a login but no reading, or no account with a login
  and a reading, is an error: there is nothing to plan from.
- **`seatswap burn simulate --from YYYY-MM-DD --to YYYY-MM-DD [--without ABBR]…`** replays the
  recorded work of those local days (both inclusive) once for each of the two policies, **lazy**
  and **eager**, and prints for each the hours worked, the hours blocked, the number of switches,
  and the week percent that expired at each account's reset. `--without` leaves an account out.
  The replay starts at the first sample of each account in the range, starts with no active
  account, and uses the recorded work hour by hour; an account without learned rates never fills.
  The rates are learned from the history as the tray app learns them. Warnings from a torn history
  line go to standard error.

### 7.8 `status --json` and agents

`seatswap status --json` prints one object:

- `views[]`: every account, with `id`, `label`, `abbr`, `color`, `email`, `plan`, `autoBurn`,
  `isActive`, `loginState`, `loginEndsAtMs`, `loginDaysLeft`, `usage`, `usable`, `blockedUntil`,
  `fetchedAt` and `lastError`, plus `slackHours` while it is known (the working hours the account
  can be left alone and still be emptied before its reset).
- `active`: the id of the live account, or null.
- `suggestion`: `{toId, text}` or null. It is the same suggestion as §7.4.5, only while auto-burn
  is off and `suggestSwitches` is on.
- `autoBurn`: `{on, pausedUntil}`, where `pausedUntil` is null when there is no pause or manual
  hold.
- `next`: the id auto-burn would switch to (the target of a switch or a waiting switch, otherwise
  the first account of the lazy plan other than the active one), or null.
- `burnError`: a history or planner error as text, or null. When it is set, `next` is null and the
  views have no `slackHours`; the command still answers.

Only the JSON form reads the history; `--waybar` and `--i3blocks` do not, because they run every
few seconds. With auto-burn on, `seatswap next` called by an agent prints
`seatswap: auto-burn is on and decides` and exits 0 without switching or touching a login; the
agent call itself is recorded as usual. A person's `next` still switches.

---

## 8. Tray icon

### 8.1 What it shows

```
 22 px icon (Linux panel)     what each part is

    ╭──────╮                   ring   = binding limit of the active account, clockwise
   ╱  ▓▓▓▓ ╲                            from 12 o'clock, coloured per §7.3
  │   AC    │                  letters = the active account's abbreviation, white or black,
   ╲       ╱                             whichever contrasts best with the account colour
    ╰──────╯                   disc   = account colour
                               (lock badge bottom right at 100 %, grey ring while unknown)
```

- The icon is drawn at run time with `tiny-skia` and a bundled font, at each size the platform asks for (16, 20, 22, 24 and 32 px, and @2x/@3x). It is redrawn when the usage or the account changes. On Cinnamon a new icon every 0.5 s showed at once, at 1–2 ms per update *(tested)*.
- **Text next to the icon, where the platform shows it:**
  - macOS: the menu bar title.
  - Linux: the AppIndicator label. **Cinnamon does not show it** *(tested, Cinnamon 6.6.9)*: the app publishes the label, but Cinnamon's tray bridge (`xapp-sn-watcher`) hands an empty one to the panel. There the ring carries the number, and the tooltip once the app publishes one (below).
  - The text reads `97%`. A setting `trayText` offers `percent`, `percent+reset` (`97% · 1h12m`) or `off`.
- Tooltip: "AC · Acme — session 97 % (resets 14:40) · week 48 %".
- **High-contrast / monochrome option:** macOS "template" style, or a plain white icon for dark panels. The ring stays, the letters become cut-outs.

### 8.2 Abbreviations

- **Up to 2 characters**, so the label stays legible at 16 px (the Windows tray at 100 % scaling). Three characters don't fit in a 16–22 px disc at a readable size. Allowed: letters A–Z and digits, stored in upper case. One character is drawn larger.
- Default: the first letters of the first two words of the label ("Acme Team" → `AT`), or the first two letters of a one-word label ("Acme" → `AC`, "Private" → `PR`). The examples in this spec show `P` for the private account; that one was shortened by hand in the editor (§11.3).
- Two accounts can't share an abbreviation (checked when editing).
- **Fallback at 16 px:** if a measured glyph width doesn't fit, the font becomes condensed, never smaller than 9 px. The colour still tells the accounts apart at a glance.

### 8.3 Click behaviour

| Platform | Left click | Right click |
|---|---|---|
| Windows | opens the pop-up | menu |
| macOS | opens the pop-up | menu |
| Linux (StatusNotifierItem, via tray-icon's `ksni` backend) | opens the pop-up (`ItemIsMenu` is false, so the host calls `Activate`); confirmed on Cinnamon | menu |

**Linux menu:**
```
  ● AC  Acme                97 % · 5 h   ✓
  ● P   Private             12 % · 5 h
  ───────────────────────────────────────
  Open panel…                       ⌃⌥C
  Add account…
  Settings…
  Quit
```

- Clicking an account switches to it.
- The usage text in the menu updates live.
- "Open panel…" opens the pop-up.
- An optional global shortcut (default Ctrl+Alt+C, configurable) opens the pop-up on every platform.
  - **Linux v1:** the app registers no shortcut itself. The person binds `seatswap panel` to a key in the desktop's keyboard settings; that command asks the running tray app to open its pop-up. It works the same on X11 and Wayland. A built-in shortcut would need Tauri's global-shortcut plugin, which is not in the project yet (and cannot grab keys on Wayland).

---

## 9. Platform support

### 9.1 Matrix

| Platform | Tray mechanism | Expected | Notes |
|---|---|---|---|
| **Linux Mint Cinnamon (X11)** — reference | AppIndicator via XApp | full, without the label | icon updates tested; no label (§8.1); the pop-up can be placed at the mouse position |
| KDE Plasma (X11/Wayland) | StatusNotifierItem | full | label goes into the tooltip |
| GNOME (Ubuntu) | AppIndicator extension, preinstalled | full | |
| GNOME (Fedora, Debian, vanilla) | needs the "AppIndicator and KStatusNotifierItem Support" extension | menu and pop-up once installed | first run detects a missing tray and says how to add it; the CLI works either way |
| XFCE, MATE, Budgie, LXQt | SNI / AppIndicator panel plugin | full | |
| Sway, Hyprland, other wlroots | Waybar / other bar with an SNI tray | full | also `seatswap status --waybar` for a native bar module |
| i3 / bars without SNI | none | CLI + `--waybar`/`--i3blocks` output | |
| **macOS 12+** | NSStatusItem | full | Keychain prompt on first access ("Always Allow") |
| **Windows 10/11** | notification area | full | tray icons hide in the overflow by default; first run tells you to pin it |

### 9.2 Placing the pop-up

- **Windows and macOS:** anchored to the icon, using the tray rectangle that Tauri reports.
- **Linux X11:** at the mouse position, kept inside the monitor that has the mouse, and flipped to sit above or below depending on whether the panel is at the top or bottom.
- **Linux Wayland:** windows can't place themselves, so the compositor decides. The pop-up is a centred, borderless card, like a launcher. It closes on Esc or when it loses focus.

### 9.3 Optional native front ends (later)

Each one is only a view onto `seatswap status --json` and `seatswap use`:
- Cinnamon applet (JavaScript, in the Spices format).
- GNOME Shell extension.
- KDE Plasma widget.

They give real click handling and exact placement on those desktops. They're not needed for v1.

### 9.4 Packaging

- **Linux:** `.deb`, `.rpm`, AppImage, and an AUR `PKGBUILD`. Autostart through `~/.config/autostart/seatswap.desktop`.
- **macOS:** signed `.dmg` plus a Homebrew cask. Login item via `SMAppService`.
- **Windows:** two programs from one code base: `seatswap.exe` (terminal, CLI) and `seatswap-tray.exe` (the same program built without a console window, for autostart and the Start menu). The first release is a zip with both; an MSI or NSIS installer and a winget manifest come later. Autostart through the value `seatswap` under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, which starts `seatswap-tray.exe` with the same `--data-dir` / `--claude-config-dir` flags.
- **CLI only:** `cargo install seatswap --no-default-features`.

---

## 10. Settings

| Setting | Default | |
|---|---|---|
| `abbr`, `label`, `color`, `autoBurn` per account | from the account | `autoBurn`: auto-burn may use this account, §7.4.4 |
| `trayText` | `percent` | `percent` · `percent+reset` · `off` |
| `iconLimit` | `binding` | which limit the ring shows: `binding` · `session` · `week` |
| `pollActive` / `pollInactive` | 120 s / 900 s | |
| `autoBurn` | off | auto-burn switches by itself, §7.4 |
| `suggestSwitches` | on | when auto-burn is off, say what it would do, §7.4 |
| `statuslineFeed` | on | `seatswap statusline` writes the live numbers to `feed.json`, §7.6 |
| `statusline` | "My current line" | the status line layout: `left`, `right`, `margin`, §7.6 |
| `notifyOnSwitch` | on | |
| `mirrorLive` | on | §6.4 |
| `refreshInactive` | on | refresh the logins of inactive accounts (§6.3). Off: inactive accounts show their last known usage with its age |
| `hotkey` | Ctrl+Alt+C | Linux v1: not used; bind `seatswap panel` in the desktop instead (§8.3) |
| `startAtLogin` | on | Linux: `~/.config/autostart/seatswap.desktop`; Windows: the `Run` key value `seatswap` → `seatswap-tray.exe` (§9.4) |
| `theme` | system | system · light · dark |
| `claudeConfigDir` | `$CLAUDE_CONFIG_DIR` or `~/.claude` | |
| `historyDir` | `null` (`<data folder>/history`) | text, an absolute folder; point it at a synced folder to share the history with other hosts |
| `hostName` | `null` (the machine's host name) | text, 1–63 characters of `A–Z a–z 0–9 . _ -` that must start with a letter or a digit, stored in lower case; the name this host writes its history under |
| `readSessions` | on | read Claude Code's session files and transcripts: model, busy or idle, waits and tokens per folder. Message text is never stored |
| `notifyUsableAgain` | on | the "usable again" notification (§7.5) |
| `notifyWaiting` | on | the "waiting for you" notification (§7.5) |
| `notifyWaitingHosts` | `thisHost` | `thisHost` · `allHosts`: whose waiting sessions notify; only this host's are known yet |
| `agentsRead` | on | agents may run `list`, `status`, `burn plan` and `burn simulate` (§6.6, §7.7) |
| `agentsSwitch` | off | agents may run `use` and `next` (§6.6); an agent's `next` does nothing while auto-burn is on (§7.8) |

The former `suggestAt` and `suggestTargetBelow` (the fixed 90 % / 50 % suggestion) are gone. An
older `accounts.json` that still has them loads: unknown keys are ignored, and the two settings
default to auto-burn off and suggestions on.

---

## 11. The pop-up ("panel")

### 11.1 Look

The target is somewhere between the macOS Control Centre and Raycast: one floating card, calm until something needs attention.

- **Card:** 380 px wide, height fitted to the content, 16 px corner radius, 1 px hairline border.
  - Translucent blur where the platform has it: vibrancy on macOS, Mica/Acrylic on Windows 11 (the `window-vibrancy` crate).
  - On Linux, a solid surface with 96 % opacity.
- **Themes:** dark by default when the system is dark; light follows the system.
  - Dark: background `#0f1115`, cards `#171a21`, text `#e7e9ee`, muted text `#8b93a7`.
  - Light: background `#f6f7f9`, cards `#ffffff`, text `#0f1115`.
- **Type:** Inter, or the system UI font as a fallback. Numbers use tabular figures so percentages don't jitter.
- **Gauges:** two concentric rings per account. The outer ring is the week, the inner the 5 h session; the week's percent is the large figure in the middle. The bars beside the gauge follow the same order: week, then session, then the per-model limits. On a plan without a session limit the inner ring is left out (no grey track) and the week's figure sits centred.
  - Rings fill with an ease-out animation (400 ms) when the pop-up opens and when values change.
  - Colours follow §7.3, drawn as a soft gradient along the arc, with a faint glow on the red state.
  - Clicking the gauge of an account that is not in use switches to it, like its Switch button. The pointer becomes a hand over it.
- **Account colour:** a 3 px accent bar on the card's left edge and the abbreviation badge.
- **Active account:** at the bottom. The card is raised (slightly lighter), with a subtle ring in the account colour and an "Active" pill.
- **Closing:** Esc, the × in the header, a click anywhere else (the pop-up loses focus), or a second left click on the tray icon. The pop-up must actually get the keyboard focus when it opens, or none of the first three work (on Cinnamon it did not at first, §17).
- **Motion:** after a switch, the cards trade places with a 250 ms spring animation. The ticking reset countdowns ("1h12m") update every minute without flicker. All animation is off when the system asks for reduced motion.
- **Keyboard:** ↑/↓ moves between accounts, Enter switches, 1–9 switch directly, `,` opens settings, Esc closes. The cursor (a blue ring) appears only with the first ↑/↓, starting from the account in use, and is gone again each time the pop-up opens; until then Enter does nothing. Clicking a card does not move it.

**Sessions line.** In the footer next to "+ Add account" the pop-up reads `9 sessions · 2 busy` (the mock-up shows it without a waiting session), and while someone waits ` · 1 waiting for you` in the warning colour. With `readSessions` off there is no line. When the session files cannot be read, their reason stands there muted instead; a newer Claude Code adds a small ⚠ whose tooltip is the warning text. The numbers do not depend on the pop-up being open.

### 11.2 Mock-up

```
╭──────────────────────────────────────────────────────╮
│  seatswap                           ⟳ 12 s ago  ⚙  ✕ │
│                                                      │
│ ▌╭────╮  P   Private                       [ Switch ]│
│ ▌│ 31 │  …@example.com · Ultra                       │
│ ▌│    │  Week      ██████▏                31 %  Thu  │
│ ▌╰────╯  Opus      ██▍                    12 %       │
│                                                      │
│ ▌╭────╮  AC  Acme ✎                       ● Active   │
│ ▌│ 48 │  someone@… · Team                            │
│ ▌│ 97 │  Week      █████████▋             48 %  Sun  │
│ ▌╰────╯  Session   ████████████████████▉  97 % 1h12m │
│ ▌        Fable     ████████▋              43 %       │
│                                                      │
│  ⚠ AC is almost out. P has room — switch?   [Switch] │
│                                                      │
│  + Add account                   3 sessions · 1 busy │
╰──────────────────────────────────────────────────────╯
```

### 11.3 Account editor (inline, opens on clicking the badge or the ✎ next to the label)

The ✎ shows while the pointer is on the card or the card has the keyboard focus. The ⚙ in the header is hidden while settings are open.

- Label (free text).
- Abbreviation: two boxes, live preview of the tray icon at 16, 22 and 32 px.
- Colour: 10 preset swatches plus a custom hex value.
- "Remove account", which asks first. It deletes the account from the keyring and list. The live login is never touched.

### 11.4 Settings view

The gear opens the settings in the same card, in the same style, with the existing setting rows:

- **History.** "Name of this host" (text; empty = the machine's name), "History folder" (text; empty = seatswap's own folder), "Read Claude Code's session files" (switch; sub-text "model, busy or idle, tokens per folder · never message text").
  - Note under the group: "Takes effect after restarting seatswap."
- **Notifications.** "An account is usable again" (switch), "A session is waiting for you" (switch).
- **Auto-burn.** "Auto-burn" (switch), "Suggest switches" (switch) with "while auto-burn is off"
  under it. The account editor also carries "Auto-burn may use this account"; the same switch is on
  the account card in the pop-up.
- **Agents.** "Agents may read usage" (switch), "Agents may switch accounts" (switch), and under them the text of §6.6.

---

## 12. Later (v2+)

- In-app login for new accounts (OAuth PKCE), so `/login` in Claude Code is no longer needed.
- Claude Desktop: research whether its login can be switched safely (separate `--user-data-dir` per account and swapping which one the launcher uses is the likely route).
- Usage history: a small sparkline per account across the last 7 days, from the poll cache.
- The status line builder: a button that writes the `statusLine` key into Claude Code's
  `settings.json`, with the change shown first and a restore. Until then the line is set by hand
  (§7.6).
- Native Cinnamon, GNOME and KDE front ends (§9.3).

---

## 13. Security and privacy

- Logins go only into the OS keyring, or a `0600` file if there is none. They are never logged; log lines print the account ID and the token's last 4 characters at most.
- Network traffic goes only to `api.anthropic.com` and `platform.claude.com`. No telemetry, no update pings unless enabled.
- Every write to Claude Code's files is atomic (temp file + rename) and keeps the original file mode.
- The status line reads Claude Code's JSON, seatswap's settings and its usage cache; its only write
  is `<data folder>/feed.json` (mode 0600). It prints no token and sends none.
- Before seatswap first writes to them, it backs up `.credentials.json` and `.claude.json` once as `*.seatswap-backup`.
- seatswap records the Claude Code version it was tested with. If the installed version is newer, the pop-up shows a warning, because the rules in §2.6 may have changed.
- Each account must be a licence the user holds. The app moves the user's own logins between the user's own slots and shares nothing.

---

## 14. Phase 0: what was verified, what is left

Run on 2026-11-10 with two real accounts in a separate config folder. Evidence and scripts: `phase0/RESULTS.md`.

| # | Item | State |
|---|---|---|
| 1 | End date of a login | **Done.** Fixed; a refresh does not move it (§2.1, §6.3). |
| 2 | Round trip with two accounts and a session running | **Done.** Works, also in the middle of a turn (§6.5). |
| 3 | Claude Code's locks | **Done.** Three locks; Claude Code waits for each (§2.6, §6.2). |
| 4 | Organization caches | **Done** for Team ↔ personal: Claude Code handles them (§2.4). Open: two Team or Enterprise organizations. |
| 5 | Usage endpoint | **Partly.** Works with a refreshed login; a limit can be missing (§3). Open: which scope it needs, its rate limit, the values `severity` takes near and at 100 %. |
| 6 | macOS | Open: the Keychain names from §2.2, and whether updating the item in place keeps Claude Code's access to it without a new password prompt. |
| 7 | Windows | **Done** (checked 2026-11-11, Claude Code 2.1.322 on Windows 11): `%USERPROFILE%\.claude\.credentials.json` and `%USERPROFILE%\.claude.json`, the same keys and format as on Linux; nothing in Windows Credential Manager; the same lock folders. Port: `docs/windows.md`. |
| 8 | Tauri tray on Cinnamon | **Done** with the `ksni` backend: the icon updates live, left click opens the pop-up at the mouse, right click shows the menu, the tooltip shows. The label next to the icon does not show (§8.1). Open: whether an open menu refreshes its text. |
| 9 | Sessions inside VS Code | Expected to follow a switch like a terminal session (experience from daily use, §6.5). Test postponed until no work sessions are running. |

## 15. Rough estimate (a guess, not measured)

- Phase 0: done on 2026-11-10, except the open items in §14.
- Linux v1 (core, CLI, tray, pop-up, Mint/KDE/GNOME testing): about 1 week.
- macOS + Windows (Keychain, packaging, signing): about 1 more week.
- One developer with Claude.

## 16. Remaining risks

| Risk | What it would mean | What the design does |
|---|---|---|
| A login has a fixed end date (§2.1, confirmed) | The app cannot keep logins alive; it warns before a login ends. Each account needs `/login` about every 4 weeks. **Accepted 2026-11-10:** this is the same as today without the app. | The warnings in §6.3. The in-app login stays in v2 (§12). |
| Every refresh uses up the old tokens (§2.1) | If seatswap refreshes an inactive login and cannot save the answer (crash, keyring error), that account is logged out and needs `/login`. | Save first (§6.3). A setting can turn the refresher off. |
| Undocumented endpoints, and seatswap calls the token endpoint with Claude Code's client ID | Anthropic can change or block this in any release. Refreshing a login from another program may also be against the terms of use; this is not checked. The worst case is that an account gets flagged. | All endpoint code sits in one module. Only inactive logins are refreshed. A setting can turn the refresher off; then inactive accounts show their last known usage with its age. |
| Everything marked *(code)* or *(tested)* comes from one version of the program (2.1.322) | A later Claude Code version may lock, re-read or save differently, and a switch could then lose a login. Several versions can also run at once: on this machine the VS Code extension brings its own 2.1.321. | seatswap records the Claude Code version it was tested with and warns when the installed version is newer. The backup from §13 is kept. |
| A switch moves all running sessions (§6.5) | A long agent run continues on the other account, with its settings and limits. | The pop-up says so before the switch. Per-terminal accounts are a non-goal (§1). |
| Claude Code can move the Windows login into Windows Credential Manager (`docs/windows.md` §13) | Version 2.1.322 has this store behind a server-side flag (`tengu_windows_credman`, off on the test machine on 2026-11-11). With it on, Claude Code reads the login from Credential Manager and deletes `.credentials.json` after its first save there; a switch on Windows then changes nothing. | seatswap reports "no login" instead of failing silently. Support for that store is not built; it is the Windows counterpart of the macOS Keychain work (§2.2). |
| Linux tray limits (§8.3, §9.2) | A desktop without a StatusNotifierItem host shows no icon at all; on Wayland the pop-up cannot sit at the icon. | The menu and `seatswap panel` reach the pop-up too. Native applets later (§9.3). |

## 17. Known issues

From daily use on Linux Mint 22 Cinnamon (X11) and the headless test runs, as of 2026-11-11.

**Fixed, kept here because the cause is easy to bring back:**
- *The pop-up did not close* (Esc, click elsewhere). Muffin, Cinnamon's window manager, refuses a focus request without a timestamp and ignores the first one while the window is still being mapped, so the pop-up never had the keyboard focus. Now it asks with the X server's time and repeats the request after mapping until it has the focus (`panel::focus_when_mapped`).
- *The pop-up flashed white at its old place before appearing.* It was shown first and moved and resized afterwards. Now it is built hidden at startup and only shown once the page has measured its height and it sits at its final place (`pending_show`, see the panel section of `docs/implementation.md`).
- *A second start did not find the first* when `--data-dir` named the same folder differently (relative and absolute): the socket's name was a hash of the spelling. Now it is a hash of the folder's real path (`AppPaths::gui_socket`).
- *The reset-time rule existed twice*, in Rust for the tray and the CLI and in the pop-up's JavaScript, and the suggestion's clock time already differed (no weekday beyond 24 h). The pop-up now asks the Rust side for every reset text, and since 2026-11-11 also for its "12 s ago" ages (`panel::time_texts`, `format::format_age`).
- *Picking a value in a settings drop-down closed the pop-up.* WebKitGTK shows the list as its own window that holds the input, so the pop-up lost the focus for as long as the list was open; the 150 ms recheck could not cover that. Now focus loss is ignored while a pop-up of seatswap holds the input (`panel::own_popup_open`). Reproduced and checked headless with Muffin, 2026-11-11.
- *Without a tray on the desktop the app crashed at startup* (no StatusNotifierWatcher on the session bus: GNOME without the AppIndicator extension, or the desktop's panel starting after seatswap at login). Now it keeps running without an icon, says so, and shows the icon as soon as a tray appears (`tray::start`); `seatswap panel` works meanwhile.
- *At every login on Linux Mint a "no system tray" notification appeared*, although the icon came a moment later. Cinnamon starts its tray (`xapp-sn-watcher`) and seatswap's autostart entry in the same second, so seatswap's first try often comes before the tray. Now it waits 30 s for a tray before it notifies (`tray::TRAY_GRACE`); the icon still appears the moment the tray does. Checked headless with a tray that comes 3 s late (no notification) and with none (notification after 30 s), 2026-11-11.
- *Inactive accounts kept an old plan label.* The plan was only recomputed when an account was live; the refresh of inactive logins skipped it. All three places that store a login now go through `Account::note_stored_login`.

**Not reproducible any more** (headless runs of 2026-11-11 with Muffin, with Metacity and with no window manager; both were seen before the focus fix above and were most likely the same cause):
- *Opening the pop-up while it is open closed it* (`seatswap panel settings` on an open pop-up). Now it stays open, switches the view and moves to the mouse; `seatswap panel` twice does the same.
- *The pop-up closed as soon as the mouse left it.* Now it stays open until a click elsewhere, Esc or ×.

**Open:**
- No built-in global shortcut (§8.3).
