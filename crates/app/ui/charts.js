// The statistics pages' charts (step-c3 Task 8). Plain SVG and plain elements,
// no chart library. Every colour is a CSS token or the colour the caller hands
// in (an account's colour is its view's colour). The one tooltip of every chart
// lives here: a mark with a `tip` shows it on hover and on keyboard focus.

import { WEEKDAYS, appendChildren, clear, dateText, h, monthText } from "./common.js";

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

// A time as milliseconds. An unreadable time is an error that names the value,
// never a point, block or mark quietly left out.
function timeOf(value) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (value instanceof Date) {
    const time = value.getTime();
    if (!Number.isNaN(time)) return time;
  } else {
    const time = Date.parse(String(value));
    if (!Number.isNaN(time)) return time;
  }
  throw new Error(`invalid time ${value instanceof Date ? String(value) : JSON.stringify(value)}`);
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
      if (min === null || time < min) min = time;
      if (max === null || time > max) max = time;
    }
  }
  return { min, max };
}

// ---------------------------------------------------------------------------
// fitting labels, numbers and marks to the space the chart has
// ---------------------------------------------------------------------------

// How much text a chart can show depends on the width it gets, and that is
// known only once it is in the page. `fitOnResize` runs a chart's fit when its
// width is first known and again whenever the width changes. A fit only shows,
// hides or rewords text and moves labels; it never changes the chart's own
// size, so it cannot start a resize loop.
const fits = new WeakMap();
const fittedWidths = new WeakMap();
let fitObserver = null;

function fitOnResize(root, fit) {
  fits.set(root, fit);
  if (fitObserver === null) {
    fitObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        // A chart not shown (a hidden tab, taken out of the page) has no width
        // to fit to; it is fitted when it is shown again.
        const width = Math.round(entry.contentRect.width);
        if (width === 0 || fittedWidths.get(entry.target) === width) continue;
        fittedWidths.set(entry.target, width);
        fits.get(entry.target)();
      }
    });
  }
  fitObserver.observe(root);
}

// The space between two labels on one line, in px.
const LABEL_GAP = 6;

// A label anchored at a point: a column's middle, a time on a track, a mark.
// It is centred on its anchor, or starts or ends there at the chart's edges
// (`data-align`, set by the fit).
function anchoredLabel(className, text) {
  const node = h("span", { class: `chart-axis-label${className ? ` ${className}` : ""}`, text });
  node.dataset.align = "middle";
  return node;
}

// Where a label lies with its text centred on its anchor, or turned inwards
// when centred it would cross the edge of `box`. The label's alignment is
// "middle" when this is called.
function labelExtent(node, box) {
  const rect = node.getBoundingClientRect();
  const width = rect.width;
  const anchor = rect.left + width / 2;
  if (rect.left < box.left) return { left: anchor, right: anchor + width, align: "start" };
  if (rect.right > box.right) return { left: anchor - width, right: anchor, align: "end" };
  return { left: rect.left, right: rect.right, align: "middle" };
}

function apart(a, b) {
  return a.right + LABEL_GAP <= b.left || b.right + LABEL_GAP <= a.left;
}

// Axis labels: `labels` is one { node, text, at, keep } per anchor, left to
// right; `at` is the anchor's time (or null), `keep` the rank of a label that
// shows before all others (2 the running week, 1 the chosen week, 0 none).
// The fit shows as many labels as fit without touching each other or leaving
// `bounds`: every one, else every second; then on a time axis the month
// starts ("Oct", a year at January), else every n-th. The labels shown in
// steps are counted from the newest one, so it always shows. A hidden label
// keeps its line, so every column keeps its height.
function layoutAxis(labels, bounds) {
  if (labels.length === 0) return;
  const box = bounds.getBoundingClientRect();
  const place = (texts) => {
    for (const [index, label] of labels.entries()) {
      label.node.textContent = texts[index];
      label.node.dataset.align = "middle";
    }
    return labels.map((label) => labelExtent(label.node, box));
  };
  const show = (extents, picked) => {
    const shown = new Set(picked);
    for (const [index, label] of labels.entries()) {
      label.node.dataset.align = extents[index].align;
      label.node.classList.toggle("chart-label-off", !shown.has(index));
    }
  };
  const together = (extents, picked) =>
    picked.every((index, at) => at === 0 || apart(extents[picked[at - 1]], extents[index]));

  const count = labels.length;
  const full = place(labels.map((label) => label.text));
  // The kept labels by their rank, highest first; one that would touch a
  // higher one is left out (the chosen week beside the running one).
  const kept = [];
  for (const index of [...labels.keys()]
    .filter((candidate) => labels[candidate].keep)
    .sort((a, b) => labels[b].keep - labels[a].keep)) {
    if (kept.every((other) => apart(full[other], full[index]))) kept.push(index);
  }
  kept.sort((a, b) => a - b);
  const everyNth = (step) =>
    [...labels.keys()].filter(
      (index) => kept.includes(index) || (!labels[index].keep && (count - 1 - index) % step === 0)
    );
  const timed = labels.every((label) => label.keep || label.at !== null);
  const steps = timed ? [1, 2] : [...labels.keys()].map((index) => index + 1);
  for (const step of steps) {
    const picked = everyNth(step);
    if (together(full, picked)) {
      show(full, picked);
      return;
    }
  }
  if (!timed) {
    show(full, kept);
    return;
  }
  // Month starts: the first column of each month, named by its month. The
  // newest first; a month label that would touch one already placed is left
  // out.
  let previous = null;
  const starts = [];
  const texts = labels.map((label, index) => {
    if (label.keep || label.at === null) return label.text;
    const date = new Date(label.at);
    const month = date.getFullYear() * 12 + date.getMonth();
    const start = previous === null || month !== previous;
    previous = month;
    if (!start) return label.text;
    starts.push(index);
    return monthText(date);
  });
  const extents = place(texts);
  const picked = [...kept];
  for (const index of starts.reverse()) {
    if (picked.every((other) => apart(extents[other], extents[index]))) picked.push(index);
  }
  show(extents, picked);
}

