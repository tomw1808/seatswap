# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- A history of every usage reading, finished window, switch and agent call, plus the tokens each
  session used per project and hour, in an append-only folder that several hosts can share.
- The pop-up shows the running Claude Code sessions: how many run, how many are busy, and how
  many are waiting for you.
- Notifications "usable again", "all accounts are full" and "waiting for you".
- Settings for agents ("Agents may read usage", "Agents may switch accounts") and
  `seatswap agent-skill install|uninstall`.
- `usable` in `seatswap status --json`: whether an account can be used right now.
- `seatswap statusline`: Claude Code's status line, with the account and its live limits.
- Live numbers from the status line: the command feeds Claude Code's 5 h and weekly numbers into
  seatswap's usage cache, so the active account is seconds old instead of up to 2 minutes.
- Auto-burn: one switch that lets seatswap move between accounts by itself, with switch
  suggestions while it is off, a quiet moment for sessions on a model that would lose its
  thinking, a manual hold, a pause, and a per-account "Auto-burn may use this account".
- `seatswap burn plan` and `seatswap burn simulate` (`--json` for the plan): the early-switching
  regime, the lazy plan, and a replay of recorded work for the lazy and eager policies.
- `slackHours` in every `status --json` view while known, and `autoBurn` and `next` at the top
  level.
- A large window with the Settings area: every setting, the account editor (abbreviation, colour,
  label, price, auto-burn) and the History and hosts view, with each control saving at once.
- Money: a currency, each account's monthly price, API prices from models.dev and the dollar rate
  from Frankfurter, both kept with their dates; *Look up prices and the dollar rate* switches both
  look-ups off.
- Projects and organizations: names, joins for the same project seen in two places, and
  organizations, kept in a project log that travels with the history folder.
- The plan level on every account, such as "Team 5x".
- `seatswap window [AREA]` for the large window, `seatswap prices [--refresh]` for the stored API
  prices, and `seatswap history export --format csv|json` / `seatswap history import <folder>`.

### Changed
- Settings and the account editor moved from the pop-up to the large window; the gear, `,` and an
  account's badge or ✎ open it there.
- A per-model limit blocks an account only while a session on that model is open.
- `seatswap status --json`: `suggestion` has a new shape, `{toId, text}` instead of the old
  `{fromId, toId, fromPercent, toPercent, fromResetsAt}`, so the target and the whole sentence are
  both there.

### Removed
- Settings `suggestAt` and `suggestTargetBelow`: the fixed 90 % / 50 % suggestion is replaced by
  auto-burn. An older `accounts.json` with them still loads; the keys are ignored.

### Fixed
- `seatswap status --help` names the fields of the JSON output as they are: `views`, `active`
  and `suggestion`. It said "the active id", which is not a field.

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

[Unreleased]: https://github.com/ymihere/seatswap/compare/v1.0.6...HEAD
[1.0.6]: https://github.com/ymihere/seatswap/releases/tag/v1.0.6
[1.0.5]: https://github.com/ymihere/seatswap/releases/tag/v1.0.5
[1.0.4]: https://github.com/ymihere/seatswap/releases/tag/v1.0.4
[1.0.3]: https://github.com/ymihere/seatswap/releases/tag/v1.0.3
[1.0.2]: https://github.com/ymihere/seatswap/releases/tag/v1.0.2
[1.0.1]: https://github.com/ymihere/seatswap/releases/tag/v1.0.1
[1.0.0]: https://github.com/ymihere/seatswap/releases/tag/v1.0.0
[0.1.0]: https://github.com/ymihere/seatswap/releases/tag/v0.1.0
