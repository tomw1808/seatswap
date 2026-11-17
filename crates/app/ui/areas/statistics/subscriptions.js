// The Subscriptions page (spec §3.3; step-c3 Task 8, part 3): the subscription
// periods as bars, one row per subscription with its verdict, and the "what if"
// replay without the chosen row's account. The page formats and draws the
// struct; every sum, verdict and sentence comes from the Rust page.

import {
  accountName,
  accountNameText,
  accountView,
  amountNode,
  amountTip,
  banner,
  clear,
  durationText,
  emptyLine,
  errorMessage,
  h,
  isColor,
  kpi,
  percentText,
  shortDate,
  tokens,
  valueTip,
  weekdayIndex,
} from "../../common.js";
import { bars, lines, timeAxis } from "../../charts.js";

const VERDICT_LABELS = {
  mostlyUnused: "Mostly unused",
  regularlyFull: "Regularly full",
  wellUsed: "Well used",
  tooEarly: "Too early",
};

// What a row without a finished weekly window in the range is missing.
const NO_WEEK_YET = "no finished week yet in this range";

function verdictTag(verdict) {
  const label = VERDICT_LABELS[verdict];
  if (label === undefined) throw new Error(`unknown verdict ${JSON.stringify(verdict)}`);
  const kind =
    verdict === "wellUsed"
      ? "good"
      : verdict === "mostlyUnused"
        ? "warn"
        : verdict === "tooEarly"
          ? "plain"
          : "focus";
  return h("span", { class: `stats-tag stats-tag-${kind}`, text: label });
}

