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
      "email": "…", "orgName": "…", "plan": "Team 5x", "price": 180.0,
      "priceCurrency": "EUR",
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

**Money.** `prices.json` holds the API prices with their dates; `fx.json` the dollar rate with its dates. Both are written under the app lock, atomically and with mode 0600; their shapes are in §11.5.

**History.** seatswap keeps what it saw and did in a folder of its own: `<data folder>/history` (`historyDir` names another one). Every host writes only into its own subfolder `<host>/`, and only appends, so several hosts can share one synced folder without producing a conflict. `<host>/host.json` holds the id of the machine that writes there; when it names another machine, seatswap refuses to write into that subfolder and says why.

The history has seven kinds of file, one JSON object per line, one file per month and kind: `samples-YYYY-MM.jsonl` (one line per usage answer the tray app receives, per account, when a number or a reset time changed, and at least once an hour while the app runs), `windows-YYYY-MM.jsonl` (a window that ended), `events-YYYY-MM.jsonl` (a switch; a limit crossing 100 %, as `blocked`, `unblocked`, `allBlocked` or `anyUsable`; an `accountAdded` or `accountRemoved` when the CLI or the tray app creates or removes an account; a `loginEnded` or `loginRenewed` when the tray app's poll sees an account's login end or work again; a `planChanged` when the plan label changes; a `priceChanged` when an account's monthly price changes; `live`; an `agentCall`; a wait), `tokens-YYYY-MM.jsonl` (what the sessions used, one line per session, hour and model), `sessions-YYYY-MM.jsonl` (a session's summary when it changes and when it closes, also for a session that is seen in a transcript but no longer runs), `projects-YYYY-MM.jsonl` (the project log: one line per rename, join, unjoin, organization assignment or organization edit, §11.6), and `up-YYYY-MM.jsonl` (at most one line per 5 minutes while seatswap runs on this host). The CLI writes the account events (`accountAdded`, `accountRemoved`); the tray app's engine writes the login events (`loginEnded`, `loginRenewed`) with the poll's time, and the app writes `planChanged` and `priceChanged` when it stores a login or a price edit changes them.

Next to the history files, each host writes one live report into its own host folder, `<host>/sessions-now.json` (§11.4): what is running on that host, for the other hosts' Sessions area. It is not history: the import never copies it and a host folder's size leaves it out.

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
- While the active account's binding limit is at 90 % or more and below 100 %, it is polled every 30 s; at 100 % the reset poll above applies. The status line's feed (§7.6) can make the numbers newer still, without a request. The feed never moves a timer poll: the poll schedule keeps its times whatever the feed does.
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
- **Going back restores a session's earlier thinking.** When every usable account has a learned
  slack, the choice is not only the least slack. Every usable account whose slack is at most
  2 typical working hours above the least one is a candidate (two slacks within 1e-9 count as
  equal), and among the candidates the account that produced more of the thinking the running
  sessions still hold wins; with none, or with equal counts, the usual order decides: slack, weekly
  reset, account id. The count is per minute: for every running session on a model whose thinking is
  tied to the account (§2.11), each answer with thinking since the user's last message adds one to
  the minute of its start, and that minute belongs to the account that was live on this host then,
  as the switch history says. A minute without a known account is left out. It needs learned rates
  (without a slack there is nothing to compare), `readSessions` on, and the sessions thread's list of
  running sessions; the command line does not know the running sessions and plans without the
  counts. The plan and the decision use the same choice, so the pop-up names the account auto-burn
  will switch to.
- **An early switch still needs its distance.** S2 to the account that holds the thinking needs that
  account's slack to be at least 2 typical working hours below the active account's, as for any S2.
  Until it is, auto-burn stays, even when the account with the least slack would have passed; it
  moves when the account with the thinking is that far below, or when another account is clearly
  more urgent. The "wait for a quiet moment" rule of §2.12 applies as before. With `readSessions`
  off nothing is held back.

While early switching is on, the pop-up's next line, `burn-next.json`, the status line's
auto-burn segment and `status --json`'s `next` follow the plan auto-burn follows — the eager plan —
and otherwise the lazy plan that switches only at 100 %. That is the same plan the Forecast shows.
The suggestion's "Staying on …" sentence is the lazy plan's — the plan that switches only at
100 % — not the followed plan.

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

While any of these is missing, auto-burn switches only when the active account is full. With
`readSessions` off there are no work units: no rates are learned, the regime says `reading
sessions is off: no rates are learned`, and auto-burn switches only when the active account is full. The
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

- **Manual switch while auto-burn is on.** A switch auto-burn did not make — by a person
  (`seatswap use`, `next` or the pop-up), the CLI, an agent, or Claude Code's `/login` — holds S2
  back for 4 hours, or until the account switched to is blocked, whichever comes first. The hold
  ends for good at that first block and does not come back. S1 is not held back.
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
  its 5 h window is full, about 1h30m. Then LB.`, `On PR for about 2h. Then AC, before PR is full.`
  (the plan leaves PR before it fills), `On PR. No switch planned this week.` While rates are
  missing: `Learning how fast AC, LB are used. Until then: switch only when full.`
- **An automatic switch** (when `notifyOnSwitch` is on): `Auto-burn: now on PR. AC is full until
  14:40.` or `Auto-burn: now on PR: its week resets on Mon 02:00.`
- **A suggestion** (auto-burn off): `Switch to AC now: its 5 h window is open and its week resets
  on Mon 02:00.` When the plan that switches only at 100 % — the lazy plan, not the plan auto-burn
  follows — expects more than 1 % of the target's week to be left unused at its reset, it adds
  ` Staying on PR is expected to let 12 % of AC expire.` The pop-up shows the same sentence and a
  [Switch] button; the desktop notification is sent once per target and hour.
- **The reason of a switch:** `AC is full until 14:40`, `its 5 h window is open and its week
  resets on Mon 02:00`, `its week resets on Mon 02:00`, or `no account was in use`.
- While auto-burn is on, no suggestions are sent: it would ask for what auto-burn does anyway.

#### 7.4.6 How old a reading may be

A decision needs a reading at most 20 minutes old for every account auto-burn may use, plus the
active account; the oldest of those readings is the one tested. An account in that set with a
login but no reading at all also stops a decision. The reading's age is the later of its
poll and its feed (`fetchedAt` in the cache). When the oldest is older than 20 minutes, auto-burn
does not switch or suggest: it asks for a poll, at most once a minute, and the engine polls only
the accounts whose last reading or attempt is older than 20 minutes. The status line feed (§7.6)
is a reading too, so a running session can keep the numbers fresh without a request.

### 7.5 Notifications about limits and waiting

- **Usable again.** When an account that a limit blocked is usable again and is not the active account, seatswap sends `PR is usable again: its 5 h window reset.` The reason is `its 5 h window reset`, `its week reset` or `its {model} week reset`. It is sent at most once per account and reset minute, and only while `notifyUsableAgain` is on. The poll 30 s after the reset time (§7.1) is what makes it timely. It is sent only when the block ended because its reset time passed (the previous `blocked_until` is at or before now): a per-model block whose last session on that model closed, or a block the server lifted early, sends nothing and does not use up the once-per-reset.
- **All accounts are full.** When every account whose login still works is blocked by a limit, and the previous snapshot had a usable account, seatswap sends `All accounts are full. Next: AC at 14:40.` once; without a next reset time it is `All accounts are full.` An account whose usage is not known yet is not usable but not blocked either, so it does not count as full. Only accounts whose login still works count.
- **Waiting for you.** A session that is still waiting 15 s after it entered `waiting` sends `acme-kb-dd is waiting for you (acme-kb, on atlas).` The label is the session's name, or `<project>-<first two hex digits of the session id>` when the session has no name. The host part is left out when this host's name cannot be read: `acme-kb-dd is waiting for you (acme-kb).` It is sent once per session and wait, and only while `notifyWaiting` is on. When the session stops waiting, its notification is withdrawn. With `notifyWaitingHosts` set to all hosts, another host's waiting session notifies here too, with that host's name; it arrives with the delay of the shared history folder (`sessions-now.json`, §11.4), so the 15 s counts from the session's own `since`.
- A waiting notification does not time out where the desktop allows that (Linux). On Windows it follows the system's rules; the pop-up keeps the count either way. A click opens the Sessions area at that session, opened and marked for 3 seconds; a session that is no longer listed says so there.

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
  printed anyway. Without a width the right part follows the left after two spaces while both
  zones print something, and directly when one of them is empty.
- **The width** is the terminal of the nearest ancestor process that has one, else `$COLUMNS`,
  else none.
- **Never slow, never a failure:** it uses no network, takes no lock that can wait, does not build
  the engine or the HTTP client, and always exits 0. Any error becomes the line itself:
  `seatswap: <reason>`, because a non-zero exit makes Claude Code hide the whole line. `CLAUDECODE`
  may be set; the command is never gated and writes no `agentCall` event.
- **The recorder's file is not the line:** a `statusline-keys.json` that cannot be parsed does not
  change the printed line and does not stop the feed; the page shows that file's error (§11.4).
- **The preset and the segments:** the preset reproduces the old script. The layout can also hold
  the account (abbreviation in its colour), plan, 5 h percent and reset, week percent and reset,
  per-model percent, auto-burn, model, effort, folder (`~` short), git branch, context used, session
  cost, lines changed, `user@host`, clock, free text and a separator. Each segment has an optional
  colour rule (fixed, by load with two thresholds, by model family, by effort, or the account's
  colour), prefix, suffix and drop order; reset segments can use the compact time left, seatswap's
  own format (§7.2) or a clock time. The `autoBurn` segment shows the next account and the planned
  switch time, like the reset segments show their times; it has no value when the tray app plans no
  switch (§7.6). The three segments Claude Code itself supplies read
  `context_window.used_percentage` (`41%`), `cost.total_cost_usd` (`$3.20`) and
  `cost.total_lines_added` with `cost.total_lines_removed` (`+120 -8`) from the payload; a field that
  is `null` or absent prints nothing, as before a session's first answer, when `used_percentage` is
  `null` and `total_cost_usd` a whole `0` (`$0.00`).
- **Turning it on:** the large window's Status line area builds the layout and writes it into Claude
  Code with *Use in Claude Code*; that is the one key `statusLine` of Claude Code's `settings.json`,
  and *Restore previous* puts the earlier value back (§11.4).

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
  its reset follow the feed when its window is new or the same window is not lower; a 5 h window
  whose reset is at or before the feed's `at` is ignored, so a window that has already ended cannot
  come back after a poll replaced it. The weekly
  percent follows it only when it is not lower, and the weekly reset stays the cache's. Per-model
  limits stay the cache's. The polls stay as they are: they bring the per-model limits and the
  inactive accounts.
- **Effect:** the active account's numbers are seconds old instead of up to 2 minutes, without an
  extra request to Anthropic; auto-burn sees 100 % sooner. With `statuslineFeed` off the command
  writes no feed file and sends nothing.

**The next account.** The tray app writes `<data folder>/burn-next.json`, atomic and mode 0600:

```json
{"account":"<account id>","at":"2026-11-15T14:40:00Z","written":"2026-11-15T09:41:12Z"}
```

`at` is the planned switch in UTC and `written` is when the app wrote the file. The app writes it
when the next account or time changes and at least every 10 minutes as its heartbeat; it removes
the file when auto-burn is off, paused, or nothing is planned. The `autoBurn` segment reads the
file without a lock: a missing file shows nothing, an account id that is no longer stored shows
nothing, and a `written` older than 20 minutes shows nothing because the tray app is not running.
A file that cannot be parsed is the line's `seatswap: <reason>` error. The polls keep their times
whatever this file says.

### 7.7 `seatswap burn plan` and `seatswap burn simulate`

Both commands only read; for an agent they follow `agentsRead` like `status` and `list`.

- **`seatswap burn plan [--json]`** prints the early-switching regime and the plan auto-burn
  follows (eager while early switching is on), read the same way the tray app reads them (with no
  session count, so `tied_busy` is 0). The text form is
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
  the first account of the plan auto-burn follows other than the active one), or null.
- `burnError`: a history or planner error as text, or null. When it is set, `next` is null and the
  views have no `slackHours`; the command still answers.

Only the JSON form reads the history; `--waybar` and `--i3blocks` do not, because they run every
few seconds. With auto-burn on, `seatswap next` called by an agent prints
`seatswap: auto-burn is on and decides` and exits 0 without switching or touching a login; the
agent call itself is recorded as usual. A person's `next` still switches.

### 7.9 Other providers: Codex and Ollama

Two more usage sources can be shown as read-only cards under the account cards in the pop-up, under
a thin heading "Other providers". Each card has its own switch and is off by default; while it is
off, its file is not read and no request is sent. Neither card switches anything, signs in or out,
refreshes a token, or sends a request that generates text. Their readings go into the history like a
Claude account's, under an organization of their own.

**Codex.** The card reads `<codex folder>/auth.json`: the `codexHome` setting when set, else
`$CODEX_HOME`, else `~/.codex/auth.json`. The file is only read, never written. Of it seatswap uses
`tokens.access_token` and `tokens.account_id`; `auth_mode` is looked at only to tell a login kept in
the OS keyring from none. No other field is used or stored, and the access token is never refreshed.
A missing file says so; a file without tokens whose `auth_mode` is `chatgpt` means Codex keeps its
login in the keyring, and the card says that instead of guessing. Nothing in the keyring is read.

With the card on, seatswap sends `GET https://chatgpt.com/backend-api/wham/usage` with
`Authorization: Bearer <access token>`, `chatgpt-account-id: <account id>` and its own
`User-Agent: seatswap/<version>`. The token goes only to that host, the service that issued it, and is
never logged.

