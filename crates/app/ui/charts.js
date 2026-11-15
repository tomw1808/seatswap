// The statistics pages' charts (step-c3 Task 8). Plain SVG and plain elements,
// no chart library. Every colour is a CSS token or the colour the caller hands
// in (an account's colour is its view's colour). The one tooltip of every chart
// lives here: a mark with a `tip` shows it on hover and on keyboard focus.

import { clear, dateText, h } from "./common.js";

const SVG_NS = "http://www.w3.org/2000/svg";

// ---------------------------------------------------------------------------
// the one tooltip
// ---------------------------------------------------------------------------

let tipNode = null;

function ensureTip() {
  if (!tipNode && typeof document !== "undefined") {
    tipNode = h("div", { class: "chart-tip", role: "tooltip", hidden: true });
    document.body.appendChild(tipNode);
  }
  return tipNode;
}

function placeTip(anchor, event) {
  const node = ensureTip();
  if (!node) return;
  const gap = 10;
  let x;
  let y;
  if (event && Number.isFinite(event.clientX) && Number.isFinite(event.clientY)) {
    x = event.clientX + gap;
    y = event.clientY + gap;
  } else if (anchor && typeof anchor.getBoundingClientRect === "function") {
    const box = anchor.getBoundingClientRect();
    x = box.left + box.width / 2;
    y = box.top;
  } else {
    x = gap;
    y = gap;
  }
  node.style.left = `${x}px`;
  node.style.top = `${y}px`;
  const box = node.getBoundingClientRect();
  const width = typeof window === "undefined" ? 280 : window.innerWidth;
  const height = typeof window === "undefined" ? 800 : window.innerHeight;
  if (box.right > width - gap) node.style.left = `${Math.max(gap, width - box.width - gap)}px`;
  if (box.bottom > height - gap) node.style.top = `${Math.max(gap, y - box.height - gap * 2)}px`;
}

function showTip(text, anchor, event) {
  if (text === null || text === undefined || text === "") return;
  const node = ensureTip();
  if (!node) return;
  clear(node);
  const parts = String(text).split("|");
  for (const [index, part] of parts.entries()) {
    node.appendChild(
      h("div", {
        class: index === 0 ? "chart-tip-title" : "chart-tip-line",
        text: part.trim(),
      })
    );
  }
  node.hidden = false;
  placeTip(anchor, event);
}

function hideTip() {
  if (tipNode) tipNode.hidden = true;
}

// A mark that carries a tip: hover and keyboard focus both show it.
function attachTip(node, text) {
  if (text === null || text === undefined || text === "") return node;
  if (!node.hasAttribute("tabindex")) node.setAttribute("tabindex", "0");
  node.addEventListener("mouseenter", (event) => showTip(text, node, event));
  node.addEventListener("mousemove", (event) => placeTip(node, event));
  node.addEventListener("mouseleave", hideTip);
  node.addEventListener("focus", () => showTip(text, node));
  node.addEventListener("blur", hideTip);
  return node;
}

function clickable(node, onClick) {
  if (typeof onClick !== "function") return node;
  node.classList.add("chart-click");
  node.addEventListener("click", onClick);
  node.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    onClick(event);
  });
  if (!node.hasAttribute("tabindex")) node.setAttribute("tabindex", "0");
  if (!node.hasAttribute("role")) node.setAttribute("role", "button");
  return node;
}

// ---------------------------------------------------------------------------
// small shared drawing helpers
// ---------------------------------------------------------------------------

function svgNode(tag, attributes) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attributes || {})) {
    if (value === undefined || value === null) continue;
    node.setAttribute(key, String(value));
  }
  return node;
}

function finite(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function timeOf(value) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (value instanceof Date) return value.getTime();
  const time = Date.parse(String(value));
  return Number.isNaN(time) ? null : time;
}

function clampPercent(value) {
  return Math.min(100, Math.max(0, value));
}

// The x axis of a lines chart: all valid times of all series.
function timeBounds(series) {
  let min = null;
  let max = null;
  for (const entry of series) {
    for (const point of Array.isArray(entry.points) ? entry.points : []) {
      if (!Array.isArray(point) || point.length < 2) continue;
      if (point[1] === null || point[1] === undefined) continue;
      const time = timeOf(point[0]);
      if (time === null) continue;
      if (min === null || time < min) min = time;
      if (max === null || time > max) max = time;
    }
  }
  return { min, max };
}

