# Windows check — report

## 1. Header

- **Date:** 2026-11-15 (one session, about 00:00–00:45 local time)
- **Windows:** Windows 11 Pro, version 10.0.26300 (build 26300)
- **Claude Code:** 2.1.322, installed with its own installer: `%USERPROFILE%\.local\bin\claude.exe` (a plain file, not a link), versions kept in `%USERPROFILE%\.local\share\claude\versions\` (2.1.318, 2.1.319, 2.1.322). Not installed through npm.
- **Rust:** rustc 1.99.0 (2026-09-28), cargo 1.99.0
- **Clone:** at `3c02dd5`, the same commit as `origin/main`
- **Who ran it:** a Claude Code session (Opus) inside the VS Code extension, started by the owner in this repository.

Marks: **seen** = observed in this check; **inferred** = concluded from what was seen, not observed directly; **not found out** = open.

## 2. Summary

| Check | Answer in one line | Mark |
|---|---|---|
| A1 | Config folder `C:\xxxxx\xxxxx\.claude`, sessions in `C:\xxxxx\xxxxx\.claude\sessions`; `CLAUDE_CONFIG_DIR` not set; 2.1.322 via its own installer | seen |
| A2 | 10 `.json` files at the start: 8 empty (0 bytes), 2 with content; every Linux key present; new: `waitingFor` (only while waiting) and a second file `<pid>.<64 hex>.key` per session | seen |
| A3 | `status`: busy, idle, waiting; `kind`: interactive (also for `claude -p`); `entrypoint`: cli, claude-vscode, sdk-cli; `pidDomain`: `win32:<host name in lower case>`; `nameSource`: derived; `version`: 2.1.322 | seen |
| A4 | Values below; `messagingSocketPath` is a named pipe `\\.\pipe\xxxxx\xx-xxx-<hex>` | seen |
| A5 | The pid is `claude.exe` itself, no wrapper. Parent: `powershell.exe` (terminal, `-p`), `Code.exe` (VS Code) | seen |
| A6 | `procStart` = the process creation time as FILETIME (100 ns since 1601, UTC), same digits; checked on 3 sessions | seen |
| A7 | `status` changes within ≤ 0.25 s; `waiting` appears when the session asks the user a question; `updatedAt` changes **only** with `status` (not while busy, not while idle) | seen; permission prompt: not found out |
| A8 | Clean end (`-p` finished, `/exit`, Ctrl-D): `.json` and `.key` removed. Kill: both stay, `status` stays `busy`, `updatedAt` frozen. The next session start removed them | seen |
| A9 | 8 left-over files, all empty, no Claude process; 2 of their pids now belong to other programs; oldest 31 days | seen |
| A10 | WSL (Ubuntu 24.04) exists, but no Claude Code in it (`~/.claude` missing); no WSL path in any Windows transcript | seen |
| B1 | No. `updatedAt` is not a heartbeat (unchanged for 31 min idle, 19 s busy); a killed session's file looks exactly like a busy one. No limit is supported by the facts | seen |
| B2 | The pipe lives exactly as long as the process (gone ≤ 0.25 s after a kill). `read_dir(r"\\.\pipe\")` works with std only, ~0.43 ms, and lists the session pipes. Every kind of session has one | seen |
| B3 | Slow: 1.3–2.3 s per call (first and tenth). `CREATE_NO_WINDOW` does prevent the console window (owner watched) | seen |
| B4 | List confirmed. No crate in `Cargo.lock` gives process times safely; `winreg` could read them from the performance data, but its read loop hangs on that key | seen |
| B5 | Smallest additions: a fixed `winreg` read loop (no new crate), or `sysinfo` 0.37.2 (MIT, adds `windows` 0.59–0.61), each with drawbacks | inferred |
| B6 | **Recommendation:** live = file non-empty and parses **and** its `messagingSocketPath` is in the `\\.\pipe\` listing | recommendation |
| C1 | Every character other than `A–Z a–z 0–9` becomes `-` (`C:\x y.z` → `C--x-y-z`); drive-letter case is kept from the first session of that folder | seen |
| C2 | Same layout as Linux | seen |
| C3 | LF only, no CR; UTF-8 without BOM | seen |
| C4 | All keys present, same types. Largest transcript: 1,157 answers, 827 with more than one line, 0 with differing `usage` | seen |
| C5 | Same three kinds; but "real" user lines also include task notifications and messages from other sessions (`origin.kind`) | seen |
| C6 | Backslashes, drive letter mostly upper case (632,931 lines) but 55,668 lower case; the same folder occurs in both spellings, even inside one transcript; no other forms; `cwd` follows a later change of folder | seen |
| C7 | Meta keys listed below; `finished` is **absent** while the agent runs (never `false`) and `true` within 0.2 s of its end | seen |
| C8 | 50 of 50 reads OK while the session wrote; no error | seen |
| C9 | 7,908 transcript files, 5,122 MB; largest 167.9 MB; oldest 2026-08-07 | seen |
| D1 | `CLAUDECODE=1`, `AI_AGENT=claude-code_2-1-322_agent`, `CLAUDE_CODE_ENTRYPOINT=claude-vscode` in both tools | seen |
| D2 | Bash tool = Git Bash 5.2.26 (MINGW64); PowerShell tool = Windows PowerShell 5.1. `seatswap` found in both, but the installed one is **0.1.0** | seen |
| D3 | Yes, `<config>\skills\<name>\SKILL.md` | seen |
| E1 | Started through Git Bash (`claude.exe` → `bash.exe` → `bash.exe` → program). The form `"<path with space>" statusline` works | seen |
| E2 | Lists below. Before the first answer: no `rate_limits`, `context_window.used_percentage` is `null`. No width key in the payload | seen |
| E3 | `COLUMNS=120`, `LINES=30`; stdout is a pipe; `CONOUT$` opens; stdin ends after the JSON; UTF-8 | seen |
| E4 | Colours and characters right; the 100-character line shown whole on a 120-column terminal, indented by 2 spaces | seen |
| E5 | Every 2.00 s (±0.02 s), while working and while idle, plus extra calls on events | seen |
| E6 | Test program 8.7 ms median (in-process); `seatswap --version` 11.7 ms median, empty Rust program 6.4 ms median; first calls much slower | seen |
| F1 | Dropbox: 297 appends, 0 errors, 1.94 ms median, 3.84 ms slowest | seen |
| F2 | Reader: 60 reads, 0 errors; writer unaffected | seen |
| F3 | TEMP: 297 appends, 0 errors, 1.81 ms median, 3.21 ms slowest | seen |
| F4 | Folder deleted; no conflict copy, no lock file, nothing in `.dropbox.cache` | seen |
| G1 | `xxxxxxx-xx`, 10 characters, allowed set; same as `hostname` **only ignoring case** (variable upper, `hostname` lower); set for a program started through Explorer | seen |
| H1 | Codex installed (winget); folder `%USERPROFILE%\.codex`, `CODEX_HOME` not set; `auth.json` present, `tokens` is an object; config names no credential store | seen |
| H2 | Ollama answers; both endpoints 200; amounts are JSON numbers | seen |
| I1 | Same commit as `origin/main`; Rust 1.99.0 | seen |
| I2 | All four pass: build 44.5 s, test 15.8 s (264 passed, 1 ignored), clippy 12.3 s, clippy CLI-only 3.8 s | seen |

## 3. Details

### A. Session files

How: read-only scripts (Python, PowerShell) in a throw-away folder under `%TEMP%`. A watcher polled the sessions folder every 250 ms for about 40 minutes and logged file appear / change / disappear, the `status`, `updatedAt`, `statusUpdatedAt` and whether the session's pipe was in the `\\.\pipe\` listing. A second logger recorded which keys changed (names only).

**A1.** `CLAUDE_CONFIG_DIR` is not set; the config folder is `C:\xxxxx\xxxxx\.claude`. Session files: `C:\xxxxx\xxxxx\.claude\sessions\<pid>.json`. Install: see header. (seen)

**A2.** At the start there were 10 `.json` files: 8 empty (0 bytes), 2 with content. Over all files with content seen during the check (5 sessions), every file had:

| Key | JSON type |
|---|---|
| `pid` | number (whole) |
| `sessionId`, `cwd`, `procStart`, `version`, `kind`, `entrypoint`, `pidDomain`, `messagingSocketPath`, `name`, `nameSource`, `status` | string |
| `startedAt`, `updatedAt`, `statusUpdatedAt`, `nameSince`, `peerProtocol` | number (whole) |
| `peerFeatures` | array of strings |
| `asksForConvertedSkills` | bool |
| `waitingFor` | string, **only while `status` is `waiting`** (added and removed with it) |

- Not in the Linux list: `waitingFor`. Missing from the Linux list: none.
- **But:** for the first ~250 ms of a session the file has no `status`, `updatedAt` or `statusUpdatedAt` (the watcher could not tell absent from `null`; the file is ~70 bytes shorter, so absent is likely). Seen on 3 starts. The planned parser, which requires `status`, would fail on a session that is just starting.
- `peerFeatures` was `["notify_idle","artifact_yield"]` on all sessions here (Linux: `["ack"]`).
- **A second file per session:** `<pid>.<64 hex digits>.key`, 114 bytes, JSON with `peerToken` (string, 32 characters — a secret), `procStartFt` (string, equal to `procStart`), `pidDomain` (string). It is written atomically through `<name>.key.tmp.<8 hex>` and renamed. It lives and dies with the `.json` file.
- The files are one line, no line end at the end, UTF-8 without BOM, no CR.
(seen)

**A3.** Values seen over the check (5 live sessions):
- `status`: `busy`, `idle`, `waiting`
- `kind`: `interactive` only — also for `claude -p`
- `entrypoint`: `cli` (terminal), `claude-vscode` (VS Code), `sdk-cli` (`claude -p`)
- `pidDomain`: `win32:xxxxxxx-xx` — the host name in lower case, as `hostname` prints it (see G1)
- `nameSource`: `derived`
- `version`: `2.1.322`
(seen)

**A4.** The check's own session (VS Code), read at one moment: `pid` 19524, `procStart` `"134391702120758613"`, `startedAt` 1794696616830, `updatedAt` 1794697457635, `statusUpdatedAt` 1794697457635, `status` `busy`. `updatedAt` always equalled `statusUpdatedAt` in every reading.
- `cwd` shape: `c:\xxxxxxxx\xxxxxxxx` (VS Code: lower-case drive letter), `C:\xxxxx\xxxxx\xxxx` (terminal: upper case). Backslashes.
- `messagingSocketPath` shape: `\\.\pipe\xxxxx\xx-xxx-<32 hex>` — a **named pipe**, not a socket file. The name has a random part, so it is not tied to the pid.
(seen)

**A5.** `Get-CimInstance Win32_Process` for the pid, its parent and grandparent:

| Session | Process with the pid | Parent | Grandparent |
|---|---|---|---|
| terminal (`claude`) | `claude.exe` (`C:\xxxxx\xxxxx\.xxxxx\xxx\xxxxxx.xxx`) | `powershell.exe` | `explorer.exe` |
| `claude -p` (started from PowerShell) | `claude.exe` | `powershell.exe` | — |
| VS Code extension | `claude.exe` inside the extension folder (`c:\xxxxx\xxxxx\.xxxxxx\xxxxxxxxxx\xxxxxxxxx.xxxxxx-xxxx-9.9.999-xxx99-x99\...\xxxxxx.xxx`) | `Code.exe` | `Code.exe` |

The pid is always Claude Code itself, no wrapper. The VS Code extension brings its own `claude.exe`, not the installed one. (seen)

**A6.** For pid 19524 (VS Code session):

| Form | Value | Matches `procStart`? |
|---|---|---|
| `procStart` | `134391702120758613` | — |
| FILETIME from `Get-Process … StartTime.ToFileTimeUtc()` | `134391702120758613` | **yes, same digits** |
| ms since 1970 | `1794696612075` | no |
| s since 1970 | `1794696612` | no |
| WMI `CreationDate` | `20261114235012.075861+060` | no (and only µs precision) |
| ms since boot | `261400576` | no |

Boot time: FILETIME `134389088115000000`, ms since 1970 `1794435211500`; uptime at the time of reading about `262261000` ms.

Same exact match on pid 36532 (terminal) and pid 12096 (a `claude -p` child of this check). The `.key` file repeats the value as `procStartFt`. Note: `Get-CimInstance … CreationDate.ToFileTimeUtc()` gives `…758610` (last digit lost, WMI keeps µs); `Get-Process` gives the exact value.

`startedAt` is 1.6 s (terminal) and 4.8 s (VS Code) after the process creation time, so it cannot stand in for `procStart`. (seen)

**A7.** From the watcher (250 ms polls) and the key logger:
- Own session (VS Code): `busy` → `waiting` within ≤ 0.25 s of the session asking the owner a question (4 times); back to `busy` when he answered; `idle` when the turn ended. `waitingFor` (string) is present only while `waiting`.
- Owner's terminal session (part E): `idle` at start; `busy` when he sent a message; `idle` again within ~0.1 s of the answer ending (the status line call that brought the first `rate_limits` came 15 ms before `statusUpdatedAt` of `idle`).
- `claude -p`: the file appears ~1.8 s after process start without `status`; `busy` follows 0.25 s later.
- **`updatedAt` changes only together with `status`.** The idle terminal session's `updatedAt` stayed the same for its whole life (31 minutes). A `claude -p` doing 19 s of work never changed it.
- The idle session's file was rewritten once (10 minutes after its start, +4 bytes) without any change to `status` or `updatedAt`. Which key changed: not found out (the key logger started later); it did not repeat in the next 10 minutes.
- **Permission prompt:** not found out. The owner works in auto mode, so no permission prompt appeared; a second try in default mode was not done.
(seen, except where marked)

**A8.**
- `claude -p "Reply with OK"` (8.6 s): `.key` appeared 1.0 s after start, `.json` 1.8 s after start, both removed when the command ended. (seen)
- A longer `claude -p` killed with `taskkill /F` after 20 s: `.json` and `.key` **stay**; `status` stays `busy`; `updatedAt` stays at the start of its work; the pipe disappears within one poll (≤ 0.25 s). (seen)
- When the owner then started a new session, that start **removed the killed session's `.json` and `.key`** within 1.5 s. It did **not** remove the 8 empty left-over files. (seen)
- Interactive session closed by the owner (part E): `.key` removed, `status` set to `busy` with the pipe already gone, `.json` removed 0.76 s later. The idle session closed with Ctrl-D Ctrl-D: `.json` and `.key` removed within one poll. (seen)

**A9.** At the start: 8 files with no Claude process behind them, all **empty (0 bytes)**, no `.key` beside them. 2 of their pids are now used by other programs (`svchost`, Docker Desktop). Oldest: 31 days. Six of the eight were last written within 15 seconds of each other on one evening — probably a crash or shutdown while they were being written (inferred). Why they are empty: not found out. No non-empty left-over existed before the kill test. (seen)

**A10.** WSL is installed (Ubuntu-24.04, docker-desktop, both stopped). Inside Ubuntu: no `claude` on PATH, no `~/.claude`. No transcript `cwd` on Windows has a `\\wsl$\` or `/mnt/` form. (seen)

### B. Telling a live session from a dead one

**B1.** The file alone is not reliable: `updatedAt` is not refreshed while idle (31 min) or busy (19 s), and a killed session's file keeps `status: busy` (until the next session start cleans it up). There is no age limit the facts support. (seen)

**B2.** Test program: Rust, std only, `std::fs::read_dir(r"\\.\pipe\")`, 20 calls.
- Works: ~350 pipes, 0.41–0.65 ms per listing (median 0.43 ms). The names come back with their inner backslash (`xxxxx\xx-xxx-<hex>`), so the part after `\\.\pipe\` of `messagingSocketPath` can be compared directly. Both live sessions' pipes were found. (seen)
- The pipe exists from the first written version of the file, for every kind seen (`cli`, `claude-vscode`, `sdk-cli`). It was gone ≤ 0.25 s after `taskkill /F` while the file stayed, and gone at a clean exit. (seen)
- On the check's own `claude -p` child: `std::fs::metadata()` on its pipe returned `Ok` (length 0) in 61 µs; `Path::exists()` returned `true` in 10 µs. The child continued until it was killed as planned. Whether these calls open a client connection on the pipe: not found out (std opens a handle internally, so probably yes — inferred). Listing the folder needs neither. (seen / inferred)

**B3.** Test program: Rust, `std::process::Command` with `CREATE_NO_WINDOW`, 10 calls each, 3 pids per call:

| Child command | Call 1 | Call 10 | Range |
|---|---|---|---|
| `powershell -Command "Get-CimInstance Win32_Process -Filter '…' …"` | 2,270 ms | 1,332 ms | 1,332–2,567 ms |
| `powershell -Command "Get-Process -Id … | … StartTime.ToFileTimeUtc()"` | 1,498 ms | 1,289 ms | 1,222–1,498 ms |

`wmic` is not installed. `Get-Process` gives the exact FILETIME; for a protected process (pid 4) it gives nothing. A console-less test program (`#![windows_subsystem = "windows"]`) started PowerShell twice: without the flag a console window appeared (the owner saw it); with `CREATE_NO_WINDOW` nothing appeared. (seen)

**B4.** `Cargo.lock` holds: `windows-sys` 0.45.0, 0.52.0, 0.59.0, 0.60.2, 0.61.2; `windows` 0.62.2; `winapi` 0.3.9; `winreg` 0.56.0; `uds_windows` 1.2.1; `windows-version` 0.1.7; `dirs` 6.0.0 and 7.0.0. Your list is right; I add that `libc`, `nix`, `rustix`, `tauri`, `tao`, `wry`, `tray-icon`, `muda` are there too, and none of them gives process information on Windows. No crate gives "process id alive" or its creation time through a safe API. (seen)

One near-miss: `winreg` (safe API) can read `HKEY_PERFORMANCE_DATA`, value `"230"` (the Process object), whose "Elapsed Time" counter holds each process's creation time as FILETIME. A Python test read it in 75–82 ms (1 MB, 415 processes) and the value **equals `procStart` exactly** for both live sessions. But `winreg` 0.56's `get_raw_value` **hangs** on that key (test stopped after 30 s): when Windows answers "buffer too small" it does not enlarge the size it passes for the next try, and performance data never reports the needed size. (seen; the cause is from reading `winreg`'s source)

**B5.** If the pipe listing (B6) is not accepted:
1. A fixed `winreg` read loop (upstream fix or a patch): no new crate, safe, exact `procStart` match; but ~80 ms and 1 MB per reading of all processes. (inferred)
2. `sysinfo` 0.37.2 (MIT; in the local cargo cache, not in `Cargo.lock`): on Windows it adds `windows` 0.59–0.61 (a second version beside 0.62.2) and optionally `ntapi`. As far as I know its `start_time()` has only whole seconds, so it cannot match `procStart` exactly. (inferred, not tested)
3. A small `unsafe` module with `windows-sys` (`OpenProcess` + `GetProcessTimes`): breaks hard rule 6.
Each needs the owner's approval (new dependency or a rule change).

**B6. Recommendation.** A session file belongs to a live session when (1) the file is not empty and parses, and (2) the part of `messagingSocketPath` after `\\.\pipe\` is in the listing of `std::fs::read_dir(r"\\.\pipe\")`. Read the listing once per refresh and check every file against it. Why: std only, no `unsafe`, no child process, under 1 ms; the pipe dies with the process (seen after a kill); its name has a random part, so a reused pid cannot fool it; every kind of session has one. Its weakness: it depends on Claude Code keeping this messaging pipe; a file without `messagingSocketPath` cannot be judged, which should be shown as "unknown", not as alive or dead. A file without `status` (first ~250 ms) should count as "starting", not as an error. *(recommendation)*

### C. Transcripts and agents

How: a read-only Python survey over all files under `projects`; it printed only shapes, key names, types and counts.

**C1.** Rule: every character other than `A–Z a–z 0–9` becomes `-`. 7,018 of 7,040 transcripts match this rule for the first `cwd` in them. All folders are on drive C (first `cwd`: 6,798 `C:`, 243 `c:`). Shapes (drive letter shown as `C`, its case varies):
- plain: `C:\xxx` → `C--xxx`
- with a space and digits: `C:\xxxxx\xxxxx\xxxxxxx\xxxxxxxxx xxxxxxxx\9999-99 xxxx xxxxx xxxxx` → `C--xxxxx-xxxxx-xxxxxxx-xxxxxxxxx-xxxxxxxx-9999-99-xxxx-xxxxx-xxxxx`
- with a dot: `C:\…\.xxxxxx\xxxxxxxxx\xxxxx-xxxxx` → `C--…--xxxxxx-xxxxxxxxx-xxxxx-xxxxx`
- non-ASCII: no such folder exists here.
- Of 37 project folders, 31 start with `C`, 3 with `c`, and 3 with `-`: those 3 look like Linux paths (`-xxxx-xxxxxxx-…`), hold no transcripts, only other files, two written today. What writes them: not found out.
The 22 others: 18 differ only in the case of the drive letter (the folder keeps the case of whichever session created it; a later session with the other case writes into the same folder, as Windows ignores case); 4 are worktree folders (`<repo>\.claude\worktrees\<name>`) whose transcript starts with the main repository as `cwd`. (seen)

**C2.** Yes: 37 project folders, 7,041 `<folder>\<sessionId>.jsonl`, 867 `<folder>\<sessionId>\subagents\agent-<id>.jsonl`, each with an `agent-<id>.meta.json`. Other files beside them (in `<sessionId>\…`): tool-result folders and the like. (seen)

**C3.** Largest transcript (167.9 MB): 14,355 lines, LF only, no CR, no BOM, valid UTF-8. (seen)

**C4.** All 2,642 `assistant` lines of the largest transcript have `type`, `message.id`, `message.model`, `timestamp`, `cwd`, `sessionId` (strings), `isSidechain` (bool), `message.usage` (object) with `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens` (whole numbers). 1,157 answers; 827 have more than one line; **0** of those have lines with differing `usage`. Many other line types occur besides `user` and `assistant` (`attachment`, `ai-title`, `last-prompt`, `mode`, `api-request`, `api-request-blob`, `file-history-delta`, `queue-operation`, `system`, …). (seen)

**C5.** In the largest transcript: 23 user lines with string content, 33 with a `text` block, 1,290 with only `tool_result` blocks, 14 with `isMeta: true` — the same three kinds as on Linux. But over the 30 largest transcripts, lines that look "real" by that rule split by `origin.kind` into `human` 1,474, `task-notification` 562, and 801 without `origin`. `turnOrigin` has `human`, `task_notification`, `scheduled`, `peer`, `system`; `promptSource` has `sdk`, `system`, `typed`. So the Linux rule also counts task notifications and messages from other sessions as user messages. (seen)

**C6.**
- All `cwd` values use backslashes and a drive letter; no forward slashes, no `\\wsl$\`, no `/c/`.
- Transcript lines: 632,931 with an upper-case drive letter, 55,668 with lower case.
- By the first `cwd` of a transcript: `cli` 73 upper / 0 lower; `sdk-cli` 6,565 upper / 0 lower; `claude-vscode` 159 upper / 243 lower. Session files: `cli` and `sdk-cli` upper, `claude-vscode` lower.
- 4 of 251 folders occur in both spellings; 71 transcripts even contain the same folder in both spellings.
- 103 transcripts contain genuinely different folders, so `cwd` follows a later change of folder (e.g. into a worktree).
(seen)

**C7.** Over all 867 meta files: `agentType`, `description`, `toolUseId` (string), `spawnDepth` (whole number) in all; optional: `requestNonInteractive` (bool, 841), `requestShape` (string, 841), `model` (string, 564), `parentAgentId` (string, 383), `finished` (bool, 255), `launchedWithDefaults` (bool, 66), `isBuiltIn` (bool, 5), `effort` (string, 4), `isFork` (bool, 3), `stoppedByUser` (bool, 2). The check started one small agent (about 21 s) and polled its folder every 200 ms: the meta file appeared with the transcript, with `pickedEffortModel` too and **without `finished`**; at the end it was rewritten with `finished: true`, within 0.2 s. `finished: false` was never seen. (seen)

**C8.** A Rust program (`std::fs::File::open` + `read_to_end`) read the check's own transcript 50 times, 400 ms apart, while the session wrote: 50 OK, 0 errors, the file grew between reads 5 times; median under 1 ms, slowest 232 ms. (seen)

**C9.** 7,908 transcript files (7,041 sessions + 867 agents), 5,122 MB in total; largest single file 167.9 MB; oldest 2026-08-07. (seen)

### D. What a command started by Claude Code sees

**D1.** Same values in the Bash tool and the PowerShell tool: `CLAUDECODE=1`, `AI_AGENT=claude-code_2-1-322_agent`, `CLAUDE_CODE_ENTRYPOINT=claude-vscode`. Other names in both: `CLAUDE_AGENT_SDK_VERSION`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_EMIT_STARTUP_TIMING`, `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`, `CLAUDE_CODE_ENABLE_TASKS`, `CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_QUESTION_PREVIEW_FORMAT`, `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_STARTUP_FAILURE_RESULTS`, `CLAUDE_PID`. Only in the Bash tool: `CLAUDE_EFFORT`, `CLAUDE_CODE_EXECPATH`. (seen)

**D2.** Bash tool: Git Bash (`/usr/bin/bash`, 5.2.26, MINGW64). PowerShell tool: Windows PowerShell 5.1. `seatswap` is found in both (`C:\xxxxx\xxxxx\.xxxxx\xxx\xxxxxxxx.xxx`, i.e. next to `claude.exe`). Its `--version` prints **`seatswap 0.1.0`** — older than this repository's releases. (seen)

**D3.** `<config>\skills\<name>\SKILL.md`: yes, for 3 of 4 folders. The fourth holds folders named by UUIDs instead of a `SKILL.md`; what writes it: not found out. (seen)

### E. The status line command

How: a throw-away Rust program `sl-test.exe` in a folder whose name has a space; it logged, per call, the time, its run time, every key path of stdin with its JSON type (never a value), and the facts of E3. The owner started `claude --settings "<throw-away folder>\status line\settings-test.json"` from a throw-away folder; the settings had `"command": "\"<path with space>\\sl-test.exe\" statusline"` and `"refreshInterval": 2`. The user's real settings were not touched. 209 calls in 6 min 52 s.

**E1.** The program's parents were `bash.exe` → `bash.exe` → `claude.exe`, and Git Bash's variables (`MSYSTEM`, `SHLVL`, `_`, `SHELL`) were set: Claude Code runs the command through Git Bash. The form seatswap would write — the path in double quotes, then ` statusline` — works; the program got `statusline` as its only argument. Documentation and program code were not consulted. (seen)

**E2.** Before the first answer (33 key paths): `context_window` {`context_window_size`, `total_input_tokens`, `total_output_tokens`: whole; `current_usage`, `remaining_percentage`, `used_percentage`: **null**}, `cost` {`total_api_duration_ms`, `total_cost_usd`, `total_duration_ms`, `total_lines_added`, `total_lines_removed`: whole}, `cwd` string, `effort.level` string, `exceeds_200k_tokens` bool, `fast_mode` bool, `model.id`, `model.display_name` string, `output_style.name` string, `scratchpad_dir` string, `session_id` string, `thinking.enabled` bool, `transcript_path` string, `version` string, `workspace` {`added_dirs` array, `current_dir`, `project_dir` string}.

After the first answer (61 key paths) additionally: `context_window.current_usage` {`input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`: whole}, `remaining_percentage`, `used_percentage` whole; `cost.total_cost_usd` **fractional** (it was a whole `0` before); `prompt_cache` {`cache_write_tokens`, `expected_rebuilds`, `expires_at`, `misses`, `miss_recache_tokens`, `recache_tokens_if_cold`, `requests`: whole; `hit_ratio`: fractional; `caching_observed`, `warm`: bool; `last_miss_at`: whole or null; `last_miss_cause`: null; `miss_causes`: object; `ttl`: string}; `prompt_id`, `session_name` string; `rate_limits.five_hour` and `rate_limits.seven_day`, each `used_percentage` and `resets_at` whole.

| Key | Before first answer | After |
|---|---|---|
| `workspace.current_dir` | yes | yes |
| `model.id`, `model.display_name` | yes | yes |
| `effort.level` | yes | yes |
| `rate_limits.five_hour.used_percentage`, `.resets_at` | no | yes |
| `rate_limits.seven_day.used_percentage`, `.resets_at` | no | yes |
| `context_window.used_percentage` | yes, `null` | yes, number |
| `cost.total_cost_usd` | yes (whole `0`) | yes (fractional) |
| `cost.total_lines_added`, `cost.total_lines_removed` | yes | yes |

No key carries the terminal's width. `current_dir` shape: `C:\xxxxx\xxxxx\xxxxxxx\xxxxx\xxxx\xxxxxxxx-xxxxxxxx-9xx99xx9` (backslashes; the drive letter's case was not logged, the project folder Claude Code made for it starts with `C--`). (seen)

**E3.** `COLUMNS=120`, `LINES=30`. `CLAUDE` variables: `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_EFFORT`, `CLAUDE_PID`, `CLAUDE_PROJECT_DIR`. Standard output is a pipe (`GetFileType` = pipe, not a terminal). `CONOUT$` opens (a console is attached). Standard input ends right after the JSON (EOF on every call). The JSON is valid UTF-8. (seen)

**E4.** The owner saw the 24-bit orange, the basic green and the characters `█ ░ · ↻ ▸` correctly. His terminal is 120 columns wide (`COLUMNS=120`). Claude Code indents the status line by 2 spaces, so the 100-character line ended at column 102: shown whole, not wrapped, not cut. A line of exactly the terminal's width was not tested. (seen)

**E5.** First call 0.7 s after the session file appeared. Then every 2.00 s (±0.02 s), both while the session worked and while it sat idle, plus extra calls on events (e.g. 0.45 s after a regular call when an answer arrived, and when a turn ended). So `"refreshInterval": 2` gives a call every two seconds, and events add a few more. (seen)

**E6.** Run time of the test program, measured inside it (without process start): median 8.7 ms, slowest 47.8 ms (the first call). Wall time, 20 calls each, from PowerShell (`Stopwatch` around the call, so a little PowerShell overhead is included):

| Program | First | Median | Slowest |
|---|---|---|---|
| installed `seatswap --version` (0.1.0) | 938 ms | 11.7 ms | 938 ms |
| empty Rust program, release | 62.8 ms | 6.4 ms | 62.8 ms |

The first start of a program is much slower (probably the virus scanner; inferred). (seen)

### F. Appending inside a file-sync folder

The owner allowed it. Dropbox was running. Folder `seatswap-probe-<random>` directly in the Dropbox folder; a throw-away Rust program appended every 200 ms for 60 s (open for append, one `write_all` of ~200 bytes, `sync_all`, close); a second program read the whole file once a second. Same test at the same time in a folder under `%TEMP%`.

| | Appends | Errors | Median | 95 % | Slowest | Reads | Read errors |
|---|---|---|---|---|---|---|---|
| Dropbox (F1, F2) | 297 | 0 | 1.94 ms | 2.83 ms | 3.84 ms | 60 | 0 |
| `%TEMP%` (F3) | 297 | 0 | 1.81 ms | 2.83 ms | 3.21 ms | 59 | 0 |

The file had all 297 lines. **F4:** the folder was deleted ~10 s after the test; 30 s later there was nothing with its name in the Dropbox folder, no "conflicted copy", nothing in `.dropbox.cache`. Only one machine wrote; several machines appending to the same file was not tested. (seen)

### G. Host name

**G1.** `%COMPUTERNAME%`: shape `xxxxxxx-xx`, 10 characters, only letters, digits and `-` — fits "at most 63 of letters, digits, `.`, `_`, `-`". It equals `hostname` **only when case is ignored**: the variable is upper case, `hostname` prints lower case. The session files' `pidDomain` uses the lower-case form. A console-less test program started through `explorer.exe` (as a Start-menu program is) found the variable set, 10 characters. (seen)

### H. Other providers

**H1.** Codex CLI is installed through winget (`codex.cmd` in the WinGet packages folder). `CODEX_HOME` is not set in the environment; its folder is `%USERPROFILE%\.codex`. `auth.json` keys: `auth_mode` string, `OPENAI_API_KEY` null, `tokens` **object** {`id_token`, `access_token`, `refresh_token`, `account_id`: string}, `last_refresh` string. `config.toml` has no key about a credential store or keyring; what Codex does by default: not found out. (seen)

**H2.** Ollama runs and answers at `http://127.0.0.1:11434`.
- `GET /api/balance` → 200, `application/json`: `included` {`balance_usd` fractional, `allowance_usd` whole, `period` {`from`, `until` string}}, `purchased` {`balance_usd` fractional}.
- `GET /api/usage?range=24h` → 200: `range`, `scope`, `granularity`, `from`, `until` string; `totals` {`request_count`, `input_tokens`, `cached_input_tokens`, `output_tokens` whole; `usage_usd` fractional}; `buckets` array of {`from`, `until` string; `request_count`, `usage_usd`, `input_tokens`, `cached_input_tokens`, `output_tokens` numbers}.
- Amounts are JSON numbers, not strings. `usage_usd` came as a whole number in a bucket (probably `0`), so a parser must accept whole and fractional numbers.
- Someone is signed in (balance data came back; inferred). The answer when nobody is signed in: not found out.
(seen)

### I. The machine as a place to build

**I1.** `HEAD` = `origin/main` = `3c02dd5`. rustc 1.99.0, cargo 1.99.0. (seen)

**I2.** Run one after another, with the `target` folder as found (not a clean build):

| Command | Result | Time |
|---|---|---|
| `cargo build --offline --workspace` | pass | 44.5 s |
| `cargo test --offline --workspace` | pass: 103 + 16 + 145 passed, 1 ignored, 0 failed | 15.8 s |
| `cargo clippy --offline --workspace --all-targets -- -D warnings` | pass | 12.3 s |
| `cargo clippy --offline -p seatswap --no-default-features --all-targets -- -D warnings` | pass | 3.8 s |

(seen)

## 4. Seen on the way

1. **Running sessions — a secret beside the session file.** Each session has `<pid>.<64 hex>.key` with a `peerToken` beside its `.json`, plus a short-lived `….key.tmp.<hex>` while it is written. seatswap must read only `<digits>.json` and never open, log or show the `.key` files.
2. **Running sessions — files that are not "complete".** A session file is empty (8 left-overs) or lacks `status`/`updatedAt`/`statusUpdatedAt` (first ~250 ms). The parser must handle both without an error popping up.
3. **Running sessions — `kind` does not tell `-p` apart.** `claude -p` has `kind: interactive`; only `entrypoint: sdk-cli` shows it. 6,565 of 7,040 transcripts here come from `sdk-cli` (scripts and tools that call `claude -p`), so a list of running sessions could be dominated by them.
4. **Running sessions — Claude Code cleans up.** A starting session removed a killed session's files. The 0-byte left-overs were not removed.
5. **Status line — seatswap will look like an agent.** The status line command gets `CLAUDECODE` in its environment (E3), so the planned agent rule (`CLAUDECODE` = `1`) would treat `seatswap statusline` as an agent call. Its value and whether `AI_AGENT` is set there were not logged. The status line command also gets `CLAUDE_PROJECT_DIR` and `CLAUDE_CODE_SESSION_ID`.
6. **Status line — payload extras.** It carries `session_id`, `transcript_path`, `cwd`, `workspace.project_dir`, `session_name`, `context_window.*`, `prompt_cache.*`. `cost.total_cost_usd` is a whole `0` before the first answer: parse numbers as numbers, whole or fractional.
7. **Status line — width.** No width in the payload, but `COLUMNS` is set. The line is indented by 2 spaces, so the usable width is probably `COLUMNS − 2` (inferred).
8. **Tokens per project — drive-letter case.** The same folder appears as `C:\…` and `c:\…`, even within one transcript; VS Code sessions mostly use lower case. Project keys on Windows must ignore case.
9. **Tokens per project — repository address.** User lines carry `serverClassifierContext.context.git_state.visibility.origin.remote` (string, 874 of 1,360 lines in the largest transcript). That is the repository address without reading `.git/config` (inferred useful; values not looked at).
10. **Tokens per project — unknown line types.** Transcripts hold many line types besides `user`/`assistant`; the reader must skip them. The largest is 168 MB, the total 5.1 GB: the first full read is big.
11. **Real user messages.** `origin.kind` (`human`, `task-notification`, `peer`) and `promptSource` tell a typed message from a notification; the Linux rule would count notifications as user messages.
12. **History across machines.** `pidDomain` is `win32:<host name in lower case>`, while `COMPUTERNAME` is upper case. If the host name goes into history files, pick one case.
13. **Agents.** In commands Claude Code runs, `AI_AGENT` is `claude-code_<version with dashes>_agent`.
14. **The installed seatswap is 0.1.0** (`%USERPROFILE%\.local\bin\seatswap.exe`), older than the repository's 1.0.x releases.

## 5. Not found out

- **A7, `waiting` on a permission prompt:** the owner works in auto mode, so no prompt appeared; a second try in default mode was not done.
- **A7, the one rewrite of an idle session's file** (+4 bytes, 10 minutes after start): which key changed. It did not repeat in the next 10 minutes.
- **A9, why the 8 left-over files are empty.** Six were written within 15 s of each other on one evening; a crash or shutdown is a guess.
- **B2, whether `metadata()` / `exists()` on a pipe opens a client connection.** Probably yes, as std opens a handle; not verified. Only listing the folder is known to be harmless.
- **B5, `sysinfo`'s precision and footprint:** from its `Cargo.toml` and memory, not tested.
- **D3, what writes the skills folder that has UUID-named folders instead of a `SKILL.md`.**
- **E1, what the documentation says:** only the test was used.
- **E4, a line exactly as wide as the terminal:** not tested (the test line was 100 of 120).
- **E5/E3, the value of `CLAUDECODE` and whether `AI_AGENT` is set in the status line command's environment.**
- **F, appends from several machines at once:** only one machine was tested.
- **H1, where Codex keeps the login when the config says nothing.**
- **H2, Ollama's answer when nobody is signed in:** someone is signed in; nobody was signed out.

## 6. Left behind

- The throw-away folder `%TEMP%\seatswap-wincheck-<random>` (scripts, test programs and their build folder, logs): **deleted** at the end.
- The Dropbox test folder: **deleted**; nothing left (F4).
- Background watchers started by this check: **stopped**.
- **Left by Claude Code itself, not by this check's code:** the three `claude -p` test runs, the owner's part E session and the small test agent were recorded by Claude Code like any session. That is one project folder named after the throw-away folder under `<config>\projects\` (3 transcripts and a `memory` folder) and one agent transcript in this check's own session folder. Under the rules this check may not delete anything in the config folder; the owner can delete that project folder.
- No file in the repository was changed except this report.
