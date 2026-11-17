// The Forecast page (spec §3.4; step-c3 Task 8, part 3): the four numbers and
// the plan's sentence, each account's forecast, and the lanes of the next seven
// days. The forecast is the live plan of every account and every host, so the
// page asks `stats_forecast` without a query and the range, the chips and the
// host do not change it. The page formats and draws the struct only.

import {
  WEEKDAYS,
  accountName,
  accountNameText,
  accountPlan,
  accountView,
  amountNode,
  clear,
  durationText,
  emptyLine,
  errorMessage,
  h,
  isColor,
  kpi,
  percentText,
  shortPercent,
  weekdayIndex,
  weekdayTimeText,
} from "../../common.js";
import { lanes } from "../../charts.js";

// The one error the page shows as a plain line: the plan does not exist before
// every account has a reading (`window.rs`'s `NO_PLAN`). It may go on after a
// colon with the accounts that have none yet. Every other failure stays the
// area's banner.
const NO_PLAN = "the forecast needs a reading of every account first";

function isNoPlan(text) {
  return text === NO_PLAN || text.startsWith(`${NO_PLAN}: `);
}

function dayTicks(from, to) {
  const ticks = [];
  const cursor = new Date(from);
  cursor.setHours(0, 0, 0, 0);
  cursor.setDate(cursor.getDate() + 1);
  while (cursor.getTime() < to) {
    ticks.push({
      at: new Date(cursor.getTime()),
      label: `${WEEKDAYS[weekdayIndex(cursor)]} ${cursor.getDate()}`,
    });
    cursor.setDate(cursor.getDate() + 1);
  }
  return ticks;
}

function spanText(from, to) {
  return `${weekdayTimeText(from)} to ${weekdayTimeText(to)}`;
}

// A struct percent as a CSS position. A value below 0 is an error. A value
// above 100 is a real window (C3-2: a window can expire nothing): its bar is
// drawn full and the number beside it is written as it is, never changed here.
function position(value, what) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`${what} ${String(value)} is not a percent`);
  }
  return `${Math.min(value, 100)}%`;
}

// Why an expected percent is missing: the account's rates are not known yet
// (`forecast.rs`'s `unknown`), or the plan has no entry for it.
function expectedReason(unknown) {
  return unknown ? "its rates are not known yet" : "the plan has no entry for it";
}

