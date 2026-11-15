// The Projects page (spec §3.6): the tree of organizations, projects and
// folders, the organization × account matrix with its sentences, and one
// project's detail. The page formats and draws the struct; it adds no sum,
// share or sentence of the struct's own.

import {
  accountName,
  accountView,
  amountText,
  banner,
  clear,
  dateText,
  errorMessage,
  h,
  isColor,
  percentText,
  tokens,
} from "../../common.js";
import { lines } from "../../charts.js";

const MODEL_TOKENS = [
  "var(--focus)",
  "var(--warn)",
  "var(--pill-text)",
  "var(--danger)",
  "var(--muted)",
];

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

function visibleRows(nodes, mode, collapsed) {
  const rows = [];
  const stack = [];
  for (let index = 0; index < nodes.length; index += 1) {
    const node = nodes[index];
    while (stack.length > 0 && stack[stack.length - 1].level >= node.level) stack.pop();
    const hidden = stack.some((entry) => collapsed.has(entry.index));
    if (!hidden && !(mode === "plain" && node.kind === "org")) {
      rows.push({
        node,
        index,
        level: mode === "plain" ? Math.max(0, node.level - 1) : node.level,
      });
    }
    if (hasChildren(nodes, index)) stack.push({ index, level: node.level });
  }
  return rows;
}

function splitBar(node, env) {
  const bar = h("span", { class: "stats-split" });
  for (const [id, percent] of node.byAccount) {
    const view = id === null ? null : accountView(env.accounts, id);
    const color = view && isColor(view.color) ? view.color : "var(--muted)";
    const name = view ? view.abbr : id === null ? "unknown account" : id;
    const part = h("i", {
      style: { width: `${Math.min(100, Math.max(0, percent))}%`, background: color },
      title: `${name}: ${percentText(percent)}`,
    });
    bar.appendChild(part);
  }
  return bar;
}

