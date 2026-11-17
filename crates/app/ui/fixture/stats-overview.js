// The statistics Overview page's fixture: `stats_overview` and its This-week
// card, `stats_this_week`.
//
// Two variants by the page address: `?fixture=stats-no-running` has no usage
// reading yet, so the This-week card is empty; `?fixture=stats-day1` has the
// running windows but no finished one, so the history below is empty. With
// `?fixture=stats-empty` every range holds nothing, and the card still answers.

import {
  BLOCKED,
  DAY,
  NOW_MS,
  byAbbr,
  chosenAccounts,
  expiredByAccount,
  findingAutoBurn,
  findingBlocked,
  findingExpired,
  findingFull,
  marksOf,
  resetEnd,
  sum,
  switchesOf,
  topProjects,
  weekAccounts,
  weekIso,
  weekTime,
  weeklyPrice,
  windowsOfWeek,
} from "./stats-frame.js";

function variant() {
  if (typeof location === "undefined") return null;
  return new URLSearchParams(location.search).get("fixture");
}

// Each account's running window by project, in % of its API value: the
// mock-up's four projects, PR's smallest one beyond the three named.
const RUNNING_PROJECTS = {
  LB: [
    ["atlas", 58],
    ["ledger", 27],
    ["notes", 15],
  ],
  AC: [
    ["acme-kb", 81],
    ["atlas", 19],
  ],
  PR: [
    ["atlas", 44],
    ["ledger", 30],
    ["notes", 16],
    ["acme-kb", 10],
  ],
};

// The three largest, then `other`, as Rust's `top_three` writes them.
function topThree(items) {
  const sorted = [...items].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  if (sorted.length <= 3) return sorted;
  return [...sorted.slice(0, 3), ["other", sum(sorted.slice(3).map(([, value]) => value))]];
}

// The weekly window of `account` that runs at the fixture's now: the first one
// whose reset lies after it.
function runningWindow(account) {
  for (let index = 0; ; index += 1) {
    const end = Date.parse(resetEnd(account, index));
    if (end > NOW_MS) return { start: end - 7 * DAY, end };
  }
}

// The other providers' work since Monday, at API prices in EUR: a Codex
// session on atlas and ledger, flash workers on ledger and notes. The account
// chips do not filter them.
const PROVIDERS = [
  {
    name: "Codex",
    api: 34.1,
    apiCurrency: "EUR",
    apiMissing: null,
    projects: [
      ["atlas", 72],
      ["ledger", 28],
    ],
  },
  {
    name: "Ollama",
    api: 6.85,
    apiCurrency: "EUR",
    apiMissing: null,
    projects: [
      ["ledger", 64],
      ["notes", 36],
    ],
  },
];

// The card ignores the range, so it never answers "empty".
export function thisWeek(query) {
  const { chosen } = chosenAccounts(query);
  if (variant() === "stats-no-running") return { accounts: [], providers: [], projects: [] };
  const accounts = chosen.map((account) => {
    const window = runningWindow(account);
    const projects = RUNNING_PROJECTS[account.abbr];
    if (!projects) throw new Error(`no running projects for ${account.abbr}`);
    return {
      id: account.id,
      used: account.now,
      now: account.now,
      money: (account.now / 100) * weeklyPrice(account),
      moneyMissing: null,
      startedAt: new Date(window.start).toISOString(),
      resetsAt: new Date(window.end).toISOString(),
      projects: topThree(projects),
    };
  });
  // All listed accounts and providers together, each weighted by its value
  // this week (an account's money stands in for its API value here).
  const total =
    sum(accounts.map((account) => account.money)) + sum(PROVIDERS.map((provider) => provider.api));
  const byProject = new Map();
  const add = (weight, projects) => {
    for (const [name, share] of projects) {
      byProject.set(name, (byProject.get(name) || 0) + (weight * share) / total);
    }
  };
  for (const account of chosen) {
    add((account.now / 100) * weeklyPrice(account), RUNNING_PROJECTS[account.abbr]);
  }
  for (const provider of PROVIDERS) add(provider.api, provider.projects);
  return { accounts, providers: PROVIDERS, projects: topThree([...byProject.entries()]) };
}