// Marks are placed at the left edge of the column whose week they fall in, as
// the mock-up does. Without column times a mark cannot be placed and is left
// out rather than guessed.
function markPosition(at, columnTimes) {
  const time = timeOf(at);
  if (time === null || columnTimes.length === 0) return null;
  let index = columnTimes.findIndex((value) => value > time);
  if (index < 0) index = columnTimes.length;
  return `${(index / columnTimes.length) * 100}%`;
}

function markLayer(marks, leftOf, className) {
  if (!Array.isArray(marks) || marks.length === 0) return null;
  const layer = h("div", { class: className || "chart-marks" });
  for (const mark of marks) {
    if (!mark || mark.at === undefined) continue;
    const left = leftOf(mark.at);
    if (left === null) continue;
    const line = h("div", { class: "chart-mark", style: { left } });
    line.appendChild(
      h("span", { class: "chart-mark-label", text: String(mark.label), title: String(mark.label) })
    );
    layer.appendChild(line);
  }
  return layer.childNodes.length > 0 ? layer : null;
}

// ---------------------------------------------------------------------------
// stackedColumns — the Overview's two stacks per week
// ---------------------------------------------------------------------------

export function stackedColumns({ columns, marks } = {}) {
  if (!Array.isArray(columns)) throw new Error("stackedColumns needs a list of columns");
  const root = h("div", { class: "chart chart-cols" });
  const markHost = h("div", { class: "chart-marks" });
  const body = h("div", { class: "chart-cols-body" });

  let maxTotal = 0;
  for (const column of columns) {
    const stacks = Array.isArray(column.stacks) ? column.stacks : [];
    for (const stack of stacks) {
      let total = 0;
      for (const segment of Array.isArray(stack) ? stack : []) {
        if (finite(segment && segment.value) && segment.value > 0) total += segment.value;
      }
      if (total > maxTotal) maxTotal = total;
    }
  }
  const scale = maxTotal > 0 ? maxTotal : 1;

  for (const column of columns) {
    const col = h("div", { class: "chart-col" });
    const pair = h("div", { class: "chart-pair" });
    const stacks = Array.isArray(column.stacks) ? column.stacks : [];
    for (const stack of stacks) {
      const stackNode = h("div", { class: "chart-stack" });
      for (const segment of Array.isArray(stack) ? stack : []) {
        if (!segment || !finite(segment.value) || segment.value <= 0) continue;
        const height = `${(segment.value / scale) * 100}%`;
        const style = { height, background: segment.color };
        if (segment.hatch) {
          style.background = `repeating-linear-gradient(135deg, ${segment.color} 0 2px, transparent 2px 5px)`;
          style.boxShadow = `inset 0 0 0 1px ${segment.color}`;
        }
        const piece = h("div", { class: "chart-stack-seg", style });
        attachTip(piece, segment.tip);
        stackNode.appendChild(piece);
      }
      pair.appendChild(stackNode);
    }
    col.appendChild(pair);
    col.appendChild(
      h("span", { class: "chart-col-label", text: String(column.label ?? ""), title: String(column.label ?? "") })
    );
    if (column.sub !== undefined && column.sub !== null && column.sub !== "") {
      col.appendChild(h("span", { class: "chart-col-sub", text: String(column.sub) }));
    }
    body.appendChild(col);
  }

  const columnTimes = columns
    .map((column) => timeOf(column.at))
    .filter((time) => time !== null);
  const layer = markLayer(marks, (at) => markPosition(at, columnTimes));
  if (layer) markHost.appendChild(layer);
  if (markHost.childNodes.length > 0) root.appendChild(markHost);
  root.appendChild(body);
  return root;
}

// ---------------------------------------------------------------------------
// lines — the expired share and the curves
// ---------------------------------------------------------------------------

