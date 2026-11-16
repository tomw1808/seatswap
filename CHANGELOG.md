# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.2.0] - 2026-11-17

### Added

**Pop-up**
- The auto-burn line names each account with its coloured chip; the tray menu and notifications show
  the same line as plain text.
- *N sessions* opens the Sessions area, *N waiting for you* opens that session there, *see the plan*
  opens Statistics → Forecast, and *Statistics ↗* opens Statistics. A waiting session id the window
  cannot read is refused with an error.

**Large window**
- Settings → History and hosts suggests `<Dropbox>/seatswap` while no history folder is set, read
  from Dropbox's own `info.json`; Use saves it as the history folder. A file that cannot be read is
  shown with its path and why.
- The tray menu has *Sessions…* and *Statistics…*, each opening the large window at that area; they
  sit between *Add account…* and *Settings…*.
- The Settings area has no second heading under its tab.
- The Settings menu stays on screen while the content pane scrolls.

**Sessions**
- A Claude Code session is named by its own title, the one VS Code and `/resume` show, with its
  label (`atlas-a6`) as a small grey tag after it; a long title is cut and shown whole on hover. The
  waiting notification and Activity's largest sessions name the session by its title too. The title
  is read from the transcript's `ai-title` lines and stored in the session records, so it travels
  with a shared history folder; a session closed before this version keeps its label until its next
  title line. A DeepSeek Harness session that is a row of its own is named by its last
  `session/title` the same way; a flash job's title stays its task text and names no row. A Codex
  session is named by its thread name, read from Codex's `session_index.jsonl` (the newest line for
  its id, so a `/rename` wins).
- Codex sessions (provider "Codex") beside Claude Code's, read from Codex's rollout files: their
  session id, folder, model, token counts and times. A Codex session counts as running while a
  process holds its file open or its last event is younger than 30 minutes. A token total that went
  down leaves its last hour unknown, shown as "—" with the reason.
- DeepSeek Harness sessions (provider "DeepSeek Harness") and flash workers: a flash job hangs under
  the Claude Code session that started it, as an agent tagged "flash", with its workers below. A job
  named in a transcript is linked there; on Windows a running job appears once its result is
  collected.
- Every total row shows the same numbers as a session row: Agents, Tokens, Last hour, value and API
  value. A model under two providers has one sum per provider; a model total row carries the
  "thinking tied" bubble, and a total's Agents shows "—" when every row it sums has no agent data.
- The "thinking tied to the account" bubble replaces the brown dot in the Models card and the words
  on a tied model's row in the By model tree; it also marks each session row that has it.
- The last hour has its EUR and API EUR values, the "N completed" agents line has its total, and a
  done agent without a price keeps its reason in the sum.
- A total's last hour is "—" when any row it sums has an unknown one. A known 0 is an empty last
  hour, not "—".
- A running session counts its linked flash/Harness workers in Agents, Tokens and both values
  exactly as its closed row does, so nothing jumps when it closes.
- A shown duration under a minute counts its seconds, redrawn once a second.
- A blocking 5 h, week or model limit that drops early sends `PR is usable again: its week was reset
  early.` (or `its 5h window was reset early` / `its {model} week was reset early`) at once, once per
  account and reset.
- Claude Code's own `cost.total_cost_usd` from the status line becomes the session's API value. An
  absent cost is no cost; a cost that is not a finite number of 0 or more is not stored, and the
  window shows the reason as a warning while it lasts.

**Status line**
- A separator's character is chosen from pipe `│`, middle dot `·`, bullet `•`, slash `/`, chevron
  `›` or blank; a new separator starts as ` │ `.
- Spaces in a segment's prefix and suffix are shown as a faint dot and a count, such as "2 spaces".
- A segment is removed by dropping it on the Segments box ("drop here to remove") or with the
  Options card's *Remove*; Delete keeps working.
- The account tag's background covers its letters only.

**History and statistics**
- A limit drop of at least one percentage point before its reset time ends the window as a reset,
  never a reading error; a smaller drop is kept as a reading and logged once. The window before the
  reset keeps its use, the next window records the percent it began at (`from`), and a week may use
  more than 100 %.