The answer becomes the same limits a Claude account has:

- `rate_limit.primary_window` and `.secondary_window`: a `limit_window_seconds` of 18000 s is the
  5 h session and 604800 s the week; `used_percent` is the percent and `reset_at` (seconds since the
  epoch) the reset time. `reset_after_seconds` is ignored.
- An `additional_rate_limits` entry is a per-model limit: its `limit_name` is the model. A 604800 s
  window is a scoped week; 18000 s and every other length keep their own name instead of becoming the
  account's session or week: `5 h limit`, `24 h limit`, `30 min limit`, `45 s limit`.
- The plan is `plan_type` with its first letter capitalised (`plus` → `Plus`); the account is the
  answer's `account_id` under the organization `codex`.
- One limit is marked active: the one with the highest percent, the first of equals. The answer has
  no severity, so the load colour comes from the percent alone (§7.3).

**Ollama.** The card asks the Ollama server at `ollamaUrl`, default `http://127.0.0.1:11434`:
`GET <ollamaUrl>/api/balance`, then `GET <ollamaUrl>/api/usage?range=24h`, both with seatswap's
`User-Agent` and no secret: the local server signs them with the machine's own key. A path on the
base URL is kept; one trailing `/` is dropped before the endpoint's path is appended.

- A **new plan** (monthly credits) has `included.balance_usd`, `included.allowance_usd`,
  `included.period.until` and `purchased.balance_usd`. The card shows the credits left of the
  allowance (or left alone when there is no allowance), the reset date, the bought credits when above
  zero, and the spend of the last 24 h from `totals.usage_usd` of `/api/usage`. When an allowance is
  known and above zero, the card's ring is the share of it used, and the reading is recorded as one
  limit of kind `monthly_credits` that resets at the period's end. Without an allowance there is no
  share and no such limit; the credits are still shown and the reading records no usage. A spend that
  cannot be read leaves the 24 h row empty; the balance still counts.
- An **old plan** (session and weekly limits) has `included.session` and `included.weekly`, each with
  `remaining_percent` and `resets_at`. They become a session and a week limit with
  `percent = 100 − remaining_percent`, like a Claude account; the card has no credits.
- The account is named `ollama` under the organization `ollama`; the plan is empty.

**What the card shows.** One card per switched-on provider, Codex first. A Codex card has the
account card's rings and bars, the plan and a row per limit with its reset and its own name; an
Ollama card on a new plan has the credit rows. A card has no Switch button and no accent bar. A first
reading without limits says `No usage limits in the reply.`; a provider that is switched off has no
card. A failed poll keeps the last numbers and their time, greys them and shows the error text under
them; the age is printed once the reading is older than two minutes. The next poll tries again.

**Every error text the card shows.** The provider reader's own messages are:

| Case | Text |
|---|---|
| no `auth.json` | `No Codex login found in {path}.` |
| a Codex login in the keyring | `Codex keeps its login in the system keyring. seatswap reads the login file only.` |
| HTTP 401 from Codex | `Codex's login has expired. Run codex once to renew it.` |
| a refused connection to Ollama | `Ollama is not running on {url}.` |
| an unusable setting | the setting's own text, for example `the Codex folder (codexHome or $CODEX_HOME) must be an absolute path, got codex` or `ollamaUrl must start with http:// or https://, got "127.0.0.1:11434"` |
| a file that cannot be read | `cannot read {path}: {reason}` |
| an answer of the wrong shape | `Unexpected answer: {reason}` |
| an HTTP or network failure | the API's own text: `the access token is not authorized` · `rate limited; retry after Some(60) seconds` (or `None`) · `server error 500` · `HTTP 404: {body}` · `cannot connect: {reason}` (Codex) · `network error: {reason}` · `cannot decode the reply: {reason}` |

For Ollama a refused connection becomes `Ollama is not running on {url}.`; every other transport
failure keeps the API's `network error: …`. The `Setting` row is what a hand-edited `codexHome` or
`ollamaUrl` produces: the settings page refuses such a value before it is saved, and the reader
checks it again before reading or asking anything.

**History.** A reading that carries a usage is recorded like an account's: a `Sample` with source
`Poll` under `AccountRef {acc, org}`, with `org` `codex` or `ollama`; a window that ends and a 100 %
crossing become a `WindowRecord` and a `Blocked`/`Unblocked` event like a Claude account's. A Claude
organization id is a 36-character UUID, so those names cannot clash. Every reader that serves Claude's
accounts uses `read_claude` (and `finished_claude_windows`), which leaves those records out: auto-burn,
the status line's next account, money and every statistics page stay Claude-only, and provider
readings add no Sessions rows, no statistics and no switch. `read` stays for the recorder, the export
and the import: an export's windows and daily token rows are raw history, so a provider's finished
window can appear in an export, while its samples do not, because the export has no sample rows. An
import copies a host folder as it is, provider lines included.

