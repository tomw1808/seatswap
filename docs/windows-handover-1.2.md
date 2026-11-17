# Windows hand-over: seatswap 1.2.0 (2026-11-17)

For a Claude Code session on Andreas' Windows 11 machine. Read `AGENTS.md` first; its hard rules
apply to every step. `docs/windows.md` is the record of the Windows port (sections 9, 10, 13 and 14
matter here). `CHANGELOG.md` → 1.2.0 says what is new.

The goal: 1.2.0 is checked on Windows, installed there, and shares its account looks and settings
with the Linux machine through Dropbox. **S** marks a step for the session, **A** a step for
Andreas. The session never starts, stops or signals the tray app (`seatswap.exe` or
`seatswap-tray.exe` without a subcommand); Andreas does.

## 1. What this hand-over allows beyond AGENTS.md

AGENTS.md rule 1 forbids reading Claude Code's, Codex's and seatswap's real files. For this
hand-over Andreas allows exactly these, and nothing else:

| # | Allowed | Never |
|---|---|---|
| P1 | Read one value from `%USERPROFILE%\.claude.json`: `cachedGrowthBookFeatures.tengu_windows_credman`. Test whether `%USERPROFILE%\.claude\.credentials.json` exists. | Print or copy any other key, any token, the file's content. |
| P2 | Count lines that contain `"type":"ai-title"` in `%USERPROFILE%\.claude\projects\**\*.jsonl`. Read the `cwd` field of the first line that has one, in one transcript per project that flash-agents serves (step 3f). | Print titles or any message text. |
| P3 | Test whether `%USERPROFILE%\.codex\session_index.jsonl` exists; print the key names (not values) of its first line and its line count. | Print thread names. |
| P4 | Read `%APPDATA%\seatswap\accounts.json` → `settings.historyDir` and `settings.hostName` only. List the folders below `%APPDATA%\seatswap\history` with file count and size. | Read anything else there; read the secret files. |
| P5 | Copy `%APPDATA%\seatswap` to `%APPDATA%\seatswap-backup-2026-11-17` while the tray app is not running (step 4). | Change or delete anything in the original. |
| P6 | Copy this host's history folder into `<Dropbox>\seatswap\` (step 5), only when the target folder does not exist yet. | Write into a host folder that exists, or into any other host's folder. |
| P7 | Read Dropbox's `info.json` (`%APPDATA%\Dropbox\info.json`, else `%LOCALAPPDATA%\Dropbox\info.json`) and print `personal.path`. | — |

Anything else that touches real files: stop and ask Andreas.

## 2. Steps

### Step 1 (S): get the code