- A week keeps every window it offered, the one before an early reset and the one after, each placed
  by its own midpoint; a week can hold more than one ordinary window too. A reset window counts
  nothing as expired, and a week's expired share is the sum of every window's, so a reset week's
  used and expired money can be more than one week's price.
- A week's Used and Expired percentages and its summary divide by the windows the week offered, not
  by one week's price.
- In Weeks, a cell draws the used part as a bar and the expired share as a hatched band from the
  top; a click on a part opens that window's detail, also when it shares its week.
- One window seen by two hosts counts once: the reset record wins over an ordinary record, and of
  two ordinary records the one with the latest last reading.
- Overview's Used and Expired shares are of the windows the week offered; a window above 100 %
  counts its whole use, so both shares together can pass 100 % and nothing is cut. Its Blocked line
  names the added account by its short name, also when the account filter leaves it out.
- Overview shows Used and Expired with their percentages, a Switches total and "auto-burn"; Weeks
  shows plan and weekly price, "of about N possible" (or "N used, more than the about M your usual
  working hours hold"), the 100 % line, the "now" mark and clickable weeks with their detail;
  Projects shows its matrix totals, the Used footer, each project's share of all use, the "API values
  at 1 USD = …" line and the by-account bar in the account colours; a share that cannot be computed
  is "—" with its reason, never "0 %"; Forecast shows its missing labels, "about N a day" and the
  "none usable" lane last.
- Activity shows a total tokens box, the "What it shows" sentence ("night(s) with work on <host>"),
  the sum over hosts, a "running" tag on the current week, a taller hour chart and host names that
  wrap instead of being cut.
- Ollama's hourly usage, with requests, tokens and US dollars, is kept in the history.
- "Other Ollama use" is Ollama's hourly input + output minus the tokens of every known session on an
  Ollama model (`…:cloud`) in that UTC hour, over every host whatever the page's host filter. An
  hour where the known sessions used more has a note and no "other" value. The KPI, the lane and the
  legend entry show only when Ollama reported at least one hour in the range.
- The Statistics frame names the history's start and size ("since … · now", Export as a button) and
  marks where subscription values begin.
- Organizations whose names differ only in case are one organization, also in a 1.1.0 history.
  Organizations can be added and removed by hand; one that comes from a GitHub owner cannot be
  removed, and any project can be assigned to another organization.

**Several hosts**
- Hosts that share a history folder share each account's label, abbreviation, colour, "Auto-burn may
  use it" and monthly price, and the settings currency, notifications and status line. An edit
  writes only what it changed, as an `accountLook` or `settingSet` line (the price stays a
  `priceChanged`); each field takes the newest value over all hosts, so the last edit wins. The tray
  app takes the other hosts' edits at start and every 5 minutes, and the open pages show them without
  a restart. A value that does not pass this host's own checks, such as an abbreviation one of its
  accounts already has, is not applied and is named in a warning with the host that wrote it. At
  start a host shares its own values of every field no host has shared yet, except settings still at
  their default. Logins never travel: an account appears on a host only after its login there, and a
  login of an account the history knows takes its shared look and price at once (the tray app,
  `seatswap add` and a switch that saves the live login). Settings → History and hosts says what is
  shared.

**Money**
- The rate line reads "ECB rate of Fri 13 Nov · European Central Bank via Frankfurter · fetched today
  10:10". The rate is fetched once per Frankfurt day while it is older than today, and on working
  days from 16:00 again every 30 minutes until an answer carries today's date or it is 18:00. An
  `fx.json` from 1.1.0 keeps its rate but has no fetch time, so the page says "fetched: not
  recorded".
- The model list lives in Settings → Providers, under "API prices from models.dev · checked 11:11",
  and is read-only.
- Claude Code's own reported cost beats a models.dev price; a model with neither has no API value. A
  Claude plan model (its id starts with `claude-`) without a price is asked for from models.dev at
  once, at most once per model and hour; a model outside the plan is never asked for.
- With a currency set and no stored dollar rate yet, API values are shown in US dollars labelled USD,
  and the settings row says `no rate yet: API values are shown in USD`; they are not unknown. With
  the look-ups off the same USD label says `look-ups are off: API values are shown in USD`.

**Other**
- The Swap S icon: a lowercase s with arrow ends on a green square, for the launcher, the Windows
  `.exe`, notifications and every window. The tray icon stays the live one. Notifications show
  `<data folder>/notification-icon.png`, written when it is missing or holds another picture; no
  installed icon file is needed.
- Durations are written without a space between number and unit: `2h 31min`, `4h`, `12min`,
  `3d 4h`; from a day on, the minutes are not shown.
- A session, agent or hour without tokens is worth a known 0, not an unknown value. A sum is
  unknown only when a part is unknown, and a missing value shows "—" with its reason as a tooltip.

### Changed
- With *Look up prices and the dollar rate* off, API values stay in US dollars and the column header
  says so.
- A status line saved by 1.1.0 with a separator that had no character now prints `│` where 1.1.0
  printed ASCII `|`. A segment's prefix and suffix keep the value's colour when no background is
  painted.
- A model outside the Claude plan (its id does not start with `claude-`) never takes part in a Claude
  account's week: its subscription value is a known 0 and it never makes the week unknown. *If you
  switched now* counts only Claude Code rows on plan models (or with no model known yet).
- A reset is measured against the last poll reading only: the status line's merged number can never
  show one, and a pair of readings that is a reset is left out of auto-burn's learned rates.
- The status line feed is rewritten when the session, its cost or a cost problem changes, not only
  when the 5 h or weekly numbers change, so a session's cost is seen within seconds.
- An agent without Claude Code's `finished` mark counts as done once its transcript ends with the
  agent's final answer (`stop_reason` `end_turn`); a resumed agent counts as running again.

### Fixed
- Settings: every row wraps at 1100 px and below (the label takes the first line, the control the
  second); the focus frame in table fields shows all four sides; the Join projects line no longer
  overlaps its button; the money rows no longer flicker; a missing rate shows "—" with its reason in
  the note, and the API prices row says `checked Fri 13 Nov 11:11`; and the Activity page's Per week
  / Largest sessions switch sits inside its frame.
- Sessions: "In use" wraps inside its box instead of overflowing, and a session without agent data
  shows "—", not "0 · 0". A Codex rollout counts as running while any process holds its file open,
  not only a process named `codex`. The Sessions area still reads Codex's rollout files while the
  Codex card is off, as long as *Read Claude Code's session files* is on.
- A transcript state written by 1.1.0 is migrated to version 2, keeping its offsets, and written
  back as version 2 on the next save, so no transcript is read or recorded twice; a state of another
  version is an error and that pass reads nothing, instead of being rebuilt.
- A rate-limited request says when seatswap asks again, such as "Claude limits how often seatswap
  may ask; next try at 10:42", instead of "rate limited; retry after Some(0) seconds".
- Ollama's usage answer ends with the running hour (`"partial": true`, ending at the moment of the
  answer); it is read as that hour so far instead of raising a notification about `buckets[24]`.
