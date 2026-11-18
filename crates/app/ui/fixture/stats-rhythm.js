// The statistics Rhythm page's fixture: `stats_rhythm`.

import { chosenAccounts, chosenHosts } from "./stats-frame.js";

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

// Average hours per whole day: Monday is 0, and Sunday has not fully ended in
// the range, so its value is null (the page writes "—", never 0).
const WORKING_PER_DAY = [0, 12, 7.25, 8, 6.5, 4, null];

const BLOCKED_PER_DAY = [0.5, 0.25, 0.75, 1.5, 2, 0.5, null];

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
  const perDay = ACT_PATTERN.map((_, day) => ({
    weekday: day,
    workingHours: WORKING_PER_DAY[day],
    blockedHours: BLOCKED_PER_DAY[day],
  }));
  return { heat, perDay };
}