// Numbers inside or under columns (a cell's percent, a week's blocked hours),
// each in one or more forms, longest first ("11h blocked", "11h"). A group
// shows all its numbers in the longest form in which each fits its own
// column, else none, so a blank never stands beside a number and reads as
// "nothing". The tooltip of the column or cell always carries the number.
const numberForms = new WeakMap();

function numberNode(className, forms) {
  const list = (Array.isArray(forms) ? forms : [forms]).map(String);
  if (list.length === 0) throw new Error("a number needs at least one form");
  const node = h("span", { class: className, text: list[0] });
  numberForms.set(node, list);
  return node;
}

function fitNumbers(nodes) {
  const count = Math.max(0, ...nodes.map((node) => numberForms.get(node).length));
  for (let form = 0; form < count; form += 1) {
    for (const node of nodes) {
      const forms = numberForms.get(node);
      node.textContent = forms[Math.min(form, forms.length - 1)];
      node.classList.remove("chart-number-off");
    }
    // 1px free on each side, so a number never touches its column's edge.
    if (nodes.every((node) => node.scrollWidth + 2 <= node.parentElement.clientWidth)) return;
  }
  for (const node of nodes) node.classList.add("chart-number-off");
}

// Mark labels ("PR added", "auto-burn on"): each on the first line where it
// touches no other label; when neither of the two lines has room, a tick on
// its line whose tooltip names it. Labels at the chart's edges turn inwards.
function layoutMarks(layer) {
  const marks = [...layer.querySelectorAll(".chart-mark")];
  const labels = marks.map((mark) => mark.querySelector(".chart-mark-label"));
  for (const [index, label] of labels.entries()) {
    label.textContent = label.dataset.text;
    label.dataset.align = "middle";
    label.classList.remove("chart-mark-tick");
    marks[index].style.setProperty("--chart-mark-row", "0");
  }
  const box = layer.getBoundingClientRect();
  const anchors = marks.map((mark) => mark.getBoundingClientRect().left);
  const full = labels.map((label) => labelExtent(label, box));
  // A tick is 8px wide (charts.css `.chart-mark-tick`) and needs 1px beside it.
  const ticks = anchors.map((anchor) =>
    anchor - 5 < box.left
      ? { left: anchor, right: anchor + 9, align: "start" }
      : anchor + 5 > box.right
        ? { left: anchor - 9, right: anchor, align: "end" }
        : { left: anchor - 5, right: anchor + 5, align: "middle" }
  );
  const order = [...marks.keys()].sort((a, b) => anchors[a] - anchors[b]);
  const tick = new Set();
  // Place left to right; a label with no room on either line becomes a tick.
  // A tick with no room either turns the label it touches into a tick too,
  // and the placing starts again: labels only ever become ticks, so it ends.
  let placed = null;
  while (placed === null) {
    placed = new Map();
    const rows = [[], []];
    const overlaps = (extent, other, gap) =>
      !(other.right + gap <= extent.left || extent.right + gap <= other.left);
    const gapOf = (a, b) => (tick.has(a) || tick.has(b) ? 0 : LABEL_GAP);
    const freeRow = (index, extent) =>
      rows.findIndex((row) =>
        row.every((other) => !overlaps(extent, placed.get(other).extent, gapOf(index, other)))
      );
    for (const index of order) {
      let extent = tick.has(index) ? ticks[index] : full[index];
      let row = freeRow(index, extent);
      if (row === -1 && !tick.has(index)) {
        tick.add(index);
        extent = ticks[index];
        row = freeRow(index, extent);
      }
      if (row === -1) {
        const touched = [...placed.keys()].find(
          (other) => !tick.has(other) && overlaps(extent, placed.get(other).extent, 0)
        );
        if (touched !== undefined) {
          tick.add(touched);
          placed = null;
          break;
        }
        // Ticks of neighbouring weeks only: they share line 0.
        row = 0;
      }
      placed.set(index, { extent, row });
      rows[row].push(index);
    }
  }
  for (const [index, { extent, row }] of placed) {
    const label = labels[index];
    if (tick.has(index)) {
      label.textContent = "";
      label.classList.add("chart-mark-tick");
    }
    label.dataset.align = extent.align;
    marks[index].style.setProperty("--chart-mark-row", String(row));
  }
}

// The time axis of a track a page draws itself (the Subscriptions page's
// periods): one label per tick `{ at, text, left }` (`left` a CSS length on
// the track), fitted like every chart's axis.
export function timeAxis(track, ticks) {
  const labels = ticks.map((tick) => {
    const node = anchoredLabel("", String(tick.text));
    node.style.left = tick.left;
    track.appendChild(node);
    return { node, text: String(tick.text), at: timeOf(tick.at), keep: 0 };
  });
  fitOnResize(track, () => layoutAxis(labels, track));
}

