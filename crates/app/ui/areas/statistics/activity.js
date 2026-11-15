// The Activity page (spec §3.9; step-c3 Task 8, part 3): the numbers per host,
// the last 7 days hour by hour, the tokens per day, the sessions waited for you
// and the tabs Per week · Largest sessions. The page formats and draws the
// struct; its unit switch picks which of the struct's per-hour numbers a bar
// shows. A missing value is "—", and the `Missing` reason of a `Values` half is
// its tooltip.

import {
  WEEKDAYS,
  amountNode,
  amountTip,
  clear,
  dateText,
  emptyLine,
  h,
  hoursText,
  isObject,
  kpi,
  legendItem,
  modelColor,
  modelName,
  pad,
  readChoice,
  shortDate,
  tokens,
  valueTip,
  weekdayIndex,
  writeChoice,
} from "../../common.js";
import { bars, hours } from "../../charts.js";

const UNIT_KEY = "seatswap.stats.unit";
const UNITS = [
  { id: "tokens", label: "Tokens" },
  { id: "subscription", label: "Subscription value" },
  { id: "api", label: "API value" },
];
const UNIT_IDS = UNITS.map((unit) => unit.id);

// Hosts are not accounts: they get a token each, in this order.
const HOST_TOKENS = [
  "var(--focus)",
  "var(--warn)",
  "var(--pill-text)",
  "var(--danger)",
  "var(--muted)",
];

const STATE_TEXT = {
  work: "worked",
  idle: "on, but idle",
  off: "off, asleep, or seatswap was not running",
  unknown: "not known",
};

function stateOf(state) {
  if (typeof state !== "string" || STATE_TEXT[state] === undefined) {
    throw new Error(`unknown hour state ${JSON.stringify(state)}`);
  }
  return state;
}

function hostColour(index) {
  return HOST_TOKENS[index % HOST_TOKENS.length];
}

// "Tue 12 Nov 10:00" in this computer's local time.
function hourText(value) {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) throw new Error(`invalid time ${JSON.stringify(value)}`);
  return `${dateText(date)} ${pad(date.getHours())}:00`;
}

// "3 Nov" from the struct's "YYYY-MM-DD": the day it names, never shifted by a
// time zone.
function dayLabel(value) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(value));
  if (!match) throw new Error(`a date is YYYY-MM-DD, not ${JSON.stringify(value)}`);
  const date = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return date.toLocaleDateString("en-GB", { day: "numeric", month: "short" });
}

function tokenText(counts) {
  if (!isObject(counts)) throw new Error("a token count is missing");
  return tokens(counts.out);
}

function tokensTip(counts) {
  return `input ${tokens(counts.input)} · output ${tokens(counts.out)} · cache write ${tokens(
    counts.cw
  )} · cache read ${tokens(counts.cr)}`;
}

// Which of the struct's numbers the hour's bar measures. Tokens are per model;
// the two values exist per hour only, so a value unit draws one segment and its
// tip says so.
function hourStack(cell, unit) {
  if (unit === "tokens") {
    const stack = [];
    for (const [model, count] of cell.byModel) {
      if (typeof count !== "number" || count <= 0) continue;
      stack.push({ value: count, color: modelColor(model) });
    }
    return stack;
  }
  const values = cell.values;
  const value = unit === "api" ? values.api : values.subscription;
  if (typeof value !== "number") return [];
  return [{ value, color: unit === "api" ? "var(--warn)" : "var(--pill-text)" }];
}

