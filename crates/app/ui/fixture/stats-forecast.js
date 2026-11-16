// The statistics Forecast page's fixture: `stats_forecast`.

import {
  ACCOUNTS,
  HOUR,
  accountById,
  round2,
  sum,
  viewOrder,
  weeklyPrice,
} from "./stats-frame.js";

// ---------------------------------------------------------------------------
// Forecast (spec §3.4; data.py's forecast('slack') and forecast('stay'))
// ---------------------------------------------------------------------------

const FORECAST_NOW = Date.parse("2026-11-12T10:00:00.000Z");
const FORECAST_FROM = "2026-11-12T10:00:00.000Z";
const FORECAST_TO = new Date(FORECAST_NOW + 7 * 24 * HOUR).toISOString();
export const FORECAST_NO_PLAN = "the forecast needs a reading of every account first";
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
// The slack plan's switches and the same when switching only at 100 %
// (data.py's forecast('slack') and forecast('stay')); "about N a day" is the
// plan's own count over the forecast's seven days.
const FORECAST_SWITCHES = 42;
const FORECAST_SWITCHES_ONLY_WHEN_FULL = 11;
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

export function forecastPage() {
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
      switches: FORECAST_SWITCHES,
      switchesPerDay: FORECAST_SWITCHES / 7,
      switchesOnlyWhenFull: FORECAST_SWITCHES_ONLY_WHEN_FULL,
    },
    accounts,
    lanes,
    laneOrder: [...viewOrder(), null],
    blocked: [],
    from: FORECAST_FROM,
    to: FORECAST_TO,
    sentence: "Enough for the week.",
  };
}