**Settings.** The four settings are in the settings table (§10) and in Settings → Providers:
`codexCard` and `ollamaCard` default off, `codexHome` is empty unless set, and `ollamaUrl` defaults to
`http://127.0.0.1:11434`. `codexHome` must be an absolute folder when set. `ollamaUrl` must start with
`http://` or `https://` and name a host; a whitespace or control character, a query string, a fragment
and an empty path segment are refused, while a port, a path, an IPv6 host and one trailing `/` are
accepted.

**What is not done.** No switching, no sign-in or sign-out (of Codex or Ollama), no token renewal, no
request that generates text, no keyring read (a Codex login kept in the keyring is only named on the
card), no statistics and no Sessions area entries.

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
- Default: the first letters of the first two words of the label ("Acme Team" → `AT`), or the first two letters of a one-word label ("Acme" → `AC`, "Private" → `PR`). The examples in this spec show `P` for the private account; that one was shortened by hand in the editor (§11.4).
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
| `statuslineInstall` | none | `null`, or `{ "installed": <value>, "previous": <value> }`: the exact `statusLine` seatswap last wrote in Claude Code's `settings.json` and the exact value the file had before its first install. `previous` absent means the file had no key; a present `null` is the value `null`. Written by *Use in Claude Code* and cleared by *Restore previous* (§11.4) |
| `statuslineRecordUntil` | none | `null` or a time: while it is in the future, `seatswap statusline` records the payload's key paths and JSON types to `<data folder>/statusline-keys.json`. *Record for a minute* sets it to one minute from now (1 to 60 minutes are accepted), §11.4 |
| `notifyOnSwitch` | on | |
| `mirrorLive` | on | §6.4 |
| `refreshInactive` | on | refresh the logins of inactive accounts (§6.3). Off: inactive accounts show their last known usage with its age |
| `hotkey` | Ctrl+Alt+C | Linux v1: not used; bind `seatswap panel` in the desktop instead (§8.3) |
| `startAtLogin` | on | Linux: `~/.config/autostart/seatswap.desktop`; Windows: the `Run` key value `seatswap` → `seatswap-tray.exe` (§9.4) |
| `theme` | system | system · light · dark |
| `claudeConfigDir` | `$CLAUDE_CONFIG_DIR` or `~/.claude` | |
| `historyDir` | `null` (`<data folder>/history`) | text, an absolute folder; point it at a synced folder to share the history with other hosts |
| `hostName` | `null` (the machine's host name) | text, 1–63 characters of `A–Z a–z 0–9 . _ -` that must start with a letter or a digit, stored in lower case; the name this host writes its history under |
| `readSessions` | on | read Claude Code's session files and transcripts: model, busy or idle, waits, tokens per folder, agents and their thinking. The Sessions area needs it; turning it on takes effect after a restart. Message text is never stored |
| `notifyUsableAgain` | on | the "usable again" notification (§7.5) |
| `notifyWaiting` | on | the "waiting for you" notification (§7.5); a click opens the Sessions area at that session |
| `notifyWaitingHosts` | `thisHost` | `thisHost` · `allHosts`: whose waiting sessions notify. Other hosts report through the shared history folder, so their notifications come with the delay of the sync (§4.1, §11.4) |
| `notifyLoginEnds` | on | the login warnings at 5 days, 1 day and when a login ended (§7.5). `bool`; Settings → Notifications, "A login ends" |
| `currency` | none | `null` or three capital letters (`"EUR"`); `Option<String>`; the currency every amount is shown in. Until it is set, amounts in money stay missing (§11.5). Settings → General, section Money, "Currency" |
| `usdRate` | none | `null` or `{ "currency": "EUR", "value": 0.95 }`: units of that currency per 1 USD, value above 0; `Option<OwnRate>`. It is used only while its currency is `currency`; an entered rate wins over the looked-up one (§11.5). Settings → General, Money, "Use my own rate" |
| `lookUpPrices` | on | `bool`; whether seatswap may ask models.dev for API prices and Frankfurter for the dollar rate (§11.5). Off, neither host is contacted and `prices --refresh` refuses; stored values stay. Settings → General, Money, "Look up prices and the dollar rate" |
| `price` per account | none | `null` or a number above 0: what the subscription costs per month, in `priceCurrency`; `Option<f64>`. Without one the account is left out of every sum in money and the page says so (§11.5). Settings → Accounts, column "Price / month" |
| `priceCurrency` per account | none | `null` or three capital letters: the `currency` in effect when `price` was entered. A price counts only in that currency; a price entered before a currency was set has `null` and does not count (§11.5). Settings → Accounts, the price cell |
| `plan` per account | from the login | read-only `Option<String>`: the identity's `planDisplayName`, else the capitalised `subscriptionType` with its level from `rateLimitTier` ("Team 5x"; a tier seatswap cannot read as a multiplier in parentheses, "Team (team_premium)"). Shown in the pop-up, Settings → Accounts and `seatswap list` |
| `agentsRead` | on | agents may run `list`, `status`, `burn plan`, `burn simulate`, `prices` and `history export` (§6.6, §7.7) |
| `agentsSwitch` | off | agents may run `use` and `next` (§6.6); an agent's `next` does nothing while auto-burn is on (§7.8) |
| `codexCard` | off | `bool`; show a read-only Codex card in the pop-up. While off, no Codex file is read and no request is sent (§7.9). Settings → Providers, "Show a Codex card" |
| `codexHome` | none | `null` or an absolute folder: where Codex's `auth.json` is; `null` means `$CODEX_HOME`, else `~/.codex`. A relative folder is refused. Settings → Providers, "Codex folder" (§7.9) |
| `ollamaCard` | off | `bool`; show a read-only Ollama card. While off, no request is sent to the Ollama server (§7.9). Settings → Providers, "Show an Ollama card" |
| `ollamaUrl` | `"http://127.0.0.1:11434"` | the Ollama server the card asks; must start with `http://` or `https://` and name a host. Refused: a whitespace or control character, no such scheme, no host, a query string, a fragment, an empty path segment. A port, a path, an IPv6 host and one trailing `/` are accepted. Settings → Providers, "Ollama server" (§7.9) |

Every setting above is edited in the large window's Settings area (§11.4); the last column names the
group and the row. The pop-up has no settings view any more (§11.3).

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
- **Keyboard:** ↑/↓ moves between accounts, Enter switches, 1–9 switch directly, `,` opens the settings in the large window, Esc closes. The cursor (a blue ring) appears only with the first ↑/↓, starting from the account in use, and is gone again each time the pop-up opens; until then Enter does nothing. Clicking a card does not move it.

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

### 11.3 Account editor and settings have moved

The pop-up has no account editor and no settings view any more. Its gear, and `,` on the keyboard,
open the large window at its Settings area. A click on an account's badge, or on its ✎, opens the
large window at Accounts with that account's editor open. The editor and every setting row are in
§11.4; `docs/implementation.md` has the page and command contracts.

---

## 11.4 The large window

seatswap has a second window next to the pop-up: label `main`, title "seatswap", 1280 × 800 at
start, resizable, decorated, in the taskbar and not always on top. It is one more Tauri webview
window with its own page (`window.html`); both windows can exist at the same time and both receive
the app's `snapshot`.

**How it is opened.**
- The pop-up's gear (or `,`): the Settings area.
- An account's badge or ✎ in the pop-up: Accounts, with that account's editor open
  (`settings/accounts?<account id>`).
- The tray menu: "Settings…" opens Settings, "Open seatswap…" opens Sessions.
- `seatswap window [AREA]` asks the running tray app; without an area it opens `sessions`.
  `seatswap panel settings` sends the same line as `seatswap window settings`.
- A page's `open_window` command, which takes the same target.

A target is an area, optionally followed by `/<sub>` and, for the sessions area only, optionally by
`?<item>`: `settings`, `settings/accounts`, `settings/accounts?<account id>`, `sessions?<session id>`,
`statistics/forecast`. The areas are `sessions`, `statistics`, `statusline` and `settings`; a
sub-area and an item may hold only `a-z`, `0-9` and `-`, and there is at most one `/`. An item after
`?` without a `/` is refused for every area except sessions. Anything else is refused with the
reason, which `seatswap window` prints as a usage error. A newly created window loads `window.html#<target>`; an
already open one is shown, un-minimized, focused and sent `open-area`. The pop-up keeps its own
`navigate` event, because an event reaches every window.

**The areas.** Four tabs at the top left, in this order: Sessions · Statistics · Status line ·
Settings. An empty address opens Sessions. Each area has a page; the Status line area is the
builder below. Under `statistics` the sub-area is one of the seven page
names (`statistics/forecast`); without one the area keeps the page last chosen. The area's title and
its subtitle stand above the card, and the snapshot's error is a banner above both. The theme
follows the `theme` setting and the system, as in the pop-up.

**Settings.** Eight groups on the left, the chosen group's card on the right:
General · Accounts · Auto-burn · Notifications · Projects · Agents · Providers · History and hosts.
The address names the group, with an alias accepted (`gen`, `acc`, `autoburn`/`burn`, `noti`, `proj`,
`prov`, `hist`), and an optional item that opens a row's editor. A click on a group writes the new
address, so Back returns to the previous group.

Every control saves at once; there is no Save button. A refused save shows its error under the row
and puts the control back to the saved value. Saves run one after another, each built from the
settings the one before left, so a quick second change never carries a stale copy; while a save is
pending, an incoming snapshot does not overwrite the page's own copy.

- **General.**
  - *Money*: the currency; "Look up prices and the dollar rate"; "1 US dollar is" with the rate in
    effect and whether it was looked up (with its date) or entered by hand, or "no rate yet: API
    values are shown in USD", with a failed rate look-up's text under that row; "Use my own rate",
    which saves the rate with the currency it is for and is disabled while no currency is set or it
    is USD; the API prices row with how many models have a price, when they were checked, what the
    last look-up changed, how many models models.dev's list left out and which, and a price list. In
    the price list each model's four prices are fields: changing one saves it as that model's own
    price, and a button puts the looked-up price back; a model whose price depends on the prompt size
    is marked and uses its base price. The list ends with "add a model": a provider (one of the
    three models.dev providers seatswap reads), a model id and the four prices, which saves an own
    price for any model, also with look-ups off and no list (§11.5).
  - *Look*: theme, tray text, which limit the tray ring shows.
  - *Start and polling*: start at login; how often the active account and the inactive accounts are
    read.
  - *Claude Code*: the config folder; read the session files; copy the live login at once; refresh
    inactive logins.
- **Accounts.** One row per account: the chip (abbreviation and colour), the label (editable), the
  account's e-mail, its plan, its price per month with the currency it was entered in, "Auto-burn may
  use it", its organization, and when its login ends. A price entered in another currency than the
  current one is marked "in EUR: enter it again for CHF", and entering the same amount again saves it
  in the current currency. A click on the chip opens the editor: one or two letters,
  ten colour swatches and a hex field, a live preview of the tray icon at 16, 22 and 32 px, and
  Remove with a confirmation that says the live login is not touched. Account and plan come from
  the login and are read-only. The organization is chosen from the organizations of §11.6 or
  created there.
- **Auto-burn.** The switch, "Suggest switches", and the read-only lines of the auto-burn view: how
  early it switches, what was measured, what it would do now, what a switch by you costs, and
  whether a session would lose its thinking. There is nothing to tune here.
- **Notifications.** "A session is waiting for you" and whether this host's or all hosts' waiting
  sessions notify; "An account is usable again"; "All accounts are full" (always on); "Every
  automatic switch"; "A switch by an agent" (always on); a link to the suggested switches under
  Auto-burn; "A login ends", the 5-day, 1-day and ended warnings.
- **Projects.** §11.6.
- **Agents.** "Agents may read usage" and "Agents may switch accounts", with the text of §6.6; the
  skill's state, an install and a remove button; the last 20 calls by agents, newest first, with
  their folder and whether they were allowed.
- **Providers.** "Show a Codex card" and "Show an Ollama card", both off by default, "Codex folder"
  (empty: `$CODEX_HOME` or `~/.codex`) and "Ollama server" (default
  `http://127.0.0.1:11434`). The section's two notes say that seatswap reads Codex's login file and
  asks chatgpt.com for the usage every 5 minutes without changing Codex's files, and that the Ollama
  card asks the server on this machine for the cloud balance every 5 minutes (§7.9).
- **History and hosts.** This host's name and its history folder; every host folder with its last
  record, its first record and its size; export and import. A changed host name or folder takes
  effect after a restart, and the row says so when the saved value differs from the one in effect.
  Export writes CSV or JSON to a file the person names (an existing file is refused); import copies
  another history folder's host folders in by hand and never writes this host's own folder or a
  same-named folder of another machine.

### The Sessions area

The first area, and what an empty address opens: every running Claude Code session on this host and,
through the shared history folder, on the user's other hosts, live, with its agents, tokens and
their two values. It is read from Claude Code's session files and transcripts, so it needs
`readSessions` (default on); turning that on takes effect after a restart, and with it off the area
says so.

**Numbers on top.**
- Sessions running, and on how many hosts; busy, with how many are waiting for you.
- Agents running, and how many completed today.
- The total of exactly the sessions in the view: their tokens, subscription value and API value; the
  last hour's tokens.
- The sessions whose thinking is tied to the account (§2.11), and how many of them are busy.
- The account in use, with its week.

**The tree.** Two orders, switched at the top and remembered: by project → provider and model →
session, or by provider → model → session with the project after the session's name. Every group
row shows how many running and closed sessions it holds, how many are busy, how many agents are
running, and the totals of its tokens and their two values; every total counts exactly the sessions
in the view — a grey row that has left is shown but no longer counted. A session's row has:

| Column | Content |
|---|---|
| Name | the session's name; opens to its agents |
| Model | the model of its last answer, with a mark when its thinking is tied to the account |
| Status | busy · idle · waiting for you, with how long; for a closed session, when it closed |
| Agents | running · completed, the running number in the busy colour (grey at zero), completed always grey |
| Tokens | output tokens of the session and its agents since it started |
| Last hour | output tokens in the last 60 minutes, empty when there were none |
| Subscription value | the value of those tokens in the account's plan (§11.5) |
| API value | the same tokens at API prices (§11.5) |
| Host | where it runs; for another host, the age of its last report |

A session opens to its agents, grouped by provider and model. One row per agent has kind, task,
running or completed, tokens and both values, and its own agents as "1 running · 2 done". An agent
that started agents opens the same way, to any depth. Completed agents are folded into one
"38 completed" row that opens.

**Around the tree.**
- *By model*: sessions, busy ones and the last hour's tokens per model.
- *If you switched now*: how many sessions would work without their earlier thinking, how many of
  those are busy, how many would send their conversation again and how many pay nothing (§2.11),
  with a Switch… button that opens the pop-up.
- *Last hour*: output tokens by project.

**Closed sessions and hosts.**
- **Closed sessions**: none (the default) · last 24 hours · last 7 days · last 30 days. They stand in
  the same tree, in their project and model group, in grey, with the time they closed. Every total
  follows the choice: the numbers on top and each group's total count exactly the sessions that are
  listed. They come from the history's session and token records, so they are there after Claude
  Code has deleted the transcript.
- **Hosts**: all, or one. Each host writes what is running on it into its own report in the shared
  history folder (`<host>/sessions-now.json`, below). Another host's row carries the age of its
  report ("vega · 2 min ago"); a host whose report is older than 15 minutes says "no report since …"
  instead of running. Without a shared history folder only this host is shown; the report is still
  written whenever this host has a history folder, shared or not.

**How it updates.** While the area is open, the sessions thread watches the session files and the
running sessions' transcripts and pushes its view: a change is on screen within about a second, at
most two redraws a second, with a trailing one so the last numbers always arrive, and once a minute
for the durations and "last hour". Rows keep their place and open rows stay open; a new session or
agent appears in its group. A session that left turns grey for 60 seconds and is then removed, or,
with closed sessions shown, a session that has a closing record is already there as a closed row.
Other hosts' rows change when their
report arrives. The thread reads the session files and transcripts whether or not the window is
open; the transcript folder watches and the pushes exist only while the area is subscribed.

**The link `sessions?<session id>`.** The waiting notification's click (§7.5) opens that session:
the area opens it, marks it for 3 seconds and scrolls to it. A session that is no longer listed says
so.

**The report other hosts read.** `<history root>/<host>/sessions-now.json` is pretty JSON, written
atomically with mode 0600 in this host's own folder: `{ "at": "…", "sessions": [ … ] }`, the rows of
this host's Sessions area at that moment. It holds the session rows and their agents' kind, model,
running or finished state, times, tokens and values. It never holds an agent's task text (it is set
to null before writing) and no message text ever reaches it: none leaves the transcript parser. It
is written when the rows changed and the last write is at least 30 seconds old, at once when a
running session leaves the list, and as a heartbeat every 5 minutes when nothing changed, so a quiet
host is not taken for a stale one; an empty list is a report too. A report more than 15 minutes
before now is stale, and its sessions say "no report"; a time in the future is fresh. The import
never copies the file and a host folder's size leaves it out (§5).