export async function show(container, env) {
  const data = await env.call("stats_activity");
  clear(container);

  const lanes = data.lanes;
  const state = {
    unit: readChoice(UNIT_KEY, UNIT_IDS, "tokens"),
    tab: "weeks",
  };

  const root = h("div", { class: "g stats-activity" });
  container.appendChild(root);

  // -------------------------------------------------------------------------
  // the numbers per host
  // -------------------------------------------------------------------------
  const kpisCard = h("div", { class: "hcard stats-activity-kpis-card" });
  const kpis = h("div", { class: "stats-kpis" });
  kpisCard.appendChild(kpis);
  root.appendChild(kpisCard);

  for (const lane of lanes) {
    const parts = [`on but idle ${hoursText(lane.idle)}`, `off ${hoursText(lane.off)}`];
    if (lane.unknown > 0) {
      parts.push(`not known ${hoursText(lane.unknown)}`);
    }
    parts.push(`${tokenText(lane.tokens)} tokens in the range`);
    kpis.appendChild(
      kpi(
        `${lane.host} · last 7 days`,
        `${hoursText(lane.working)} working`,
        parts.join(" · "),
        `${lane.host} | last 7 days: working ${hoursText(lane.working)} · idle ${hoursText(
          lane.idle
        )} · off ${hoursText(lane.off)}${
          lane.unknown > 0 ? ` · not known ${hoursText(lane.unknown)}` : ""
        } | in the range: ${tokensTip(lane.tokens)} | ${valueTip(
          lane.values,
          "subscription",
          "in the range"
        )} | ${valueTip(lane.values, "api", "in the range")}`
      )
    );
  }

  if (lanes.length === 0) {
    kpis.appendChild(emptyLine("no host recorded in the range"));
  }

  const valueParts = [];
  const apiParts = [];
  const valueTips = [];
  const apiTips = [];
  for (const lane of lanes) {
    valueParts.push(
      `${lane.host} ${amountTip(lane.values.subscription, lane.values.currency)}`
    );
    apiParts.push(`${lane.host} ${amountTip(lane.values.api, lane.values.apiCurrency)}`);
    valueTips.push(`${lane.host}: ${valueTip(lane.values, "subscription", "in the range")}`);
    apiTips.push(`${lane.host}: ${valueTip(lane.values, "api", "in the range")}`);
  }
  if (lanes.length > 0) {
    kpis.appendChild(
      kpi(
        "Their value",
        valueParts.join(" · "),
        `at API prices in the range: ${apiParts.join(" · ")}`,
        `Value of the subscriptions | per host, never added across hosts | ${valueTips.join(
          " | "
        )} | at API prices | ${apiTips.join(" | ")}`,
        "stats-kpi-value-small"
      )
    );

    const perHour = lanes.map((lane) => {
      if (lane.perWorkingHour === null || lane.perWorkingHour === undefined) {
        return `${lane.host} — (no working hour in the last 7 days)`;
      }
      return `${lane.host} ${tokens(lane.perWorkingHour)}`;
    });
    kpis.appendChild(
      kpi(
        "Per working hour · last 7 days",
        perHour.join(" · "),
        "output tokens of the last 7 days in the working hours of those days",
        "Per working hour · last 7 days | each host's output tokens of the last 7 days over its working hours of those days",
        "stats-kpi-value-small"
      )
    );
  }

  kpis.appendChild(
    kpi(
      "Highest hour",
      tokenText(data.peakHour),
      "output tokens in one hour, all hosts together",
      `Highest hour | ${tokensTip(data.peakHour)}`
    )
  );

  // -------------------------------------------------------------------------
  // the last 7 days, hour by hour
  // -------------------------------------------------------------------------
  const hoursCard = h("div", { class: "hcard stats-activity-hours-card" });
  root.appendChild(hoursCard);

  const hoursHead = h("div", { class: "ch" });
  hoursHead.appendChild(h("span", { class: "t", text: "The last 7 days, hour by hour" }));
  hoursHead.appendChild(
    h("span", {
      class: "s",
      text: "bar = the hour · strip = sessions working · hatched = off, plain = on but idle, empty = not known",
    })
  );
  hoursHead.appendChild(h("span", { class: "sp" }));
  const unitSwitch = h("div", {
    class: "stats-tabs stats-mode",
    role: "group",
    "aria-label": "What the bars measure",
  });
  hoursHead.appendChild(unitSwitch);
  hoursCard.appendChild(hoursHead);

  const legend = h("div", { class: "stats-act-legend" });
  hoursCard.appendChild(legend);
  const hoursBody = h("div", { class: "stats-act-hours-body" });
  hoursCard.appendChild(hoursBody);

  const firstLane = lanes.find((lane) => lane.hours.length > 0);
  const hourTimes = firstLane ? firstLane.hours.map((cell) => new Date(cell.t)) : [];
  const days = [];
  if (hourTimes.length > 0) {
    let start = 0;
    for (let index = 1; index <= hourTimes.length; index += 1) {
      const sameDay =
        index < hourTimes.length &&
        hourTimes[index].toDateString() === hourTimes[start].toDateString();
      if (sameDay) continue;
      days.push({
        label: `${WEEKDAYS[weekdayIndex(hourTimes[start])]} ${hourTimes[start].getDate()}`,
        span: index - start,
      });
      start = index;
    }
  }

  function paintLegend() {
    clear(legend);
    if (state.unit === "tokens") {
      const models = new Set();
      for (const lane of lanes) {
        for (const cell of lane.hours) {
          for (const [model] of cell.byModel) models.add(model);
        }
      }
      for (const model of models) legend.appendChild(legendItem(modelColor(model), modelName(model), false));
    } else {
      legend.appendChild(
        legendItem(
          state.unit === "api" ? "var(--warn)" : "var(--pill-text)",
          state.unit === "api" ? "API value of the hour" : "subscription value of the hour",
          false
        )
      );
    }
    legend.appendChild(legendItem("var(--track)", "on, but idle", false));
    legend.appendChild(legendItem("var(--text)", "off", true));
  }

  function hourTip(lane, cell) {
    const lines = [`${lane.host} · ${hourText(cell.t)}`, stateOf(cell.state)];
    const parts = [];
    for (const [model, count] of cell.byModel) {
      parts.push(`${modelName(model)} ${tokens(count)}`);
    }
    if (parts.length > 0) lines.push(parts.join(" · "));
    else if (cell.state === "work") lines.push("no output tokens in this hour");
    lines.push(
      `${cell.sessions} session${cell.sessions === 1 ? "" : "s"}${
        cell.topProject !== null && cell.topProject !== undefined
          ? ` · most: ${cell.topProject}`
          : ""
      }`
    );
    lines.push(valueTip(cell.values, "subscription", "in this hour"));
    lines.push(valueTip(cell.values, "api", "in this hour"));
    return lines.join(" | ");
  }

  function paintHours() {
    for (const button of unitSwitch.childNodes) {
      const on = button.dataset && button.dataset.unit === state.unit;
      button.classList.toggle("stats-tab-on", on);
      button.setAttribute("aria-pressed", on ? "true" : "false");
    }
    paintLegend();
    clear(hoursBody);
    const hourLanes = lanes.map((lane) => ({
      label: h("span", { text: lane.host }),
      cells: lane.hours.map((cell) => ({
        state: stateOf(cell.state),
        stack: hourStack(cell, state.unit),
        strip: cell.sessions,
        tip: hourTip(lane, cell),
      })),
    }));
    if (hourLanes.every((lane) => lane.cells.length === 0)) {
      hoursBody.appendChild(emptyLine("no hour recorded on this host"));
    } else {
      hoursBody.appendChild(hours({ lanes: hourLanes, days }));
      for (const [index, lane] of lanes.entries()) {
        if (lane.hours.length === 0) {
          hoursBody.appendChild(
            emptyLine(`no hour recorded on ${hourLanes[index].label.textContent}`)
          );
        }
      }
    }
  }

  for (const unit of UNITS) {
    const button = h(
      "button",
      {
        type: "button",
        class: "stats-tab",
        "aria-pressed": state.unit === unit.id ? "true" : "false",
        onclick: () => {
          state.unit = unit.id;
          writeChoice(UNIT_KEY, unit.id);
          paintHours();
        },
      },
      unit.label
    );
    button.dataset.unit = unit.id;
    unitSwitch.appendChild(button);
  }
  paintHours();

  // -------------------------------------------------------------------------
  // the bottom row: tokens per day and the waited minutes, the tabs
  // -------------------------------------------------------------------------
  const left = h("div", { class: "hcard stats-act-days-card" });
  root.appendChild(left);
  left.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Tokens per day" }),
      h("span", { class: "s", text: "output tokens · the last 14 days" }),
    ])
  );
  const daysBody = h("div", { class: "stats-act-days-body" });
  left.appendChild(daysBody);

  const hosts = lanes.map((lane) => lane.host);
  const dayRows = data.days;
  const daySeries = hosts.map((host, index) => {
    const values = dayRows.map((day) => {
      const found = day.out.find(([name]) => name === host);
      return found ? found[1] : 0;
    });
    return {
      name: host,
      color: hostColour(index),
      values,
      tip: (category, value) => `${category} | ${host}: ${tokens(value)} output tokens`,
    };
  });
  if (dayRows.length > 0 && daySeries.length > 0) {
    daysBody.appendChild(
      bars({
        categories: dayRows.map((day) => dayLabel(day.date)),
        series: daySeries,
      })
    );
  } else {
    daysBody.appendChild(emptyLine("no session in the range"));
  }

  const waitedCard = h("div", { class: "stats-act-waited" });
  waitedCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Minutes sessions waited for you" }),
      h("span", { class: "s", text: "per day" }),
    ])
  );
  const waitedBody = h("div", { class: "stats-act-waited-body" });
  waitedCard.appendChild(waitedBody);
  left.appendChild(waitedCard);

  if (data.waited.length > 0) {
    waitedBody.appendChild(
      bars({
        categories: data.waited.map(([date]) => dayLabel(date)),
        labelEvery: Math.max(1, Math.ceil(data.waited.length / 14)),
        series: [
          {
            name: "waited",
            color: "var(--warn)",
            values: data.waited.map(([, minutes]) => Math.round(minutes * 10) / 10),
            unit: " min",
            tip: (category, value) => `${category} | ${value} minutes waited for you`,
          },
        ],
      })
    );
  } else {
    waitedBody.appendChild(emptyLine("no session waited for you in the range"));
  }

  const tabCard = h("div", { class: "hcard stats-act-tabs-card" });
  root.appendChild(tabCard);
  const tabSwitch = h("div", {
    class: "stats-tabs stats-mode",
    role: "tablist",
    "aria-label": "Activity lists",
  });
  const tabBody = h("div", { class: "stats-act-tabbody" });
  tabCard.appendChild(tabSwitch);
  tabCard.appendChild(tabBody);

  function tabButton(id, label) {
    const button = h(
      "button",
      {
        type: "button",
        class: `stats-tab${state.tab === id ? " stats-tab-on" : ""}`,
        role: "tab",
        "aria-pressed": state.tab === id ? "true" : "false",
        onclick: () => {
          state.tab = id;
          paintTab();
        },
      },
      label
    );
    button.dataset.tab = id;
    return button;
  }

  function weeksView() {
    const view = h("div", { class: "stats-act-table" });
    view.style.setProperty(
      "--stats-act-columns",
      `62px ${hosts.map(() => "54px").join(" ")} 62px 76px 70px 46px minmax(0, 1fr)`
    );
    const head = h("div", { class: "stats-act-row stats-act-row-head" });
    head.appendChild(h("span", { text: "Week" }));
    for (const host of hosts) head.appendChild(h("span", { class: "n", text: host }));
    for (const label of ["Tokens", "Subscription", "API", "Sessions", "Most tokens"]) {
      head.appendChild(h("span", { class: "n", text: label }));
    }
    view.appendChild(head);
    const weeks = [...data.weeks].reverse();
    if (weeks.length === 0) {
      view.appendChild(emptyLine("no week in the range"));
      return view;
    }
    for (const week of weeks) {
      const row = h("div", { class: "stats-act-row" });
      row.appendChild(h("span", { class: "mf", text: shortDate(week.week) }));
      for (const host of hosts) {
        const found = week.workingHours.find(([name]) => name === host);
        row.appendChild(
          h("span", {
            class: "n",
            text: found ? hoursText(found[1]) : "—",
            title: found ? undefined : `no hour recorded on ${host} in this week`,
          })
        );
      }
      row.appendChild(
        h("span", { class: "n", text: tokenText(week.tokens), title: tokensTip(week.tokens) })
      );
      row.appendChild(
        h("span", { class: "n", title: valueTip(week.values, "subscription") }, [
          amountNode(week.values.subscription, week.values.currency),
        ])
      );
      row.appendChild(
        h("span", { class: "n", title: valueTip(week.values, "api") }, [
          amountNode(week.values.api, week.values.apiCurrency),
        ])
      );
      row.appendChild(h("span", { class: "n", text: String(week.sessions) }));
      row.appendChild(
        h("span", {
          class: "stats-act-top",
          text: week.top ? `${week.top[0]} · ${week.top[1]}` : "—",
          title: week.top ? `${week.top[0]} · ${week.top[1]}` : "no output tokens this week",
        })
      );
      view.appendChild(row);
    }
    return view;
  }

  function largestView() {
    const view = h("div", { class: "stats-act-table" });
    view.style.setProperty(
      "--stats-act-columns",
      "minmax(0, 1fr) 96px 48px 58px 76px 70px 46px"
    );
    const head = h("div", { class: "stats-act-row stats-act-row-head" });
    for (const label of ["Session", "Model", "Hours", "Tokens", "Subscription", "API", "Agents"]) {
      head.appendChild(h("span", { class: label === "Session" ? "" : "n", text: label }));
    }
    view.appendChild(head);
    if (data.largest.length === 0) {
      view.appendChild(emptyLine("no session in the range"));
      return view;
    }
    for (const session of data.largest) {
      const row = h("div", { class: "stats-act-row" });
      const name = h("span", { class: "stats-act-session" });
      name.appendChild(h("span", { class: "stats-act-session-label", text: session.label }));
      name.appendChild(
        h("span", {
          class: "stats-act-session-sub",
          text: `${session.project} · ${session.host}`,
        })
      );
      row.appendChild(name);
      row.appendChild(
        h("span", {
          text:
            session.model === null || session.model === undefined
              ? "—"
              : modelName(session.model),
          title:
            session.model === null || session.model === undefined
              ? "no model with output tokens in this session"
              : undefined,
        })
      );
      row.appendChild(h("span", { class: "n", text: hoursText(session.workedHours) }));
      row.appendChild(
        h("span", { class: "n", text: tokenText(session.tokens), title: tokensTip(session.tokens) })
      );
      row.appendChild(
        h("span", { class: "n", title: valueTip(session.values, "subscription") }, [
          amountNode(session.values.subscription, session.values.currency),
        ])
      );
      row.appendChild(
        h("span", { class: "n", title: valueTip(session.values, "api") }, [
          amountNode(session.values.api, session.values.apiCurrency),
        ])
      );
      const agentsKnown = session.agents !== null && session.agents !== undefined;
      row.appendChild(
        h("span", {
          class: "n",
          text: agentsKnown ? String(session.agents) : "—",
          title: agentsKnown
            ? undefined
            : "the session is still running: its agents are counted when it ends",
        })
      );
      view.appendChild(row);
    }
    return view;
  }

  function paintTab() {
    clear(tabBody);
    for (const button of tabSwitch.childNodes) {
      const on = button.dataset && button.dataset.tab === state.tab;
      button.classList.toggle("stats-tab-on", on);
      button.setAttribute("aria-pressed", on ? "true" : "false");
    }
    tabBody.appendChild(state.tab === "weeks" ? weeksView() : largestView());
  }

  tabSwitch.appendChild(tabButton("weeks", "Per week"));
  tabSwitch.appendChild(tabButton("largest", "Largest sessions"));
  paintTab();
}
