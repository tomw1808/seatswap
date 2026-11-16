// The statistics Subscriptions page's fixture: `stats_subscriptions` and
// `stats_what_if`.

import {
  API_TOTAL,
  BLOCKED,
  TOKENS_PER_PERCENT,
  accountById,
  allUsedMoney,
  chosenAccounts,
  expiredMoney,
  fixtureValues,
  round1,
  round2,
  sum,
  usedMoney,
  weekIso,
  weeklyPrice,
} from "./stats-frame.js";

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
    sentence: "Without LB you would have been blocked 3d 12h more. Keep it.",
    weekly: [22, 26, 19, 24, 21, 6, 4, 5, 3, 6, 3, 2],
  },
  AC: {
    blocked: 96,
    expired: 164,
    saved: 388,
    sentence:
      "Before PR was added AC saved you 1d 7h. In the 7 weeks since, dropping it would have cost 8h.",
    weekly: [17, 21, 14, 18, 15, 3, 1, 1.5, 1, 2.5, 1, 1],
  },
  PR: {
    blocked: 111,
    expired: 114,
    saved: 582,
    sentence: "Without PR the blocked hours of August come back: 2d 6h more in 7 weeks.",
    weekly: [11, 14, 8, 12, 9, 9, 7, 8, 6, 9, 9, 9],
  },
};

function verdictFor(weeks, average, fullWeeks) {
  if (weeks < 4) return "tooEarly";
  if (average !== null && average < 40) return "mostlyUnused";
  if (fullWeeks * 2 >= weeks) return "regularlyFull";
  return "wellUsed";
}

export function subscriptions(query) {
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

export function whatIf(query, without) {
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