### The Statistics area

The second area: seven pages over the history. Every number is computed from what the history
holds — usage readings, finished weekly windows, switches and blocks, token and session records,
waits — together with the accounts and settings, the prices and dollar rate of §11.5 and the
project log of §11.6. **The statistics only read:** no function of `core::stats` writes a file. The
history records they count hold only time, folder, model, block kinds, token counts and ids; no
message text is read or kept for them.

**The frame.** Seven tabs at the top of the page, in this order: Overview · Weeks · Projects ·
Subscriptions · Forecast · Rhythm · Activity. Above them stand:

- **The range**: This week · Last week · 30 d · 90 d · 12 mo · All. Every range starts at Monday
  00:00 local time. *This week* runs from this week's Monday to now, *Last week* is the week before,
  and *30 d* / *90 d* / *12 mo* start at the Monday of the week that holds now − 30 / 90 / 365 days.
  *All* starts at the Monday of the week that holds the first record, so every page's first
  calendar week holds all its windows; a history with no record uses this week's Monday.
- **The accounts**, as chips in their colours. A click takes an account out of every page; at least
  one stays chosen. While no chip is taken out the query names no account, which means every
  account the history knows, including one no longer stored. As soon as a chip is taken out the
  query names the remaining stored accounts, so accounts no longer stored fall out too.
