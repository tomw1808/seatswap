// The Projects page (spec §3.6): the tree of organizations, projects and
// folders, the organization × subscription matrix with its sentences, and one
// project's detail. The page formats and draws the struct; it adds no sum,
// share or sentence of the struct's own.

import {
  accountName,
  accountNameText,
  accountView,
  amountNode,
  amountTip,
  banner,
  clear,
  dateText,
  emptyLine,
  errorMessage,
  foldMemory,
  h,
  isColor,
  isObject,
  modelColor,
  modelName,
  percentText,
  shortDate,
  showTokens,
  subscriptionSplit,
  tokens,
  valueNode,
} from "../../common.js";
import { lines } from "../../charts.js";

// The user's folds, made once so they last while the window is open (rule R7):
// an organization is open until folded, a project is folded until unfolded.
const folds = foldMemory();

// A matrix cell is `null` when a record of it has no subscription value and
// `null` also when only some of its records have one; its row and column totals
// keep the known part (rule R3), which the page's fixed reason says.
const CELL_UNKNOWN =
  "some of this work has no subscription value; the totals keep the known part";

function sameKey(first, second) {
  return Boolean(
    first &&
      second &&
      first.kind === second.kind &&
      first.id === second.id
  );
}

function hasChildren(nodes, index) {
  return index + 1 < nodes.length && nodes[index + 1].level > nodes[index].level;
}

// A row's stable fold key, the same across ranges: an organization by its name,
// a project by its key. Folders are leaves.
function foldKey(node) {
  if (node.kind === "org") return `org:${node.name}`;
  if (node.kind === "project") {
    if (!isObject(node.key)) {
      throw new Error(`a project row needs its key, not ${JSON.stringify(node.key)}`);
    }
    return `project:${JSON.stringify(node.key)}`;
  }
  throw new Error(`a ${JSON.stringify(node.kind)} row has no fold of its own`);
}

function defaultOpen(node) {
  if (node.kind === "org") return true;
  if (node.kind === "project") return false;
  throw new Error(`a ${JSON.stringify(node.kind)} row has no fold of its own`);
}

function isNodeOpen(node) {
  return folds.isOpen(foldKey(node), defaultOpen(node));
}

function visibleRows(nodes, mode) {
  const rows = [];
  const stack = [];
  for (let index = 0; index < nodes.length; index += 1) {
    const node = nodes[index];
    while (stack.length > 0 && stack[stack.length - 1].node.level >= node.level) stack.pop();
    const hidden = stack.some((entry) => !isNodeOpen(entry.node));
    if (!hidden && !(mode === "plain" && node.kind === "org")) {
      rows.push({
        node,
        index,
        level: mode === "plain" ? Math.max(0, node.level - 1) : node.level,
      });
    }
    if (hasChildren(nodes, index)) stack.push({ node });
  }
  return rows;
}

// What a row stands for, in a bar part's tooltip.
const NODE_NOUN = { org: "organization", project: "project", folder: "folder" };

// The tree's bar split by subscription (rule R2): the one helper every page
// uses, so the unknown part is grey and striped here as on the Overview.
function splitBar(node, env) {
  const noun = NODE_NOUN[node.kind];
  if (!noun) throw new Error(`unknown row kind ${JSON.stringify(node.kind)}`);
  return subscriptionSplit(node.bySubscription, env.accounts, (name, percent) =>
    `${name} · ${percentText(percent)} of this ${noun}'s API value`);
}

// The reason a share is unknown: projects.rs sends `null` when the row has no
// priced API value or the page's total has none.
const NO_SHARE = "no priced API value for this row's use";

// A share of all use: "21.9 %", or "—" with the reason when the row has no
// priced API value or the page's priced total is unknown or 0 (projects.rs never
// sends a made-up 0 %).
function shareText(share) {
  if (share === null || share === undefined) return { text: "—", title: NO_SHARE };
  return { text: percentText(share), title: "" };
}

