// seatswap page helpers shared by the pop-up (panel.js) and the large window
// (window.js). Moved out of panel.js unchanged; both pages import them from here.

import { fixtureInvoke } from "./fixture.js";

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

const PALETTE = [
  "#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#8b5cf6",
  "#ec4899", "#14b8a6", "#f97316", "#6366f1", "#84cc16",
];

// The tray's grey for an unknown or invalid account colour (icon.rs GREY).
const TRAY_GREY = "#555b66";

const LEVEL_COLORS = {
  fine: "#22c55e",
  close: "#f59e0b",
  critical: "#ef4444",
  locked: "#ef4444",
};

// The one table of the models the window names: the id Claude Code writes, the
// name the pages show and the colour an Activity hour bar paints it with. A
// model family keeps one colour across its versions; `MODEL_NAMES` is this same
// table reduced to its names, so no second list of models exists. A model this
// table does not know has no name of its own and is drawn in the neutral
// `--muted`; its pages always write its id next to the colour.
const MODELS = [
  { id: "claude-opus-5-5", name: "Opus 5.5", color: "var(--focus)" },
  { id: "claude-opus-5-4", name: "Opus 5.4", color: "var(--focus)" },
  { id: "claude-fable-5-2", name: "Fable 5.2", color: "var(--warn)" },
  { id: "claude-fable-5-1", name: "Fable 5.1", color: "var(--warn)" },
  { id: "claude-mythos-5-2", name: "Mythos 5.2", color: "var(--danger)" },
  { id: "claude-sonnet-5-5", name: "Sonnet 5.5", color: "var(--pill-text)" },
  { id: "claude-haiku-5-5", name: "Haiku 5.5", color: "var(--muted)" },
];

const MODEL_NAMES = Object.fromEntries(MODELS.map((model) => [model.id, model.name]));

// The weekdays Monday first: the statistics pages' weeks and the Rhythm page's
// heat map start the week on Monday (step-c3, Global Constraints).
const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

// The colour of a model's Activity bar: the table's colour, or the neutral
// token for a model the table does not know (the tip still names it).
function modelColor(model) {
  const entry = MODELS.find((candidate) => candidate.id === model);
  return entry ? entry.color : "var(--muted)";
}

// The model's name from the one table; a model the table does not know keeps
// its id as the name (its colour is the neutral token, as `modelColor` says).
function modelName(model) {
  return MODEL_NAMES[model] !== undefined ? MODEL_NAMES[model] : model;
}

const TAURI_GLOBAL = typeof window !== "undefined" ? window.__TAURI__ : undefined;
const HAS_TAURI = Boolean(
  TAURI_GLOBAL && TAURI_GLOBAL.core && typeof TAURI_GLOBAL.core.invoke === "function"
);

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// Whether an account view of a snapshot is the account in use: its own
// `isActive` flag, or the snapshot's `activeId`. The pop-up and the window
// both ask this one rule.
function isActiveView(view, snapshot) {
  if (!isObject(view)) return false;
  if (view.isActive === true) return true;
  return Boolean(
    isObject(snapshot) &&
      typeof snapshot.activeId === "string" &&
      view.id === snapshot.activeId
  );
}

function clamp(value, low, high) {
  return Math.min(high, Math.max(low, value));
}

function clone(value) {
  return value === undefined || value === null ? value : JSON.parse(JSON.stringify(value));
}

function errorMessage(error) {
  if (typeof error === "string") return error;
  if (error && typeof error.message === "string") return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

function isColor(value) {
  return typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value);
}

function hexToRgb(value) {
  if (!isColor(value)) return null;
  const number = parseInt(value.slice(1), 16);
  return {
    r: (number >> 16) & 0xff,
    g: (number >> 8) & 0xff,
    b: number & 0xff,
  };
}

function rgba(value, alpha) {
  const rgb = hexToRgb(value);
  if (!rgb) return `rgba(139, 147, 167, ${alpha})`;
  return `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${alpha})`;
}