// A column narrower than this (px, gap included) is drawn narrow: smaller gaps
// and corners (charts.css `.chart-narrow`).
const NARROW_COLUMN = 28;

// The axis label under each column of a chart of columns; the label row keeps
// its height when the fit hides the label.
function columnLabel(text, at) {
  const node = anchoredLabel("", text);
  node.title = text;
  return { slot: h("span", { class: "chart-col-label" }, [node]), label: { node, text, at, keep: 0 } };
}

// Marks are placed at the left edge of the column whose week they fall in, as
// the mock-up does. Without column times a mark cannot be placed and is left
// out rather than guessed.
function markPosition(at, columnTimes) {
  const time = timeOf(at);
  if (columnTimes.length === 0) return null;
  let index = columnTimes.findIndex((value) => value > time);
  if (index < 0) index = columnTimes.length;
  return `${(index / columnTimes.length) * 100}%`;
}

function markLayer(marks, leftOf, className) {
  if (!Array.isArray(marks) || marks.length === 0) return null;
  const layer = h("div", { class: className || "chart-marks" });
  for (const mark of marks) {
    if (!mark || mark.at === undefined) {
      throw new Error(`a mark needs its time, not ${JSON.stringify(mark)}`);
    }
    const left = leftOf(mark.at);
    if (left === null) {
      throw new Error(`cannot place the mark ${JSON.stringify(mark.label)}: the chart has no time axis`);
    }
    const line = h("div", { class: "chart-mark", style: { left } });
    const text = String(mark.label);
    const label = anchoredLabel("chart-mark-label", text);
    label.dataset.text = text;
    attachTip(label, text);
    line.appendChild(label);
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

  const columnTimes = columns.map((column) => timeOf(column.at));
  const labels = [];
  const subs = [];
  for (const [index, column] of columns.entries()) {
    const col = h("div", { class: "chart-col" });
    const pair = h("div", { class: "chart-pair" });
    // The line under the column: one text or its forms, longest first.
    const subForms =
      column.sub === undefined || column.sub === null || column.sub === ""
        ? null
        : Array.isArray(column.sub)
          ? column.sub
          : [column.sub];
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
        // The line under the column is hidden when the column is narrow; its
        // tooltips always carry it.
        const tip =
          subForms !== null && segment.tip !== undefined && segment.tip !== null && segment.tip !== ""
            ? `${segment.tip} | ${subForms[0]}`
            : segment.tip;
        attachTip(piece, tip);
        stackNode.appendChild(piece);
      }
      pair.appendChild(stackNode);
    }
    col.appendChild(pair);
    const { slot, label } = columnLabel(String(column.label ?? ""), columnTimes[index]);
    labels.push(label);
    col.appendChild(slot);
    if (subForms !== null) {
      const sub = numberNode("chart-col-sub", subForms);
      subs.push(sub);
      col.appendChild(sub);
    }
    body.appendChild(col);
  }

  const layer = markLayer(marks, (at) => markPosition(at, columnTimes), "chart-marks chart-marks-cols");
  // The marks come after the columns so their labels lie above them, as in the
  // mock-up (Task 8, part 3, item 7).
  root.appendChild(body);
  if (layer) root.appendChild(layer);
  fitOnResize(root, () => {
    root.classList.toggle("chart-narrow", body.clientWidth / Math.max(1, columns.length) < NARROW_COLUMN);
    layoutAxis(labels, body);
    fitNumbers(subs);
    if (layer) layoutMarks(layer);
  });
  return root;
}

// ---------------------------------------------------------------------------
// lines — the expired share and the curves
// ---------------------------------------------------------------------------