// "API values at 1 USD = 0.9048 EUR (13 Nov)." — the rate projects.rs converted
// the page's API values with. Its currency is the rate's own, and it writes the
// value exactly as sent (Rust rounded it to four decimals). When the range's
// days had more than one rate, the line says so and names the latest: "API
// values at each day's rate; latest 1 USD = …".
function usdRateLine(rate) {
  if (
    !isObject(rate) ||
    typeof rate.value !== "number" ||
    !Number.isFinite(rate.value) ||
    rate.value <= 0
  ) {
    throw new Error(`the page's dollar rate is not a positive number: ${JSON.stringify(rate)}`);
  }
  if (typeof rate.currency !== "string" || !/^[A-Z]{3}$/.test(rate.currency)) {
    throw new Error(`the dollar rate's currency is missing: ${JSON.stringify(rate.currency)}`);
  }
  if (typeof rate.varies !== "boolean") {
    throw new Error(`the dollar rate's varies is missing: ${JSON.stringify(rate.varies)}`);
  }
  const value = String(rate.value);
  const date =
    rate.date === null || rate.date === undefined
      ? ""
      : ` (${shortDate(`${rate.date}T00:00:00`)})`;
  const latest = `1 USD = ${value} ${rate.currency}${date}`;
  return rate.varies
    ? `API values at each day's rate; latest ${latest}.`
    : `API values at ${latest}.`;
}