- **The host**, when the history holds more than one: All hosts, or one. Every page except Forecast
  follows it. A missing host list is a banner, and the pages still show every host.

The range, the excluded chips, the host and the tab are remembered. The address names a page
(`statistics/forecast`); without a page name the area keeps the last one chosen.

**An empty range.** A page whose range holds no record at all shows no chart of zeros. It says
`Nothing recorded yet.` or, when the history has any record, `Nothing recorded yet. seatswap has
been recording since {date}.` with the date as `Mon 9 Nov`. This is one marker from `stats::empty`,
the same on every page except Forecast. The Forecast is a plan for every account and host, not a
range; without a reading of every account it says `the forecast needs a reading of every account
first` instead.

**Definitions every page uses.**

- **Local time.** Every clock hour, day, week, weekday and curve point is in this computer's local
  time zone: the zone's offset at that instant, not one offset for the whole range, so a record on
  the other side of a daylight-saving change keeps its own hour and week. This holds for records
  from another host too. The Activity lanes and the Rhythm heat map take the hours of the week in
  this host's local time for every host.
- **A calendar week** is Monday 00:00 local to the next Monday 00:00 local. A page shows every
  calendar week its range touches; a partial week counts as one.
- **One window per account and calendar week.** A finished `weekly_all` window belongs to the
  calendar week of its midpoint, its end minus 3.5 days, and a range chooses the windows whose
  midpoints fall inside it. If several hosts recorded the same window (same account, same end), the
  history keeps the one with the latest last reading. If one account has two windows whose
  midpoints fall in one week because its reset moved, only the window with the later end is kept.
- **The price of a window** is the account's monthly price at the window's midpoint: the last
  `PriceChanged` at or before it, or the first change when the midpoint lies before all of them, or
  the stored price when there is no change. It counts only in the currency it was entered in, as in
  §11.5.
- **Used, paid and expired money.** One percent of a week is worth `pv(price) = price × 12 / 52 /
  100` in the display currency. The money paid for a week is `100 × pv`, one week's share of the
  monthly price. What expired is `max(0, 100 − final percent) × pv`, the history's `lost`; what was
  used is the rest, `paid − expired`. A window above 100 % therefore expired nothing. A week
  without a price is left out of every money sum — the account's other, priced weeks still count —
  and the Overview and Subscriptions pages name that account under "No price for …".
- **Blocked hours.** A block is counted only while its account was the account in use on that host,
  and only in the working hours of the rhythm of the four weeks before the range's end: an hour of
  the week whose learned work is above zero. A block that is still open at the range's end counts
  up to it. Blocks of every limit kind count (weekly, session and per-model). Two hosts blocked at
  the same time each have their own span, so each machine's blocked time is shown.
- **Avoidable hours.** The part of the blocked hours in which another stored account with its
  auto-burn switch on was not blocked on that host and had a finished weekly window running that
  ended with something expired. The rest of the blocked hours is not avoidable: no other
  auto-burn account had expired quota running then, or it was blocked on that host too.
- **Switches** are the `Switch` events of the range and host, by who made them: *you* (the tray or
  the pop-up), *cli* (`seatswap use`), *auto* (auto-burn), *agent* (a call by an agent), and
  *external* (a change seatswap noticed but did not make).

**Overview.** Five numbers for the range:

- **Used**: the money used of the priced account weeks, with the money paid for them below it.
- **Expired unused**: the money that expired, split per account, largest first.
- **Blocked**: the working hours in which the account in use was blocked.
- **Avoidable**: the part of those hours in which another account's quota expired.
- **Switches**: by you, cli, auto, agents and external, and what one switch costs this host's
  account in use at the range's end as a percent of its 5 h window (the measured cost of §2.13);
  `—` when the cost or the rates are missing.

Below them, every calendar week of the range carries two stacked bars in money: one stacked in the
accounts' colours with the expired part hatched on top, and one splitting the projects'
subscription value among the projects (the three largest, the rest as *other*). The hours blocked
stand under each week, and the marks say when an account was added and when auto-burn was switched
on or off. Beside them, "Share that expired, per week" is one line: the expired money over the paid
money of that week's priced weeks; a week without a priced week has no point. "What the numbers say"
holds at most four sentences, in this order, each only when it has data:

1. `{abbr} is where most money expired: {amount} in {n} weeks.` — the account with the largest
   expired money, if above zero; only with a currency.
2. `You were blocked {h} in {n} weeks; {a} of them while other quota expired.`
3. `Since auto-burn ({date}): {h} blocked a week, {e} expired. Before: {h0}, {e0}.` — from the latest
   `AutoBurnSet { on: true }` in the range that stayed on, with a full week before and after it.
4. `{abbr} was full in {n} of {m} weeks.` — the account with the most weeks at 100 %.

**Weeks.** Every window as a grid: one row per account, one column per calendar week. A cell is
filled up to its final percent, the rest is hatched; a week before the account existed is empty, not
zero. Next to the rows stands the account's percent now from its newest reading, and a row below
the grid holds the hours blocked per week. One curve per stored account shows its weekly percent
over the last 14 days; a reset is a drop to 0 at the earlier window's reset when that lies between
two readings, else at the later reading. The page opens on the summary of the newest finished week
of the range when it has one.

- **The summary of a week** (a click on a week's date): used as a percent of what was paid and in
  money, expired in money, the hours blocked, the switches by who made them, the money used per
  account, the three projects with the largest share of the week's API value (then *other*), the
  week's tokens and API value, and the change of the used money against the week before (`—` when
  either week has no priced account week). Its conclusion has at most three sentences:
  `No hour blocked.` or `{h} blocked.`; then the account week with the largest expired share — `{abbr}
  came closest to expiring unused: {n} left at its reset.` under 10 %, else `{n} of {abbr}'s week
  expired, {money}.` when a currency is set; then `Auto-burn held back {n} planned
  {switch|switches}, {m} {minute|minutes} in total.` when switches of the week carry a waited time.
- **The detail of a window** (a click on a cell): used and expired in percent and money; the first
  weekly block of that account inside the window (`reached 100 %`) and the working hours blocked
  after it; how many finished session windows of the account ended inside it with some use; how
  many switches went into or out of the account and how many of them were auto-burn's; the age of
  the last reading before the reset; whether the window is marked uncertain (seatswap was off at the
  reset); and the week's curve, built from the account's weekly readings inside the window that name
  its reset.

