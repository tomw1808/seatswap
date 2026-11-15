// The Forecast page (spec §3.4; step-c3 Task 8, part 3): the four numbers and
// the plan's sentence, each account's forecast, and the lanes of the next seven
// days. The forecast is the live plan of every account and every host, so the
// page asks `stats_forecast` without a query and the range, the chips and the
// host do not change it. The page formats and draws the struct only.

import {
  WEEKDAYS,
  accountName,
  accountNameText,
  accountView,
  amountNode,
  clear,
  emptyLine,
  errorMessage,
  h,
  hoursText,
  isColor,
  kpi,
  percentText,
  weekdayIndex,
  weekdayTimeText,
} from "../../common.js";
import { lanes } from "../../charts.js";

// The one error the page shows as a plain line: the plan does not exist before
// every account has a reading. Every other failure stays the area's banner.
const NO_PLAN = "the forecast needs a reading of every account first";

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

// A struct percent as a CSS position: a value outside 0..100 is an error, never
// silently clamped.
function position(value, what) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 100) {
    throw new Error(`${what} ${String(value)} is not a percent`);
  }
  return `${value}%`;
}

// Why an expected percent is missing: the account is still learning its rates,
// or the plan has no entry for it (`forecast.rs` 74-77 and 234-242).
function expectedReason(learning) {
  return learning ? "its rates are not known yet" : "the plan has no entry for it";
}

function learningLine(env, ids) {
  const line = h("span", {}, ["still learning the rates of "]);
  for (const [index, id] of ids.entries()) {
    if (index > 0) line.appendChild(document.createTextNode(", "));
    line.appendChild(accountName(env.accounts, id));
  }
  return line;
}

export async function show(container, env) {
  let data;
  try {
    data = await env.call("stats_forecast");
  } catch (error) {
    const text = errorMessage(error);
    if (error && error.name === "EmptyRange") throw error;
    if (text !== NO_PLAN) throw error;
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
  const kpis = h("div", { class: "stats-kpis" });
  kpis.appendChild(
    kpi(
      "Expire this week",
      amountNode(data.kpis.expirePlanned, currency),
      "with auto-burn's plan, as it is set now",
      "Expire this week | the money of the percent each account is planned to leave unused at its reset"
    )
  );
  kpis.appendChild(
    kpi(
      "Only when full",
      amountNode(data.kpis.expireOnlyWhenFull, currency),
      "if switching waits until an account is at 100 %",
      "Only when full | the same money when every switch waits for a full account"
    )
  );
  kpis.appendChild(
    kpi(
      "Blocked",
      hoursText(data.kpis.blockedHours),
      "working hours with no usable account in the next 7 days",
      "Blocked | the hours the plan has no usable account, in your usual working hours"
    )
  );
  kpis.appendChild(
    kpi(
      "Switches",
      String(data.kpis.switches),
      `${data.kpis.switchesOnlyWhenFull} with “only when full”`,
      "Switches | the switches the plan makes in the next 7 days"
    )
  );
  const sentence = kpi(
    "All accounts together",
    data.sentence,
    data.learning.length > 0 ? learningLine(env, data.learning) : null,
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
    const learning = data.learning.includes(account.id);
    const row = h("div", { class: "stats-fc-row" });

    const name = h("div", { class: "stats-fc-account" }, [
      accountName(env.accounts, account.id),
    ]);
    name.appendChild(
      h("span", {
        class: "stats-plan",
        text: view && typeof view.plan === "string" && view.plan !== "" ? view.plan : "—",
        title:
          view && typeof view.plan === "string" && view.plan !== ""
            ? undefined
            : "no plan stored for this account",
      })
    );
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
        : `expected at its reset: — (${expectedReason(learning)})`,
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

    const lazy = h("span", { class: "stats-fc-lazy" });
    lazy.appendChild(
      document.createTextNode(
        typeof account.onlyWhenFullExpected === "number"
          ? `only when full: ${percentText(account.onlyWhenFullExpected)} at the reset`
          : `only when full: — (${expectedReason(learning)}) at the reset`
      )
    );
    lazy.appendChild(document.createTextNode(" · "));
    if (typeof account.onlyWhenFullExpires === "number") {
      lazy.appendChild(amountNode(account.onlyWhenFullExpires, currency));
      lazy.appendChild(document.createTextNode(" expires"));
    } else {
      lazy.appendChild(document.createTextNode(`— expires (${expectedReason(learning)})`));
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
      h("span", { class: "t", text: "The plan, next 7 days" }),
      h("span", {
        class: "s",
        text: "one lane per account in its colour · a line is its weekly reset",
      }),
    ])
  );
  const lanesBody = h("div", { class: "stats-forecast-lanes" });
  lanesCard.appendChild(lanesBody);
  root.appendChild(lanesCard);

  const planned = [];
  if (data.blocked.length > 0) {
    planned.push({
      label: "none usable",
      color: "var(--danger)",
      blocks: data.blocked,
      blockTips: data.blocked.map(([from, to]) => `No account usable | ${spanText(from, to)}`),
      className: "stats-fc-none",
    });
  }
  for (const lane of data.lanes) {
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
  if (planned.length === 0) {
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
      text: "The plan uses your usual working hours and each account's measured rates; it is recomputed at every reading.",
    })
  );
}
