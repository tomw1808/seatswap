// The Overview page (spec §3.1): the weekly windows running now, the five
// numbers, each finished week by account and by project, what the numbers
// say, and the share that expired per week. The page formats and draws the
// struct; it adds no sum, share or sentence of its own.

import {
  accountName,
  accountNameText,
  accountPlan,
  accountView,
  amountNode,
  amountTip,
  clear,
  dateText,
  dateTimeText,
  hoursText,
  emptyLine,
  errorMessage,
  h,
  isColor,
  kpi,
  legendItem,
  percentText,
  providerName,
  shortDate,
  switchText,
  weekdayTimeText,
} from "../../common.js";
import { lines, stackedColumns } from "../../charts.js";

const PROJECT_TOKENS = [
  "var(--focus)",
  "var(--warn)",
  "var(--pill-text)",
  "var(--danger)",
  "var(--muted)",
];

// One colour per project across the page: the lists of `[name, value]` pairs
// are walked in order, a new name takes the next token, `other` is always the
// muted one.
function projectColours(lists) {
  const colours = new Map();
  let index = 0;
  for (const list of lists) {
    for (const [name] of list) {
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

// A percent as a bar's width; the text beside the bar carries the number
// itself, which may pass 100.
function barWidth(value) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`the bar width ${String(value)} is not a percent`);
  }
  return `${Math.min(value, 100)}%`;
}

// The subscription behind a This-week provider row: the frame's view for its
// abbreviation, or null for a company row (work whose subscription is not
// known), which `providerName` names.
function providerSubscription(provider, env) {
  if (typeof provider.name !== "string" || provider.name === "") return null;
  return env.accounts.find((view) => view.abbr === provider.name) || null;
}

// Whether a provider row has no subscription value to fill a bar with: a
// company row (no subscription) and a subscription whose view has no price are
// the paler kind. The frame's view carries the snapshot's monthly price.
function providerIsPale(view) {
  return view === null || view.price === null;
}

// The pieces of a provider row's bar by project, as the account rows draw them.
function providerFill(provider, used, color, colours, tip) {
  const fill = h("div", { class: "stats-tw-fill" });
  if (typeof used === "number") fill.style.width = barWidth(used);
  if (provider.projects.length === 0) {
    const reason =
      typeof provider.apiMissing === "string" ? provider.apiMissing : "no API value this week";
    fill.appendChild(
      h("i", {
        class: "stats-tw-seg",
        style: { flexGrow: "1", background: color },
        title: `${tip} | no project split: ${reason}`,
      })
    );
  }
  for (const [project, share] of provider.projects) {
    const amount =
      typeof provider.api === "number"
        ? ` = ${amountTip((provider.api * share) / 100, provider.apiCurrency)}`
        : "";
    fill.appendChild(
      h("i", {
        class: "stats-tw-seg",
        style: { flexGrow: String(share), background: colours.get(project) || "var(--muted)" },
        title: `${project} · ${provider.name} | ${percentText(share)} of its API value this week${amount}`,
      })
    );
  }
  return fill;
}

// A provider subscription's row of the This-week card: the account row's code
// (chip, used bar, money, reset). Without a price the bar is the paler kind and
// its money is "—" titled with the reason.
function subscriptionRow(provider, view, colours, currency, env) {
  const name = accountNameText(env.accounts, view.id);
  const color = isColor(view.color) ? view.color : "var(--muted)";
  const used = typeof provider.used === "number" ? provider.used : null;
  const tip =
    used === null
      ? `${name} this week: no running window of this subscription`
      : `${name} this week: used ${percentText(used)} so far`;
  const pale = providerIsPale(view);
  const row = h("div", { class: `stats-tw-row${pale ? " stats-tw-row-api" : ""}` }, [
    h("div", { class: "stats-tw-account" }, [
      accountName(env.accounts, view.id),
      accountPlan(env.accounts, view.id),
    ]),
  ]);
  row.appendChild(
    h("div", { class: `stats-tw-bar${pale ? " stats-tw-bar-api" : ""}` }, [
      providerFill(provider, used, color, colours, tip),
    ])
  );
  const amount = amountNode(provider.money, currency);
  if (provider.money === null && typeof provider.moneyMissing === "string") {
    amount.title = provider.moneyMissing;
  }
  const valueTitle = [tip, provider.moneyMissing].filter(Boolean).join(" | ");
  row.appendChild(
    h("span", { class: "stats-tw-figure", title: valueTitle }, [
      `${used === null ? "—" : percentText(used)} · `,
      amount,
    ])
  );
  const reset =
    typeof provider.resetsAt === "string"
      ? {
          text: `resets ${weekdayTimeText(provider.resetsAt)}`,
          title: `resets ${dateTimeText(provider.resetsAt)}`,
        }
      : {
          text: "resets —",
          title: provider.moneyMissing || "no running window of this subscription",
        };
  row.appendChild(h("span", { class: "stats-tw-reset", text: reset.text, title: reset.title }));
  return row;
}

