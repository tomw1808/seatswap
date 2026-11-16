// The Overview page (spec §3.1): the five numbers, each week by account and by
// project, what the numbers say, and the share that expired per week. The page
// formats and draws the struct; it adds no sum, share or sentence of its own.

import {
  accountName,
  accountNameText,
  accountView,
  amountNode,
  amountTip,
  clear,
  dateText,
  durationText,
  emptyLine,
  h,
  isColor,
  kpi,
  legendItem,
  percentText,
  shortDate,
  switchText,
} from "../../common.js";
import { lines, stackedColumns } from "../../charts.js";

const PROJECT_TOKENS = [
  "var(--focus)",
  "var(--warn)",
  "var(--pill-text)",
  "var(--danger)",
  "var(--muted)",
];

function projectColours(weeks) {
  const colours = new Map();
  let index = 0;
  for (const week of weeks) {
    for (const [name] of week.projects) {
      if (colours.has(name)) continue;
      if (name === "other") colours.set(name, "var(--muted)");
      else {
        colours.set(name, PROJECT_TOKENS[index % PROJECT_TOKENS.length]);
        index += 1;
      }
    }
  }
  return colours;
}

// A KPI value with its percent beside the money, as the mock-up writes it.
// The percent is a number the view carries; the page only formats it.
function percentFigure(amount, percent) {
  return h("span", {}, [
    amount,
    ...(typeof percent === "number"
      ? [h("span", { class: "stats-kpi-percent", text: percentText(percent) })]
      : []),
  ]);
}

