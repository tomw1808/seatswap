// The Rhythm page (spec §3.5; step-c3 Task 8, part 3): the week as a heat map of
// 7 rows × 24 columns, Monday first, and the working and blocked hours per
// weekday. The page formats and draws the struct; the averages and the blocked
// hours come from the Rust page.

import { WEEKDAYS, clear, emptyLine, h, hoursText, pad, tokens } from "../../common.js";
import { bars, heat } from "../../charts.js";

const WEEKDAY_NAMES = [
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
  "Sunday",
];

function weekdayName(category) {
  const index = WEEKDAYS.indexOf(category);
  if (index < 0) {
    throw new Error(`the Rhythm page has no weekday ${JSON.stringify(category)}`);
  }
  return WEEKDAY_NAMES[index];
}

function hourRange(hour) {
  return `${pad(hour)}:00–${pad(hour + 1)}:00`;
}

// A null is a weekday that has not fully ended in the range: it has no
// average, and the page writes "—" for it, never 0.
function valueLabel(value) {
  return value === null ? "—" : hoursText(value);
}

// The y axis is 0–24h. A weekday on the day the clocks go back has 25 distinct
// local hours, so an average can be 25: only then does the axis grow to 25h
// (a value above 25 is still an error, which `bars()` reports). The labels stay
// at 0, 6, 12, 18 and 24h.
const HOUR_TICKS = [0, 6, 12, 18, 24];

function axisTop(values) {
  return values.some((value) => value !== null && value > 24) ? 25 : 24;
}

function dayValues(rows, field, name) {
  const values = new Array(7).fill(undefined);
  for (const row of rows) {
    if (!row || !Number.isInteger(row.weekday) || row.weekday < 0 || row.weekday > 6) {
      throw new Error(`the Rhythm page has a row without a weekday: ${JSON.stringify(row)}`);
    }
    if (values[row.weekday] !== undefined) {
      throw new Error(`the Rhythm page names ${name} twice for ${WEEKDAYS[row.weekday]}`);
    }
    const value = row[field];
    if (value !== null && (typeof value !== "number" || !Number.isFinite(value))) {
      throw new Error(
        `the Rhythm page has no ${name} for ${WEEKDAYS[row.weekday]}: ${JSON.stringify(value)}`
      );
    }
    values[row.weekday] = value;
  }
  for (const [index, value] of values.entries()) {
    if (value === undefined) {
      throw new Error(`the Rhythm page has no ${name} for ${WEEKDAYS[index]}`);
    }
  }
  return values;
}

export async function show(container, env) {
  const data = await env.call("stats_rhythm");
  clear(container);

  const root = h("div", { class: "g stats-rhythm" });
  container.appendChild(root);

  // -------------------------------------------------------------------------
  // the heat map
  // -------------------------------------------------------------------------
  const heatCard = h("div", { class: "hcard stats-rhythm-heat-card" });
  heatCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Your week, hour by hour" }),
      h("span", {
        class: "s",
        text: "output tokens per hour of the week, averaged over the range · stronger colour = more",
      }),
    ])
  );
  const heatBody = h("div", { class: "stats-rhythm-heat-body" });
  heatCard.appendChild(heatBody);
  root.appendChild(heatCard);

  if (data.heat.length === 0) {
    heatBody.appendChild(emptyLine("no hour recorded in the range"));
  } else {
    heatBody.appendChild(
      heat({
        rows: 7,
        cols: 24,
        values: data.heat,
        tip: (row, col, value) =>
          `${WEEKDAYS[row]} ${hourRange(col)} | ${tokens(value)} tokens a week`,
      })
    );
  }

  // -------------------------------------------------------------------------
  // the two bars per day
  // -------------------------------------------------------------------------
  const side = h("div", { class: "stats-rhythm-side" });
  root.appendChild(side);

  const workingCard = h("div", { class: "hcard" });
  workingCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Working hours per day" }),
      h("span", { class: "s", text: "hours with tokens, per day" }),
    ])
  );
  const workingBody = h("div", { class: "stats-rhythm-bars-body" });
  workingCard.appendChild(workingBody);
  side.appendChild(workingCard);

  const blockedCard = h("div", { class: "hcard" });
  blockedCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Blocked hours per day" }),
      h("span", { class: "s", text: "the active account was at 100 %, per day" }),
    ])
  );
  const blockedBody = h("div", { class: "stats-rhythm-bars-body" });
  blockedCard.appendChild(blockedBody);
  side.appendChild(blockedCard);

  if (data.perDay.length === 0) {
    workingBody.appendChild(emptyLine("no hour recorded in the range"));
    blockedBody.appendChild(emptyLine("no hour recorded in the range"));
  } else {
    const working = dayValues(data.perDay, "workingHours", "working hours");
    const blocked = dayValues(data.perDay, "blockedHours", "blocked hours");
    workingBody.appendChild(
      bars({
        categories: WEEKDAYS,
        yMax: axisTop(working),
        yTicks: HOUR_TICKS,
        yLabel: (value) => `${value}h`,
        labels: valueLabel,
        series: [
          {
            name: "working hours",
            color: "var(--focus)",
            values: working,
            unit: "h",
            tip: (category, value) =>
              value === null
                ? `${category} · — : ${weekdayName(category)} has not ended in the range`
                : `${category} · ${hoursText(value)} on an average ${weekdayName(category)}`,
          },
        ],
      })
    );
    blockedBody.appendChild(
      bars({
        categories: WEEKDAYS,
        yMax: axisTop(blocked),
        yTicks: HOUR_TICKS,
        yLabel: (value) => `${value}h`,
        labels: valueLabel,
        series: [
          {
            name: "blocked hours",
            color: "var(--danger)",
            values: blocked,
            unit: "h",
            tip: (category, value) =>
              value === null
                ? `${category} · — : ${weekdayName(category)} has not ended in the range`
                : `${category} · ${hoursText(value)} blocked on an average ${weekdayName(category)}`,
          },
        ],
      })
    );
  }

  side.appendChild(
    h("p", {
      class: "stats-note",
      text: "Auto-burn and the Forecast use the last 4 weeks before now, of every host: an hour that is usually coloured counts as a working hour.",
    })
  );
}