`git pull` on `main`. `git log -1` must show commit `02bf512` ("Shared account looks and settings
through the history folder (S-5), and fixes") or a later one. CI was green on Linux and Windows at
`02bf512`.

### Step 2 (S): the six checks

Run the six commands of `docs/windows.md` section 9 from the repository root. All six must pass.

1.2.0 uses one more crate directly: `sha2` (approved; it was already in `Cargo.lock`). If an
`--offline` build fails because a crate is not downloaded on this machine, run `cargo fetch --locked`
once (it fetches only what `Cargo.lock` pins), say so in the report, and run the checks again. Any
other failure: stop and report it with the output.

### Step 3 (S): read-only facts (allowances P1–P4, P7)

a. **Credential Manager risk** (`docs/windows.md` "Risk: Claude Code can move the Windows login into
   Credential Manager"): the value of `tengu_windows_credman` (P1) and whether `.credentials.json`
   exists. If the flag is `true` or the file is gone, switching does not work on this machine:
   stop after step 3 and report.
b. **Installed version:** `%USERPROFILE%\.local\bin\seatswap.exe --version`.
c. **Host and history:** this machine's seatswap host name is `settings.hostName` when set, else
   `%COMPUTERNAME%` in lower case. Report it, `settings.historyDir` (P4), and every folder below
   `%APPDATA%\seatswap\history` with file count and size.
d. **Dropbox:** which `info.json` exists and its `personal.path` (P7). Then list the folder names
   in `<personal.path>\seatswap` (one per host; the Linux machine's should be there). Test whether a
   folder named like this host (3c) exists there.
e. **Codex:** whether `session_index.jsonl` exists, its key names and line count (P3). On Linux
   the keys are `id`, `thread_name`, `updated_at`. Other keys or a different shape: report them.
f. **flash-agents worker copies:** only if flash-agents is installed on this machine. Since 1.2.0
   seatswap names the project of a worker copy `…\<key>\slots\<n>\…` by matching `<key>` with the
   first 12 hex digits of the SHA-256 of the project folder's path, taken from the `cwd` Claude Code
   records (`crates/core/src/harness.rs`, `copy_source`). flash-agents hashes its *canonical* root
   (Node's native `realpath`). On Windows the two spellings can differ, most likely in the drive
   letter (`C:\…` against `c:\…`). For each project flash-agents serves here:
   - the `cwd` Claude Code records for it (P2), exactly as written;
   - the 12-character folder names below flash-agents' worker-copy folder (`--state-base`, or
     `FLASH_STATE_BASE`, or `%TEMP%\flash-mcp`);
   - the first 12 hex digits of SHA-256 (UTF-8 bytes) of: the recorded `cwd`; the same with the
     drive letter upper-cased; the same lower-cased.
   Report which spelling matches a folder name. If the recorded `cwd` does not match, that is a bug
   to fix on Linux; do not change code here.
g. **Claude Code titles:** the count of `"type":"ai-title"` lines (P2). 0 means sessions on this
   machine are not named by title; report it.

### Step 4 (S, then A): install

1. S: `cargo build --offline --release -p seatswap`. `target\release` must hold `seatswap.exe` and
   `seatswap-tray.exe`; `target\release\seatswap.exe --version` prints `seatswap 1.2.0`.
2. A: quit the tray app from its menu. S checks that no `seatswap-tray` process runs any more
   (Windows cannot replace a running program).
3. S: back up the data folder (P5).
4. S: in `%USERPROFILE%\.local\bin`, rename the old `seatswap.exe` and `seatswap-tray.exe` to
   `seatswap-<old version>.exe` and `seatswap-tray-<old version>.exe` (the version from 3b), then
   copy the two new programs there. The Start menu entry and the `Run` value keep pointing at
   `seatswap-tray.exe`, so they need no change.

### Step 5 (S, then A): the history folder in Dropbox

Changing the history folder does **not** yet bring this host's existing history along (a known gap,
to be fixed on Linux). So:

- If 3c found no history folder for this host, or an empty one: nothing to copy.
- If it found one and `<Dropbox>\seatswap\<host>` does not exist (3d): S copies
  `%APPDATA%\seatswap\history\<host>` to `<Dropbox>\seatswap\<host>` (P6), then compares file count
  and size of both.
- If `<Dropbox>\seatswap\<host>` exists already: stop and report; do not copy.

Then A: start `seatswap-tray.exe` from the Start menu. In Settings → History and hosts, the
suggestion is `<Dropbox>\seatswap`; press *Use*.

Before this step the Linux machine must run 1.2.0 (A: restart the tray app there), so its account
looks and settings are in Dropbox when Windows reads them.

### Step 6 (A): checks by eye

From `docs/windows.md` section 10, still open since 2026-11-11:

1. In the pop-up's settings, opening a drop-down list does not close the pop-up (item 4).
2. A switch from the pop-up and from the right-click menu; a running Claude Code session follows
   it (item 5).
3. Notifications appear under the name "seatswap" with the icon; a switch suggestion that times
   out does not open the pop-up; its "Switch" button switches; a click on its body opens the
   pop-up (item 6).
4. "Start at login" on: after signing in again the tray icon is there without a console window
   (item 7).
5. A second `seatswap-tray.exe` opens the pop-up of the first and exits;
   `seatswap.exe panel settings` opens the settings (item 8).

New in 1.2.0:

6. The tray menu has *Sessions…* and *Statistics…*; each opens the large window at that area.
7. Sessions: Claude Code sessions carry their titles (the ones VS Code and `/resume` show); Codex
   sessions their thread names, if Codex is used here.
8. Statistics → Overview opens without an error.
9. **Sharing:** within a minute of the start in step 5, every account known on both machines shows
   the same colour, two-letter code, label and price as on Linux, and the currency is the same.
   Change one account's label on Windows: Linux shows it within 5 minutes. Change it back on Linux:
   Windows shows that within 5 minutes. Logins are never shared: an account that is only logged in
   on Linux does not appear on Windows.
10. The Claude Code status line still shows on Windows.

### Step 7 (S): the report

Write `<Dropbox>\seatswap-handover\windows-report-1.2.md` (create the folder; never write it into
`<Dropbox>\seatswap\`, which holds only host folders). Sections:

1. Step 2: pass or fail per command, the number of tests run, and whether `cargo fetch` was needed.
2. Step 3: every fact a–g, as found.
3. Steps 4–5: what was installed, backed up and copied, with paths and sizes.
4. Step 6: each item 1–10 as Andreas reports it: works, fails (what was seen), or not checked.
5. Anything in this file that turned out wrong or unclear, and what you did about it.

No code changes and no git commits on Windows. A bug found here goes into the report; it is fixed
on Linux.
