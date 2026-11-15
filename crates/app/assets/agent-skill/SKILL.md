---
name: seatswap
description: Read the usage limits of the Claude accounts on this machine (5 h window, week, per-model limits, reset times, which account is in use) and, if the owner allows it, switch Claude Code to another account. Use when you need to know how much quota is left, when a limit resets, why requests are being refused for a usage limit, before starting long or unattended work, or when asked to switch account or subscription.
---

# seatswap

`seatswap` is a command on this machine. It holds several Claude accounts and switches Claude Code between them. All accounts share one config folder, so memory, skills and settings stay the same after a switch.

## Read the usage

```sh
seatswap status --json            # from the cache, at most a few minutes old
seatswap status --json --refresh  # asks the server first; use only when you need fresh numbers
```

The answer is one JSON object:

| Field | Meaning |
|---|---|
| `active` | id of the account Claude Code is using now |
| `views[]` | one entry per account: `id`, `abbr`, `label`, `plan`, `isActive`, `loginState`, `usable` |
| `views[].slackHours` | working hours this account can be left alone and still be emptied before its reset; absent while unknown |
| `views[].usage.limits[]` | one entry per limit: `kind` (`session` = the 5 h window, `weekly_all` = the week, `weekly_scoped` = one model's week, named in `model`), `percent` used, `resetsAt` (UTC; null when no window is running), `isActive` (true on the limit that binds now) |
| `autoBurn` | `{on, pausedUntil}`: whether auto-burn switches by itself, and until when it is paused |
| `next` | id of the account auto-burn would switch to, or null |

An account without a `session` entry has no 5 h limit. A limit at 100 % or more is blocked until `resetsAt`.

## Switch the account

Only if the owner has allowed it. If not, the command exits with code 3 and says so; then stop and tell the user, do not look for another way.

```sh
seatswap use PR      # to a named account, by abbreviation or id
```

Rules:

1. If `autoBurn.on` is true, do not switch: auto-burn decides.
2. A switch moves every Claude Code session on the machine, not only yours. They continue on the new account with their following request. Sessions on Fable 5.2, Sonnet 5.5 or Haiku 5.5 then work without their earlier reasoning until someone switches back.
3. Switch only when the active account is blocked or will be before your task ends, and another account is `usable`.
4. After a switch, say in your answer which account is in use now and why you switched.
5. Never run `seatswap add`, `rm` or `edit`, never change seatswap's settings, and never read or write seatswap's data folder or Claude Code's login files yourself.

## When everything is blocked

If no account is `usable`, take the earliest `resetsAt` among the blocking limits, tell the user that time, and stop. Do not retry in a loop.