// A company row of the This-week card (work whose subscription is not known):
// its API value since Monday by project, with no limit to fill, so the pieces
// span the whole paler bar and never read as a share of a limit used.
function companyRow(provider, colours) {
  const name = providerName(provider.company).textContent;
  const tip = `${name} this week: no subscription, valued at API prices`;
  const row = h("div", { class: "stats-tw-row stats-tw-row-api" }, [
    h("div", { class: "stats-tw-account" }, [providerName(provider.company, tip)]),
  ]);
  row.appendChild(
    h("div", { class: "stats-tw-bar stats-tw-bar-api" }, [
      providerFill(provider, null, "var(--muted)", colours, tip),
    ])
  );
  const amount = amountNode(provider.api, provider.apiCurrency);
  if (provider.api === null && typeof provider.apiMissing === "string") {
    amount.title = provider.apiMissing;
  }
  row.appendChild(
    h("span", { class: "stats-tw-figure", title: tip }, [amount, " at API prices"])
  );
  row.appendChild(
    h("span", {
      class: "stats-tw-reset",
      text: "since Mon",
      title: "since Monday 00:00 of this calendar week",
    })
  );
  return row;
}

// One other provider's row of the This-week card: a subscription row when the
// frame knows the subscription its name is, else the company row.
function providerRow(provider, colours, currency, env) {
  const view = providerSubscription(provider, env);
  return view === null
    ? companyRow(provider, colours)
    : subscriptionRow(provider, view, colours, currency, env);
}