export async function show(container, env) {
  const data = await env.call("stats_subscriptions");
  clear(container);

  const currency = env.currency;
  const state = { token: 0 };
  const rowNodes = new Map();

  const root = h("div", { class: "g stats-subscriptions" });
  container.appendChild(root);

  // -------------------------------------------------------------------------
  // subscriptions over time
  // -------------------------------------------------------------------------
  const periodsCard = h("div", { class: "hcard stats-periods-card" });
  periodsCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Subscriptions over time" }),
      h("span", { class: "s", text: "one bar per subscription period, from the week it started" }),
    ])
  );
  const periodsBody = h("div", { class: "stats-periods-body" });
  periodsCard.appendChild(periodsBody);
  root.appendChild(periodsCard);

  if (data.periods.length === 0) {
    periodsBody.appendChild(emptyLine("no plan yet"));
  } else {
    const starts = data.periods.map((period) => Date.parse(period.from));
    const ends = data.periods.map((period) =>
      period.to === null ? Date.now() : Date.parse(period.to)
    );
    for (const time of starts.concat(ends)) {
      if (Number.isNaN(time)) throw new Error("a subscription period has no readable time");
    }
    const start = Math.min(...starts);
    const end = Math.max(Date.now(), ...ends);
    if (!(end > start)) throw new Error("the subscription periods do not span any time");
    const xOf = (time) => ((time - start) / (end - start)) * 100;

    const axis = h("div", { class: "stats-gantt-axis" });
    axis.appendChild(h("span", { class: "stats-gantt-label" }));
    const axisTrack = h("div", { class: "stats-gantt-track" });
    const ticks = [];
    const firstTick = new Date(start);
    firstTick.setHours(0, 0, 0, 0);
    while (firstTick.getTime() < start) firstTick.setDate(firstTick.getDate() + 1);
    while (weekdayIndex(firstTick) !== 0) firstTick.setDate(firstTick.getDate() + 1);
    // Every Monday is a tick; the chart's axis fit shows those that fit. A
    // week is counted in calendar days, so a clock change keeps the Monday.
    for (const tick = new Date(firstTick); tick.getTime() <= end; tick.setDate(tick.getDate() + 7)) {
      const time = tick.getTime();
      ticks.push({ at: time, text: shortDate(new Date(time)), left: `${xOf(time)}%` });
    }
    timeAxis(axisTrack, ticks);
    axis.appendChild(axisTrack);
    periodsBody.appendChild(axis);

    for (const period of data.periods) {
      const view = accountView(env.accounts, period.id);
      const color = view && isColor(view.color) ? view.color : "var(--muted)";
      const from = Date.parse(period.from);
      const to = period.to === null ? end : Date.parse(period.to);
      const left = xOf(from);
      const width = Math.max(0.6, xOf(to) - left);
      const line = h("div", { class: "stats-gantt-row" });
      const label = h("span", { class: "stats-gantt-label" }, [
        accountName(env.accounts, period.id),
      ]);
      const plan = period.plan === null || period.plan === undefined ? "—" : period.plan;
      const price = amountTip(period.price, currency);
      label.appendChild(
        h("span", { class: "stats-plan" }, [
          `${plan} · `,
          amountNode(period.price, currency),
          " / month",
        ])
      );
      // Each bar names its own period's start: an account whose plan changed has
      // two periods, and their labels must not both show the account's first one.
      label.appendChild(
        h("span", {
          class: "stats-plan",
          text:
            period.to === null
              ? `since ${shortDate(period.from)}`
              : `from ${shortDate(period.from)}`,
        })
      );
      line.appendChild(label);
      const track = h("div", { class: "stats-gantt-track" });
      const bar = h("i", {
        style: { left: `${left}%`, width: `${width}%`, background: color },
      });
      bar.title =
        period.to === null
          ? `${plan} · ${price} — since ${shortDate(period.from)}, still running`
          : `${plan} · ${price} — ${shortDate(period.from)} to ${shortDate(period.to)}`;
      track.appendChild(bar);
      line.appendChild(track);
      periodsBody.appendChild(line);
    }
  }

  // -------------------------------------------------------------------------
  // one row per subscription
  // -------------------------------------------------------------------------
  const rowsCard = h("div", { class: "hcard stats-sub-rows-card" });
  rowsCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Each subscription" }),
      h("span", { class: "s", text: "a click on a row runs “what if without it”" }),
      h("span", { class: "sp" }),
      h("span", { class: "x11 mf", text: "prices as entered per account" }),
    ])
  );
  const rowsBody = h("div", { class: "stats-sub-rows-body" });
  rowsCard.appendChild(rowsBody);
  root.appendChild(rowsCard);

  const COLUMNS = [
    "Account",
    "Plan",
    "Price / month",
    "Since",
    "Weeks",
    "Average used",
    "",
    "Expired",
    "Full",
    "Blocked on it",
    "At API prices · × paid",
    "Verdict",
  ].map((label, index) =>
    h("span", {
      class: index === 5 || index === 6 ? "stats-sub-cell-wide" : "n",
      text: label,
    })
  );
  const head = h("div", { class: "stats-subrow stats-subrow-head" });
  for (const cell of COLUMNS) head.appendChild(cell);
  rowsBody.appendChild(head);

  if (data.rows.length === 0) {
    rowsBody.appendChild(emptyLine("no plan yet"));
  }
  for (const row of data.rows) {
    const view = accountView(env.accounts, row.id);
    const color = view && isColor(view.color) ? view.color : "var(--muted)";
    const node = h("div", { class: "stats-subrow", role: "button", tabindex: "0" });
    node.appendChild(
      h("span", { class: "stats-sub-cell-account" }, [accountName(env.accounts, row.id)])
    );
    node.appendChild(
      h("span", { text: row.plan === null || row.plan === undefined ? "—" : row.plan })
    );
    node.appendChild(h("span", { class: "n" }, [amountNode(row.price, currency)]));
    node.appendChild(h("span", { class: "mf", text: shortDate(row.since) }));
    node.appendChild(h("span", { class: "n", text: String(row.weeks) }));
    const average = h("span", { class: "stats-sub-cell-wide" });
    if (row.weeks === 0) {
      // No finished week: the bar's place says so instead of an empty bar.
      average.appendChild(h("span", { class: "mf", text: NO_WEEK_YET }));
    } else {
      const bar = h("span", { class: "stats-ubar" });
      if (typeof row.avgUsed === "number") {
        if (!Number.isFinite(row.avgUsed) || row.avgUsed < 0) {
          throw new Error(`the average used ${String(row.avgUsed)} is not a percent`);
        }
        // A window above 100 % exists (C3-2): the bar is full, the number beside
        // it is written as it is.
        bar.appendChild(
          h("i", { style: { width: `${Math.min(100, row.avgUsed)}%`, background: color } })
        );
      }
      average.appendChild(bar);
    }
    node.appendChild(average);
    node.appendChild(
      h("span", {
        class: "n",
        text: typeof row.avgUsed === "number" ? percentText(row.avgUsed) : "—",
      })
    );
    const hasExpired =
      typeof row.expiredPercent === "number" ||
      (row.expired !== null && row.expired !== undefined);
    if (hasExpired) {
      const expired = h("span", { class: "n" });
      expired.appendChild(
        document.createTextNode(
          `${typeof row.expiredPercent === "number" ? percentText(row.expiredPercent) : "—"} · `
        )
      );
      expired.appendChild(amountNode(row.expired, currency));
      node.appendChild(expired);
    } else {
      node.appendChild(h("span", { class: "n" }, [amountNode(row.expired, currency)]));
    }
    node.appendChild(
      h("span", { class: "n", text: row.weeks === 0 ? "—" : `${row.fullWeeks} of ${row.weeks}` })
    );
    node.appendChild(
      h("span", {
        class: "n",
        text: durationText(Math.round(row.blockedHoursOnIt * 3600)),
      })
    );
    const times = typeof row.timesPaid === "number" ? row.timesPaid.toFixed(1) : "—";
    const timesReason =
      typeof row.timesPaidMissing === "string" ? ` (${row.timesPaidMissing})` : "";
    const apiValue = amountNode(row.values.api, row.values.apiCurrency);
    apiValue.title = valueTip(row.values, "api", "in its finished weeks");
    const api = h("span", {
      class: "n",
      title: `${apiValue.title} · × paid: ${times}${timesReason}`,
    });
    api.appendChild(apiValue);
    api.appendChild(document.createTextNode(` · × ${times}`));
    node.appendChild(api);
    const verdict = verdictTag(row.verdict);
    if (row.verdict === "tooEarly") {
      verdict.title = `a verdict needs 4 finished weeks; this account has ${row.weeks} in the range`;
    }
    node.appendChild(h("span", {}, [verdict]));
    node.addEventListener("click", () => void selectRow(row));
    node.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      void selectRow(row);
    });
    rowNodes.set(row.id, node);
    rowsBody.appendChild(node);
  }

  if (data.withoutPrice.length > 0) {
    const note = h("p", { class: "stats-note" }, ["No price for "]);
    for (const [index, id] of data.withoutPrice.entries()) {
      if (index > 0) note.appendChild(document.createTextNode(", "));
      note.appendChild(accountName(env.accounts, id));
    }
    note.appendChild(document.createTextNode(": left out of the money sums."));
    rowsCard.appendChild(note);
  }

  // -------------------------------------------------------------------------
  // what if without it
  // -------------------------------------------------------------------------
  const whatCard = h("div", { class: "hcard stats-whatif-card" });
  const whatHead = h("div", { class: "ch" });
  const whatBody = h("div", { class: "stats-whatif-body" });
  whatCard.appendChild(whatHead);
  whatCard.appendChild(whatBody);
  root.appendChild(whatCard);

  function renderWhatHead(row) {
    clear(whatHead);
    whatHead.appendChild(h("span", { class: "t", text: "What if: the range without" }));
    whatHead.appendChild(accountName(env.accounts, row.id));
    whatHead.appendChild(h("span", { class: "sp" }));
    whatHead.appendChild(
      h("span", {
        class: "x11 mf",
        text: "replayed with the same working hours, switching only at 100 %",
      })
    );
  }

  function renderWhatIf(row, what) {
    clear(whatBody);
    const kpis = h("div", { class: "stats-kpis stats-whatif-kpis" });
    kpis.appendChild(
      kpi(
        "Blocked",
        what.blockedHours === null
          ? "—"
          : durationText(Math.round(what.blockedHours * 3600)),
        `instead of ${durationText(Math.round(what.blockedHoursWas * 3600))}`,
        "Blocked | the hours without this account, replayed with the same working hours; never below the recorded hours",
        what.blockedHours === null ? null : "stats-kpi-value-bad"
      )
    );
    kpis.appendChild(
      kpi(
        "Expired unused",
        amountNode(what.expired, currency),
        h("span", {}, ["instead of ", amountNode(what.expiredWas, currency)]),
        "Expired unused | the money the range's weekly windows left unused without this account"
      )
    );
    kpis.appendChild(
      kpi(
        "Saved on the price",
        amountNode(what.savedPrice, currency),
        row.weeks === 0 ? NO_WEEK_YET : "in this range",
        "Saved on the price | the paid money of this account's weeks of the range"
      )
    );
    const sentence = kpi("In one sentence", what.sentence, null, what.sentence);
    sentence.classList.add("stats-kpi-sentence");
    kpis.appendChild(sentence);
    whatBody.appendChild(kpis);

    const charts = h("div", { class: "stats-whatif-charts" });

    const blockedBlock = h("div", { class: "stats-whatif-chart" });
    blockedBlock.appendChild(
      h("p", { class: "stats-detail-label", text: "Blocked hours per week" })
    );
    if (what.weekly.length > 0) {
      const name = accountNameText(env.accounts, row.id);
      blockedBlock.appendChild(
        bars({
          categories: what.weekly.map(([week]) => shortDate(week)),
          series: [
            {
              name: "as it was",
              color: "var(--muted)",
              values: what.weekly.map(([, was]) => was),
              unit: "h",
              tip: (category, value) =>
                `${category} | as it was: ${durationText(Math.round(value * 3600))} blocked`,
            },
            {
              name: h("span", {}, ["without ", accountName(env.accounts, row.id)]),
              color: "var(--danger)",
              values: what.weekly.map(([, , without]) => without),
              unit: "h",
              tip: (category, value) =>
                `${category} | without ${name}: ${durationText(Math.round(value * 3600))} blocked`,
            },
          ],
        })
      );
    } else {
      // A refused replay has no weeks; the sentence beside the numbers says why.
      blockedBlock.appendChild(
        emptyLine(
          what.blockedHours === null
            ? "not replayed yet, so nothing to compare"
            : "no week in the range"
        )
      );
    }
    charts.appendChild(blockedBlock);

    const tokensBlock = h("div", { class: "stats-whatif-chart stats-whatif-tokens" });
    tokensBlock.appendChild(
      h("p", { class: "stats-detail-label", text: "Output tokens per percent of the week" })
    );
    if (row.tokensPerPercent.length === 0) {
      tokensBlock.appendChild(
        emptyLine(row.weeks === 0 ? NO_WEEK_YET : "no week with use in this range")
      );
    } else {
      const view = accountView(env.accounts, row.id);
      tokensBlock.appendChild(
        lines({
          series: [
            {
              name: "tokens per percent",
              color: view && isColor(view.color) ? view.color : "var(--focus)",
              points: row.tokensPerPercent,
              tip: (time, value) =>
                `${shortDate(new Date(time))}: ${tokens(value)} output tokens per percent of the week`,
            },
          ],
          yFormat: (value) => tokens(value),
        })
      );
    }
    charts.appendChild(tokensBlock);
    whatBody.appendChild(charts);
  }

  async function selectRow(row) {
    const token = ++state.token;
    for (const [id, node] of rowNodes) {
      node.classList.toggle("stats-subrow-selected", id === row.id);
    }
    renderWhatHead(row);
    clear(whatBody);
    whatBody.appendChild(h("p", { class: "stats-empty", text: "replaying…" }));
    let what;
    try {
      what = await env.call("stats_what_if", { without: row.id });
    } catch (error) {
      if (token !== state.token) return;
      clear(whatBody);
      if (error && error.name === "EmptyRange") {
        whatBody.appendChild(h("p", { class: "stats-empty", text: errorMessage(error) }));
      } else {
        whatBody.appendChild(banner("error", "⚠", errorMessage(error)));
      }
      return;
    }
    if (token !== state.token) return;
    renderWhatIf(row, what);
  }

  if (data.rows.length > 0) await selectRow(data.rows[0]);
}
