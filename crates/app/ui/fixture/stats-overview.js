// The statistics Overview page's fixture: `stats_overview`.

import {
  BLOCKED,
  DAY,
  byAbbr,
  chosenAccounts,
  expiredByAccount,
  findingAutoBurn,
  findingBlocked,
  findingExpired,
  findingFull,
  marksOf,
  sum,
  switchesOf,
  topProjects,
  weekAccounts,
  weekIso,
  weekTime,
  weeklyPrice,
  windowsOfWeek,
} from "./stats-frame.js";

export function overview(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const currency = "EUR";
  const weeks = indexes.map((index) => {
    const accounts = weekAccounts(index, chosen);
    const used = sum(accounts.map((account) => account.used ?? 0));
    const expired = sum(accounts.map((account) => account.expired ?? 0));
    // Every window of a priced account week offers one week's price, as Rust
    // counts the offered weeks (a reset week offers each of its windows).
    const offered = sum(
      chosen
        .filter((account) => account.used[index] !== null)
        .map((account) => weeklyPrice(account) * windowsOfWeek(account, index).length)
    );
    return {
      week: weekIso(index),
      accounts,
      projects: topProjects(index, used, false),
      blockedHours: BLOCKED[index],
      expiredShare: offered > 0 ? (expired / offered) * 100 : null,
    };
  });
  const used = sum(weeks.flatMap((week) => week.accounts.map((account) => account.used ?? 0)));
  const expired = sum(weeks.flatMap((week) => week.accounts.map((account) => account.expired ?? 0)));
  const paid = sum(
    indexes.flatMap((index) =>
      chosen
        .filter((account) => account.used[index] !== null)
        .map((account) => weeklyPrice(account))
    )
  );
  // The percentages divide by the windows the weeks offered, not by `paid`: a
  // reset week offers each of its windows.
  const offered = sum(
    indexes.flatMap((index) =>
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
      usedPercent,
      expiredPercent,
      expiredByAccount: expiredByAccount(indexes, chosen),
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
