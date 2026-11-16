// The statistics Rhythm page's fixture: `stats_rhythm`.

import {
  chosenAccounts,
  chosenHosts,
  round1,
  sum,
} from "./stats-frame.js";

// ---------------------------------------------------------------------------
// Rhythm (spec §3.5; data.py's heat())
// ---------------------------------------------------------------------------

export const ACT_PATTERN = [
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[8, 13], [14, 19], [19.5, 24]],
  [[9, 13], [15, 21]],
  [[9, 13], [15, 21]],
];

const BLOCKED_PER_DAY = [0.4, 0.3, 0.6, 0.9, 1.1, 0.2, 1.3];

export function rhythm(query) {
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