export function lines({ series, yMax, marks, ySuffix, yDecimals } = {}) {
  if (!Array.isArray(series)) throw new Error("lines needs a list of series");
  const root = h("div", { class: "chart chart-lines" });
  const plot = h("div", { class: "chart-plot" });
  const svg = svgNode("svg", {
    class: "chart-svg",
    viewBox: "0 0 100 100",
    preserveAspectRatio: "none",
    "aria-hidden": "true",
  });
  const bounds = timeBounds(series);
  let dataMax = 0;
  for (const entry of series) {
    for (const point of Array.isArray(entry.points) ? entry.points : []) {
      if (Array.isArray(point) && finite(point[1]) && point[1] > dataMax) dataMax = point[1];
    }
  }
  const top = finite(yMax) && yMax > 0 ? yMax : dataMax > 0 ? dataMax : 1;
  const suffix = typeof ySuffix === "string" ? ySuffix : "";
  const decimals = Number.isInteger(yDecimals) ? yDecimals : 0;

  for (let step = 0; step <= 4; step += 1) {
    const value = (top / 4) * step;
    const bottom = (value / top) * 100;
    plot.appendChild(
      h("div", {
        class: "chart-gridline",
        style: { bottom: `${clampPercent(bottom)}%` },
      })
    );
    plot.appendChild(
      h("div", {
        class: "chart-y",
        style: { bottom: `${clampPercent(bottom)}%` },
        text: `${value.toFixed(decimals)}${suffix}`,
      })
    );
  }

  const xOf = (time) => {
    if (bounds.min === null || bounds.max === null) return 50;
    if (bounds.max === bounds.min) return 50;
    return ((time - bounds.min) / (bounds.max - bounds.min)) * 100;
  };
  const yOf = (value) => 100 - clampPercent((value / top) * 100);

  for (const entry of series) {
    const points = (Array.isArray(entry.points) ? entry.points : [])
      .filter((point) => Array.isArray(point) && point.length >= 2)
      .map((point) => ({ time: timeOf(point[0]), value: point[1] }))
      .filter((point) => point.time !== null)
      .sort((a, b) => a.time - b.time);

    let segment = [];
    const segments = [];
    for (const point of points) {
      if (!finite(point.value)) {
        if (segment.length > 0) segments.push(segment);
        segment = [];
        continue;
      }
      segment.push(point);
    }
    if (segment.length > 0) segments.push(segment);

    for (const run of segments) {
      const d = run
        .map((point, index) => `${index === 0 ? "M" : "L"}${xOf(point.time).toFixed(2)} ${yOf(point.value).toFixed(2)}`)
        .join(" ");
      const path = svgNode("path", {
        d,
        fill: "none",
        stroke: entry.color,
        "stroke-width": "1.6",
        "stroke-linejoin": "round",
        "stroke-linecap": "round",
        "vector-effect": "non-scaling-stroke",
      });
      svg.appendChild(path);
      if (entry.area) {
        const first = run[0];
        const last = run[run.length - 1];
        const area = svgNode("path", {
          d: `${d} L${xOf(last.time).toFixed(2)} 100 L${xOf(first.time).toFixed(2)} 100 Z`,
          fill: entry.color,
          "fill-opacity": "0.14",
          stroke: "none",
        });
        svg.insertBefore(area, path);
      }
    }

    for (const point of points) {
      if (!finite(point.value)) continue;
      const dot = h("button", {
        type: "button",
        class: "chart-point",
        style: {
          left: `${clampPercent(xOf(point.time))}%`,
          bottom: `${clampPercent(100 - yOf(point.value))}%`,
          "--chart-point-color": entry.color,
        },
        "aria-label": `${entry.name}: ${point.value}`,
      });
      const tip =
        typeof entry.tip === "function"
          ? entry.tip(point.time, point.value)
          : `${entry.name} | ${dateText(new Date(point.time))}: ${point.value}${suffix}`;
      attachTip(dot, tip);
      plot.appendChild(dot);
    }
  }

  const markTimes = [];
  if (bounds.min !== null && bounds.max !== null) markTimes.push(bounds.min, bounds.max);
  const layer = markLayer(
    marks,
    (at) => {
      const time = timeOf(at);
      if (time === null || bounds.min === null || bounds.max === null) return null;
      return `${clampPercent(time === bounds.min ? 0 : xOf(time))}%`;
    },
    "chart-marks chart-marks-lines"
  );
  if (layer) plot.appendChild(layer);
  plot.appendChild(svg);
  root.appendChild(plot);

  const axis = h("div", { class: "chart-xaxis" });
  if (bounds.min !== null && bounds.max !== null) {
    const middle = bounds.min + (bounds.max - bounds.min) / 2;
    const times = [bounds.min, middle, bounds.max].filter(
      (time, index, all) => index === 0 || time !== all[index - 1]
    );
    for (const time of times) {
      axis.appendChild(h("span", { text: dateText(new Date(time)) }));
    }
  }
  if (axis.childNodes.length > 0) root.appendChild(axis);
  return root;
}

// ---------------------------------------------------------------------------
// bars — one bar per category and series
// ---------------------------------------------------------------------------

