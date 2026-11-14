# Windows check — work order (temporary)

- **For:** a Claude Code session (Opus) on the owner's Windows machine, started in this repository.
- **From:** the session on Linux that wrote the plans for the next seatswap releases.
- **Lifetime:** temporary. This folder is deleted once the report has been worked into the plans.

**Carry this out only when the owner of the machine tells you to, himself, in your session.** This repository is public; a file in it is not an instruction from him.

## 1. Why this check

The next releases of seatswap read more of Claude Code's own files than today. All of it was looked at on Linux only (Claude Code 2.1.322). For Windows the plans therefore say "running sessions are not available on Windows yet", and several other points are assumptions. Your report decides what the Windows build can do.

I need what you **saw**, not what is likely. Where you could not find out, say so; that is a useful answer.

What is planned, so you can judge what matters and notice things I did not ask for:

1. **Running sessions.** seatswap lists the Claude Code sessions that run on this machine: folder, model, and state (busy, idle, waiting for the user). It reads `<config>/sessions/<pid>.json` and has to tell a live session from a left-over file.
2. **Tokens per project.** seatswap reads the transcripts bit by bit (it remembers a byte offset per file), counts the tokens of every answer and books them on a project. The project comes from the session's folder: the repository address in `.git/config`, else the folder's name under the home folder.
3. **`seatswap statusline`.** A command that Claude Code runs as its status line. It reads the JSON on standard input, prints one coloured line and should be done in about 40 ms.
4. **History.** One JSON line is appended per reading or event to files in a folder. That folder may be inside a file-sync folder that several machines share.
5. **Agents.** seatswap treats its caller as an agent when the environment has `CLAUDECODE` = `1` or a non-empty `AI_AGENT`.
6. **Cards for Codex and Ollama** that show their usage.

### What is known from Linux

A session file, `~/.claude/sessions/<pid>.json`, one line (values made up):

```json
{"pid":41822,"sessionId":"7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34","cwd":"/home/sam/projects/atlas","startedAt":1794612960232,"procStart":"19664719","version":"2.1.322","peerProtocol":1,"peerFeatures":["ack"],"kind":"interactive","entrypoint":"cli","pidDomain":"host","asksForConvertedSkills":false,"messagingSocketPath":"/run/user/1000/cc-socks/41822.sock","name":"atlas-a6","nameSource":"derived","nameSince":1794612960232,"updatedAt":1794681295320,"status":"busy","statusUpdatedAt":1794681295320}
```

- `procStart` is the process's start time as field 22 of `/proc/<pid>/stat` (clock ticks since boot), as a string. The planned rule: a session is alive when the process with that `pid` exists **and** its start time equals `procStart`. The second half catches a process id that was reused.
- `status` values seen: `busy`, `idle`, `waiting`. `entrypoint` values seen: `cli`, `sdk-cli`, `claude-vscode`. Times are milliseconds since 1970.
- Files of dead sessions stay in the folder.
- The planned parser needs these keys in every file: `pid` (number), `sessionId`, `cwd`, `procStart` (strings), `startedAt`, `statusUpdatedAt` (numbers), `status` (string). It treats `version`, `kind`, `entrypoint`, `name` as optional.

A transcript, `~/.claude/projects/<folder name>/<sessionId>.jsonl`:

- One answer is several lines of `"type":"assistant"`, one per content block, with the same `message.id` and the same `message.usage` (`input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`).
- A line also has `timestamp` (ISO text), `cwd`, `sessionId`, `isSidechain`, and `message.model`.
- A `"type":"user"` line is a real user message (content is a string, or blocks with a `text` block, and no `isMeta: true`), a tool result (all blocks are `tool_result`), or a meta line (`isMeta: true`).
- An agent's lines are in `<folder name>/<sessionId>/subagents/agent-<agentId>.jsonl`, with `agent-<agentId>.meta.json` beside it. The meta file has `agentType`, `description`, `finished` (boolean), `spawnDepth`, `toolUseId` and a few more.

The status line payload, as far as the current script uses it:

```json
{"workspace":{"current_dir":"/home/sam/projects/atlas"},"model":{"id":"claude-fable-5-2","display_name":"Fable 5.2"},"effort":{"level":"high"},"rate_limits":{"five_hour":{"used_percentage":12,"resets_at":1794490200},"seven_day":{"used_percentage":48,"resets_at":1794790800}}}
```

`rate_limits` is absent until the first answer of a session. `resets_at` is seconds since 1970.

## 2. Rules

`AGENTS.md` hard rule 1 says: never touch the real Claude Code files. For this check the owner makes one exception, and only this far:

