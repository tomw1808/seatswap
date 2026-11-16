// The statistics Overview page's fixture: `stats_overview`.

import {
  BLOCKED,
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
} from "./stats-frame.js";

export function overview(query) {
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