function rgbToHex(rgb) {
  const part = (number) => clamp(Math.round(number), 0, 255).toString(16).padStart(2, "0");
  return `#${part(rgb.r)}${part(rgb.g)}${part(rgb.b)}`;
}

function mixHex(first, second, ratio) {
  const a = hexToRgb(first);
  const b = hexToRgb(second);
  if (!a || !b) return first;
  const mix = (x, y) => x + (y - x) * ratio;
  return rgbToHex({ r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b) });
}

function relativeLuminance(value) {
  const rgb = hexToRgb(value);
  if (!rgb) return 0;
  const channel = (part) => {
    const scaled = part / 255;
    return scaled <= 0.03928 ? scaled / 12.92 : Math.pow((scaled + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b);
}

function contrastText(background) {
  const luminance = relativeLuminance(background);
  const whiteRatio = 1.05 / (luminance + 0.05);
  const darkRatio = (luminance + 0.05) / (relativeLuminance("#0f1115") + 0.05);
  return darkRatio >= whiteRatio ? "#0f1115" : "#ffffff";
}

function limitLevel(limit) {
  const percent = Number(limit && limit.percent);
  if (!Number.isFinite(percent)) return "fine";
  if (percent >= 100) return "locked";
  if (percent >= 90 || limit.severity === "critical") return "critical";
  if (percent >= 70) return "close";
  return "fine";
}

function shortPercent(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "–";
  const rounded = Math.round(number * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

function clear(node) {
  while (node && node.firstChild) node.removeChild(node.firstChild);
}

// "9 Nov": the statistics pages' date without the weekday (`dateText` writes
// "Mon 9 Nov"). A bad time is an error, never a made-up date.
function shortDate(value) {
  return dateText(value).replace(/^\S+\s+/, "");
}

// A date as a Monday-first weekday index: 0 = Monday, 6 = Sunday. This is the
// one place the statistics pages apply the week's Monday-first rule; they name
// the day with `WEEKDAYS[index]`.
function weekdayIndex(value) {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    throw new Error(`invalid time ${JSON.stringify(value)}`);
  }
  return (date.getDay() + 6) % 7;
}

// "07" from 7: the hour a label or a time names.
function pad(number) {
  return String(number).padStart(2, "0");
}

// One of the pages' KPI boxes: the label, the value (text or a node such as an
// amount), an optional sub-line, the whole card's tip and an optional class on
// the value. The statistics pages share this one box.
function kpi(label, value, sub, tip, valueClass) {
  const node = h("div", { class: "stats-kpi" }, [
    h("span", { class: "stats-kpi-label", text: label }),
    h("span", { class: `stats-kpi-value${valueClass ? ` ${valueClass}` : ""}` }, value),
    sub ? h("span", { class: "stats-kpi-sub" }, sub) : null,
  ]);
  node.title = tip;
  return node;
}

// One legend entry: a coloured (or hatched) box and the name beside it.
function legendItem(colour, name, hatch) {
  return h("span", { class: "stats-legend-item" }, [
    h("span", {
      class: `stats-legend-box${hatch ? " stats-legend-box-hatch" : ""}`,
      style: { "--stats-legend-color": colour },
    }),
    h("span", { class: "stats-legend-name", text: name, title: name }),
  ]);
}

// A card's short line for a list the struct carries empty: "— · no week in the
// range". The reason is written out, never replaced by a zero or a blank.
function emptyLine(reason) {
  if (typeof reason !== "string" || reason === "") {
    throw new Error(`an empty list needs its reason, not ${JSON.stringify(reason)}`);
  }
  return h("p", { class: "stats-empty", text: `— · ${reason}` });
}

function setStyles(node, styles) {
  for (const [key, value] of Object.entries(styles)) {
    if (value === undefined || value === null) continue;
    if (key.startsWith("--")) node.style.setProperty(key, String(value));
    else node.style[key] = value;
  }
}

function applyProps(node, props) {
  for (const [key, value] of Object.entries(props || {})) {
    if (key === "hidden") {
      node.hidden = Boolean(value);
      continue;
    }
    if (key === "checked") {
      node.checked = Boolean(value);
      continue;
    }
    if (key === "disabled") {
      node.disabled = Boolean(value);
      continue;
    }
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = String(value);
    else if (key === "style") setStyles(node, value);
    else if (key === "dataset") {
      for (const [dataKey, dataValue] of Object.entries(value)) {
        if (dataValue !== undefined && dataValue !== null) node.dataset[dataKey] = String(dataValue);
      }
    } else if (key === "value") {
      node.value = value;
    } else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2), value);
    } else if (value === true) {
      node.setAttribute(key, "");
    } else {
      node.setAttribute(key, String(value));
    }
  }
}