- Claude Code on an Ollama model: its answers count under the model the session asked for
  (`deepseek-v4.1-flash:cloud`), not Ollama's shorter spelling, so they match the Harness rows, the
  prices and the "other Ollama use" of Activity.
- Statistics → Overview: "Share that expired" says so when no finished weekly window with a price is
  in the range yet, instead of failing with "cannot place the mark".
- Sessions: a flash job without its Claude Code session takes the project it was copied from
  instead of `fm` (the worker copies' folder), and leaves the list once its task ended instead of
  staying as an idle session.
- The `tied` bubble and the count of sessions whose thinking is tied to the account leave out
  models off the Claude plan (Harness, Codex, Ollama): a switch does not touch them.
- The large window uses half the margin around its content (12 px instead of 24 px).

### Removed
- *Use my own rate* (`usdRate`) and the own prices in Settings. With the look-ups off, money is
  shown in US dollars. An older settings file still loads; both keys are ignored silently.
- The tray's *Open seatswap…* entry; *Sessions…* and *Statistics…* open the large window instead.
- The brown dot that marked a model whose thinking is tied to the account; the bubble replaces it.

## [1.1.0] - 2026-11-16

### Added

**Pop-up**
- The pop-up shows the running Claude Code sessions: how many run, how many are busy and how many
  are waiting for you.
- Notifications "usable again", "all accounts are full" and "waiting for you"; with *For sessions
  on: all hosts*, a session waiting on another host notifies here too, and a click opens the
  Sessions area at that session.
- The plan level on every account, such as "Team 5x".

**Large window**
- A large window with four areas, Sessions, Statistics, Status line and Settings, opened from the
  pop-up's gear, `,`, an account's badge or ✎, the tray's *Settings…* and *Open seatswap…*, or
  `seatswap window [AREA]`; every control saves at once.
- The Settings area: every setting, the account editor (abbreviation, colour, label, price) and the
  History and hosts view.
- The Sessions area: every running session on this host and on the user's other hosts, live, with
  its agents, tokens and their subscription and API values, the closed sessions of the last 24
  hours, 7 or 30 days, and the account in use.

**Status line and auto-burn**
- `seatswap statusline`: Claude Code's status line with the account and its live limits, which also
  feeds Claude Code's 5 h and weekly numbers into seatswap's usage cache, so the active account is
  seconds old instead of up to 2 minutes.
- A status line builder in the large window: segments in two zones, options per segment and a
  preview at three widths.
- "Use in Claude Code" and "Restore previous": seatswap writes only the `statusLine` key of Claude
  Code's settings and can put the earlier value back.
- Status line segments for context used, session cost and lines changed, shown when Claude Code
  sends those fields.
- Auto-burn: one switch that moves between accounts by itself, with switch suggestions while it is
  off, a quiet moment for sessions on a model that would lose its thinking, a manual hold, a pause,
  and a per-account "Auto-burn may use it".
- `seatswap burn plan` and `seatswap burn simulate` (`--json` for the plan): the early-switching
  regime, the lazy plan and a replay of recorded work for the lazy and eager policies.
- The pop-up, `burn-next.json` and the status line name the same next account as the Forecast while
  early switching is on; the pop-up's line says `Then AC, before PR is full.` when that plan leaves
  PR before its week fills.
- Auto-burn goes back to the account that produced the running sessions' thinking when two
  accounts are about equally good.
- `usable` in `seatswap status --json` (whether an account can be used right now), `slackHours` in
  every view while known, and `autoBurn`, `next` and `burnError` at the top level.

**History and statistics**
- A history of every usage reading, finished window, switch and agent call, plus the tokens each
  session used per project and hour, in an append-only folder that several hosts can share;
  `seatswap history export --format csv|json` and `seatswap history import <folder>` read and copy
  it.
- The Statistics area: Overview, Weeks with a summary per week, Projects and organizations,
  Subscriptions with "what if", Forecast, Rhythm and Activity.
- Money: a currency, each account's monthly price, API prices from models.dev and the dollar rate
  from Frankfurter, each kept with its date, so past days keep that day's price; one switch,
  *Look up prices and the dollar rate*, turns both look-ups off, and `seatswap prices [--refresh]`
  prints the stored API prices.
- Projects and organizations: names, joins for the same project seen in two places, and
  organizations, in a project log that travels with the history folder.

**Agents**
- Settings for agents ("Agents may read usage", "Agents may switch accounts") and
  `seatswap agent-skill install|uninstall`.

**Other providers**
- Read-only cards for Codex (5 h and weekly limits) and Ollama's cloud plan (credits left, reset
  date, spend in the last 24 h), each off by default, in Settings → Providers.