export function lines({ series, yMax, marks, hlines, ySuffix, yDecimals, yFormat } = {}) {
  if (!Array.isArray(series)) throw new Error("lines needs a list of series");
  if (hlines !== undefined && !Array.isArray(hlines)) {
    throw new Error("lines needs a list of reference lines");
  }
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
        text:
          typeof yFormat === "function"
            ? String(yFormat(value))
            : `${value.toFixed(decimals)}${suffix}`,
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
      // A likely range is drawn as a dashed line without its own point markers.
      if (entry.dash === true) path.setAttribute("stroke-dasharray", "4 3");
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
      if (entry.dots === false) continue;
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
  // Horizontal reference lines (`{ y, label }`): the 100 % line of a window.
  // A line outside the chart's 0..top scale is an error, never moved to fit.
  for (const line of Array.isArray(hlines) ? hlines : []) {
    if (!line || !finite(line.y)) {
      throw new Error(`a reference line needs a finite y, not ${JSON.stringify(line)}`);
    }
    if (line.y < 0 || line.y > top) {
      throw new Error(
        `a reference line at ${line.y} lies outside the chart's 0..${top}`
      );
    }
    const node = h("div", {
      class: "chart-hline",
      style: { bottom: `${(line.y / top) * 100}%` },
    });
    if (line.label !== undefined && line.label !== null) {
      node.appendChild(
        h("span", { class: "chart-hline-label", text: String(line.label) })
      );
    }
    plot.appendChild(node);
  }
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
  // Points closer than this (px) are drawn only on hover and focus: the line
  // shows the course, and each point still answers with its tooltip.
  const densePoints = 14;
  const mostPoints = Math.max(
    0,
    ...series.map((entry) => (Array.isArray(entry.points) ? entry.points.length : 0))
  );
  fitOnResize(root, () => {
    root.classList.toggle(
      "chart-lines-dense",
      mostPoints > 1 && plot.clientWidth / (mostPoints - 1) < densePoints
    );
    if (layer) layoutMarks(layer);
  });

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

export function bars({ categories, series, yMax, labels, yLabel, yTicks } = {}) {
  if (!Array.isArray(categories)) throw new Error("bars needs a list of categories");
  if (!Array.isArray(series)) throw new Error("bars needs a list of series");
  if (yMax !== undefined && !(finite(yMax) && yMax > 0)) {
    throw new Error(`bars' yMax must be a finite number above 0, not ${JSON.stringify(yMax)}`);
  }
  if (labels !== undefined && typeof labels !== "function") {
    throw new Error(`bars' labels must be a function, not ${JSON.stringify(labels)}`);
  }
  if (yLabel !== undefined && typeof yLabel !== "function") {
    throw new Error(`bars' yLabel must be a function, not ${JSON.stringify(yLabel)}`);
  }
  if (yLabel !== undefined && yMax === undefined) {
    throw new Error("bars' yLabel needs a yMax to place the axis labels");
  }
  if (yTicks !== undefined && yMax === undefined) {
    throw new Error("bars' yTicks needs a yMax to place the axis lines");
  }
  if (yTicks !== undefined && !Array.isArray(yTicks)) {
    throw new Error(`bars' yTicks must be a list, not ${JSON.stringify(yTicks)}`);
  }
  if (yTicks !== undefined) {
    let previous = -Infinity;
    for (const value of yTicks) {
      if (!finite(value) || value < 0 || value > yMax || value <= previous) {
        throw new Error(
          `bars' yTicks must rise from 0 to its yMax ${yMax}, not ${JSON.stringify(yTicks)}`
        );
      }
      previous = value;
    }
  }
  const root = h("div", { class: "chart chart-bars" });
  const legend = h("div", { class: "chart-legend" });
  for (const entry of series) {
    const name = h("span", {});
    appendChildren(name, entry.name === undefined || entry.name === null ? "" : entry.name);
    legend.appendChild(
      h("span", { class: "chart-legend-item" }, [
        h("span", { class: "chart-legend-box", style: { background: entry.color } }),
        name,
      ])
    );
  }
  if (legend.childNodes.length > 0) root.appendChild(legend);

  let max = 0;
  for (const entry of series) {
    for (const value of Array.isArray(entry.values) ? entry.values : []) {
      if (finite(value) && value > max) max = value;
    }
  }
  const fixed = yMax === undefined ? null : yMax;
  const scale = fixed !== null ? fixed : max > 0 ? max : 1;
  // A fixed scale gets the lines chart's axis, but in the bars' own box: the
  // grid lines and their labels live in `.chart-bars-body`, the box the bars
  // grow in, so a line at value v lies at the height a bar of v reaches.
  // Without `yTicks`, the lines sit at 0, ¼, ½, ¾ and 1 of `yMax`.
  const ticks =
    fixed === null
      ? null
      : yTicks !== undefined
        ? yTicks
        : [0, fixed / 4, fixed / 2, (fixed * 3) / 4, fixed];
  const body = h("div", { class: "chart-bars-body" });
  if (ticks !== null) {
    for (const value of ticks) {
      const bottom = (value / fixed) * 100;
      body.appendChild(
        h("div", {
          class: "chart-gridline",
          style: { bottom: `${clampPercent(bottom)}%` },
        })
      );
      const text = typeof yLabel === "function" ? yLabel(value) : String(value);
      if (typeof text !== "string") {
        throw new Error(`bars' yLabel must return a string, not ${JSON.stringify(text)}`);
      }
      body.appendChild(
        h("div", {
          class: "chart-y",
          style: { bottom: `${clampPercent(bottom)}%` },
          text,
        })
      );
    }
  }
  // With a fixed scale the column labels wait in a row below the bars' box,
  // so that box is exactly the bars' height.
  const labelsRow = ticks !== null ? h("div", { class: "chart-bar-labels" }) : null;
  const columnLabels = [];
  for (const [index, category] of categories.entries()) {
    const group = h("div", { class: "chart-bar-group" });
    for (const entry of series) {
      const values = Array.isArray(entry.values) ? entry.values : [];
      const value = values[index];
      const missing = value === null;
      if (!missing && !finite(value)) {
        throw new Error(
          `bars' ${String(category)} value is not a number: ${JSON.stringify(value)}`
        );
      }
      if (missing && labels === undefined) continue;
      if (!missing && fixed !== null && value > fixed) {
        throw new Error(
          `bars' ${String(category)} value ${value} lies above the chart's yMax ${fixed}`
        );
      }
      const unit = typeof entry.unit === "string" ? entry.unit : "";
      const tip =
        typeof entry.tip === "function"
          ? entry.tip(category, missing ? null : value)
          : `${category} | ${entry.name}: ${missing ? null : value}${unit}`;
      if (labels === undefined) {
        const bar = h("div", {
          class: "chart-bar",
          style: { height: `${(value / scale) * 100}%`, background: entry.color },
        });
        attachTip(bar, tip);
        group.appendChild(bar);
        continue;
      }
      const text = labels(missing ? null : value);
      if (typeof text !== "string") {
        throw new Error(`bars' labels must return a string, not ${JSON.stringify(text)}`);
      }
      // The slot keeps the bar's height scale; the label sits above the bar,
      // or at the foot when there is no bar (a missing value, written "—").
      const slot = h("div", { class: "chart-bar-slot" });
      if (!missing) {
        const bar = h("div", {
          class: "chart-bar",
          style: { height: `${(value / scale) * 100}%`, background: entry.color },
        });
        attachTip(bar, tip);
        slot.appendChild(bar);
      }
      const label = h("span", {
        class: "chart-bar-label",
        style: { bottom: missing ? "2px" : `calc(${(value / scale) * 100}% + 3px)` },
        text,
      });
      if (missing) attachTip(label, tip);
      slot.appendChild(label);
      group.appendChild(slot);
    }
    // A category is a name, not a time: its labels are thinned in steps.
    const { slot, label } = columnLabel(String(category), null);
    columnLabels.push(label);
    if (labelsRow !== null) {
      body.appendChild(h("div", { class: "chart-bar-col" }, [group]));
      labelsRow.appendChild(slot);
    } else {
      body.appendChild(h("div", { class: "chart-bar-col" }, [group, slot]));
    }
  }
  if (labelsRow !== null) {
    root.appendChild(h("div", { class: "chart-bars-plot" }, [body]));
    root.appendChild(labelsRow);
  } else {
    root.appendChild(body);
  }
  const labelBounds = labelsRow !== null ? labelsRow : body;
  fitOnResize(root, () => {
    root.classList.toggle(
      "chart-narrow",
      body.clientWidth / Math.max(1, categories.length) < NARROW_COLUMN
    );
    layoutAxis(columnLabels, labelBounds);
  });
  return root;
}

// ---------------------------------------------------------------------------
// grid — the Weeks page's account × week grid
// ---------------------------------------------------------------------------

// A grid cell's filled height in percent: its `total`, else its `fill`, else
// the sum of its `parts`; `null` for a plain cell.
function cellTotal(cell) {
  if (finite(cell.total)) return cell.total;
  if (finite(cell.fill)) return cell.fill;
  if (Array.isArray(cell.parts)) {
    return cell.parts.reduce((sum, part) => sum + (finite(part) ? part : 0), 0);
  }
  return null;
}

// The fill percent from which a grid cell's number stands on the fill, not on
// the track: the number's foot padding and line take the lowest 40 % of the
// cell (`.chart-cell-text` in charts.css, 4 px + 13 px of 42 px).
const TEXT_ON_FILL = 40;

export function grid({ rows, headers } = {}) {
  if (!Array.isArray(rows)) throw new Error("grid needs a list of rows");
  if (!Array.isArray(headers)) throw new Error("grid needs a list of headers");
  const root = h("div", { class: "chart chart-grid" });
  // The column sizes live in charts.css; a grid wider than its card scrolls
  // sideways inside the chart, with the row labels kept in view.
  root.style.setProperty("--chart-grid-cols", String(headers.length));
  // A cell's track is its height for 100 %. A week with a reset sums its
  // windows and may pass 100: the bar then grows into the headroom above the
  // cell, while the 100 % line stays at the cell's top. The gap between rows
  // holds the largest overshoot, so a bar never covers the row above it.
  // `.chart-cell` is 42 px high (charts.css); the headroom is that scale.
  let highest = 100;
  for (const row of rows) {
    const cells = row && Array.isArray(row.cells) ? row.cells : [];
    for (const cell of cells) {
      if (!cell) continue;
      const total = cellTotal(cell);
      if (total !== null && total > highest) highest = total;
    }
  }
  root.style.setProperty("--chart-grid-over", String(highest - 100));

  const head = h("div", { class: "chart-grid-row chart-grid-headrow" });
  head.appendChild(h("span", { class: "chart-grid-corner" }));
  // A header may carry its week's time (`at`): a long run of weeks is then
  // labelled by its month starts. The running and the chosen week keep their
  // label.
  const labels = [];
  let selectedHead = null;
  for (const header of headers) {
    const spec = header !== null && typeof header === "object" ? header : { label: header };
    const text = String(spec.label ?? "");
    const label = anchoredLabel("", text);
    const node = h(
      "div",
      {
        class: `chart-grid-head${spec.selected ? " chart-grid-head-selected" : ""}${spec.current ? " chart-grid-head-current" : ""}`,
        title: spec.tip ? undefined : text,
      },
      [label]
    );
    labels.push({
      node: label,
      text,
      at: spec.at === undefined || spec.at === null ? null : timeOf(spec.at),
      keep: spec.current ? 2 : spec.selected ? 1 : 0,
    });
    if (spec.selected) selectedHead = node;
    attachTip(node, spec.tip);
    clickable(node, spec.onClick);
    head.appendChild(node);
  }
  root.appendChild(head);
  const barTexts = [];
  const plainTexts = [];

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
      if (cell.full) classes.push("chart-cell-full");
      if (cell.uncertain) classes.push("chart-cell-uncertain");
      if (cell.current) classes.push("chart-cell-current");
      const style = {};
      if (cell.color) style["--chart-cell-color"] = cell.color;
      // The number sits at the cell's foot (about its lowest 40 %): it takes
      // the fill's contrast colour only when the fill reaches over it, else it
      // stands on the empty track and keeps the page's text colour (a dark
      // number on the dark theme's track was unreadable).
      const total = cellTotal(cell);
      if (cell.textColor && total !== null && total >= TEXT_ON_FILL) {
        style["--chart-cell-text"] = cell.textColor;
      }
      const node = h("div", { class: classes.join(" "), style });
      const parts = Array.isArray(cell.parts) ? cell.parts : null;
      if (parts) {
        for (const part of parts) {
          if (!finite(part)) {
            throw new Error(`a cell's window part must be a finite percent, not ${JSON.stringify(part)}`);
          }
          if (part < 0) {
            throw new Error(`a cell's window part must be 0 or more, not ${JSON.stringify(part)}`);
          }
        }
      }
      const over = total !== null && total > 100;
      const splitBar = parts !== null && (parts.length > 1 || over);
      if (splitBar && total !== null) {
        // One part per window, oldest first, from the bottom up.
        const bar = h("div", {
          class: "chart-cell-bar",
          style: { height: `${total}%` },
        });
        const partClick =
          typeof cell.partClick === "function" && parts.length > 1 ? cell.partClick : null;
        for (const [index, part] of parts.entries()) {
          // A 0 % window used nothing: do not draw a 1 px part for it. The
          // cell's total and tooltip still count it.
          if (part === 0) continue;
          const partNode = h("div", {
            class: "chart-cell-part",
            style: { height: `${total === 0 ? 0 : (part / total) * 100}%` },
          });
          if (partClick !== null) {
            // A click on one part opens that window, not the cell's last one.
            clickable(partNode, (event) => {
              event.stopPropagation();
              partClick(index);
            });
          }
          bar.appendChild(partNode);
        }
        node.appendChild(bar);
        // The 100 % line stays at the top of the cell; the bar may pass it.
        node.appendChild(h("div", { class: "chart-cell-limit" }));
      } else if (total !== null) {
        node.appendChild(
          h("div", {
            class: "chart-cell-fill",
            style: { height: `${total}%` },
          })
        );
        if (over) node.appendChild(h("div", { class: "chart-cell-limit" }));
      }
      if (cell.expired !== undefined && cell.expired !== null) {
        if (!finite(cell.expired)) {
          throw new Error(`a cell's expired share must be a finite percent, not ${JSON.stringify(cell.expired)}`);
        }
        if (cell.expired < 0) {
          throw new Error(`a cell's expired share must be 0 or more, not ${JSON.stringify(cell.expired)}`);
        }
        if (cell.expired > 0) {
          // The hatched part is the share that expired, from the cell's top
          // down (charts.css stops the band at the cell's height; the tooltip
          // gives a share above 100 %).
          node.appendChild(
            h("div", {
              class: "chart-cell-hatch",
              style: { height: `${cell.expired}%` },
            })
          );
        }
      }
      if (cell.text !== undefined && cell.text !== null && cell.text !== "") {
        const text = numberNode("chart-cell-text", cell.text);
        // The numbers on bars and those in plain cells (blocked hours) are
        // shown or hidden as two groups.
        (total !== null ? barTexts : plainTexts).push(text);
        node.appendChild(text);
      }
      attachTip(node, cell.tip);
      clickable(node, cell.onClick);
      rowNode.appendChild(node);
    }
    root.appendChild(rowNode);
  }
  let scrolled = false;
  fitOnResize(root, () => {
    // Narrow week columns get the small gap (charts.css), so 53 weeks fit a
    // wide window without scrolling.
    root.classList.remove("chart-narrow");
    const weeks = head.lastChild.getBoundingClientRect().right - head.firstChild.getBoundingClientRect().right;
    root.classList.toggle("chart-narrow", weeks / Math.max(1, headers.length) < NARROW_COLUMN);
    layoutAxis(labels, head);
    fitNumbers(barTexts);
    fitNumbers(plainTexts);
    // A grid wider than its card opens at its newest weeks, and keeps the
    // chosen week in view.
    if (!scrolled) {
      scrolled = true;
      root.scrollLeft = root.scrollWidth;
    }
    if (selectedHead !== null) {
      // The column and its label, which may be wider than the column.
      const corner = head.firstChild.getBoundingClientRect();
      const view = root.getBoundingClientRect();
      const column = selectedHead.getBoundingClientRect();
      const label = selectedHead.firstChild.getBoundingClientRect();
      const left = Math.min(column.left, label.left);
      const right = Math.max(column.right, label.right);
      // Whole pixels: the scroll offset is rounded, a part of a pixel would
      // leave the label just under the row labels.
      if (left < corner.right) root.scrollLeft -= Math.ceil(corner.right - left);
      else if (right > view.right) root.scrollLeft += Math.ceil(right - view.right);
    }
    hideCovered();
  });
  // A week label scrolled partly under the row labels is hidden, never cut.
  function hideCovered() {
    const edge = head.firstChild.getBoundingClientRect().right;
    for (const label of labels) {
      label.node.classList.toggle(
        "chart-label-covered",
        label.node.getBoundingClientRect().left < edge - 1
      );
    }
  }
  root.addEventListener("scroll", hideCovered, { passive: true });
  return root;
}