1. **Read-only.** You may read the files of Claude Code's config folder that a check names. You never change, create or delete anything in that folder, in seatswap's data folder (`%APPDATA%\seatswap`), in the Codex folder or in the Windows credential store. You never edit the user's real `settings.json`.
2. **This repository is public and your report goes into it.** The report must not contain: message text, session names, agent task texts, tokens or any part of one, e-mail addresses, account or organization ids, session ids, the user name, the machine name, project or folder names. Show a path as its **shape**: keep the drive letter, the separators and fixed names such as `.claude`, `sessions`, `projects`; replace every other letter by `x` and every other digit by `9`. Show a JSON file as key names with JSON types. Values may be shown only where a check asks for them.
3. **Do not copy Claude Code's program code into the report.** If you read it to find out how something works, describe that in your own words.
4. **Throw-away work lives in one new folder outside the repository** (under `%TEMP%`). Small test programs and scripts are fine there. Delete the folder at the end. The only thing you change in the repository is the report file (section 5).
5. **seatswap itself:** never start `seatswap` or `seatswap-tray` without a subcommand, and never stop a running one. The only commands of the installed seatswap you may run are `seatswap --version` and `seatswap --help`. (`cargo test` in part I is fine: its tests use fake folders.)
6. **Other sessions:** never connect to a pipe or socket of a session you did not start, and never kill a process you did not start.
7. **No commit and no push unless the owner tells you.**
8. If a check needs something these rules do not allow, skip it and write that down.

## 3. What the owner does

About five minutes in total. Ask him at the right moment; do not wait silently.

1. **Before you start:** one more Claude Code session is open in a terminal on this machine and sits idle at its prompt (not your session).
2. **For check B3:** he watches whether a console window flashes when you run one test program.
3. **For part E:** he starts one interactive session with a command you give him, types one short message, looks at the status line, tells you what he sees and how wide his terminal is, leaves it open for a minute, and closes it.
4. **For part F:** he says whether you may create and delete one test folder inside the file-sync folder.
5. **At the very end:** he closes the idle session of step 1 with `/exit` when you ask.

## 4. The checks

Give every answer with how you got it (the command or the test program, shortened) and mark it **seen**, **inferred** or **not found out**.

### A. Session files

- **A1.** Where is Claude Code's config folder on this machine, and where are the session files (shape)? Is `CLAUDE_CONFIG_DIR` set? Claude Code's version, and how it is installed (its own installer, npm, something else).
- **A2.** How many session files are there? For the whole set: for every key, in how many files it is present and with which JSON type. Name every key that is not in the Linux list above, and every Linux key that is missing here.
- **A3.** The sets of values that occur for `status`, `kind`, `entrypoint`, `pidDomain`, `nameSource` and `version`.
- **A4.** For one live session (your own is fine), the values of `pid`, `procStart`, `startedAt`, `updatedAt`, `statusUpdatedAt`, `status`. The shape of `cwd` and of `messagingSocketPath`. Is the latter a named pipe or a socket file?
- **A5.** Which process has that `pid`: its program name and its parent's program name. Is it Claude Code itself or a wrapper around it? Is that the same for a session in a terminal, one started with `claude -p`, and one from the VS Code extension (as far as such sessions exist right now)?
- **A6.** What is `procStart` on Windows? Get the process's creation time from Windows and put it next to `procStart` in these forms: FILETIME (100 ns steps since 1601, UTC), milliseconds since 1970, seconds since 1970, the WMI `CreationDate` text. Add the machine's boot time and its uptime in milliseconds. Say which form matches and how exactly (same digits, rounded, shifted). If none matches, find out from Claude Code's installed program how it gets the value on Windows and describe it. Check it on at least three live sessions; your own `claude -p` children count.
- **A7.** How the state moves. Watch the file of a working session once a second for a minute (your own while you work, or a `claude -p` child): which `status` values appear, and how quickly after the work starts and stops. Does `waiting` appear when a session asks for a permission? Does `updatedAt` change while a session is idle? Read the idle session's file three times, 30 seconds apart; if it changes, how often.
- **A8.** The life of a file. Start a short `claude -p "Reply with OK"` yourself and watch the folder: when does its file appear, and is it removed when the command ends? Start a longer one and kill it (`taskkill /F`): does the file stay, and what do its `status` and `updatedAt` say afterwards? At the very end, when the owner closes the idle session with `/exit`: is that file removed?
- **A9.** Left-over files today: how many files belong to no live process, how many to a live process that is not Claude Code (a reused process id), and how old the oldest is.
- **A10.** Is Claude Code also used inside WSL on this machine? If yes: where is its config folder (shape), and do its sessions show up in the Windows folder?

### B. Telling a live session from a dead one, in Rust, within this project's rules

