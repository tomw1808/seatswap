// The statistics fixture (step-c3 Task 8): the mock-up's made-up numbers from
// specs/page-build/data.py in exactly the JSON the Rust structs serialise to.
// The page may not add a number; this fixture may, and does: every answer is
// built from the weeks the chosen range covers and the chosen accounts.

import { ensureFixture } from "./fixture.js";

const DAY = 86400000;
const HOUR = 3600000;

// The snapshot's view order; the app's Rust commands walk Ctx::accounts, so
// the pages see this order too.
const VIEW_ABBRS = ["PR", "LB", "AC"];

const WEEKS = [
  "2026-08-17",
  "2026-08-24",
  "2026-08-31",
  "2026-09-07",
  "2026-09-14",
  "2026-09-21",
  "2026-09-28",
  "2026-10-05",
  "2026-10-12",
  "2026-10-19",
  "2026-10-26",
  "2026-11-02",
];

// data.py's ACC, in the mock-up's ORDER (LB, AC, PR). The id is the built-in
// snapshot's view id for the abbreviation, filled in before an answer is built.
const ACCOUNTS = [
  {
    id: null,
    abbr: "LB",
    label: "Lab",
    plan: "Max",
    price: 180,
    used: [100, 100, 96, 100, 100, 88, 74, 81, 69, 92, 94, 97],
    now: 40,
    resetDay: 7,
    resetHour: 17,
  },
  {
    id: null,
    abbr: "AC",
    label: "Acme",
    plan: "Team 5x",
    price: 140,
    used: [91, 100, 100, 97, 100, 72, 58, 64, 49, 71, 91, 93],
    now: 48,
    resetDay: 7,
    resetHour: 2,
  },
  {
    id: null,
    abbr: "PR",
    label: "Private",
    plan: "Ultra",
    price: 360,
    used: [null, null, null, null, null, 61, 83, 77, 90, 68, 88, 90],
    now: 70,
    resetDay: 5,
    resetHour: 19,
  },
];

// Each answer names the built-in snapshot's accounts: their ids come from its
// views by abbreviation, never from ids of the fixture's own.
function resolveIds() {
  const fixture = ensureFixture();
  if (!fixture || !Array.isArray(fixture.views)) {
    throw new Error("the built-in snapshot has no account views");
  }
  for (const account of ACCOUNTS) {
    const view = fixture.views.find(
      (candidate) => candidate && candidate.abbr === account.abbr
    );
    if (!view || typeof view.id !== "string" || view.id === "") {
      throw new Error(`the built-in snapshot has no account ${JSON.stringify(account.abbr)}`);
    }
    account.id = view.id;
  }
}

function viewOrder() {
  return VIEW_ABBRS.map((abbr) => {
    const account = byAbbr(abbr);
    if (!account) throw new Error(`unknown account abbreviation ${JSON.stringify(abbr)}`);
    return account.id;
  });
}

const BLOCKED = [11, 14, 8, 12, 9, 1, 0, 0.5, 0, 1, 0.5, 0];
const SW_YOU = [9, 11, 8, 10, 9, 6, 5, 6, 4, 6, 2, 1];
const SW_AUTO = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 19, 22];
const SW_AGENT = [0, 0, 0, 0, 0, 0, 1, 0, 2, 0, 0, 0];

// Output tokens (millions) of all accounts together, data.py's 68.9 in week 11
// and 44.8 in week 8.
const OUT_M = [58.0, 61.5, 55.2, 66.4, 64.1, 70.8, 72.6, 79.4, 44.8, 88.0, 110.3, 68.9];
const TOK_TOTAL = 840.0;
const API_TOTAL = 17100;
const API_PER_M = API_TOTAL / TOK_TOTAL;
const NOW_MS = Date.parse("2026-11-12T10:00:00Z");

// The mock-up's tokens per percent of a finished account week (data.py: LB 1.9 M,
// AC 1.4 M, PR 3.8 M).
const TOKENS_PER_PERCENT = { LB: 1900000, AC: 1400000, PR: 3800000 };

const ORG_ACC = {
  acme: { LB: 70, AC: 258, PR: 141 },
  sam: { LB: 369, AC: 55, PR: 304 },
  Unassigned: { LB: 14, AC: 6, PR: 17 },
};

const TREE = [
  { level: 0, name: "acme", share: 38, split: [15, 55, 30], sessions: 253, key: null },
  { level: 1, name: "acme-kb", share: 31, split: [14, 58, 28], sessions: 212, key: ["repo", "github.com/acme/acme-kb"] },
  { level: 2, name: "ingest", share: 19, split: [14, 58, 28], sessions: 131, key: null },
  { level: 2, name: "web", share: 9, split: [14, 58, 28], sessions: 64, key: null },
  { level: 2, name: "(top folder)", share: 3, split: [14, 58, 28], sessions: 17, key: null },
  { level: 1, name: "acme-notes", share: 7, split: [19, 42, 39], sessions: 41, key: ["repo", "github.com/acme/acme-notes"] },
  { level: 0, name: "sam", share: 59, split: [51, 7, 42], sessions: 428, key: null },
  { level: 1, name: "atlas", share: 34, split: [55, 5, 40], sessions: 268, key: ["repo", "github.com/sam/atlas"] },
  { level: 2, name: "engine", share: 21, split: [55, 5, 40], sessions: 160, key: null },
  { level: 2, name: "frontend", share: 11, split: [55, 5, 40], sessions: 91, key: null },
  { level: 2, name: "(top folder)", share: 2, split: [55, 5, 40], sessions: 17, key: null },
  { level: 1, name: "ledger", share: 17, split: [44, 10, 46], sessions: 97, key: ["repo", "github.com/sam/ledger"] },
  { level: 1, name: "notes", share: 8, split: [48, 10, 42], sessions: 63, key: ["repo", "github.com/sam/notes"] },
  { level: 0, name: "Unassigned", share: 3, split: [38, 16, 46], sessions: 22, key: null },
  { level: 1, name: "~ (home folder)", share: 3, split: [38, 16, 46], sessions: 22, key: ["folder", "~"] },
];

const ORG_ACCOUNTS = { acme: ["AC"], sam: ["LB", "PR"] };

function accountById(id) {
  return ACCOUNTS.find((account) => account.id === id) || null;
}

function byAbbr(abbr) {
  return ACCOUNTS.find((account) => account.abbr === abbr) || null;
}

function sum(values) {
  let total = 0;
  for (const value of values) {
    if (typeof value === "number" && Number.isFinite(value)) total += value;
  }
  return total;
}

function round1(value) {
  return Math.round(value * 10) / 10;
}