// ---------------------------------------------------------------------------
// heat — the Rhythm page's 7 × 24 hour map
// ---------------------------------------------------------------------------

// `values` is row-major, 7 rows of 24 columns, Monday first. The colour strength
// is the value over the largest one. `tip(row, col, value)` writes the tooltip;
// the page knows the weekday and hour names.
export function heat({ rows = 7, cols = 24, values, tip } = {}) {
  if (!Array.isArray(values)) throw new Error("heat needs a list of values");
  const expected = rows * cols;
  if (values.length !== expected) {
    throw new Error(`heat needs ${expected} values, got ${values.length}`);
  }
  let max = 0;
  for (const value of values) {
    if (finite(value) && value > max) max = value;
  }
  const root = h("div", { class: "chart chart-heat" });
  root.style.setProperty("--chart-heat-columns", `repeat(${cols}, minmax(0, 1fr))`);

  const axis = h("div", { class: "chart-heat-row chart-heat-axis" });
  axis.appendChild(h("span", { class: "chart-heat-day" }));
  for (let col = 0; col < cols; col += 1) {
    axis.appendChild(
      h("span", {
        class: "chart-heat-hour",
        text: col % 3 === 0 ? String(col) : "",
      })
    );
  }
  root.appendChild(axis);

  for (let row = 0; row < rows; row += 1) {
    const line = h("div", { class: "chart-heat-row" });
    line.appendChild(
      h("span", { class: "chart-heat-day", text: WEEKDAYS[row] !== undefined ? WEEKDAYS[row] : "" })
    );
    for (let col = 0; col < cols; col += 1) {
      const value = values[row * cols + col];
      const strength = finite(value) && max > 0 ? clampPercent((value / max) * 100) : 0;
      const cell = h("div", {
        class: "chart-heat-cell chart-heat-tone",
        style: { "--chart-heat-strength": `${strength}%` },
      });
      if (typeof tip === "function") attachTip(cell, tip(row, col, value));
      line.appendChild(cell);
    }
    root.appendChild(line);
  }
  return root;
}