function appendChildren(node, children) {
  const list = Array.isArray(children) ? children : [children];
  for (const child of list) {
    if (child === undefined || child === null || child === false) continue;
    if (typeof child === "string" || typeof child === "number") {
      node.appendChild(document.createTextNode(String(child)));
    } else {
      node.appendChild(child);
    }
  }
}

function h(tag, props, children) {
  const node = document.createElement(tag);
  applyProps(node, props || {});
  if (children !== undefined) appendChildren(node, children);
  return node;
}

// The one banner of the pop-up and the large window: a kind, an icon, the text
// and optional children (buttons). Both pages show a snapshot error with it.
function banner(kind, icon, text, extraChildren) {
  const body = h("div", { class: "banner-body" }, [
    h("div", { class: "banner-text" }, Array.isArray(text) ? text : String(text)),
  ]);
  if (extraChildren) {
    if (Array.isArray(extraChildren)) {
      for (const child of extraChildren) {
        if (child) body.appendChild(child);
      }
    } else {
      body.appendChild(extraChildren);
    }
  }
  return h(
    "div",
    {
      class: `banner ${kind}`,
      role: kind === "error" ? "alert" : "status",
    },
    [h("span", { class: "banner-icon", "aria-hidden": "true", text: icon }), body]
  );
}

// The settings controls shared by the pop-up and the large window: one text
// input, one select and one switch, so both pages build them the same way.
function textInput(props) {
  return h("input", Object.assign({ type: "text", spellcheck: "false" }, props || {}));
}

function selectInput(options, value, onchange) {
  const select = h("select", {
    class: "inp sel",
    onchange: (event) => onchange(event.target.value),
  });
  for (const [optionValue, optionLabel] of options) {
    const option = h("option", { value: optionValue, text: optionLabel });
    if (optionValue === value) option.selected = true;
    select.appendChild(option);
  }
  return select;
}

// A switch for a boolean setting. The caller saves and reverts the control when
// the save is refused.
function switchButton(checked, label, onchange) {
  const control = h("button", {
    type: "button",
    class: "switch",
    role: "switch",
    "aria-checked": checked ? "true" : "false",
    "aria-label": label,
  });
  control.addEventListener("click", () => {
    const next = control.getAttribute("aria-checked") !== "true";
    control.setAttribute("aria-checked", next ? "true" : "false");
    onchange(next);
  });
  return control;
}

// ---------------------------------------------------------------------------
// the account chip editor (spec §3.7): letter boxes, swatches, hex field and
// the tray-icon preview. The pop-up's old inline editor used to build these;
// the Accounts group in the large window is now their one user.
// ---------------------------------------------------------------------------

// One letter box holds a single A-Z or 0-9, upper case.
function sanitizeAbbrChar(value) {
  return String(value || "")
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "")
    .slice(0, 1);
}