### Changed
- Settings and the account editor moved from the pop-up to the large window; the gear, `,` and an
  account's badge or ✎ open it there.
- A per-model limit blocks an account only while a session on that model is open.
- A file seatswap rewrites that is a symbolic link stays a link, with its target rewritten; a link
  pointing nowhere is now an error that names the link, instead of being replaced by a file.
- `seatswap status --json`: `suggestion` has a new shape, `{toId, text}` instead of the old
  `{fromId, toId, fromPercent, toPercent, fromResetsAt}`, so the target and the whole sentence are
  both there.

### Removed
- Settings `suggestAt` and `suggestTargetBelow`: the fixed 90 % / 50 % suggestion is replaced by
  auto-burn. An older `accounts.json` with them still loads; the keys are ignored.

- A reset time in Claude's usage reply that seatswap cannot read is no longer dropped without a
  word: the limit is shown without it and the account's card says why.
- Once a settings file uses the new status line segments (context used, session cost, lines
  changed), a seatswap older than 1.1.0 cannot read it: going back below 1.1.0 then means removing
  those segments first.

### Fixed
- `seatswap status --help` names the JSON fields `views`, `active` and `suggestion` as they are; it
  said "the active id", which is not a field.
- With early switching on, the pop-up, `burn-next.json` and the status line could name a switch for
  "now" within 30 minutes of the last switch or during a manual hold or pause, although auto-burn
  waits then; the plan now starts from the last switch and the hold.