export function overview(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const currency = "EUR";
  // The weeks whose windows finished: none on day 1.
  const finished = variant() === "stats-day1" ? [] : indexes;
  const weeks = indexes.map((index) => {
    const done = finished.includes(index);
    const accounts = done ? weekAccounts(index, chosen) : [];
    const used = sum(accounts.map((account) => account.used ?? 0));
    const expired = sum(accounts.map((account) => account.expired ?? 0));
    // Every window of a priced account week offers one week's price, as Rust
    // counts the offered weeks (a reset week offers each of its windows).
    const offered = done
      ? sum(
          chosen
            .filter((account) => account.used[index] !== null)
            .map((account) => weeklyPrice(account) * windowsOfWeek(account, index).length)
        )
      : 0;
    return {
      week: weekIso(index),
      accounts,
      projects: done ? topProjects(index, used, false) : [],
      blockedHours: BLOCKED[index],
      expiredShare: offered > 0 ? (expired / offered) * 100 : null,
    };
  });
  const used = sum(weeks.flatMap((week) => week.accounts.map((account) => account.used ?? 0)));
  const expired = sum(weeks.flatMap((week) => week.accounts.map((account) => account.expired ?? 0)));
  const paid = sum(
    finished.flatMap((index) =>
      chosen
        .filter((account) => account.used[index] !== null)
        .map((account) => weeklyPrice(account))
    )
  );
  // The percentages divide by the windows the weeks offered, not by `paid`: a
  // reset week offers each of its windows.
  const offered = sum(
    finished.flatMap((index) =>
      chosen
        .filter((account) => account.used[index] !== null)
        .map((account) => weeklyPrice(account) * windowsOfWeek(account, index).length)
    )
  );
  const switches = switchesOf(indexes);
  const switchesTotal =
    switches.user + switches.cli + switches.auto + switches.agent + switches.external;
  const blockedHours = sum(indexes.map((index) => BLOCKED[index]));
  const avoidableHours = blockedHours > 0 ? (blockedHours / 57) * 6 : 0;
  // The mock-up's percentages: the used and expired money of the priced weeks
  // over the money their windows offered, the same rule as the Rust page.
  const usedPercent = offered > 0 ? (used / offered) * 100 : null;
  const expiredPercent = offered > 0 ? (expired / offered) * 100 : null;
  // The last account added in the range: the blocked hours of the whole weeks
  // before the week it was added in (the Rust page's `blockedBeforeAdded`).
  const added = marksOf(indexes).filter((mark) => mark.label.endsWith(" added"));
  const lastAdded = added[added.length - 1];
  let blockedBeforeAdded = null;
  if (lastAdded) {
    const name = lastAdded.label.slice(0, -" added".length);
    if (!byAbbr(name)) {
      throw new Error(`the added account ${JSON.stringify(name)} is not in the fixture`);
    }
    const at = Date.parse(lastAdded.at);
    const hours = sum(
      indexes.filter((index) => weekTime(index) + 7 * DAY <= at).map((index) => BLOCKED[index])
    );
    blockedBeforeAdded = { name, hours };
  }
  const findings = [
    finished.length > 0 ? findingExpired(finished, chosen, currency) : null,
    findingBlocked(indexes, blockedHours, avoidableHours),
    finished.length > 0 ? findingAutoBurn(finished, chosen) : null,
    findingFull(finished, chosen),
  ].filter((text) => text !== null);
  return {
    withoutPrice: [],
    kpis: {
      used,
      paid,
      expired,
      usedPercent,
      expiredPercent,
      expiredByAccount: expiredByAccount(finished, chosen),
      blockedHours,
      blockedBeforeAdded,
      avoidableHours,
      switches,
      switchesTotal,
      switchCostPercentOfWindow: 0.8,
      weeks: indexes.length,
    },
    weeks,
    marks: marksOf(indexes),
    findings,
  };
}
