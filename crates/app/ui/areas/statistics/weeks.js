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
  emptyLine,
  errorMessage,
  h,
  hoursText,
  isColor,
  percentText,
  shortDate,
  shortPercent,
  tokens,
  valueTip,
  weekdayIndex,
} from "../../common.js";
import { grid, lines } from "../../charts.js";

const WEEK_MS = 7 * 86400000;

function accountLabel(row, env) {
  const label = h("span", { class: "stats-row-label" }, [
    accountName(env.accounts, row.id),
  ]);
  if (row.plan) label.appendChild(h("span", { class: "stats-plan", text: row.plan }));
  return label;
}

function thisWeekStart() {
  const now = new Date();
  const monday = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  monday.setDate(monday.getDate() - weekdayIndex(now));
  return monday.getTime();
}

function newestFinished(weekStarts) {
  const now = Date.now();
  let best = null;
  for (const week of weekStarts) {
    const time = Date.parse(week);
    if (Number.isNaN(time)) continue;
    if (time + WEEK_MS <= now) best = week;
  }
  return best;
}

function switchLine(switches) {
  return `you ${switches.user} · cli ${switches.cli} · auto-burn ${switches.auto} · agents ${switches.agent} · external ${switches.external}`;
}

export async function show(container, env) {
  const data = await env.call("stats_weeks");
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

  const lastIsCurrent =
    data.weekStarts.length > 0 &&
    Math.abs(Date.parse(data.weekStarts[data.weekStarts.length - 1]) - thisWeekStart()) < 86400000;

  function renderDetailError() {
    if (state.detailError === null) return null;
    if (state.detailEmpty) {
      return h("p", { class: "stats-empty", text: state.detailError });
    }
    return banner("error", "⚠", state.detailError);
  }

  function summaryCard() {
    const summary = state.summary;
    const reason = summary.moneyMissing;
    const missingTitle = typeof reason === "string" ? reason : undefined;
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
        h("span", { class: "stats-kpi-label", text: "Used" }),
        usedPercent,
        h("span", { class: "stats-kpi-sub" }, [usedMoney]),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Expired" }),
        h("span", { class: "stats-kpi-value" }, [expiredMoney]),
        h("span", { class: "stats-kpi-sub", text: "quota left at the resets" }),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Blocked" }),
        h("span", { class: "stats-kpi-value", text: hoursText(summary.blockedHours) }),
        h("span", { class: "stats-kpi-sub", text: "working hours" }),
      ])
    );
    card.appendChild(kpis);

    const apiValue = amountNode(summary.values.api, summary.values.apiCurrency);
    apiValue.title = valueTip(summary.values, "api", "in this week");
    const beforeTitle = missingTitle || undefined;
    const list = h("dl", { class: "stats-kv" });
    for (const [term, value] of [
      ["Switches", switchLine(summary.switches)],
      [
        "Tokens",
        `out ${tokens(summary.tokens.out)} · in ${tokens(summary.tokens.input)} · cache read ${tokens(
          summary.tokens.cr
        )} · written ${tokens(summary.tokens.cw)}`,
      ],
      ["API value", apiValue],
      [
        "Week before",
        h("span", {
          text:
            typeof summary.usedChangePercent !== "number"
              ? "—"
              : `${summary.usedChangePercent >= 0 ? "+" : ""}${percentText(
                  summary.usedChangePercent
                )}`,
          title: beforeTitle,
        }),
      ],
    ]) {
      list.appendChild(h("dt", { text: term }));
      list.appendChild(h("dd", {}, value));
    }
    card.appendChild(list);

    card.appendChild(h("p", { class: "stats-detail-label", text: "Used per account" }));
    if (summary.perAccount.length === 0) {
      card.appendChild(emptyLine("no account week in this week"));
    } else {
      const perAccount = h("div", { class: "stats-chipline" });
      for (const [id, amount] of summary.perAccount) {
        perAccount.appendChild(
          h("span", { class: "stats-account-line" }, [
            accountName(env.accounts, id),
            h("span", {}, amountNode(amount, env.currency)),
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
    const kpis = h("div", { class: "stats-mini-kpis" });
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Used" }),
        h("span", { class: "stats-kpi-value", text: percentText(detail.used) }),
        h("span", { class: "stats-kpi-sub" }, [amountNode(detail.usedMoney, env.currency)]),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Expired" }),
        h("span", { class: "stats-kpi-value", text: percentText(detail.expired) }),
        h("span", { class: "stats-kpi-sub" }, [amountNode(detail.expiredMoney, env.currency)]),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Blocked after 100 %" }),
        h("span", { class: "stats-kpi-value", text: hoursText(detail.blockedHoursAfter) }),
        h("span", { class: "stats-kpi-sub", text: "working hours" }),
      ])
    );
    card.appendChild(kpis);

    const list = h("dl", { class: "stats-kv" });
    const rows = [
      ["Reached 100 %", detail.reached100At === null ? "not reached" : dateTimeText(detail.reached100At)],
      ["5 h windows", detail.sessionWindowsUsed === null ? "—" : String(detail.sessionWindowsUsed)],
      [
        "Switches",
        `into or out ${detail.switchesInOut} · auto-burn ${detail.switchesAuto}`,
      ],
      [
        "Last reading",
        `${Math.round(detail.lastSampleBeforeEndSecs / 60)} min before the reset`,
      ],
      ["Certainty", detail.uncertain ? "uncertain: seatswap was off at the reset" : "fresh"],
    ];
    for (const [term, value] of rows) {
      list.appendChild(h("dt", { text: term }));
      list.appendChild(h("dd", {}, value));
    }
    card.appendChild(list);

    card.appendChild(h("p", { class: "stats-detail-label", text: "Curve of the week" }));
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
        })
      );
    }
    card.appendChild(chart);
    return card;
  }

  function detailHead() {
    const head = h("div", { class: "ch" });
    if (state.window !== null) {
      head.appendChild(h("span", { class: "t", text: "Week ending" }));
      head.appendChild(h("span", { class: "stats-plan", text: dateTimeText(state.window.end) }));
      head.appendChild(accountName(env.accounts, state.window.id));
      if (state.window.uncertain) {
        head.appendChild(h("span", { class: "stats-tag stats-tag-warn", text: "uncertain" }));
      }
    } else if (state.summary !== null) {
      head.appendChild(h("span", { class: "t", text: "Summary" }));
      head.appendChild(
        h("span", { class: "stats-plan", text: `week from ${dateText(state.summary.week)}` })
      );
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
        h("span", { class: "s", text: "a click on a date shows the week; on a cell the account's week" }),
      ])
    );
    const gridBody = h("div", { class: "stats-grid-body" });
    const headers = data.weekStarts.map((week) => ({
      label: shortDate(week),
      tip: `Summary of the week from ${dateText(week)}`,
      selected: state.summaryWeek === week,
      onClick: () => void selectSummary(week),
    }));
    if (!lastIsCurrent) {
      headers.push({ label: "now", tip: "the running week", current: true });
    }
    const rows = data.rows.map((row) => {
      const view = accountView(env.accounts, row.id);
      const color = view && isColor(view.color) ? view.color : "var(--muted)";
      const cells = row.cells.map((cell, index) => {
        if (!cell) return null;
        const week = data.weekStarts[index];
        const tip = `${accountNameText(env.accounts, row.id)} · week from ${dateText(week)} | used ${percentText(
          cell.used
        )} · expired ${percentText(cell.expired)} = ${amountTip(cell.expiredMoney, env.currency)}${
          cell.full ? " | full" : ""
        }${cell.uncertain ? " | uncertain: seatswap was off at the reset" : ""}`;
        return {
          fill: cell.used,
          color,
          textColor: contrastText(color),
          hatch: true,
          text: shortPercent(cell.used),
          tip,
          full: cell.full,
          uncertain: cell.uncertain,
          onClick: () => void selectWindow(row, cell),
        };
      });
      const nowCell =
        typeof row.nowPercent !== "number"
          ? null
          : {
              fill: row.nowPercent,
              color,
              textColor: contrastText(color),
              hatch: true,
              current: true,
              text: shortPercent(row.nowPercent),
              tip: `${accountNameText(env.accounts, row.id)} · now ${percentText(row.nowPercent)}`,
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
      cells: data.blocked.map((value) => ({
        text: hoursText(value),
        tip: "working hours blocked in this week",
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
        h("span", { class: "s", text: "weekly percent over the last 14 days" }),
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

  async function selectWindow(row, cell) {
    const token = ++state.detailToken;
    try {
      const detail = await env.call("stats_window_detail", {
        accountId: row.id,
        end: cell.end,
      });
      if (token !== state.detailToken) return;
      state.window = detail;
      state.summary = null;
      state.summaryWeek = null;
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
  const week = newestFinished(data.weekStarts);
  if (week !== null) await selectSummary(week);
}