function fixtureMoney(value, currency) {
  const rounded = Math.round(value * 100) / 100;
  if (rounded < 10) return `${rounded.toFixed(2)} ${currency}`;
  return `${Math.round(rounded).toLocaleString("en-US")} ${currency}`;
}

function fixtureWholePercent(value) {
  return `${Math.round(value)} %`;
}

function fixtureHours(value) {
  const rounded = round1(value);
  return Number.isInteger(rounded) ? `${rounded} h` : `${rounded.toFixed(1)} h`;
}

function joinNames(names) {
  if (names.length === 0) return "";
  if (names.length === 1) return names[0];
  if (names.length === 2) return `${names[0]} and ${names[1]}`;
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function weekIso(index) {
  return `${WEEKS[index]}T00:00:00.000Z`;
}

function weekTime(index) {
  if (index < WEEKS.length) return Date.parse(weekIso(index));
  // The running week after the last finished one.
  return weekTime(WEEKS.length - 1) + (index - (WEEKS.length - 1)) * 7 * DAY;
}

function weeklyPrice(account) {
  return (account.price * 12) / 52;
}

function usedMoney(account, index) {
  const used = account.used[index];
  if (used === null) return 0;
  return (used / 100) * weeklyPrice(account);
}

function expiredMoney(account, index) {
  const used = account.used[index];
  if (used === null) return 0;
  return ((100 - used) / 100) * weeklyPrice(account);
}

function resetEnd(account, index) {
  return new Date(
    weekTime(index) + account.resetDay * DAY + account.resetHour * HOUR
  ).toISOString();
}

function rangeIndexes(range) {
  switch (range) {
    case "thisWeek":
      return [11];
    case "lastWeek":
      return [10];
    case "days30":
      return [7, 8, 9, 10, 11];
    case "days90":
    case "months12":
    case "all":
      return WEEKS.map((_, index) => index);
    default:
      throw new Error(`unknown range ${JSON.stringify(range)}`);
  }
}

function chosenAccounts(query) {
  if (!query || typeof query !== "object") throw new Error("a statistics query is missing");
  const indexes = rangeIndexes(query.range);
  if (!Array.isArray(query.accounts)) throw new Error("query.accounts must be a list");
  const ids = query.accounts.length === 0 ? viewOrder() : query.accounts;
  const chosen = [];
  for (const id of ids) {
    if (!accountById(id)) throw new Error(`unknown account ${JSON.stringify(id)}`);
    if (!chosen.includes(id)) chosen.push(id);
  }
  return { indexes, chosen: chosen.map(accountById) };
}

function totalUsedMoney(index) {
  return sum(
    ACCOUNTS.filter((account) => account.used[index] !== null).map((account) =>
      usedMoney(account, index)
    )
  );
}

function accountOut(account, index) {
  const used = account.used[index];
  if (used === null) return 0;
  const total = totalUsedMoney(index);
  if (total <= 0) return 0;
  return OUT_M[index] * 1000000 * (usedMoney(account, index) / total);
}

function tokenCounts(out) {
  return {
    input: Math.round(out * 0.15),
    out: Math.round(out),
    cw: Math.round(out * 0.2),
    cr: Math.round(out * 3.2),
  };
}

function switchesOf(indexes) {
  return {
    user: Math.round(sum(indexes.map((index) => SW_YOU[index]))),
    cli: 0,
    auto: Math.round(sum(indexes.map((index) => SW_AUTO[index]))),
    agent: Math.round(sum(indexes.map((index) => SW_AGENT[index]))),
    external: 0,
  };
}

function projectShares(index) {
  const atlas = 30 + ((index * 7) % 11);
  const acmeKb = index === 8 ? 21 : 34 - ((index * 5) % 9);
  const ledger = 13 + ((index * 3) % 8);
  const other = 100 - atlas - acmeKb - ledger;
  return [
    ["atlas", atlas],
    ["acme-kb", acmeKb],
    ["ledger", ledger],
    ["other", other],
  ];
}

function topProjects(index, usedTotal, asPercent) {
  const shares = projectShares(index);
  const named = shares.slice(0, 3);
  named.sort((a, b) => b[1] - a[1]);
  const other = shares[3][1];
  const rows = named.map(([name, share]) => [
    name,
    asPercent ? share : (share / 100) * usedTotal,
  ]);
  rows.push(["other", asPercent ? other : (other / 100) * usedTotal]);
  return rows;
}

function weekAccounts(index, chosen) {
  return chosen
    .filter((account) => account.used[index] !== null)
    .map((account) => ({
      id: account.id,
      usedPercent: account.used[index],
      used: usedMoney(account, index),
      expiredPercent: 100 - account.used[index],
      expired: expiredMoney(account, index),
      uncertain: false,
    }));
}

function expiredByAccount(indexes, chosen) {
  const rows = chosen
    .map((account) => [
      account.id,
      sum(indexes.map((index) => expiredMoney(account, index))),
    ])
    .filter(([, value]) => value > 0);
  rows.sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  return rows;
}

function marksOf(indexes) {
  const first = weekTime(indexes[0]);
  const end = weekTime(indexes[indexes.length - 1]) + 7 * DAY;
  return [
    { at: "2026-09-21T00:00:00.000Z", label: "PR added" },
    { at: "2026-10-26T00:00:00.000Z", label: "auto-burn on" },
  ].filter((mark) => {
    const at = Date.parse(mark.at);
    return at >= first && at < end;
  });
}

function findingExpired(indexes, chosen, currency) {
  const rows = expiredByAccount(indexes, chosen);
  if (rows.length === 0) return null;
  const [id, amount] = rows[0];
  if (amount <= 0) return null;
  const account = accountById(id);
  if (!account) return null;
  const weeks = indexes.filter((index) => account.used[index] !== null).length;
  const noun = weeks === 1 ? "week" : "weeks";
  return `${account.abbr} is where most money expired: ${fixtureMoney(amount, currency)} in ${weeks} ${noun}.`;
}

function weekMoney(week, chosen) {
  let used = 0;
  let expired = 0;
  let paid = 0;
  for (const account of chosen) {
    if (account.used[week] === null) continue;
    used += usedMoney(account, week);
    expired += expiredMoney(account, week);
    paid += weeklyPrice(account);
  }
  return { used, expired, paid };
}

function findingBlocked(indexes, blockedHours, avoidableHours) {
  if (blockedHours <= 0 || indexes.length === 0) return null;
  const noun = indexes.length === 1 ? "week" : "weeks";
  return `You were blocked ${fixtureHours(blockedHours)} in ${indexes.length} ${noun}; ${fixtureHours(avoidableHours)} of them while other quota expired.`;
}

function findingAutoBurn(indexes, chosen) {
  const at = Date.parse("2026-10-26T00:00:00.000Z");
  const first = weekTime(indexes[0]);
  const end = weekTime(indexes[indexes.length - 1]) + 7 * DAY;
  if (at < first || at >= end) return null;
  const before = indexes.filter((index) => weekTime(index) + 7 * DAY <= at);
  const after = indexes.filter((index) => weekTime(index) >= at);
  if (before.length === 0 || after.length === 0) return null;
  let paidBefore = 0;
  let expiredBefore = 0;
  for (const index of before) {
    const money = weekMoney(index, chosen);
    paidBefore += money.paid;
    expiredBefore += money.expired;
  }
  let paidAfter = 0;
  let expiredAfter = 0;
  for (const index of after) {
    const money = weekMoney(index, chosen);
    paidAfter += money.paid;
    expiredAfter += money.expired;
  }
  if (paidBefore <= 0 || paidAfter <= 0) return null;
  const blockedBefore = sum(before.map((index) => BLOCKED[index])) / before.length;
  const blockedAfter = sum(after.map((index) => BLOCKED[index])) / after.length;
  const date = new Date(at).toLocaleDateString("en-GB", { day: "numeric", month: "short" });
  return `Since auto-burn (${date}): ${fixtureHours(blockedAfter)} blocked a week, ${fixtureWholePercent(
    (expiredAfter / paidAfter) * 100
  )} expired. Before: ${fixtureHours(blockedBefore)}, ${fixtureWholePercent(
    (expiredBefore / paidBefore) * 100
  )}.`;
}

function findingFull(indexes, chosen) {
  let best = null;
  for (const account of chosen) {
    const weeks = indexes.filter((index) => account.used[index] !== null).length;
    if (weeks === 0) continue;
    const full = indexes.filter(
      (index) => account.used[index] !== null && account.used[index] >= 100
    ).length;
    if (full === 0) continue;
    if (best === null || full > best.full) best = { account, full, weeks };
  }
  if (best === null) return null;
  const noun = best.weeks === 1 ? "week" : "weeks";
  return `${best.account.abbr} was full in ${best.full} of ${best.weeks} ${noun}.`;
}

function overview(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const currency = "EUR";
  const weeks = indexes.map((index) => {
    const accounts = weekAccounts(index, chosen);
    const used = sum(accounts.map((account) => account.used ?? 0));
    const paid = sum(accounts.map((account) => (account.used ?? 0) + (account.expired ?? 0)));
    const expired = sum(accounts.map((account) => account.expired ?? 0));
    return {
      week: weekIso(index),
      accounts,
      projects: topProjects(index, used, false),
      blockedHours: BLOCKED[index],
      expiredShare: paid > 0 ? (expired / paid) * 100 : null,
    };
  });
  const used = sum(weeks.flatMap((week) => week.accounts.map((account) => account.used ?? 0)));
  const expired = sum(weeks.flatMap((week) => week.accounts.map((account) => account.expired ?? 0)));
  const paid = used + expired;
  const blockedHours = sum(indexes.map((index) => BLOCKED[index]));
  const avoidableHours = blockedHours > 0 ? (blockedHours / 57) * 6 : 0;
  const findings = [
    findingExpired(indexes, chosen, currency),
    findingBlocked(indexes, blockedHours, avoidableHours),
    findingAutoBurn(indexes, chosen),
    findingFull(indexes, chosen),
  ].filter((text) => text !== null);
  return {
    currency,
    withoutPrice: [],
    kpis: {
      used,
      paid,
      expired,
      expiredByAccount: expiredByAccount(indexes, chosen),
      blockedHours,
      avoidableHours,
      switches: switchesOf(indexes),
      switchCostPercentOfWindow: 0.8,
    },
    weeks,
    marks: marksOf(indexes),
    findings,
  };
}

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

function weeks(query) {
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
  };
}

