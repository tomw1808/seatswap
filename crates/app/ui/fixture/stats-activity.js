// The statistics Activity page's fixture: `stats_activity`.

import {
  API_PER_M,
  DAY,
  HOUR,
  TOK_TOTAL,
  WEEKS,
  allUsedMoney,
  chosenAccounts,
  chosenHosts,
  round2,
  tokenCounts,
  weekTime,
} from "./stats-frame.js";
import { ACT_PATTERN } from "./stats-rhythm.js";

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
    week: new Date(weekTime(index)).toISOString(),
    workingHours: hosts.map((host) => [host, hours.get(host).size]),
    tokens: tokenCounts(out),
    values: valuesOfMillion(out / 1000000),
    sessions,
    top,
    running: from <= ACT_NOW && ACT_NOW < from + 7 * DAY,
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

// The mock-up's "What it shows" sentence: the page shows it, the fixture names
// its words; the numbers in it are the mock-up's, not the page's. The lanes
// carry no agent data, so the sentence names the work, not an agent run.
const ACT_WHAT_IT_SHOWS =
  "Two nights with work on atlas. vega is used on three evenings and at the weekend.";

// A 30-character host name for the wrapping check (`?fixture=longhost`).
const ACT_LONG_HOSTS = {
  atlas: "atlas-sam-workstation-nov-2026",
  vega: "vega-sam-laptop-workstation-01",
};

function longHostsWanted() {
  if (typeof location === "undefined" || typeof URLSearchParams !== "function") return false;
  try {
    return new URLSearchParams(location.search).get("fixture") === "longhost";
  } catch {
    return false;
  }
}

function addTokens(one, other) {
  return {
    input: one.input + other.input,
    out: one.out + other.out,
    cw: one.cw + other.cw,
    cr: one.cr + other.cr,
  };
}

function sumValues(values) {
  const total = {
    api: 0,
    apiCurrency: values.length > 0 ? values[0].apiCurrency : null,
    apiMissing: null,
    subscription: 0,
    currency: values.length > 0 ? values[0].currency : null,
    subscriptionMissing: null,
  };
  for (const value of values) {
    total.api += value.api;
    total.subscription += value.subscription;
  }
  return total;
}

function renameHosts(answer) {
  if (!longHostsWanted()) return answer;
  const name = (host) => ACT_LONG_HOSTS[host] || host;
  for (const lane of answer.lanes) lane.host = name(lane.host);
  for (const day of answer.days) day.out = day.out.map(([host, out]) => [name(host), out]);
  for (const week of answer.weeks) {
    week.workingHours = week.workingHours.map(([host, hours]) => [name(host), hours]);
  }
  for (const session of answer.largest) session.host = name(session.host);
  answer.whatItShows = answer.whatItShows
    .replace(/\batlas\b/g, name("atlas"))
    .replace(/\bvega\b/g, name("vega"));
  return answer;
}

// The weeks the Activity page shows. The last week of `WEEKS` is finished;
// the week after it is the one ACT_NOW lies in, so it is the running week.
function activityWeekIndexes(range, indexes) {
  const current = WEEKS.length;
  if (range === "thisWeek") return [current];
  if (range === "lastWeek") return [WEEKS.length - 1];
  return indexes.includes(current) ? indexes : [...indexes, current];
}

export function activity(query) {
  const { indexes: rangeIndexes } = chosenAccounts(query);
  const indexes = activityWeekIndexes(query.range, rangeIndexes);
  const hosts = chosenHosts(query);
  const lanes = hosts.map(hostLane);
  let laneOut = 0;
  let working = 0;
  for (const lane of lanes) {
    for (const hour of lane.hours) {
      for (const [, out] of hour.byModel) laneOut += out;
    }
    working += lane.working;
  }
  // A host's tokens and values follow the range, as the Rust's do: they are the
  // sums over the weeks the Per week table lists, not over the 7-day lane. The
  // totals are the sums over the hosts.
  const weeks = indexes.map((index) => activityWeek(index, hosts));
  for (const lane of lanes) {
    const own = indexes.map((index) => activityWeek(index, [lane.host]));
    lane.tokens = own.reduce((total, week) => addTokens(total, week.tokens), {
      input: 0,
      out: 0,
      cw: 0,
      cr: 0,
    });
    lane.values = sumValues(own.map((week) => week.values));
  }
  return renameHosts(
    {
      lanes,
      days: activityDays(hosts),
      weeks,
      largest: activityLargest(),
      waited: activityWaited(indexes),
      peakHour: activityPeak(),
      totalTokens: lanes.reduce((total, lane) => addTokens(total, lane.tokens), {
        input: 0,
        out: 0,
        cw: 0,
        cr: 0,
      }),
      totalValues: sumValues(lanes.map((lane) => lane.values)),
      perWorkingHour: working > 0 ? laneOut / working : null,
      whatItShows: ACT_WHAT_IT_SHOWS,
    }
  );
}