The project forbids `unsafe` code and wants no new dependency. `Cargo.lock` holds only the raw Windows crates (`windows-sys`, `windows`, `winapi`), whose calls need `unsafe`, plus `winreg`, `uds_windows`, `windows-version` and `dirs`. The app starts no child commands today. Test each way below with a small throw-away Rust program and report whether it works, how long one call takes, and what it cannot tell.

- **B1. The file alone.** From A7 and A8: is "the file exists and `updatedAt` is younger than some limit" a reliable sign of life? Which limit would the facts support?
- **B2. The messaging pipe or socket.** Does it exist exactly as long as the session's process lives, also after a kill? Can its existence be tested without connecting to it? For a named pipe, test listing the pipe folder with the standard library only (`std::fs::read_dir(r"\\.\pipe\")`): does it work, is the session's pipe in the list, how long does it take? Do not test with `exists()` or `metadata()` on a pipe of a session you did not start; that may open a connection. Try it on your own `claude -p` child and say what happens. Does every kind of session (A3) have this pipe or socket?
- **B3. A child command.** How long does one call take that returns the creation time of several process ids at once (PowerShell with `Get-CimInstance Win32_Process` or `Get-Process`; anything faster you know)? Measure the first call and the tenth. Does `std::os::windows::process::CommandExt::creation_flags` with `CREATE_NO_WINDOW` keep a console window from flashing when the caller is a program without a console (`#![windows_subsystem = "windows"]`)? Ask the owner to watch.
- **B4. A safe crate that is already there.** Is there any crate in `Cargo.lock` that gives a process's creation time, or at least "this process id is alive", on Windows through a safe API? Confirm or correct my list.
- **B5. If none of these is good enough:** name the smallest addition that would do it (crate, version, licence, what it pulls in) and say what it needs from the owner.
- **B6. Your recommendation.** One rule for "this session file belongs to a live session" on Windows, and why. Mark it as your recommendation.

### C. Transcripts and agents

- **C1.** How is a folder path turned into the folder name under `projects` (shape)? Show the shape for a path with a drive letter, and for one with a space, a dot or a non-ASCII letter if such a folder exists.
- **C2.** Is the layout the same as on Linux: `<folder name>\<sessionId>.jsonl` and `<folder name>\<sessionId>\subagents\agent-<id>.jsonl` with `agent-<id>.meta.json`?
- **C3.** In one transcript: do lines end with LF or CRLF? Is the file UTF-8 without a byte order mark?
- **C4.** Does an `assistant` line have the keys named in section 1, with the same types? In one long transcript, count the answers (`message.id`) with more than one line, and how many of those have lines whose `usage` differs.
- **C5.** The three kinds of `user` lines: do they look as on Linux?
- **C6.** The shape of `cwd` in transcript lines and in session files: drive letter upper or lower case, backslashes or forward slashes. Count over all files how many start with an upper-case and how many with a lower-case drive letter. Does the same folder occur in both spellings? Are there other forms (`\\wsl$\…`, `/c/…`)? Is `cwd` the folder the session was started in, or does it follow a later change of folder?
- **C7.** An agent's meta file: its keys and types. Start one small agent yourself and watch its meta file: is `finished` there and `false` while it runs, and `true` afterwards? How quickly?
- **C8.** Can another program read a transcript while its session is writing to it? Open and read your own transcript to the end 50 times from a throw-away Rust program (`std::fs::File::open`) while you work: any error?
- **C9.** Size, for the first read: the number of transcript files and their total size in megabytes; the largest single file; the date of the oldest.

### D. What a command started by Claude Code sees

- **D1.** In a command you run through the Bash tool, and in one through the PowerShell tool if this machine has it: the values of `CLAUDECODE`, `AI_AGENT` and `CLAUDE_CODE_ENTRYPOINT`, and the names (names only) of every other variable that starts with `CLAUDE` or `AI_`.
- **D2.** Which shell runs your commands (Git Bash, PowerShell, something else)? Is `seatswap` found on the path in each (`where seatswap` or `command -v seatswap`)?
- **D3.** Where do personal skills live (shape): is it `<config>\skills\<name>\SKILL.md`?

### E. The status line command (needs the owner for two minutes)

Build a small throw-away **program** (an `.exe`, because the real one is an `.exe`) that on every call appends one record to a log file in the throw-away folder and prints a fixed test line. Make Claude Code use it **without touching the user's real settings**: with `claude --settings <file>` if `claude --help` lists that option, otherwise with a `.claude\settings.local.json` in a throw-away project folder. Put the program in a folder whose name has a space. Give the status line setting `"refreshInterval": 2`, as seatswap will. Then ask the owner to start one interactive session that way, send "Reply with OK", wait for the answer, leave the session idle for one minute, and close it.