// ---------------------------------------------------------------------------
// lanes — the Forecast's timeline of the next 7 days
// ---------------------------------------------------------------------------

// One lane per account: `blocks` are [from, to] pairs, `marks` single times.
// `color` on a lane paints its blocks; `blockTips` and `markTips` (parallel
// lists) write their tooltips. `ticks` are { at, label } day ticks.
export function lanes({ lanes, from, to, ticks } = {}) {
  if (!Array.isArray(lanes)) throw new Error("lanes needs a list of lanes");
  const start = timeOf(from);
  const end = timeOf(to);
  if (end <= start) {
    throw new Error("lanes needs a from before its to");
  }
  const xOf = (time) => clampPercent(((time - start) / (end - start)) * 100);
  const root = h("div", { class: "chart chart-lanes" });

  const axis = h("div", { class: "chart-lanes-row chart-lanes-axis" });
  axis.appendChild(h("span", { class: "chart-lanes-label" }));
  const axisTrack = h("div", { class: "chart-lanes-track" });
  for (const tick of Array.isArray(ticks) ? ticks : []) {
    const at = timeOf(tick && tick.at !== undefined ? tick.at : tick);
    if (at < start || at > end) continue;
    const mark = h("span", {
      class: "chart-lanes-tick",
      style: { left: `${xOf(at)}%` },
      text: String(tick && tick.label !== undefined ? tick.label : ""),
    });
    axisTrack.appendChild(mark);
  }
  axis.appendChild(axisTrack);
  root.appendChild(axis);

  for (const lane of lanes) {
    const row = h("div", { class: `chart-lanes-row${lane.className ? ` ${lane.className}` : ""}` });
    const label = h("span", { class: "chart-lanes-label" });
    if (lane.label !== undefined && lane.label !== null) {
      if (typeof lane.label === "string" || typeof lane.label === "number") {
        label.appendChild(document.createTextNode(String(lane.label)));
      } else {
        label.appendChild(lane.label);
      }
    }
    row.appendChild(label);
    const track = h("div", { class: "chart-lanes-track" });
    const blocks = Array.isArray(lane.blocks) ? lane.blocks : [];
    for (const [index, block] of blocks.entries()) {
      if (!Array.isArray(block) || block.length < 2) {
        throw new Error(`a lane block is a from and a to, not ${JSON.stringify(block)}`);
      }
      const blockStart = timeOf(block[0]);
      const blockEnd = timeOf(block[1]);
      if (blockEnd <= blockStart) {
        throw new Error(`a lane block ends before it starts: ${JSON.stringify(block)}`);
      }
      // A block shorter than the axis's width still gets a visible sliver.
      const left = xOf(Math.max(start, blockStart));
      const width = Math.max(0.35, xOf(Math.min(end, blockEnd)) - left);
      const piece = h("i", {
        style: { left: `${left}%`, width: `${width}%`, background: lane.color },
      });
      const tip = Array.isArray(lane.blockTips) ? lane.blockTips[index] : undefined;
      attachTip(piece, tip);
      track.appendChild(piece);
    }
    const marks = Array.isArray(lane.marks) ? lane.marks : [];
    for (const [index, mark] of marks.entries()) {
      const at = timeOf(mark);
      if (at < start || at > end) continue;
      const line = h("u", { style: { left: `${xOf(at)}%` } });
      const tip = Array.isArray(lane.markTips) ? lane.markTips[index] : undefined;
      attachTip(line, tip);
      track.appendChild(line);
    }
    row.appendChild(track);
    root.appendChild(row);
  }
  return root;
}

