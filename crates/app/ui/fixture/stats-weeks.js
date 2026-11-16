// The statistics Weeks page's fixture: `stats_weeks`, `stats_week_summary`
// and `stats_window_detail`.

import { durationText } from "../common.js";
import {
  ACCOUNTS,
  API_PER_M,
  BLOCKED,
  DAY,
  HOUR,
  NOW_MS,
  SW_AUTO,
  accountOut,
  chosenAccounts,
  expiredMoney,
  fixtureMoney,
  fixtureValues,
  fixtureWholePercent,
  resetEnd,
  round1,
  sum,
  switchesOf,
  tokenCounts,
  topProjects,
  usedMoney,
  weekAccounts,
  weekIso,
  weekTime,
} from "./stats-frame.js";

function recentCurve(account) {
  const start = NOW_MS - 14 * DAY;
  const points = new Map();
  for (let index = 0; index <= 12; index += 1) {
    if (account.used[index] === undefined && index < ACCOUNTS[0].used.length) continue;
    if (account.used[index] === null) {
      // A week before the account existed: no window, so no point.
      const end = Date.parse(resetEnd(account, index));
      const windowStart = end - 7 * DAY;
      if (windowStart > NOW_MS || end < start) continue;
      continue;
    }
    const end = Date.parse(resetEnd(account, index));
    const windowStart = end - 7 * DAY;
    if (windowStart > NOW_MS || end < start) continue;
    const running = account.used[index] === undefined;
    const final = running ? account.now : account.used[index];
    const span = running ? NOW_MS - windowStart : 7 * DAY;
    const from = Math.max(windowStart, start);
    const to = Math.min(end, NOW_MS);
    for (let time = from; time <= to; time += 4 * HOUR) {
      const value = Math.min(100, final * ((time - windowStart) / span));
      points.set(time, round1(value));
    }
    if (to === NOW_MS) {
      const value = Math.min(100, final * ((to - windowStart) / span));
      points.set(to, round1(value));
    }
  }
  return [...points.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([time, value]) => [new Date(time).toISOString(), value]);
}

// The windows of one account week, oldest first. One made-up week has a reset:
// LB's 2 Nov week is 62 % then 35 % from 0, 97 % in total. LB's 26 Oct week
// holds a window that used nothing, so the page must not draw a 1 px part for
// it while its total and tooltip still count it.
function partsFor(account, index) {
  const used = account.used[index];
  if (used === null) return [];
  if (account.abbr === "LB" && index === 11) return [62, 35];
  if (account.abbr === "LB" && index === 10) return [94, 0];
  return [used];
}

export function weeks(query) {
  const { indexes, chosen } = chosenAccounts(query);
  return {
    weekStarts: indexes.map(weekIso),
    // The column that holds the fixture's now; its week has no finished window.
    running: indexes.map(
      (index) => weekTime(index) <= NOW_MS && NOW_MS < weekTime(index + 1)
    ),
    rows: chosen.map((account) => ({
      id: account.id,
      plan: account.plan,
      cells: indexes.map((index) => {
        if (account.used[index] === null) return null;
        const used = account.used[index];
        return {
          end: resetEnd(account, index),
          used,
          total: used,
          parts: partsFor(account, index),
          expired: 100 - used,
          expiredMoney: expiredMoney(account, index),
          full: used >= 100,
          uncertain: false,
        };
      }),
      nowPercent: account.now,
    })),
    blocked: indexes.map((index) => BLOCKED[index]),
    recent: chosen.map((account) => ({ id: account.id, points: recentCurve(account) })),
    now: new Date(NOW_MS).toISOString(),
  };
}

export function usedPercentTotal(account, indexes) {
  return sum(indexes.map((index) => account.used[index] ?? 0));
}