export async function show(container, env) {
  let data;
  try {
    data = await env.call("stats_forecast");
  } catch (error) {
    const text = errorMessage(error);
    if (error && error.name === "EmptyRange") throw error;
    if (!isNoPlan(text)) throw error;
    clear(container);
    container.appendChild(h("p", { class: "stats-empty stats-forecast-noplan", text }));
    return;
  }

  clear(container);
  const currency = env.currency;

  const note = h("p", { class: "stats-note" }, [
    "The forecast covers every account and host.",
    data.basisWeeks > 0 ? ` It reads ${data.basisWeeks} weeks of history.` : "",
  ]);
  container.appendChild(note);

  const root = h("div", { class: "g stats-forecast" });
  container.appendChild(root);

  // -------------------------------------------------------------------------
  // the four numbers and the sentence
  // -------------------------------------------------------------------------
  const kpisCard = h("div", { class: "hcard" });
  if (!Number.isFinite(data.kpis.switchesPerDay)) {
    throw new Error(`the forecast's switches a day is not a number: ${JSON.stringify(data.kpis.switchesPerDay)}`);
  }
  const kpis = h("div", { class: "stats-kpis" });
  kpis.appendChild(
    kpi(
      "Expected to expire this week",
      amountNode(data.kpis.expirePlanned, currency),
      "with auto-burn's plan, as it is set now",
      "Expected to expire this week | the money of the percent each account is planned to leave unused at its reset"
    )
  );
  kpis.appendChild(
    kpi(
      "With “only when full”",
      amountNode(data.kpis.expireOnlyWhenFull, currency),
      "if switching waits until an account is at 100 %",
      "With “only when full” | the same money when every switch waits for a full account"
    )
  );
  kpis.appendChild(
    kpi(
      "Hours with no usable account",
      durationText(Math.round(data.kpis.blockedHours * 3600)),
      "working hours with no usable account in the next 7 days",
      "Hours with no usable account | the hours the plan has no usable account, in your usual working hours"
    )
  );
  kpis.appendChild(
    kpi(
      "Switches planned",
      String(data.kpis.switches),
      `about ${shortPercent(data.kpis.switchesPerDay)} a day · ${data.kpis.switchesOnlyWhenFull} with “only when full”`,
      "Switches planned | the switches the plan makes in the next 7 days"
    )
  );
  const sentence = kpi(
    "All accounts together",
    data.sentence,
    null,
    data.sentence
  );
  sentence.classList.add("stats-kpi-sentence");
  kpis.appendChild(sentence);
  kpisCard.appendChild(kpis);
  root.appendChild(kpisCard);

  // -------------------------------------------------------------------------
  // each account
  // -------------------------------------------------------------------------
  const accountsCard = h("div", { class: "hcard" });
  accountsCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "This week, per account" }),
      h("span", {
        class: "s",
        text: "filled = now · a line = expected at its reset · a dashed line = the lowest of the last 8 weeks",
      }),
    ])
  );
  const accountsBody = h("div", { class: "stats-forecast-accounts" });
  accountsCard.appendChild(accountsBody);
  root.appendChild(accountsCard);

  if (data.accounts.length === 0) {
    accountsBody.appendChild(emptyLine("no plan yet"));
  }
  for (const account of data.accounts) {
    const view = accountView(env.accounts, account.id);
    const color = view && isColor(view.color) ? view.color : "var(--muted)";
    const unknown = data.unknown.includes(account.id);
    const row = h("div", { class: "stats-fc-row" });

    const name = h("div", { class: "stats-fc-account" }, [
      accountName(env.accounts, account.id),
      accountPlan(env.accounts, account.id),
    ]);
    row.appendChild(name);

    const bar = h("div", { class: "stats-fc-bar" });
    bar.appendChild(
      h("i", {
        class: "stats-fc-now",
        style: { width: position(account.now, "now"), background: color },
      })
    );
    if (typeof account.expected === "number") {
      bar.appendChild(
        h("i", {
          class: "stats-fc-expected",
          style: { left: position(account.expected, "expected") },
        })
      );
    }
    if (typeof account.low === "number") {
      bar.appendChild(
        h("i", {
          class: "stats-fc-low",
          style: { left: position(account.low, "low") },
        })
      );
    }
    const tip = [
      `${accountNameText(env.accounts, account.id)}`,
      `now: ${percentText(account.now)}`,
      typeof account.expected === "number"
        ? `expected at its reset: ${percentText(account.expected)}`
        : `expected at its reset: — (${expectedReason(unknown)})`,
      typeof account.low === "number"
        ? `lowest of the last 8 weeks: ${percentText(account.low)}`
        : "lowest of the last 8 weeks: — (fewer than four finished windows)",
      `resets: ${weekdayTimeText(account.resetsAt)}`,
    ].join(" | ");
    bar.title = tip;
    row.appendChild(bar);

    row.appendChild(
      h("span", { class: "stats-fc-now-text", text: percentText(account.now) })
    );
    row.appendChild(h("span", { class: "stats-fc-arrow", text: "→" }));
    row.appendChild(
      h("span", {
        class: "stats-fc-expected-text",
        text: typeof account.expected === "number" ? percentText(account.expected) : "—",
      })
    );

    const times = [];
    if (account.fullAt !== null && account.fullAt !== undefined) {
      times.push(`full on ${weekdayTimeText(account.fullAt)}`);
    }
    times.push(`resets ${weekdayTimeText(account.resetsAt)}`);
    row.appendChild(h("span", { class: "stats-fc-times", text: times.join(" · ") }));

    // Without an expected percent there is no money either (`forecast.rs`):
    // the reason is said once. With a percent, a missing amount means the
    // account's price could not be valued (`forecast.rs`'s `price_at`).
    const lazy = h("span", { class: "stats-fc-lazy" });
    if (typeof account.onlyWhenFullExpected === "number") {
      lazy.appendChild(
        document.createTextNode(
          `only when full: ${percentText(account.onlyWhenFullExpected)} at the reset · `
        )
      );
      if (typeof account.onlyWhenFullExpires === "number") {
        lazy.appendChild(amountNode(account.onlyWhenFullExpires, currency));
        lazy.appendChild(document.createTextNode(" expires"));
      } else {
        lazy.appendChild(document.createTextNode("— expires (its price is not known)"));
      }
    } else {
      lazy.appendChild(
        document.createTextNode(`only when full: — (${expectedReason(unknown)})`)
      );
    }
    row.appendChild(lazy);

    accountsBody.appendChild(row);
  }

  // -------------------------------------------------------------------------
  // the plan's lanes
  // -------------------------------------------------------------------------
  const lanesCard = h("div", { class: "hcard stats-forecast-lanes-card" });
  lanesCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Burn plan, next 7 days" }),
      h("span", {
        class: "s",
        text: "one lane per account in its colour · a line is its weekly reset",
      }),
    ])
  );
  const lanesBody = h("div", { class: "stats-forecast-lanes" });
  lanesCard.appendChild(lanesBody);
  root.appendChild(lanesCard);

  // The rows come in Rust's order: every account's lane, then the "none usable"
  // row, which is always last (`forecast.rs`'s `laneOrder`).
  const order = data.laneOrder;
  if (!Array.isArray(order) || order[order.length - 1] !== null) {
    throw new Error("the forecast's lane order does not end with the 'none usable' row");
  }
  if (order.filter((id) => id === null).length !== 1) {
    throw new Error("the forecast's lane order has more than one 'none usable' row");
  }
  const lanesById = new Map(data.lanes.map((lane) => [lane.id, lane]));
  const seen = new Set();
  const planned = [];
  for (const id of order) {
    if (id === null) {
      planned.push({
        label: "none usable",
        color: "var(--danger)",
        blocks: data.blocked,
        blockTips: data.blocked.map(([from, to]) => `No account usable | ${spanText(from, to)}`),
        className: "stats-fc-none",
      });
      continue;
    }
    if (seen.has(id)) {
      throw new Error(`the forecast's lane order names account ${JSON.stringify(id)} twice`);
    }
    seen.add(id);
    const lane = lanesById.get(id);
    if (!lane) {
      throw new Error(`the forecast has no lane for account ${JSON.stringify(id)}`);
    }
    const view = accountView(env.accounts, lane.id);
    const color = view && isColor(view.color) ? view.color : "var(--muted)";
    planned.push({
      label: accountName(env.accounts, lane.id),
      color,
      blocks: lane.blocks,
      blockTips: lane.blocks.map(
        ([from, to]) => `${accountNameText(env.accounts, lane.id)} in use | ${spanText(from, to)}`
      ),
      marks: lane.resets,
      markTips: lane.resets.map(
        (reset) =>
          `${accountNameText(env.accounts, lane.id)} weekly reset | ${weekdayTimeText(reset)}`
      ),
    });
  }
  if (seen.size !== data.lanes.length) {
    throw new Error("the forecast's lane order leaves out a lane");
  }
  if (data.lanes.length === 0) {
    lanesBody.appendChild(emptyLine("no plan yet"));
  } else {
    lanesBody.appendChild(
      lanes({
        lanes: planned,
        from: data.from,
        to: data.to,
        ticks: dayTicks(new Date(data.from), new Date(data.to)),
      })
    );
  }
  lanesCard.appendChild(
    h("p", {
      class: "stats-note",
      text: "The plan uses your usual working hours and each account's rates, measured or assumed from its plan; it is recomputed at every reading.",
    })
  );
}