// Two letter boxes for the abbreviation. `onInput` gets the current value at
// every keystroke; `onChange` gets it when the field is left or Enter is
// pressed, never when the focus only moves to the second box.
function abbrLetterBoxes(value, handlers) {
  const callbacks = handlers || {};
  const chars = String(value || "").toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 2);
  const first = textInput({
    value: chars[0] || "",
    maxlength: "1",
    class: "abbr-box",
    "aria-label": "First abbreviation character",
    autocapitalize: "characters",
    autocomplete: "off",
    dataset: { field: "abbr1" },
  });
  const second = textInput({
    value: chars[1] || "",
    maxlength: "1",
    class: "abbr-box",
    "aria-label": "Second abbreviation character",
    autocapitalize: "characters",
    autocomplete: "off",
    dataset: { field: "abbr2" },
  });
  const boxes = [first, second];
  const current = () => `${sanitizeAbbrChar(first.value)}${sanitizeAbbrChar(second.value)}`;

  function changed() {
    if (typeof callbacks.onInput === "function") callbacks.onInput(current());
  }

  function commit() {
    if (typeof callbacks.onChange !== "function") return;
    callbacks.onChange(current());
  }

  for (const [index, box] of boxes.entries()) {
    box.addEventListener("input", () => {
      const cleaned = sanitizeAbbrChar(box.value);
      box.value = cleaned;
      if (cleaned && index === 0 && !second.value) {
        second.focus();
        second.select();
      }
      changed();
    });
    box.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        commit();
        return;
      }
      if (index === 0 && event.key === "ArrowRight") {
        event.preventDefault();
        second.focus();
        second.select();
      }
    });
    box.addEventListener("blur", (event) => {
      const next = event.relatedTarget;
      if (boxes.includes(next)) return;
      commit();
    });
  }

  return {
    node: h("div", { class: "abbr-boxes" }, boxes),
    value: current,
    setValue(abbr) {
      const cleaned = String(abbr || "").toUpperCase().replace(/[^A-Z0-9]/g, "");
      first.value = cleaned[0] || "";
      second.value = cleaned[1] || "";
    },
  };
}

// The ten palette colours as a swatch row; `onPick` gets the chosen colour.
function colorSwatches(color, onPick) {
  const node = h("div", { class: "swatches" });
  const buttons = [];
  function select(value) {
    const wanted = String(value || "").toLowerCase();
    for (const entry of buttons) {
      entry.button.classList.toggle("selected", entry.color === wanted);
    }
  }
  for (const entry of PALETTE) {
    const swatch = h("button", {
      type: "button",
      class: "swatch",
      "aria-label": `Colour ${entry}`,
      title: entry,
    });
    swatch.style.backgroundColor = entry;
    swatch.addEventListener("click", () => {
      select(entry);
      if (typeof onPick === "function") onPick(entry);
    });
    buttons.push({ button: swatch, color: entry });
    node.appendChild(swatch);
  }
  select(color);
  return { node, select };
}

// A `#rrggbb` field: `onInput` while typing, `onChange` on blur or Enter.
function hexField(value, handlers) {
  const callbacks = handlers || {};
  const input = textInput({
    value: value || "",
    maxlength: "7",
    class: "hex-input",
    "aria-label": "Colour hex value",
    dataset: { field: "hex" },
  });
  const current = () => input.value.trim().toLowerCase();
  input.addEventListener("input", () => {
    if (typeof callbacks.onInput === "function") callbacks.onInput(current());
  });
  input.addEventListener("change", () => {
    if (typeof callbacks.onChange === "function") callbacks.onChange(current());
  });
  input.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    if (typeof callbacks.onChange === "function") callbacks.onChange(current());
  });
  return {
    node: input,
    value: current,
    setValue(color) {
      input.value = color || "";
    },
  };
}

// ---------------------------------------------------------------------------
// the kept choices of an area
// ---------------------------------------------------------------------------

// Which area a `seatswap.<area>.<choice>` key belongs to, for the warning text.
function choiceArea(key) {
  const match = /^seatswap\.([a-z0-9-]+)\./.exec(String(key));
  return match ? match[1] : "window";
}

// A choice kept in localStorage, checked against the values the page allows.
// A value that is not allowed is dropped and named in a console warning; it is
// never quietly replaced by another value. `allowed` is a list or a predicate.
function readChoice(key, allowed, fallback) {
  try {
    const value = localStorage.getItem(key);
    if (value === null) return fallback;
    const ok = typeof allowed === "function" ? allowed(value) : allowed.includes(value);
    if (!ok) {
      localStorage.removeItem(key);
      console.warn(
        `[${choiceArea(key)}] dropped the unknown choice ${JSON.stringify(value)} for ${key}`
      );
      return fallback;
    }
    return value;
  } catch (error) {
    console.warn(`[${choiceArea(key)}] cannot read ${key}`, error);
    return fallback;
  }
}