export function weekSummary(query, week) {
  const { indexes, chosen } = chosenAccounts(query);
  const index = indexes.find((candidate) => weekIso(candidate) === week);
  if (index === undefined) throw new Error(`week ${JSON.stringify(week)} is not in the chosen range`);
  const accounts = weekAccounts(index, chosen);
  const used = sum(accounts.map((account) => account.used ?? 0));
  const expired = sum(accounts.map((account) => account.expired ?? 0));
  const paid = used + expired;
  const switches = switchesOf([index]);
  const out = sum(chosen.map((account) => accountOut(account, index)));
  const tokens = tokenCounts(out);
  const previous = index - 1;
  let usedChangePercent = null;
  if (previous >= 0 && chosen.length > 0) {
    const before = sum(
      chosen
        .filter((account) => account.used[previous] !== null)
        .map((account) => usedMoney(account, previous))
    );
    if (before > 0) usedChangePercent = ((used - before) / before) * 100;
  }
  const conclusion = [];
  conclusion.push(
    switchesOf([index]) && BLOCKED[index] > 0
      ? `${durationText(Math.round(BLOCKED[index] * 3600))} blocked.`
      : "No hour blocked."
  );
  let closest = null;
  for (const account of chosen) {
    if (account.used[index] === null) continue;
    const expiredPercent = 100 - account.used[index];
    if (closest === null || expiredPercent > closest.expiredPercent) {
      closest = { account, expiredPercent };
    }
  }
  if (closest !== null) {
    if (closest.expiredPercent < 10) {
      conclusion.push(
        `${closest.account.abbr} came closest to expiring unused: ${fixtureWholePercent(
          closest.expiredPercent
        )} left at its reset.`
      );
    } else {
      conclusion.push(
        `${fixtureWholePercent(closest.expiredPercent)} of ${closest.account.abbr}'s week expired, ${fixtureMoney(
          expiredMoney(closest.account, index),
          "EUR"
        )}.`
      );
    }
  }
  if (SW_AUTO[index] > 0) {
    const minutes = index === 11 ? 38 : index === 10 ? 19 : SW_AUTO[index];
    const count = SW_AUTO[index];
    conclusion.push(
      `Auto-burn held back ${count} planned ${count === 1 ? "switch" : "switches"}, ${minutes} ${
        minutes === 1 ? "minute" : "minutes"
      } in total.`
    );
  }
  return {
    week: weekIso(index),
    usedPercentOfPaid: paid > 0 ? (used / paid) * 100 : null,
    used: paid > 0 ? used : null,
    expired: paid > 0 ? expired : null,
    moneyMissing: paid > 0 ? null : "no finished, priced weekly window in this week",
    blockedHours: BLOCKED[index],
    switches,
    switchesTotal:
      switches.user + switches.cli + switches.auto + switches.agent + switches.external,
    perAccountPercent: accounts.map((account) => [account.id, account.usedPercent]),
    expiredPercentOfPaid: paid > 0 ? (expired / paid) * 100 : null,
    topProjects: topProjects(index, used, true),
    tokens,
    values: fixtureValues(out > 0 ? (out / 1000000) * API_PER_M : null, null),
    usedChangePercent,
    conclusion,
  };
}

function windowCurve(account, index, reachedAt) {
  const end = Date.parse(resetEnd(account, index));
  const start = end - 7 * DAY;
  const used = account.used[index];
  const points = [];
  for (let time = start; time <= end; time += 4 * HOUR) {
    let value;
    if (reachedAt !== null && time >= reachedAt) value = 100;
    else value = Math.min(100, used * ((time - start) / (7 * DAY)));
    points.push([new Date(time).toISOString(), round1(value)]);
  }
  if (reachedAt !== null) points.push([new Date(reachedAt).toISOString(), 100]);
  points.sort((a, b) => Date.parse(a[0]) - Date.parse(b[0]));
  return points;
}

// The mock-up's notes for the weeks its detail card shows; every other week
// gets the note its numbers state.
const NOTES = {
  "LB:11": "Second week with auto-burn. 3 % expired at the reset on Monday 17:00.",
  "AC:8": "51 % expired. All work that week went to PR and LB; AC was opened on two days only.",
  "PR:5": "First week of this subscription.",
  "LB:1": "AC was full that week too: 14h with no usable account.",
};

function noteFor(account, index) {
  const note = NOTES[`${account.abbr}:${index}`];
  if (note !== undefined) return note;
  const expiredPercent = 100 - account.used[index];
  return expiredPercent > 0
    ? `${fixtureWholePercent(expiredPercent)} expired at its reset.`
    : "Nothing expired at its reset.";
}

export function windowDetail(query, accountId, end) {
  const { indexes, chosen } = chosenAccounts(query);
  const account = chosen.find((candidate) => candidate.id === accountId);
  if (!account) throw new Error(`unknown account ${JSON.stringify(accountId)}`);
  const index = indexes.find((candidate) => Date.parse(resetEnd(account, candidate)) === Date.parse(end));
  if (index === undefined) throw new Error(`no window of ${account.abbr} ends at ${JSON.stringify(end)}`);
  const used = account.used[index];
  let reached100At = null;
  if (used !== null && used >= 100) {
    reached100At = new Date(weekTime(index) + 4 * DAY + 10 * HOUR).toISOString();
  }
  const switches = switchesOf([index]);
  const windowEnd = Date.parse(resetEnd(account, index));
  // A typical week holds 24 windows; LB's 26 Oct week is the demo of a week
  // that used more windows (12) than its usual working hours hold (8).
  const possibleSessionWindows =
    account.abbr === "PR" ? null : account.abbr === "LB" && index === 10 ? 8 : 24;
  const sessionWindowsUsed =
    possibleSessionWindows === null
      ? null
      : account.abbr === "LB" && index === 10
        ? 12
        : used > 0
          ? Math.round(used / (account.abbr === "AC" ? 8 : 10))
          : 0;
  return {
    id: account.id,
    start: new Date(windowEnd - 7 * DAY).toISOString(),
    end: resetEnd(account, index),
    plan: account.plan,
    weeklyPrice: (account.price * 12) / 52,
    used,
    usedMoney: usedMoney(account, index),
    expired: 100 - used,
    expiredMoney: expiredMoney(account, index),
    reached100At,
    blockedHoursAfter: reached100At !== null ? BLOCKED[index] : 0,
    sessionWindowsUsed,
    possibleSessionWindows,
    usedMoreThanPossible:
      possibleSessionWindows !== null && sessionWindowsUsed > possibleSessionWindows,
    note: noteFor(account, index),
    switchesInOut: switches.user + switches.cli + switches.auto + switches.agent + switches.external,
    switchesAuto: switches.auto,
    lastSampleBeforeEndSecs: 120,
    uncertain: false,
    curve: windowCurve(account, index, reached100At === null ? null : Date.parse(reached100At)),
  };
}