function usedPercentTotal(account, indexes) {
  return sum(indexes.map((index) => account.used[index] ?? 0));
}

function weekSummary(query, week) {
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
  conclusion.push(switchesOf([index]) && BLOCKED[index] > 0 ? `${fixtureHours(BLOCKED[index])} blocked.` : "No hour blocked.");
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
    perAccount: accounts.map((account) => [account.id, account.used ?? 0]),
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

function windowDetail(query, accountId, end) {
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
  return {
    id: account.id,
    end: resetEnd(account, index),
    used,
    usedMoney: usedMoney(account, index),
    expired: 100 - used,
    expiredMoney: expiredMoney(account, index),
    reached100At,
    blockedHoursAfter: reached100At !== null ? BLOCKED[index] : 0,
    sessionWindowsUsed: used > 0 ? Math.round(used / 10) : 0,
    switchesInOut: switches.user + switches.cli + switches.auto + switches.agent + switches.external,
    switchesAuto: switches.auto,
    lastSampleBeforeEndSecs: 120,
    uncertain: false,
    curve: windowCurve(account, index, reached100At === null ? null : Date.parse(reached100At)),
  };
}

function keyOf(entry) {
  if (entry.key === null) return null;
  return { kind: entry.key[0], id: entry.key[1] };
}

function sameKey(first, second) {
  return Boolean(
    first &&
      second &&
      first.kind === second.kind &&
      first.id === second.id
  );
}

function chosenSplit(entry, chosen) {
  const weights = chosen.map((account) => {
    const index = ACCOUNTS.indexOf(account);
    return { account, weight: entry.split[index] };
  });
  const total = sum(weights.map((row) => row.weight));
  return weights
    .map((row) => [row.account.id, total > 0 ? (row.weight / total) * 100 : 0])
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

function accountUsedTotal(account, indexes) {
  return sum(indexes.map((index) => usedMoney(account, index)));
}

function accountOrgTotal(abbr) {
  return sum(Object.values(ORG_ACC).map((org) => org[abbr] ?? 0));
}

function matrixCell(org, account, indexes) {
  const values = ORG_ACC[org];
  if (!values) return 0;
  const total = accountOrgTotal(account.abbr);
  if (total <= 0) return 0;
  return accountUsedTotal(account, indexes) * ((values[account.abbr] ?? 0) / total);
}

function matrixSentences(orgs, chosen, cells, indexes) {
  const sentences = [];
  for (const [orgIndex, org] of orgs.entries()) {
    const ownAbbrs = ORG_ACCOUNTS[org] || [];
    const own = chosen.filter((account) => ownAbbrs.includes(account.abbr));
    if (own.length === 0) continue;
    const total = sum(cells[orgIndex]);
    const ownSub = sum(own.map((account) => matrixCell(org, account, indexes)));
    if (total <= 0 || ownSub <= 0) continue;
    const names = joinNames(own.map((account) => account.abbr));
    const noun = own.length === 1 ? "subscription" : "subscriptions";
    const verb = own.length === 1 ? "is" : "are";
    const share = (ownSub / total) * 100;
    let sentence = `${names} ${verb} ${org}'s ${noun}. It covered ${fixtureWholePercent(
      share
    )} of the work for ${org}.`;
    if (share < 100) {
      const others = chosen.filter(
        (account) => !own.includes(account) && matrixCell(org, account, indexes) > 0
      );
      const amount = sum(others.map((account) => matrixCell(org, account, indexes)));
      if (others.length > 0 && amount > 0) {
        sentence += ` The rest ran on ${joinNames(
          others.map((account) => account.abbr)
        )}: ${fixtureMoney(amount, "EUR")} of their value.`;
      }
    }
    sentences.push(sentence);

    let carried = 0;
    const otherOrgs = [];
    for (const [otherIndex, other] of orgs.entries()) {
      if (other === org) continue;
      const value = sum(own.map((account) => matrixCell(other, account, indexes)));
      if (value > 0) {
        carried += value;
        otherOrgs.push(other);
      }
    }
    if (carried > 0 && otherOrgs.length > 0) {
      sentences.push(
        `The other way round: ${names} carried ${fixtureMoney(
          carried,
          "EUR"
        )} of work for ${joinNames(otherOrgs)}.`
      );
    }
  }
  return sentences;
}

function projects(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const keptFraction = indexes.length / WEEKS.length;
  const usedTotal = sum(chosen.map((account) => accountUsedTotal(account, indexes)));
  const outTotal = sum(chosen.map((account) => sum(indexes.map((index) => accountOut(account, index)))));
  const apiTotal = (outTotal / 1000000) * API_PER_M;
  const nodes = TREE.map((entry) => {
    const out = (entry.share / 100) * outTotal;
    const orgAccounts = (ORG_ACCOUNTS[entry.name] || [])
      .map((abbr) => byAbbr(abbr))
      .filter(Boolean)
      .map((account) => account.id);
    return {
      kind: entry.level === 0 ? "org" : entry.level === 1 ? "project" : "folder",
      level: entry.level,
      name: entry.name,
      key: keyOf(entry),
      orgAccounts,
      tokens: tokenCounts(out),
      share: entry.share,
      values: fixtureValues((entry.share / 100) * apiTotal, (entry.share / 100) * usedTotal),
      byAccount: chosenSplit(entry, chosen),
      sessions: Math.round(entry.sessions * keptFraction),
    };
  });
  const orgs = TREE.filter((entry) => entry.level === 0).map((entry) => entry.name);
  const cells = orgs.map((org) => chosen.map((account) => matrixCell(org, account, indexes)));
  return {
    nodes,
    matrix: {
      orgs,
      accounts: chosen.map((account) => account.id),
      cells,
      sentences: matrixSentences(orgs, chosen, cells, indexes),
    },
    tokensPerPercent: chosen.flatMap((account) => {
      const percent = usedPercentTotal(account, indexes);
      if (percent <= 0) return [];
      const perPercent = TOKENS_PER_PERCENT[account.abbr];
      return perPercent === undefined ? [] : [[account.id, perPercent]];
    }),
    unknownAccountTokens: { input: 0, out: 0, cw: 0, cr: 0 },
    withoutPrice: [],
    notes: [
      "Not in any project: use of these accounts without a session file on a host with seatswap (claude.ai in the browser, the phone).",
    ],
  };
}

function projectDetail(query, key) {
  const { indexes, chosen } = chosenAccounts(query);
  const entry = TREE.find((candidate) => sameKey(keyOf(candidate), key));
  if (!entry) throw new Error(`unknown project ${JSON.stringify(key)}`);
  const keptOutTotal = sum(
    chosen.map((account) => sum(indexes.map((index) => accountOut(account, index))))
  );
  const out = (entry.share / 100) * keptOutTotal;
  const outWeights = indexes.map((index) => OUT_M[index]);
  const weightTotal = sum(outWeights);
  const modelShares = [
    ["claude-opus-5-5", 0.64],
    ["claude-sonnet-5-5", 0.24],
    ["claude-fable-5-2", 0.12],
  ];
  const weeklyByModel = indexes.map((index, position) => {
    const weekShare = weightTotal > 0 ? outWeights[position] / weightTotal : 0;
    const weekOut = out * weekShare;
    return [
      weekIso(index),
      modelShares.map(([model, share]) => [model, Math.round(weekOut * share)]),
    ];
  });
  const hosts =
    entry.name === "atlas"
      ? [["atlas", 71], ["vega", 29]]
      : entry.name === "ledger"
        ? [["atlas", 38], ["vega", 62]]
        : [["atlas", 100]];
  return {
    name: entry.name,
    tokens: tokenCounts(out),
    agentShare: entry.name === "ledger" ? 44 : 58,
    hosts,
    values: fixtureValues(
      (entry.share / 100) * ((keptOutTotal / 1000000) * API_PER_M),
      (entry.share / 100) * sum(chosen.map((account) => accountUsedTotal(account, indexes)))
    ),
    weeklyByModel,
    sessions: Math.round(entry.sessions * (indexes.length / WEEKS.length)),
  };
}

// ---------------------------------------------------------------------------
// Subscriptions (spec §3.3; step-c3 Task 8, part 3)
// ---------------------------------------------------------------------------

// The mock-up's blocked hours per account over its twelve weeks.
const BLOCKED_BY_ACCOUNT = { LB: 31, AC: 19, PR: 7 };

// The mock-up's "what if without it" numbers (its weekly series sums to its
// blocked hours): LB 57 → 141 h, AC 57 → 96 h, PR 57 → 111 h.
const WHAT_IF = {
  LB: {
    blocked: 141,
    expired: 121,
    saved: 498,
    sentence: "Without LB you would have been blocked 84 h more. Keep it.",
    weekly: [22, 26, 19, 24, 21, 6, 4, 5, 3, 6, 3, 2],
  },
  AC: {
    blocked: 96,
    expired: 164,
    saved: 388,
    sentence:
      "Before PR was added AC saved you 31 h. In the 7 weeks since, dropping it would have cost 8 h.",
    weekly: [17, 21, 14, 18, 15, 3, 1, 1.5, 1, 2.5, 1, 1],
  },
  PR: {
    blocked: 111,
    expired: 114,
    saved: 582,
    sentence: "Without PR the blocked hours of August come back: 54 h more in 7 weeks.",
    weekly: [11, 14, 8, 12, 9, 9, 7, 8, 6, 9, 9, 9],
  },
};

function round2(value) {
  return Math.round(value * 100) / 100;
}

function allUsedMoney() {
  let total = 0;
  for (const account of ACCOUNTS) {
    for (let index = 0; index < WEEKS.length; index += 1) {
      total += usedMoney(account, index);
    }
  }
  return total;
}

function verdictFor(weeks, average, fullWeeks) {
  if (weeks < 4) return "tooEarly";
  if (average !== null && average < 40) return "mostlyUnused";
  if (fullWeeks * 2 >= weeks) return "regularlyFull";
  return "wellUsed";
}

function subscriptions(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const usedTotal = allUsedMoney();
  const blockedTotal = sum(indexes.map((index) => BLOCKED[index]));
  const rows = chosen.map((account) => {
    const windows = indexes.filter((index) => account.used[index] !== null);
    const weeks = windows.length;
    const average = weeks > 0 ? sum(windows.map((index) => account.used[index])) / weeks : null;
    const used = sum(windows.map((index) => usedMoney(account, index)));
    const apiValue = weeks > 0 && usedTotal > 0 ? (used / usedTotal) * API_TOTAL : null;
    const paid = weeklyPrice(account) * weeks;
    const fullWeeks = windows.filter((index) => account.used[index] >= 100).length;
    const first = account.used.findIndex((value) => value !== null);
    return {
      id: account.id,
      plan: account.plan,
      price: account.price,
      since: weekIso(first),
      weeks,
      avgUsed: average === null ? null : round1(average),
      expiredPercent: average === null ? null : round1(100 - average),
      expired: weeks > 0 ? round2(sum(windows.map((index) => expiredMoney(account, index)))) : null,
      fullWeeks,
      blockedHoursOnIt: round1(
        BLOCKED_BY_ACCOUNT[account.abbr] * (sum(BLOCKED) > 0 ? blockedTotal / sum(BLOCKED) : 0)
      ),
      values: fixtureValues(apiValue, null),
      timesPaid: apiValue !== null && paid > 0 ? apiValue / paid : null,
      timesPaidMissing:
        apiValue !== null && paid > 0
          ? null
          : weeks === 0
            ? "no finished weekly window in the range"
            : apiValue === null
              ? "no API value in this answer"
              : "the paid money of its finished weeks is zero",
      verdict: verdictFor(weeks, average, fullWeeks),
      tokensPerPercent: windows.map((index) => [weekIso(index), TOKENS_PER_PERCENT[account.abbr]]),
    };
  });
  const periods = chosen.map((account) => {
    const first = account.used.findIndex((value) => value !== null);
    return {
      id: account.id,
      plan: account.plan,
      price: account.price,
      from: weekIso(first),
      to: null,
    };
  });
  return { periods, rows, withoutPrice: [] };
}

function whatIf(query, without) {
  if (typeof without !== "string" || without === "") {
    throw new Error("stats_what_if needs the id of an account");
  }
  const { indexes, chosen } = chosenAccounts(query);
  const account = accountById(without);
  if (!account) throw new Error(`unknown account ${JSON.stringify(without)}`);
  const mock = WHAT_IF[account.abbr];
  const windows = indexes.filter((index) => account.used[index] !== null);
  const allWindows = account.used.filter((value) => value !== null).length;
  const blockedHours = round1(sum(indexes.map((index) => mock.weekly[index])));
  const blockedHoursWas = round1(sum(indexes.map((index) => BLOCKED[index])));
  const expiredWas = round2(
    sum(
      indexes.flatMap((index) =>
        chosen
          .filter((candidate) => candidate.used[index] !== null)
          .map((candidate) => expiredMoney(candidate, index))
      )
    )
  );
  return {
    without: account.id,
    blockedHours: windows.length > 0 ? blockedHours : null,
    blockedHoursWas,
    expired: windows.length > 0 ? round2(mock.expired * (windows.length / allWindows)) : null,
    expiredWas,
    savedPrice: windows.length > 0 ? round2(weeklyPrice(account) * windows.length) : null,
    sentence: mock.sentence,
    weekly: indexes.map((index) => [weekIso(index), BLOCKED[index], mock.weekly[index]]),
  };
}

// ---------------------------------------------------------------------------
// Rhythm (spec §3.5; data.py's heat())
// ---------------------------------------------------------------------------

const ACT_PATTERN = [
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[9, 13], [15, 21]],
  [[9, 13], [15, 21]],
];

const BLOCKED_PER_DAY = [0.4, 0.3, 0.6, 0.9, 1.1, 0.2, 1.3];

function rhythm(query) {
  chosenAccounts(query);
  chosenHosts(query);
  const heat = [];
  for (let day = 0; day < 7; day += 1) {
    for (let hour = 0; hour < 24; hour += 1) {
      const base = ACT_PATTERN[day].some(
        ([from, to]) => from <= hour + 0.5 && hour + 0.5 < to
      )
        ? 1
        : 0;
      const n = ((day * 37 + hour * 17 + day * hour * 5) % 11) / 10;
      let value = base * (0.55 + 0.45 * n);
      if (base === 0 && [0, 1, 2, 7, 13, 19].includes(hour)) {
        value = 0.12 * n + ((day + hour) % 4 === 0 ? 0.25 : 0);
      }
      if (day >= 5) value *= 0.75;
      value = Math.max(0, Math.min(1, value));
      // The mock-up's share of an hour, in output tokens (0 .. 1.2 M).
      heat.push(Math.round(value * 1200000));
    }
  }
  const perDay = ACT_PATTERN.map((ranges, day) => ({
    weekday: day,
    workingHours: round1(sum(ranges.map(([from, to]) => to - from))),
    blockedHours: BLOCKED_PER_DAY[day],
  }));
  return { heat, perDay };
}

// ---------------------------------------------------------------------------
// Activity (spec §3.9; mock_activity.py)
// ---------------------------------------------------------------------------

const ACT_START = Date.parse("2026-08-17T00:00:00.000Z");
const ACT_NOW = Date.parse("2026-11-12T10:00:00.000Z");
const ACT_NOW_HOUR = (ACT_NOW - ACT_START) / HOUR;
const ACT_FIRST = ACT_NOW_HOUR - 168;
const ACT_DAYS_START = Date.parse("2026-10-30T00:00:00.000Z");
// mock_activity.py's random stream starts at its own 30 October; the fixture
// starts the history earlier so its weeks have data, and keeps the stream.
const ACT_RANDOM_SHIFT = (ACT_DAYS_START - ACT_START) / HOUR;
const ACT_PROJECT_OF = ["atlas", "atlas", "acme-kb", "ledger", "atlas", "acme-kb", "notes"];
const ACT_FOLDER_OF = {
  atlas: "engine",
  "acme-kb": "ingest",
  ledger: "ledger",
  notes: "notes",
};

function actRandom(hour, salt) {
  const step = hour - ACT_RANDOM_SHIFT;
  return ((step * 37 + salt * 101 + Math.floor(step / 7) * 13) % 97) / 96;
}

function actHostHour(host, hour) {
  const time = new Date(ACT_START + hour * HOUR);
  const day = (time.getUTCDay() + 6) % 7;
  const clock = time.getUTCHours();
  const date = time.getUTCDate();
  if (host === "atlas") {
    const nightRun = (date === 3 || date === 10) && clock < 5;
    const on = day < 5 ? clock >= 7 || clock < 1 : clock >= 9 && clock < 23;
    if (nightRun) {
      return { state: "work", tokens: round2(0.9 + 0.5 * actRandom(hour, 1)), sessions: 1 };
    }
    if (!on) return { state: "off", tokens: 0, sessions: 0 };
    const work = ACT_PATTERN[day].some(
      ([from, to]) => from <= clock + 0.5 && clock + 0.5 < to
    );
    if (!work || actRandom(hour, 2) < 0.08) return { state: "idle", tokens: 0, sessions: 0 };
    return {
      state: "work",
      tokens: round2(0.35 + 0.75 * actRandom(hour, 3)),
      sessions: 1 + Math.floor(actRandom(hour, 4) * 3.6),
    };
  }
  if (host === "vega") {
    const evening = [0, 2, 3].includes(day) && clock >= 19 && clock < 24;
    const weekend = (day === 5 && clock >= 13 && clock < 21) || (day === 6 && clock >= 10 && clock < 19);
    if (!(evening || weekend)) return { state: "off", tokens: 0, sessions: 0 };
    if (actRandom(hour, 5) < 0.25) return { state: "idle", tokens: 0, sessions: 0 };
    return {
      state: "work",
      tokens: round2(0.2 + 0.5 * actRandom(hour, 6)),
      sessions: 1 + Math.floor(actRandom(hour, 7) * 1.9),
    };
  }
  throw new Error(`unknown host ${JSON.stringify(host)}`);
}

function subscriptionPerMillion() {
  return allUsedMoney() / TOK_TOTAL;
}

function valuesOfMillion(millions) {
  return {
    api: millions * API_PER_M,
    apiCurrency: "EUR",
    apiMissing: null,
    subscription: millions * subscriptionPerMillion(),
    currency: "EUR",
    subscriptionMissing: null,
  };
}

// A `Values` object for an answer whose two halves are already in money: the
// fixture has no missing half and no second currency.
function fixtureValues(api, subscription) {
  return {
    api,
    apiCurrency: api === null ? null : "EUR",
    apiMissing: null,
    subscription,
    currency: "EUR",
    subscriptionMissing:
      subscription === null ? "the fixture has no subscription value here" : null,
  };
}

function chosenHosts(query) {
  const all = ["atlas", "vega"];
  if (!query || typeof query !== "object") throw new Error("a statistics query is missing");
  if (query.host === null || query.host === undefined) return all;
  if (typeof query.host !== "string" || !all.includes(query.host)) {
    throw new Error(`unknown host ${JSON.stringify(query.host)}`);
  }
  return [query.host];
}

function hostLane(host) {
  const hours = [];
  let working = 0;
  let idle = 0;
  let off = 0;
  let unknown = 0;
  let out = 0;
  for (let step = 0; step < 168; step += 1) {
    const hour = ACT_FIRST + step;
    const entry = actHostHour(host, hour);
    if (entry.state === "work") working += 1;
    else if (entry.state === "idle") idle += 1;
    else if (entry.state === "off") off += 1;
    else unknown += 1;
    const outTokens = Math.round(entry.tokens * 1000000);
    out += outTokens;
    const byModel = [];
    if (entry.state === "work" && outTokens > 0) {
      const opus = Math.round(outTokens * 0.7);
      const sonnet = Math.round(outTokens * 0.22);
      byModel.push(["claude-opus-5-5", opus], ["claude-sonnet-5-5", sonnet]);
      byModel.push(["claude-fable-5-2", outTokens - opus - sonnet]);
    }
    hours.push({
      t: new Date(ACT_START + hour * HOUR).toISOString(),
      state: entry.state,
      byModel: byModel.filter(([, count]) => count > 0),
      sessions: entry.sessions,
      topProject: entry.state === "work" ? ACT_PROJECT_OF[hour % 7] : null,
      values: valuesOfMillion(entry.tokens),
    });
  }
  return {
    host,
    hours,
    working,
    perWorkingHour: working > 0 ? out / working : null,
    idle,
    off,
    unknown,
    tokens: tokenCounts(out),
    values: valuesOfMillion(out / 1000000),
  };
}

function activityDays(hosts) {
  const days = [];
  for (let day = 0; day < 14; day += 1) {
    const start = ACT_DAYS_START + day * DAY;
    const end = Math.min(start + DAY, ACT_NOW);
    const out = hosts.map((host) => {
      let total = 0;
      for (let hour = Math.ceil((start - ACT_START) / HOUR); hour < ACT_NOW_HOUR; hour += 1) {
        if (ACT_START + hour * HOUR >= end) break;
        total += actHostHour(host, hour).tokens * 1000000;
      }
      return [host, Math.round(total)];
    });
    days.push({ date: new Date(start).toISOString().slice(0, 10), out });
  }
  return days;
}

function activityWeek(index, hosts) {
  const from = weekTime(index);
  const to = Math.min(from + 7 * DAY, ACT_NOW);
  const hours = new Map(hosts.map((host) => [host, new Set()]));
  const byProject = new Map();
  let out = 0;
  let sessions = 0;
  for (let hour = Math.ceil((from - ACT_START) / HOUR); hour < ACT_NOW_HOUR; hour += 1) {
    if (ACT_START + hour * HOUR >= to) break;
    for (const host of hosts) {
      const entry = actHostHour(host, hour);
      if (entry.state !== "work") continue;
      hours.get(host).add(hour);
      const outTokens = Math.round(entry.tokens * 1000000);
      out += outTokens;
      sessions += entry.sessions;
      const project = ACT_PROJECT_OF[hour % 7];
      byProject.set(project, (byProject.get(project) || 0) + outTokens);
    }
  }
  let top = null;
  let best = -1;
  for (const [project, tokens] of [...byProject].sort((a, b) => a[0].localeCompare(b[0]))) {
    if (tokens > best) {
      best = tokens;
      top = [project, ACT_FOLDER_OF[project]];
    }
  }
  return {
    week: weekIso(index),
    workingHours: hosts.map((host) => [host, hours.get(host).size]),
    tokens: tokenCounts(out),
    values: valuesOfMillion(out / 1000000),
    sessions,
    top,
  };
}

function activityLargest() {
  // mock_activity.py's five largest sessions, largest by output tokens first.
  const rows = [
    ["atlas-a6", "atlas", "atlas", "claude-opus-5-5", 11, 12.4, 41],
    ["atlas-11", "atlas", "atlas", "claude-opus-5-5", 5, 7.9, null],
    ["acme-kb-3c", "acme-kb", "atlas", "claude-opus-5-5", 2, 4.4, 9],
    ["notes-47", "notes", "atlas", "claude-fable-5-2", 6, 5.3, 2],
    ["atlas-b3", "atlas", "vega", "claude-fable-5-2", 4, 3.9, 4],
  ];
  return rows
    .map(([label, project, host, model, workedHours, millions, agents]) => ({
      label,
      project,
      host,
      model,
      workedHours,
      tokens: tokenCounts(millions * 1000000),
      values: valuesOfMillion(millions),
      agents,
    }))
    .sort((a, b) => b.tokens.out - a.tokens.out || a.label.localeCompare(b.label));
}

function activityPeak() {
  let best = 0;
  for (const host of ["atlas", "vega"]) {
    for (let step = 0; step < 168; step += 1) {
      const out = Math.round(actHostHour(host, ACT_FIRST + step).tokens * 1000000);
      if (out > best) best = out;
    }
  }
  return tokenCounts(best);
}

// The minutes sessions waited per day of the range: a few days with 0 to 40
// minutes, so the bars have something to draw. `?fixture=stats-empty` still
// answers empty before this is reached.
function activityWaited(indexes) {
  const start = weekTime(indexes[0]);
  const end = Math.min(ACT_NOW, weekTime(indexes[indexes.length - 1]) + 7 * DAY);
  const rows = [];
  let day = 0;
  for (let time = start; time < end; time += DAY, day += 1) {
    const minutes = Math.round((((day * 17 + 11) % 41) * 10)) / 10;
    if (minutes === 0) continue;
    rows.push([new Date(time).toISOString().slice(0, 10), minutes]);
  }
  return rows;
}

function activity(query) {
  const { indexes } = chosenAccounts(query);
  const hosts = chosenHosts(query);
  return {
    lanes: hosts.map(hostLane),
    days: activityDays(hosts),
    weeks: indexes.map((index) => activityWeek(index, hosts)),
    largest: activityLargest(),
    waited: activityWaited(indexes),
    peakHour: activityPeak(),
  };
}

// ---------------------------------------------------------------------------
// Forecast (spec §3.4; data.py's forecast('slack') and forecast('stay'))
// ---------------------------------------------------------------------------

const FORECAST_NOW = Date.parse("2026-11-12T10:00:00.000Z");
const FORECAST_FROM = "2026-11-12T10:00:00.000Z";
const FORECAST_TO = new Date(FORECAST_NOW + 7 * 24 * HOUR).toISOString();
const FORECAST_NO_PLAN = "the forecast needs a reading of every account first";
const FORECAST_RESETS = {
  LB: "2026-11-16T17:00:00.000Z",
  AC: "2026-11-16T02:00:00.000Z",
  PR: "2026-11-14T19:00:00.000Z",
};
const FORECAST_FULL = {
  LB: "2026-11-15T20:30:00.000Z",
  AC: "2026-11-15T10:00:00.000Z",
  PR: "2026-11-14T11:30:00.000Z",
};
const FORECAST_LOW = { LB: 86, AC: 90, PR: 97 };
// The slack plan's percent at each reset, and the same when switching only at
// 100 % (data.py's forecast('slack') and forecast('stay')).
const FORECAST_PLANNED_AT_RESET = { LB: 100, AC: 100, PR: 100 };
const FORECAST_LAZY_AT_RESET = { LB: 90, AC: 80, PR: 100 };
const FORECAST_RUNS = [
  ["2026-11-12T10:00:00.000Z", "2026-11-12T12:00:00.000Z", "AC"],
  ["2026-11-12T12:00:00.000Z", "2026-11-12T13:00:00.000Z", "LB"],
  ["2026-11-12T14:00:00.000Z", "2026-11-12T15:00:00.000Z", "LB"],
  ["2026-11-12T15:00:00.000Z", "2026-11-12T17:00:00.000Z", "AC"],
  ["2026-11-12T17:00:00.000Z", "2026-11-12T19:00:00.000Z", "PR"],
  ["2026-11-12T19:30:00.000Z", "2026-11-12T20:00:00.000Z", "PR"],
  ["2026-11-12T20:00:00.000Z", "2026-11-12T22:00:00.000Z", "AC"],
  ["2026-11-12T22:00:00.000Z", "2026-11-13T00:00:00.000Z", "PR"],
  ["2026-11-13T08:00:00.000Z", "2026-11-13T10:00:00.000Z", "AC"],
  ["2026-11-13T10:00:00.000Z", "2026-11-13T13:00:00.000Z", "PR"],
  ["2026-11-13T14:00:00.000Z", "2026-11-13T17:30:00.000Z", "PR"],
  ["2026-11-13T17:30:00.000Z", "2026-11-13T19:00:00.000Z", "LB"],
  ["2026-11-13T19:30:00.000Z", "2026-11-13T20:30:00.000Z", "LB"],
  ["2026-11-13T20:30:00.000Z", "2026-11-13T22:30:00.000Z", "AC"],
  ["2026-11-13T22:30:00.000Z", "2026-11-14T00:00:00.000Z", "PR"],
  ["2026-11-14T09:00:00.000Z", "2026-11-14T11:30:00.000Z", "PR"],
  ["2026-11-14T11:30:00.000Z", "2026-11-14T13:00:00.000Z", "LB"],
  ["2026-11-14T15:00:00.000Z", "2026-11-14T16:00:00.000Z", "LB"],
  ["2026-11-14T16:00:00.000Z", "2026-11-14T18:00:00.000Z", "AC"],
  ["2026-11-14T18:00:00.000Z", "2026-11-14T20:30:00.000Z", "LB"],
  ["2026-11-14T20:30:00.000Z", "2026-11-14T21:00:00.000Z", "PR"],
  ["2026-11-15T09:00:00.000Z", "2026-11-15T10:00:00.000Z", "AC"],
  ["2026-11-15T10:00:00.000Z", "2026-11-15T12:30:00.000Z", "LB"],
  ["2026-11-15T12:30:00.000Z", "2026-11-15T13:00:00.000Z", "PR"],
  ["2026-11-15T15:00:00.000Z", "2026-11-15T17:30:00.000Z", "LB"],
  ["2026-11-15T17:30:00.000Z", "2026-11-15T20:00:00.000Z", "PR"],
  ["2026-11-15T20:00:00.000Z", "2026-11-15T20:30:00.000Z", "LB"],
  ["2026-11-15T20:30:00.000Z", "2026-11-15T21:00:00.000Z", "PR"],
  ["2026-11-16T08:00:00.000Z", "2026-11-16T10:00:00.000Z", "AC"],
  ["2026-11-16T10:00:00.000Z", "2026-11-16T13:00:00.000Z", "PR"],
  ["2026-11-16T14:00:00.000Z", "2026-11-16T16:00:00.000Z", "AC"],
  ["2026-11-16T16:00:00.000Z", "2026-11-16T17:00:00.000Z", "PR"],
  ["2026-11-16T17:00:00.000Z", "2026-11-16T19:00:00.000Z", "LB"],
  ["2026-11-16T19:30:00.000Z", "2026-11-16T21:30:00.000Z", "AC"],
  ["2026-11-16T21:30:00.000Z", "2026-11-16T23:30:00.000Z", "PR"],
  ["2026-11-16T23:30:00.000Z", "2026-11-17T00:00:00.000Z", "LB"],
  ["2026-11-17T08:00:00.000Z", "2026-11-17T10:00:00.000Z", "AC"],
  ["2026-11-17T10:00:00.000Z", "2026-11-17T13:00:00.000Z", "PR"],
  ["2026-11-17T14:00:00.000Z", "2026-11-17T16:00:00.000Z", "AC"],
  ["2026-11-17T16:00:00.000Z", "2026-11-17T18:30:00.000Z", "PR"],
  ["2026-11-17T18:30:00.000Z", "2026-11-17T19:00:00.000Z", "LB"],
  ["2026-11-17T19:30:00.000Z", "2026-11-17T21:30:00.000Z", "AC"],
  ["2026-11-17T21:30:00.000Z", "2026-11-18T00:00:00.000Z", "PR"],
  ["2026-11-18T08:00:00.000Z", "2026-11-18T10:00:00.000Z", "AC"],
  ["2026-11-18T10:00:00.000Z", "2026-11-18T13:00:00.000Z", "PR"],
  ["2026-11-18T14:00:00.000Z", "2026-11-18T16:00:00.000Z", "AC"],
  ["2026-11-18T16:00:00.000Z", "2026-11-18T18:30:00.000Z", "LB"],
  ["2026-11-18T18:30:00.000Z", "2026-11-18T19:00:00.000Z", "PR"],
  ["2026-11-18T19:30:00.000Z", "2026-11-19T00:00:00.000Z", "PR"],
  ["2026-11-19T08:00:00.000Z", "2026-11-19T10:00:00.000Z", "PR"],
];

function forecastPage() {
  const expire = (atReset) =>
    round2(
      sum(
        ACCOUNTS.map(
          (account) => ((100 - atReset[account.abbr]) / 100) * weeklyPrice(account)
        )
      )
    );
  const accounts = viewOrder().map((id) => {
    const account = accountById(id);
    const abbr = account.abbr;
    return {
      id,
      now: account.now,
      expected: FORECAST_PLANNED_AT_RESET[abbr],
      low: FORECAST_LOW[abbr],
      fullAt: FORECAST_FULL[abbr] === undefined ? null : FORECAST_FULL[abbr],
      resetsAt: FORECAST_RESETS[abbr],
      onlyWhenFullExpected: FORECAST_LAZY_AT_RESET[abbr],
      onlyWhenFullExpires: round2(
        ((100 - FORECAST_LAZY_AT_RESET[abbr]) / 100) * weeklyPrice(account)
      ),
    };
  });
  const lanes = viewOrder().map((id) => {
    const abbr = accountById(id).abbr;
    return {
      id,
      blocks: FORECAST_RUNS.filter(([, , account]) => account === abbr).map(([from, to]) => [
        from,
        to,
      ]),
      resets: [FORECAST_RESETS[abbr]],
    };
  });
  return {
    basisWeeks: 12,
    learning: [],
    kpis: {
      expirePlanned: expire(FORECAST_PLANNED_AT_RESET),
      expireOnlyWhenFull: expire(FORECAST_LAZY_AT_RESET),
      blockedHours: 0,
      switches: 42,
      switchesOnlyWhenFull: 11,
    },
    accounts,
    lanes,
    blocked: [],
    from: FORECAST_FROM,
    to: FORECAST_TO,
    sentence: "Enough for the week.",
  };
}

function statsEmpty() {
  return (
    typeof location !== "undefined" &&
    new URLSearchParams(location.search).get("fixture") === "stats-empty"
  );
}

function answer(query, build) {
  if (statsEmpty()) {
    return { empty: { recordingSince: "2026-11-09T08:00:00Z" } };
  }
  return { data: build(query) };
}

export function fixtureStats(command, args) {
  resolveIds();
  const params = args || {};
  const query = params.query;
  switch (command) {
    case "stats_hosts":
      return ["atlas", "vega"];
    case "stats_overview":
      return answer(query, overview);
    case "stats_weeks":
      return answer(query, weeks);
    case "stats_week_summary":
      return answer(query, (value) => weekSummary(value, params.week));
    case "stats_window_detail":
      return answer(query, (value) => windowDetail(value, params.accountId, params.end));
    case "stats_projects":
      return answer(query, projects);
    case "stats_project_detail":
      return answer(query, (value) => projectDetail(value, params.key));
    case "stats_subscriptions":
      return answer(query, subscriptions);
    case "stats_what_if":
      return answer(query, (value) => whatIf(value, params.without));
    case "stats_forecast":
      // The forecast never answers "empty": without a plan its one error is the
      // plain line the page shows.
      if (statsEmpty()) throw new Error(FORECAST_NO_PLAN);
      return { data: forecastPage() };
    case "stats_rhythm":
      return answer(query, rhythm);
    case "stats_activity":
      return answer(query, activity);
    default:
      throw new Error("unknown command");
  }
}