**Projects and organizations.** A tree table with three levels: organization → project → folder.
An organization's row comes first; its projects follow by API value, and a project's folders appear
under it when it has more than one. A folder is the first path part below the project's root on
that host (the common parent of the project's folders), or the top folder. The second view is the
plain folder tree without organization rows; either view opens and closes its rows. Every row shows
its tokens, its share of the page's API value, its subscription value and API value, a bar split by
account, and its distinct sessions.

- **Organization × account** is the subscription value per organization and account, in the stored
  accounts' order. For an organization linked to its own subscription it carries the sentence
  `{abbr} is {org}'s subscription. It covered {n} of the work for {org}.`; below 100 % it adds
  ` The rest ran on {others}: {amount} of their value.` when another account carried some, and, when
  that account's subscription also carried work for other organizations,
  `The other way round: {abbr} carried {amount} of work for {orgs}.`
- **A project's detail** (a click on a project row): its tokens, the share of answers that came from
  agents, each host's share of its API value, its subscription and API values, its output tokens per
  calendar week and model, and its distinct sessions. A folder key belongs to one host: with All
  hosts chosen, the detail of that key counts every host's folder of that name; with one host chosen,
  only that host's.
- **Tokens per percent** of each account is its output tokens of its finished weeks over the sum of
  those weeks' final percents. A fall means use that left no transcript, such as claude.ai, the
  phone, Claude Desktop or a host without the shared history. The page says that such use is in the
  percent but in no project, and how many tokens have no known account at all.
- The page also names the models whose API value is missing because they have no price, and the
  tokens that have no subscription value with the reasons. It does not merge two projects with the
  same name: their keys differ.

**Subscriptions.** One bar per subscription period that overlaps the range: from the account's
`AccountAdded` (or its first record) to its `AccountRemoved`, split at every plan and price change.
A period that still runs has no end; its price is the price at the period's start. The rows follow,
one per account, stored accounts first and accounts no longer stored after them:

| Column | What it is |
|---|---|
| Plan, price | the plan and monthly price of the account's latest period that overlaps the range; without one, the stored account's plan and price, or for an account no longer stored its latest window record's plan and the price at the range's end |
| Since | the start of its first period in the whole history, or its `addedAt` without one |
| Weeks | its finished account weeks in the range |
| Average used | the mean final percent of those weeks (`—` without a week) |
| Expired | the mean expired percent and the expired money (`—` without a week or a price) |
| Full | the weeks at 100 % out of all its weeks |
| Blocked on it | the working hours blocked while it was the account in use |
| At API prices · × paid | the API value of its records of the range, and that over the paid money of its weeks; `—` when a record has no value or a week has no price |
| Verdict | *Too early* under 4 weeks; *Mostly unused* when the average is under 40 %; *Regularly full* when at least half its weeks were full; else *Well used* |

- **What if without it** (a click on a row): each calendar week of the range is replayed twice, with
  every account and without that one. The replay is the simulator of §2.8 with the recorded work and
  the **lazy** rule — auto-burn switches only when the account in use is full. The switching rule
  that was set in a week is not in the history, so it cannot be replayed per week. The extra blocked
  work of a week is turned into hours with the replay's typical working hour, and the page never
  reports fewer hours than were recorded. It shows the hours blocked instead of the recorded number,
  the money expired instead of the recorded expired money, the money paid for that account's weeks
  of the range ("saved on the price"), one sentence, and the blocked hours per week as it was and
  without the account. Expired money is `recorded + (replay without − replay with)`; a week with no
  price is left out. Without enough history — no learned rate for an account with a reading in the
  range — it says `Not enough history yet to replay this.`, gives no replayed number, and keeps the
  recorded numbers beside the sentence. A reading from an account the store no longer holds refuses
  the replay too: `{abbr} is no longer stored, so this range cannot be replayed.`

**Forecast.** The latest plan of every account and every host, so it follows neither the range, the
account chips nor the host choice. Each time the page loads it uses the plan reading kept from the
auto-burn runtime; when the oldest reading that plan was built from is older than 20 minutes the
page refuses it and says `the forecast needs a reading of every account first`. It says how many
weeks of history that plan reads. Below it:

- **Four numbers**: the money expected to expire this week with auto-burn's plan as it is set now
  (eager when early switching pays, else lazy), the same when switching only when an account is full
  (the lazy plan), the hours of this plan with no usable account, and the switches planned with each
  of the two rules.
- **Per account**: the weekly percent now; the percent the plan expects at its reset (`100 − the
  plan's unused percent`); the low of its last 8 finished windows; when the plan first has it full;
  its next reset; and what switching only when full would leave unused, in percent and money.
- **The low** is this week's percent now plus the second-lowest gain of its last 8 finished windows,
  at most 100. A window's gain is its final percent minus the percent of the newest weekly reading
  at or before `end − d` that names the window's reset, where `d` is the time from now to the reset
  the plan plays the account to. A window without such a reading is left out; with fewer than four
  gains there is no low.
- **The sentence**: while an account auto-burn may use has no rates it starts
  `Still learning how fast {abbrs} is used. ` Then either `Enough for the week.`, or
  `All accounts full from {weekday HH:MM} to the end of the forecast.`, or
  `All accounts full from {weekday HH:MM} until {abbr} resets {weekday HH:MM}.` when the gap ends at
  that account's reset, else `All accounts full from {weekday HH:MM} until {abbr} is usable again
  {weekday HH:MM}.` A reopened 5 h window is not a reset.
- **The lanes** are the next 7 days, one row per account: the stretches the plan plays it, its
  weekly reset as a mark, and the stretches where no account is usable. Times are local.

**Rhythm.** A heat map of 7 weekdays × 24 clock hours: the output tokens of each hour of the week,
averaged over the range's calendar weeks (a partial week counts as one), all chosen hosts together.
Below it, per weekday: the distinct local hours with tokens divided by the number of calendar weeks
("working hours per week"), and the blocked working hours of that weekday divided by the same
number. Auto-burn and the Forecast plan with the rhythm of the last 4 weeks before now, not with
the range's picture; an hour that usually has work counts as a working hour.

**Activity.** Every chosen host gets a lane over the last 168 local hours, ending with the hour that
holds the range's end. Each hour is one of four states, decided in this order:

| State | Rule |
|---|---|
| Work | the hour holds token records of that host |
| Idle | no tokens, but an `up` record in that hour |
| Off | no tokens and no `up`, but the hour lies at or after the host's first `up` record: seatswap was not running, the machine was off or asleep |
| Unknown | before the host's first `up` record, or without one |

An hour's tooltip holds its output tokens per model, its sessions and the project that used most;
the strip below each lane shows how many sessions worked in that hour. On top, per host: hours
working, on but idle, off and not known, the tokens of the whole range and their two values, and
the output tokens of the last 7 days over its working hours; the highest hour of the
range is all chosen hosts together. A switch picks what the hour bars measure: tokens (per model) ·
subscription value · API value (the two values exist per hour only). Below the lanes: output tokens
per day for the last 14 days, one bar per host; one row per calendar week with its working hours per
host, tokens and their two values, sessions, and the project and folder that used most; and the
largest sessions of the range, at most 20, by output tokens, with project, host, model, hours worked,
tokens and their two values, and the agents it started once the session has ended (a running
session has none yet). The minutes sessions waited for you per local day come from
the `Waited` events; they follow the host choice but no account chip, because a wait names no
account.

**What is not built here.** Of the proposals of §3.10, "Tokens per percent, over time" and "Time
sessions waited for you" are on these pages (Subscriptions and Activity). "What auto-burn did" and
"Where tokens are lost" are not built and stay proposals.

### The Status line area

The third area: the layout of `seatswap statusline` is built here, and turned on in Claude Code from
here. The page edits `settings.statusline` and asks the backend for every preview; it draws no line
of its own. Every change is saved through the settings page's one writer at once; a refused save
puts the stored layout back and shows the reason at the top of the page.

**The palette.** The segments that are in neither zone, in three groups: *seatswap* (account, plan,
5 h percent, 5 h reset, week percent, week reset, per-model %, auto-burn: next), *Claude Code*
(model, effort, folder, git branch, context used, session cost, lines changed) and *System*
(`user@host`, clock, free text, separator). Free text and separator can be added more than once; a
segment that is already on the line is not in the palette. A click adds it to the zone last used.

**The two zones and the margin.** *Left*, and *Right* which is flush with the right edge of the
terminal; at least two spaces stay between the zones while both print something, and an empty
zone takes no gap. A segment is dragged from the palette into a
zone, between two zones and inside one; the keyboard does the same: ←/→ reorder it, `L`/`R` move it
to that zone, `Delete` sends it back to the palette. A segment dropped onto itself, or a drop
outside a zone, changes nothing. Below the zones: the *margin*, the columns kept free at the
terminal's right edge, and the switch *Pass Claude Code's live numbers on to seatswap*
(`statuslineFeed`, §7.6).

**The options of the chosen segment.** The page offers only what applies to its kind:

| Option | What it offers |
|---|---|
| Colour | *none* · *fixed colour* (`#rrggbb`) for every kind; *by load* (yellow from, red from) only for the 5 h percent, the week percent and the per-model percent; *by model family* only for the model; *by effort* only for the effort; *the account's colour* only for the account |
| Time format | Only the 5 h reset and the week reset: *Compact time left* · *seatswap's* (the rule of §7.2) · *Clock time*; without one seatswap's own format is used |
| Text | Only free text; a segment whose text is empty prints nothing, with its prefix and suffix |
| Prefix / suffix | Every kind; printed only when the segment has a value |
| Drop when narrow | *never* · *1 · first* · *2* · *3* |

The folder has no style option: it is always `~`-shortened. The settings check refuses a colour
rule, a time format or text that the segment's kind does not offer, naming the kind and its place
in the layout. A value the page cannot read (an unknown kind, a colour rule its kind does not
offer, a drop number above 3) is shown as it was stored, never replaced by a guess.

**The preview.** Three width fields (120, 100 and 80 columns at start, each 1 to 65535 and
editable), a *Dark terminal* and a *Light terminal* tab, and the scenario *Live data* · *Test: 97 %*
· *Test: account blocked* · *Test: no 5 h limit*. Every width gets its own line, drawn with the
spans' own colours and bold flags on the terminal background, in a monospace terminal font, in a box
as wide as its width in columns; the line ends the margin before the right edge, as in a terminal. Beside each line is its note: `everything fits`, or the kinds the fitting
removed, as `5 h reset, week reset and 5 h percent are dropped`. The preview draws a made-up
session with the live account's numbers: the folder `~/projects/atlas` under the user's home,
the branch `main`, context used `41%`, session cost `$3.20`, lines changed `+120 -8`, and the
model and effort the payload names; the account's abbreviation, colour, plan and limits are the
live account's. The text and the colours come from
`statusline_preview`, the command's own renderer (§7.6): the preview and the real line are one
implementation. The preview is requested again with every new reading, and 150 ms after the last
change. Without a live login the preview shows the command's error instead of a line.

**The presets.** *My current line* (what the script of §5.1 printed; the stored default), *Limits
only* and *Everything*. A layout that equals none of them shows *Custom layout*. Choosing a preset
while the layout was changed asks *Replace the layout?* with Cancel and Replace first; without a
change it applies at once. Every preset is saved like a hand-made layout.

**The three payload segments.** They read Claude Code's own numbers from the payload, and each is a
plain number in the JSON:

| Segment | Payload fields | Prints |
|---|---|---|
| context used | `context_window.used_percentage` | `41%` |
| session cost | `cost.total_cost_usd` | `$3.20` |
| lines changed | `cost.total_lines_added` and `cost.total_lines_removed` | `+120 -8` |

A field that is `null` or absent prints nothing, like every other segment, and so does a value that
is not a number. Lines changed needs both numbers. Before a session's first answer Claude Code sends
`"used_percentage": null`, so the context used segment is empty, and `"total_cost_usd": 0`, a whole
number, which prints as `$0.00`.

**Use in Claude Code.** The button first shows a card: the file (`<config>/settings.json`; on Linux
`~/.claude/settings.json`, on Windows `%USERPROFILE%\.claude\settings.json`), the value of
`statusLine` *now* and *after* as JSON, and the sentence "seatswap changes only this key. The value
it replaces is kept and can be put back."; then Install or Cancel. When seatswap's line is already
in place the button reads *In use in Claude Code*, and *Restore previous* stands beside it whenever
there is something to restore.

The value seatswap writes is

```json
{"type": "command", "command": "\"<path>\" statusline", "refreshInterval": 2}
```

where `<path>` is the console `seatswap` program beside the running one (on Windows
`seatswap.exe`, also when the tray program makes the change). The path is always inside double
quotes, so the shell Claude Code runs the command through — bash, or Git Bash on Windows — reads it
as one word; inside the quotes `"`, `$` and `` ` `` get a leading backslash and a backslash is
doubled where it would otherwise escape or be eaten, so a path with a space, a dollar sign or
Windows backslashes arrives unchanged. A path that is not valid Unicode is refused.

The write itself:
- **One key.** seatswap writes exactly one key of Claude Code's `settings.json`: `statusLine`. Every
  other key keeps its value and its place in the file's order; the file is written again as JSON
  with two spaces per level and no trailing newline.
- **A backup once.** Before the first write of an existing file, a copy is kept beside it as
  `settings.json.seatswap-backup`. It is never updated.
- **A missing file** is created holding only `statusLine`.
- **A file that is not a JSON object** — an empty file, broken JSON, a list, a string — is refused
  with its path and the reason, and left byte for byte as it was.
- **A symbolic link stays a link.** The target the link (or the chain of links) points at is
  rewritten; the link is never replaced by a regular file. A link whose target does not exist is
  refused with the link's path, and no file is created beside it.
- Every write is atomic (`atomic::write_atomic`), so a reader sees the old or the new file.

**The install record.** seatswap keeps `statuslineInstall` in its own settings:
`{installed, previous}` — the exact `statusLine` value it wrote last, and the exact value the file
had before its first install. `previous` absent means the file had no `statusLine` key; `previous:
null` means the key was there with the value `null`, and that is what comes back. Installing again
while seatswap's line is in place writes nothing and keeps the record. Installing from a moved
program (another `<path>`) writes the new command but keeps the first `previous` while the file
still holds the record's `installed` line, so the user's own line is never replaced by seatswap's
older one; a line set by hand after an install becomes the new `previous`.

**Restore previous.** It is offered only while seatswap's line is in Claude Code — the record exists
and the current value is seatswap's line or the record's `installed` line. It then writes the
record's `previous` back exactly, or removes the key when there was none, and clears the record. A
line set by hand afterwards is never overwritten: without a record it refuses with "seatswap has no
record of the line before its own: nothing to restore", and when the file holds any other line "the
status line in Claude Code is not seatswap's: nothing to restore", leaving the file untouched.

**Undoing it by hand.** In Claude Code's `settings.json`, `statusLine` may be deleted (or replaced by
the old value) with an editor; nothing else seatswap wrote there has to be touched, because it
wrote nothing else. The copy `settings.json.seatswap-backup` is the file as it was before seatswap's
first write to it.

---

## 11.5 Money: API prices, the dollar rate and the value of tokens

Usage is a percent of each account's own week. Money is the common unit that adds accounts up and
says what a token or a subscription was worth. Amounts are shown with the currency code after the
number: `1,234 EUR`, and below 10 with two decimals, `6.47 EUR`.

- **One currency.** `currency` is the currency every amount is shown in. There is no default: until
  it is set, amounts in money stay missing and the page asks for it.
- **A price per account.** An account's `price` is what its subscription costs per month, in the
  currency it was entered in, `priceCurrency`. Without a price the account is left out of every sum
  in money, and the window says so; it is never assumed. A price whose `priceCurrency` is another
  currency than the current one does not count, and the page marks it so it can be entered again; a
  price entered before a currency was set has no `priceCurrency` and does not count either.
- **The dollar rate.** API prices are in dollars; a rate turns them into the chosen currency. A rate
  entered by hand (`usdRate`) wins over the looked-up one, but only while its own `currency` is the
  current one; a rate for another currency is not used. `USD` itself is always 1, with no looked-up
  rate. Without a rate an API value has no value in the currency, and the value says its rate is
  missing; the code never assumes a rate. The settings row shows the rate in effect and where it came
  from, or that API values are shown in USD, and a failed rate look-up is shown there.
- **The value of one percent** of an account's week is `monthly price × 12 / 52 / 100`: 180 EUR a
  month is 41.54 EUR a week and 0.415 EUR per percent.

**API prices from models.dev.** The list is read for three providers: `anthropic`, `openai` and
`ollama-cloud`. A model's key is `<provider>/<model id>`; its price has four parts, in USD per
million tokens: `input`, `output`, `cacheWrite` and `cacheRead`. A model whose `cost.tiers` makes
the price depend on the prompt size is marked `tiered` and counted at its base price. A model that
lacks an input or an output price is left out; a missing cache price counts as 0. A cost that is
not a number, negative or not finite leaves that model out of the list and names it in the settings
("2 models of models.dev's list were left out"), while the rest of the list is stored; a provider
without a model list, an answer that is not the expected model list, and an answer with none of the
three providers are errors naming the answer, and nothing is stored. Any model, listed or not, can
be given an own price in the settings; its key must be `<provider>/<model id>` with one of those
three providers and a non-empty model id.

**Prices are kept with their dates.** Each looked-up price that differs from the stored one starts a
new point at the look-up's time; the tokens of a past day keep the price of that day. `prices.json`
holds the points per key, the last look-up's `checkedAt`, its `etag`, the last change and the own
prices. The price of a key at a time is its own price if there is one, else the last point at or
before that time, else the first point. A model missing from a later list keeps its points; it is
reported as removed once, not on every list that still lacks it. The settings page's price list and
`seatswap prices` show the price in effect.

**The dollar rate from Frankfurter.** `fx.json` holds one currency and its dated points. The rate of
a day is the last point at or before it, else the first point. A looked-up rate that is not a
finite number above 0 is an error and nothing is saved. On the next look-up after the currency
changes, the old currency's points are replaced by a new file for the new currency.

**The two look-ups and the switch.** `lookUpPrices` (default on) is the one switch. Both requests go
through `api`, both carry the User-Agent `seatswap/<version>` and nothing about the user, an account
or this machine:

| What | Request | When |
|---|---|---|
| API prices | `GET https://models.dev/api.json`, with `If-None-Match: <etag>` when a list is stored | at the tray app's start, then every hour while `lookUpPrices` is on; on a Money setting's save; and with `seatswap prices --refresh` |
| The dollar rate | `GET https://api.frankfurter.dev/v1/latest?base=USD&symbols=<currency>` | at the tray app's start and then in the hourly round, once per local day, only when `lookUpPrices` is on and the currency is set and not `USD` |

"Not modified" only moves `checkedAt`; the stored prices stay. A look-up that fails saves nothing:
the last values stay with their age, the error is shown under "API prices" or at the rate, and no
number is zeroed or replaced. After a failed rate request the next round tries again; an answer for
today is not asked for again that day, so a weekend with no new ECB rate does not cause an hourly
request. With `lookUpPrices` off neither host is contacted and `seatswap prices --refresh` refuses;
the stored values keep being shown. Nothing else on the machine is contacted for money.

**What a failure means for a value.** A token record whose model has no price gets no API value and
the model's name is recorded as the reason, never a guessed price. A record without a rate gets no
value in the currency and says the rate is missing. The value of a subscription is left out when
the account has no price, or when the week's used part or its API value is unknown.

**How a value is computed.**
- One token record's API value in USD is
  `(input × input price + output × output price + cache write × cacheWrite price + cache read ×
  cacheRead price) / 1,000,000`, at the price of the record's day. A token record's model is priced
  as an Anthropic model (`anthropic/<model>`), the provider Claude Code's transcripts come from.
  The value in the currency is that USD amount times the rate of the record's day.
- A token record belongs to the account that the history's live and switch events say was in use on
  its host at its time.
- The value of a subscription for a week: the window's used money, `paid − expired` and at most
  what was paid, split among the account's token records of that week in proportion to their API
  value. A record's subscription value is `its API USD / the week's API USD × used money`. The used
  percent is a finished window's final percent, or the latest sample's percent for a running week;
  a window above 100 % expired nothing, so its used money is what was paid. The paid and expired
  money are the Statistics area's "Used, paid and expired money": `paid = 100 ×
  percent_value(price)`, `expired = max(0, 100 − used percent) × percent_value(price)`,
  `used = paid − expired`.
- The price used is the Statistics area's "The price of a window": the account's monthly price at
  the window's midpoint, its end minus 3.5 days; the last `priceChanged` at or before the midpoint,
  the first change when the midpoint lies before all of them, or, when the account has no such
  event, the price stored in `accounts.json` with its `priceCurrency`. A price entered in another
  currency than the current `currency` is not used, and the value is left out; a price without a
  currency does not count either. So is a record whose week has no reading, or whose account's
  records in that week have no API value to split.

`priceChanged` is written to the history when an account's price changes; it carries the new
`amount` and the `currency` in effect at that moment. The same amount saved again in a new currency
is a change, so it is recorded with the new currency. A plan change is `planChanged`.

**The files.**

`<data folder>/prices.json`, pretty JSON, atomic, mode 0600:

```json
{
  "version": 1,
  "checkedAt": "2026-11-15T09:41:12Z",
  "etag": "W/\"1a2b3c\"",
  "lastChange": {
    "at": "2026-11-15T09:41:12Z",
    "added": ["anthropic/claude-opus-5-5"],
    "changed": [],
    "removed": []
  },
  "models": {
    "anthropic/claude-opus-5-5": [
      { "from": "2026-11-14T08:00:00Z",
        "price": { "input": 4.0, "output": 20.0, "cacheWrite": 5.0, "cacheRead": 0.2, "tiered": false } }
    ]
  },
  "overrides": {
    "anthropic/claude-haiku-5-5": { "input": 0.1, "output": 0.5, "cacheWrite": 0.125, "cacheRead": 0.01, "tiered": false }
  },
  "missing": ["openai/gpt-6"]
}
```

`<data folder>/fx.json`, the same way:

```json
{ "version": 1, "currency": "EUR",
  "points": [ { "date": "2026-11-13", "rate": 0.90481 } ] }
```

`checkedAt`, `etag` and `lastChange` are left out of the file rather than written as null;
`models`, `overrides` and `missing` are always present, empty when nothing is there. A file of
another `version` is refused with its path, never replaced.

---

## 11.6 Projects and organizations

A project is a repository, or a folder that has none. Organizations group projects and the accounts
that pay for them. Both are the same on every host once the hosts' project logs have met.

**What names a project.** seatswap reads only `.git` and the git `config` of the folders Claude Code
reports; it executes nothing. From a folder it walks up to the repository root and reads the
`origin` remote. The project's identity is that address, normalised: host lower-cased, no protocol,
no user or token, no port, no `.git`, no trailing `/` (`github.com/acme/atlas`). Without a remote it
is the repository's folder name; outside a repository, the first folder under the home folder, or
`~` for the home folder itself. The key is `repo:<host/path>` or `folder:<name>`. A repository key
is the same on every host; a folder key also names its host (`{"kind":"folder","id":"notes",
"host":"atlas"}`), because a folder project is per host until it is joined by hand.

**Names.** A project's default name is the repository's last path part (`atlas`), the folder's name,
or `~ (home folder)` for `~`. A folder project's default name stays its folder name; the page shows
the host beside it in the folders column. A rename is 1 to 40 characters and may not be only
spaces. A renamed project keeps its key, so a later rename wins and the past keeps counting under
the same project.

**Organizations.** A project's default organization is the repository's owner (`acme` for
`github.com/acme/atlas`); a folder has none. An account's first organization is its login's
`orgName`; without one it has none. An account sits in exactly one organization. Each change of an
account's organization is one `accountOrg` record for that account; the latest such record, in log
order, puts the account in the root of its organization, or in none. Nothing else moves an account,
so moving one account never drops another — even one only another host stores. Organizations have
an identity, which a rename does not change, and a shown name, which starts as that identity.
Organizations can be joined like projects; their accounts and projects then count for the
organization they were joined into.

**Sessions and folders.** A project's folders per host and its session count come from the session
records of every host, over all time: distinct session ids, and the folders the sessions ran in. A
key that no session ever named is not a row of the Projects group. A project seen on two hosts in
two folders is one project; the folders are listed per host. A folder project seen on two hosts is
two projects until they are joined.

**The shared project log.** Every edit is one line in the host's own
`projects-YYYY-MM.jsonl` under the history root, appended and never rewritten. Every host reads all
hosts' files; the later record wins, by its time, ties broken by host name and then by the file's
line order. A join follows its chain; a cycle keeps the later of the records that form it and drops
the earlier one. Because the log travels with the history folder (§1.5), the same names, joins and
organizations appear on every host once the folder has synced. A refused edit appends nothing; the
row shows the reason.

| `edit` | Fields | Meaning |
|---|---|---|
| `rename` | `key`, `name` | the project's shown name |
| `join` | `key`, `into` | `key`'s tokens count for `into`; `key` gets no row of its own |
| `unjoin` | `key` | undo a join |
| `assign` | `key`, `org` (an identity, or `null` for none) | the project's organization; creating an organization is an `assign` with a new name |
| `orgRename` | `org`, `name` | an organization's shown name; `org` stays its identity |
| `orgJoin` | `org`, `into` | `org`'s accounts and projects count for `into` |
| `accountOrg` | `account`, `org` (an identity, or `null` for none) | one account's organization |

A shown project or organization name is 1 to 40 characters and not only spaces. An organization
identity is only checked to be non-empty: it comes from a login's `orgName` or a repository owner,
not from typing. A project or an organization cannot be joined into itself. A folder project id
names its host; a repository id does not, because a repository is one project on every host. The
Projects group shows each project with its repository, its folders per host, its organization
(chosen from the list or created in place), its session count and the keys joined into it, each
with an undo. Under it, the organizations with their accounts and project counts. Changes apply to
the past too, because the history stores the repository's identity, not the shown name.

**The file.** One line per edit, camelCase, the project id as `{"kind":"repo","id":"…"}` or
`{"kind":"folder","id":"…","host":"…"}`:

```json
{"t":"2026-11-15T12:25:05Z","edit":"join","key":{"kind":"folder","id":"atlas-old","host":"atlas"},"into":{"kind":"repo","id":"github.com/acme/atlas"}}
{"t":"2026-11-15T12:30:00Z","edit":"assign","key":{"kind":"repo","id":"github.com/acme/atlas"},"org":"acme"}
{"t":"2026-11-15T12:31:00Z","edit":"orgRename","org":"acme","name":"Acme"}
{"t":"2026-11-15T12:32:00Z","edit":"accountOrg","account":{"acc":"6f1c2a9e-…","org":"a92d7b31-…"},"org":"acme"}
{"t":"2026-11-15T12:33:00Z","edit":"accountOrg","account":{"acc":"6f1c2a9e-…","org":"a92d7b31-…"},"org":null}
```

---

## 12. Later (v2+)

- In-app login for new accounts (OAuth PKCE), so `/login` in Claude Code is no longer needed.
- Claude Desktop: research whether its login can be switched safely (separate `--user-data-dir` per account and swapping which one the launcher uses is the likely route).
- Usage history: a small sparkline per account across the last 7 days, from the poll cache.
- Native Cinnamon, GNOME and KDE front ends (§9.3).

---

## 13. Security and privacy

- Logins go only into the OS keyring, or a `0600` file if there is none. They are never logged; log lines print the account ID and the token's last 4 characters at most.
- Network traffic goes only to `api.anthropic.com` and `platform.claude.com` for an account's usage
  and login; to `chatgpt.com/backend-api/wham/usage` while the Codex card is on; to the Ollama server
  at `ollamaUrl` for `/api/balance` and `/api/usage?range=24h` while the Ollama card is on; and — only
  while `lookUpPrices` is on — to `models.dev` for API prices and `api.frankfurter.dev` for the dollar
  rate (§11.5). The two look-ups send the User-Agent `seatswap/<version>` and no account, token or
  machine data. No telemetry, no update pings unless enabled.
- **The two provider cards' requests.** The Codex card sends one GET to `chatgpt.com` and carries
  Codex's own access token and account id only to that service; the token is never logged, and
  seatswap never refreshes it. The Ollama card sends two GETs to the Ollama server and carries no
  secret: the local server signs them with the machine's own key. Each card is off by default; while
  it is off nothing is read and nothing is sent, and no request generates text (§7.9).
- Every write to Claude Code's files is atomic (temp file + rename) and keeps the original file mode;
  a file that is a symbolic link stays a link and its target is rewritten.
- The status line reads Claude Code's JSON, seatswap's settings and its usage cache; its only write
  is `<data folder>/feed.json` (mode 0600), plus `<data folder>/statusline-keys.json` while a
  recording runs — the key paths and JSON types of the payload, never a value. It prints no token
  and sends none.
- **seatswap's one write in Claude Code's settings:** *Use in Claude Code* writes exactly one key of
  `<config>/settings.json`, `statusLine`, and nothing else in that file changes: every other key
  keeps its value and its order. The file is backed up once (`settings.json.seatswap-backup`), the
  change is shown before it is made, and *Restore previous* puts the exact earlier value back or
  removes the key (§11.4).
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
| The Codex usage endpoint is internal and not documented by OpenAI (§7.9) | OpenAI can change or remove it in any release; the card would then stop showing numbers. | The endpoint lives in `api.rs` with Codex's other endpoints. A changed answer shows the API error text on that card, and nothing else — no switch, no stored account, no Claude reading — is affected. The card is off by default. |
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