// ---------------------------------------------------------------------------
// hours — the Activity page's hour-per-host lanes
// ---------------------------------------------------------------------------

// One lane per host; `cells` is one per hour, oldest first, and each carries
// its time `t`. A cell's `state` is "work", "idle", "off" or "unknown". Its
// `stack` (a list of { value, color }) is the bar of that hour, and `strip` the
// number of sessions working in it: the strip is a heatmap whose cells are as
// tall as the strip and coloured by that number over the largest in any lane.
// `days` is the axis: { label, span } per day boundary. A `mark` is placed at
// the left edge of the first hour column after its time.
export function hours({ lanes, days, marks } = {}) {
  if (!Array.isArray(lanes)) throw new Error("hours needs a list of lanes");
  const firstCells = lanes.length > 0 && Array.isArray(lanes[0].cells) ? lanes[0].cells : [];
  const count = firstCells.length;
  const columnTimes = firstCells.map((cell) => timeOf(cell && cell.t));

  function laneName(lane) {
    if (lane.label === undefined || lane.label === null) return "?";
    return typeof lane.label === "string" ? lane.label : lane.label.textContent;
  }

  // A missing session count is none; any other non-number or a negative count
  // is an error, never a silently drawn 0.
  function stripSessions(lane, index) {
    const cell = Array.isArray(lane.cells) ? lane.cells[index] : undefined;
    const sessions = cell ? cell.strip : undefined;
    if (sessions === undefined || sessions === null) return 0;
    if (!finite(sessions) || sessions < 0) {
      throw new Error(
        `an hour cell has no session count: ${String(sessions)} (lane ${JSON.stringify(
          laneName(lane)
        )}, hour ${index})`
      );
    }
    return sessions;
  }

  let maxStack = 0;
  let maxStrip = 0;
  for (const lane of lanes) {
    for (const [index, cell] of (Array.isArray(lane.cells) ? lane.cells : []).entries()) {
      let total = 0;
      for (const segment of Array.isArray(cell && cell.stack) ? cell.stack : []) {
        if (finite(segment && segment.value) && segment.value > 0) total += segment.value;
      }
      if (total > maxStack) maxStack = total;
      const sessions = stripSessions(lane, index);
      if (sessions > maxStrip) maxStrip = sessions;
    }
  }

  const root = h("div", { class: "chart chart-hours" });
  const axis = h("div", { class: "chart-hours-row chart-hours-axis" });
  axis.appendChild(h("span", { class: "chart-hours-label" }));
  const axisTrack = h("div", { class: "chart-hours-dayaxis" });
  for (const day of Array.isArray(days) ? days : []) {
    if (!day) continue;
    const span = Number.isFinite(day.span) ? Math.max(1, Math.round(day.span)) : 1;
    axisTrack.appendChild(
      h("span", { class: "chart-hours-day", style: { flex: String(span) }, text: String(day.label ?? "") })
    );
  }
  axis.appendChild(axisTrack);
  root.appendChild(axis);

  for (const lane of lanes) {
    const row = h("div", { class: "chart-hours-row" });
    const label = h("span", { class: "chart-hours-label" });
    if (lane.label !== undefined && lane.label !== null) {
      if (typeof lane.label === "string" || typeof lane.label === "number") {
        label.appendChild(document.createTextNode(String(lane.label)));
      } else {
        label.appendChild(lane.label);
      }
    }
    row.appendChild(label);
    const track = h("div", { class: "chart-hours-track" });
    const cells = h("div", { class: "chart-hours-cells" });
    const strip = h("div", { class: "chart-hours-strip" });
    for (let index = 0; index < count; index += 1) {
      const cell = Array.isArray(lane.cells) ? lane.cells[index] : undefined;
      const state = cell && cell.state;
      if (typeof state !== "string" || state === "") {
        throw new Error(
          `an hour cell has no state: ${JSON.stringify(state)} (lane ${JSON.stringify(
            laneName(lane)
          )}, hour ${index})`
        );
      }
      const node = h("div", { class: `chart-hours-cell chart-hours-${state}` });
      const stack = h("div", { class: "chart-hours-stack" });
      for (const segment of Array.isArray(cell && cell.stack) ? cell.stack : []) {
        if (!segment || !finite(segment.value) || segment.value <= 0) continue;
        const height = maxStack > 0 ? clampPercent((segment.value / maxStack) * 100) : 0;
        stack.appendChild(
          h("i", { style: { height: `${height}%`, background: segment.color } })
        );
      }
      node.appendChild(stack);
      if (cell && typeof cell.tip === "string") attachTip(node, cell.tip);
      cells.appendChild(node);

      const sessions = stripSessions(lane, index);
      const strength =
        sessions > 0 && maxStrip > 0 ? clampPercent((sessions / maxStrip) * 100) : 0;
      const stripCell = h("div", {
        class: "chart-hours-stripcell chart-heat-tone",
        style: { "--chart-heat-strength": `${strength}%` },
      });
      strip.appendChild(stripCell);
    }
    track.appendChild(cells);
    track.appendChild(strip);
    row.appendChild(track);
    root.appendChild(row);
  }
  const layer = markLayer(
    marks,
    (at) => markPosition(at, columnTimes),
    "chart-marks chart-marks-hours"
  );
  if (layer) {
    root.appendChild(layer);
    fitOnResize(root, () => layoutMarks(layer));
  }
  return root;
}

export { attachTip, hideTip, showTip };