export async function show(container, env) {
  const data = await env.call("stats_projects");
  clear(container);

  const state = {
    mode: "org",
    collapsed: new Set(),
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
      h("span", { text: "By account" }),
      h("span", { class: "n", text: "Sess." }),
    ]);
    list.appendChild(head);

    for (const { node, index, level } of visibleRows(data.nodes, state.mode, state.collapsed)) {
      const group = hasChildren(data.nodes, index);
      const selected = node.key !== null && sameKey(node.key, state.detailKey);
      const classes = ["stats-tree-row", node.kind === "org" ? "stats-tree-row-org" : "", selected ? "stats-tree-row-selected" : ""];
      const row = h("div", { class: classes.filter(Boolean).join(" ") });

      const nameCell = h("span", { class: "stats-tree-name" });
      nameCell.style.paddingLeft = `${level * 16}px`;
      if (group) {
        const caret = h("button", {
          type: "button",
          class: "stats-caret",
          "aria-label": state.collapsed.has(index) ? "Open" : "Close",
          text: state.collapsed.has(index) ? "▸" : "▾",
        });
        caret.addEventListener("click", (event) => {
          event.stopPropagation();
          if (state.collapsed.has(index)) state.collapsed.delete(index);
          else state.collapsed.add(index);
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

      row.appendChild(h("span", { class: "n", text: tokens(node.tokens.out) }));
      row.appendChild(h("span", { class: "n mf", text: percentText(node.share) }));
      const usage = h("span", { class: "stats-ubar" });
      usage.appendChild(
        h("i", { style: { width: `${Math.min(100, Math.max(0, node.share))}%` } })
      );
      row.appendChild(usage);
      row.appendChild(
        h("span", { class: "n", text: amountText(node.subscription, env.currency) })
      );
      row.appendChild(h("span", { class: "n mf", text: amountText(node.api, env.currency) }));
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
    for (const note of Array.isArray(data.notes) ? data.notes : []) {
      notes.appendChild(h("p", { class: "stats-note", text: note }));
    }
    const perPercent = Array.isArray(data.tokensPerPercent) ? data.tokensPerPercent : [];
    if (perPercent.length > 0) {
      const line = h("p", { class: "stats-note" }, ["Tokens per percent of a week: "]);
      for (const [index, [id, value]] of perPercent.entries()) {
        if (index > 0) line.appendChild(document.createTextNode(" · "));
        line.appendChild(accountName(env.accounts, id));
        line.appendChild(document.createTextNode(` ${tokens(value)}`));
      }
      line.appendChild(document.createTextNode("."));
      notes.appendChild(line);
    }
    if (Array.isArray(data.withoutPrice) && data.withoutPrice.length > 0) {
      notes.appendChild(
        h("p", {
          class: "stats-note",
          text: `No price for ${data.withoutPrice.join(", ")}: their tokens add no value.`,
        })
      );
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
    body.appendChild(treeRows());
    body.appendChild(notesBlock());
    treeCard.appendChild(body);
  }

  function paintMatrix() {
    clear(matrixCard);
    matrixCard.appendChild(
      h("div", { class: "ch" }, [
        h("span", { class: "t", text: "Organization × account" }),
        h("span", { class: "s", text: "subscription value" }),
      ])
    );
    const body = h("div", { class: "stats-matrix-body" });
    const table = h("div", { class: "stats-matrix" });
    const width = `minmax(84px, 1fr)${
      data.matrix.accounts.length > 0
        ? ` repeat(${data.matrix.accounts.length}, minmax(0, 1fr))`
        : ""
    }`;
    table.style.setProperty("--stats-matrix-columns", width);
    const head = h("div", { class: "stats-matrix-row stats-matrix-row-head" });
    head.appendChild(h("span", { text: "Organization" }));
    for (const id of data.matrix.accounts) {
      const view = accountView(env.accounts, id);
      head.appendChild(h("span", { class: "stats-matrix-head-cell" }, [accountName(env.accounts, id)]));
    }
    table.appendChild(head);
    for (const [rowIndex, org] of data.matrix.orgs.entries()) {
      const row = h("div", { class: "stats-matrix-row" });
      row.appendChild(h("span", { class: "stats-matrix-org", text: org, title: org }));
      for (const [column, id] of data.matrix.accounts.entries()) {
        const value = data.matrix.cells[rowIndex][column];
        const view = accountView(env.accounts, id);
        const color = view && isColor(view.color) ? view.color : "var(--muted)";
        const cell = h("span", {
          class: "stats-matrix-cell",
          text: amountText(value, env.currency),
          title: `${org} on ${view ? view.abbr : id}: ${amountText(value, env.currency)}`,
          style: { "--stats-matrix-color": color },
        });
        row.appendChild(cell);
      }
      table.appendChild(row);
    }
    body.appendChild(table);
    const sentences = h("ul", { class: "stats-findings stats-matrix-sentences" });
    for (const sentence of data.matrix.sentences) {
      sentences.appendChild(h("li", { text: sentence }));
    }
    body.appendChild(sentences);
    matrixCard.appendChild(body);
  }

  function detailBody() {
    if (state.detailError !== null) {
      if (state.detailEmpty) return h("p", { class: "stats-empty", text: state.detailError });
      return banner("error", "⚠", state.detailError);
    }
    if (state.detail === null) return h("p", { class: "stats-empty", text: "—" });
    const detail = state.detail;
    const card = h("div", { class: "stats-detail-body" });
    const kpis = h("div", { class: "stats-mini-kpis" });
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Output" }),
        h("span", { class: "stats-kpi-value", text: tokens(detail.tokens.out) }),
        h("span", {
          class: "stats-kpi-sub",
          text: `${percentText(detail.agentShare)} from agents`,
        }),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Cache read" }),
        h("span", { class: "stats-kpi-value", text: tokens(detail.tokens.cr) }),
        h("span", { class: "stats-kpi-sub", text: `written ${tokens(detail.tokens.cw)}` }),
      ])
    );
    kpis.appendChild(
      h("div", { class: "stats-mini-kpi" }, [
        h("span", { class: "stats-kpi-label", text: "Value" }),
        h("span", { class: "stats-kpi-value", text: amountText(detail.subscription, env.currency) }),
        h("span", {
          class: "stats-kpi-sub",
          text: `at API prices ${amountText(detail.api, env.currency)}`,
        }),
      ])
    );
    card.appendChild(kpis);

    if (detail.hosts.length > 0) {
      card.appendChild(h("p", { class: "stats-detail-label", text: "Hosts" }));
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
      h("p", { class: "stats-detail-label", text: "Output tokens per week, by model (millions)" })
    );
    const models = new Map();
    for (const [, rows] of detail.weeklyByModel) {
      for (const [model] of rows) {
        if (!models.has(model)) models.set(model, MODEL_TOKENS[models.size % MODEL_TOKENS.length]);
      }
    }
    const series = [...models.entries()].map(([model, color]) => ({
      name: model,
      color,
      points: detail.weeklyByModel.map(([week, rows]) => {
        const found = rows.find(([name]) => name === model);
        return [week, found ? found[1] / 1000000 : 0];
      }),
      tip: (time, value) =>
        `${model} | week from ${dateText(new Date(time))}: ${tokens(
          Math.round(value * 1000000)
        )}`,
    }));
    const chart = h("div", { class: "stats-detail-chart stats-model-chart" });
    chart.appendChild(lines({ series, ySuffix: " M", yDecimals: 0 }));
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
      head.appendChild(
        h("span", { class: "s", text: `${state.detail.sessions} sessions` })
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
  const atlas = data.nodes.find(
    (node) => node.kind === "project" && node.name === "atlas" && node.key !== null
  );
  const first = atlas || data.nodes.find((node) => node.kind === "project" && node.key !== null);
  if (first) await selectDetail(first.key);
}