export async function show(container, env) {
  const data = await env.call("stats_projects");
  clear(container);

  const state = {
    mode: "org",
    detail: null,
    detailKey: null,
    detailError: null,
    detailEmpty: false,
    detailToken: 0,
  };

  const root = h("div", { class: "g stats-projects" });
  const treeCard = h("div", { class: "hcard stats-projects-tree" });
  const matrixCard = h("div", { class: "hcard stats-projects-matrix" });
  const detailCard = h("div", { class: "hcard stats-projects-detail" });
  root.appendChild(treeCard);
  root.appendChild(matrixCard);
  root.appendChild(detailCard);
  container.appendChild(root);

  function modeSwitch() {
    const switchNode = h("div", { class: "stats-tabs stats-mode", role: "group", "aria-label": "Tree" });
    for (const [id, label] of [
      ["org", "By organization"],
      ["plain", "Plain folder tree"],
    ]) {
      switchNode.appendChild(
        h(
          "button",
          {
            type: "button",
            class: `stats-tab${state.mode === id ? " stats-tab-on" : ""}`,
            "aria-pressed": state.mode === id ? "true" : "false",
            onclick: () => {
              state.mode = id;
              paint();
            },
          },
          label
        )
      );
    }
    return switchNode;
  }

  function treeRows() {
    const list = h("div", { class: "stats-tree" });
    const head = h("div", { class: "stats-tree-row stats-tree-row-head" }, [
      h("span", { text: "Organization · project · folder" }),
      h("span", { class: "n", text: "Tokens" }),
      h("span", { class: "n", text: "Share" }),
      h("span", {}),
      h("span", { class: "n", text: "Subscr." }),
      h("span", { class: "n", text: "API value" }),
      h("span", { text: "By subscription" }),
      h("span", { class: "n", text: "Sess." }),
    ]);
    list.appendChild(head);

    for (const { node, index, level } of visibleRows(data.nodes, state.mode)) {
      const group = hasChildren(data.nodes, index);
      const selected = node.key !== null && sameKey(node.key, state.detailKey);
      const classes = ["stats-tree-row", node.kind === "org" ? "stats-tree-row-org" : "", selected ? "stats-tree-row-selected" : ""];
      const row = h("div", { class: classes.filter(Boolean).join(" ") });

      const nameCell = h("span", { class: "stats-tree-name" });
      nameCell.style.paddingLeft = `${level * 16}px`;
      if (group) {
        const open = isNodeOpen(node);
        const caret = h("button", {
          type: "button",
          class: "stats-caret",
          "aria-label": open ? "Close" : "Open",
          text: open ? "▾" : "▸",
        });
        caret.addEventListener("click", (event) => {
          event.stopPropagation();
          folds.set(foldKey(node), !open);
          paint();
        });
        nameCell.appendChild(caret);
      } else {
        nameCell.appendChild(h("span", { class: "stats-caret-space" }));
      }
      nameCell.appendChild(
        h("span", { class: "stats-tree-title", text: node.name, title: node.name })
      );
      if (node.kind === "org") {
        for (const id of node.orgAccounts) {
          nameCell.appendChild(accountName(env.accounts, id));
        }
        if (node.orgAccounts.length === 0) {
          nameCell.appendChild(h("span", { class: "stats-tree-none", text: "· no organization" }));
        }
      }
      row.appendChild(nameCell);

      row.appendChild(showTokens(h("span", { class: "n" }), node.tokens.out));
      const share = shareText(node.share);
      row.appendChild(
        h("span", { class: "n mf", text: share.text, title: share.title || undefined })
      );
      const usage = h("span", { class: "stats-ubar" });
      usage.appendChild(
        h("i", {
          style: {
            width:
              typeof node.share === "number"
                ? `${Math.min(100, Math.max(0, node.share))}%`
                : "0%",
          },
        })
      );
      row.appendChild(usage);
      row.appendChild(
        h("span", { class: "n" }, [valueNode(node.values, "subscription", env.currency)])
      );
      row.appendChild(
        h("span", { class: "n mf" }, [valueNode(node.values, "api", env.currency)])
      );
      row.appendChild(splitBar(node, env));
      row.appendChild(h("span", { class: "n", text: String(node.sessions) }));

      if (node.kind === "project" && node.key) {
        row.classList.add("stats-tree-row-click");
        row.setAttribute("tabindex", "0");
        row.setAttribute("role", "button");
        row.title = `${node.name} — show its detail`;
        row.addEventListener("click", () => void selectDetail(node.key));
        row.addEventListener("keydown", (event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          void selectDetail(node.key);
        });
      }
      list.appendChild(row);
    }
    return list;
  }

  function notesBlock() {
    const notes = h("div", { class: "stats-tree-notes" });
    for (const note of data.notes) {
      notes.appendChild(h("p", { class: "stats-note", text: note }));
    }
    if (data.tokensPerPercent.length > 0) {
      const line = h("p", { class: "stats-note" }, ["Tokens per percent of a week: "]);
      for (const [index, [id, value]] of data.tokensPerPercent.entries()) {
        if (index > 0) line.appendChild(document.createTextNode(" · "));
        line.appendChild(accountName(env.accounts, id));
        line.appendChild(document.createTextNode(` ${tokens(value)}`));
      }
      line.appendChild(document.createTextNode("."));
      notes.appendChild(line);
    }
    if (data.withoutPrice.length > 0) {
      notes.appendChild(
        h("p", {
          class: "stats-note",
          text: `No price for ${data.withoutPrice.join(", ")}: their tokens add no value; shares count priced work only.`,
        })
      );
    }
    if (data.usdRate !== null && data.usdRate !== undefined) {
      notes.appendChild(
        h("p", { class: "stats-note", text: usdRateLine(data.usdRate) })
      );
    }
    if (
      data.notes.length === 0 &&
      data.tokensPerPercent.length === 0 &&
      data.withoutPrice.length === 0 &&
      (data.usdRate === null || data.usdRate === undefined)
    ) {
      notes.appendChild(emptyLine("no note for this range"));
    }
    return notes;
  }

  function paintTree() {
    clear(treeCard);
    treeCard.appendChild(
      h("div", { class: "ch" }, [
        h("span", { class: "t", text: "What the subscriptions were used for" }),
        h("span", { class: "s", text: "click a project row for its detail" }),
        h("span", { class: "sp" }),
        modeSwitch(),
      ])
    );
    const body = h("div", { class: "stats-tree-body" });
    body.appendChild(data.nodes.length === 0 ? emptyLine("no session in the range") : treeRows());
    body.appendChild(notesBlock());
    treeCard.appendChild(body);
  }

  function paintMatrix() {
    clear(matrixCard);
    matrixCard.appendChild(
      h("div", { class: "ch" }, [
        h("span", { class: "t", text: "Organization × subscription" }),
        h("span", { class: "s", text: "subscription value" }),
      ])
    );
    const body = h("div", { class: "stats-matrix-body" });
    if (data.matrix.subscriptions.length === 0) {
      body.appendChild(emptyLine("no session in the range"));
      matrixCard.appendChild(body);
      return;
    }
    const table = h("div", { class: "stats-matrix" });
    const width = `minmax(84px, 1fr) repeat(${data.matrix.subscriptions.length}, minmax(0, 1fr)) minmax(46px, auto)`;
    table.style.setProperty("--stats-matrix-columns", width);
    const head = h("div", { class: "stats-matrix-row stats-matrix-row-head" });
    head.appendChild(h("span", { text: "Organization" }));
    for (const id of data.matrix.subscriptions) {
      head.appendChild(h("span", { class: "stats-matrix-head-cell" }, [accountName(env.accounts, id)]));
    }
    head.appendChild(
      h("span", {
        class: "stats-matrix-head-total",
        text: typeof env.currency === "string" ? env.currency : "",
      })
    );
    table.appendChild(head);
    for (const [rowIndex, org] of data.matrix.orgs.entries()) {
      const cells = data.matrix.cells[rowIndex];
      if (!Array.isArray(cells) || cells.length !== data.matrix.subscriptions.length) {
        throw new Error(
          `the matrix row ${JSON.stringify(org)} does not match the subscriptions`
        );
      }
      const row = h("div", { class: "stats-matrix-row" });
      row.appendChild(h("span", { class: "stats-matrix-org", text: org, title: org }));
      for (const [column, id] of data.matrix.subscriptions.entries()) {
        const value = cells[column];
        const view = accountView(env.accounts, id);
        const color = view && isColor(view.color) ? view.color : "var(--muted)";
        const style = { "--stats-matrix-color": color };
        if (value === null) {
          row.appendChild(
            h("span", {
              class: "stats-matrix-cell stats-matrix-cell-none",
              text: "—",
              title: CELL_UNKNOWN,
              style,
            })
          );
        } else if (typeof value === "number" && Number.isFinite(value)) {
          row.appendChild(
            h("span", {
              class: "stats-matrix-cell",
              title: `${org} on ${accountNameText(env.accounts, id)}: ${amountTip(
                value,
                env.currency
              )}`,
              style,
            }, [amountNode(value, env.currency)])
          );
        } else {
          throw new Error(
            `the matrix cell of ${JSON.stringify(org)} and ${JSON.stringify(id)} is not a number or null: ${JSON.stringify(value)}`
          );
        }
      }
      row.appendChild(
        h("span", {
          class: "stats-matrix-total",
        }, [valueNode(data.matrix.rowTotals[rowIndex], "subscription", env.currency)])
      );
      table.appendChild(row);
    }
    if (data.matrix.orgs.length > 0) {
      const foot = h("div", { class: "stats-matrix-row stats-matrix-row-foot" });
      foot.appendChild(h("span", { class: "stats-matrix-org", text: "Used" }));
      for (const [column, id] of data.matrix.subscriptions.entries()) {
        foot.appendChild(
          h("span", {
            class: "stats-matrix-total",
            title: `${accountNameText(env.accounts, id)}: used in the range`,
          }, [valueNode(data.matrix.columnTotals[column], "subscription", env.currency)])
        );
      }
      foot.appendChild(
        h("span", {
          class: "stats-matrix-total",
          title: "every organization: used in the range",
        }, [valueNode(data.matrix.total, "subscription", env.currency)])
      );
      table.appendChild(foot);
    }
    body.appendChild(table);
    if (data.matrix.sentences.length === 0) {
      body.appendChild(emptyLine("no sentence for this range"));
    } else {
      const sentences = h("ul", { class: "stats-findings stats-matrix-sentences" });
      for (const sentence of data.matrix.sentences) {
        sentences.appendChild(h("li", { text: sentence }));
      }
      body.appendChild(sentences);
    }
    matrixCard.appendChild(body);
  }

  function detailBody() {
    if (state.detailError !== null) {
      if (state.detailEmpty) return h("p", { class: "stats-empty", text: state.detailError });
      return banner("error", "⚠", state.detailError);
    }
    if (state.detail === null) return emptyLine("no project in the range");
    const detail = state.detail;
    const card = h("div", { class: "stats-detail-body" });
    const kpis = h("div", { class: "stats-mini-kpis" });
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Output" }),
        showTokens(h("span", { class: "stats-kpi-value" }), detail.tokens.out),
        h("span", {
          class: "stats-kpi-sub",
          text: `${percentText(detail.agentShare)} from agents`,
        }),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Cache read" }),
        showTokens(h("span", { class: "stats-kpi-value" }), detail.tokens.cr),
        h("span", { class: "stats-kpi-sub", text: `written ${tokens(detail.tokens.cw)}` }),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Value" }),
        h("span", { class: "stats-kpi-value" }, [
          valueNode(detail.values, "subscription", env.currency),
        ]),
        h("span", { class: "stats-kpi-sub" }, [
          "at API prices ",
          valueNode(detail.values, "api", env.currency),
        ]),
      ])
    );
    card.appendChild(kpis);

    // The detail's subscriptions as the mock-up lays them out: the tag, a bar
    // as long as the share, its percent. projects.rs carries shares, not
    // amounts, so this card has no amount column (a default of this run).
    card.appendChild(h("p", { class: "stats-detail-label", text: "By subscription" }));
    if (detail.bySubscription.length === 0) {
      card.appendChild(emptyLine("no subscription share for this project"));
    } else {
      const subs = h("div", { class: "stats-detail-subs" });
      for (const [id, percent] of detail.bySubscription) {
        const name = id === null ? "unknown account" : accountNameText(env.accounts, id);
        const tag =
          id === null
            ? h("span", {
                class: "stats-acc stats-acc-unknown",
                text: "unknown account",
                title: "work without a known subscription",
              })
            : accountName(env.accounts, id);
        subs.appendChild(
          h("div", { class: "stats-detail-sub" }, [
            tag,
            subscriptionSplit([[id, percent]], env.accounts, () =>
              `${name} · ${percentText(percent)} of this project's API value`),
            h("span", { class: "n", text: percentText(percent) }),
          ])
        );
      }
      card.appendChild(subs);
    }

    card.appendChild(h("p", { class: "stats-detail-label", text: "Hosts" }));
    if (detail.hosts.length === 0) {
      card.appendChild(emptyLine("no host recorded for this project"));
    } else {
      const hosts = h("div", { class: "stats-chipline" });
      for (const [host, percent] of detail.hosts) {
        hosts.appendChild(
          h("span", { class: "stats-host-share", title: `${host}: ${percentText(percent)}` }, [
            h("span", { text: host }),
            h("span", { class: "stats-top-bar" }, [
              h("i", { style: { width: `${Math.min(100, Math.max(0, percent))}%` } }),
            ]),
            h("span", { class: "stats-top-value", text: percentText(percent) }),
          ])
        );
      }
      card.appendChild(hosts);
    }

    card.appendChild(
      h("p", { class: "stats-detail-label", text: "Output tokens per week, by model" })
    );
    const models = new Map();
    for (const [, rows] of detail.weeklyByModel) {
      for (const [model] of rows) {
        if (!models.has(model)) models.set(model, modelColor(model));
      }
    }
    const series = [...models.entries()].map(([model, color]) => ({
      name: modelName(model),
      color,
      points: detail.weeklyByModel.map(([week, rows]) => {
        const found = rows.find(([name]) => name === model);
        return [week, found ? found[1] : 0];
      }),
      tip: (time, value) =>
        `${modelName(model)} | week from ${dateText(new Date(time))}: ${tokens(Math.round(value))}`,
    }));
    const chart = h("div", { class: "stats-detail-chart stats-model-chart" });
    if (series.length === 0) chart.appendChild(emptyLine("no session in the range"));
    else chart.appendChild(lines({ series, yFormat: (value) => tokens(Math.round(value)), yDecimals: 0 }));
    card.appendChild(chart);
    return card;
  }

  function paintDetail() {
    clear(detailCard);
    const head = h("div", { class: "ch" });
    if (state.detail !== null) {
      head.appendChild(
        h("span", { class: "t", text: state.detail.name, title: state.detail.name })
      );
      const share = shareText(state.detail.share);
      head.appendChild(
        h("span", {
          class: "s",
          text: `${share.text} of all use · ${state.detail.sessions} sessions`,
          title: share.title || undefined,
        })
      );
    } else {
      head.appendChild(h("span", { class: "t", text: "Project" }));
    }
    detailCard.appendChild(head);
    detailCard.appendChild(detailBody());
  }

  function paint() {
    paintTree();
    paintMatrix();
    paintDetail();
  }

  async function selectDetail(key) {
    const token = ++state.detailToken;
    try {
      const detail = await env.call("stats_project_detail", { key });
      if (token !== state.detailToken) return;
      state.detail = detail;
      state.detailKey = key;
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
  const first = data.nodes.find((node) => node.kind === "project" && node.key !== null);
  if (first) await selectDetail(first.key);
}