export async function show(container, env) {
  const data = await env.call("stats_overview");
  clear(container);
  const currency = data.currency;
  const colours = projectColours(data.weeks);

  const root = h("div", { class: "g stats-overview" });

  const kpis = h("div", { class: "stats-kpis" });
  const expiredRows = [];
  for (const [id, amount] of data.kpis.expiredByAccount) {
    if (expiredRows.length > 0) expiredRows.push(" · ");
    expiredRows.push(accountName(env.accounts, id), ` ${amountTip(amount, currency)}`);
  }
  if (expiredRows.length === 0) expiredRows.push("— · no account expired money in the range");
  const cost =
    typeof data.kpis.switchCostPercentOfWindow === "number"
      ? ` · a switch costs ${percentText(data.kpis.switchCostPercentOfWindow)} of its 5h window`
      : " · switch cost —";
  const weeks = data.kpis.weeks === 1 ? "week" : "weeks";
  kpis.appendChild(
    kpi(
      "Used",
      percentFigure(amountNode(data.kpis.used, currency), data.kpis.usedPercent),
      h("span", {}, [
        "of ",
        amountNode(data.kpis.paid, currency),
        ` paid for ${data.kpis.weeks} ${weeks}`,
      ]),
      "Used | money used of the priced account weeks; an account without a price is left out"
    )
  );
  kpis.appendChild(
    kpi(
      "Expired unused",
      percentFigure(amountNode(data.kpis.expired, currency), data.kpis.expiredPercent),
      expiredRows,
      "Expired unused | quota left at a weekly reset, in money; per account, largest first"
    )
  );
  const blockedBefore = data.kpis.blockedBeforeAdded;
  const blockedSub = blockedBefore
    ? [
        durationText(Math.round(blockedBefore.hours * 3600)),
        " of them before ",
        blockedBefore.name,
        " was added",
      ]
    : "working hours the active account was full";
  kpis.appendChild(
    kpi(
      "Blocked",
      durationText(Math.round(data.kpis.blockedHours * 3600)),
      blockedSub,
      "Blocked | working hours in which the active account was at 100 %"
    )
  );
  kpis.appendChild(
    kpi(
      "Avoidable",
      durationText(Math.round(data.kpis.avoidableHours * 3600)),
      "blocked while other quota expired",
      "Avoidable | blocked hours in a week in which another account's quota expired",
      data.kpis.avoidableHours > 0 ? "stats-kpi-value-bad" : null
    )
  );
  kpis.appendChild(
    kpi(
      "Switches",
      String(data.kpis.switchesTotal),
      `${switchText(data.kpis.switches)}${cost}`,
      "Switches | every change of the live account, by who made it; the total of the five counts"
    )
  );

  const kpiCard = h("div", { class: "hcard stats-kpi-card" }, [kpis]);
  root.appendChild(kpiCard);

  const weeksCard = h("div", { class: "hcard stats-weeks-card" });
  const weeksHead = h("div", { class: "ch" }, [
    h("span", { class: "t", text: "Each week: by account, and by project" }),
    h("span", {
      class: "s",
      text: currency || "—",
      title: currency ? undefined : "no currency set in Settings",
    }),
    h("span", { class: "sp" }),
  ]);
  const legend = h("div", { class: "stats-legend" });
  for (const view of env.accounts) {
    legend.appendChild(accountName(env.accounts, view.id));
  }
  legend.appendChild(legendItem("var(--muted)", "expired", true));
  const sep = h("span", { class: "stats-legend-sep", text: "│" });
  legend.appendChild(sep);
  for (const [name, colour] of colours) {
    legend.appendChild(legendItem(colour, name, false));
  }
  weeksHead.appendChild(legend);
  weeksCard.appendChild(weeksHead);

  const columns = data.weeks.map((week) => {
    const label = shortDate(week.week);
    // The expired money is carried per account, not as one week total, so each
    // account's hatched expired piece stands above the used pieces of the stack.
    const expiredStack = [];
    const usedStack = [];
    for (const account of week.accounts) {
      const view = accountView(env.accounts, account.id);
      const color = view && isColor(view.color) ? view.color : "var(--muted)";
      const name = accountNameText(env.accounts, account.id);
      if (account.expired !== null && account.expired !== undefined) {
        expiredStack.push({
          value: account.expired,
          color: "var(--muted)",
          hatch: true,
          tip: `${name} · week from ${label} | expired ${percentText(
            account.expiredPercent
          )} = ${amountTip(account.expired, currency)}${
            account.uncertain ? " | uncertain: a reading near a reset is missing" : ""
          }`,
        });
      }
      if (account.used !== null && account.used !== undefined) {
        usedStack.push({
          value: account.used,
          color,
          tip: `${name} · week from ${label} | used ${percentText(
            account.usedPercent
          )} = ${amountTip(account.used, currency)}`,
        });
      }
    }
    const projectStack = week.projects.map(([name, value]) => ({
      value,
      color: colours.get(name) || "var(--muted)",
      tip: `${name} · week from ${label} | ${amountTip(value, currency)}`,
    }));
    return {
      label,
      at: week.week,
      sub: `${durationText(Math.round(week.blockedHours * 3600))} blocked`,
      stacks: [expiredStack.concat(usedStack), projectStack],
    };
  });

  const chartBody = h("div", { class: "stats-chart-body" });
  if (data.weeks.length === 0) {
    chartBody.appendChild(emptyLine("no week in the range"));
  } else {
    chartBody.appendChild(
      stackedColumns({
        columns,
        marks: data.marks.map((mark) => ({ at: mark.at, label: mark.label })),
      })
    );
  }
  weeksCard.appendChild(chartBody);
  root.appendChild(weeksCard);

  const side = h("div", { class: "stats-side" });
  const findingsCard = h("div", { class: "hcard" });
  findingsCard.appendChild(
    h("div", { class: "ch" }, [h("span", { class: "t", text: "What the numbers say" })])
  );
  const findingsBody = h("div", { class: "stats-body" });
  if (data.findings.length === 0) {
    findingsBody.appendChild(emptyLine("no finding in the range"));
  } else {
    const list = h("ul", { class: "stats-findings" });
    for (const finding of data.findings) {
      list.appendChild(h("li", { text: finding }));
    }
    findingsBody.appendChild(list);
  }
  findingsCard.appendChild(findingsBody);
  side.appendChild(findingsCard);

  const shareCard = h("div", { class: "hcard stats-share-card" });
  shareCard.appendChild(
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "Share that expired, per week" }),
      h("span", { class: "s", text: "of the value of all accounts" }),
    ])
  );
  const shareBody = h("div", { class: "stats-chart-body" });
  if (data.weeks.length === 0) {
    shareBody.appendChild(emptyLine("no week in the range"));
  } else {
    shareBody.appendChild(
      lines({
        series: [
          {
            name: "expired",
            color: "var(--warn)",
            area: true,
            points: data.weeks.map((week) => [week.week, week.expiredShare]),
            tip: (time, value) =>
              `expired | week from ${dateText(new Date(time))}: ${percentText(value)} of the value`,
          },
        ],
        yMax: 100,
        ySuffix: " %",
        marks: data.marks.map((mark) => ({ at: mark.at, label: mark.label })),
      })
    );
  }
  shareCard.appendChild(shareBody);
  side.appendChild(shareCard);
  root.appendChild(side);

  if (data.withoutPrice.length > 0) {
    const note = h("p", { class: "stats-note" }, ["No price for "]);
    for (const [index, id] of data.withoutPrice.entries()) {
      if (index > 0) note.appendChild(document.createTextNode(", "));
      note.appendChild(accountName(env.accounts, id));
    }
    note.appendChild(document.createTextNode(": left out of the money sums."));
    root.appendChild(note);
  }

  container.appendChild(root);
}
