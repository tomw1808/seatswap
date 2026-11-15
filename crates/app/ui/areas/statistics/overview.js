// The Overview page (spec §3.1): the five numbers, each week by account and by
// project, what the numbers say, and the share that expired per week. The page
// formats and draws the struct; it adds no sum, share or sentence of its own.

import {
  accountName,
  accountView,
  amountText,
  clear,
  dateText,
  h,
  hoursText,
  isColor,
  percentText,
} from "../../common.js";
import { lines, stackedColumns } from "../../charts.js";

const PROJECT_TOKENS = [
  "var(--focus)",
  "var(--warn)",
  "var(--pill-text)",
  "var(--danger)",
  "var(--muted)",
];

function shortDate(value) {
  return dateText(value).replace(/^\S+\s+/, "");
}

function projectColours(weeks) {
  const colours = new Map();
  let index = 0;
  for (const week of weeks) {
    for (const [name] of Array.isArray(week.projects) ? week.projects : []) {
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

function legendItem(colour, name, hatch) {
  const box = h("span", {
    class: `stats-legend-box${hatch ? " stats-legend-box-hatch" : ""}`,
    style: { "--stats-legend-color": colour },
  });
  return h(
    "span",
    { class: "stats-legend-item", title: name },
    [box, h("span", { class: "stats-legend-name", text: name })]
  );
}

function kpi(label, value, sub, tip, valueClass) {
  const node = h("div", { class: "stats-kpi" }, [
    h("span", { class: "stats-kpi-label", text: label }),
    h("span", {
      class: `stats-kpi-value${valueClass ? ` ${valueClass}` : ""}`,
      text: value,
    }),
    sub ? h("span", { class: "stats-kpi-sub" }, sub) : null,
  ]);
  node.title = tip;
  return node;
}

function switchText(switches) {
  return `you ${switches.user} · auto ${switches.auto}`;
}

export async function show(container, env) {
  const data = await env.call("stats_overview");
  clear(container);
  const currency = data.currency;
  const colours = projectColours(data.weeks);

  const root = h("div", { class: "g stats-overview" });

  const kpis = h("div", { class: "stats-kpis" });
  const expiredRows = [];
  for (const [id, amount] of Array.isArray(data.kpis.expiredByAccount)
    ? data.kpis.expiredByAccount
    : []) {
    if (expiredRows.length > 0) expiredRows.push(" · ");
    expiredRows.push(accountName(env.accounts, id), ` ${amountText(amount, currency)}`);
  }
  const cost =
    typeof data.kpis.switchCostPercentOfWindow === "number"
      ? ` · a switch costs ${percentText(data.kpis.switchCostPercentOfWindow)} of its 5 h window`
      : " · switch cost —";
  kpis.appendChild(
    kpi(
      "Used",
      amountText(data.kpis.used, currency),
      `of ${amountText(data.kpis.paid, currency)} paid for the chosen weeks`,
      "Used | money used of the priced account weeks; an account without a price is left out"
    )
  );
  kpis.appendChild(
    kpi(
      "Expired unused",
      amountText(data.kpis.expired, currency),
      expiredRows,
      "Expired unused | quota left at a weekly reset, in money; per account, largest first"
    )
  );
  kpis.appendChild(
    kpi(
      "Blocked",
      hoursText(data.kpis.blockedHours),
      "working hours the active account was full",
      "Blocked | working hours in which the active account was at 100 %"
    )
  );
  kpis.appendChild(
    kpi(
      "Avoidable",
      hoursText(data.kpis.avoidableHours),
      "blocked while other quota expired",
      "Avoidable | blocked hours in a week in which another account's quota expired",
      data.kpis.avoidableHours > 0 ? "stats-kpi-value-bad" : null
    )
  );
  kpis.appendChild(
    kpi(
      "Switches",
      switchText(data.kpis.switches),
      `agents ${data.kpis.switches.agent} · cli ${data.kpis.switches.cli} · external ${data.kpis.switches.external}${cost}`,
      "Switches | every change of the live account, by who made it"
    )
  );

  const kpiCard = h("div", { class: "hcard stats-kpi-card" }, [kpis]);
  root.appendChild(kpiCard);

  const weeksCard = h("div", { class: "hcard stats-weeks-card" });
  const weeksHead = h("div", { class: "ch" }, [
    h("span", { class: "t", text: "Each week: by account, and by project" }),
    h("span", { class: "s", text: currency || "—" }),
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
    for (const account of Array.isArray(week.accounts) ? week.accounts : []) {
      const view = accountView(env.accounts, account.id);
      const color = view && isColor(view.color) ? view.color : "var(--muted)";
      const name = accountName(env.accounts, account.id).textContent;
      if (account.expired !== null && account.expired !== undefined) {
        expiredStack.push({
          value: account.expired,
          color: "var(--muted)",
          hatch: true,
          tip: `${name} · week from ${label} | expired ${percentText(
            account.expiredPercent
          )} = ${amountText(account.expired, currency)}${
            account.uncertain ? " | uncertain: seatswap was off at the reset" : ""
          }`,
        });
      }
      if (account.used !== null && account.used !== undefined) {
        usedStack.push({
          value: account.used,
          color,
          tip: `${name} · week from ${label} | used ${percentText(
            account.usedPercent
          )} = ${amountText(account.used, currency)}`,
        });
      }
    }
    const projectStack = week.projects.map(([name, value]) => ({
      value,
      color: colours.get(name) || "var(--muted)",
      tip: `${name} · week from ${label} | ${amountText(value, currency)}`,
    }));
    return {
      label,
      at: week.week,
      sub: `${hoursText(week.blockedHours)} blocked`,
      stacks: [expiredStack.concat(usedStack), projectStack],
    };
  });

  const chartBody = h("div", { class: "stats-chart-body" });
  chartBody.appendChild(
    stackedColumns({
      columns,
      marks: (data.marks || []).map((mark) => ({ at: mark.at, label: mark.label })),
    })
  );
  weeksCard.appendChild(chartBody);
  root.appendChild(weeksCard);

  const side = h("div", { class: "stats-side" });
  const findingsCard = h("div", { class: "hcard" });
  findingsCard.appendChild(
    h("div", { class: "ch" }, [h("span", { class: "t", text: "What the numbers say" })])
  );
  const findingsBody = h("div", { class: "stats-body" });
  const list = h("ul", { class: "stats-findings" });
  for (const finding of Array.isArray(data.findings) ? data.findings : []) {
    list.appendChild(h("li", { text: finding }));
  }
  findingsBody.appendChild(list);
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
      marks: (data.marks || []).map((mark) => ({ at: mark.at, label: mark.label })),
    })
  );
  shareCard.appendChild(shareBody);
  side.appendChild(shareCard);
  root.appendChild(side);

  if (Array.isArray(data.withoutPrice) && data.withoutPrice.length > 0) {
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