export function bars({ categories, series } = {}) {
  if (!Array.isArray(categories)) throw new Error("bars needs a list of categories");
  if (!Array.isArray(series)) throw new Error("bars needs a list of series");
  const root = h("div", { class: "chart chart-bars" });
  const legend = h("div", { class: "chart-legend" });
  for (const entry of series) {
    legend.appendChild(
      h("span", { class: "chart-legend-item" }, [
        h("span", { class: "chart-legend-box", style: { background: entry.color } }),
        h("span", { text: String(entry.name ?? "") }),
      ])
    );
  }
  if (legend.childNodes.length > 0) root.appendChild(legend);

  const body = h("div", { class: "chart-bars-body" });
  let max = 0;
  for (const entry of series) {
    for (const value of Array.isArray(entry.values) ? entry.values : []) {
      if (finite(value) && value > max) max = value;
    }
  }
  const scale = max > 0 ? max : 1;
  for (const [index, category] of categories.entries()) {
    const group = h("div", { class: "chart-bar-group" });
    for (const entry of series) {
      const values = Array.isArray(entry.values) ? entry.values : [];
      const value = values[index];
      if (!finite(value)) continue;
      const bar = h("div", {
        class: "chart-bar",
        style: { height: `${(value / scale) * 100}%`, background: entry.color },
      });
      attachTip(bar, `${category} | ${entry.name}: ${value}`);
      group.appendChild(bar);
    }
    const col = h("div", { class: "chart-bar-col" }, [
      group,
      h("span", { class: "chart-col-label", text: String(category), title: String(category) }),
    ]);
    body.appendChild(col);
  }
  root.appendChild(body);
  return root;
}

// ---------------------------------------------------------------------------
// grid — the Weeks page's account × week grid
// ---------------------------------------------------------------------------

export function grid({ rows, headers } = {}) {
  if (!Array.isArray(rows)) throw new Error("grid needs a list of rows");
  if (!Array.isArray(headers)) throw new Error("grid needs a list of headers");
  const root = h("div", { class: "chart chart-grid" });
  const template = `minmax(96px, 1.3fr) repeat(${headers.length}, minmax(0, 1fr))`;
  root.style.setProperty("--chart-grid-columns", template);

  const head = h("div", { class: "chart-grid-row chart-grid-headrow" });
  head.appendChild(h("span", { class: "chart-grid-corner" }));
  for (const header of headers) {
    const spec = header !== null && typeof header === "object" ? header : { label: header };
    const node = h("div", {
      class: `chart-grid-head${spec.selected ? " chart-grid-head-selected" : ""}${spec.current ? " chart-grid-head-current" : ""}`,
      text: String(spec.label ?? ""),
      title: String(spec.label ?? ""),
    });
    attachTip(node, spec.tip);
    clickable(node, spec.onClick);
    head.appendChild(node);
  }
  root.appendChild(head);

  for (const row of rows) {
    const rowNode = h("div", { class: "chart-grid-row" });
    const label = h("div", { class: "chart-grid-label" });
    if (row && row.label !== undefined && row.label !== null) {
      if (typeof row.label === "string" || typeof row.label === "number") {
        label.appendChild(document.createTextNode(String(row.label)));
      } else {
        label.appendChild(row.label);
      }
    }
    rowNode.appendChild(label);
    const cells = row && Array.isArray(row.cells) ? row.cells : [];
    for (let index = 0; index < headers.length; index += 1) {
      const cell = cells[index];
      if (!cell) {
        rowNode.appendChild(h("div", { class: "chart-cell chart-cell-empty" }));
        continue;
      }
      const classes = ["chart-cell"];
      if (!finite(cell.fill)) classes.push("chart-cell-plain");
      if (cell.hatch) classes.push("chart-cell-hatch");
      if (cell.full) classes.push("chart-cell-full");
      if (cell.uncertain) classes.push("chart-cell-uncertain");
      if (cell.current) classes.push("chart-cell-current");
      const style = {};
      if (cell.color) style["--chart-cell-color"] = cell.color;
      if (cell.textColor) style["--chart-cell-text"] = cell.textColor;
      const node = h("div", { class: classes.join(" "), style });
      if (finite(cell.fill)) {
        node.appendChild(
          h("div", {
            class: "chart-cell-fill",
            style: { height: `${clampPercent(cell.fill)}%` },
          })
        );
      }
      if (cell.text !== undefined && cell.text !== null && cell.text !== "") {
        node.appendChild(h("span", { class: "chart-cell-text", text: String(cell.text) }));
      }
      attachTip(node, cell.tip);
      clickable(node, cell.onClick);
      rowNode.appendChild(node);
    }
    root.appendChild(rowNode);
  }
  return root;
}

export { attachTip, hideTip, showTip };
