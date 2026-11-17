// The Weeks page (spec §3.2): every weekly window as a grid, the last two weeks
// as curves, the summary of a calendar week and the detail of one window. The
// page formats and draws the struct; it adds no rule of its own.

import {
  accountName,
  accountNameText,
  accountView,
  amountNode,
  amountTip,
  banner,
  clear,
  contrastText,
  dateText,
  dateTimeText,
  durationText,
  emptyLine,
  errorMessage,
  h,
  isColor,
  percentText,
  shortDate,
  shortPercent,
  showTokens,
  switchText,
  tokens,
  valueTip,
} from "../../common.js";
import { grid, lines } from "../../charts.js";

function accountLabel(row, env) {
  const label = h("span", { class: "stats-row-label" }, [
    accountName(env.accounts, row.id),
  ]);
  if (row.plan) label.appendChild(h("span", { class: "stats-plan", text: row.plan }));
  return label;
}

// The earliest time of a curve's points, for the history mark's "before the
// chart's first column" rule. A point the chart cannot read is left to the
// chart, which names it; here only the readable times count.
function firstPointTime(points) {
  let first = null;
  for (const point of points) {
    const time = Date.parse(String(point[0]));
    if (Number.isNaN(time)) continue;
    if (first === null || time < first) first = time;
  }
  return first;
}

// Item 31: on a chart of subscription values, a mark at the history's start,
// "history from <date> · no subscription value before". Before the chart's
// first column there is nothing to mark; absent or null, no mark.
function historyMark(env, points) {
  if (typeof env.historyStart !== "string" || env.historyStart === "") return [];
  const start = Date.parse(env.historyStart);
  if (Number.isNaN(start)) {
    throw new Error(`the history start ${JSON.stringify(env.historyStart)} is not a time`);
  }
  const first = firstPointTime(points);
  if (first === null || start < first) return [];
  return [
    {
      at: env.historyStart,
      label: `history from ${dateText(env.historyStart)} · no subscription value before`,
    },
  ];
}