function writeChoice(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch (error) {
    console.warn(`[${choiceArea(key)}] cannot store ${key}`, error);
  }
}

// ---------------------------------------------------------------------------
// times and statistics numbers: one way to write them on every page
// ---------------------------------------------------------------------------

// "Mon 9 Nov" in this computer's local time, as the statistics pages write a
// date. A bad time is an error, never a made-up date.
function dateText(value) {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    throw new Error(`invalid time ${JSON.stringify(value)}`);
  }
  return date.toLocaleDateString("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
  });
}

// "Mon 9 Nov 10:00".
function dateTimeText(value) {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    throw new Error(`invalid time ${JSON.stringify(value)}`);
  }
  const time = date.toLocaleTimeString("en-GB", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  return `${dateText(date)} ${time}`;
}

// "Mon 17:00": a weekday and a time, without the date, in this computer's local
// time — how the Forecast writes a reset and how the Activity names an hour.
function weekdayTimeText(value) {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    throw new Error(`invalid time ${JSON.stringify(value)}`);
  }
  const weekday = date.toLocaleDateString("en-GB", { weekday: "short" });
  const time = date.toLocaleTimeString("en-GB", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  return `${weekday} ${time}`;
}

// "2 h" or "1.5 h", one decimal only when the value has one.
function hoursText(value) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`invalid hour count ${String(value)}: use a finite number of 0 or more`);
  }
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? `${rounded} h` : `${rounded.toFixed(1)} h`;
}

// "27 %" or "21.9 %", one decimal only when the value has one.
function percentText(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`invalid percent ${String(value)}: use a finite number`);
  }
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? `${rounded} %` : `${rounded.toFixed(1)} %`;
}

// A money value, or "—" when the value or the page's currency is missing.
function amountText(value, currency) {
  if (value === null || value === undefined) return "—";
  if (typeof currency !== "string" || !/^[A-Z]{3}$/.test(currency)) return "—";
  return money(value, currency);
}

// The amount as an element: a "—" caused by a currency the settings do not set
// carries that reason as its title.
function amountNode(value, currency) {
  const node = h("span", { text: amountText(value, currency) });
  if (typeof currency !== "string" || !/^[A-Z]{3}$/.test(currency)) {
    node.title = "no currency set in Settings";
  }
  return node;
}

// The amount in a tooltip: the same "—", with the missing currency's reason
// beside it (a tooltip has no title of its own).
function amountTip(value, currency) {
  const text = amountText(value, currency);
  if (text === "—" && (typeof currency !== "string" || !/^[A-Z]{3}$/.test(currency))) {
    return "— (no currency set in Settings)";
  }
  return text;
}

// The tip of one `Values` half (spec §1.4, §3.6). `scope` names the span the
// number covers ("in the range", "in this hour"), so two numbers of different
// spans never look alike. A missing half carries the struct's own reason; a
// missing currency carries the settings' reason. The pages draw the number
// itself with `amountNode(values.api, values.apiCurrency)` and the other half
// with `values.currency`.
function valueTip(values, unit, scope) {
  const span = scope ? ` ${scope}` : "";
  const label = unit === "api" ? "API value" : "Subscription value";
  const missing = unit === "api" ? values.apiMissing : values.subscriptionMissing;
  if (missing !== null && missing !== undefined) {
    return `${label}${span}: — (${missing})`;
  }
  const amount = unit === "api" ? values.api : values.subscription;
  const currency = unit === "api" ? values.apiCurrency : values.currency;
  return `${label}${span}: ${amountTip(amount, currency)}`;
}

// The snapshot's account view for an id, or null when the snapshot has none.
function accountView(views, id) {
  if (!Array.isArray(views)) return null;
  return views.find((view) => isObject(view) && view.id === id) || null;
}