## [1.0.6] - 2026-11-14

### Fixed
- The pop-up's keyboard cursor (the blue ring) no longer stays on an account from an earlier
  opening or a click. It shows only after ↑/↓, starting from the account in use, and is gone
  each time the pop-up opens; until then Enter does nothing.

## [1.0.5] - 2026-11-13

### Added
- Clicking an account's gauge in the pop-up switches to that account, like its Switch button.

## [1.0.4] - 2026-11-13

### Changed
- The pop-up shows the account in use as the bottom card; the other accounts stay above it in
  list order.

## [1.0.3] - 2026-11-13

### Added
- The settings view shows seatswap's version at the right of its header.

## [1.0.2] - 2026-11-12

### Changed
- Pop-up gauges: the outer ring now shows the week and the inner ring the 5 h session; the
  week's percent is the large figure in the middle. On plans without a session limit (Ultra) the
  inner ring is left out instead of showing an empty grey track.
- The usage bars beside the gauge follow the same order: week, session, then the per-model
  limits.

## [1.0.1] - 2026-11-11

### Fixed
- No more "no system tray" notification at login when the desktop's tray starts a moment after
  seatswap (Linux Mint: same second). seatswap now waits 30 s for a tray before it notifies.

## [1.0.0] - 2026-11-11

### Added
- Windows support (Windows 10 1803+ and 11): the same login files as on Linux, the single-instance
  socket, start at login through the `Run` registry key, notifications under the name
  "seatswap". Windows gets two programs: `seatswap.exe` for the terminal and `seatswap-tray.exe`,
  the same program without a console window, for autostart and the Start menu.

### Changed
- A switch also takes Claude Code's two newer renewal locks, `.design_oauth_refresh.lock` and
  `.cloud_sessions_oauth_refresh.lock`, so it cannot land in the middle of a renewal of the
  `designOauth` or `cloudSessionsOauth` login.

### Fixed
- "Start at login" with a program path or a `--data-dir` / `--claude-config-dir` folder that is
  not valid Unicode is now an error. Before, the autostart entry was written with the odd
  characters replaced, which would have started seatswap with another folder.
- A switch suggestion or "new login found" notification that timed out or was dismissed opened
  the pop-up, as if it had been clicked. Now only a click on its button or on its body does
  anything.

## [0.1.0] - 2026-11-11

### Added
- First version, for Linux: tray icon with the active account's abbreviation and a usage ring,
  pop-up with every account's session and weekly usage, switch suggestion and notification,
  refresh of inactive logins, end-of-login warnings, settings, start at login.
- Command line: `list`, `use`, `next`, `add`, `edit`, `rm`, `status` (`--json`, `--waybar`,
  `--i3blocks`), `panel`.
- Left click on the tray icon opens the pop-up and the tooltip shows the usage, via the
  StatusNotifierItem backend of `tray-icon`.
- Runs without a system tray (GNOME without the AppIndicator extension, or a desktop panel that
  starts later): it says so and shows the icon as soon as a tray appears; `seatswap panel` opens
  the pop-up meanwhile.

[Unreleased]: https://github.com/ymihere/seatswap/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/ymihere/seatswap/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/ymihere/seatswap/releases/tag/v1.1.0
[1.0.6]: https://github.com/ymihere/seatswap/releases/tag/v1.0.6
[1.0.5]: https://github.com/ymihere/seatswap/releases/tag/v1.0.5
[1.0.4]: https://github.com/ymihere/seatswap/releases/tag/v1.0.4
[1.0.3]: https://github.com/ymihere/seatswap/releases/tag/v1.0.3
[1.0.2]: https://github.com/ymihere/seatswap/releases/tag/v1.0.2
[1.0.1]: https://github.com/ymihere/seatswap/releases/tag/v1.0.1
[1.0.0]: https://github.com/ymihere/seatswap/releases/tag/v1.0.0
[0.1.0]: https://github.com/ymihere/seatswap/releases/tag/v0.1.0