export async function show(container, env) {
  const data = await env.call("stats_weeks");
  if (typeof data.now !== "string" || Number.isNaN(Date.parse(data.now))) {
    throw new Error("stats_weeks returned no time for the now mark");
  }
  clear(container);

  const state = {
    summaryWeek: null,
    summary: null,
    window: null,
    detailError: null,
    detailEmpty: false,
    detailToken: 0,
  };
  const root = h("div", { class: "g stats-weeks" });
  const left = h("div", { class: "stats-weeks-left" });
  const right = h("div", { class: "hcard stats-weeks-detail" });
  root.appendChild(left);
  root.appendChild(right);
  container.appendChild(root);

  const runningIndex = data.running.findIndex((running) => running === true);
  const lastIsCurrent =
    runningIndex !== -1 && runningIndex === data.weekStarts.length - 1;

  function renderDetailError() {
    if (state.detailError === null) return null;
    if (state.detailEmpty) {
      return h("p", { class: "stats-empty", text: state.detailError });
    }
    return banner("error", "⚠", state.detailError);
  }

  function summaryCard() {
    const summary = state.summary;
    if (!Array.isArray(summary.perAccountPercent)) {
      throw new Error("the week summary has no perAccountPercent");
    }
    if (typeof summary.switchesTotal !== "number") {
      throw new Error("the week summary has no switchesTotal");
    }
    const reason = summary.moneyMissing;
    const missingTitle = typeof reason === "string" ? reason : undefined;
    // A week whose window still runs shows its use so far; what it leaves
    // unused is only known at its reset.
    const running = summary.running === true;
    const usedPercent = h("span", {
      class: "stats-kpi-value",
      text:
        typeof summary.usedPercentOfPaid === "number"
          ? percentText(summary.usedPercentOfPaid)
          : "—",
      title: missingTitle,
    });
    const usedMoney = amountNode(summary.used, env.currency);
    const expiredMoney = amountNode(summary.expired, env.currency);
    if (missingTitle) {
      usedMoney.title = missingTitle;
      expiredMoney.title = missingTitle;
    }
    const card = h("div", { class: "stats-detail-body" });
    const kpis = h("div", { class: "stats-mini-kpis" });
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: running ? "Used so far" : "Used" }),
        usedPercent,
        h("span", { class: "stats-kpi-sub" }, [usedMoney]),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Expired" }),
        h("span", { class: "stats-kpi-value" }, [expiredMoney]),
        h("span", {
          class: "stats-kpi-sub",
          text:
            typeof summary.expiredPercentOfPaid === "number"
              ? `${percentText(summary.expiredPercentOfPaid)} of value`
              : running
                ? "the week still runs"
                : "—",
          title: running ? "what the running week leaves unused is known at its reset" : missingTitle,
        }),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Blocked" }),
        h("span", {
          class: "stats-kpi-value",
          text: durationText(Math.round(summary.blockedHours * 3600)),
        }),
        h("span", {
          class: "stats-kpi-sub",
          text: `${summary.switchesTotal} ${summary.switchesTotal === 1 ? "switch" : "switches"}`,
        }),
      ])
    );
    card.appendChild(kpis);
    if (summary.perAccountPercent.length === 0 && missingTitle) {
      card.appendChild(h("p", { class: "stats-note", text: missingTitle }));
    }

    const apiValue = amountNode(summary.values.api, summary.values.apiCurrency);
    apiValue.title = valueTip(summary.values, "api", "in this week");
    const beforeTitle = missingTitle || undefined;
    const tokenLine = h("span", {}, [
      showTokens(h("span"), summary.tokens.out),
      " · worth ",
      apiValue,
      " at API prices",
    ]);
    const list = h("dl", { class: "stats-kv" });
    for (const [term, value] of [
      ["Tokens", tokenLine],
      [
        "Week before",
        h("span", {
          text:
            typeof summary.usedChangePercent === "number"
              ? `${summary.usedChangePercent >= 0 ? "+" : ""}${percentText(
                  summary.usedChangePercent
                )} used`
              : running
                ? "— · the week still runs"
                : "—",
          title: beforeTitle,
        }),
      ],
      ["Switches", switchText(summary.switches)],
    ]) {
      list.appendChild(h("dt", { text: term }));
      list.appendChild(h("dd", {}, value));
    }
    card.appendChild(list);

    card.appendChild(h("p", { class: "stats-detail-label", text: "Per account" }));
    if (summary.perAccountPercent.length === 0) {
      card.appendChild(emptyLine("no account week in this week"));
    } else {
      const perAccount = h("div", { class: "stats-chipline" });
      for (const [id, percent] of summary.perAccountPercent) {
        perAccount.appendChild(
          h("span", { class: "stats-account-line" }, [
            accountName(env.accounts, id),
            h("span", { text: percentText(percent) }),
          ])
        );
      }
      card.appendChild(perAccount);
    }

    card.appendChild(h("p", { class: "stats-detail-label", text: "Top projects" }));
    if (summary.topProjects.length === 0) {
      card.appendChild(emptyLine("no top project in this week"));
    } else {
      for (const [name, percent] of summary.topProjects) {
        card.appendChild(
          h("div", { class: "stats-top-project", title: name }, [
            h("span", { class: "stats-top-name", text: name }),
            h("span", { class: "stats-top-bar" }, [
              h("i", { style: { width: `${Math.min(100, Math.max(0, percent))}%` } }),
            ]),
            h("span", { class: "stats-top-value", text: percentText(percent) }),
          ])
        );
      }
    }

    card.appendChild(h("p", { class: "stats-detail-label", text: "Conclusion" }));
    if (summary.conclusion.length === 0) {
      card.appendChild(emptyLine("no conclusion for this week"));
    } else {
      const conclusion = h("ul", { class: "stats-findings" });
      for (const sentence of summary.conclusion) {
        conclusion.appendChild(h("li", { text: sentence }));
      }
      card.appendChild(conclusion);
    }
    return card;
  }

  function windowCard(detail) {
    const card = h("div", { class: "stats-detail-body" });
    const list = h("dl", { class: "stats-kv" });
    const rows = [
      [
        "Used",
        h("span", {}, [
          h("b", { text: percentText(detail.used) }),
          " = ",
          amountNode(detail.usedMoney, env.currency),
        ]),
      ],
      [
        "Expired unused",
        h("span", { class: detail.used < 80 ? "stats-week-bad" : undefined }, [
          percentText(detail.expired),
          " = ",
          amountNode(detail.expiredMoney, env.currency),
        ]),
      ],
      [
        "Reached 100 %",
        detail.reached100At === null
          ? "not reached"
          : `${dateTimeText(detail.reached100At)} · blocked ${durationText(
              Math.round(detail.blockedHoursAfter * 3600)
            )} after that`,
      ],
    ];
    if (detail.sessionWindowsUsed !== null) {
      rows.push([
        "5h windows",
        typeof detail.possibleSessionWindows === "number"
          ? detail.usedMoreThanPossible
            ? `${detail.sessionWindowsUsed} used, more than the about ${detail.possibleSessionWindows} your usual working hours hold`
            : `${detail.sessionWindowsUsed} used of about ${detail.possibleSessionWindows} possible in your working hours`
          : `${detail.sessionWindowsUsed} used`,
      ]);
    }
    rows.push(
      [
        "Switches",
        `${detail.switchesInOut} into or out of it · ${detail.switchesAuto} by auto-burn`,
      ],
      [
        "Last reading",
        `${durationText(detail.lastSampleBeforeEndSecs)} before the reset`,
      ],
      ["Note", detail.note === null || detail.note === undefined ? "—" : detail.note]
    );
    for (const [term, value] of rows) {
      list.appendChild(h("dt", { text: term }));
      list.appendChild(h("dd", {}, value));
    }
    card.appendChild(list);

    card.appendChild(
      h("p", { class: "stats-detail-label", text: "Percent of the week, hour by hour" })
    );
    const chart = h("div", { class: "stats-detail-chart" });
    if (detail.curve.length === 0) {
      chart.appendChild(emptyLine("no reading in this window"));
    } else {
      chart.appendChild(
        lines({
          series: [
            {
              name: "used",
              color: "var(--focus)",
              area: true,
              points: detail.curve,
              tip: (time, value) => `used | ${dateTimeText(new Date(time))}: ${percentText(value)}`,
            },
          ],
          yMax: 100,
          ySuffix: " %",
          hlines: [{ y: 100, label: "limit" }],
        })
      );
    }
    card.appendChild(chart);
    return card;
  }

  function detailHead() {
    const head = h("div", { class: "ch" });
    if (state.window !== null) {
      head.appendChild(accountName(env.accounts, state.window.id));
      head.appendChild(
        h("span", { class: "t", text: `Week from ${dateText(state.window.start)}` })
      );
      const meta = h("span", { class: "stats-plan" });
      if (state.window.plan) meta.appendChild(document.createTextNode(state.window.plan));
      if (typeof state.window.weeklyPrice === "number") {
        if (meta.textContent !== "") meta.appendChild(document.createTextNode(" · "));
        meta.appendChild(amountNode(state.window.weeklyPrice, env.currency));
        meta.appendChild(document.createTextNode(" a week"));
      }
      if (meta.textContent !== "") head.appendChild(meta);
      if (state.window.uncertain) {
        head.appendChild(h("span", { class: "stats-tag stats-tag-warn", text: "uncertain" }));
      }
    } else if (state.summary !== null) {
      head.appendChild(
        h("span", {
          class: "t",
          text: `Summary · week from ${dateText(state.summary.week)}`,
        })
      );
      if (state.summary.running === true) {
        head.appendChild(h("span", { class: "stats-tag stats-tag-running", text: "so far" }));
      }
      const names = h("span", { class: "s" });
      for (const [index, view] of env.accounts.entries()) {
        if (index > 0) names.appendChild(document.createTextNode(" · "));
        names.appendChild(accountName(env.accounts, view.id));
      }
      head.appendChild(names);
    } else {
      head.appendChild(h("span", { class: "t", text: "Summary" }));
    }
    return head;
  }

  function paint() {
    clear(left);
    clear(right);

    const gridCard = h("div", { class: "hcard stats-grid-card" });
    gridCard.appendChild(
      h("div", { class: "ch" }, [
        h("span", { class: "t", text: "Every weekly window" }),
        h("span", {
          class: "s",
          text: "the filled part was used, the hatched part expired · a click on a week shows it",
        }),
      ])
    );
    const gridBody = h("div", { class: "stats-grid-body" });
    const headers = data.weekStarts.map((week, index) => {
      const current = lastIsCurrent && index === data.weekStarts.length - 1;
      return {
        label: current ? "now" : shortDate(week),
        at: week,
        tip: current
          ? `Summary of the running week from ${dateText(week)}, so far`
          : `Summary of the week from ${dateText(week)}`,
        selected: state.summaryWeek === week,
        current,
        onClick: () => void selectSummary(week),
      };
    });
    if (!lastIsCurrent) {
      headers.push({ label: "now", tip: "the running week", current: true });
    }
    const rows = data.rows.map((row) => {
      const view = accountView(env.accounts, row.id);
      const color = view && isColor(view.color) ? view.color : "var(--muted)";
      const cells = row.cells.map((cell, index) => {
        if (!cell) return null;
        const week = data.weekStarts[index];
        const parts =
          Array.isArray(cell.parts) && cell.parts.length > 1
            ? ` (${cell.parts.map((part) => percentText(part)).join(" + ")})`
            : "";
        const tip = `${accountNameText(env.accounts, row.id)} · week from ${dateText(week)} | used ${percentText(
          cell.used
        )}${parts} · expired ${percentText(cell.expired)} = ${amountTip(
          cell.expiredMoney,
          env.currency
        )}${cell.full ? " | full" : ""}${
          cell.uncertain ? " | uncertain: a reading near a reset is missing" : ""
        }`;
        const windowEnds = Array.isArray(cell.windowEnds) ? cell.windowEnds : null;
        return {
          total: cell.used,
          parts: cell.parts,
          expired: cell.expired,
          color,
          textColor: contrastText(color),
          text: shortPercent(cell.used),
          tip,
          full: cell.full,
          uncertain: cell.uncertain,
          onClick: () => void selectWindow(row, cell.end, week),
          partClick:
            windowEnds !== null &&
            windowEnds.length === cell.parts.length &&
            cell.parts.length > 1
              ? (index) => void selectWindow(row, windowEnds[index], week)
              : undefined,
        };
      });
      if (typeof row.nowPercent === "number" && typeof row.nowWeek !== "string") {
        throw new Error(`the running window of ${row.id} names no week it counts in`);
      }
      const nowCell =
        typeof row.nowPercent !== "number"
          ? null
          : {
              total: row.nowPercent,
              color,
              textColor: contrastText(color),
              current: true,
              text: shortPercent(row.nowPercent),
              tip: `${accountNameText(env.accounts, row.id)} · now ${percentText(
                row.nowPercent
              )} | counts in the week from ${dateText(row.nowWeek)} · a click shows that week so far`,
              onClick: () => void selectSummary(row.nowWeek),
            };
      if (lastIsCurrent) {
        const index = cells.length - 1;
        if (index >= 0 && cells[index] === null && nowCell !== null) cells[index] = nowCell;
      } else {
        cells.push(nowCell);
      }
      return { label: accountLabel(row, env), cells };
    });
    const blocked = {
      label: h("span", { class: "stats-row-label", text: "Blocked" }),
      // A narrow column hides the number; its tooltip always has it.
      cells: data.blocked.map((value, index) => ({
        text: durationText(Math.round(value * 3600)),
        tip: `Blocked · week from ${dateText(data.weekStarts[index])} | ${durationText(
          Math.round(value * 3600)
        )}: working hours in which the active account was full`,
      })),
    };
    if (!lastIsCurrent) {
      blocked.cells.push({ text: "—", tip: "the running week has no measured blocked hours yet" });
    }
    rows.push(blocked);
    if (data.weekStarts.length === 0 || data.rows.length === 0) {
      gridBody.appendChild(emptyLine("no week in the range"));
    } else {
      gridBody.appendChild(grid({ headers, rows }));
    }
    gridCard.appendChild(gridBody);
    left.appendChild(gridCard);

    const recentCard = h("div", { class: "hcard stats-recent-card" });
    recentCard.appendChild(
      h("div", { class: "ch" }, [
        h("span", { class: "t", text: "Last week and this one" }),
        h("span", {
          class: "s",
          text: "percent of each account's week; a drop to 0 is its reset",
        }),
        h("span", { class: "sp" }),
        h(
          "span",
          { class: "stats-chipline stats-recent-legend" },
          env.accounts.map((view) => accountName(env.accounts, view.id))
        ),
      ])
    );
    const recentBody = h("div", { class: "stats-chart-body" });
    if (data.recent.length === 0) {
      recentBody.appendChild(emptyLine("no week in the range"));
    } else {
      const recentPoints = data.recent.flatMap((curve) => curve.points);
      recentBody.appendChild(
        lines({
          series: data.recent.map((curve) => {
            const view = accountView(env.accounts, curve.id);
            return {
              name: accountNameText(env.accounts, curve.id),
              color: view && isColor(view.color) ? view.color : "var(--muted)",
              area: true,
              points: curve.points,
              tip: (time, value) =>
                `${accountNameText(env.accounts, curve.id)} | ${dateTimeText(new Date(time))}: ${percentText(value)}`,
            };
          }),
          yMax: 100,
          ySuffix: " %",
          marks: [{ at: data.now, label: "now" }, ...historyMark(env, recentPoints)],
        })
      );
    }
    recentCard.appendChild(recentBody);
    left.appendChild(recentCard);

    right.appendChild(detailHead());
    const error = renderDetailError();
    if (error) right.appendChild(error);
    else if (state.window !== null) right.appendChild(windowCard(state.window));
    else if (state.summary !== null) right.appendChild(summaryCard());
    else {
      right.appendChild(h("div", { class: "stats-detail-body" }, [emptyLine("no week in the range")]));
    }
  }

  async function selectSummary(week) {
    const token = ++state.detailToken;
    try {
      const summary = await env.call("stats_week_summary", { week });
      if (token !== state.detailToken) return;
      state.summaryWeek = week;
      state.summary = summary;
      state.window = null;
      state.detailError = null;
      state.detailEmpty = false;
      paint();
    } catch (error) {
      if (token !== state.detailToken) return;
      state.detailError = errorMessage(error);
      state.detailEmpty = Boolean(error && error.name === "EmptyRange");
      paint();
    }
  }

  async function selectWindow(row, end, week) {
    const token = ++state.detailToken;
    try {
      const detail = await env.call("stats_window_detail", {
        accountId: row.id,
        end,
      });
      if (token !== state.detailToken) return;
      state.window = detail;
      state.summary = null;
      state.summaryWeek = week;
      state.detailError = null;
      state.detailEmpty = false;
      paint();
    } catch (error) {
      if (token !== state.detailToken) return;
      state.detailError = errorMessage(error);
      state.detailEmpty = Boolean(error && error.name === "EmptyRange");
      paint();
    }
  }

  paint();
  const finished =
    runningIndex === -1 ? data.weekStarts : data.weekStarts.slice(0, runningIndex);
  // The newest finished week; without one the running week, so far: no page
  // waits a week before it shows something.
  const week =
    finished.length > 0
      ? finished[finished.length - 1]
      : runningIndex !== -1
        ? data.weekStarts[runningIndex]
        : null;
  if (week !== null) await selectSummary(week);
}