// The name of an account on the statistics pages, as data: its abbreviation in
// the account's colour, its label as the title; an id the snapshot does not know
// (an account no longer stored) is its first eight characters, titled with the
// full id.
function accountLabel(views, id) {
  if (typeof id !== "string" || id === "") {
    throw new Error(`an account name needs an account id, not ${JSON.stringify(id)}`);
  }
  const view = accountView(views, id);
  const known = isObject(view);
  const abbr = known && typeof view.abbr === "string" && view.abbr !== "" ? view.abbr : null;
  const label = known && typeof view.label === "string" && view.label !== "" ? view.label : null;
  const color = known && isColor(view.color) ? view.color : null;
  return {
    text: abbr || id.slice(0, 8),
    title: known ? [abbr, label].filter(Boolean).join(" · ") || id : id,
    style: color
      ? { background: color, color: contrastText(color) }
      : { background: "var(--button-bg)", color: "var(--text)" },
  };
}

// The account's name as plain text, for a tooltip or a chart's series name: the
// same rule as `accountName`, which draws the coloured abbreviation.
function accountNameText(views, id) {
  return accountLabel(views, id).text;
}

function accountName(views, id) {
  const spec = accountLabel(views, id);
  return h("span", {
    class: "stats-acc",
    text: spec.text,
    title: spec.title,
    style: spec.style,
  });
}

function bytesOf(value) {
  if (Array.isArray(value)) return value;
  if (value instanceof Uint8Array || value instanceof Uint8ClampedArray) return value;
  if (typeof value === "string") {
    try {
      const binary = atob(value);
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) {
        bytes[index] = binary.charCodeAt(index);
      }
      return bytes;
    } catch {
      return [];
    }
  }
  if (value && Array.isArray(value.data)) return value.data;
  return [];
}

// The tray icon at 16, 22 and 32 px. `draw` asks `icon_preview`; `schedule`
// debounces it while the letters are typed. An error is returned, never hidden.
function iconPreview() {
  const node = h("div", { class: "previews" });
  const canvases = new Map();
  for (const size of [16, 22, 32]) {
    const canvas = h("canvas", {
      width: String(size),
      height: String(size),
      dataset: { size: String(size) },
      "aria-label": `Tray icon preview at ${size} pixels`,
    });
    canvases.set(size, canvas);
    node.appendChild(
      h("div", { class: "preview-cell" }, [
        canvas,
        h("span", { class: "preview-size", text: `${size} px` }),
      ])
    );
  }
  let token = 0;
  let timer = null;

  function clear() {
    for (const canvas of canvases.values()) {
      const context = canvas.getContext("2d");
      if (context) context.clearRect(0, 0, canvas.width, canvas.height);
    }
  }

  function apply(reply) {
    if (!Array.isArray(reply)) {
      throw new Error("icon preview returned an unexpected reply");
    }
    for (const item of reply) {
      if (!isObject(item)) continue;
      const size = Number(item.size);
      const canvas = canvases.get(size);
      if (!canvas) continue;
      canvas.width = size;
      canvas.height = size;
      const context = canvas.getContext("2d");
      if (!context) continue;
      const source = bytesOf(item.rgba);
      const expected = size * size * 4;
      if (source.length < expected) {
        throw new Error(`icon preview for ${size} px is too short`);
      }
      const image = context.createImageData(size, size);
      for (let index = 0; index < expected; index += 1) {
        image.data[index] = source[index] & 0xff;
      }
      context.putImageData(image, 0, 0);
    }
  }

  async function draw(abbr, color) {
    if (!/^[A-Z0-9]{1,2}$/.test(abbr) || !isColor(color)) {
      clear();
      return null;
    }
    const current = ++token;
    try {
      const reply = await invoke("icon_preview", { abbr, color });
      if (current !== token) return null;
      apply(reply);
      return null;
    } catch (error) {
      if (current !== token) return null;
      clear();
      return errorMessage(error);
    }
  }

  function schedule(abbr, color, onError) {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      draw(abbr, color).then((error) => {
        if (error && typeof onError === "function") onError(error);
      });
    }, 110);
  }

  return { node, draw, schedule, clear };
}