// The This-week card: each account's weekly window running now, its use so
// far split by project, its money and its reset; then each other provider's
// work of this calendar week at API prices. It ignores the range.
function thisWeekCard(card, currency, colours, env) {
  const node = h("div", { class: "hcard stats-tw-card" }, [
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: "This week · so far" }),
      h("span", {
        class: "s",
        text: "each account's and subscription's current window, by project",
      }),
    ]),
  ]);
  if (card.accounts.length === 0) {
    node.appendChild(
      h("p", {
        class: "stats-tw-empty",
        text: "No usage reading yet: this card fills in with the next usage poll.",
      })
    );
    if (card.providers.length === 0) return node;
  }
  const rows = h("div", { class: "stats-tw-rows" });
  for (const account of card.accounts) {
    const view = accountView(env.accounts, account.id);
    const color = view && isColor(view.color) ? view.color : "var(--muted)";
    const name = accountNameText(env.accounts, account.id);
    const since = `${name} · since ${dateTimeText(account.startedAt)}`;
    const row = h("div", { class: "stats-tw-row" }, [
      h("div", { class: "stats-tw-account" }, [
        accountName(env.accounts, account.id),
        accountPlan(env.accounts, account.id),
      ]),
    ]);

    const fill = h("div", { class: "stats-tw-fill", style: { width: barWidth(account.used) } });
    if (account.projects.length === 0) {
      // No token record of the window has a value yet: the use is one piece.
      fill.appendChild(
        h("i", {
          class: "stats-tw-seg",
          style: { flexGrow: "1", background: color },
          title: `${since} | used ${percentText(account.used)} | no project split: no valued token record in this window yet`,
        })
      );
    }
    for (const [project, share] of account.projects) {
      const money =
        typeof account.money === "number"
          ? ` = ${amountTip((account.money * share) / 100, currency)}`
          : "";
      fill.appendChild(
        h("i", {
          class: "stats-tw-seg",
          style: { flexGrow: String(share), background: colours.get(project) || "var(--muted)" },
          title: `${project} · ${name} | ${percentText(share)} of the window's value${money}`,
        })
      );
    }
    row.appendChild(h("div", { class: "stats-tw-bar" }, [fill]));

    const amount = amountNode(account.money, currency);
    if (account.money === null && typeof account.moneyMissing === "string") {
      amount.title = account.moneyMissing;
    }
    row.appendChild(
      h(
        "span",
        {
          class: "stats-tw-figure",
          title:
            account.now !== account.used
              ? `${since} | the newest reading is ${percentText(
                  account.now
                )}; a reset began this window at ${percentText(
                  account.now - account.used
                )}, so ${percentText(account.used)} is used so far`
              : `${since} | used ${percentText(account.used)} so far`,
        },
        [`${percentText(account.used)} · `, amount]
      )
    );
    row.appendChild(
      h("span", {
        class: "stats-tw-reset",
        text: `resets ${weekdayTimeText(account.resetsAt)}`,
        title: `resets ${dateTimeText(account.resetsAt)}`,
      })
    );
    rows.appendChild(row);
  }
  for (const provider of card.providers) {
    rows.appendChild(providerRow(provider, colours, currency, env));
  }
  node.appendChild(rows);

  // The legend names every colour the bars show: the projects of all listed
  // accounts and providers together with their share of the API value, then
  // a project only one row's split names (all together count it in `other`).
  const legend = h("div", { class: "stats-tw-legend" });
  const named = new Set();
  for (const [project, share] of card.projects) {
    named.add(project);
    const item = legendItem(colours.get(project) || "var(--muted)", project, false);
    item.appendChild(h("span", { class: "stats-tw-share", text: percentText(share) }));
    item.title = `${project} | ${percentText(share)} of the listed accounts' and providers' API value this week`;
    legend.appendChild(item);
  }
  for (const part of [...card.accounts, ...card.providers]) {
    for (const [project] of part.projects) {
      if (named.has(project)) continue;
      named.add(project);
      const item = legendItem(colours.get(project) || "var(--muted)", project, false);
      item.title = `${project} | in "other" over all listed accounts and providers`;
      legend.appendChild(item);
    }
  }
  // The paler bar means no subscription value: say so once, only when a row
  // draws one.
  if (card.providers.some((provider) => providerIsPale(providerSubscription(provider, env)))) {
    legend.appendChild(
      h("span", {
        class: "stats-tw-paler",
        text: "paler: valued at API prices only, no price entered",
      })
    );
  }
  if (legend.childNodes.length > 0) node.appendChild(legend);
  return node;
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
  // The card ignores the range, so a range that holds no record still shows
  // it, with the area's "nothing recorded" line below it instead of the rest.
  const [card, page] = await Promise.all([
    env.ask("stats_this_week"),
    env.call("stats_overview").then(
      (data) => ({ data, empty: null }),
      (error) => {
        if (error && error.name === "EmptyRange") return { data: null, empty: errorMessage(error) };
        throw error;
      }
    ),
  ]);
  if (
    !Array.isArray(card.accounts) ||
    !Array.isArray(card.providers) ||
    !Array.isArray(card.projects)
  ) {
    throw new Error("stats_this_week returned an unexpected card");
  }
  clear(container);
  const currency = env.currency;
  const data = page.data;
  // The card's projects first, so the projects running now take the first
  // colours; the weekly chart uses the same colour for the same project.
  const colours = projectColours([
    card.projects,
    ...card.accounts.map((account) => account.projects),
    ...card.providers.map((provider) => provider.projects),
    ...(data === null ? [] : data.weeks.map((week) => week.projects)),
  ]);

  const root = h("div", { class: "g stats-overview" });
  root.appendChild(thisWeekCard(card, currency, colours, env));
  if (data === null) {
    root.appendChild(h("p", { class: "stats-empty stats-overview-empty", text: page.empty }));
    container.appendChild(root);
    return;
  }

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
      "Used | money used of the priced weekly windows that finished; an account without a price is left out"
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
  if (!Array.isArray(data.kpis.blockedByCompany)) {
    throw new Error("stats_overview returned no blockedByCompany");
  }
  const blockedBefore = data.kpis.blockedBeforeAdded;
  const blockedSub = blockedBefore
    ? [
        hoursText(blockedBefore.hours),
        " of Anthropic's before ",
        blockedBefore.name,
        " was added",
      ]
    : "working hours each company was blocked";
  // One figure per company, each on its own line: a provider's blocked hours
  // are not Anthropic's, so they are never added up, and no figure is cut off.
  const blocked = data.kpis.blockedByCompany;
  const blockedValue =
    blocked.length === 0
      ? hoursText(0)
      : h(
          "span",
          {
            class: `stats-blocked-companies${blocked.length > 1 ? " stats-blocked-many" : ""}`,
          },
          blocked.map((entry) =>
            h("span", { class: "stats-blocked-company" }, [
              providerName(
                entry.company,
                "working hours blocked at 100 % of this company's limit"
              ),
              ` ${hoursText(entry.hours)}`,
            ])
          )
        );
  kpis.appendChild(
    kpi(
      "Blocked",
      blockedValue,
      blockedSub,
      "Blocked | working hours in which each company's account was at 100 % of its limit; one figure per company, never a sum",
      "stats-kpi-blocked"
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
  // The chart's own projects; the card's legend names the ones running now.
  const chartProjects = new Set(data.weeks.flatMap((week) => week.projects.map(([name]) => name)));
  if (chartProjects.size > 0) {
    legend.appendChild(h("span", { class: "stats-legend-sep", text: "│" }));
  }
  for (const [name, colour] of colours) {
    if (chartProjects.has(name)) legend.appendChild(legendItem(colour, name, false));
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
    const blocked = hoursText(week.blockedHours);
    return {
      label,
      at: week.week,
      // The short form when the long one has no room under a column.
      sub: [`${blocked} blocked`, blocked],
      stacks: [expiredStack.concat(usedStack), projectStack],
    };
  });

  const chartBody = h("div", { class: "stats-chart-body" });
  if (data.weeks.length === 0) {
    chartBody.appendChild(emptyLine("no week in the range"));
  } else if (data.weeks.every((week) => week.accounts.length === 0)) {
    // The history counts finished windows only; the card above shows the
    // running ones, so the page is never empty while one runs.
    chartBody.appendChild(
      h("p", {
        class: "stats-empty",
        text: "No weekly window has finished in this range yet. The card above shows the running ones.",
      })
    );
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
  } else if (data.weeks.every((week) => week.expiredShare === null)) {
    // A share needs a finished weekly window of an account with a price;
    // without any there is no line and no time axis to place the marks on.
    shareBody.appendChild(emptyLine("no finished weekly window with a price in the range yet"));
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