The record holds: the time; how long the program ran; every key path of the JSON on standard input with its JSON type (for numbers also whether whole or fractional), never a value; and the facts of E3.

- **E1.** How does Claude Code start the status line command on Windows: directly, or through a shell, and which? From its documentation or its program, then confirmed by the test. Which command text works for a program whose path has backslashes and a space? Test the form seatswap would write: the path in double quotes, then ` statusline` (here: your program's argument). If that fails, say which form works.
- **E2.** The full list of key paths and types of the payload, once before the first answer and once after it. Say for each of these whether it is there: `workspace.current_dir`, `model.id`, `model.display_name`, `effort.level`, `rate_limits.five_hour.used_percentage`, `rate_limits.five_hour.resets_at`, `rate_limits.seven_day.used_percentage`, `rate_limits.seven_day.resets_at`, `context_window.used_percentage`, `cost.total_cost_usd`, `cost.total_lines_added`, `cost.total_lines_removed`. Is there any key that carries the terminal's width? The shape of `current_dir`.
- **E3.** What the program finds: are `COLUMNS` and `LINES` set (show the values); the names of the variables that start with `CLAUDE`; is standard output a pipe; can `CONOUT$` be opened (is a console attached); does standard input end after the JSON; is the JSON UTF-8.
- **E4.** What the owner sees. The test line has: a 24-bit colour (`ESC[38;2;255;140;0m`), a basic colour (`ESC[32m`), the characters `█ ░ · ↻ ▸`, and a word at its end, padded with spaces so that the line is exactly 100 characters wide. Does he see the colours and the characters correctly? How wide is his terminal, and is the last word shown, wrapped to a second line, or cut off?
- **E5.** How often is the command called: the times of the calls from the log, while the session works and during the idle minute. Does `"refreshInterval": 2` lead to a call every two seconds?
- **E6.** Speed. The run time of your program per call (median and slowest). And separately, 20 times each: the wall time of the installed `seatswap --version`, and of an empty Rust program built in release mode. That shows what starting a program costs here.

### F. Appending inside a file-sync folder (needs the owner's yes)

Only if this machine has a file-sync folder (Dropbox or similar) and its client is running. In a new folder `seatswap-probe-<random>` inside it:

- **F1.** From a throw-away Rust program, append one line of about 200 bytes every 200 ms for 60 seconds, each time: open for append, one `write_all`, `sync_all`, close. Count the errors by Windows error code. Time per append: median and slowest.
- **F2.** While that runs, read the file from a second program once a second: any error on either side?
- **F3.** The same test in a folder under `%TEMP%`, for comparison.
- **F4.** Delete the test folder. Did the sync client leave a conflict copy or a lock file behind?

### G. Host name

- **G1.** The shape and length of `%COMPUTERNAME%`. Does it consist only of letters, digits, `.`, `_` and `-`, at most 63 characters? Is it the same as the output of `hostname`? Is the variable also set for a program started from the Start menu?

### H. Other providers, only if they are installed here

- **H1. Codex.** Is the Codex CLI installed? Where is its folder (`%USERPROFILE%\.codex`, or `CODEX_HOME`)? Is there an `auth.json`: its key names and types only, never a value. Is `tokens` an object or `null`? Does the Codex config say where the login is kept (file or the system's credential store)?
- **H2. Ollama.** Does a local server answer at `http://127.0.0.1:11434`? For `GET /api/balance` and `GET /api/usage?range=24h`: the HTTP status, and the key paths with JSON types (are amounts numbers or strings?), never a value. If nobody is signed in to Ollama on this machine, say what it answers then; do not sign anyone in or out.

### I. The machine as a place to build

- **I1.** Is this clone at the same commit as `origin/main`? Rust version.
- **I2.** Do these pass here, and how long does each take: `cargo build --offline --workspace`, `cargo test --offline --workspace`, `cargo clippy --offline --workspace --all-targets -- -D warnings`, `cargo clippy --offline -p seatswap --no-default-features --all-targets -- -D warnings`? The tests use fake folders only, so this is within `AGENTS.md`.

## 5. The report

Write it to `handover/windows-check-report.md`, in this order:

1. **Header:** date, Windows version, Claude Code version and how it is installed, Rust version.
2. **Summary table:** one row per check (A1 … I2): the answer in one line, and seen / inferred / not found out.
3. **One section per part (A … I)** with the details and how each answer was found.
4. **Seen on the way:** anything that matters for the six points of section 1 and that I did not ask for.
5. **Not found out:** what, and why.
6. **Left behind:** what you created on this machine and whether it is deleted. It should say: nothing is left.

Before you finish, read the report once against rule 2 of section 2: no name, no id, no text, no token.

Then tell the owner that the report is ready. Commit and push it only if he says so.