function applyTheme(setting) {
  // Callers that follow a stored setting pass it; this page alone knows where
  // that setting is kept.
  const wanted = setting || "system";
  let resolved = wanted;
  if (wanted !== "light" && wanted !== "dark") {
    const light =
      typeof window !== "undefined" &&
      window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: light)").matches;
    resolved = light ? "light" : "dark";
  }
  document.documentElement.dataset.theme = resolved;
}

// Both pages follow the system's light/dark change with this one function.
// `getSetting` returns the stored theme; only "system" follows the system.
function followTheme(getSetting) {
  if (typeof window === "undefined" || !window.matchMedia) return;
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  if (typeof media.addEventListener !== "function") return;
  media.addEventListener("change", () => {
    if (typeof getSetting !== "function" || getSetting() !== "system") return;
    applyTheme("system");
  });
}

// ---------------------------------------------------------------------------
// bridge: invoke() and listen() over the Tauri 2 global
// ---------------------------------------------------------------------------

async function invoke(command, args) {
  if (HAS_TAURI) {
    return TAURI_GLOBAL.core.invoke(command, args || {});
  }
  return fixtureInvoke(command, args || {});
}

async function listen(eventName, handler) {
  if (HAS_TAURI && TAURI_GLOBAL.event && typeof TAURI_GLOBAL.event.listen === "function") {
    const unlisten = await TAURI_GLOBAL.event.listen(eventName, (event) => {
      handler(event && Object.prototype.hasOwnProperty.call(event, "payload") ? event.payload : event);
    });
    return unlisten;
  }
  return () => {};
}

// ---------------------------------------------------------------------------
// money and token counts (spec §1.4; the plan's Global Constraints)
// ---------------------------------------------------------------------------

// "1,234 EUR" from 10 on, "6.47 EUR" below 10. A bad value is an error, never a
// fallback: the page shows the error text.
function money(amount, currency) {
  if (typeof amount !== "number" || !Number.isFinite(amount) || amount < 0) {
    throw new Error(`invalid amount ${String(amount)}: use a finite number of 0 or more`);
  }
  if (typeof currency !== "string" || !/^[A-Z]{3}$/.test(currency)) {
    throw new Error(
      `invalid currency ${JSON.stringify(currency)}: use three capital letters A to Z`
    );
  }
  const value = Math.round(amount * 100) / 100;
  const number =
    value < 10
      ? value.toFixed(2)
      : String(Math.round(value)).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${number} ${currency}`;
}

// "12.4 M" and "0.4 M" for every count of tokens, and "0 M" for none: the
// Sessions page writes every token cell and total in millions, one decimal.
function tokens(n) {
  if (typeof n !== "number" || !Number.isFinite(n) || n < 0) {
    throw new Error(`invalid token count ${String(n)}: use a finite number of 0 or more`);
  }
  if (n === 0) return "0 M";
  return `${(n / 1000000).toFixed(1)} M`;
}

export {
  PALETTE,
  TRAY_GREY,
  LEVEL_COLORS,
  MODELS,
  MODEL_NAMES,
  modelColor,
  modelName,
  WEEKDAYS,
  TAURI_GLOBAL,
  HAS_TAURI,
  isObject,
  isActiveView,
  clamp,
  clone,
  errorMessage,
  isColor,
  hexToRgb,
  rgba,
  rgbToHex,
  mixHex,
  relativeLuminance,
  contrastText,
  limitLevel,
  shortPercent,
  clear,
  shortDate,
  weekdayIndex,
  pad,
  kpi,
  legendItem,
  emptyLine,
  setStyles,
  applyProps,
  appendChildren,
  h,
  banner,
  applyTheme,
  followTheme,
  invoke,
  listen,
  money,
  tokens,
  readChoice,
  writeChoice,
  dateText,
  dateTimeText,
  weekdayTimeText,
  hoursText,
  percentText,
  amountText,
  amountNode,
  amountTip,
  valueTip,
  accountView,
  accountLabel,
  accountNameText,
  accountName,
  textInput,
  selectInput,
  switchButton,
  sanitizeAbbrChar,
  abbrLetterBoxes,
  colorSwatches,
  hexField,
  iconPreview,
};
